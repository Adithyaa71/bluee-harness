//! Shared terminal (§4f) - real PTYs both Adithya and the model can see.
//!
//! Sessions live in the manager, not in the websocket. Closing the tab, or the
//! browser, does not kill the shell: reattaching replays the scrollback and
//! resumes the same process. That is the "come back after days and it's still
//! there" behaviour, without reimplementing tmux - and where real tmux is
//! available, running it *inside* one of these is still the better answer for
//! persistence across a machine restart.
//!
//! Any shell works, because a PTY just runs whatever binary you hand it:
//! powershell, cmd, bash, or `ssh pi@...` straight to the Pi 5.

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// How much output to retain per session for replay on reattach.
const SCROLLBACK_LIMIT: usize = 256 * 1024;

pub struct PtySession {
    pub id: String,
    pub shell: String,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    /// Held for the life of the session. Dropping the child handle closes
    /// handles the shell needs - that is what made the first version emit a
    /// cursor-position query and then nothing at all.
    _child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    /// Live output. Late subscribers get scrollback first, then this.
    tx: broadcast::Sender<Vec<u8>>,
    scrollback: Arc<Mutex<Vec<u8>>>,
}

impl PtySession {
    fn spawn(id: &str, shell: &str, rows: u16, cols: u16) -> Result<Arc<Self>> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("opening pty")?;

        // A bare shell name is enough - CommandBuilder resolves it on PATH,
        // which is what makes `ssh`, `bash`, `cmd` all work identically.
        // Split on whitespace so "ssh pi@host" works as a single entry.
        let mut parts = shell.split_whitespace();
        let program = parts.next().unwrap_or("cmd.exe");
        let mut cmd = CommandBuilder::new(program);
        for arg in parts {
            cmd.arg(arg);
        }
        // PowerShell prints a banner and can sit waiting; -NoLogo keeps the
        // first screen useful.
        if program.eq_ignore_ascii_case("powershell.exe") && shell.split_whitespace().count() == 1 {
            cmd.arg("-NoLogo");
        }
        cmd.cwd(std::env::current_dir().unwrap_or_else(|_| ".".into()));

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawning `{shell}`"))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("cloning pty reader")?;
        let writer = pair.master.take_writer().context("taking pty writer")?;

        let (tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let scrollback = Arc::new(Mutex::new(Vec::<u8>::new()));

        // The PTY reader is blocking, so it gets a real thread rather than a
        // task. It ends when the shell exits and closes the fd.
        {
            let tx = tx.clone();
            let scrollback = scrollback.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let chunk = buf[..n].to_vec();
                            if let Ok(mut sb) = scrollback.lock() {
                                sb.extend_from_slice(&chunk);
                                if sb.len() > SCROLLBACK_LIMIT {
                                    let cut = sb.len() - SCROLLBACK_LIMIT;
                                    sb.drain(..cut);
                                }
                            }
                            // Err just means nobody is attached right now; the
                            // shell should keep running regardless.
                            let _ = tx.send(chunk);
                        }
                    }
                }
            });
        }

        Ok(Arc::new(Self {
            id: id.to_string(),
            shell: shell.to_string(),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            _child: Mutex::new(child),
            tx,
            scrollback,
        }))
    }

    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        let mut w = self.writer.lock().map_err(|_| anyhow::anyhow!("pty writer poisoned"))?;
        w.write_all(bytes).context("writing to pty")?;
        w.flush().context("flushing pty")?;
        Ok(())
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let m = self.master.lock().map_err(|_| anyhow::anyhow!("pty master poisoned"))?;
        m.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("resizing pty")?;
        Ok(())
    }

    /// Everything printed so far, for replay when a client attaches.
    pub fn scrollback(&self) -> Vec<u8> {
        self.scrollback.lock().map(|s| s.clone()).unwrap_or_default()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.tx.subscribe()
    }
}

#[derive(Default)]
pub struct PtyManager {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
}

impl PtyManager {
    /// Attach to `id`, creating it if it does not exist yet.
    pub fn attach(&self, id: &str, shell: &str, rows: u16, cols: u16) -> Result<Arc<PtySession>> {
        let mut map = self
            .sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("pty manager poisoned"))?;

        if let Some(existing) = map.get(id) {
            return Ok(existing.clone());
        }
        let session = PtySession::spawn(id, shell, rows, cols)?;
        map.insert(id.to_string(), session.clone());
        Ok(session)
    }

    pub fn list(&self) -> Vec<(String, String)> {
        self.sessions
            .lock()
            .map(|m| {
                m.values()
                    .map(|s| (s.id.clone(), s.shell.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn close(&self, id: &str) {
        if let Ok(mut m) = self.sessions.lock() {
            m.remove(id);
        }
    }
}
