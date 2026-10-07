// Drive the sub-agents panel in a real browser.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7792';
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
const CDP = 9340;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-agents-profile')}`,
  '--no-first-run', '--no-default-browser-check',
  '--window-size=1440,900', `http://127.0.0.1:${PORT}/`,
], { stdio: 'ignore' });
const sleep = ms => new Promise(r => setTimeout(r, ms));

let page;
for (let i = 0; i < 60; i++) {
  try {
    const l = await fetch(`http://127.0.0.1:${CDP}/json/list`).then(r => r.json());
    page = l.find(t => t.type === 'page' && t.url.includes(`:${PORT}`));
    if (page) break;
  } catch (_) {}
  await sleep(300);
}
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let id = 0; const waiting = new Map(); const errs = [];
ws.addEventListener('message', e => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    errs.push('exception: ' + (m.params.exceptionDetails?.exception?.description || '').split('\n')[0]);
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    errs.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});
const send = (m, p = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method: m, params: p }));
  return new Promise(r => waiting.set(mid, r));
};
await send('Runtime.enable'); await send('Page.enable');
const ev = async expr => {
  const r = await send('Runtime.evaluate', {
    expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails)
    throw new Error(r.result.exceptionDetails.exception?.description || 'threw');
  return r.result?.result?.value;
};
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const f = path.join(process.cwd(), 'dev', 'shots', name + '.png');
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  return f;
}
const fails = [];
const check = (n, ok, d = '') => {
  console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '  ' + d : ''}`);
  if (!ok) fails.push(n);
};

for (let i = 0; i < 60; i++) {
  if (await ev("return typeof loadAgents === 'function'")) break;
  await sleep(300);
}
await sleep(1500);

console.log('=== RAIL ===');
check('button exists under terminal', await ev(`
  const rail = [...document.querySelectorAll('.rb')];
  const t = rail.findIndex(b => b.dataset.toggle === 'term');
  const a = rail.findIndex(b => b.dataset.toggle === 'agents');
  return a === t + 1;
`));

await ev("document.querySelector('.rb[data-toggle=\"agents\"]').click()");
await sleep(1200);

console.log('\n=== PANEL, AND THAT IT IS ITS OWN ===');
/* The point of this section is SEPARATION. It used to assert the opposite -
   that the pane sat inside #side's box - which was true because it was a
   section welded under the log. Now the assertion is that they are two panels:
   siblings in the column, not overlapping, each with its own card edge. */
const pane = await ev(`
  const p = document.querySelector('#agpane');
  const s = document.querySelector('#side');
  const col = document.querySelector('#rightcol');
  const r = p.getBoundingClientRect(), sr = s.getBoundingClientRect();
  const cr = col.getBoundingClientRect();
  const cs = getComputedStyle(p), ss = getComputedStyle(s);
  return { visible: p.offsetParent !== null, h: Math.round(r.height),
           nested: s.contains(p),
           inCol: r.top >= cr.top - 1 && r.bottom <= cr.bottom + 1,
           overlaps: r.top < sr.bottom - 1,
           gap: Math.round(r.top - sr.bottom),
           radius: cs.borderTopLeftRadius, sideRadius: ss.borderTopLeftRadius,
           border: cs.borderTopWidth, sideBorder: ss.borderTopWidth,
           lit: document.querySelector('.rb[data-toggle="agents"]').classList.contains('on') };
`);
check('panel visible', pane.visible);
check('has height', pane.h > 100, 'h=' + pane.h);
check('NOT nested inside the log panel', !pane.nested);
check('sits inside the right column', pane.inCol);
check('does not overlap the log panel', !pane.overlaps, 'gap=' + pane.gap + 'px');
check('separated by a visible gap', pane.gap >= 4, 'gap=' + pane.gap + 'px');
check('same corner radius as the log panel', pane.radius === pane.sideRadius,
  pane.radius + ' vs ' + pane.sideRadius);
check('same edge as the log panel', pane.border === pane.sideBorder,
  pane.border + ' vs ' + pane.sideBorder);
check('rail button lit', pane.lit);

/* The bug that made "separate" more than cosmetic: the pane lived inside #side,
   so closing the log closed the sub-agents too. */
await ev("document.querySelector('[data-toggle=\\\"side\\\"]').click()");
await sleep(400);
const alone = await ev(`
  const p = document.querySelector('#agpane').getBoundingClientRect();
  return { agH: Math.round(p.height), agW: Math.round(p.width),
           sideShown: document.querySelector('#side').offsetParent !== null };
`);
check('closing the log hides the log', !alone.sideShown);
check('...and leaves sub-agents open', alone.agH > 100 && alone.agW > 100,
  alone.agW + 'x' + alone.agH);
await ev("document.querySelector('[data-toggle=\\\"side\\\"]').click()");
await sleep(400);
check('reopening the log brings it back',
  await ev("return document.querySelector('#side').offsetParent !== null"));

/* Each panel closes itself from its own header, the way the reference panels
   do. Closing one must leave the other alone - that is the whole claim. */
await ev("document.querySelector('#agclose').click()");
await sleep(400);
const closed = await ev(`
  const vis = el => !!el && el.offsetParent !== null;
  return { ag: vis(document.querySelector('#agpane')),
           side: vis(document.querySelector('#side')),
           lit: document.querySelector('.rb[data-toggle="agents"]').classList.contains('on') };
