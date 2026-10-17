# Visual canvas (OpenUI) + a calmer shell - build plan

Each stage is built, then checked in real Chrome before the next.

| # | Stage | Status |
|---|---|---|
| 1 | **OpenUI inside bluee** - vendor `@openuidev/browser-bundle` (offline, no CDN); a sandboxed canvas page that renders OpenUI Lang and sends button/form actions back; the component-library prompt generated once and measured (tokens per message when on). | **done** |
| 2 | **Visual switch + canvas in chat** - a toggle by the composer and `/visual`, per session; when on, bluee may answer with a canvas. A canvas shows as a preview card in the reply that opens a side canvas panel (like Claude Code's preview), with **pop out** (own window), **full screen** and **close**. | **done** |
| 3 | **Per session, not global** - canvases live only in that session's log. A new session shows no canvas button at all; reopening a session that made canvases shows a **Canvas (N)** button to browse them. Nothing stored anywhere else. | **done** |
| 4 | **Sub-agents too** - the same switch, cards, panel and pop-out in every agent window, per agent session. | **done** |
| 5 | **Calmer shell** - left rail slides away; top bar can collapse to plain; **Ctrl/Cmd + / - / 0** zooms the whole UI (remembered), in every window. | **done** |
| 6 | **Playground tidy-up** - proposal first (it is crowded: files, browser, cards, board); build after Adithya picks. | planned |

Later (needs Adithya's "Reverse Engineer Anything" repo): own component
libraries built from top UI themes, plus instructions for bluee on using them.
OpenUI's prebuilt bundle only ships its built-in chat library; custom
components mean building our own bundle from its source (pnpm), which is the
same work.
