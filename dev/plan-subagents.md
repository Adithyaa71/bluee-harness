# Sub-agents v2 - build plan

Each stage is built, then checked (cargo test + a real-browser check or a live
call) before the next one starts. Status is updated as stages land.

| # | Stage | Suggestions | Check | Status |
|---|---|---|---|---|
| 1 | **Backend core** - one worker for new/resumed/woken agents; live event stream per agent; sleep (45m) / end (2h) per agent, "work given" never idles; parent inbox; child tools `message_parent` + `ask_user`; `ask_agent` background mode that notifies bluee when done; fan-out | 2, 3, 4 | unit tests + `harness dash` API calls (no model) | **done** |
| 2 | **Agent window** - opens on first spawn; replay + live tool calls over a websocket; you talk to it, bluee sees it; answers to `ask_user`; clock for sleep/end; closing never kills | 1, 2 | real-Chrome check `dev/uisubwin.mjs` | **done** (20/20) |
| 3 | **Composer pickers** - `/` skills + commands, `@` servers / tools / browsers, filter by typing, arrows/Tab, highlighted chips; chips attach the skill / scope the tools for that message. Main chat and every agent window | 5, 6, 7 | real-Chrome check `dev/uipickers.mjs` | **done** (19/19) |
| 4 | **Templates, model, caps, folder** - `agents/*.md` (instructions, model, servers, skills, browser, sleep/end, max turns, max $); per-agent model; caps stop a runaway; own work folder | 11, 12, 13, 14 | unit tests + `dev/uitemplates.mjs` | **done** (16/16) |
| 5 | **Browsers** - SnareVec: extension reports its browser (chrome/brave/edge) + instance, daemon queues per browser, `browser_*` take `browser`; bluee pins an agent to one browser and refuses others; native CDP fallback launches Chrome/Edge/Brave per agent | 8, 9, 10 | SnareVec tests 69/69 + `dev/uibrowsers.mjs` 16/16 | **done** (extension reload needed) |
| 6 | **Agents grid** - every agent live: status, current tool, cost, stop/sleep/wake | 15 | real-Chrome check `dev/uigrid.mjs` | **done** (12/12) |

Lifecycle rules (agreed):
- busy = a turn running, a job queued, or waiting on `ask_user` -> never sleeps or ends, window open or not
- sleep after 45 min idle: agent unloaded, history kept; a message wakes it (Agent::resume)
- end after 2 h idle: worker gone, window closes; the session stays in Sessions (resumable)
- both per agent, changeable from its window and from templates
