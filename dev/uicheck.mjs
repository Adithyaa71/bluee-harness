// Drive the real dashboard in a real browser, over the Chrome DevTools Protocol.
//
// Why this exists: every earlier UI check ran against a DOM *stub*. A stub
// proves the code executes; it cannot tell you that clicking a file does
// nothing, that a panel has zero height, or that an element is painted off
// screen. Those are exactly the bugs that kept reaching Adithya. This drives
// headless Chrome, so a click is a click and a screenshot is what the page
// actually looks like.
//
//   node dev/uicheck.mjs <port> [--head] [--shot name]
//
// Needs nothing installed: Chrome ships with Windows and CDP speaks plain
// WebSocket, which node has built in.

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

const CDP_PORT = 9333;
const profile = path.join(os.tmpdir(), 'bluee-uicheck-profile');

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
await send('DOM.enable');

/** Evaluate in the page and return the value. Throws page errors as errors. */
export async function evaluate(expr) {
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

const out = [];
const log = (...a) => { out.push(a.join(' ')); console.log(...a); };

// Start from a known state: the panel width and collapsed flag persist in
// localStorage, so without this a run inherits whatever the last run left and
// the numbers stop being comparable.
await evaluate("try{ localStorage.clear(); }catch(_){}; location.reload();");
await sleep(3000);   // let boot fetches settle

// ---------------------------------------------------------------- checks
log('=== PAGE ===');
log('title      :', await evaluate('return document.title'));
log('topbar     :', await evaluate("return document.querySelector('#tmeta')?.textContent"));

log('\n=== PLAYGROUND ===');
await evaluate("document.querySelector('.rb[data-page=\"play\"]').click()");
await sleep(1800);

log('page visible:', await evaluate("return document.querySelector('#v-play').classList.contains('on')"));
log('files panel :', await evaluate(`
  const p = document.querySelector('#pgfiles');
  const r = p.getBoundingClientRect();
  return Math.round(r.width) + 'x' + Math.round(r.height);
`));
log('tree rows   :', await evaluate("return document.querySelectorAll('#pgtree .fnode').length"));
log('row labels  :', await evaluate(
  "return [...document.querySelectorAll('#pgtree .fnode .nm')].map(e=>e.textContent).join(', ')"));

// The actual complaint: click a file.
const clicked = await evaluate(`
  const rows = [...document.querySelectorAll('#pgtree .fnode')];
  const f = rows.find(r => r.dataset.d === '0' && r.querySelector('.nm').textContent.endsWith('.html'));
  if (!f) return 'no html file row found';
  f.click();
  return 'clicked ' + f.dataset.p;
`);
log('click       :', clicked);
await sleep(1200);

log('after click :', await evaluate(`
  const v = document.querySelector('#pgview');
  const r = v.getBoundingClientRect();
  return 'pgview.on=' + v.classList.contains('on') +
         ' size=' + Math.round(r.width) + 'x' + Math.round(r.height) +
         ' name=' + JSON.stringify(document.querySelector('#pgname').textContent) +
         ' playbody.display=' + document.querySelector('#playbody').style.display;
`));
log('preview     :', await evaluate(`
  const fr = document.querySelector('#pgframe'), co = document.querySelector('#pgcode');
  const fb = fr.getBoundingClientRect(), cb = co.getBoundingClientRect();
  return 'iframe ' + Math.round(fb.width) + 'x' + Math.round(fb.height) + ' src=' + (fr.getAttribute('src')||'-') +
         ' | code ' + Math.round(cb.width) + 'x' + Math.round(cb.height);
`));
log('shot        :', await shot('playground'));
log('browser btn :', await evaluate(`
  const b = document.querySelector('#pgbrowse');
  b.click();
  await new Promise(r => setTimeout(r, 2200));
  const p = document.querySelector('#pgweb');
  const view = document.querySelector('#webview').textContent.trim().slice(0, 90);
  return 'panel open=' + p.classList.contains('on') +
    ' width=' + Math.round(p.getBoundingClientRect().width) +
    ' | ' + view;
`));
log('browser off :', await evaluate(`
  document.querySelector('#webclose').click();
  await new Promise(r => setTimeout(r, 300));
  return 'closed=' + !document.querySelector('#pgweb').classList.contains('on') +
    ' files still there=' + (document.querySelector('#pgfiles').getBoundingClientRect().width > 0);
`));
log('rename      :', await evaluate(`
  const before = document.querySelector('#pgroot').value;
  document.querySelector('#pgrename').click();
  await new Promise(r => setTimeout(r, 250));
  const msg = document.querySelector('#pgrootmsg').textContent;
  return 'on playground -> ' + JSON.stringify(msg);
`));

// The panel controls, actually exercised.
log('collapse    :', await evaluate(`
  document.querySelector('#pgfhide').click();
  await new Promise(r=>setTimeout(r,250));
  const p=document.querySelector('#pgfiles').getBoundingClientRect();
  const show=document.querySelector('#pgshow');
  return 'panel width=' + Math.round(p.width) +
         ' reopen-button visible=' + (show.offsetParent !== null);
`));
log('reopen      :', await evaluate(`
  document.querySelector('#pgshow').click();
  await new Promise(r=>setTimeout(r,250));
  return 'panel width=' + Math.round(document.querySelector('#pgfiles').getBoundingClientRect().width);
`));
log('resize      :', await evaluate(`
  const bar=document.querySelector('#pgdrag'), panel=document.querySelector('#pgfiles');
  const r=bar.getBoundingClientRect();
  const at=(t,x)=>bar.dispatchEvent(new MouseEvent(t,{clientX:x,clientY:r.top+20,bubbles:true}));
  const w0=panel.getBoundingClientRect().width;
  bar.dispatchEvent(new MouseEvent('mousedown',{clientX:r.left+2,clientY:r.top+20,bubbles:true,cancelable:true}));
  window.dispatchEvent(new MouseEvent('mousemove',{clientX:r.left+122,clientY:r.top+20,bubbles:true}));
  window.dispatchEvent(new MouseEvent('mouseup',{bubbles:true}));
  await new Promise(r=>setTimeout(r,200));
  return Math.round(w0) + 'px -> ' + Math.round(panel.getBoundingClientRect().width) + 'px';
`));
log('folder click:', await evaluate(`
  document.querySelector('#pgback').click();
  await new Promise(r=>setTimeout(r,200));
  const dir=[...document.querySelectorAll('#pgtree .fnode')].find(r=>r.dataset.d==='1');
  if(!dir) return 'no folder row';
  dir.click();
  await new Promise(r=>setTimeout(r,600));
  return 'clicked folder ' + dir.dataset.p + ' -> pgview.on=' +
    document.querySelector('#pgview').classList.contains('on') +
    ' showing=' + JSON.stringify(document.querySelector('#pgname').textContent);
`));
log('source view :', await evaluate(`
  document.querySelector('#pgsrc').click();
  await new Promise(r=>setTimeout(r,600));
  const co=document.querySelector('#pgcode');
  return 'code chars=' + co.textContent.length + ' visible=' + (co.offsetParent !== null);
`));

// Grant a real folder and work inside it - the "open a directory" flow.
log('\n=== GRANTED FOLDERS ===');
log('picker      :', await evaluate(
  "return [...document.querySelectorAll('#pgroot option')].map(o=>o.value).join(', ')"));

// A disposable folder, created here and revoked at the end.
//
// This used to default to `<cwd>\persona` - a real project folder holding
// SOUL.md, which §5c calls the highest-value target in the system - and it
// never revoked it. So every run of this check permanently granted the model
// read/write/delete access to the persona files, as a side effect of testing
// the file tree. A check should not change what the assistant is allowed to
// touch. GRANT_PATH still overrides, for pointing it at a real project.
const grantPath = process.env.GRANT_PATH || (() => {
  const d = path.join(os.tmpdir(), 'bluee-uicheck-grant');
  fs.mkdirSync(path.join(d, 'nested'), { recursive: true });
  fs.writeFileSync(path.join(d, 'README.md'), '# sample\n\nA throwaway file for the UI check.\n');
  fs.writeFileSync(path.join(d, 'sample.json'), '{ "hello": "world" }\n');
  fs.writeFileSync(path.join(d, 'nested', 'inner.txt'), 'nested file\n');
  return d;
})();
const grantIsTemp = !process.env.GRANT_PATH;
log('granting    :', grantPath + (grantIsTemp ? '  (temp, revoked at the end)' : ''));
log('grant       :', await evaluate(`
  document.querySelector('#pgroadd').click();
  const inp = document.querySelector('#pgrootpath');
  inp.value = ${JSON.stringify(grantPath)};
  document.querySelector('#pgrootok').click();
  await new Promise(r => setTimeout(r, 1800));
  return document.querySelector('#pgrootmsg').textContent;
`));
log('now showing :', await evaluate(`
  return document.querySelector('#pgroot').value + ' | ' +
    document.querySelectorAll('#pgtree .fnode').length + ' rows: ' +
    [...document.querySelectorAll('#pgtree .fnode .nm')].map(e => e.textContent).join(', ');
`));
log('open a file :', await evaluate(`
  const f = [...document.querySelectorAll('#pgtree .fnode')].find(r => r.dataset.d === '0');
  if (!f) return 'no file row';
  f.click();
  await new Promise(r => setTimeout(r, 1000));
  return f.dataset.p + ' -> ' + document.querySelector('#pgcode').textContent.length +
    ' chars, visible=' + (document.querySelector('#pgcode').offsetParent !== null);
`));
log('code gutter :', await evaluate(`
  const j = [...document.querySelectorAll('#pgtree .fnode')]
    .find(r => r.querySelector('.nm').textContent.endsWith('.json'));
  if (!j) return 'no json file';
  j.click();
  await new Promise(r => setTimeout(r, 900));
  const w = document.querySelector('#pgcodewrap');
  const g = document.querySelector('#pggutter');
  return 'soft=' + w.classList.contains('soft') +
         ' gutter lines=' + (g.textContent ? g.textContent.split('\\n').length : 0) +
         ' gutter visible=' + (g.offsetParent !== null);
`));
log('wrap toggle :', await evaluate(`
  document.querySelector('#pgwrapbtn').click();
  await new Promise(r => setTimeout(r, 200));
  const w = document.querySelector('#pgcodewrap');
  const on = document.querySelector('#pgwrapbtn').classList.contains('on');
  return 'soft=' + w.classList.contains('soft') + ' button-lit=' + on +
         ' gutter hidden=' + (document.querySelector('#pggutter').offsetParent === null);
`));
log('terminal here:', await evaluate(`
  document.querySelector('#pgterm').click();
  await new Promise(r => setTimeout(r, 1600));
  const t = document.querySelector('#term');
  const r = t.getBoundingClientRect();
  return 'terminal ' + Math.round(r.width) + 'x' + Math.round(r.height) +
         ' shown=' + (r.height > 40);
`));
log('shot        :', await shot('granted-folder'));
log('escape test :', await evaluate(`
  const r = await fetch('/api/file?root=' + document.querySelector('#pgroot').value +
    '&path=' + encodeURIComponent('../.env')).then(x => x.json());
  return r.error || ('LEAKED ' + (r.text || '').slice(0, 40));
`));
log('revoke      :', await evaluate(`
  const b = document.querySelector('#pgrootrm');
  if (!document.querySelector('#pgrootadd').classList.contains('on'))
    document.querySelector('#pgroadd').click();
  b.click(); await new Promise(r => setTimeout(r, 250));
  b.click(); await new Promise(r => setTimeout(r, 1400));
  return document.querySelector('#pgrootmsg').textContent + ' | now on ' +
    document.querySelector('#pgroot').value;
`));

log('\n=== GRAPH ===');
await evaluate("document.querySelector('.rb[data-page=\"graph\"]').click()");
await sleep(3000);
log('legend      :', (await evaluate("return document.querySelector('#glegend').textContent") || '').slice(0, 150));
// Close the terminal opened earlier so the graph gets the full height.
await evaluate(`
  if (!document.querySelector('#app').classList.contains('no-term'))
    document.querySelector('.rb[data-toggle="term"]').click();
`);
await sleep(600);
log('canvas      :', await evaluate(`
  const c = document.querySelector('#gcv'); const r = c.getBoundingClientRect();
  return Math.round(r.width) + 'x' + Math.round(r.height);
`));
log('zoom in x3  :', await evaluate(`
  const c = document.querySelector('#gcv');
  const r = c.getBoundingClientRect();
  for (let i = 0; i < 3; i++)
    c.dispatchEvent(new WheelEvent('wheel', {
      deltaY: -240, clientX: r.left + r.width/2, clientY: r.top + r.height/2,
      bubbles: true, cancelable: true }));
  await new Promise(r => setTimeout(r, 200));
  return 'k=' + window.__k();
`));
log('pan by drag :', await evaluate(`
  const c = document.querySelector('#gcv');
  const before = window.__view();
  c.dispatchEvent(new MouseEvent('mousedown', { clientX: 400, clientY: 300, bubbles: true }));
  c.dispatchEvent(new MouseEvent('mousemove', { clientX: 520, clientY: 360, bubbles: true }));
  window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  await new Promise(r => setTimeout(r, 150));
  const after = window.__view();
  return 'dx=' + Math.round(after.x - before.x) + ' dy=' + Math.round(after.y - before.y);
`));
log('hover focus :', await evaluate(`
  const c = document.querySelector('#gcv');
  const rect = c.getBoundingClientRect();
  const v = window.__view();
  // Pick a node that is actually on screen after the zoom and pan above, and
  // convert canvas coords to client coords - the canvas is not at 0,0.
  const n = window.__nodes().find(m => {
    const x = m.x * v.k + v.x, y = m.y * v.k + v.y;
    return x > 20 && x < rect.width - 20 && y > 20 && y < rect.height - 20;
  });
  if (!n) return 'no node on screen to hover';
  c.dispatchEvent(new MouseEvent('mousemove', {
    clientX: rect.left + n.x * v.k + v.x,
    clientY: rect.top + n.y * v.k + v.y, bubbles: true }));
  await new Promise(r => setTimeout(r, 150));
  return 'hovering ' + JSON.stringify(window.__hover() && window.__hover().name);
`));
log('filter chip :', await evaluate(`
  const chip = [...document.querySelectorAll('#glegend .gk')].find(c => c.dataset.k === 'file');
  if (!chip) return 'no file chip';
  chip.click();
  await new Promise(r => setTimeout(r, 2500));
  return 'files on -> ' + window.__nodes().length + ' nodes, k=' + window.__k();
`));
log('hide all    :', await evaluate(`
  for (const chip of [...document.querySelectorAll('#glegend .gk')]) {
    if (!chip.classList.contains('off')) { chip.click(); await new Promise(r=>setTimeout(r,400)); }
  }
  await new Promise(r => setTimeout(r, 800));
  return document.querySelector('#glegend').textContent.slice(0, 60);
`));
log('restore     :', await evaluate(`
  // Back to the default view, hovering a node, for the screenshot.
  for (const chip of [...document.querySelectorAll('#glegend .gk')]) {
    const k = chip.dataset.k;
    const shouldBeOn = k !== 'symbol' && k !== 'file';
    if (shouldBeOn === chip.classList.contains('off')) {
      chip.click(); await new Promise(r => setTimeout(r, 400));
    }
  }
  await new Promise(r => setTimeout(r, 1200));
  document.querySelector('#gcv').dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
  await new Promise(r => setTimeout(r, 200));
  const c = document.querySelector('#gcv'), rect = c.getBoundingClientRect();
  const v = window.__view();
  const hub = window.__nodes().find(n => n.name === 'Adithya') || window.__nodes()[0];
  c.dispatchEvent(new MouseEvent('mousemove', {
    clientX: rect.left + hub.x * v.k + v.x,
    clientY: rect.top + hub.y * v.k + v.y, bubbles: true }));
  await new Promise(r => setTimeout(r, 250));
  return window.__nodes().length + ' nodes, hovering ' +
    JSON.stringify(window.__hover() && window.__hover().name);
`));
log('search       :', await evaluate(`
  const s = document.querySelector('#gsearch');
  s.value = 'snarevec';
  s.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise(r => setTimeout(r, 300));
  return document.querySelector('#ghits').textContent +
    ' | ringed: ' + (window.__match() || []).length;
`));
log('hidden hint  :', await evaluate(`
  const s = document.querySelector('#gsearch');
  s.value = 'read_stream';
  s.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise(r => setTimeout(r, 300));
  return document.querySelector('#ghits').textContent;
`));
log('enter centres:', await evaluate(`
  const s = document.querySelector('#gsearch');
  s.value = 'SnareVec';
  s.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise(r => setTimeout(r, 200));
  s.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
  await new Promise(r => setTimeout(r, 300));
  const c = document.querySelector('#gcv');
  const v = window.__view(), p = window.__pin();
  if (!p) return 'nothing pinned';
  const sx = p.x * v.k + v.x, sy = p.y * v.k + v.y;
  return 'pinned ' + JSON.stringify(p.name) + ' at ' + Math.round(sx) + ',' + Math.round(sy) +
    ' (canvas centre ' + Math.round(c.clientWidth/2) + ',' + Math.round(c.clientHeight/2) + ')';
`));
log('click unpins :', await evaluate(`
  const c = document.querySelector('#gcv'), rect = c.getBoundingClientRect();
  // Move to empty space FIRST, with no button down - that is the order a real
  // pointer produces, and it is what clears the hover before the click.
  c.dispatchEvent(new MouseEvent('mousemove', { clientX: rect.left+12, clientY: rect.top+12, bubbles: true }));
  await new Promise(r => setTimeout(r, 120));
  const hoverNow = window.__hover();
  c.dispatchEvent(new MouseEvent('mousedown', { clientX: rect.left+12, clientY: rect.top+12, bubbles: true }));
  window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  c.dispatchEvent(new MouseEvent('click', { clientX: rect.left+12, clientY: rect.top+12, bubbles: true }));
  await new Promise(r => setTimeout(r, 200));
  return 'hover cleared=' + (hoverNow === null) +
    ' pin now ' + JSON.stringify(window.__pin() && window.__pin().name);
`));
log('drag not pin :', await evaluate(`
  const c = document.querySelector('#gcv'), rect = c.getBoundingClientRect();
  // Start from a known state so the result cannot be inherited from the last
  // step - a test that passes because of leftover state is not a test.
  window.__clearPin();
  const pinBefore = window.__pin();
  const v = window.__view();
  const n = window.__nodes().find(m => {
    const x = m.x*v.k+v.x, y = m.y*v.k+v.y;
    return x > 60 && x < rect.width-60 && y > 60 && y < rect.height-60;
  });
  const px = rect.left + n.x*v.k+v.x, py = rect.top + n.y*v.k+v.y;
  c.dispatchEvent(new MouseEvent('mousemove', { clientX: px, clientY: py, bubbles: true }));
  c.dispatchEvent(new MouseEvent('mousedown', { clientX: px, clientY: py, bubbles: true }));
  c.dispatchEvent(new MouseEvent('mousemove', { clientX: px+80, clientY: py+40, bubbles: true }));
  window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  c.dispatchEvent(new MouseEvent('click', { clientX: px+80, clientY: py+40, bubbles: true }));
  await new Promise(r => setTimeout(r, 200));
  return 'started null=' + (pinBefore === null) +
    ', after dragging across a node pin is ' +
    JSON.stringify(window.__pin() && window.__pin().name) +
    ' (must stay null - a pan is not a click)';
`));
log('shot        :', await shot('graph'));
log('\n=== MEMORY + GRAPH ===');
await evaluate("document.querySelector('.rb[data-page=\"memory\"]').click()");
await sleep(3500);
log('layout      :', await evaluate(`
  const top = document.querySelector('#memtop').getBoundingClientRect();
  const g = document.querySelector('#memgraph').getBoundingClientRect();
  const cv = document.querySelector('#gcv');
  return 'memory ' + Math.round(top.width) + 'x' + Math.round(top.height) +
         ' | graph pane ' + Math.round(g.width) + 'x' + Math.round(g.height) +
         ' | canvas inside graph pane=' + (cv.closest('#memgraph') !== null);
`));
log('graph drawn :', await evaluate(`
  return document.querySelector('#mgstat').textContent + ' | nodes=' + window.__nodes().length;
`));
log('split drag  :', await evaluate(`
  const bar = document.querySelector('#memsplit'), pane = document.querySelector('#memgraph');
  const r = bar.getBoundingClientRect();
  const h0 = pane.getBoundingClientRect().height;
  bar.dispatchEvent(new MouseEvent('mousedown', { clientY: r.top, clientX: r.left+100, bubbles: true, cancelable: true }));
  window.dispatchEvent(new MouseEvent('mousemove', { clientY: r.top - 90, clientX: r.left+100, bubbles: true }));
  window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  await new Promise(x => setTimeout(x, 250));
  return Math.round(h0) + 'px -> ' + Math.round(pane.getBoundingClientRect().height) + 'px';
`));
log('graph popout:', await evaluate(`
  document.querySelector('#mgpop').click();
  await new Promise(x => setTimeout(x, 700));
  const w = document.querySelector('.win');
  const cv = document.querySelector('#gcv');
  return w ? ('window ' + Math.round(w.getBoundingClientRect().width) + 'x' +
    Math.round(w.getBoundingClientRect().height) +
    ' canvas inside=' + (cv.closest('.win') !== null)) : 'no window opened';
`));
log('popout close:', await evaluate(`
  document.querySelector('.win .close').click();
  await new Promise(x => setTimeout(x, 700));
  const cv = document.querySelector('#gcv');
  return 'window gone=' + (document.querySelector('.win') === null) +
    ' canvas back in memory pane=' + (cv.closest('#memgraph') !== null);
`));
log('back to graph page:', await evaluate(`
  document.querySelector('.rb[data-page="graph"]').click();
  await new Promise(x => setTimeout(x, 1800));
  const cv = document.querySelector('#gcv');
  return 'canvas on graph page=' + (cv.closest('#v-graph') !== null) +
    ' size=' + Math.round(cv.getBoundingClientRect().width) + 'x' +
    Math.round(cv.getBoundingClientRect().height);
`));
log('shot        :', await shot('memory-graph'));
log('\n=== TERMINAL TABS ===');
await evaluate(`
  document.querySelector('.rb[data-page="chat"]').click();
  if (document.querySelector('#app').classList.contains('no-term'))
    document.querySelector('.rb[data-toggle="term"]').click();
`);
await sleep(1500);
log('tabs        :', await evaluate(`
  const t = [...document.querySelectorAll('#termtabs .ttab')];
  return t.length + ' tab(s): ' + t.map(x => x.querySelector('.nm').textContent).join(', ') +
    ' | add button=' + (document.querySelector('#ttadd') !== null);
`));
log('add a tab   :', await evaluate(`
  document.querySelector('#ttadd').click();
  await new Promise(r => setTimeout(r, 1200));
  const t = [...document.querySelectorAll('#termtabs .ttab')];
  return t.length + ' tabs, active=' +
    (t.find(x => x.classList.contains('on'))||{}).dataset?.id;
`));
log('switch back :', await evaluate(`
  const tabs = [...document.querySelectorAll('#termtabs .ttab')];
  const targetId = tabs[0].dataset.id;
  tabs[0].click();
  await new Promise(r => setTimeout(r, 1400));
  // Re-query: drawTabs rebuilds the strip, so the node captured before the
  // click is detached and asserting on it measures nothing.
  const now = [...document.querySelectorAll('#termtabs .ttab')]
    .find(n => n.classList.contains('on'));
  return 'active=' + (now && now.dataset.id) + ' expected=' + targetId +
    ' -> ' + (now && now.dataset.id === targetId ? 'OK' : 'WRONG');
`));
log('persisted   :', await evaluate(`
  const k = 'bluee.terms.' + (window.fileRoot || 'playground');
  const raw = localStorage.getItem(k);
  return raw ? JSON.parse(raw).length + ' session(s) remembered under ' + k : 'nothing saved';
`));
log('close a tab :', await evaluate(`
  const tabs = [...document.querySelectorAll('#termtabs .ttab')];
  const x = tabs[1] && tabs[1].querySelector('.x');
  if (!x) return 'no close button';
  x.click();
  await new Promise(r => setTimeout(r, 1000));
  return document.querySelectorAll('#termtabs .ttab').length + ' tab(s) left';
`));

log('\n=== SMOOTHNESS ===');
log('drag frames :', await evaluate(`
  // Open an in-page window and measure what a drag actually costs per frame.
  openWindow({ id: 'perftest', title: 'perf', html: '<div style="padding:20px">x</div>',
               w: 600, h: 400 });
  await new Promise(r => setTimeout(r, 300));
  const head = document.querySelector('.win .win-head');
  const r = head.getBoundingClientRect();
  head.dispatchEvent(new MouseEvent('mousedown', { clientX: r.left+40, clientY: r.top+10, bubbles: true }));
  const times = [];
  let last = performance.now();
  for (let i = 0; i < 40; i++) {
    window.dispatchEvent(new MouseEvent('mousemove',
      { clientX: r.left+40+i*6, clientY: r.top+10+i*3, bubbles: true }));
    await new Promise(r2 => requestAnimationFrame(r2));
    const now = performance.now();
    times.push(now - last); last = now;
  }
  const inert = getComputedStyle(document.body).cursor;
  const usingTransform = document.querySelector('.win').style.transform !== '';
  window.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
  times.sort((a,b)=>a-b);
  const med = times[Math.floor(times.length/2)].toFixed(1);
  const worst = times[times.length-1].toFixed(1);
  return 'median ' + med + 'ms, worst ' + worst + 'ms | transform used=' + usingTransform +
    ' | iframes inert during drag=' + (inert === 'grabbing');
`));
log('after drag  :', await evaluate(`
  const w = document.querySelector('.win');
  const res = 'transform cleared=' + (w.style.transform === '') +
    ' left=' + w.style.left + ' top=' + w.style.top +
    ' body still dragging=' + document.body.classList.contains('dragging');
  document.querySelector('.win .close').click();
  return res;
`));

log('\n=== SOLO MODE ===');
for (const view of ['terminal', 'graph', 'tasks']) {
  await evaluate(`location.href = '/?only=' + ${JSON.stringify(view)};`);
  await sleep(3000);
  log('  ?only=' + view.padEnd(9), ':', await evaluate(`
    const app = document.querySelector('#app');
    const rail = document.querySelector('#rail');
    const vis = el => el && el.offsetParent !== null && el.getBoundingClientRect().height > 5;
    let main = 'none';
    if (${JSON.stringify(view)} === 'terminal') main = 'term ' + Math.round(document.querySelector('#term').getBoundingClientRect().height) + 'px';
    if (${JSON.stringify(view)} === 'graph') main = 'canvas ' + Math.round(document.querySelector('#gcv').getBoundingClientRect().height) + 'px';
    if (${JSON.stringify(view)} === 'tasks') main = 'panel ' + Math.round(document.querySelector('#side').getBoundingClientRect().width) + 'px';
    return 'solo=' + app.classList.contains('solo') +
           ' rail hidden=' + !vis(rail) +
           ' | ' + main + ' | title=' + JSON.stringify(document.title);
  `));
}
await evaluate("location.href = '/';");
await sleep(2500);
log('');
log('=== + MENU ===');
await evaluate("document.querySelector('.rb[data-page=\"chat\"]').click()");
await sleep(600);
log('opens       :', await evaluate(`
  document.querySelector('#plus').click();
  await new Promise(r => setTimeout(r, 300));
  const m = document.querySelector('#addmenu');
  const r = m.getBoundingClientRect();
  return 'open=' + m.classList.contains('on') +
    ' at ' + Math.round(r.left) + ',' + Math.round(r.top) +
    ' size ' + Math.round(r.width) + 'x' + Math.round(r.height) +
    ' | ' + [...m.querySelectorAll('.mi')].map(x => x.textContent.trim()).join(' / ');
`));
log('connectors  :', await evaluate(`
  [...document.querySelectorAll('#addmenu .mi')].find(x => x.dataset.a === 'connectors').click();
  await new Promise(r => setTimeout(r, 900));
  const m = document.querySelector('#addmenu');
  return [...m.querySelectorAll('.mi[data-s]')].map(x =>
    x.dataset.s + '=' + (x.querySelector('.sw').classList.contains('on') ? 'on' : 'off')).join(', ')
    || 'no servers listed';
`));
log('skills      :', await evaluate(`
  document.querySelector('#plus').click();
  await new Promise(r => setTimeout(r, 250));
  [...document.querySelectorAll('#addmenu .mi')].find(x => x.dataset.a === 'skills').click();
  await new Promise(r => setTimeout(r, 900));
  return [...document.querySelectorAll('#addmenu .mi[data-k]')].map(x => x.dataset.k).join(', ')
    || 'no skills listed';
`));
log('insert skill:', await evaluate(`
  const first = document.querySelector('#addmenu .mi[data-k]');
  if (!first) return 'nothing to click';
  first.click();
  await new Promise(r => setTimeout(r, 300));
  return 'composer now: ' + JSON.stringify(document.querySelector('#input').value) +
    ' | menu closed=' + !document.querySelector('#addmenu').classList.contains('on');
`));
log('escape      :', await evaluate(`
  document.querySelector('#plus').click();
  await new Promise(r => setTimeout(r, 200));
  window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await new Promise(r => setTimeout(r, 200));
  return 'closed=' + !document.querySelector('#addmenu').classList.contains('on');
`));
log('');
log('=== WORKSPACE CONNECTORS ===');
await evaluate("document.querySelector('.rb[data-page=\"play\"]').click()");
await sleep(1200);
log('tickboxes   :', await evaluate(`
  document.querySelector('#pgplus').click();
  await new Promise(r => setTimeout(r, 300));
  [...document.querySelectorAll('#addmenu .mi')].find(x => x.dataset.a === 'connectors').click();
  await new Promise(r => setTimeout(r, 1400));
  const m = document.querySelector('#addmenu');
  return m.querySelector('.hd').textContent + ' | ' +
    [...m.querySelectorAll('.mi[data-s]')].map(x =>
      x.dataset.s + '=' + (x.querySelector('.sw').classList.contains('on') ? 'on' : 'off')).join(', ') +
    ' | ' + (m.querySelector('.cost') || {}).textContent;
`));
log('untick one  :', await evaluate(`
  const row = [...document.querySelectorAll('#addmenu .mi[data-s]')].find(x => x.dataset.s === 'uacc');
  if (!row) return 'no uacc row';
  row.click();
  await new Promise(r => setTimeout(r, 1600));
  const m = document.querySelector('#addmenu');
  return [...m.querySelectorAll('.mi[data-s]')].map(x =>
    x.dataset.s + '=' + (x.querySelector('.sw').classList.contains('on') ? 'on' : 'off')).join(', ') +
    ' | ' + (m.querySelector('.cost') || {}).textContent;
`));
log('persisted   :', await evaluate(`
  const d = await fetch('/api/roots').then(r => r.json());
  const pg = d.roots.find(r => r.id === 'playground');
  return 'playground servers = ' + JSON.stringify(pg.servers);
`));
log('use all     :', await evaluate(`
  [...document.querySelectorAll('#addmenu .mi')].find(x => x.dataset.a === 'all').click();
  await new Promise(r => setTimeout(r, 1600));
  const d = await fetch('/api/roots').then(r => r.json());
  const pg = d.roots.find(r => r.id === 'playground');
  return 'back to ' + JSON.stringify(pg.servers) + ' | ' +
    (document.querySelector('#addmenu .cost') || {}).textContent;
`));

// Safety net. The granted-folders section above already revokes through the UI;
// this catches the case where that step fails or is skipped, because leaving the
// grant behind would mean the check quietly widens what bluee may touch every
// time it runs. "already gone" here is the expected result, not a problem.
if (grantIsTemp) {
  log('revoke grant:', await evaluate(`
    const before = await fetch('/api/roots').then(r => r.json());
    const mine = before.roots.find(r => r.path && r.path.includes('bluee-uicheck-grant'));
    if (!mine) return 'already gone (revoked by the UI flow above)';
    const res = await fetch('/api/roots/remove', { method:'POST',
      headers:{'Content-Type':'application/json'},
      body: JSON.stringify({ id: mine.id }) }).then(r=>r.json()).catch(e=>({error:String(e)}));
    const after = await fetch('/api/roots').then(r => r.json());
    return (res.error || 'revoked ' + mine.id) +
      ' | roots now: ' + after.roots.map(x=>x.id).join(', ');
  `));
}

log('\nconsole errors :', consoleErrors.length ? consoleErrors.join(' | ') : 'none');

ws.close();
chrome.kill();
process.exit(consoleErrors.length ? 1 : 0);
