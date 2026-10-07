//! Hooks: commands Adithya writes that run around every tool call.
//!
//! §21 is why this exists. A confirmation flag the model could set itself was
//! set by the model itself, and `format C: /q` ran. The fix then was specific -
//! delete the flag, hard-refuse a denylist. This is the general form: a gate
//! that runs OUTSIDE the model's turn, so nothing the model says can reach it.
//!
//! It is also the harness side of "he builds the guards himself" (§4f): the
//! harness does not decide policy, it gives his policy a place to run. A hook
//! is any command - a Python script, a PowerShell one-liner, a compiled binary.
//!
//! `hooks.json` at the repo root (or `HARNESS_HOOKS`):
//!
//! ```json
//! {
//!   "pre_tool":  [{ "match": "harness__run_command|uacc__*", "command": "python hooks/guard.py" }],
//!   "post_tool": [{ "match": "harness__create_artifact",     "command": "python hooks/lint.py" }]
//! }
//! ```
//!
//! - `match` is against `server__tool` (native tools are `harness__<name>`),
//!   `*` is a wildcard and `|` separates alternatives.
//! - The call arrives as JSON on stdin: `{event, server, tool, args, session}`,
//!   plus `ok` and `result` for `post_tool`.
//! - **pre_tool: exit 0 allows. Anything else blocks**, and what the hook
//!   printed is handed to the model as the reason. A hook that crashes or
//!   times out also blocks - a guard that fails open is not a guard.
//! - **post_tool never blocks.** Whatever it prints is attached to the tool
//!   result the model sees, as `hook_output`. That is how compiler feedback
//!   gets into the loop: a post-hook on file writes that runs `cargo check`
//!   hands the errors straight back (RESEARCH.md §7).
//!
//! The file is re-read on every call, so an edit takes effect on the next tool
//! call with no restart. It is small; the read costs nothing next to the call.

use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Deserialize, Default)]
struct HookFile {
    #[serde(default)]
    pre_tool: Vec<Hook>,
    #[serde(default)]
    post_tool: Vec<Hook>,
}

#[derive(Debug, Deserialize, Clone)]
struct Hook {
    #[serde(rename = "match")]
    pattern: String,
    command: String,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
}

fn default_timeout() -> u64 {
    30
}

/// Hook output handed to the model is clipped: a build log would otherwise
/// eat the context window on every write.
const MAX_OUT: usize = 6000;

fn path() -> PathBuf {
    std::env::var("HARNESS_HOOKS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("hooks.json"))
}

/// `Ok(None)` when there is no hooks file, which is the common case and must
/// cost nothing. A file that exists but does not parse is an error, not
/// "no hooks" - silently dropping his guards because of a stray comma would
/// be the worst possible failure for this feature.
fn load() -> Result<Option<HookFile>, String> {
    let p = path();
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("{} does not parse: {e}", p.display()))
}

/// `*` wildcard glob, `|` alternatives. Case-insensitive.
pub fn matches(pattern: &str, name: &str) -> bool {
    fn glob(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && glob(p, &s[1..])),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => glob(&p[1..], &s[1..]),
            _ => false,
        }
    }
    pattern
        .split('|')
        .map(str::trim)
        .any(|alt| !alt.is_empty() && glob(alt.as_bytes(), name.as_bytes()))
}

struct Ran {
    code: Option<i32>,
    output: String,
}

async fn run(hook: &Hook, input: &serde_json::Value) -> Result<Ran, String> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        // raw_arg: cmd has its own quoting rules, and letting Rust re-quote
        // the command breaks any hook that contains quotes of its own.
        c.arg("/C").raw_arg(&hook.command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(&hook.command);
        c
    };
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // No console window flashing up on every tool call.
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| format!("could not start `{}`: {e}", hook.command))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.to_string().as_bytes()).await;
    }
    let out = tokio::time::timeout(Duration::from_secs(hook.timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| format!("`{}` timed out after {}s", hook.command, hook.timeout_secs))?
        .map_err(|e| format!("`{}` failed: {e}", hook.command))?;

    let mut output = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&err);
    }
    if output.len() > MAX_OUT {
        let mut cut = MAX_OUT;
        while !output.is_char_boundary(cut) {
            cut -= 1;
        }
        output.truncate(cut);
        output.push_str("\n[... clipped]");
    }
    Ok(Ran { code: out.status.code(), output })
}

/// Run every matching pre-tool hook. `Err(reason)` means the call is blocked.
pub async fn pre_tool(
    server: &str,
    tool: &str,
    args: &serde_json::Value,
    session: &str,
) -> Result<(), String> {
    let file = match load() {
        Ok(Some(f)) => f,
        Ok(None) => return Ok(()),
        // Fail closed: a broken guard file stops tools rather than silently
        // removing every guard in it.
        Err(e) => return Err(format!("hooks are misconfigured, so no tool may run: {e}")),
    };
    let name = format!("{server}__{tool}");
    let input = serde_json::json!({
        "event": "pre_tool", "server": server, "tool": tool, "args": args, "session": session,
    });
    for hook in file.pre_tool.iter().filter(|h| matches(&h.pattern, &name)) {
        let ran = run(hook, &input).await.map_err(|e| format!("guard hook failed, so blocked: {e}"))?;
        if ran.code != Some(0) {
            let why = if ran.output.is_empty() { "no reason given".into() } else { ran.output };
            return Err(format!("blocked by hook `{}`: {why}", hook.command));
        }
    }
    Ok(())
}

/// Run every matching post-tool hook and collect what they printed.
pub async fn post_tool(
    server: &str,
    tool: &str,
    args: &serde_json::Value,
    ok: bool,
    result: &serde_json::Value,
    session: &str,
) -> Option<String> {
    let file = load().ok().flatten()?;
    let name = format!("{server}__{tool}");
    let input = serde_json::json!({
        "event": "post_tool", "server": server, "tool": tool, "args": args,
        "ok": ok, "result": result, "session": session,
    });
    let mut notes = Vec::new();
    for hook in file.post_tool.iter().filter(|h| matches(&h.pattern, &name)) {
        match run(hook, &input).await {
            Ok(ran) if !ran.output.is_empty() => notes.push(ran.output),
            Ok(_) => {}
            Err(e) => notes.push(format!("[hook error] {e}")),
        }
    }
    (!notes.is_empty()).then(|| notes.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(matches("uacc__*", "uacc__click"));
        assert!(matches("harness__run_command|uacc__*", "harness__run_command"));
        assert!(matches("*__delete_*", "harness__delete_file"));
        assert!(matches("HARNESS__RUN_COMMAND", "harness__run_command"));
        assert!(!matches("uacc__*", "snarevec__browser_click"));
        assert!(!matches("", "anything"));
        assert!(!matches("harness__run", "harness__run_command"));
    }
}
