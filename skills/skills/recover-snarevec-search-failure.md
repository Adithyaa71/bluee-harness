# Recover SnareVec search failure

> When a snarevec search tool fails, check daemon status and guide the user to reopen the workbench before retrying.

- category: skills
- tools: snarevec__snarevec_status
- created: 2026-08-29T14:10:54.890594300+00:00
- updated: 2026-08-29T14:10:54.890594300+00:00

---

1. A snarevec search call failed. Do not retry the search immediately.
2. Call `snarevec__snarevec_status`.
3. Read the status:
   - If it says **NOT RUNNING**: the daemon has idled out, it is not broken. Tell Adithya to reopen the SnareVec workbench to restart the daemon.
   - If it is running: the failure is something else — read the original error and report what it said.
4. After the workbench is reopened (or if it was already running), retry the original search call.
5. If the retry still fails with the daemon confirmed running, stop and report the error rather than looping.
