# UACC desktop control

> Open and operate desktop apps like a person would - bluee's verified GUI tools first, raw UACC when they cannot see the control.

- category: skills
- tools: uacc__launch_app, uacc__list_windows, uacc__focus_window, uacc__get_screen_info, uacc__get_screen_info_enhanced, uacc__smart_click, uacc__smart_type, uacc__click, uacc__type_text, uacc__hotkey, uacc__scroll, uacc__screenshot, uacc__vlm_analyze, uacc__wait_for_element, uacc__acknowledge_user_override
- triggers: open notepad, open the app, desktop app, click on, type into, my screen, window, file explorer
- created: 2026-10-07T05:30:00+00:00
- updated: 2026-10-07T05:30:00+00:00

---

## Order of tools
1. bluee's own GUI tools - they verify what happened:
   `list_apps` → `open_app_window` (waits for a real window) → `focus_app` →
   `read_screen` (labelled controls) → `click_label` (by visible label, pass `app`) →
   `type_into_app` → `press_keys`. `arrange_app` to snap a window into place.
2. Raw UACC only when step 1 cannot find the control:
   - read: `uacc__get_screen_info` (accessibility text map, cheap);
     `uacc__get_screen_info_enhanced` with `include_ocr: true` when the app exposes no labels
   - act: `uacc__smart_click` with `target` = the control's description, `uacc__smart_type`
     with `text` (and `target_field` to focus a field first), or `uacc__click` /
     `uacc__type_text` at coordinates from the screen read
   - keys: `uacc__hotkey` with `keys` as a list, e.g. `["ctrl", "s"]`
3. Pixels and vision last: `uacc__screenshot`, `uacc__vlm_analyze` - slow and costly.

## Rules
- `open_app_window` reports `already_open`; a title starting with `*` has unsaved work -
  never type into it without asking.
- Read the screen again after every action that should change it; do not assume.
- If UACC reports "User override: mouse moved away", the user touched the mouse. Use
  `uacc__acknowledge_user_override` once and retry once; if it happens again, stop and ask.
- Never type passwords, OTPs or card numbers.
