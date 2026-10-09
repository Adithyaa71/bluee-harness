# Sub-agent templates

Each `.md` file here is a kind of sub-agent bluee (or you) can spawn by name:
"spawn a researcher to ...", or pick one in the sub-agents panel's **+**.

```text
---
name: researcher                 # how you call it (defaults to the file name)
description: one line            # shown in its window and to bluee
servers: [snarevec]              # MCP servers it may use; [] = memory + native tools only
skills: [snarevec-crawl-and-search]   # attached on its first turn, tools loaded
model: z-ai/glm-5.3-flash        # optional; your provider chain stays behind it as fallback
browser: chrome                  # optional; chrome / brave / edge - the browser it owns
folder: playground/agents/research   # optional; default playground/agents/<name>
sleep: 45                        # minutes idle before it sleeps, or never
end: 120                         # minutes idle before it ends, or never (3h works too)
max_turns: 30                    # optional cap: refuses new work past it
max_cost: 0.50                   # optional cap in US dollars (from OpenRouter's own usage)
---
The instructions: who it is and how it should work. Plain prose.
```

Only `name` matters; everything else is optional. Edit these by hand - they are
re-read on every spawn, so there is nothing to restart.
