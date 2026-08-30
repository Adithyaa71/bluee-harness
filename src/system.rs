//! System actions: open a file, open an app, run a command.
//!
//! This is the most dangerous thing in the harness, so the design is explicit
//! about where the limits are and where they are not.
//!
//! **Bounded:** every command runs with its working directory set to a granted
//! folder (§4f-c / `roots.rs`). The model cannot grant itself a folder, so it
//! cannot choose where it runs from - only what it runs there.
//!
//! **Not bounded:** a shell can reach the whole machine regardless of its cwd.
//! `cd C:\` works. This is not a sandbox and must not be mistaken for one. What
//! it gives you instead is *visibility*: every command is a logged tool call
//! with its full text, exit code and output, so nothing happens that you cannot
//! read back afterwards. Adithya has said he builds his own guards; the
//! harness's job here is to make actions legible, not to invent a policy layer
//! that would fight his.
//!
//! The one thing refused outright is a short list of commands that destroy a
//! machine in a single line, unless `confirm: true` is passed. That is a guard
//! against a slip, not against intent.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// Commands that ruin a machine in one line. Matched loosely on purpose - a
/// false positive costs one `confirm: true`, a false negative costs the disk.
const CATASTROPHIC: &[&str] = &[
    "format ",
    "rd /s",
    "rmdir /s",
    "del /f /s /q c:",
    "rm -rf /",
    "rm -rf c:",
    "mkfs",
    "diskpart",
    "cipher /w",
    "vssadmin delete",
    "bcdedit",
    "reg delete hklm",
    "shutdown",
    ":(){:|:&};:",
];

pub fn looks_catastrophic(command: &str) -> Option<&'static str> {
    let c = command.to_lowercase();
    CATASTROPHIC.iter().copied().find(|pat| c.contains(pat))
}

/// Result of running something, shaped for the model to read.
#[derive(Debug, serde::Serialize)]
pub struct RunOutput {
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub secs: f32,
}

/// How much output goes back into the prompt. A build log is megabytes and
/// would blow the context on one call.
const MAX_OUT: usize = 20_000;

fn clip(s: &str) -> (String, bool) {
    if s.len() <= MAX_OUT {
        return (s.to_string(), false);
    }
    // Keep both ends: the command echo is at the top, the error is at the
    // bottom, and the middle of a long log is the least useful part.
    let head: String = s.chars().take(MAX_OUT / 2).collect();
    let tail: String = s
        .chars()
        .skip(s.chars().count().saturating_sub(MAX_OUT / 2))
        .collect();
    (format!("{head}\n\n… [middle trimmed] …\n\n{tail}"), true)
}

/// Run a command in a directory and wait for it, bounded by a timeout.
pub fn run(command: &str, cwd: &Path, timeout_secs: u64) -> Result<RunOutput> {
    if command.trim().is_empty() {
        bail!("empty command");
    }
    let started = std::time::Instant::now();

    // Through the platform shell on purpose: pipes, redirects and built-ins are
    // most of why you would want this at all.
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("powershell.exe");
        c.args(["-NoLogo", "-NonInteractive", "-NoProfile", "-Command", command]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-lc", command]);
        c
    };

    let mut child = cmd
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting: {command}"))?;

    // Poll rather than block forever: a command that hangs must not take the
    // whole turn with it.
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs.max(1));
    let status = loop {
        match child.try_wait()? {
            Some(s) => break Some(s),
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                break None;
            }
            None => std::thread::sleep(Duration::from_millis(60)),
        }
    };

    let out = child.wait_with_output()?;
    let (stdout, t1) = clip(&String::from_utf8_lossy(&out.stdout));
    let (mut stderr, t2) = clip(&String::from_utf8_lossy(&out.stderr));
    if status.is_none() {
        stderr = format!("[killed after {timeout_secs}s]\n{stderr}");
    }

    Ok(RunOutput {
        command: command.to_string(),
        cwd: crate::roots::pretty(cwd),
        exit_code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        truncated: t1 || t2,
        secs: started.elapsed().as_secs_f32(),
    })
}

/// Open a path with whatever the OS uses for it - a file in its editor, a
/// folder in Explorer. Fire and forget; it is a GUI action, not a query.
pub fn open_path(path: &Path) -> Result<String> {
    if !path.exists() {
        bail!("no such path: {}", path.display());
    }
    #[cfg(windows)]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
            .with_context(|| format!("opening {}", path.display()))?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(path)
            .spawn()
            .with_context(|| format!("opening {}", path.display()))?;
    }
    Ok(crate::roots::pretty(path))
}

/// Launch an application by name or full path (`notepad`, `code`, `chrome`).
pub fn open_app(app: &str, args: &[String]) -> Result<String> {
    if app.trim().is_empty() {
        bail!("no application named");
    }
    let mut cmd = std::process::Command::new(app);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .with_context(|| format!("launching `{app}` - is it on PATH?"))?;
    Ok(format!(
        "{app}{}",
        if args.is_empty() {
            String::new()
        } else {
            format!(" {}", args.join(" "))
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_in_the_directory_it_is_given() {
        let dir = std::env::temp_dir();
        let out = run(if cfg!(windows) { "echo hello" } else { "echo hello" }, &dir, 20).unwrap();
        assert!(out.stdout.contains("hello"), "got {:?}", out.stdout);
        assert_eq!(out.exit_code, Some(0));
    }

    #[test]
    fn reports_a_failing_command_rather_than_erroring() {
        let out = run("exit 3", &std::env::temp_dir(), 20).unwrap();
        assert_eq!(out.exit_code, Some(3));
    }

    #[test]
    fn a_hanging_command_is_killed_not_waited_on() {
        let cmd = if cfg!(windows) { "Start-Sleep -Seconds 30" } else { "sleep 30" };
        let out = run(cmd, &std::env::temp_dir(), 2).unwrap();
        assert!(out.secs < 10.0, "should have been killed quickly, took {}", out.secs);
        assert!(out.stderr.contains("killed after"));
    }

    #[test]
    fn flags_the_commands_that_end_a_machine() {
        assert!(looks_catastrophic("format C: /q").is_some());
        assert!(looks_catastrophic("rm -rf / --no-preserve-root").is_some());
        assert!(looks_catastrophic("shutdown /r /t 0").is_some());
        assert!(looks_catastrophic("cargo build --release").is_none());
        assert!(looks_catastrophic("git status").is_none());
    }

    #[test]
    fn long_output_is_clipped_from_the_middle() {
        let (s, t) = clip(&"x".repeat(MAX_OUT * 2));
        assert!(t);
        assert!(s.contains("middle trimmed"));
        assert!(s.len() < MAX_OUT + 200);
    }
}
