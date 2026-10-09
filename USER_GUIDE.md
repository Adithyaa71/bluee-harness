# bluee — user guide

Everything you can actually do, in plain English.

`CLAUDE.md` is the *build* spec — why things are the way they are. This is the
*use* guide. If something here disagrees with what the app does, the app is
right and this is stale; tell me and I'll fix it.

---

## Starting it

*Paths below are from the machine this was written on. If you cloned it,
substitute your own checkout directory - see README.md for setup.*


**Double-click `bluee` on your desktop.** That's it.

If the shortcut ever goes missing, `bluee-app.vbs` in the project folder does
the same thing, and you can make a new shortcut to it.

### From a terminal

`bluee.cmd` is in the project folder. It works from anywhere and passes
everything through:

```bat
"D:\Conceptual Project ~ clg\bluee.cmd"
```

Or `cd` in first and just type `bluee`:

```bat
cd /d "D:\Conceptual Project ~ clg"
bluee                    :: desktop app (same as the shortcut)
bluee dash               :: in a browser instead, http://127.0.0.1:7777
bluee chat               :: plain terminal chat, no UI
bluee reduce             :: rebuild memory from the logs
bluee search "daemon"    :: search memory
bluee log                :: print the last session's raw trace
bluee tools              :: list every connected tool
bluee models             :: list model ids your provider offers
```

PowerShell is the same, but call it with `&` because of the spaces in the path:

```powershell
cd "D:\Conceptual Project ~ clg"
.\bluee.cmd
.\bluee.cmd chat
```

### After changing the code

The launcher runs a compiled binary, so rebuild first:

```bat
cargo build --release
```

`cargo run -- app` still works too — it just rebuilds every time, which is
slower to start.

**Why a launcher rather than the raw `.exe`:** bluee looks for `.venv`,
`mcps/`, `persona/`, `skills/` and `data/` relative to the project folder. The
launcher `cd`s there first. Running the `.exe` directly from somewhere else
will start but find nothing.

Nothing is exposed to the internet. It listens on `127.0.0.1` only, and the
desktop app picks a random port each launch.

---

## The window

**Top left** — the session name. Click it to rename. Until you name one it shows
the first line of what you said, so the list stays readable either way. Renaming
doesn't erase the old name; it appends a new one to the log, same as everything
else here.

**Left rail** — pages:

- 💬 **Chat** — where you talk to it
- ◉ **Memory** — search everything it remembers, by meaning
- ❋ **Graph** — how things it knows connect to each other
- ▣ **Playground** — things it has *built* for you
- ☰ **Sessions** — every past conversation
- ⚙ **Settings** — persona and providers

Below the divider: **>_** opens the terminal, and the **◫** button (top right,
next to *new chat*) shows or hides the right panel.

**Both can float.** The **↗** button on the terminal bar or the right panel pops
it out into a proper window — drag it by its header, resize from the edges,
maximise, Escape to close. The terminal keeps running the whole time; popping it
out does not restart your shell.

**Right panel** — two tabs:

- **LOG** — the raw event trace of the current session
- **TASKS** — every tool that ran this session, how long it took, and why.
  Click any row to see the full arguments and result.

**Context meter** — top right, next to *new chat*. Shows roughly how full the
prompt is. Amber at 70%, red at 80%. Click it to run `/compact`.

It measures against whatever context length is set on the **Providers** page.
For `qwen/qwen3.8-27b` that's **1,000,000 tokens** — the model's real figure,
read off the provider — so the meter will sit very low for a long time. It used
to assume 32,000 and shout at you at around 23k, which was wrong.

Low percentage doesn't mean free, though. A big prompt costs what it costs on
every turn. `/compact` is still worth running on a genuinely long session.

---

## Talking to it

Type and press Enter. Shift+Enter for a new line.

It can see your screen, control your mouse and keyboard, drive your browser,
search your files, and remember everything. You don't invoke tools; you just
ask, and it picks.

**It can also drive your other applications.** Ask it to open something, put two
windows side by side, or click a button by name:

```
open notepad and put it on the left half of the screen
what's open right now?
in File Explorer, switch to Large Icons
```

It works through the accessibility tree — the same labels a screen reader uses —
not by guessing pixel positions, so it clicks *"Details"* because that is what
the control is called. If it can't find a label it tells you which ones it can
see, instead of clicking somewhere and hoping.

Two things worth knowing. If a window is **already open**, it says so rather than
pretending it opened one — and warns you if the title starts with `*`, which
usually means unsaved work it should not type into. And **moving your mouse
cancels whatever it's doing**: that's UACC's safety catch, and you always win.
It will quietly clear the block once and retry; move the mouse again and it
stops for good.

