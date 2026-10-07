"""Find tool parameters that a constrained-decoding provider will reject.

A parameter that is BOTH nullable and optional emits
`anyOf:[{string},{null}] + default:null` with no `required` entry - so the same
meaning has two encodings, omit it or send `null`. Providers that compile the
whole toolset into a grammar refuse that, and the WHOLE TURN 400s on a tool the
model never called:

    400 ... grammar rejected: tool "kuzu_graph__query_graph" parameter schema:
    parameter "relation": more than one JSON reading of the same emitted value

Observed on OpenRouter's free `qwen/qwen3.8-27b:free`, which routes to ModelRun.
The paid rung never hit it, which is why it looked like a rate-limit problem.

    python dev/schemacheck.py

Fix in our own servers is `str = ""` instead of `str | None = None`, converting
back with `x or None` at the call site when the callee distinguishes them.
"""
import json, subprocess, os, sys
servers = json.load(open('mcps/servers.json', encoding='utf-8'))
servers = servers.get('servers', servers)
bad = []
for name, cfg in servers.items():
    if not isinstance(cfg, dict) or not cfg.get('enabled'):
        continue
    cmd = [os.path.abspath(cfg['command'])] + cfg.get('args', [])
    env = dict(os.environ); env.update(cfg.get('env') or {})
    try:
        p = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, bufsize=1, env=env, encoding='utf-8', errors='replace')
    except Exception as e:
        print(f'{name}: could not start ({e})'); continue
    def send(o): p.stdin.write(json.dumps(o) + "\n"); p.stdin.flush()
    try:
        send({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
          "protocolVersion":"2024-11-05","capabilities":{},
          "clientInfo":{"name":"probe","version":"1"}}})
        p.stdout.readline()
        send({"jsonrpc":"2.0","method":"notifications/initialized","params":{}})
        send({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        n = 0
        for line in p.stdout:
            try: m = json.loads(line)
            except Exception: continue
            if m.get("id") != 2: continue
            for t in m["result"]["tools"]:
                sch = t.get("inputSchema") or {}
                req = set(sch.get("required") or [])
                for pname, pv in (sch.get("properties") or {}).items():
                    types = []
                    if isinstance(pv.get("anyOf"), list):
                        types = [b.get("type") for b in pv["anyOf"]]
                    elif isinstance(pv.get("type"), list):
                        types = pv["type"]
                    if "null" in types and pname not in req:
                        bad.append(f'{name}__{t["name"]}.{pname}')
                n += 1
            break
        print(f'{name}: {n} tools checked')
    finally:
        p.kill()
print()
print('AMBIGUOUS (nullable AND optional):', len(bad))
for b in bad: print('  ', b)
