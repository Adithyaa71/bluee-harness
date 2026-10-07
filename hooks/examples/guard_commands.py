"""Example pre_tool hook: refuse shell commands that match a pattern list.

The harness sends the call as JSON on stdin:
    {"event": "pre_tool", "server": "harness", "tool": "run_command",
     "args": {...}, "session": "..."}

Exit 0 to allow. Exit anything else to block; whatever you print is handed to
the model as the reason, so say what to do instead.

This is a starting point, not a policy. Replace the list with your own.
"""
import json
import re
import sys

BLOCK = [
    (r"\bgit\s+push\b.*--force", "force-push rewrites shared history; push normally or ask Adithya"),
    (r"\bRemove-Item\b.*-Recurse", "recursive delete; name the exact files instead"),
    (r"\brm\s+-[a-z]*r", "recursive delete; name the exact files instead"),
    (r"\bshutdown\b|\bRestart-Computer\b", "powering the machine off or on is Adithya's call"),
]

call = json.load(sys.stdin)
command = str(call.get("args", {}).get("command", ""))

for pattern, why in BLOCK:
    if re.search(pattern, command, re.IGNORECASE):
        print(f"refused: {why}")
        sys.exit(2)

sys.exit(0)
