# USER.md

Facts about Adithya. Kept current as things are learned; only verified facts
belong here, not guesses.

## Who

Adithya (adityaummadi@gmail.com). Builds things — this harness, and SnareVec,
which he wrote himself.

## Machine

- Laptop, called **Grey**: i5-11300H, 32 GB RAM, Intel Iris Xe, **no discrete
  GPU**. Windows 11 Pro.
- No GPU is a real constraint: local vision models and local LLMs are too slow
  to be useful here. Reasoning goes to a cloud API.
- Has a **Raspberry Pi 5**, reached over SSH from the terminal. It is not a
  service this harness depends on.

## Software environment

- System Python is **3.14**. The project venv is **3.12** (`.venv`) and exists
  for the Python MCP tool servers, not for the harness.
- UACC runs from its **own** venv (`mcps/UACC/.venv`) because it needs mcp v1
  while the graph server needs mcp v2.
- Rust 1.97.1, MSVC toolchain, VS 2022 BuildTools.
- Project lives at `D:\Conceptual Project ~ clg`. The path has spaces and a
  tilde and has already broken one native build — builds go to `D:/tgt/harness`.

## Working style

- **Wants plain, straightforward English.** Explain the thing, not the
  process. He has said this explicitly.
- Makes decisions fast and revises them when new information arrives. If you
  think a decision is wrong, say so once with reasoning, then follow it.
- **Does not write Rust.** The harness core is Rust anyway — a deliberate
  tradeoff he accepted. This means: when you touch Rust, explain what changed
  in ordinary language, and do not assume he will read the diff.
- Builds his own guardrails and has said not to over-engineer safety layers
  that would duplicate his. Focus on making your actions *visible* rather than
  inventing policy.

## This project

A personal assistant harness. Rust core, memory in three layers (append-only
event log as the source of truth, vector search and a graph derived from it),
tools over MCP, a dashboard, and voice much later if at all.

The stated end goal beyond this build is a Linux kernel-level AI-OS harness.
This version is the thing he actually lives with in the meantime.

## Provider

Currently `https://aicredits.in/v1` (OpenAI-compatible, proxies OpenRouter),
model `qwen/qwen3.8-27b`. **Moving to OpenRouter directly** once the build
settles. The account has an in-flight credit budget that can return
`402 in_flight_budget_exhausted` under load — that is a billing limit, not a
bug in the harness.
