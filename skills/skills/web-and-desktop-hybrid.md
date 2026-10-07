# Web and desktop hybrid

> Which tool to reach for: SnareVec to read and to drive web pages, bluee's GUI tools and UACC for apps and hard UIs.

- category: skills
- tools: snarevec__crawl_site, snarevec__add_urls, snarevec__search_collection, snarevec__browser_navigate, snarevec__browser_query, snarevec__browser_click, snarevec__browser_type, snarevec__browser_status, uacc__get_screen_info, uacc__launch_app, uacc__smart_click, uacc__screenshot
- triggers: browse, browser, website, web page, open app, open the app, desktop, my computer, my laptop, use my pc, gui
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

Pick the cheapest tool that can do the job, and only climb when it fails.

## 1. Just need to READ something on the web → SnareVec crawl/fetch
No browser needed, nothing moves on screen.
- One or a few pages: `snarevec__add_urls` into a collection, then `snarevec__search_collection`.
- A whole site or docs section: `snarevec__crawl_site` (then `snarevec__embed_collection`),
  then `snarevec__search_collection`.
- Use `render: "browser"` only for JavaScript-heavy pages.

## 2. Need to DO something on a website → SnareVec browser (the user's real Chrome)
Drives a real, logged-in tab by DOM element - far more reliable than pixels.
1. `snarevec__browser_status` first. If it says no extension is polling, tell the user to
   open Chrome with the SnareVec extension (workbench) - do not fall back silently.
2. `snarevec__browser_new_tab` or `snarevec__browser_navigate` to the URL.
3. SEE the page with `snarevec__browser_query` (CSS selector), not a screenshot.
4. Act with `snarevec__browser_click` / `snarevec__browser_type` (`press_enter: true` to submit).
5. Confirm with `snarevec__browser_get_page_info` after anything that may redirect.

## 3. Need a desktop APP, or the web UI defeats the DOM → bluee GUI tools, then UACC
Climb to this when: it is not a web page (Notepad, File Explorer, Settings, WhatsApp
desktop), or `browser_click` keeps failing (canvas, drag-and-drop, custom widgets,
anything with no stable selector).
- Prefer bluee's own verified tools first: `open_app_window`, `focus_app`, `read_screen`,
  `click_label`, `type_into_app`, `press_keys`, `arrange_app`, `list_apps`.
- Drop to raw UACC only when those cannot see the control: `uacc__get_screen_info`
  to read the screen, `uacc__smart_click` / `uacc__click` / `uacc__type_text`, and
  `uacc__screenshot` or `uacc__vlm_analyze` for purely visual things.
- `uacc__launch_app` opens an app by name when `open_app_window` cannot find it.

## Every tool, with its parameters
`mcps/TOOL_REFERENCE.md` lists all 31 SnareVec and 70 UACC tools with their exact
parameters - read it with `read_source` before guessing at an argument name.

## Rules
- Say which route you are taking in one short line before a multi-step task.
- Never claim a step worked without reading the result back (page info, read_screen).
- If the user moves the mouse, UACC aborts (MouseSentinel). Stop and ask before retrying twice.
- Never enter passwords, OTPs or payment details. Ask the user to do those steps.