```
what's on my screen right now?
open github in my browser and find my starred repos
what did we decide about the graph database?
```

**📎 Attach** takes text files — code, logs, markdown, CSV, JSON. Images need
vision, which isn't built yet.

**🎙 Mic** works now. Press it once to start listening, press again to stop —
it's push-to-talk, not hold-to-talk, so you can think mid-sentence. What it
heard goes **into the composer, not straight out as a message**: Whisper
mishears sometimes, and you should see it before it's sent.

It's greyed out until you switch voice on at **Settings → Voice**. Everything
there runs on your own machine — there's no key to set because nothing is being
sent anywhere. `voice/README.md` has the setup, including how to use a GPU.

---

## Slash commands

Type `/` and a menu appears. Arrow keys or Tab to pick, Enter to run.

These are handled by bluee itself and never sent to the AI, so they're instant
and cost nothing.

### `/compact` — **the one that matters**

**Use this when the context meter goes amber or red.**

As a conversation gets long, the model starts losing the thread — forgetting
the goal, reasoning worse. `/compact` fixes that: it takes everything said so
far, files it into searchable memory, and clears it out of the prompt.

```
/compact          keeps the last 6 messages visible
/compact 2        keeps the last 2
/compact 0        keeps none
```

**Nothing is lost.** Not summarised, not trimmed, not paraphrased. Every
character stays in the log on disk. What changes is only what's loaded into the
model's working memory. Afterwards you can still ask about anything from
earlier and it will find it.

Try it: tell it a fact, run `/compact 0`, then ask for the fact back. It'll
search its memory and give it to you exactly.

### The rest

| Command | What it does |
|---|---|
| `/context` | how full the prompt is right now |
| `/sessions` | list your past conversations |
| `/skills` | list saved procedures |
| `/mcp` | which tool servers are connected |
| `/reduce` | rebuild general memory from the logs |
| `/help` | the list |

---

## Memory — how it actually works

Three layers, but you only deal with one of them.

**You never manage memory.** Just talk. Everything is written down
automatically, and bluee searches it when it needs to.

- **The log** — every message, tool call and result, written to disk as it
  happens. Nothing is ever overwritten. This is the source of truth.
- **Memory search** — the log turned into something searchable by meaning, so
  "that daemon problem" finds the right conversation even without matching words.
- **The graph** — how things connect. People, machines, projects, tools,
  preferences.

**bluee also knows its own source code.** The repo is indexed alongside your
conversations - 119 files, every function and struct it declares. So you can ask
"where do you handle the provider timeout?" and it will find the file, read it,
and tell you. It can read any file in the project except secrets (`.env`),
`data/`, and build output.

**For a big codebase, there's a second layer: Neovim's language servers.**
`codemap` tells you *where something lives*; an LSP tells you *what calls it*.
Grep finds the word `run` in forty files — a language server finds the seven
that call **this** `run`. Ask "what breaks if I change this function" and bluee
uses `find_references`.

Neovim and rust-analyzer are both installed and working. Real numbers from
your own repo, asking what calls `roots::pretty`:

```
first call   28s   (rust-analyzer indexes the workspace)
after that  ~0s   (Neovim stays alive and keeps the index)
answer       6 call sites, with the source line of each
```

The resolution is the point. Asking what calls `system::run` gives 4 real
callers and does **not** match `reduce::run` — grep would have returned both.

Other languages need their own server; `lsp_status` lists what works and how to
add the rest, and it checks by *running* each one rather than trusting PATH.

**It cannot edit itself.** Reading is on; writing is not. That's the
prerequisite for the self-editing you want later, not the thing itself - turning
on writes is its own decision with its own guardrails.

**It can delete artifacts and skills** when you ask - those are plain files.
**It cannot delete sessions.** Those are the source of truth, so deleting one
stays a deliberate two-click action you take on the Sessions page.

**Memory is current.** Every turn is indexed the moment it finishes, so
something you said two minutes ago - in this chat or another one - is already
searchable. `/reduce` is still worth running now and then: it rebuilds the
graph and re-indexes the code, and it is the only thing that picks up screen
context. It is no longer needed just to make recent conversations findable.

**Three kinds of memory, and bluee uses all of them:**

| | what it is | how bluee reaches it |
|---|---|---|
| **Short-term** | this conversation | it's in front of it; after `/compact`, `search_memory` with scope *session* |
| **Recent** | the last 7 days | `search_memory` scope *recent* |
| **Long-term** | every conversation + remembered facts | `search_memory` (default), `recall`, the graph |

Memory tools and the graph are **always loaded**: in every chat, workspace,
loop and sub-agent, whatever the Connectors tick-boxes say. (The graph's
switch shows *memory · always on*.)