`);
check('the panel closes from its own header', !closed.ag);
check('...without touching the log panel', closed.side);
check('...and the rail button goes dark', !closed.lit);
await ev("document.querySelector('.rb[data-toggle=\\\"agents\\\"]').click()");
await sleep(900);
check('rail button reopens it',
  await ev("return document.querySelector('#agpane').offsetParent !== null"));
/* Relative, not absolute. A sub-agent lives in the SERVER process, so a second
   run against the same dashboard inherits the first run's agents - the empty
   state is only correct on a cold one. Asserting a change means the check says
   the same thing either way. */
const startRows = await ev("return document.querySelectorAll('#aglist .agrow').length");
check(startRows === 0
  ? 'empty state explains itself'
  : 'inherited ' + startRows + ' agent(s) from an earlier run, empty state skipped',
  startRows > 0 ||
  (await ev("return document.querySelector('#aglist').textContent")).includes('No sub-agents'));

console.log('\n=== SPAWN ===');
await ev("document.querySelector('#agnew').click()");
await sleep(700);
check('spawn menu lists connected servers',
  (await ev("return document.querySelectorAll('#connmenu .crow').length")) >= 1);
await ev(`
  document.querySelector('#agname').value = 'grapher';
  document.querySelector('#agpurpose').value = 'graph questions';
  const row = [...document.querySelectorAll('#connmenu .crow')].find(r => r.dataset.s === 'kuzu_graph');
  if (row) row.click();
  document.querySelector('#agcreate').click();
`);
await sleep(2500);
const after = await ev(`
  return { rows: document.querySelectorAll('#aglist .agrow').length,
           text: document.querySelector('#aglist').textContent.slice(0,80),
           count: document.querySelector('#agcount').textContent };
`);
check('a new agent row appeared', after.rows === startRows + 1,
  startRows + ' -> ' + after.rows);
check('count shown', /of \d/.test(after.count), after.count);

/* Opening a sub-agent must write NOTHING to the log. Creating the agent eagerly
   meant ten tabs were ten empty conversations in the Sessions page - §4a records
   what happened, and an agent that was opened and never used did not happen. */
const fresh = await ev(`
  const d = await (await fetch('/api/agents')).json();
  const a = d.agents[d.agents.length - 1];
  return { session: a.session, tools: a.tools, status: a.status };
`);
check('a new sub-agent has no session until it works',
  fresh.session === '' && fresh.tools === 0, JSON.stringify(fresh));
check('and the panel says so, not "starting"',
  (await ev(`
     [...document.querySelectorAll('#aglist .agrow')].pop().click();
     await new Promise(r=>setTimeout(r,500));
     return document.querySelector('#agstream').textContent;
   `)).includes('Not started yet'));

console.log('\n=== CLOCK ===');
await ev("document.querySelector('#agclock').click()");
await sleep(600);
const clock = await ev(`
  const m = document.querySelector('#connmenu');
  return { open: !!m, text: m ? m.textContent.slice(0,120) : '',
           rows: m ? m.querySelectorAll('.crow').length : 0 };
`);
check('clock menu opens', clock.open);
check('offers never as the default', clock.text.includes('Default is never'), '');
check('offers timer choices', clock.rows >= 4, 'rows=' + clock.rows);
await ev(`
  const row = [...document.querySelectorAll('#connmenu .crow')].find(r => r.dataset.m === '15');
  if (row) row.click();
`);
await sleep(900);
check('timer applied to the agent',
  (await ev("return document.querySelector('#aglist').textContent")).includes('15m'));

console.log('\n=== RESIZE ===');
/* From a KNOWN height. `--agh` persists in localStorage, so successive runs
   inherited each other's drags until the pane started near the
   `innerHeight - 200` clamp - at which point the drag did nothing and the
   check failed while the code was correct. Same trap as §12c, third costume. */
await ev(`
  document.querySelector('#app').style.setProperty('--agh','240px');
  try { localStorage.setItem('bluee.agh','240px'); } catch(_){}
`);
await sleep(200);
const before = await ev("return Math.round(document.querySelector('#agpane').getBoundingClientRect().height)");
await ev(`
  const g = document.querySelector('#agdrag');
  const r = g.getBoundingClientRect();
  const y = r.top + r.height/2;
  g.dispatchEvent(new MouseEvent('mousedown', {clientY:y, bubbles:true}));
  dispatchEvent(new MouseEvent('mousemove', {clientY:y-80, bubbles:true}));
  dispatchEvent(new MouseEvent('mouseup', {clientY:y-80, bubbles:true}));
