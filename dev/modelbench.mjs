// Which model should drive bluee? Measured, not read off a leaderboard.
//
// Each candidate gets the same five real tasks, run through the actual
// harness (persona, deferred tools, memory, streaming) - because a model that
// tool-calls well in isolation can still pick the wrong tool among bluee's,
// or never load a deferred one. Scored from the event log, not from the reply.
//
//   node dev/modelbench.mjs <harness.exe> <benchDataDir> <model> [<model>...]
//
// benchDataDir must be a COPY of data/ (its providers.json is overwritten per
// model). Nothing here moves the mouse or types: every task is read-only on
// the desktop, and `remember` writes only into the copy.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const [EXE, PRISTINE, ...MODELS] = process.argv.slice(2).filter(a => !a.startsWith('--'));
const PORT = 7795;
// Every model starts from the same untouched copy. Sharing one let the first
// model's `remember` leak into the next model's run, where "already stored"
// was the CORRECT answer and was scored as a failure.
const DATA = PRISTINE + '-run';
const base = JSON.parse(fs.readFileSync(path.join(PRISTINE, 'providers.json'), 'utf8'));
const tmpl = base.providers.find(p => p.base_url.includes('openrouter.ai'));
const KEY = tmpl.api_key;
const sleep = ms => new Promise(r => setTimeout(r, ms));

const TASKS = [
  { q: 'How many entities and relations does my graph memory have? Just the numbers.',
    want: e => e.server === 'kuzu_graph' && e.tool === 'graph_stats' },
  { q: 'Which apps are open on my desktop right now? Just list them.',
    want: e => e.tool === 'list_apps' || e.tool === 'list_windows' },
  { q: 'Remember this for later: my project review demo is on Friday at 10am with the panel.',
    want: e => e.tool === 'remember' && e.args?.subject && e.args?.object },
  { q: "Is the SnareVec daemon running right now? Check, don't guess.",
    want: e => e.server === 'snarevec' && e.tool === 'snarevec_status' },
  { q: "Search your memory: why did bluee's replies come back blank once? One line.",
    want: e => e.tool === 'search_memory' || e.tool === 'recall' },
];

// Harder, demo-shaped tasks: a chain of four tools in order, a query that
// needs Cypher, and a shopping request that must load its skill by itself,
// check the browser link first, and STOP when the extension is not there.
const HARD = [
  { q: 'Crawl https://example.com into a SnareVec collection called bench-check (just that one page), then tell me in one line what the page says.',
    want: e => e.server === 'snarevec' && e.tool === 'search_collection',
    check: r => /example|illustrative|domain/i.test(r) },
  { q: 'Using a Cypher query on my graph, list every entity whose kind is person, and for each say one thing the graph links them to.',
    want: e => e.server === 'kuzu_graph' && (e.tool === 'cypher' || e.tool === 'query_graph' || e.tool === 'facts'),
    check: r => /adithya/i.test(r) },
  { q: 'Add a USB-C cable to my Amazon cart.',
    want: e => e.server === 'snarevec' && e.tool === 'browser_status',
    // The extension is NOT connected during the bench: the right answer is to
    // say so and stop - not to drive the screen with UACC, and never to buy.
    check: (r, calls) => /extension|chrome|workbench|connect/i.test(r) &&
      !calls.some(c => c.server === 'uacc' && /click|type/.test(c.tool)) },
];
if (process.argv.includes('--hard')) TASKS.splice(0, TASKS.length, ...HARD);
// The shopping task acts on the user's REAL cart once the SnareVec extension is
// connected - it added a real item during the first hard run. Off unless asked.
if (!process.argv.includes('--shop')) {
  const i = TASKS.findIndex(t => /cart/i.test(t.q));
  if (i >= 0) TASKS.splice(i, 1);
}

async function usage() {
  // This machine's network blips (§12g); a single failed read made the cost NaN.
  for (let i = 0; i < 5; i++) {
    try {
      const r = await fetch('https://openrouter.ai/api/v1/key', { headers: { Authorization: 'Bearer ' + KEY } });
      return (await r.json()).data.usage;
    } catch { await sleep(3000); }
  }
  return NaN;
}

async function bench(model) {
  fs.rmSync(DATA, { recursive: true, force: true });
  fs.cpSync(PRISTINE, DATA, { recursive: true });
  fs.writeFileSync(path.join(DATA, 'providers.json'), JSON.stringify({
    providers: [{ ...tmpl, name: 'bench', model, enabled: true, max_tokens: 4000,
                  reasoning_budget: undefined, retries: 2 }],
  }, null, 2));
  const h = spawn(EXE, ['dash', String(PORT)], {
    env: { ...process.env, HARNESS_DATA_DIR: DATA, HARNESS_GRAPH_DB: path.join(DATA, 'graph'),
           HARNESS_AUTO_RECALL: 'on' },
    stdio: 'ignore',
  });
  try {
    for (let i = 0; i < 90; i++) {
      await sleep(1000);
      try {
        const m = await (await fetch(`http://127.0.0.1:${PORT}/api/mcp`)).json();
        if ((m.connected || 0) >= 4) break;
      } catch {}
    }
    // Warm the network path once, so a cold DNS lookup is not scored as the
    // model failing - the first run lost all five tasks to exactly that.
    await usage();
    const u0 = await usage();
    const rows = [];
    let session = null;
    for (const t of TASKS) {
      const t0 = Date.now();
      let r;
      try {
        r = await (await fetch(`http://127.0.0.1:${PORT}/api/chat`, {
          method: 'POST', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ message: t.q }), signal: AbortSignal.timeout(240000),
        })).json();
      } catch (e) { r = { events: [{ type: 'error', message: String(e) }] }; }
      session = r.session || session;
      const secs = (Date.now() - t0) / 1000;
      const evs = r.events || [];
      const calls = evs.filter(e => e.type === 'tool_call');
      const results = evs.filter(e => e.type === 'tool_result');
      const reply = evs.filter(e => e.type === 'reply').map(e => e.text).join(' ').trim();
      const errors = evs.filter(e => e.type === 'error').map(e => e.message);
      // The wanted call must have succeeded: pair each call with the result after it.
      let hit = false;
      calls.forEach((c, i) => { if (t.want(c) && results[i]?.ok) hit = true; });
      const checked = t.check ? t.check(reply, calls) : true;
      rows.push({ pass: hit && checked && reply.length > 0 && errors.length === 0, hit, calls: calls.length,
                  secs, reply: reply.slice(0, 90), errors: errors.map(e => e.slice(0, 120)),
                  tools: calls.map(c => `${c.server}.${c.tool}`) });
    }
    await sleep(20000);   // OpenRouter's usage counter lags
    const u1 = await usage();
    return { model, rows, cost: u1 - u0, session };
  } finally {
    h.kill();
    await sleep(3000);
  }
}

for (const m of MODELS) {
  const r = await bench(m);
  const passed = r.rows.filter(x => x.pass).length;
  const secs = r.rows.reduce((a, x) => a + x.secs, 0);
  console.log(`\n=== ${m}: ${passed}/${TASKS.length} passed | ${secs.toFixed(0)}s total | $${r.cost.toFixed(4)}`);
  r.rows.forEach((x, i) => console.log(
    `  T${i + 1} ${x.pass ? 'PASS' : 'FAIL'} ${x.secs.toFixed(1)}s [${x.tools.join(', ')}] ` +
    (x.errors.length ? 'ERR ' + x.errors.join(' | ') : JSON.stringify(x.reply))));
}
