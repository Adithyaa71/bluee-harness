# bluee — user guide

Everything you can actually do, in plain English.

`CLAUDE.md` is the *build* spec — why things are the way they are. This is the
*use* guide. If something here disagrees with what the app does, the app is
right and this is stale; tell me and I'll fix it.

---

## Starting it

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
search your files, and remember everything — because it has 106 tools across
three servers. You don't invoke tools; you just ask, and it picks.

```
what's on my screen right now?
open github in my browser and find my starred repos
what did we decide about the graph database?
```

**📎 Attach** takes text files — code, logs, markdown, CSV, JSON. Images need
vision, which isn't built yet.

**🎙 Mic** is deliberately disabled. Voice isn't built (that's Phase 5). It's
greyed out rather than pretending to work.

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

**It cannot edit itself.** Reading is on; writing is not. That's the
prerequisite for the self-editing you want later, not the thing itself - turning
on writes is its own decision with its own guardrails.

**It can delete artifacts and skills** when you ask - those are plain files.
**It cannot delete sessions.** Those are the source of truth, so deleting one
stays a deliberate two-click action you take on the Sessions page.

**One thing you do need to know:** memory search only covers what's been
*indexed*, and indexing happens when you run `/reduce` (or `cargo run --
reduce`). Recent conversations aren't searchable until then. `/compact` indexes
the current session immediately, which is a second reason to use it.

To ask memory something directly, use the **Memory** page. To see the shape of
what it knows, use the **Graph** page.

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

---

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

Pick your shell from the dropdown: powershell, cmd, bash, or **custom** — type
anything, including:

```
ssh pi@raspberrypi.local
```

That's how the Pi 5 connects.

**Sessions survive.** Close the panel, close the window, come back later — the
shell kept running and you get the scrollback. Each shell keeps its own
session, so switching between powershell and cmd doesn't lose either.

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
Run `/reduce`. Memory search only covers indexed conversations.

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

- **Voice** — no wake word, no speech in or out (Phase 5)
- **Image attachments** — needs vision support
- **Automatic skill discovery** — it saves skills when asked, but doesn't
  propose them itself yet
- **The UI itself** — components are placeholders. A proper pass with Framer
  is planned.