**It remembers facts, not just conversations.** Tell it something that will
still matter ("Ravi works at Acme and is helping with jev, due 15 Oct") and it
quietly stores each piece with `remember`. Next week, in a different chat,
"when is Ravi's deadline?" just works: facts about anyone your message names
are attached to it automatically, before bluee even starts thinking.

**Facts change without losing the old ones.** "Ravi moved to Globex" closes
the Acme fact as *history* rather than deleting it, so "where did he work
before?" still has an answer. Jobs, roles, managers, where someone lives,
titles and similar one-at-a-time facts replace automatically.

**Every fact has a source.** It comes from a logged `remember` call, so it
points back at the exact conversation and message. Deleting that
conversation deletes the facts it produced. `/reduce` rebuilds all of them from
the logs.

To see them: ask "what do you know about Ravi?", or on the Graph page, search
the name.

**The Memory page shows all of it.** Five chips across the top:
**This session · Last 7 days · All conversations · Facts · Code**, each with
its count. Pick one and everything in it is listed, newest first ("load more"
at the bottom). Type in the box to search *inside* that tier. **Facts** lists
every remembered fact; tick *show history* to see the ones that stopped being
true, struck through with the date they ended.

The graph under it is **this conversation's** graph, and it fills in as you
talk: tools used, what followed what, facts remembered. You don't need
`/reduce` for it. The **Graph** page (left rail) is everything, all
conversations and the code index together.

**Search matches meaning *and* exact words.** "That daemon problem" finds the
right conversation by meaning; "402", `read_stream` or a file name find it by
the literal text. Both run on every search and the results are merged.

To ask memory something directly, use the **Memory** page. To see the shape of
what it knows, use the **Graph** page.

**The Graph page:** scroll to zoom, drag to move, double-click to fit it all
back on screen, hover a dot to name it. Labels appear as you zoom in.

**Find something:** type in the box at the top. Matches get a gold ring so you
can see where they are; press Enter to fly to the first one and pin it. If what
you searched for is in a hidden kind it tells you that instead of pretending
there's no match.

**Click a node to pin it.** Its connections light up and everything else fades,
and it stays that way while you scroll and zoom. Click empty space to let go.
The card shows how many links it has.

**The coloured chips under the graph are filters** — click one to show or hide
that kind. **`symbol` and `file` start hidden**, and that's deliberate: indexing
bluee's own source added 837 symbols and 119 files, which is 94% of the graph.
Drawn together they bury the part you actually want — you, your machines, your
projects, your tools. With them off you get about 57 things and can read every
label. Click `symbol` to pull the code back in when you want it.

The count always tells you what's hidden ("57 of 1013 · 956 hidden"), so you're
never looking at part of the graph thinking it's all of it.

---

## Sessions — your past conversations

The **Sessions** page lists every conversation, newest first, with the first
thing you said so you can recognise it.

- **Open & resume** — carries on where you left off. It gets the conversation
  back and continues in the same log.
- **Read only** — shows the transcript without resuming.
- **Rename** — give it a proper name. Same as clicking the title in the top bar.
- **Delete** — removes it. Click twice; it asks for confirmation because this
  deletes real data permanently.

**Deleting a session now takes its memory with it.** Its searchable chunks and
the graph edges it contributed go immediately, not at the next `/reduce`. What
stays: anything another session also knows about, plus the seeded facts and
bluee's index of its own source. An entity several conversations refer to isn't
one conversation's to delete.

So a throwaway session genuinely is throwaway — do some scratch work, delete it,
and it leaves nothing behind. Keep it and it's simply part of the main memory;
there's no separate step to "merge" it.

Nothing is ever lost automatically. Every conversation you've had is there.

---

## Playground — things it builds

Ask for something and it builds a real, working page:

```
build me a page that charts BTC candles
make a dashboard showing my disk usage
```

It writes actual HTML and it appears in the **Playground**, running.

**There's a prompt box at the bottom of the Playground page**, so you can build
and tweak things without going back to Chat. It's the *same conversation* — what
you type there is part of the same session, shows up in the chat transcript, and
goes in the same log. The strip above the box shows what it's doing as it does
it, and the list refreshes itself when a turn finishes.

**The useful part is that these get remembered by topic.** Later:

```
right, let's do trading
```

and it pulls up what it built last time plus what you discussed. Not a blank
page — where you left off.

**FILES / browser** — two buttons top right of the Playground. `files` shows
the folder tree, `browser` opens a real web browser inside the panel.

**The browser is bluee's own now.** It used to go through SnareVec, which meant
it only worked if that daemon was running and you had switched browser actions
on in its config — so in practice it never worked. bluee now starts Chrome (or
Edge) itself. There is nothing to enable and nothing to keep running.

