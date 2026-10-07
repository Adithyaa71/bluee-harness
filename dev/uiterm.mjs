// Terminal checks, in a real browser over CDP.
//
//   node dev/uiterm.mjs <port> [--head]
//
// The bug this exists for: there used to be ONE xterm instance shared by every
// tab. Switching tabs closed the socket and opened another, but never touched
// the buffer - and the server replays up to 256 KB of scrollback on attach, so
// the incoming session's history landed on top of the outgoing one's. Two
// shells interleaved in one buffer.
//
// So the load-bearing assertion here is buffer ISOLATION: a marker written into
// one tab must never appear in another, and must survive switching away and
// back. Everything else in this file is secondary to that.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const HEAD = process.argv.includes('--head');
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP_PORT = 9334;   // not uicheck's 9333, so the two can run side by side
const profile = path.join(os.tmpdir(), 'bluee-uiterm-profile');

const chrome = spawn(CHROME, [
  HEAD ? '--new-window' : '--headless=new',
  `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${profile}`,
  '--no-first-run', '--no-default-browser-check',
  '--window-size=1440,900',
  `http://127.0.0.1:${PORT}/`,
], { stdio: 'ignore', detached: false });

const sleep = ms => new Promise(r => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`).then(r => r.json());
      const page = list.find(t => t.type === 'page' && t.url.includes(`:${PORT}`));
      if (page) return page;
    } catch (_) {}
    await sleep(300);
  }
  throw new Error('chrome never exposed the page over CDP');
}

const t = await target();
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));

let id = 0;
const waiting = new Map();
const consoleErrors = [];
ws.addEventListener('message', ev => {
  const m = JSON.parse(ev.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    consoleErrors.push('exception: ' + (m.params.exceptionDetails?.exception?.description
      || m.params.exceptionDetails?.text));
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    consoleErrors.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});

function send(method, params = {}) {
  const mid = ++id;
  ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(res => waiting.set(mid, res));
}

await send('Runtime.enable');
await send('Page.enable');

async function evaluate(expr) {
  const r = await send('Runtime.evaluate', {
    expression: `(async()=>{ ${expr} })()`,
    awaitPromise: true, returnByValue: true,
  });
  if (r.result?.exceptionDetails)
    throw new Error(r.result.exceptionDetails.exception?.description || 'page threw');
  return r.result?.result?.value;
}

async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const file = path.join(process.cwd(), 'dev', 'shots', name + '.png');
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, Buffer.from(r.result.data, 'base64'));
  return file;
}

const fails = [];
const log = (...a) => console.log(...a);
const check = (name, ok, detail = '') => {
  log(`  ${ok ? 'ok  ' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
  if (!ok) fails.push(name);
};

/* Wait for a condition instead of sleeping a fixed amount.
   A fixed sleep after reload is why the first version of this file failed: at
   3.5s the page script sometimes had not run yet, and `typeof app` hides that
   while `app.classList` throws. A test whose result depends on how busy the
   machine is proves nothing either way. */
async function waitFor(expr, what, ms = 25000) {
  const started = Date.now();
  while (Date.now() - started < ms) {
    try { if (await evaluate('return !!(' + expr + ')')) return Date.now() - started; }
    catch (_) {}
    await sleep(250);
  }
  throw new Error('timed out waiting for ' + what);
}

const READY = "typeof app!=='undefined' && typeof panes!=='undefined' && typeof openTerm==='function'";

/* Known state. The tab list and the saved connections persist, so without this
   a run inherits the last one and the numbers stop being comparable.

   The clear has to happen AFTER the first load. CDP hands you the target as
   soon as the tab exists, which can be while the document is still about:blank
   - so clearing straight away wiped the wrong origin and left the real data
   untouched. The symptom was a run that saw the previous run's tabs and
   connections and failed on counts that were correct for a clean profile. */
log('first load after', await waitFor(READY, 'the page script to run'), 'ms');
await evaluate("try{ localStorage.clear(); }catch(_){}");
await evaluate("location.reload()");
await sleep(600);
log('page ready after', await waitFor(READY, 'the reload'), 'ms');
check('profile really is clean',
  (await evaluate("return (JSON.parse(localStorage.getItem('bluee.conns')||'[]')).length")) === 0);

// Open the terminal strip if it is collapsed.
await evaluate(`
  if (app.classList.contains('no-term'))
    document.querySelector('.rb[data-toggle="term"]').click();
`);
await waitFor('panes.size >= 1', 'the first pane to be created');
await sleep(600);

log('\n=== STRIP ===');
check('strip visible', await evaluate("return !app.classList.contains('no-term')"));
check('one tab to start', (await evaluate('return termTabs.length')) === 1,
  'tabs=' + await evaluate('return termTabs.length'));
check('one pane to start', (await evaluate('return panes.size')) === 1,
  'panes=' + await evaluate('return panes.size'));

log('\n=== CONNECT MENU ===');
await evaluate("document.querySelector('#termnew').click()");
await sleep(400);
const rows = await evaluate("return document.querySelectorAll('#connmenu .crow').length");
check('menu lists the local shells', rows >= 3, 'rows=' + rows);
check('menu has an add form', await evaluate("return !!document.querySelector('#connmenu #cncmd')"));

/* Count first, assert the DELTA. The first version asserted the saved list was
   exactly 1 and failed on the second run, because the reload-and-clear at the
   top does not always land before the page reads localStorage - so it was
   measuring the previous run's leftovers. Asserting a change rather than an
   absolute makes the check mean the same thing whatever it inherits. */
const connsBefore = await evaluate(
  "return (JSON.parse(localStorage.getItem('bluee.conns')||'[]')).length");

// Add a named remote. It must persist AND open a tab.
await evaluate(`
  document.querySelector('#cnname').value = 'pi-' + Date.now().toString(36);
  document.querySelector('#cncmd').value  = 'ssh pi@raspberrypi.local';
  document.querySelector('#cnsave').click();
`);
await sleep(900);
const connsAfter = await evaluate(
  "return (JSON.parse(localStorage.getItem('bluee.conns')||'[]')).length");
check('saved connection persists', connsAfter === connsBefore + 1,
  connsBefore + ' -> ' + connsAfter);
check('remote tab opened', (await evaluate('return termTabs.length')) === 2,
  'tabs=' + await evaluate('return termTabs.length'));
// Put the active tab back on the first one, so the isolation section starts
// from a known place rather than wherever the add happened to leave it.
await evaluate(`openTerm(termTabs[0].id)`);
await sleep(500);
check('remote marked as remote',
  await evaluate("return termTabs.some(t=>/^ssh/.test(t.shell))"));

log('\n=== ISOLATION (the reported bug) ===');
const ids = (await evaluate('return termTabs.map(t=>t.id)')).slice(0, 2);
/* Panes are created lazily when a tab is first opened, so assert on tabs that
   genuinely have one rather than indexing into the list and hoping. */
for (const id of ids) {
  await evaluate(`openTerm(${JSON.stringify(id)})`);
  await sleep(500);
}
check('two panes exist', (await evaluate('return panes.size')) >= 2,
  'panes=' + await evaluate('return panes.size'));
check('exactly one pane shown',
  (await evaluate("return document.querySelectorAll('.xtpane.on').length")) === 1);

// Write a distinct marker into each buffer directly. This is the exact failure
// mode: bytes arriving for one session landing in another's buffer.
await evaluate(`
  panes.get(${JSON.stringify(ids[0])}).term.write('MARKER_ALPHA');
  panes.get(${JSON.stringify(ids[1])}).term.write('MARKER_BETA');
`);
await sleep(400);

const bufOf = id => `
  const p = panes.get(${JSON.stringify(id)});
  const b = p.term.buffer.active; let s = '';
  for (let i = 0; i < b.length; i++) s += (b.getLine(i)?.translateToString(true) || '') + '\\n';
  return s;
`;

let a = await evaluate(bufOf(ids[0]));
let b = await evaluate(bufOf(ids[1]));
check('alpha only in tab 1', a.includes('MARKER_ALPHA') && !a.includes('MARKER_BETA'));
check('beta only in tab 2', b.includes('MARKER_BETA') && !b.includes('MARKER_ALPHA'));

// Switch away and back. The old code replayed scrollback into the shared
// buffer here, which is where the two sessions got mixed.
await evaluate(`switchTab(${JSON.stringify(ids[0])})`);
await sleep(700);
await evaluate(`switchTab(${JSON.stringify(ids[1])})`);
await sleep(700);
await evaluate(`switchTab(${JSON.stringify(ids[0])})`);
await sleep(700);

a = await evaluate(bufOf(ids[0]));
b = await evaluate(bufOf(ids[1]));
check('tab 1 survives switching, uncontaminated',
  a.includes('MARKER_ALPHA') && !a.includes('MARKER_BETA'));
check('tab 2 survives switching, uncontaminated',
  b.includes('MARKER_BETA') && !b.includes('MARKER_ALPHA'));
check('still exactly one pane shown',
  (await evaluate("return document.querySelectorAll('.xtpane.on').length")) === 1);
check('active pane is tab 1',
  await evaluate(`return document.querySelector('.xtpane.on').dataset.id === ${JSON.stringify(ids[0])}`));

log('\n=== GEOMETRY ===');
const geo = await evaluate(`
  const p = document.querySelector('.xtpane.on');
  const r = p.getBoundingClientRect();
  return { w: Math.round(r.width), h: Math.round(r.height), cols: panes.get(termActive).term.cols };
`);
check('active pane has real size', geo.w > 200 && geo.h > 60, JSON.stringify(geo));
check('fitted to more than one column', geo.cols > 20, 'cols=' + geo.cols);

/* The pop-out cycle. This is where it was worst: the in-page window's onClose
   set `style.display`, which was never what hid the strip, so closing a popped
   out terminal left it in the DOM but collapsed to zero height - it looked like
   the terminal had disappeared. */
log('\n=== POP OUT ===');
const beforePop = await evaluate(`
  const r = document.querySelector('#term').getBoundingClientRect();
  return { h: Math.round(r.height), panes: panes.size };
`);
await evaluate("document.querySelector('#termpop').click()");
await sleep(1200);

const popped = await evaluate(`
  const w = document.querySelector('.win');
  const strip = document.querySelector('#term');
  const pane = document.querySelector('.xtpane.on');
  return {
    window: !!w,
    stripInsideWindow: !!(w && w.contains(strip)),
    paneH: pane ? Math.round(pane.getBoundingClientRect().height) : 0,
    railLit: document.querySelector('[data-toggle="term"]').classList.contains('on'),
    subtitle: w ? w.querySelector('.win-sub').textContent : '',
  };
`);
check('window opened', popped.window);
check('strip moved into it', popped.stripInsideWindow);
check('pane has height inside the window', popped.paneH > 60, 'h=' + popped.paneH);
check('rail button not lit while popped out', popped.railLit === false);
check('subtitle names the tab', !!popped.subtitle, JSON.stringify(popped.subtitle));

// Pressing pop-out again must put it back, not open a second window.
await evaluate("document.querySelector('#termpop').click()");
await sleep(1200);
const restored = await evaluate(`
  const strip = document.querySelector('#term');
  const r = strip.getBoundingClientRect();
  const pane = document.querySelector('.xtpane.on');
  return {
    windows: document.querySelectorAll('.win').length,
    backInPage: !strip.closest('.win'),
    stripH: Math.round(r.height),
    paneH: pane ? Math.round(pane.getBoundingClientRect().height) : 0,
    noTerm: app.classList.contains('no-term'),
    railLit: document.querySelector('[data-toggle="term"]').classList.contains('on'),
    panes: panes.size,
  };
`);
check('window closed', restored.windows === 0);
check('strip back in the page', restored.backInPage);
check('strip has height again', restored.stripH > 60, 'h=' + restored.stripH);
check('pane has height again', restored.paneH > 60, 'h=' + restored.paneH);
check('no-term cleared', restored.noTerm === false);
check('rail button lit again', restored.railLit === true);
check('no panes lost in the round trip',
  restored.panes === beforePop.panes, restored.panes + ' vs ' + beforePop.panes);

log('shot: ' + await shot('terminal-restored'));

log('\n=== CLOSE ===');
await evaluate(`closeTab(${JSON.stringify(ids[1])})`);
await sleep(700);
check('tab removed', (await evaluate('return termTabs.length')) === 1);
check('pane disposed with it', (await evaluate('return panes.size')) === 1,
  'panes=' + await evaluate('return panes.size'));

log('\nshot: ' + await shot('terminal'));

log('\n=== CONSOLE ===');
if (consoleErrors.length) { consoleErrors.forEach(e => log('  ' + e)); fails.push('console errors'); }
else log('  none');

log('\n' + (fails.length ? 'FAILED: ' + fails.join(', ') : 'all terminal checks passed'));
try { chrome.kill(); } catch (_) {}
process.exit(fails.length ? 1 : 0);
