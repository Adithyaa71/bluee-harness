//! Guard: the dashboard's JavaScript must actually parse.
//!
//! `dash/index.html` is `include_str!`'d into the binary, so nothing ever
//! validates it - a syntax error compiles fine, ships, and then the whole page
//! silently does nothing. That happened once already: a stray real newline
//! inside a single-quoted string killed the script, and the only symptom was
//! the window sitting on "connecting…" forever, with every API endpoint
//! answering perfectly when probed directly.
//!
//! This shells out to `node --check`. If node is not installed the test skips
//! rather than failing, since node is not otherwise a dependency of this build.

use std::io::Write;
use std::process::Command;

/// Pull the inline application script out of the page.
fn app_script(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find("<script") {
        let after = &rest[open..];
        let Some(gt) = after.find('>') else { break };
        let tag = &after[..gt];
        let Some(close) = after.find("</script>") else { break };
        // Skip <script src="..."> - only the inline app code is ours.
        if !tag.contains("src=") {
            out.push_str(&after[gt + 1..close]);
            out.push('\n');
        }
        rest = &after[close + "</script>".len()..];
    }
    out
}

#[test]
fn dashboard_javascript_parses() {
    let html = include_str!("../dash/index.html");
    let js = app_script(html);
    assert!(
        js.len() > 5_000,
        "expected to find the dashboard's inline script, got {} chars",
        js.len()
    );

    let node = if cfg!(windows) { "node.exe" } else { "node" };
    if Command::new(node).arg("--version").output().is_err() {
        eprintln!("node not installed - skipping dashboard JS syntax check");
        return;
    }

    let path = std::env::temp_dir().join(format!("bluee-dash-{}.js", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("writing temp js");
    f.write_all(js.as_bytes()).expect("writing temp js");
    drop(f);

    let out = Command::new(node)
        .arg("--check")
        .arg(&path)
        .output()
        .expect("running node --check");
    let _ = std::fs::remove_file(&path);

    assert!(
        out.status.success(),
        "dash/index.html has a JavaScript syntax error - the page would load and do \
         nothing:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
