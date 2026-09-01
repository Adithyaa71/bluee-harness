// What actually breaks when the window gets narrow.
//
// The app is a desktop window, so phone breakpoints are beside the point - but
// the window is resizable, and nothing in the stylesheet has ever said what
// should happen when it shrinks. This measures rather than guesses: it walks a
// set of widths and reports horizontal overflow, elements pushed off screen,
// and the point at which the main column stops being usable.
//
//   node dev/uinarrow.mjs <port> [label]

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const LABEL = process.argv[3] || 'narrow';
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP_PORT = 9335;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-uinarrow-profile')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,900',
  `http://127.0.0.1:${PORT}/`,
], { stdio: 'ignore' });

const sleep = ms => new Promise(r => setTimeout(r, ms));
async function target() {
  for (let i = 0; i < 60; i++) {
    try {
      const list = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`).then(r => r.json());
      const pg = list.find(t => t.type === 'page' && t.url.includes(`:${PORT}`));
      if (pg) return pg;
    } catch (_) {}
    await sleep(300);
  }
  throw new Error('no page over CDP');
}
const t = await target();
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let id = 0; const waiting = new Map();
ws.addEventListener('message', ev => {
  const m = JSON.parse(ev.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
});
const send = (method, params = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(res => waiting.set(mid, res));
};
await send('Runtime.enable'); await send('Page.enable');
const evaluate = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  return r.result?.result?.value;
};
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const f = path.join(process.cwd(), 'dev', 'shots', `${LABEL}-${name}.png`);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  return path.basename(f);
}

await evaluate("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(3500);

const WIDTHS = [1440, 1280, 1100, 980, 860, 760, 640];
console.log(`=== NARROW-WINDOW BEHAVIOUR [${LABEL}] ===\n`);
console.log('width  bodyScroll  main   side  rail  composer  verdict');

for (const w of WIDTHS) {
  await send('Emulation.setDeviceMetricsOverride',
    { width: w, height: 860, deviceScaleFactor: 1, mobile: false });
  await sleep(450);
  const m = await evaluate(`
    const q = s => document.querySelector(s);
    const r = s => { const e = q(s); if (!e) return 0; const b = e.getBoundingClientRect();
                     return Math.round(b.width); };
    const over = document.documentElement.scrollWidth - document.documentElement.clientWidth;
    // anything painted past the right edge
    let clipped = 0;
    for (const e of document.querySelectorAll('#app *')) {
      const b = e.getBoundingClientRect();
      if (b.width > 0 && b.left < window.innerWidth && b.right > window.innerWidth + 1) clipped++;
    }
    return { over, clipped, main:r('#main'), side:r('#side'), rail:r('#rail'),
             comp:r('.cwrap'), topbarOver: (()=>{ const t=q('#topbar'); return t ? t.scrollWidth - t.clientWidth : 0; })() };
  `);
  const verdict = m.main < 380 ? 'MAIN UNUSABLE'
    : m.over > 0 ? 'BODY SCROLLS'
    : m.clipped > 0 ? `${m.clipped} clipped`
    : m.topbarOver > 0 ? 'topbar overflows'
    : 'ok';
  console.log(
    String(w).padStart(5),
    String(m.over).padStart(10),
    String(m.main).padStart(6),
    String(m.side).padStart(6),
    String(m.rail).padStart(5),
    String(m.comp).padStart(9),
    '  ' + verdict);
}

await send('Emulation.setDeviceMetricsOverride',
  { width: 860, height: 860, deviceScaleFactor: 1, mobile: false });
await sleep(400);
console.log('\nshot 860:', await shot('860'));
await send('Emulation.setDeviceMetricsOverride',
  { width: 640, height: 860, deviceScaleFactor: 1, mobile: false });
await sleep(400);
console.log('shot 640:', await shot('640'));

ws.close(); chrome.kill(); process.exit(0);
