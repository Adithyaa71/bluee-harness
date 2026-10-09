# Project plan

> Keep a repo's .bluee/plan.md current - phases, task checkboxes, next, waiting on Adithya - so the Playground shows progress without asking.

- category: skills
- tools: read_file, write_file, edit_file, list_folders
- triggers: project plan, build plan, roadmap, build phase, phases, progress, where were we, where did we leave, what's next, what next, plan.md
- created: 2026-10-09T12:00:00+00:00
- updated: 2026-10-09T12:00:00+00:00

---

Every granted project folder can have a plan at `.bluee/plan.md` (pass that
folder's id as `root`). The Playground reads it directly and shows phase,
progress, next steps and open questions - so Adithya sees where a project
stands without spending a turn. It is only useful if it is TRUE and CURRENT.

## When starting work in a repo
- `read_file` `.bluee/plan.md` first. That is where you left off.
- If there is none and the work is more than a one-off, offer to write one.

## The format (keep it exactly like this - it is parsed)
```
# <Project name>
> <one-line goal>

## Phase 1: <name>
- [x] done task
- [~] task in progress
- [ ] task not started

## Phase 2: <name>
- [ ] ...

## Next
- the next one to three concrete steps

## Waiting on you
- decisions or details only Adithya can give

## Decisions
- choices made, with the one-line reason

## Log
- YYYY-MM-DD: what happened (newest last)
```
Every `##` heading other than Next / Waiting on you / Decisions / Log / Notes
is a phase. Only `- [x]`, `- [~]`, `- [ ]` lines count as tasks.

## Keeping it current
- Tick tasks with `edit_file` as you finish them - change `[ ]`/`[~]` to `[x]`.
  Small edits, not a rewrite of the file.
- Mark what you are working on now as `[~]`.
- After a work session: update **Next**, add a **Log** line with today's date,
  and move anything you need from Adithya into **Waiting on you**. Remove items
  from Waiting on you once he answers.
- Never tick something that is not actually done and checked.
