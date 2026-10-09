// One browser per sub-agent (stage 5 of dev/plan-subagents.md).
//
//   harness dash 7788 &      then      node dev/uibrowsers.mjs 7788
//
// Uses the native fallback (bluee's own Chrome/Edge per agent), because the
// SnareVec half needs Adithya's real browsers with the updated extension -
// that half is covered by SnareVec's own tests/test_browser_actions.py.
// A few cheap model turns.
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const get = p => fetch(B + p).then(r => r.json());
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();
const agent = async id => (await get('/api/agents')).agents.find(a => a.id === id);
async function waitTurn(id, before, ms = 120000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const a = await agent(id);
    if (a && a.turns > before && a.status !== 'running') return a;
    await sleep(500);
  }
  return agent(id);
}

console.log('what is available');
const br = await get('/api/browsers');
const kinds = (br.browsers || []).map(b => b.id);
check('installed browsers are listed', kinds.includes('chrome') && kinds.includes('edge'), kinds.join(','));

console.log('two agents, two browsers, at the same time');
const a = (await post('/api/agents', { name: 'edgey', servers: [], browser: 'edge' })).agent;
const c = (await post('/api/agents', { name: 'chromey', servers: [], browser: 'chrome' })).agent;
check('both spawned with their browser', a?.browser === 'edge' && c?.browser === 'chrome');
const third = await post('/api/agents', { name: 'thief', servers: [], browser: 'edge' });
check('a second agent cannot take an owned browser', /already owned by sub-agent `edgey`/.test(third.error || ''), third.error);
const owned = await get('/api/browsers');
check('the list says who owns it', (owned.browsers || []).some(b => b.id === 'edge' && b.owner === 'edgey'));

await post('/api/agents/say', { id: a.id, text: 'Use web_open on https://example.com then web_read, and reply with only the page title.' });
await post('/api/agents/say', { id: c.id, text: 'Use web_open on https://www.wikipedia.org then web_read, and reply with only the page title.' });
const t0 = Date.now();
const [ra, rc] = await Promise.all([waitTurn(a.id, 0), waitTurn(c.id, 0)]);
console.log(`  both done in ${((Date.now() - t0) / 1000).toFixed(1)}s`);
check('edge agent read its page', /example domain/i.test(ra.last), ra.last);
check('chrome agent read its page', /wikipedia/i.test(rc.last), rc.last);
const evA = await get('/api/events?session=' + ra.session);
const calls = (evA.events || []).filter(e => e.kind === 'tool_call').map(e => e.tool);
// web_open already returns the title, so a model may rightly skip web_read.
check('it used its web tools', calls.includes('web_open'), calls.join(','));
const res = (evA.events || []).filter(e => e.kind === 'tool_result').map(e => JSON.stringify(e.result));
check('every result came from its own browser', res.filter(r => r.includes('"browser"')).every(r => r.includes('"edge"')), res.length + ' results');

console.log('clicking and typing by text');
await post('/api/agents/say', { id: c.id, text: 'In your browser: web_type "Ada Lovelace" into the search field with submit true, then web_read and reply with only the page title.' });
const rc2 = await waitTurn(c.id, rc.turns);
check('typed, submitted and landed on the article', /lovelace/i.test(rc2.last), rc2.last);

console.log('pinning');
const pinned = (await post('/api/agents', { name: 'pinned', servers: ['snarevec'], browser: 'brave' })).agent;
if (pinned) {
  await post('/api/agents/say', { id: pinned.id, text: 'Call snarevec__browser_list_tabs with browser set to "chrome" exactly as written, and tell me what it said.' });
  const rp = await waitTurn(pinned.id, 0);
  const evp = await get('/api/events?session=' + rp.session);
  const refused = (evp.events || []).some(e => e.kind === 'tool_result' && /belongs to someone else/.test(JSON.stringify(e.result)));
  const noOther = !(evp.events || []).some(e => e.kind === 'tool_call' && e.tool === 'browser_list_tabs' && e.args?.browser === 'chrome' &&
    (evp.events || []).some(r => r.kind === 'tool_result' && r.call_id === e.call_id && r.ok));
  check('another browser is refused for a pinned agent', refused && noOther, rp.last.slice(0, 120));
  await post('/api/agents/stop', { id: pinned.id });
}

console.log('spawn dialog');
const { chrome, target } = await launch(PORT, { cdp: 9366, profile: 'bluee-browsers' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof agSpawn === 'function' && document.readyState === 'complete'", 20000);
await p.ev("app.classList.add('agents-on'); syncRight(); await agSpawn(); return 1");
await p.until("return document.querySelectorAll('#connmenu .crow.brw').length>0", 5000);
const rows = await p.ev("return [...document.querySelectorAll('#connmenu .crow.brw')].map(r=>r.dataset.b+(r.classList.contains('taken')?':taken':''))");
check('dialog offers browsers and greys out owned ones', rows.includes('edge:taken') && rows.includes('chrome:taken'), rows.join(' '));
await p.shot('browsers-1-dialog');
check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();

for (const x of (await get('/api/agents')).agents) await post('/api/agents/stop', { id: x.id });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
