// Can the Log/Tasks panel actually be closed, by every route that claims to?
import { spawn } from 'node:child_process';
import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path';
const PORT = process.argv[2] || '7793', CDP = 9371;
const CHROME = ['C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].find(p => fs.existsSync(p));
const chrome = spawn(CHROME, ['--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-close')}`, '--no-first-run',
  '--window-size=1440,900', `http://127.0.0.1:${PORT}/`], { stdio: 'ignore' });
const sleep = ms => new Promise(r => setTimeout(r, ms));
let page;
for (let i = 0; i < 60; i++) {
  try { const l = await fetch(`http://127.0.0.1:${CDP}/json/list`).then(r => r.json());
    page = l.find(t => t.type === 'page' && t.url.includes(':' + PORT)); if (page) break; } catch (_) {}
  await sleep(300);
}
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let id = 0; const w = new Map(); const errs = [];
ws.addEventListener('message', e => { const m = JSON.parse(e.data);
  if (m.id && w.has(m.id)) { w.get(m.id)(m); w.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    errs.push('EXCEPTION ' + String(m.params.exceptionDetails?.exception?.description).split('\n')[0]);
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    errs.push('console.error ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});
const send = (m, p = {}) => { const i = ++id; ws.send(JSON.stringify({ id: i, method: m, params: p })); return new Promise(r => w.set(i, r)); };
await send('Runtime.enable'); await send('Page.enable');
await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
const ev = async x => { const r = await send('Runtime.evaluate',
  { expression: `(async()=>{ ${x} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return 'THREW: ' + (r.result.exceptionDetails.exception?.description || '').split('\n')[0];
  return r.result?.result?.value; };
for (let i = 0; i < 60; i++) { if (await ev("return typeof syncRight === 'function'")) break; await sleep(300); }
await sleep(1200);

const state = () => ev(`
  const q = s => document.querySelector(s);
  const box = s => { const e = q(s); if (!e) return 'MISSING';
    const r = e.getBoundingClientRect();
    return Math.round(r.width) + 'x' + Math.round(r.height); };
  return JSON.stringify({
    col: box('#rightcol'), side: box('#side'),
    sideVisible: (() => { const e = q('#side'); const r = e.getBoundingClientRect();
      return r.width > 4 && getComputedStyle(e).display !== 'none'; })(),
    appClass: q('#app').className,
    btnOn: q('[data-toggle="side"]').classList.contains('on'),
  });
`);

const fails = [];
const check = (n, ok, d = '') => { console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '   ' + d : ''}`); if (!ok) fails.push(n); };

console.log('start       :', await state());

console.log('\n=== TOPBAR TOGGLE (agents CLOSED) ===');
await ev("document.querySelector('[data-toggle=\"side\"]').click()");
await sleep(700);
let s = JSON.parse(await state());
console.log('  ', JSON.stringify(s));
check('closes with sub-agents shut', !s.sideVisible && s.col.startsWith('0x'), s.col);
await ev("document.querySelector('[data-toggle=\"side\"]').click()");
await sleep(700);
check('reopens', JSON.parse(await state()).sideVisible);

console.log('\n=== HEADER X (agents CLOSED) ===');
await ev("document.querySelector('#sideclose').click()");
await sleep(700);
s = JSON.parse(await state());
console.log('  ', JSON.stringify(s));
check('header X closes it', !s.sideVisible, s.col);
await ev("document.querySelector('[data-toggle=\"side\"]').click()");
await sleep(700);

console.log('\n=== WITH SUB-AGENTS OPEN ===');
await ev("document.querySelector('.rb[data-toggle=\"agents\"]').click()");
await sleep(900);
await ev("document.querySelector('[data-toggle=\"side\"]').click()");
await sleep(700);
s = JSON.parse(await state());
console.log('  ', JSON.stringify(s));
check('closes while sub-agents stay', !s.sideVisible, 'side=' + s.side);

console.log('\n=== MEMORY POP-OUT (?only=memory) ===');
await send('Emulation.setDeviceMetricsOverride', { width: 860, height: 640, deviceScaleFactor: 1, mobile: false });
await ev("location.href='/?only=memory'");
await sleep(3000);
for (let i = 0; i < 40; i++) { if (await ev("return typeof syncRight === 'function'")) break; await sleep(300); }
await sleep(1200);
console.log(await ev(`
  const R = s => { const e = document.querySelector(s); if (!e) return s + ' MISSING';
    const r = e.getBoundingClientRect(), c = getComputedStyle(e);
    return s.padEnd(12) + Math.round(r.width) + 'x' + Math.round(r.height) +
      '  top=' + Math.round(r.top) + '  disp=' + c.display + '  r=' + c.borderTopLeftRadius; };
  return [R('#app'), R('#v-memory'), R('#memtop'), R('#memsplit'), R('#memgraph'),
          R('#mgholder'), R('#gcv')].join('\\n  ');
`));
console.log('  bodyScroll :', await ev("return document.body.scrollHeight - innerHeight"));
console.log('  overflow   :', await ev(`
  const bad = [];
  for (const el of document.querySelectorAll('#v-memory *')) {
    const r = el.getBoundingClientRect();
    if (r.height > 0 && r.bottom > innerHeight + 2 && el.children.length === 0)
      bad.push((el.className||el.tagName) + ' bottom=' + Math.round(r.bottom));
  }
  return bad.slice(0,6).join(' | ') || 'none clipped';
`));
const cap = await send('Page.captureScreenshot', { format: 'png' });
fs.writeFileSync('dev/shots/memory-solo.png', Buffer.from(cap.result.data, 'base64'));
console.log('  shot       : dev/shots/memory-solo.png');

console.log('\nconsole:', errs.length ? errs.join(' | ') : 'none');
console.log(fails.length ? '\nFAILED: ' + fails.join(', ') : '\nall close checks passed');
try { chrome.kill(); } catch (_) {}
process.exit(0);
