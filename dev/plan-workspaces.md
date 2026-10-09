# Workspaces - build plan

The interface bluee works in, not example artifacts. Each stage is built, then
checked before the next one starts.

| # | Stage | Check | Status |
|---|---|---|---|
| A | **Write in granted folders** - `write_file` (create / overwrite, makes parent folders), `edit_file` (exact replace, must match once unless `replace_all`). Only inside granted folders, `.git/` refused, every write a logged tool call (hooks apply). A sub-agent's relative paths land in its own sub-folder. | unit tests + `dev/livewrite.mjs` (live) | **done** |
| B | **Live previews inside repos** - any `.html` in a granted folder renders live (sandboxed by the server, like artifacts), with source toggle and pop-out. `create_artifact` takes `root` and saves into `<repo>/.bluee/artifacts/`; the Playground lists the selected repo's artifacts. | `dev/uirepoview.mjs` 11/11 | **done** |
| D | **Project progress** - `<repo>/.bluee/plan.md`: phases with checkbox tasks, plus *Next* and *Waiting on you*. bluee keeps it current (a skill tells it how); bluee's UI parses it - viewing costs no tokens. | parser unit tests + real-Chrome check | planned |
| E | **Workspace board** - opening a repo shows tiles: progress, artifacts, file previews. Drag, resize, pop out; layout saved in `<repo>/.bluee/board.json`. | real-Chrome check | planned |

Decisions (Adithya): writes are free inside granted folders; a repo's artifacts
and plan live in `<repo>/.bluee/`; API-keyed dashboards (stage C) not now.