Type a URL and press Enter, or type a phrase and it searches. Back, forward and
reload are on the bar; the page icon reads the page as text, the picture icon
goes back to the view. Click straight on the picture and the click lands on the
real page. Drag the right edge to make the panel wider.

One thing to know: **it is a separate browser profile, so it is signed out of
everything.** Sign in once and it stays signed in — those cookies live in
`data/browser`. It is not the browser you already have open.

**"Filter files…" is just a search box for the tree.** Type `chart` and only
rows matching `chart` stay visible — it hides the rest of the tree, nothing
more. With three files it does nothing useful; with a real project folder open
it is how you find something without scrolling.

**The FILES panel down the left is the folder itself.** Everything bluee has
built, as it sits on disk. Filter it, click a file to open it — HTML runs live
with a **Source** toggle, anything else shows as text. **open window** floats it
in its own window you can drag and resize. The **x** on a row deletes it; click
twice, because it removes the real file.

bluee can do this too — ask it to list or clean up files in the playground and
it will. That is bounded to the playground folder; nothing else on your machine
is reachable through it.

## Opening your own folders

The **FILES** panel isn't limited to the playground. Press **+**, paste a folder
path, and bluee can work in it — the same as giving a coding agent a directory.

```
D:\projects\my-thing
```

Then pick it from the dropdown at the top of the panel. **terminal here** opens
the terminal already `cd`'d into it.

**Each folder picks its own tools.** Open the **+** menu → **Connectors** and
tick which MCP servers this workspace uses. It's per folder, and it's the
cheapest single thing you can do about cost:

```
all four servers   108 tools   ~18,700 prompt tokens every turn
without uacc        38 tools    ~6,600 prompt tokens every turn
```

That's paid on *every* message before you've said anything. A trading workspace
doesn't need 70 GUI-automation tools. The menu shows the count and the estimate
as you tick, and it applies to the running conversation immediately — no
restart. **Use all** puts it back.

**You grant folders; bluee cannot.** There's deliberately no tool that lets it
add one — a boundary the thing inside can move isn't a boundary. It can list,
read and delete *inside* what you've granted, and nowhere else. **Revoke** takes
the access away; it never touches the folder itself.

