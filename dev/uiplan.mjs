// Stage D of dev/plan-workspaces.md: the project progress panel.
//
//   harness dash 7788 &      then      node dev/uiplan.mjs 7788
//
// A throwaway repo with a plan.md; the panel must show phase, progress, the
// current phase's tasks, Waiting on you and Next - from the file, no model.
// Then ONE cheap live turn: bluee ticks a task with edit_file, and the panel
// follows. Also checks the empty state for a repo with no plan.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();

const repo = path.join(os.tmpdir(), 'bluee-plan-check-' + Date.now());
fs.mkdirSync(path.join(repo, '.bluee'), { recursive: true });
fs.writeFileSync(path.join(repo, '.bluee', 'plan.md'), `# Invoice SaaS
> Small-business invoicing

## Phase 1: Foundations
- [x] repo and CI
- [x] project layout

## Phase 2: Core
- [x] invoice model
- [~] auth
- [ ] payments

## Phase 3: Launch
- [ ] landing page

## Next
- finish auth

## Waiting on you
- pick the pricing tiers

## Log
- 2026-10-08: invoice model done
`);
const bare = path.join(os.tmpdir(), 'bluee-noplan-' + Date.now());
fs.mkdirSync(bare);
const id = (await post('/api/roots', { path: repo })).id;
const id2 = (await post('/api/roots', { path: bare })).id;

const api = await fetch(`${B}/api/plan?root=${id}`).then(r => r.json());
check('API parses the plan', api.exists && api.plan.current === 1 && api.plan.done === 3 && api.plan.total === 6,
  JSON.stringify({ cur: api.plan?.current, d: api.plan?.done, t: api.plan?.total }));

const { chrome, target } = await launch(PORT, { cdp: 9374, profile: 'bluee-plan' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof planPanel === 'function' && document.readyState === 'complete'", 20000);
await p.ev("document.querySelector('.rb[data-page=\"play\"]').click(); return 1");
await sleep(600);
const pick = async r => p.ev(`await loadRoots?.(); const s=document.querySelector('#pgroot'); s.value='${r}'; s.dispatchEvent(new Event('change')); return 1`);

await pick(id2);
check('a repo without a plan says how to get one', await p.until("return /No project plan yet/.test(document.querySelector('#playbody').textContent)", 5000));

await pick(id);
await p.until("return !!document.querySelector('#playbody .plan h3')", 5000);
const v = await p.ev(`const b=document.querySelector('#playbody .plan'); return {
  title:b.querySelector('h3').textContent, meta:b.querySelector('.pl-pmeta').textContent,
  cur:b.querySelector('.pl-ph.pl-cur .pl-pt span').textContent, tasks:[...b.querySelectorAll('.pl-ph.pl-cur .pl-tasks li')].map(l=>l.className+':'+l.textContent),
  wait:b.querySelector('.pl-psec.wait')?.textContent||'', next:b.querySelector('.pl-psec.next')?.textContent||'',
  bar:b.querySelector('.pl-pbar i').style.width }`);
check('title and where we are', v.title === 'Invoice SaaS' && /Phase 2 of 3/.test(v.meta) && /3\/6 tasks/.test(v.meta), v.meta);
check('current phase shown with its tasks', v.cur === 'Phase 2: Core' && v.tasks.length === 3 && v.tasks[1].startsWith('doing'), v.tasks.join(' | '));
check('waiting on you and next', /pricing tiers/.test(v.wait) && /finish auth/.test(v.next));
check('progress bar at 50%', v.bar === '50%', v.bar);
await p.shot('plan-1-panel');

await p.ev("document.querySelector('#planopen').click(); return 1");
check('plan.md opens from the panel', await p.until("return document.querySelector('#pgname').textContent==='.bluee/plan.md'", 5000));

console.log('bluee ticks a task');
const ws = new WebSocket(B.replace('http', 'ws') + '/ws/chat');
await new Promise(r => (ws.onopen = r));
const done = new Promise(r => (ws.onmessage = m => { if (JSON.parse(m.data).type === 'done') r(); }));
ws.send(JSON.stringify({ message: `In granted folder "${id}": payments is finished. Update .bluee/plan.md accordingly with edit_file (tick it), then reply done.` }));
await done; ws.close();
await p.ev("document.querySelector('#pgback')?.click(); await loadPlayground(); return 1");
const after = await p.ev("return document.querySelector('#playbody .plan .pl-pmeta')?.textContent||''");
check('panel follows the file bluee edited', /4\/6 tasks/.test(after), after);

check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
await post('/api/roots/remove', { id }); await post('/api/roots/remove', { id: id2 });
fs.rmSync(repo, { recursive: true, force: true }); fs.rmSync(bare, { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
