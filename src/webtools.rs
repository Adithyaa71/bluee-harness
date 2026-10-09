//! A sub-agent's own browser, when SnareVec cannot give it one.
//!
//! The preferred path for "this agent uses Brave" is SnareVec: the extension
//! in Adithya's real Brave, signed in as him, with commands routed to that
//! browser only. That needs the extension installed and polling there. When
//! it is not, the agent still needs a browser of its own that no other agent
//! touches - so bluee starts that browser itself (§28's native CDP), with its
//! own profile `data/browser-<kind>`, and these four tools drive it.
//!
//! Selector-or-text, not coordinates: the chat model reads text, and "click
//! the button labelled Add to Cart" is something it can say reliably, where
//! "click at 812,440" is a guess.
//!
//! Only offered to a sub-agent that owns a browser, and always pinned to that
//! browser - the model cannot name another one.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use crate::browser::Browser;
use crate::llm::ToolDef;

static POOL: OnceLock<Mutex<HashMap<String, Arc<Browser>>>> = OnceLock::new();

/// One live browser per kind, shared by every call for that kind.
fn browser(data_dir: &Path, kind: &str) -> Result<Arc<Browser>> {
    let pool = POOL.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = pool.lock().unwrap();
    if let Some(b) = map.get(kind) {
        return Ok(b.clone());
    }
    let b = Arc::new(Browser::for_kind(data_dir, kind)?);
    map.insert(kind.to_string(), b.clone());
    Ok(b)
}

/// Which of the three are installed - for the `@` picker and the spawn dialog.
pub fn installed() -> Vec<(String, PathBuf)> {
    ["chrome", "edge", "brave"]
        .into_iter()
        .filter_map(|k| crate::browser::find_kind(k).map(|p| (k.to_string(), p)))
        .collect()
}

pub fn defs(kind: &str) -> Vec<ToolDef> {
    let own = format!(
        "bluee's own {kind} - a separate, signed-out profile only you use. Prefer the snarevec \
         browser_* tools with browser: \"{kind}\" when Adithya's real {kind} is connected there \
         (it has his logins); use these when it is not."
    );
    vec![
        ToolDef {
            name: "web_open".into(),
            description: format!("Open a URL (or search for words) in {own}"),
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "A URL, a bare host, or words to search for." } },
                "required": ["url"]
            }),
        },
        ToolDef {
            name: "web_read".into(),
            description: "Read the current page in your own browser: its text, plus the labels of \
                the links and buttons you could click."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": { "max_chars": { "type": "integer", "description": "Text to return (default 8000)." } }
            }),
        },
        ToolDef {
            name: "web_click".into(),
            description: "Click something on the page in your own browser, by CSS selector or by its \
                visible text (a link, a button)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "Visible label, e.g. `Add to Cart`." },
                    "selector": { "type": "string", "description": "CSS selector, when the text is ambiguous." }
                }
            }),
        },
        ToolDef {
            name: "web_type".into(),
            description: "Type into a field in your own browser, found by CSS selector or by its \
                placeholder / label / name. Optionally submit (press Enter)."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" },
                    "field": { "type": "string", "description": "Placeholder, label or name of the field, e.g. `Search`." },
                    "selector": { "type": "string" },
                    "submit": { "type": "boolean" }
                },
                "required": ["text"]
            }),
        },
    ]
}

pub fn handles(name: &str) -> bool {
    matches!(name, "web_open" | "web_read" | "web_click" | "web_type")
}

/// A JS string literal for `s`.
fn lit(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const READ_JS: &str = r#"(() => {
  const label = e => (e.innerText || e.value || e.getAttribute('aria-label') || e.title || '').trim().replace(/\s+/g, ' ');
  const seen = new Set(), clickable = [];
  for (const e of document.querySelectorAll('a[href],button,input[type=submit],input[type=button],[role=button]')) {
    const r = e.getBoundingClientRect();
    if (!r.width || !r.height) continue;
    const l = label(e).slice(0, 60);
    if (l && !seen.has(l)) { seen.add(l); clickable.push(l); }
    if (clickable.length >= 60) break;
  }
  return { url: location.href, title: document.title,
           text: document.body ? document.body.innerText : '', clickable };
})()"#;