Two things it refuses: a whole drive (`C:\`) and system folders. Not because you
can't be trusted, but because a delete tool pointed at `C:\` is a bad afternoon.

Only playground files render live in the preview. A folder you granted shows as
source — serving it over HTTP would quietly widen what you agreed to.

The preview has **line numbers** for code and **wrap** for prose, with a toggle
either way.

Artifacts are real files at `data/artifacts/<name>/index.html`. Open, edit or
delete them yourself; bluee doesn't own them.

**Limitation to know:** a page that needs an API key can't have one yet.
Putting a key in a generated HTML file would leave it in plaintext on disk.
Proper handling needs a proxy that holds the key server-side — not built.

---

## Skills — teaching it procedures

When it works something out that will come up again:

```
remember that as a skill
```

It writes the steps to `skills/` as Markdown you can read and edit. Later:

```
run the snarevec recovery skill
```

**A skill is a recipe, not a program.** `run_skill` doesn't execute anything —
it hands bluee the steps and bluee follows them with its normal tools. That
means every action still shows up in the log as a real tool call. Nothing
happens invisibly.

Four folders under `skills/`:

- **skills/** — procedures you meant to save
- **recorded/** — captured from something that actually happened
- **toolkit/** — bundles dropped in from elsewhere
- **proposed/** — candidates for review. Nothing here is offered to bluee until
  you move it into `skills/`. This is the safety catch for automatic skill
  discovery later.

**Skills load themselves.** You never have to say "run the skill". A skill is
attached to your message, with its tools already loaded, when the message
mentions:

- **a server it uses**: "do it with uacc", "use snare vec", "check neovim".
  Naming a server brings up **every** skill that uses it.
- **one of its tools**: "try crawl site on the docs".
- **one of its trigger words**: "add the headphones to my Flipkart cart" loads
  the shopping skill without anyone naming a tool.
- **its name**.

At most three per message (each is a page of instructions). The Log panel
shows which: `auto-recall: … skill(s): shop-add-to-cart`. bluee can also load
any skill itself with `list_skills` / `run_skill`, and when it loads a server's
tools with `find_tools`, the skills for that server are listed with them.

Trigger words are one optional line in the skill's header, hand-written:

```
- tools: snarevec__browser_click, uacc__smart_click
- triggers: amazon, flipkart, add to cart
```

Saving from the Settings page or with "remember that as a skill" keeps them.

**The skills that ship:**

| Skill | For |
|---|---|
| `web-and-desktop-hybrid` | Which tool to use: SnareVec to read and drive websites, bluee's GUI tools and UACC for apps and hard UIs |
| `shop-add-to-cart` | Amazon / Flipkart in your real Chrome, **stops at the cart, never pays** |
| `snarevec-crawl-and-search` | Read a site without a browser: crawl, embed, search |
| `uacc-desktop-control` | Open and operate desktop apps, verified step by step |
| `graph-memory` | Remembering facts, recalling, querying the graph |
| `code-navigation` | Neovim LSP + nyx: find references, safe edits |

---

## Demo checklist — before showing it to anyone

The setup decides whether a live demo works, not the model.

1. **Open Chrome with the SnareVec extension and its workbench.** This starts
   the daemon and connects the browser. Check: ask bluee "is the snarevec
   browser connected?" The answer must say connected, not "no extension is
   polling".
2. **Use the Chrome profile you are signed into Amazon/Flipkart with.** The
   extension is installed in your Default, Profile 6 and Profile 8.
3. **Empty the cart first** so the panel sees the item arrive.
4. **SnareVec idles out after 30 minutes**. Open the workbench again shortly
   before you present.
5. **Don't touch the mouse during a desktop step.** UACC treats any movement as
   you taking over, and stops.
6. **Keep the Tasks panel open** so every tool call is visible while it works.
7. **Start a new chat for the demo** so earlier test turns aren't in the way.

---

## Loops — things it does without being asked

A skill is something you invoke. A **loop** is something that runs on a
schedule. One loop is one Markdown file in `loops/`:

```markdown
---
name: daily-review
at: 22:00
servers: []
---

Read back the day and write down what is worth keeping.
```

Save the file and it is live within 30 seconds. No restart.

```
harness loops                   list them, and when each last ran
harness loop project-kickoff    run one now, ignoring its schedule
```

**Scheduling.** Use one of `every: 6h` / `at: 22:00` / `on: startup`.

**A loop with no schedule never runs on its own** — it waits for
`harness loop <name>`. That is deliberate: dropping a half-finished file into
`loops/` must not be able to start spending money while you sleep.

**`servers:` is about cost, not preference.** Tool descriptions are paid on
every turn before bluee says anything — all servers is about 22,300 tokens a
turn, `[]` is none. `[]` still leaves the built-in tools (memory search, source
reading, artifacts), which is all a review loop needs. Most loops want `[]` or
one server. `max_runs:` caps a loop per day; the default is 24. `min_gap:`
sets the shortest time between runs. Startup loops default to 4 hours, so
relaunching the app doesn't repeat (and pay for) a catch-up you just had.

**Whatever a loop notices becomes memory.** It runs in a real session, so its
output goes into the event log like anything else, and `reduce` makes it
searchable. A review that noticed something last week is findable this week.

Four are shipped: `catch-up` (on startup), `daily-review` (22:00), and two with
no schedule — `project-kickoff` and `deep-research`, which are checklists to run
at the start of something rather than background work.

`loops/README.md` has the full list of keys.

---

## Sub-agents — other bluees with their own window

A sub-agent is another assistant bluee (or you) starts for one job. Each has
its own conversation, its own tools, its own work folder, and **its own
window**, which opens the first time it is spawned.

**Starting one**
- Ask bluee: *"spawn a researcher to find the best 27-inch monitors under 20k"*
  or *"spawn two agents, one in Chrome and one in Edge, and compare prices"*.
  Given a task, it works in the background and bluee is told the result when
  it lands; you do not have to wait or ask again.
- Or press **+** in the sub-agents panel, or **+ new agent** on the **Agents**
  page (grid icon in the left rail). Pick a template, the tool servers it may
  use, a browser, and optionally a model.

**Talking to it.** Type in its window, exactly like the main chat. bluee sees
that conversation too, and work bluee gives it shows up in the window, marked
*from bluee*. When an agent needs a decision from you, a question card appears
in its window and a note appears in the main chat. Answer in the card.

**Closing the window never stops it.** Reopen it from the panel or the Agents
page. What stops an agent:
- **sleep** after 45 min with no work: unloaded from memory, history kept;
  your next message wakes it as if nothing happened;
- **end** after 2 h with no work: it stops and its window says so; the
  conversation stays in Sessions and can be resumed;
- the **×** in its window or on its card (click twice).

"No work" means nothing running, nothing queued and no question waiting on you.
An agent that is busy never sleeps. Change both times, and a spending cap, from
the timer button in its window (`$0.0012 · sleeps 45m · ends 2h`).

**Templates** live in `agents/` (researcher, shopper, coder, desktop). Each is
a Markdown file: settings at the top, instructions below. See `agents/README.md`.
Edit them by hand; they are re-read on every spawn.

**Browsers.** Give an agent its own browser (`chrome`, `brave` or `edge`) and
no other agent can use it, so three agents can browse at once without
touching each other's tabs.
- If the SnareVec extension is loaded in that browser, the agent drives **your
  real browser**, signed in as you. After updating SnareVec, restart its
  daemon and **reload the extension in each browser** (`chrome://extensions`,
  `brave://extensions`, `edge://extensions`). Then `browser_status` lists every
  connected browser.
