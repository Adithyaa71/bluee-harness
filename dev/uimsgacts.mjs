// Per-message copy / edit / retry, against a real replayed conversation.
//
// Retry and edit both SEND, which costs an API call, so this stops short of
// firing them: it proves the control exists, opens the editor, checks the text
// it was seeded with, and cancels. What it does exercise fully is copy, which
// is the one with a real failure mode (clipboard permissions).

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

const CDP = 9340;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-msgacts')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,900',
  // Clipboard writes need permission in headless; grant it for the origin.
  `--unsafely-treat-insecure-origin-as-secure=http://127.0.0.1:${PORT}`,
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
});
const send = (method, params = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(r => waiting.set(mid, r));
};
await send('Runtime.enable');
/* Headless Chrome denies clipboard reads by default, so `readText()` came back
   with something that was not what had just been written - and the check
   reported MISMATCH against an app that copies correctly. Verified separately
   with permission granted: src and clipboard are byte-identical. */
await send('Browser.grantPermissions',
  { permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'] }); await send('Page.enable');
await send('Browser.grantPermissions', {
  origin: `http://127.0.0.1:${PORT}`,
  permissions: ['clipboardReadWrite', 'clipboardSanitizedWrite'],
}).catch(() => {});
const ev = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return 'THREW: ' + r.result.exceptionDetails.exception?.description;
  return r.result?.result?.value;
};
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const f = path.join(process.cwd(), 'dev', 'shots', `acts-${name}.png`);
  fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  return path.basename(f);
}
const log = (...a) => console.log(...a);

await ev("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(3800);

log('=== LOAD A REAL CONVERSATION ===');
log('open        :', await ev(`
  document.querySelector('.rb[data-page="sess"]').click();
  await new Promise(r=>setTimeout(r,1600));
  const c=document.querySelector('#sessbody .scard');
  if(!c) return 'no sessions';
  c.querySelector('.s-read').click();
  await new Promise(r=>setTimeout(r,2600));
  return document.querySelectorAll('#stream .msg').length+' messages';
`));

log('\\n=== WHICH ACTIONS APPEAR WHERE ===');
log('on a prompt :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  if(!m) return 'no user message';
  return [...m.querySelectorAll('.mact')].map(b=>b.dataset.a).join(', ');
`));
log('on a reply  :', await ev(`
  const m=document.querySelector('#stream .msg.ai');
  if(!m) return 'no assistant message';
  const acts=[...m.querySelectorAll('.mact')].map(b=>b.dataset.a);
  return acts.join(', ') +
    (acts.includes('edit') ? '   WRONG - bluee messages must not be editable' : '   (no edit, correct)');
`));
/* The row is ALWAYS there now because it carries the timestamp; the BUTTONS
   are what stay hidden until hover. Asserting the row's opacity alone would
   pass whether or not the actions are correctly out of the way. */
log('idle state  :', await ev(`
  const a=document.querySelector('#stream .msg .macts');
  const btn=a.querySelector('.mact');
  const t=a.querySelector('.mtime');
  return 'row='+getComputedStyle(a).opacity+
         '  buttons='+(btn?getComputedStyle(btn).opacity:'none')+
         '  time='+(t?JSON.stringify(t.textContent):'MISSING')+
         ((t && getComputedStyle(a).opacity==='1' && btn && getComputedStyle(btn).opacity==='0')
            ? '   (correct: time shown, actions hidden)'
            : '   WRONG');
`));
log('no reflow   :', await ev(`
  // Revealing the row must not move the conversation.
  const msgs=[...document.querySelectorAll('#stream .msg')];
  const before=msgs.map(m=>Math.round(m.getBoundingClientRect().top));
  msgs[0].dispatchEvent(new MouseEvent('mouseover',{bubbles:true}));
  msgs[0].classList.add('__hover');
  await new Promise(r=>setTimeout(r,250));
  const after=msgs.map(m=>Math.round(m.getBoundingClientRect().top));
  const moved=before.filter((v,i)=>v!==after[i]).length;
  return moved===0 ? 'nothing moved (correct)' : moved+' messages SHIFTED';
`));

log('\\n=== COPY ===');
log('copy prompt :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  const src=m.dataset.src||'';
  m.querySelector('.mact[data-a="copy"]').click();
  await new Promise(r=>setTimeout(r,500));
  let got='';
  try{ got=await navigator.clipboard.readText(); }catch(e){ got='<unreadable: '+e.name+'>'; }
  /* Compare with line endings normalised. Windows hands back CRLF from the
     clipboard whatever you wrote into it, so a multi-line message reads back
     one byte longer per line and a raw === reports a MISMATCH against an app
     that copied perfectly. Measured: src 373, clipboard 379, difference is
     exactly the six line breaks. A single-line message matches byte for byte. */
  const norm = t => t.split(String.fromCharCode(13)).join('');
  const same = norm(got) === norm(src);
  const crlf = got.length - src.length;
  return 'len '+src.length+'/'+got.length+'  '+JSON.stringify(src.slice(0,40))+
    (same ? '   match'+(crlf ? ' (+'+crlf+' from CRLF, expected on Windows)' : ' (exact)')
          : '   MISMATCH  clipboard='+JSON.stringify(got.slice(0,40)));
`));
log('tick shown  :', await ev(`
  const b=document.querySelector('#stream .msg.me .mact[data-a="copy"]');
  return 'ok class='+b.classList.contains('ok');
`));

log('\\n=== EDIT (opened, then cancelled - does not send) ===');
log('open editor :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  m.querySelector('.mact[data-a="edit"]').click();
  await new Promise(r=>setTimeout(r,400));
  const ta=m.querySelector('.medit textarea');
  if(!ta) return 'NO EDITOR';
  return 'seeded with '+JSON.stringify(ta.value.slice(0,44))+
    '  note="'+(m.querySelector('.mnote')||{}).textContent+'"';
`));
log('shot        :', await shot('edit'));
log('cancel      :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  m.querySelector('.m-cancel').click();
  await new Promise(r=>setTimeout(r,300));
  return m.querySelector('.medit') ? 'STILL OPEN' : 'restored, body len '+
    m.querySelector('.body').innerHTML.length;
`));
log('escape too  :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  m.querySelector('.mact[data-a="edit"]').click();
  await new Promise(r=>setTimeout(r,300));
  const ta=m.querySelector('.medit textarea');
  ta.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}));
  await new Promise(r=>setTimeout(r,300));
  return m.querySelector('.medit') ? 'Escape did nothing' : 'Escape restored it';
`));

log('\\n=== RETRY (wired, not fired) ===');
log('retry       :', await ev(`
  const m=document.querySelector('#stream .msg.me');
  const b=m.querySelector('.mact[data-a="retry"]');
  return b ? 'present, title="'+b.title+'"' : 'MISSING';
`));

log('\\n=== CONSOLE ===');
log(errs.length ? errs.join('\\n') : 'clean');
ws.close(); chrome.kill(); process.exit(0);
