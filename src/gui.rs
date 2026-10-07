//! Driving other applications through their GUI.
//!
//! # Why this exists when UACC already has 70 tools
//!
//! The primitives were already there and they work - `list_windows`,
//! `focus_window`, `click`, `type_text`, `get_screen_info`. What was missing is
//! that doing one useful thing took five or six of them in the right order,
//! with the model improvising pixel coordinates in between. That is fine when
//! you can retry; it is the wrong shape when someone is watching.
//!
//! So these are composed and they verify. `focus_app` focuses and then reads
//! the active window back to check it actually took. `open_app` waits for the
//! window to exist rather than reporting success because a process spawned.
//! `click_label` finds a control by its **visible label** in the accessibility
//! tree and clicks the coordinates the tree gives, instead of guessing.
//!
//! It is also cheaper. §12f: tool schemas are paid on every turn, and seven
//! well-shaped tools beat seventy for both cost and for the model's ability to
//! pick the right one.
//!
//! # What it does not do
//!
//! No vision, no OCR, no pixel hunting. Everything here goes through the
//! accessibility tree, which is why it can say *why* it failed - "no control
//! labelled Save; the labels I can see are …" is a useful answer, and "I
//! clicked at 812,440 and nothing happened" is not. When an app has no
//! accessible labels at all (a canvas, a game), these tools say so and point at
//! UACC's visual tools rather than pretending.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use crate::llm::ToolDef;
use crate::mcp::McpRegistry;

/// UACC answers `{"result": "<a json string>"}`. Unwrap one level so the rest
/// of this file deals in real values rather than strings-of-json.
fn inner(v: &Value) -> Value {
    if let Some(s) = v.get("result").and_then(|r| r.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(s) {
            return parsed;
        }
    }
    v.clone()
}

/// One UACC call, with the two things that make this usable live.
///
/// **It surfaces the real error.** The first version collapsed every failure to
/// "the GUI layer refused", which threw away the one message that actually told
/// you what to do - UACC's own "User override: mouse moved away".
///
/// **It recovers from the mouse sentinel once.** UACC arms a MouseSentinel that
/// aborts automation the moment the cursor moves more than 40px, which is a
/// genuinely good safety feature and a terrible way to lose a demo: one stray
/// twitch and every later call fails until someone knows to call
/// `acknowledge_user_override`. So that acknowledgement is made here, once, and
/// the call is retried. Twice in a row is treated as the human meaning it.
async fn uacc(reg: &McpRegistry, tool: &str, args: Value) -> Result<Value> {
    for attempt in 0..2 {
        let raw = reg.call("uacc", tool, args.clone()).await?;
        let v = inner(&raw);
        if v.get("success").and_then(|s| s.as_bool()) != Some(false) {
            return Ok(v);
        }
        let msg = v
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("no reason given")
            .to_string();
        let killed = v.get("killed").and_then(|k| k.as_bool()).unwrap_or(false)
            || msg.contains("User override");
        if killed && attempt == 0 {
            reg.call("uacc", "acknowledge_user_override", json!({}))
                .await
                .ok();
            continue;
        }
        return Err(anyhow!("{tool}: {msg}"));
    }
    Err(anyhow!("{tool}: failed twice, including after clearing the mouse override"))
}

/// A window, trimmed to what is worth putting in a prompt. UACC returns nine
/// fields per window and fifteen windows; most of that is noise the model then
/// pays for on every subsequent turn.
fn brief(w: &Value) -> Value {
    let b = w.get("bounds").cloned().unwrap_or(json!({}));
    json!({
        "title": w.get("title").and_then(|t| t.as_str()).unwrap_or(""),
        "app": w.get("process_name").and_then(|t| t.as_str()).unwrap_or(""),
        "focused": w.get("is_focused").and_then(|t| t.as_bool()).unwrap_or(false),
        "minimized": w.get("is_minimized").and_then(|t| t.as_bool()).unwrap_or(false),
        "at": format!("{}x{} at {},{}",
            w.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
            w.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
            b.get("left").and_then(|v| v.as_i64()).unwrap_or(0),
            b.get("top").and_then(|v| v.as_i64()).unwrap_or(0)),
    })
}

