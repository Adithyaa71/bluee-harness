"""Example post_tool hook: sanity-check an HTML artifact after it is written.

Whatever this prints is attached to the tool result as `hook_output`, so the
model sees it and can fix the artifact in the same turn. Printing nothing means
"all good" and adds nothing to the prompt.

The same shape works for real compiler feedback: match a tool that writes code
and run `cargo check --message-format short` (or `tsc --noEmit`, `ruff`, ...)
instead of these string checks.
"""
import json
import sys

call = json.load(sys.stdin)
args = call.get("args", {})
if args.get("kind", "html") != "html":
    sys.exit(0)

html = str(args.get("content", ""))
problems = []
lower = html.lower()
if "<html" not in lower and "<!doctype" not in lower:
    problems.append("no <html> or <!doctype> - the page is not a complete document")
if lower.count("<script") != lower.count("</script>"):
    problems.append("unbalanced <script> tags")
if "http://" in lower:
    problems.append("plain http:// URL - it will be blocked as mixed content; use https://")

if problems:
    print("artifact check found problems:\n- " + "\n- ".join(problems))
sys.exit(0)
