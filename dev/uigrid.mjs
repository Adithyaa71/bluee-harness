// The Agents page (stage 6 of dev/plan-subagents.md), in real Chrome.
//
//   harness dash 7788 &      then      node dev/uigrid.mjs 7788
//
// One cheap model turn. Cards appear for each agent, show the tool an agent
// is in WHILE it is in it, show cost and the sleep countdown afterwards, and
// the card buttons sleep and stop.
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const get = p => fetch(B + p).then(r => r.json());
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();
// Start from no agents: an earlier check may have left some running, and a
// check that inherits state proves nothing (§12c).
for (const x of (await get('/api/agents')).agents) await post('/api/agents/stop', { id: x.id });
const { chrome, target } = await launch(PORT, { cdp: 9368, profile: 'bluee-grid' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof renderGrid === 'function' && document.readyState === 'complete'", 20000);
await sleep(600);

await p.ev("document.querySelector('.rb[data-page=\"agents\"]').click(); return 1");
await sleep(400);
check('page opens with an empty state', await p.ev("return document.querySelector('#v-agents').classList.contains('on') && !!document.querySelector('#agrid .agempty')"));

const a = (await post('/api/agents', { name: 'gridder', purpose: 'grid check', servers: [] })).agent;
const b = (await post('/api/agents', { name: 'idler', purpose: 'stays idle', servers: [], browser: 'edge' })).agent;
// The spawn opens windows in this headless browser; the grid is what we look at.
await p.until("return document.querySelectorAll('#agrid .agcard').length===2", 8000);
check('a card per agent', await p.ev("return document.querySelectorAll('#agrid .agcard').length") === 2);
check('browser shown on the card', await p.ev("return document.querySelector('.agcard[data-id=\"" + b.id + "\"] .meta').textContent.includes('edge')"));

await post('/api/agents/say', { id: a.id, text: 'Call list_skills once, then reply with how many skills there are, as a number only.' });
const sawTool = await p.until("const n=document.querySelector('.agcard[data-id=\"" + a.id + "\"] .now .act'); return n && /list_skills|thinking/.test(n.textContent)", 30000);
const seen = await p.ev("return document.querySelector('.agcard[data-id=\"" + a.id + "\"] .now').textContent");
check('live activity while it works', sawTool, seen);
await p.shot('grid-1-working');
const done = await p.until("const c=document.querySelector('.agcard[data-id=\"" + a.id + "\"]'); return c && c.classList.contains('ready')", 90000);
const card = await p.ev("const c=document.querySelector('.agcard[data-id=\"" + a.id + "\"]'); return {now:c.querySelector('.now').textContent, cost:c.querySelector('.cost').textContent, when:c.querySelector('.when').textContent}");
check('after: shows its answer, cost and sleep countdown', done && /\d/.test(card.now) && /\$0\.\d{4}/.test(card.cost) && /sleeps in/.test(card.when), JSON.stringify(card));
check('summary counts and totals', /2 of \d+ · 0 working · \$0\.\d{4} spent/.test(await p.ev("return document.querySelector('#agridsum').textContent")),
  await p.ev("return document.querySelector('#agridsum').textContent"));
await p.shot('grid-2-done');

await p.ev("document.querySelector('.agcard[data-id=\"" + b.id + "\"] button[data-a=\"sleep\"]').click(); return 1");
check('sleep button puts it to sleep', await p.until("return document.querySelector('.agcard[data-id=\"" + b.id + "\"]')?.classList.contains('sleeping')", 5000));
check('a sleeping card counts down to its end', /ends in/.test(await p.ev("return document.querySelector('.agcard[data-id=\"" + b.id + "\"] .when').textContent")));

await p.ev("const s=document.querySelector('.agcard[data-id=\"" + b.id + "\"] button[data-a=\"stop\"]'); s.click(); return 1");
await sleep(300);
check('stop needs a second click', await p.ev("return document.querySelectorAll('#agrid .agcard').length") === 2);
await p.ev("document.querySelector('.agcard[data-id=\"" + b.id + "\"] button[data-a=\"stop\"]').click(); return 1");
check('second click ends it', await p.until("return document.querySelectorAll('#agrid .agcard').length===1", 5000));

await p.ev("document.querySelector('#agridnew').click(); return 1");
const dlg = await p.until("const m=document.querySelector('#connmenu'); if(!m) return false; const r=m.getBoundingClientRect(); return r.top>=0 && r.bottom<=innerHeight+1", 5000);
check('+ new agent opens the dialog on screen', dlg);
await p.shot('grid-3-new');

check('console clean', p.errs.length === 0, p.errs.join(' | '));
for (const x of (await get('/api/agents')).agents) await post('/api/agents/stop', { id: x.id });
chrome.kill();
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