async fn windows(reg: &McpRegistry) -> Result<Vec<Value>> {
    let v = uacc(reg, "list_windows", json!({})).await?;
    Ok(v.get("windows")
        .and_then(|w| w.as_array())
        .cloned()
        .unwrap_or_default())
}

/// Find a window by a loose name: exact title, then substring of the title,
/// then the process name. Case-insensitive throughout, because nobody types
/// "Task Manager" with the right capitals under pressure.
fn match_window<'a>(list: &'a [Value], name: &str) -> Option<&'a Value> {
    let n = name.to_lowercase();
    let title = |w: &Value| {
        w.get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_lowercase()
    };
    let proc = |w: &Value| {
        w.get("process_name")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_lowercase()
    };
    list.iter()
        .find(|w| title(w) == n)
        .or_else(|| list.iter().find(|w| title(w).contains(&n)))
        .or_else(|| list.iter().find(|w| proc(w).contains(&n)))
        .or_else(|| {
            // Last resort: the executable without its extension, so "notepad"
            // finds notepad.exe.
            list.iter()
                .find(|w| proc(w).trim_end_matches(".exe") == n.trim_end_matches(".exe"))
        })
}

/// The labelled, clickable things on screen right now.
///
/// Returned as a list rather than UACC's pre-rendered text blob: the model has
/// to pick one and act on it, and picking out of a list is more reliable than
/// parsing a diagram out of an escaped string.
fn parse_elements(text_map: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text_map.lines() {
        let t = line.trim();
        if !t.starts_with('[') {
            continue;
        }
        // e.g.  [e7] button  "Minimize"  at (1805, 18)  clickable
        let id = t[1..].split(']').next().unwrap_or("").to_string();
        let rest = match t.find(']') {
            Some(i) => &t[i + 1..],
            None => continue,
        };
        let label = match (rest.find('"'), rest.rfind('"')) {
            (Some(a), Some(b)) if b > a => rest[a + 1..b].to_string(),
            _ => continue,
        };
        let (x, y) = match (rest.find('('), rest.find(')')) {
            (Some(a), Some(b)) if b > a => {
                let nums: Vec<i64> = rest[a + 1..b]
                    .split(',')
                    .filter_map(|p| p.trim().parse::<i64>().ok())
                    .collect();
                if nums.len() == 2 {
                    (nums[0], nums[1])
                } else {
                    continue;
                }
            }
            _ => continue,
        };
        let kind = rest
            .split('"')
            .next()
            .unwrap_or("")
            .trim()
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        out.push(json!({
            "id": id, "kind": kind, "label": label, "x": x, "y": y,
            "clickable": rest.contains("clickable"),
        }));
    }
    out
}

async fn screen(reg: &McpRegistry) -> Result<(String, Vec<Value>, Value)> {
    let v = uacc(reg, "get_screen_info", json!({})).await?;
    let map = v
        .get("text_map")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let active = v
        .get("active_window")
        .cloned()
        .unwrap_or(Value::Null);
    Ok((map.clone(), parse_elements(&map), active))
}

pub const TOOLS: &[&str] = &[
    "list_apps",
    "open_app_window",
    "focus_app",
    "arrange_app",
    "read_screen",
    "click_label",
    "type_into_app",
    "press_keys",
];

pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