- If not, the agent uses **bluee's own copy** of that browser (separate,
  signed-out profile in `data/browser-<kind>`), driven by clicking and typing on
  visible text.

**The Agents page** shows every agent on one screen: what it is doing at this
moment (the tool it is in, or *needs you*), what it has cost, and when it will
sleep or end. Double-click a card to open its window.

## Composer: `/` and `@`

In the main chat and in every agent window:
- **`/`** lists slash commands (at the start of the main chat only) and your
  **skills**;
- **`@`** lists **tool servers**, single **tools**, and **browsers**.

Keep typing to filter; arrows to move; **Tab** or **Enter** to pick. A pick
becomes a coloured chip above the box (blue = skill, green = server/tool,
orange = browser). Backspace on an empty box removes the last chip. Chips go
with that one message: the skill is attached, the tools are loaded, and bluee
is told you chose them. In an agent's window, `@server` also *gives* that
agent the server.

## Screen vision

Three-way toggle in the top bar. **Passive is the default.**

- **off** — no capture at all.
- **passive** — every ~45s bluee reads your screen as *text* (window titles,
  buttons, visible text) via the accessibility tree. No AI model, no cost,
  nothing leaves your machine. Identical screens are skipped, so it doesn't
  fill memory with noise.
- **active** — unlocks the vision model. Only in this mode does bluee get a
  tool for actually *looking* at the screen.

**The important bit:** in off and passive, the vision tool doesn't exist in
bluee's toolset. It's not told "please don't" — it genuinely cannot, because it
doesn't know the capability is there. So a screenshot can never be sent to a
model in a session where you didn't turn it on.

Passive screen reads go into the log, not into the conversation — otherwise
every message would drag a screen dump along with it. They become searchable
once you run `/reduce`.

**Active needs a vision model.** Already set for you:

```
LLM_VISION_MODEL=qwen/qwen3-vl-8b-instruct
```

I benchmarked this. The small 8B model was both the **cheapest and the most
accurate** — it read text off the screen exactly, in 2.9s. The 30B was slower
and slightly worse; your chat model (`qwen3.8-27b`) accepted the image but
returned nothing useful. Bigger isn't better here — it's reading, not thinking.

**What a screen look costs:** about **2,000 tokens each time**, whatever you
ask. That's why passive (free, text-only) is the default and active is a
deliberate choice. Leaving active on and letting bluee look repeatedly is how a
bill grows.

End to end it takes ~30s, and about 20s of that is just capturing the
screenshot — not the AI.

---

## The terminal

Click **>_**. It's a real shell, and **you and bluee share it** — you both see
the same session.

### Connections

Press **+** on the tab strip, or **connect…** in the bar. You get a list:
powershell, cmd, bash, and anything you have saved.

To add a machine, type a name and a command in the box at the bottom of that
list and press **add**:

```
name: pi      command: ssh pi@raspberrypi.local
name: box     command: ssh adithya@your-cloud-host
```

It is saved, so next time it is one click. Anything starting with `ssh` is
marked with a globe on its tab so you can see at a glance which terminals are
on another machine. That's how the Pi 5 connects, and any cloud box the same
way — a remote is just a command the terminal runs, so there is nothing else to
set up.

### Tabs

Each tab is its own shell with its own scrollback. Switching is instant and
tabs do not bleed into each other — what powershell printed stays in the
powershell tab.

Tabs belong to the **folder you are working in**. Open a different granted
folder and you get that folder's terminals back, not this one's.

**Sessions survive.** Close a tab, close the panel, close the window, come back
days later — the shell kept running and you get the scrollback. Closing a tab
puts the window away; it does not kill the shell. The bar says **reattached**
when it has picked one back up.

---

## Tools load on demand

bluee has ~150 tools. Sending every one of their instructions with every
message used to cost ~27,000 tokens a round before anything was said.

Now it carries its own tools and the graph, plus a **catalogue** of the rest by
name. When a job needs, say, the GUI tools, it calls `find_tools` to load them,
and they stay loaded for the rest of the conversation. You'll see this in
TASKS as a `find_tools` call.

