// Templates, per-agent model, caps and work folder (stage 4 of
// dev/plan-subagents.md).
//
//   harness dash 7788 &      then      node dev/uitemplates.mjs 7788
//
// A few cheap model turns. Proves: templates load and spawn with everything
// they say; the spawn dialog offers them; a per-agent model really answers;
// cost is tracked from the provider's own usage; a spending cap refuses work
// without running it; commands run in the agent's own folder; bluee can spawn
// from a template by name.
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const get = p => fetch(B + p).then(r => r.json());
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();
const agent = async id => (await get('/api/agents')).agents.find(a => a.id === id);
async function sayAndWait(id, text, ms = 90000) {
  const before = (await agent(id)).turns;
  await post('/api/agents/say', { id, text });
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const a = await agent(id);
    if (a.turns > before && a.status !== 'running') return a;
    await sleep(400);
  }
  return agent(id);
}

console.log('templates');
const tp = await get('/api/agents/templates');
const names = (tp.templates || []).map(t => t.name).sort();
check('four templates load, README skipped', names.join(',') === 'coder,desktop,researcher,shopper', names.join(','));

const r = await post('/api/agents', { template: 'researcher', name: 'r1' });
const ra = r.agent || {};
check('template spawn carries its settings',
  ra.template === 'researcher' && ra.servers?.join() === 'snarevec' && ra.max_cost === 0.5 && ra.max_turns === 40,
  JSON.stringify({ t: ra.template, s: ra.servers, c: ra.max_cost, mt: ra.max_turns }));
check('default folder is playground/agents/<name>', ra.folder === 'playground/agents/r1', ra.folder);
await post('/api/agents/stop', { id: ra.id });
const bad = await post('/api/agents', { template: 'nope' });
check('unknown template is refused, naming the real ones', /researcher/.test(bad.error || ''), bad.error);

console.log('per-agent model and cost');
const m = (await post('/api/agents', { name: 'luna', servers: [], model: 'openai/gpt-6-luna' })).agent;
const ma = await sayAndWait(m.id, 'Reply with exactly the word PONG and nothing else. No tools.');
check('the chosen model answered', /PONG/.test(ma.last) && ma.model === 'openai/gpt-6-luna', ma.last);
const ev = await get('/api/events?session=' + ma.session);
check('log records the model switch', (ev.events || []).some(e => e.kind === 'system' && /model for this agent: openai\/gpt-6-luna/.test(e.note || '')));
check('cost tracked from provider usage', ma.cost > 0 && ma.tokens > 0, `$${ma.cost} ${ma.tokens} tokens`);

console.log('spending cap');
await post('/api/agents/caps', { id: m.id, max_turns: null, max_cost: 0.0000001 });
const capped = await sayAndWait(m.id, 'Say hello.', 20000);
check('capped agent refuses without running', capped.status === 'failed' && /spending cap reached/.test(capped.last), capped.last);
const ev2 = await get('/api/events?session=' + ma.session);
check('nothing was sent to the model', !(ev2.events || []).some(e => e.kind === 'user_message' && e.text === 'Say hello.'));
await post('/api/agents/stop', { id: m.id });

console.log('work folder');
const f = (await post('/api/agents', { name: 'folder test', servers: [] })).agent;
const fa = await sayAndWait(f.id, 'Use run_command to run `cd` (no root argument) and reply with only the path it printed.');
// The model may wrap the path in a sentence or backticks - look for it anywhere.
check('commands run in its own folder', /artifacts[\\/]+agents[\\/]+folder-test\b/i.test(fa.last), fa.last);
await post('/api/agents/stop', { id: f.id });

console.log('spawn dialog and bluee');
const { chrome, target } = await launch(PORT, { cdp: 9364, profile: 'bluee-templates' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof agSpawn === 'function' && document.readyState === 'complete'", 20000);
await sleep(500);
await p.ev("app.classList.add('agents-on'); syncRight(); await agSpawn(); return 1");
await p.until("return document.querySelectorAll('#connmenu .crow.tpl').length>0", 5000);
const tcount = await p.ev("return document.querySelectorAll('#connmenu .crow.tpl').length");
check('spawn dialog lists the templates', tcount === 4, 'n=' + tcount);
await p.ev("document.querySelector('#connmenu .crow.tpl[data-t=\"desktop\"]').click(); return 1");
await p.shot('templates-1-dialog');
check('picking one highlights it', await p.ev("return document.querySelector('#connmenu .crow.tpl.on')?.dataset.t==='desktop'"));
await p.ev("document.querySelector('#agcreate').click(); return 1");
const dw = await target(u => u.includes('only=agent'), 40);
check('a desktop agent window opened', !!dw);
const desk = (await get('/api/agents')).agents.find(a => a.template === 'desktop');
check('it is the desktop template', !!desk && desk.servers.join() === 'uacc', JSON.stringify(desk?.servers));
if (desk) await post('/api/agents/stop', { id: desk.id });

await p.ev("const i=document.querySelector('#input'); i.value='Spawn a sub-agent from the researcher template, named scout, with no task. Then just say done.'; i.dispatchEvent(new Event('input')); send(); return 1");
await p.until("return !busy && [...document.querySelectorAll('#stream .msg.ai')].length>0", 90000);
const scout = (await get('/api/agents')).agents.find(a => a.name === 'scout');
check('bluee spawned from the template by name', scout?.template === 'researcher', JSON.stringify(scout && { t: scout.template, s: scout.servers }));
if (scout) await post('/api/agents/stop', { id: scout.id });

check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