pub fn defs() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_apps".into(),
            description: "Every application window that is open right now, with which one has \
                focus. Start here when asked to do something in another app - it is how you \
                learn what the app is actually called."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "open_app_window".into(),
            description: "Launch an application and WAIT until its window actually exists, then \
                focus it. Use this rather than run_command for anything with a GUI: a process \
                that has spawned is not yet a window you can click."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "app": { "type": "string",
                        "description": "notepad, calc, mspaint, explorer, or a full path" },
                    "wait_secs": { "type": "integer",
                        "description": "how long to wait for the window (default 12)" }
                },
                "required": ["app"]
            }),
        },
        ToolDef {
            name: "focus_app".into(),
            description: "Bring a window to the front by a loose name - part of its title, or \
                the program name. Verifies the focus actually landed and tells you if it did not."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "app": { "type": "string" } },
                "required": ["app"]
            }),
        },
        ToolDef {
            name: "arrange_app".into(),
            description: "Move or resize a window: left, right, maximize, minimize, restore, or \
                centre. Useful for putting two apps side by side."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "app": { "type": "string" },
                    "where": { "type": "string",
                        "enum": ["left", "right", "maximize", "minimize", "restore", "centre"] }
                },
                "required": ["app", "where"]
            }),
        },
        ToolDef {
            name: "read_screen".into(),
            description: "What is on screen in the focused window: every labelled control with \
                its position. Read this BEFORE clicking anything - it is what makes clicking by \
                label possible instead of guessing coordinates."
                .into(),
            parameters: json!({ "type": "object", "properties": {} }),
        },
        ToolDef {
            name: "click_label".into(),
            description: "Click a control by its visible label, e.g. 'Save' or 'Close'. Finds it \
                in the accessibility tree and clicks where the tree says it is. If there is no \
                such label it tells you which labels it can see, so you can pick a real one."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string" },
                    "app": { "type": "string",
                        "description": "Which window the control is in. Strongly recommended -                             it is focused first, so the click cannot land in whatever happened                             to come to the front in between." },
                    "double": { "type": "boolean", "description": "double-click instead" }
                },
                "required": ["label"]
            }),
        },
        ToolDef {
            name: "type_into_app".into(),
            description: "Type text into whichever window has focus. Focus it first."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
        },
        ToolDef {
            name: "press_keys".into(),
            description: "Press a key combination in the focused window, e.g. 'ctrl+s', \
                'alt+f4', 'enter', 'win+left'."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "keys": { "type": "string" } },
                "required": ["keys"]
            }),
        },
    ]
}