`);
await sleep(500);
const afterH = await ev("return Math.round(document.querySelector('#agpane').getBoundingClientRect().height)");
check('drag resizes the pane', afterH > before + 30, before + ' -> ' + afterH);

console.log('\n=== DRAG OUT ===');
/* A drag that stays inside the panel must NOT detach - otherwise every click
   becomes a window. Then a drag that leaves must arm it. popOut is stubbed so
   the check does not actually open windows. */
await ev(`
  window.__popped = [];
  window.__realPop = window.popOut;
  popOut = (view, opts) => { window.__popped.push({view, id: opts && opts.id}); return 'stub'; };
`);
const drag = async (dx, dy) => ev(`
  const row = document.querySelector('#aglist .agrow');
  const r = row.getBoundingClientRect();
  const x = r.left + 20, y = r.top + r.height/2;
  row.dispatchEvent(new MouseEvent('mousedown', {clientX:x, clientY:y, button:0, bubbles:true}));
  dispatchEvent(new MouseEvent('mousemove', {clientX:x+${dx}, clientY:y+${dy}, bubbles:true}));
  await new Promise(z=>setTimeout(z,60));
  const g = document.querySelector('.agghost');
  const seen = { ghost: !!g, out: g ? g.classList.contains('out') : false };
  dispatchEvent(new MouseEvent('mouseup', {clientX:x+${dx}, clientY:y+${dy}, bubbles:true}));
  await new Promise(z=>setTimeout(z,150));
  seen.popped = window.__popped.length;
  return seen;
`);

const tiny = await drag(3, 3);
check('a click does not detach', tiny.popped === 0 && !tiny.ghost, JSON.stringify(tiny));
const inside = await drag(30, 20);
check('dragging inside the panel does not detach', inside.popped === 0, JSON.stringify(inside));
const outside = await drag(-700, 0);
check('dragging out opens a window', outside.popped === 1, JSON.stringify(outside));
check('ghost marked as leaving', outside.out === true);
check('it carries the agent id',
  !!(await ev("return window.__popped[window.__popped.length-1]?.id")));
await ev("popOut = window.__realPop;");

console.log('\n=== RESUME ===');
await ev("document.querySelector('#agnew').dispatchEvent(new MouseEvent('click',{altKey:true,bubbles:true}))");
await sleep(1000);
const res = await ev(`
  const m = document.querySelector('#connmenu');
  return { open: !!m, text: m ? m.textContent.slice(0,200) : '',
           servers: m ? m.querySelectorAll('.crow.srv').length : 0 };
`);
check('alt-click opens resume', res.open);
check('says the toolset is not restored', /not restored|cost decision/.test(res.text));
check('offers connected servers', res.servers >= 1, 'servers=' + res.servers);
await ev("document.querySelector('#connmenu')?.remove()");

console.log('\n=== SOLO ===');
/* At the SIZE THE WINDOW ACTUALLY OPENS AT. This check used to run at 1440px
   and passed, while the real pop-out - 520x640 - rendered an empty black
   rectangle, because `@media (max-width:900px)` hides the companion column and
   a pop-out IS that column. A solo check at a width no pop-out ever has is not
   a check. */
await send('Emulation.setDeviceMetricsOverride',
  { width: 520, height: 640, deviceScaleFactor: 1, mobile: false });
await ev("location.href='/?only=agents'");
await sleep(2500);
for (let i = 0; i < 40; i++) {
  if (await ev("return typeof loadAgents === 'function'")) break;
  await sleep(300);
}
await sleep(1500);
const solo = await ev(`
  const p = document.querySelector('#agpane');
  const vis = el => !!el && el.offsetParent !== null && el.getBoundingClientRect().height > 5;
  return { pane: vis(p), h: Math.round(p.getBoundingClientRect().height),
           chatGone: !vis(document.querySelector('#welcome')),
           railGone: !vis(document.querySelector('#rail')),
           tabsGone: !vis(document.querySelector('#sidetabs')) };
`);
check('solo pane fills the window at 520x640', solo.pane && solo.h > 300, 'h=' + solo.h);
check('no chat page bleeding in', solo.chatGone);
check('no rail', solo.railGone);
check('no LOG/TASKS tabs', solo.tabsGone);
/* These used to be live in a pop-out, and pressing close blanked the window:
   it removes the class the panel is shown by, which in the page means "put it
   away" and in a window means "erase yourself". */
check('no self-destruct buttons in the header', await ev(`
  const vis = el => !!el && el.offsetParent !== null;
  return !vis(document.querySelector('#agclose')) &&
         !vis(document.querySelector('#agpop'));
`));

console.log('\nshot: ' + await shot('subagents'));
console.log('\n=== CONSOLE ===');
if (errs.length) { errs.forEach(e => console.log('  ' + e)); fails.push('console errors'); }
else console.log('  none');
console.log('\n' + (fails.length ? 'FAILED: ' + fails.join(', ') : 'all sub-agent panel checks passed'));
try { chrome.kill(); } catch (_) {}
process.exit(fails.length ? 1 : 0);
