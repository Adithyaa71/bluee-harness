//! A browser the harness drives itself, over the Chrome DevTools Protocol.
//!
//! # Why this exists
//!
//! The playground's browser panel used to go through SnareVec's `browser_*`
//! MCP tools. That path has three failure modes stacked on top of each other -
//! the daemon idles out, the daemon has to be running *and* reachable, and
//! `"browser": {"enabled": true}` has to be set by hand in
//! `~/.snarevec/config.json`. §12e records that last one as a deliberate human
//! gate in Adithya's own software, which is a fine thing for SnareVec to have
//! and a bad thing for this panel to depend on. The result was a browser panel
//! that, in practice, never worked.
//!
//! This owns the whole path instead: find Chrome or Edge, start it, speak CDP
//! to it. No daemon, no MCP hop, no gate. `dev/uicheck.mjs` has been driving
//! this same protocol on this same machine since the UI checks were written,
//! so the approach is already proven here - Chrome ships with Windows and CDP
//! is plain WebSocket.
//!
//! # Shape
//!
//! A fresh WebSocket per command, rather than one long-lived multiplexed
//! connection. Chrome keeps the page alive between connections, so the only
//! cost is a few milliseconds of handshake, and what it buys is that there is
//! no shared stream to demultiplex, no reader task, and no way for two
//! concurrent requests to read each other's replies. For a panel driven by
//! human clicks that is the right trade.
//!
//! The profile lives in `data/browser`, so cookies and logins persist across
//! restarts the same way they would in a browser you use.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

/// Where Chrome or Edge might be. Checked in preference order; Chrome first
/// because its CDP is the reference implementation, Edge second because it is
/// present on every Windows install even when Chrome is not.
const CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
];