pub async fn call(data_dir: &Path, kind: &str, tool: &str, args: &Value) -> Result<Value> {
    let b = browser(data_dir, kind)?;
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let settle = || tokio::time::sleep(std::time::Duration::from_millis(900));
    match tool {
        "web_open" => {
            let url = s("url");
            if url.is_empty() {
                return Err(anyhow!("url is empty"));
            }
            let page = b.navigate(&url).await?;
            Ok(json!({ "browser": kind, "page": page, "next": "web_read to see what is there" }))
        }
        "web_read" => {
            let max = args.get("max_chars").and_then(|v| v.as_u64()).unwrap_or(8000).min(40000) as usize;
            let mut v = b.eval(READ_JS).await?;
            if let Some(t) = v.get("text").and_then(|t| t.as_str()) {
                let clipped: String = t.chars().take(max).collect();
                let more = t.chars().count() > max;
                v["text"] = json!(clipped);
                v["truncated"] = json!(more);
            }
            v["browser"] = json!(kind);
            Ok(v)
        }
        "web_click" => {
            let (sel, text) = (s("selector"), s("text"));
            if sel.is_empty() && text.is_empty() {
                return Err(anyhow!("give text or selector"));
            }
            let js = format!(
                r#"(() => {{
  const sel = {sel}, txt = {txt}.toLowerCase();
  const label = e => (e.innerText || e.value || e.getAttribute('aria-label') || e.title || '').trim().replace(/\s+/g,' ').toLowerCase();
  let el = sel ? document.querySelector(sel) : null;
  if (!el && txt) {{
    const c = [...document.querySelectorAll('a,button,input[type=submit],input[type=button],[role=button],[role=link],[role=tab],[role=menuitem],summary,label,[onclick]')]
      .filter(e => {{ const r = e.getBoundingClientRect(); return r.width && r.height; }});
    el = c.find(e => label(e) === txt) || c.find(e => label(e).includes(txt));
  }}
  if (!el) return {{ ok: false, error: 'nothing on the page matches ' + (sel || txt) }};
  el.scrollIntoView({{ block: 'center' }});
  el.click();
  return {{ ok: true, clicked: label(el).slice(0, 80) || el.tagName.toLowerCase() }};
}})()"#,
                sel = lit(&sel),
                txt = lit(&text)
            );
            let r = b.eval(&js).await?;
            if r.get("ok") != Some(&json!(true)) {
                return Err(anyhow!("{}", r.get("error").and_then(|e| e.as_str()).unwrap_or("click failed")));
            }
            settle().await;
            let page = b.eval("({url: location.href, title: document.title})").await.unwrap_or(Value::Null);
            Ok(json!({ "browser": kind, "clicked": r["clicked"], "now_on": page }))
        }
        "web_type" => {
            let (sel, field, text) = (s("selector"), s("field"), s("text"));
            let submit = args.get("submit").and_then(|v| v.as_bool()).unwrap_or(false);
            let js = format!(
                r#"(() => {{
  const sel = {sel}, want = {field}.toLowerCase();
  const fields = [...document.querySelectorAll('input:not([type=hidden]),textarea,[contenteditable=true]')]
    .filter(e => {{ const r = e.getBoundingClientRect(); return r.width && r.height; }});
  const name = e => [e.placeholder, e.getAttribute('aria-label'), e.name, e.id,
    e.labels && e.labels[0] && e.labels[0].innerText].filter(Boolean).join(' ').toLowerCase();
  let el = sel ? document.querySelector(sel) : null;
  if (!el && want) el = fields.find(e => name(e).includes(want));
  if (!el && !sel && !want) el = fields.find(e => e.type === 'search' || /search/.test(name(e))) || fields[0];
  if (!el) return {{ ok: false, error: 'no field matches ' + (sel || want || '(any)') }};
  el.focus();
  if (el.isContentEditable) {{ el.innerText = {text}; }}
  else {{
    const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, {text});
  }}
  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  if ({submit}) {{
    const kd = new KeyboardEvent('keydown', {{ key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }});
    el.dispatchEvent(kd);
    if (el.form) {{ el.form.requestSubmit ? el.form.requestSubmit() : el.form.submit(); }}
  }}
  return {{ ok: true, field: name(el).slice(0, 60) || el.tagName.toLowerCase() }};
}})()"#,
                sel = lit(&sel),
                field = lit(&field),
                text = lit(&text),
                submit = submit
            );
            let r = b.eval(&js).await?;
            if r.get("ok") != Some(&json!(true)) {
                return Err(anyhow!("{}", r.get("error").and_then(|e| e.as_str()).unwrap_or("typing failed")));
            }
            if submit {
                settle().await;
            }
            let page = b.eval("({url: location.href, title: document.title})").await.unwrap_or(Value::Null);
            Ok(json!({ "browser": kind, "typed_into": r["field"], "submitted": submit, "now_on": page }))
        }
        other => Err(anyhow!("not a web tool: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_tools_naming_their_browser() {
        let d = defs("brave");
        let names: Vec<&str> = d.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["web_open", "web_read", "web_click", "web_type"]);
        assert!(d[0].description.contains("bluee's own brave"));
        assert!(names.iter().all(|n| handles(n)));
    }

    #[test]
    fn literals_cannot_break_out_of_the_script() {
        assert_eq!(lit(r#"a"); alert(1); ("#), r#""a\"); alert(1); (""#);
    }
}
