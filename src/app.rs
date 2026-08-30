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
            Ok(win) => { let _ = win.set_focus(); }
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
                let _ = w.set_focus();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .context("running the desktop window")?;

    Ok(())
}
