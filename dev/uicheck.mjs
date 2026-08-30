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

await sleep(2500);   // let boot fetches settle

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

log('\n=== GRAPH ===');
await evaluate("document.querySelector('.rb[data-page=\"graph\"]').click()");
await sleep(3000);
log('legend      :', (await evaluate("return document.querySelector('#glegend').textContent") || '').slice(0, 150));
log('canvas      :', await evaluate(`
  const c = document.querySelector('#gcv'); const r = c.getBoundingClientRect();
  return Math.round(r.width) + 'x' + Math.round(r.height);
`));
log('shot        :', await shot('graph'));

log('\nconsole errors :', consoleErrors.length ? consoleErrors.join(' | ') : 'none');

ws.close();
chrome.kill();
process.exit(consoleErrors.length ? 1 : 0);