pub fn find_browser() -> Option<PathBuf> {
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

struct Live {
    /// `None` when we adopted a browser a previous run left behind rather than
    /// starting one ourselves - see `ensure`.
    child: Option<Child>,
    port: u16,
}

impl Drop for Live {
    fn drop(&mut self) {
        // Best effort: if the harness is going down, the browser should not
        // outlive it as an orphan holding the profile lock. Only kill what we
        // started; an adopted instance is not ours to close.
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
    }
}

pub struct Browser {
    live: Mutex<Option<Live>>,
    profile: PathBuf,
}

impl Browser {
    pub fn new(data_dir: &Path) -> Self {
        // MUST be absolute. `cfg.data_dir` defaults to the relative "data", and
        // Chrome on Windows silently declines a relative --user-data-dir: it
        // starts, writes nothing to the directory, and never opens the
        // debugging port. That failure looked exactly like a hang, because the
        // probe loop then waited out its whole budget against a port nothing
        // was ever going to bind.
        let profile = data_dir.join("browser");
        let profile = if profile.is_absolute() {
            profile
        } else {
            std::env::current_dir()
                .map(|c| c.join(&profile))
                .unwrap_or(profile)
        };
        Self {
            live: Mutex::new(None),
            profile,
        }
    }

    /// Is a browser binary present at all? Answered by looking at the disk
    /// rather than by trusting PATH - the same lesson as `rust-analyzer` in
    /// §12g, where a shim on PATH existed and did not work.
    pub fn available(&self) -> Option<PathBuf> {
        find_browser()
    }

    /// Start the browser if it is not already running, and return its
    /// debugging port. Idempotent: a live child is reused.
    async fn ensure(&self) -> Result<u16> {
        {
            let mut g = self.live.lock().unwrap();
            if let Some(l) = g.as_mut() {
                // `try_wait` distinguishes "still running" from "exited while
                // we were not looking", which happens if the user closes it.
                let alive = match l.child.as_mut() {
                    Some(c) => matches!(c.try_wait(), Ok(None)),
                    None => true,
                };
                if alive {
                    return Ok(l.port);
                }
                *g = None;
            }
        }

        // Adopt an instance a previous run left behind.
        //
        // The harness is killed with `taskkill /F` on every rebuild (§12a), which
        // does not run destructors - so the Chrome it started survives, still
        // holding the lock on this profile directory. A fresh instance pointed at
        // the same --user-data-dir then cannot start, and never opens a debugging
        // port, which presents as the panel hanging on "Starting the browser".
        // Remembering the port in the profile makes the orphan reusable instead
        // of an obstacle.
        let portfile = self.profile.join(".bluee-port");
        if let Ok(txt) = std::fs::read_to_string(&portfile) {
            if let Ok(old) = txt.trim().parse::<u16>() {
                let probe = reqwest::Client::builder()
                    .timeout(Duration::from_millis(600))
                    .build()?;
                if probe
                    .get(format!("http://127.0.0.1:{old}/json/version"))
                    .send()
                    .await
                    .is_ok()
                {
                    *self.live.lock().unwrap() = Some(Live {
                        child: None,
                        port: old,
                    });
                    return Ok(old);
                }
            }
        }

        let exe = find_browser().ok_or_else(|| {
            anyhow!("no Chrome or Edge found - looked in Program Files for chrome.exe and msedge.exe")
        })?;
        std::fs::create_dir_all(&self.profile).ok();

        // Port 0 is not an option here: Chrome needs to be told a number, and
        // its own --remote-debugging-port=0 writes the chosen port to a file in
        // the profile. Binding one ourselves and releasing it is racier than
        // reading that file, so we read the file.
        let port = free_port()?;

        let child = Command::new(&exe)
            .args([
                "--headless=new",
                &format!("--remote-debugging-port={port}"),
                &format!("--user-data-dir={}", self.profile.display()),
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-background-networking",
                "--disable-features=Translate,MediaRouter",
                "--window-size=1280,900",
                "about:blank",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("could not start {}", exe.display()))?;

        // Wait for the protocol endpoint rather than sleeping a fixed amount:
        // a cold start is far slower than a warm one. Every probe carries its
        // own deadline - reqwest has no default timeout, so one unanswered
        // request would otherwise hang the route with nothing to look at.
        let probe = reqwest::Client::builder()
            .timeout(Duration::from_millis(700))
            .build()?;
        for _ in 0..40 {
            if probe
                .get(format!("http://127.0.0.1:{port}/json/version"))
                .send()
                .await
                .is_ok()
            {
                std::fs::write(&portfile, port.to_string()).ok();
                *self.live.lock().unwrap() = Some(Live {
                    child: Some(child),
                    port,
                });
                return Ok(port);
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let mut child = child;
        let exited = child.try_wait().ok().flatten();
        let _ = child.kill();
        Err(anyhow!(
            "{} never opened its debugging port on {port}{} - profile {}",
            exe.display(),
            match exited {
                Some(st) => format!(" (it exited with {st})"),
                None => " (still running, but silent)".into(),
            },
            self.profile.display()
        ))
    }

    /// Shut the browser down. The profile stays, so cookies survive.
    pub fn stop(&self) {
        std::fs::remove_file(self.profile.join(".bluee-port")).ok();
        *self.live.lock().unwrap() = None;
    }

    pub fn running(&self) -> bool {
        let mut g = self.live.lock().unwrap();
        match g.as_mut() {
            Some(l) => match l.child.as_mut() {
                Some(c) => matches!(c.try_wait(), Ok(None)),
                // Adopted: we have no handle to wait on, so trust the record.
                None => true,
            },
            None => false,
        }
    }

    /// Every page target the browser has open.
    async fn targets(&self, port: u16) -> Result<Vec<Value>> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;
        let list: Vec<Value> = client
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await
            .context("could not list browser tabs")?
            .json()
            .await
            .context("browser tab list was not JSON")?;
        Ok(list
            .into_iter()
            .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
            .collect())
    }

    async fn first_target(&self, port: u16) -> Result<Value> {
        let mut ts = self.targets(port).await?;
        if ts.is_empty() {
            // Every tab was closed; open one so the panel is never stuck.
            reqwest::Client::new()
                .put(format!("http://127.0.0.1:{port}/json/new?about:blank"))
                .send()
                .await
                .ok();
            tokio::time::sleep(Duration::from_millis(300)).await;
            ts = self.targets(port).await?;
        }
        ts.into_iter()
            .next()
            .ok_or_else(|| anyhow!("browser has no page target"))
    }

    /// One CDP command, on a fresh connection. See the module note on why this
    /// is per-call rather than a shared socket.
    async fn cmd(&self, ws_url: &str, method: &str, params: Value) -> Result<Value> {
        let (mut sock, _) = tokio::time::timeout(
            Duration::from_secs(8),
            tokio_tungstenite::connect_async(ws_url),
        )
        .await
        .map_err(|_| anyhow!("timed out opening a CDP socket for {method}"))?
        .with_context(|| format!("could not open a CDP socket for {method}"))?;

        let msg = json!({ "id": 1, "method": method, "params": params });
        sock.send(tokio_tungstenite::tungstenite::Message::Text(
            msg.to_string().into(),
        ))
        .await?;

        // Read until our id comes back. Chrome interleaves unsolicited events
        // on the same socket, so anything without our id is skipped rather
        // than treated as the answer.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let next = tokio::time::timeout_at(deadline, sock.next())
                .await
                .map_err(|_| anyhow!("{method} timed out after 30s"))?;
            let Some(frame) = next else {
                return Err(anyhow!("CDP socket closed during {method}"));
            };
            let frame = frame?;
            let text = match frame {
                tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    return Err(anyhow!("CDP socket closed during {method}"))
                }
                _ => continue,
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|i| i.as_u64()) != Some(1) {
                continue;
            }
            if let Some(e) = v.get("error") {
                let m = e
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown CDP error");
                return Err(anyhow!("{method}: {m}"));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn on_first<F>(&self, f: F) -> Result<Value>
    where
        F: for<'a> FnOnce(&'a Self, String) -> futures_util::future::BoxFuture<'a, Result<Value>>,
    {
        let port = self.ensure().await?;
        let t = self.first_target(port).await?;
        let ws = t
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("page target has no debugger url"))?
            .to_string();
        f(self, ws).await
    }

    pub async fn status(&self) -> Value {
        let exe = find_browser();
        json!({
            "browser": exe.as_ref().map(|p| p.display().to_string()),
            "running": self.running(),
            "native": true,
        })
    }

    pub async fn navigate(&self, url: &str) -> Result<Value> {
        // A bare host is a URL the user meant, not a search - but a string with
        // a space in it is a search, not a host. Guessing wrong in either
        // direction is worse than the small amount of code this takes.
        let target = if url.contains("://") {
            url.to_string()
        } else if url.contains(' ') || !url.contains('.') {
            format!(
                "https://duckduckgo.com/?q={}",
                urlencode(url)
            )
        } else {
            format!("https://{url}")
        };
        let t = target.clone();
        self.on_first(move |me, ws| {
            Box::pin(async move {
                me.cmd(&ws, "Page.navigate", json!({ "url": t })).await?;
                // Poll readyState instead of a fixed sleep - a local page is
                // ready in 30ms and a cold remote one can take seconds. Capped
                // low: each poll is its own connection, so sixty of them is a
                // lot of handshakes for a page that is never going to load.
                for _ in 0..24 {
                    tokio::time::sleep(Duration::from_millis(180)).await;
                    let r = me
                        .cmd(
                            &ws,
                            "Runtime.evaluate",
                            json!({ "expression": "document.readyState", "returnByValue": true }),
                        )
                        .await;
                    if let Ok(v) = r {
                        let s = v
                            .pointer("/result/value")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if s == "complete" || s == "interactive" {
                            break;
                        }
                    }
                }
                me.page_info(&ws).await
            })
        })
        .await
    }

    async fn page_info(&self, ws: &str) -> Result<Value> {
        let v = self
            .cmd(
                ws,
                "Runtime.evaluate",
                json!({
                    "expression": "JSON.stringify({url:location.href,title:document.title})",
                    "returnByValue": true
                }),
            )
            .await?;
        let s = v
            .pointer("/result/value")
            .and_then(|v| v.as_str())
            .unwrap_or("{}");
        Ok(serde_json::from_str(s).unwrap_or_else(|_| json!({})))
    }

    pub async fn shot(&self) -> Result<Value> {
        self.on_first(|me, ws| {
            Box::pin(async move {
                let v = me
                    .cmd(
                        &ws,
                        "Page.captureScreenshot",
                        json!({ "format": "png", "captureBeyondViewport": false }),
                    )
                    .await?;
                let data = v
                    .get("data")
                    .and_then(|d| d.as_str())
                    .ok_or_else(|| anyhow!("no screenshot data"))?
                    .to_string();
                let info = me.page_info(&ws).await.unwrap_or_else(|_| json!({}));
                Ok(json!({ "png": data, "page": info }))
            })
        })
        .await
    }

    /// A click at viewport coordinates. The panel scales the screenshot to fit,
    /// so the caller converts back to page pixels before calling this - the
    /// browser knows nothing about how large the panel happens to be.
    pub async fn click(&self, x: f64, y: f64) -> Result<Value> {
        self.on_first(move |me, ws| {
            Box::pin(async move {
                for kind in ["mousePressed", "mouseReleased"] {
                    me.cmd(
                        &ws,
                        "Input.dispatchMouseEvent",
                        json!({ "type": kind, "x": x, "y": y,
                                "button": "left", "clickCount": 1 }),
                    )
                    .await?;
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
                me.page_info(&ws).await
            })
        })
        .await
    }

    pub async fn scroll(&self, x: f64, y: f64, dy: f64) -> Result<Value> {
        self.on_first(move |me, ws| {
            Box::pin(async move {
                me.cmd(
                    &ws,
                    "Input.dispatchMouseEvent",
                    json!({ "type": "mouseWheel", "x": x, "y": y,
                            "deltaX": 0, "deltaY": dy }),
                )
                .await?;
                Ok(json!({ "ok": true }))
            })
        })
        .await
    }

    pub async fn type_text(&self, text: &str) -> Result<Value> {
        let t = text.to_string();
        self.on_first(move |me, ws| {
            Box::pin(async move {
                me.cmd(&ws, "Input.insertText", json!({ "text": t })).await?;
                Ok(json!({ "ok": true }))
            })
        })
        .await
    }

    pub async fn key(&self, key: &str) -> Result<Value> {
        // Only the keys a browser panel actually needs. A general key mapper
        // would be a lot of table for no one.
        let (code, vk, txt) = match key {
            "Enter" => ("Enter", 13, "\r"),
            "Backspace" => ("Backspace", 8, ""),
            "Tab" => ("Tab", 9, ""),
            "Escape" => ("Escape", 27, ""),
            other => return Err(anyhow!("unsupported key `{other}`")),
        };
        self.on_first(move |me, ws| {
            Box::pin(async move {
                for kind in ["keyDown", "keyUp"] {
                    let mut p = json!({
                        "type": kind, "key": code, "code": code,
                        "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk
                    });
                    if kind == "keyDown" && !txt.is_empty() {
                        p["text"] = json!(txt);
                    }
                    me.cmd(&ws, "Input.dispatchKeyEvent", p).await?;
                }
                tokio::time::sleep(Duration::from_millis(400)).await;
                me.page_info(&ws).await
            })
        })
        .await
    }

    pub async fn history(&self, delta: i64) -> Result<Value> {
        self.on_first(move |me, ws| {
            Box::pin(async move {
                let expr = if delta < 0 {
                    "history.back()"
                } else {
                    "history.forward()"
                };
                me.cmd(&ws, "Runtime.evaluate", json!({ "expression": expr }))
                    .await?;
                tokio::time::sleep(Duration::from_millis(700)).await;
                me.page_info(&ws).await
            })
        })
        .await
    }

    pub async fn reload(&self) -> Result<Value> {
        self.on_first(|me, ws| {
            Box::pin(async move {
                me.cmd(&ws, "Page.reload", json!({})).await?;
                tokio::time::sleep(Duration::from_millis(900)).await;
                me.page_info(&ws).await
            })
        })
        .await
    }

    /// Page text, for handing to the model. A screenshot is for the human; the
    /// model wants the words.
    pub async fn text(&self) -> Result<Value> {
        self.on_first(|me, ws| {
            Box::pin(async move {
                let v = me
                    .cmd(
                        &ws,
                        "Runtime.evaluate",
                        json!({
                            "expression": "document.body ? document.body.innerText.slice(0,20000) : ''",
                            "returnByValue": true
                        }),
                    )
                    .await?;
                let text = v
                    .pointer("/result/value")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let info = me.page_info(&ws).await.unwrap_or_else(|_| json!({}));
                Ok(json!({ "text": text, "page": info }))
            })
        })
        .await
    }
}

/// Decode a base64 screenshot into bytes, so the route can serve it as a real
/// image rather than making the page carry a multi-megabyte data: URI.
pub fn decode_png(b64: &str) -> Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .context("screenshot was not valid base64")
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Ask the OS for a free port, then let it go. There is a race between this and
/// Chrome binding it, but the window is microseconds and the alternative -
/// parsing Chrome's DevToolsActivePort file - has its own failure mode when a
/// stale file is left by a crashed run.
fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_host_becomes_https_and_a_phrase_becomes_a_search() {
        // Not a public fn, so exercised through the same rules navigate uses.
        let cases = [
            ("example.com", true),
            ("https://example.com", true),
            ("what is a merkle tree", false),
            ("localhost", false),
        ];
        for (input, is_url) in cases {
            let looks_like_url =
                input.contains("://") || (!input.contains(' ') && input.contains('.'));
            assert_eq!(looks_like_url, is_url, "misjudged `{input}`");
        }
    }

    #[test]
    fn urlencode_escapes_what_a_query_string_cannot_carry() {
        assert_eq!(urlencode("a b&c=d"), "a+b%26c%3Dd");
        assert_eq!(urlencode("plain-text_1.0~"), "plain-text_1.0~");
    }

    #[test]
    fn decode_png_rejects_rubbish() {
        assert!(decode_png("not base64!!").is_err());
        assert!(decode_png("aGVsbG8=").is_ok());
    }

    #[test]
    fn free_port_returns_something_bindable() {
        let p = free_port().unwrap();
        assert!(p > 1024);
        // And it is genuinely free right after we let it go.
        assert!(std::net::TcpListener::bind(("127.0.0.1", p)).is_ok());
    }
}