Measured on the same question: **27,483 → 7,010 prompt tokens a round**, 157 →
32 tools in the prompt.

You don't need to do anything. If a model handles it badly, set
`HARNESS_TOOL_SEARCH=off` in `.env` to go back to sending everything.
`HARNESS_CORE_SERVERS` (default `kuzu_graph`) lists servers that are always
loaded.

The Connectors tick-boxes still matter: a server you untick is not even in the
catalogue.

---

## Hooks — your guards, run outside the model

A hook is a command **you** write that runs around every tool call. The model
can't see it, argue with it, or switch it off. This is the general version of
the lesson from `format C:` (the model once approved its own confirmation
flag).

Copy `hooks.example.json` to `hooks.json` at the repo root:

```json
{
  "pre_tool":  [{ "match": "harness__run_command", "command": "python hooks/examples/guard_commands.py" }],
  "post_tool": [{ "match": "harness__create_artifact", "command": "python hooks/examples/check_artifact.py" }]
}
```

- **`match`** is the tool's full name: `server__tool`, and bluee's own tools
  are `harness__<name>`. `*` is a wildcard, `|` means "or":
  `"uacc__*|harness__run_command"`.
- The tool call arrives on the hook's **stdin** as JSON: `server`, `tool`,
  `args`, `session` (plus `ok` and `result` after the call).
- **pre_tool — exit 0 lets it run, any other exit code blocks it.** What the
  hook prints is shown to bluee as the reason, so write it as an instruction
  ("refused: name the files instead of deleting recursively"). A hook that
  crashes or times out also blocks.
- **post_tool never blocks.** What it prints is attached to the result bluee
  sees. Point one at `cargo check` after a code write and compiler errors come
  straight back to it in the same turn.
- The file is read on every call. Edit it and the next tool call uses it; no
  restart.

Blocked calls show in TASKS as failed, with your hook's message.

---

## Settings

### Persona — who bluee is

Four files control its behaviour:

- **SOUL.md** — personality, tone, hard limits. Keep it short; longer dilutes it.
- **AGENTS.md** — how to work. Check memory first, cheapest tool first.
- **TOOLS.md** — which tool to reach for and what to watch out for.
- **USER.md** — facts about you and your machine.

Edit them here. **Changes apply on your next new chat**, not mid-conversation —
the personality is assembled once per session so it stays stable.

Every save keeps a `.bak` of the previous version.

### MCP — the tool servers

Every server bluee talks to, with whether it's **connected right now** and how
many tools it gave you. Edit the command, arguments, environment, or switch one
off. Add new ones here.

**Changes need a restart** — servers are launched once when bluee opens.

**Reconnect** starts them all again without restarting bluee — and without
losing the conversation you're in. Use it when a server shows *enabled, not
connected*, or after starting something bluee depends on (the SnareVec daemon,
for example). It takes about 25 seconds, and the tools come back in the chat you
already have open.

A server that didn't start now says **why**, right on its row. If nothing is
connected you'll get a banner at the top of the page saying so.

### Tools & Skills

Your saved procedures, grouped by category, with an editor on the right. Edit
the steps, save, copy, or delete. Changing a skill's category moves it.

**Drag `.md` files onto the list** to import them as a toolkit.

### Providers — the AI behind it

You can configure several, in order. The first is the default; if it fails, the
next one answers. That's the point — a daily driver shouldn't die because one
endpoint is having a bad day.

Each needs a base URL, an API key, and a model id. Then four settings:

| Setting | What it does |
|---|---|
| **reply cap** | The most it can write in one answer. |
| **context length** | How much the model can hold at once. Only used by the meter. |
| **temperature** / **top_p** | How loose the answers are. **Leave blank** and the model uses its own default — that's usually what you want. |
| **timeout (s)** | How long to wait before giving up and trying the next provider. |
| **retries** | How many times to try the same provider again after a hiccup. |
| **stream** | **Leave this on.** See below. |

**Stream is not a display preference — it's what stops your provider timing
out.** aicredits.in kills any non-streamed request at ~30 seconds and returns
`500 Internal Server Error`. Streamed, the same work runs for minutes and
finishes. The one thing to know: this provider still buffers, so you won't see
words appear one by one — you just stop losing long answers.

**Reply cap and context length are not the same thing, and mixing them up costs
you money.** The cap is the *answer* size. If you set it to the model's full
window, the provider reserves that whole budget in advance and refuses the
request outright when your balance is small — that's what the
`you requested up to 100000 tokens, but can only afford 8048` error was. The
context length is never sent anywhere; it just tells the meter what full means.

