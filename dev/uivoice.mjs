// The Voice settings page, and the composer's mic, in a real browser.
//
// The parts worth proving here are the ones a stub cannot see: that the engine
// sections actually swap, that "Speak it" gets real audio back from the local
// worker, and that the mic button reflects whether voice is switched on rather
// than failing on press.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP = 9341;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-voice-check')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,980',
  `http://127.0.0.1:${PORT}/`,
], { stdio: 'ignore' });

const sleep = ms => new Promise(r => setTimeout(r, ms));
async function target() {
  for (let i = 0; i < 60; i++) {
    try {
      const l = await fetch(`http://127.0.0.1:${CDP}/json/list`).then(r => r.json());
      const p = l.find(t => t.type === 'page' && t.url.includes(`:${PORT}`));
      if (p) return p;
    } catch (_) {}
    await sleep(300);
  }
  throw new Error('no page');
}
const t = await target();
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let id = 0; const waiting = new Map(); const errs = [];
ws.addEventListener('message', e => {
  const m = JSON.parse(e.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    errs.push('EXCEPTION: ' + (m.params.exceptionDetails?.exception?.description || ''));
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    errs.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});
const send = (method, params = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(r => waiting.set(mid, r));
};
await send('Runtime.enable'); await send('Page.enable');
const ev = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return 'THREW: ' + r.result.exceptionDetails.exception?.description;
  return r.result?.result?.value;
};
const log = (...a) => console.log(...a);

await ev("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(3800);

log('=== VOICE SETTINGS PAGE ===');
log('tab exists :', await ev(`
  document.querySelector('.rb[data-page="set"]').click();
  await new Promise(r=>setTimeout(r,900));
  const b=[...document.querySelectorAll('#settabs button')].find(x=>x.dataset.st==='voice');
  if(!b) return 'NO VOICE TAB';
  b.click();
  await new Promise(r=>setTimeout(r,5000));
  return [...document.querySelectorAll('#settabs button')].map(x=>x.textContent).join(' / ');
`));
log('status strip:', await ev(
  "return (document.querySelector('#vstatus')||{}).textContent?.trim().replace(/\\s+/g,' ').slice(0,170)"));
log('controls   :', await ev(
  "return document.querySelectorAll('#setbody .fin').length+' fields, '+" +
  "document.querySelectorAll('#setbody input[type=checkbox]').length+' toggles'"));
log('engine grp :', await ev(`
  const all=[...document.querySelectorAll('#setbody .vgroup')].map(g=>g.dataset.eng);
  const on=[...document.querySelectorAll('#setbody .vgroup.on')].map(g=>g.dataset.eng);
  return 'showing ['+on.join(',')+'] of ['+all.join(',')+']';
`));
log('swap engine:', await ev(`
  const sel=document.querySelector('#setbody .v-t-eng');
  sel.value='kokoro'; sel.dispatchEvent(new Event('change',{bubbles:true}));
  await new Promise(r=>setTimeout(r,350));
  const on=[...document.querySelectorAll('#setbody .vgroup.on')].map(g=>g.dataset.eng);
  sel.value='piper'; sel.dispatchEvent(new Event('change',{bubbles:true}));
  await new Promise(r=>setTimeout(r,250));
  const back=[...document.querySelectorAll('#setbody .vgroup.on')].map(g=>g.dataset.eng);
  return 'kokoro -> ['+on.join(',')+']   back to piper -> ['+back.join(',')+']';
`));

log('\n=== SPEAK IT (real synthesis through the local worker) ===');
log('result     :', await ev(`
  document.querySelector('#v-speak').click();
  for(let i=0;i<40;i++){
    await new Promise(r=>setTimeout(r,500));
    const t=document.querySelector('#v-tts-msg').textContent;
    if(t && t!=='synthesising…') return t;
  }
  return 'still synthesising after 20s';
`));

log('\n=== MIC BUTTON ===');
log('state      :', await ev(`
  document.querySelector('.rb[data-page="chat"]').click();
  await new Promise(r=>setTimeout(r,1400));
  const b=document.querySelector('#mic');
  return 'disabled='+b.disabled+'   title="'+b.title+'"';
`));

const shot = await send('Page.captureScreenshot', { format: 'png' });
fs.mkdirSync('dev/shots', { recursive: true });
fs.writeFileSync('dev/shots/voice-mic.png', Buffer.from(shot.result.data, 'base64'));

// Back to the settings page for the screenshot that matters.
await ev(`
  document.querySelector('.rb[data-page="set"]').click();
  await new Promise(r=>setTimeout(r,700));
  [...document.querySelectorAll('#settabs button')].find(x=>x.dataset.st==='voice').click();
`);
await sleep(4000);
const shot2 = await send('Page.captureScreenshot', { format: 'png' });
fs.writeFileSync('dev/shots/voice-settings.png', Buffer.from(shot2.result.data, 'base64'));
log('shots      : voice-settings.png, voice-mic.png');

log('\n=== CONSOLE ===');
log(errs.length ? errs.join('\n') : 'clean');
ws.close(); chrome.kill(); process.exit(errs.length ? 1 : 0);