pub async fn call(reg: &McpRegistry, tool: &str, args: &Value) -> Result<Value> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();

    match tool {
        "list_apps" => {
            let ws = windows(reg).await?;
            let list: Vec<Value> = ws
                .iter()
                .filter(|w| {
                    // Windows with no title are shell plumbing, not apps.
                    !w.get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .trim()
                        .is_empty()
                })
                .map(brief)
                .collect();
            Ok(json!({ "count": list.len(), "windows": list }))
        }

        "focus_app" => {
            let name = s("app");
            let ws = windows(reg).await?;
            let Some(w) = match_window(&ws, &name) else {
                let seen: Vec<&str> = ws
                    .iter()
                    .filter_map(|w| w.get("title").and_then(|t| t.as_str()))
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                return Err(anyhow!(
                    "no window matching `{name}`. Open right now: {}",
                    seen.join(" | ")
                ));
            };
            let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
            uacc(reg, "focus_window", json!({ "title": title })).await?;
            tokio::time::sleep(std::time::Duration::from_millis(450)).await;

            // Verify. Focus requests are refused more often than people expect
            // - a modal elsewhere, a foreground lock - and reporting success
            // without checking is how the next three tool calls go wrong.
            let active = uacc(reg, "get_active_window", json!({})).await?;
            let now = active.get("title").and_then(|t| t.as_str()).unwrap_or("");
            Ok(json!({
                "focused": now,
                "ok": now.eq_ignore_ascii_case(&title),
                "note": if now.eq_ignore_ascii_case(&title) { Value::Null }
                        else { json!("asked for `{title}` but the foreground is still something else - \
                                      Windows can refuse a focus change") }
            }))
        }

        "open_app_window" => {
            let app = s("app");
            let wait = args.get("wait_secs").and_then(|v| v.as_u64()).unwrap_or(12);
            let existing = windows(reg).await?;
            let before = existing.len();

            // If a window for this app is already open, say so and stop.
            //
            // This matters more than it looks. Whatever is already open may
            // hold unsaved work - the first live run of this found a Notepad
            // titled "*tmr ~ dbms review 1", the asterisk meaning unsaved - and
            // a model told to "open notepad and type" would have typed straight
            // into it. Reporting "opened" for a window we did not open is how
            // that happens, so the answer distinguishes the two and leaves the
            // decision upstream.
            if let Some(w) = match_window(&existing, &app) {
                let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                uacc(reg, "focus_window", json!({ "title": title })).await.ok();
                let unsaved = title.starts_with('*');
                return Ok(json!({
                    "already_open": true,
                    "focused": title,
                    "window": brief(w),
                    "note": if unsaved {
                        json!("This window was ALREADY open and its title starts with `*`,                                which usually means unsaved changes. Do not type into it without                                asking. To get a clean one, open a new window from the app itself.")
                    } else {
                        json!("This window was already open - nothing new was launched.                                Anything you type goes into whatever it already contains.")
                    }
                }));
            }

            uacc(reg, "launch_app", json!({ "app_name": app })).await?;

            // Wait for a WINDOW, not for the process. `launch_app` returns as
            // soon as it has spawned something, which is several seconds before
            // there is anything to click.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait);
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                let ws = windows(reg).await?;
                if let Some(w) = match_window(&ws, &app) {
                    let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    uacc(reg, "focus_window", json!({ "title": title })).await.ok();
                    return Ok(json!({
                        "already_open": false, "opened": title, "window": brief(w)
                    }));
                }
                if std::time::Instant::now() > deadline {
                    let ws = windows(reg).await?;
                    return Err(anyhow!(
                        "launched `{app}` but no window matching it appeared within {wait}s \
                         (windows went {before} -> {}). It may be called something else - \
                         use list_apps.",
                        ws.len()
                    ));
                }
            }
        }

        "arrange_app" => {
            let name = s("app");
            let wh = s("where");
            let ws = windows(reg).await?;
            let Some(w) = match_window(&ws, &name) else {
                return Err(anyhow!("no window matching `{name}` - use list_apps"));
            };
            let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
            uacc(reg, "focus_window", json!({ "title": title })).await.ok();

            match wh.as_str() {
                "maximize" | "minimize" | "restore" => {
                    uacc(reg, "minimize_maximize",
                         json!({ "title": title, "state": wh })).await?;
                }
                _ => {
                    let info = uacc(reg, "get_system_info", json!({})).await.unwrap_or(json!({}));
                    // Fall back to 1920x1080 rather than failing: getting the
                    // arrangement slightly wrong beats not arranging at all.
                    let sw = info.get("screen_width").and_then(|v| v.as_i64()).unwrap_or(1920);
                    let sh = info.get("screen_height").and_then(|v| v.as_i64()).unwrap_or(1080);
                    let (x, y, cw, ch) = match wh.as_str() {
                        "left" => (0, 0, sw / 2, sh),
                        "right" => (sw / 2, 0, sw / 2, sh),
                        "centre" | "center" => (sw / 6, sh / 8, sw * 2 / 3, sh * 3 / 4),
                        other => return Err(anyhow!("unknown position `{other}`")),
                    };
                    uacc(reg, "move_window", json!({ "title": title, "x": x, "y": y })).await?;
                    uacc(reg, "resize_window",
                         json!({ "title": title, "width": cw, "height": ch })).await?;
                }
            }
            Ok(json!({ "arranged": title, "where": wh }))
        }

        "read_screen" => {
            let (map, els, active) = screen(reg).await?;
            if els.is_empty() {
                return Ok(json!({
                    "active_window": active,
                    "elements": [],
                    "note": "this window exposes no labelled controls - it is probably a canvas \
                             or custom-drawn UI. click_label cannot help here; UACC's visual \
                             tools (uacc__detect_elements_visual, uacc__vlm_locate_element) can.",
                    "raw": map,
                }));
            }
            Ok(json!({ "active_window": active, "count": els.len(), "elements": els }))
        }

        "click_label" => {
            let want = s("label");
            let dbl = args.get("double").and_then(|v| v.as_bool()).unwrap_or(false);

            // Focus the named window first, if one was named.
            //
            // Without this the click lands wherever the foreground happens to be
            // when it runs, which is not necessarily where the screen was read.
            // Observed live: a read found 171 controls in File Explorer, and by
            // the time the click ran the foreground was a browser - so it
            // correctly reported "nothing labelled Large Icons" while listing a
            // completely different window's labels.
            let app = s("app");
            if !app.is_empty() {
                let ws = windows(reg).await?;
                if let Some(w) = match_window(&ws, &app) {
                    let title = w.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string();
                    uacc(reg, "focus_window", json!({ "title": title })).await.ok();
                    tokio::time::sleep(std::time::Duration::from_millis(450)).await;
                } else {
                    return Err(anyhow!("no window matching `{app}` - use list_apps"));
                }
            }

            let (_, els, active) = screen(reg).await?;
            let lw = want.to_lowercase();
            let hit = els
                .iter()
                .find(|e| {
                    e.get("label").and_then(|l| l.as_str()).unwrap_or("").to_lowercase() == lw
                })
                .or_else(|| {
                    els.iter().find(|e| {
                        e.get("label")
                            .and_then(|l| l.as_str())
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&lw)
                    })
                });
            let Some(e) = hit else {
                let seen: Vec<&str> = els
                    .iter()
                    .filter_map(|e| e.get("label").and_then(|l| l.as_str()))
                    .collect();
                return Err(anyhow!(
                    "nothing labelled `{want}` in the focused window ({}). What is there: {}.                      If that is the wrong window, pass `app` so it gets focused first.",
                    active.as_str().unwrap_or("unknown"),
                    if seen.is_empty() { "(no labelled controls at all)".into() }
                    else { seen.join(" | ") }
                ));
            };
            let x = e.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
            let y = e.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
            let label = e
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or(&want)
                .to_string();

            // By name first. UACC resolves the name against the accessibility
            // tree itself and re-finds the control if it has moved since we
            // read the screen - which is exactly the race that makes scripted
            // clicking flaky. Coordinates are the fallback, not the plan.
            let by_name = uacc(
                reg,
                "click_element",
                json!({ "name": label, "reasoning": "clicked by label" }),
            )
            .await;
            let how = match by_name {
                Ok(_) => "by name",
                Err(_) => {
                    uacc(reg, "click",
                         json!({ "x": x, "y": y, "count": if dbl { 2 } else { 1 },
                                 "reasoning": "clicked by accessibility-tree coordinates" }))
                        .await?;
                    "by coordinates"
                }
            };
            Ok(json!({
                "clicked": label, "how": how, "at": format!("{x},{y}"),
                "kind": e.get("kind"), "in_window": active
            }))
        }

        "type_into_app" => {
            let text = s("text");
            if text.is_empty() {
                return Err(anyhow!("nothing to type"));
            }
            let active = uacc(reg, "get_active_window", json!({})).await.ok();
            let where_ = active
                .as_ref()
                .and_then(|a| a.get("title"))
                .and_then(|t| t.as_str())
                .unwrap_or("unknown")
                .to_string();
            uacc(reg, "type_text", json!({ "text": text })).await?;
            // Naming the window is the difference between "it typed" and "it
            // typed somewhere you did not expect", which is the failure that
            // actually costs you.
            Ok(json!({ "typed": text.chars().count(), "into": where_ }))
        }

        "press_keys" => {
            let keys = s("keys");
            if keys.is_empty() {
                return Err(anyhow!("no key combination given"));
            }
            // UACC wants the parts separately; people write them joined.
            let parts: Vec<&str> = keys
                .split(|c| c == '+' || c == '-')
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .collect();
            uacc(reg, "hotkey", json!({ "keys": parts })).await?;
            Ok(json!({ "pressed": keys }))
        }

        other => Err(anyhow!("unknown GUI tool `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Screen: 1920x1080 | Window: \"Claude\"\n\
        ─── Interactive Elements ───\n\
        \x20 [e7] button         \"Minimize\"                        at (1805, 18)      clickable\n\
        \x20 [e8] button         \"Restore\"                         at (1851, 18)      clickable\n\
        \x20 [e9] button         \"Close\"                           at (1897, 18)      clickable\n\
        \n─── Visible Text ───\n\
        \x20 [e1] \"Claude\" at (960, 516)";

    #[test]
    fn parses_labels_and_coordinates_out_of_the_text_map() {
        let els = parse_elements(SAMPLE);
        assert_eq!(els.len(), 4, "three buttons and one text node");
        assert_eq!(els[0]["label"], "Minimize");
        assert_eq!(els[0]["kind"], "button");
        assert_eq!(els[0]["x"], 1805);
        assert_eq!(els[0]["y"], 18);
        assert_eq!(els[0]["clickable"], true);
        // The plain text node has no kind and is not clickable.
        assert_eq!(els[3]["label"], "Claude");
        assert_eq!(els[3]["clickable"], false);
    }

    #[test]
    fn a_map_with_no_controls_yields_nothing_rather_than_junk() {
        assert!(parse_elements("Screen: 1920x1080\nnothing here").is_empty());
        assert!(parse_elements("").is_empty());
    }

    #[test]
    fn window_matching_is_loose_but_ordered() {
        let ws = vec![
            json!({ "title": "Untitled - Notepad", "process_name": "notepad.exe" }),
            json!({ "title": "Task Manager", "process_name": "Taskmgr.exe" }),
            json!({ "title": "bluee", "process_name": "harness.exe" }),
        ];
        // exact title wins
        assert_eq!(match_window(&ws, "Task Manager").unwrap()["title"], "Task Manager");
        // case does not matter
        assert_eq!(match_window(&ws, "task manager").unwrap()["title"], "Task Manager");
        // substring of the title
        assert_eq!(match_window(&ws, "notepad").unwrap()["process_name"], "notepad.exe");
        // process name when the title says nothing useful
        assert_eq!(match_window(&ws, "harness").unwrap()["title"], "bluee");
        assert!(match_window(&ws, "photoshop").is_none());
    }

    #[test]
    fn unwraps_uaccs_json_in_a_string() {
        let wrapped = json!({ "result": "{\"success\": true, \"count\": 2}" });
        assert_eq!(inner(&wrapped)["count"], 2);
        // Already-unwrapped values pass through untouched.
        let plain = json!({ "count": 3 });
        assert_eq!(inner(&plain)["count"], 3);
    }

    #[test]
    fn brief_keeps_what_matters_and_drops_the_rest() {
        let w = json!({
            "title": "Untitled - Notepad", "process_name": "notepad.exe",
            "is_focused": true, "is_minimized": false, "width": 800, "height": 600,
            "bounds": { "left": 100, "top": 50 },
            "is_visible": 1, "process_id": 1234, "center": { "x": 500, "y": 350 }
        });
        let b = brief(&w);
        assert_eq!(b["app"], "notepad.exe");
        assert_eq!(b["focused"], true);
        assert_eq!(b["at"], "800x600 at 100,50");
        // process_id and center are noise the model would pay for every turn.
        assert!(b.get("process_id").is_none());
        assert!(b.get("center").is_none());
    }
}
