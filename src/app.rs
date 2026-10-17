//! Desktop app shell (§4g).
//!
//! Tauri window wrapping the same axum server the browser build uses, so the
//! web frontend is not thrown away by the wrap - it *is* the desktop UI.
//!
//! On the "leaking through the internet" concern: the server binds `127.0.0.1`
//! (loopback), which is unreachable from the network. What the desktop build
//! adds is that the port is **ephemeral** - the OS picks a free one at startup
//! rather than a fixed, guessable 7777 that other local programs could find.

use anyhow::{Context, Result};
use std::sync::OnceLock;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

use crate::config::Config;

/// The running desktop app, so the HTTP layer can ask it for a real OS window.
///
/// A pop-out used to be a `<div>` positioned inside the page, which cannot
/// leave the app window and therefore cannot be dragged to a second monitor -
/// the thing that makes a pop-out worth having. This is how the server reaches
/// back into Tauri to open a genuine window instead.
static APP: OnceLock<tauri::AppHandle> = OnceLock::new();

pub fn handle() -> Option<&'static tauri::AppHandle> {
    APP.get()
}

/// The whole-UI zoom (Ctrl/Cmd + / - / 0), as f64 bits. One value for every
/// window, so a pop-out opens at the same scale as the window it came from.
static ZOOM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x3FF0_0000_0000_0000); // 1.0

pub fn zoom() -> f64 {
    f64::from_bits(ZOOM.load(std::sync::atomic::Ordering::Relaxed))
}

/// Set the zoom on every open window - the webview's own zoom, exactly what a
/// browser's Ctrl + does, so layout and pointer maths stay consistent.
/// Returns false when not running as the desktop app (the page then scales
/// itself with CSS).
pub fn set_zoom_all(scale: f64) -> bool {
    let scale = scale.clamp(0.5, 2.5);
    ZOOM.store(scale.to_bits(), std::sync::atomic::Ordering::Relaxed);
    let Some(app) = handle() else { return false };
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        for (_, w) in app2.webview_windows() {
            let _ = w.set_zoom(scale);
        }
    });
    true
}

/// Open a real OS window showing one view of the dashboard.
///
/// Built on the main thread because window creation must happen there; calling
/// from the axum worker directly is a crash on some platforms and a silent
/// nothing on others.
pub fn open_view(url: String, label: String, title: String, w: f64, h: f64) -> Result<()> {
    let app = handle().context("not running as the desktop app")?.clone();
    let app2 = app.clone();
    app.run_on_main_thread(move || {
        // Re-focus rather than stacking duplicates if it is already open.
        if let Some(existing) = app2.get_webview_window(&label) {
            let _ = existing.set_focus();
            return;
        }
        let built = url
            .parse()
            .map_err(|e| format!("{e}"))
            .and_then(|u| {
                WebviewWindowBuilder::new(&app2, &label, WebviewUrl::External(u))
                    .title(&title)
                    .inner_size(w, h)
                    .min_inner_size(360.0, 220.0)
                    .decorations(true)
                    .build()
                    .map_err(|e| format!("{e}"))
            });
        match built {
            Ok(win) => {
                let _ = win.set_zoom(zoom());
                let _ = win.set_focus();
            }
            Err(e) => eprintln!("[bluee] could not open window `{label}`: {e}"),
        }
    })
    .context("dispatching to the main thread")?;
    Ok(())
}

pub fn run(cfg: Config) -> Result<()> {
    // Bind first, on port 0, so the OS hands us a free port and we know the
    // real number before the window is told where to look. Binding here rather
    // than inside the server task also means a bind failure is a clean startup
    // error instead of a blank window.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("binding local server")?;
    listener
        .set_nonblocking(true)
        .context("setting non-blocking")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}");
    // The zoom you left it at - saved by the UI in data/ui.json.
    let saved_zoom = std::fs::read_to_string(cfg.data_dir.join("ui.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t.trim_start_matches('\u{feff}')).ok())
        .and_then(|v| v.get("zoom").and_then(|z| z.as_f64()))
        .unwrap_or(1.0)
        .clamp(0.5, 2.5);
    ZOOM.store(saved_zoom.to_bits(), std::sync::atomic::Ordering::Relaxed);
    println!("bluee: serving on {url} (loopback only)");

    // The server runs on its own tokio runtime in a background thread; Tauri
    // owns the main thread because the OS event loop must live there.
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[bluee] runtime failed: {e}");
                return;
            }
        };
        rt.block_on(async move {
            if let Err(e) = crate::dash::serve_on(cfg, listener).await {
                eprintln!("[bluee] server stopped: {e:#}");
            }
        });
    });

    tauri::Builder::default()
        .setup(move |app| {
            let _ = APP.set(app.handle().clone());
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url.parse()?))
                .title("bluee")
                .inner_size(1360.0, 860.0)
                .min_inner_size(900.0, 600.0)
                .build()?;
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_zoom(zoom());
                let _ = w.set_focus();
            }
            Ok(())
        })
        /* Closing the MAIN window quits bluee, pop-outs included. Before, any
           pop-out still open somewhere (an agent's window, a tile on another
           monitor) kept the process alive with no visible sign - so "closed
           it" left harness.exe running, the next rebuild failed with "Access
           is denied", and a relaunch started a second copy beside it. */
        .on_window_event(|window, event| {
            if window.label() == "main" && matches!(event, tauri::WindowEvent::Destroyed) {
                window.app_handle().exit(0);
            }
        })
        .run(tauri::generate_context!())
        .context("running the desktop window")?;

    Ok(())
}