**Press `detect`** and bluee asks your provider directly what that model's
context length is and fills it in. If the provider doesn't publish one, it says
so rather than making something up.

**Keys are shown masked** (`••••••••…4e60`). Leave a masked key alone and it
stays as it is — you can edit everything else and save safely without retyping
your key.

To find valid model ids: `cargo run -- models`.

---

## When something goes wrong

**The Graph page is blank, or bluee can't answer things it normally can**
Check the top bar: it says how many **tools** and **servers** are live. If it
reads `0 tools · 0 servers`, the tool servers didn't start — bluee still has its
own built-in tools, which is why it may go hunting through files instead of just
asking the graph.

Go to **Settings → MCP**. Each server shows why it failed. Press **Reconnect**.

An empty Graph page tells you which kind of empty it is: *nothing in the store
yet* (run `/reduce`), *this conversation isn't folded in yet* (also `/reduce`),
or *the graph server isn't connected* (Reconnect). They need different things,
so it no longer gives the same advice for all three.

**"Search isn't working" / SnareVec errors**
The SnareVec daemon idles out and shuts down. That's normal, not a fault. Open
the SnareVec workbench to start it again. bluee will tell you when this is the
problem rather than claiming it's broken.

**`500 Internal Server Error` from the provider**
Your provider cuts any request off at about **30 seconds** and returns this. It
has nothing to do with your conversation — a tiny question with a long answer
triggers it just as reliably as a big one.

bluee now asks for answers as a **stream**, which keeps the connection alive and
gets past that wall (measured: the same work went from a guaranteed 500 at 30s
to finishing at 362s). It also **retries** a failed call twice before giving up.

If you see this anyway: check that **stream** is still ticked on the Providers
page. It should be on for every provider.

**`402 Payment Required`**
Your provider is out of credit, or too many requests are in flight at once.
Wait a moment, or top up. If you have a fallback provider configured, it should
have taken over.

**Replies stop coming**
Check the context meter. If it's red, run `/compact`.

**A tool failed**
Open the **TASKS** panel. It shows what ran, what it returned, and why bluee
called it. Failures are red and expandable.

**It doesn't remember something recent**
It should - every turn is indexed as it finishes. If it still can't find it,
ask with a word that was actually used ("the 402", a file name): exact words
are matched too. `/reduce` rebuilds everything from scratch if memory looks
wrong.

**Graph page says "no MCP server is connected" / every server failed**
Open **Settings → MCP**: each failed server now shows what it printed as it
died. If it says **`No Python at '...AppData\Roaming\uv\python...'`**: the
Python the tool servers run on was installed from *inside* the Claude desktop
app, which is a packaged app, so Windows quietly stored it in Claude's private
folder. bluee started from Claude could see it; bluee started from your
desktop shortcut could not. Fixed on 2026-09-25 by moving it to
`D:\tgt\python\cpython-3.12.14-windows-x86_64-none` and pointing both
`pyvenv.cfg` files there. If you ever recreate a venv, make sure its Python
lives outside `AppData` (the old configs are kept as `pyvenv.cfg.bak-appdata`).
Then press **Reconnect** on the MCP page; no restart needed.

**A turn stopped with "did not answer within 120s"**
A tool hung and was abandoned, so the turn could carry on. The usual culprit is
UACC's OCR (`include_ocr`) without `pytesseract` installed. Set
`HARNESS_TOOL_TIMEOUT` (seconds) in `.env` to change the limit.

**"hooks are misconfigured, so no tool may run"**
Your `hooks.json` doesn't parse. This is deliberate: a broken guard file stops
tools rather than silently switching every guard off. Fix the JSON or delete
the file.

---

## Where things live

```
persona/          SOUL.md, AGENTS.md, TOOLS.md, USER.md, graph-seed.json
skills/           saved procedures (skills/ recorded/ toolkit/ proposed/)
data/             NOT committed — your actual content
  events/         every conversation, one .jsonl per session
  artifacts/      things bluee built for you
  vectors.db      searchable memory
  graph/          the knowledge graph
  providers.json  provider chain (contains API keys)
.env              your API key
mcps/             the tool servers
```

**Safe to delete and rebuild:** `data/vectors.db`, `data/graph`. Run `/reduce`
and they come back.

**Not safe to delete:** `data/events/` — that's the source of truth. Everything
else is derived from it.

---

## What isn't built yet

Being straight with you so you don't go looking:

- **Wake word** — no "hey bluee" yet. Speech in and out both work; you just
  have to press the mic.
- **Image attachments** — needs vision support
- **Automatic skill discovery** — it saves skills when asked, but doesn't
  propose them itself yet
- **The UI itself** — components are placeholders. A proper pass with Framer
  is planned.
