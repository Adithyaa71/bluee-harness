// Providers page: `detect` must ask the row you pressed it on.
//
// It used to ask `chain.primary_client()` - the saved default - so pressing
// detect on a new OpenRouter row queried aicredits.in and reported that
// OpenRouter "does not offer" a model OpenRouter plainly has. The count was
// the tell: 412 is aicredits.in's catalogue, OpenRouter's is 447.
//
// Needs a key to run:  node dev/uidetect.mjs 7793 sk-or-v1-...
// Skipped without one rather than failing, since it calls a real endpoint.
import { spawn } from 'node:child_process';
import fs from 'node:fs'; import os from 'node:os'; import path from 'node:path';
const PORT = process.argv[2] || '7793', CDP = 9375;
const KEY = process.argv[3] || process.env.OPENROUTER_KEY;
if (!KEY) {
  console.log('skipped: no OpenRouter key (pass one as argv[3] or set OPENROUTER_KEY)');
  process.exit(0);
}
const CHROME = ['C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe'].find(p => fs.existsSync(p));
const chrome = spawn(CHROME, ['--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-detect')}`, '--no-first-run',
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
  if (m.method === 'Runtime.exceptionThrown') errs.push('EXCEPTION ' + String(m.params.exceptionDetails?.exception?.description).split('\n')[0]);
});
const send = (m, p = {}) => { const i = ++id; ws.send(JSON.stringify({ id: i, method: m, params: p })); return new Promise(r => w.set(i, r)); };
await send('Runtime.enable');
const ev = async x => { const r = await send('Runtime.evaluate',
  { expression: `(async()=>{ ${x} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return 'THREW: ' + (r.result.exceptionDetails.exception?.description || '').split('\n')[0];
  return r.result?.result?.value; };
for (let i = 0; i < 60; i++) { if (await ev("return typeof detectContext === 'function'")) break; await sleep(300); }
await sleep(1200);

await ev("document.querySelector('.rb[data-page=\"set\"]').click()");
await sleep(1500);
await ev("[...document.querySelectorAll('#settabs button')].find(b=>/PROVIDER/i.test(b.textContent))?.click()");
await sleep(1800);

const fails = [];
const check = (n, ok, d = '') => { console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '\n         ' + d : ''}`); if (!ok) fails.push(n); };

// Fill the FIRST card as an OpenRouter row with the mistyped id.
await ev(`
  const c = document.querySelector('#plist .pcard');
  c.querySelector('.f-name').value = 'qwen3.8 27';
  c.querySelector('.f-url').value  = 'https://openrouter.ai/api/v1';
  c.querySelector('.f-model').value = 'qwen/qwen3.827b:free';
  c.querySelector('.f-key').value  = ${JSON.stringify(KEY)};
  return 'filled';
`);
await ev("document.querySelector('#plist .pcard .f-detect').click()");
await sleep(6000);
const msg1 = await ev("return document.querySelector('#plist .pcard .fmsg').textContent");
console.log('\nmistyped id  :', JSON.stringify(msg1));
check('names the endpoint it asked', /openrouter\.ai/.test(msg1), msg1);
check('does NOT blame aicredits (412)', !/412/.test(msg1));
check('suggests the right id', /qwen\/qwen3\.8-27b:free/.test(msg1));
check('offers a one-click fix',
  await ev("return !!document.querySelector('#plist .pcard .fmsg button')"));

// Press the fix button.
await ev("document.querySelector('#plist .pcard .fmsg button').click()");
await sleep(6000);
const msg2 = await ev("return document.querySelector('#plist .pcard .fmsg').textContent");
const model = await ev("return document.querySelector('#plist .pcard .f-model').value");
const ctx = await ev("return document.querySelector('#plist .pcard .f-ctx').value");
console.log('\nafter the fix :', JSON.stringify(msg2));
check('model id corrected', model === 'qwen/qwen3.8-27b:free', model);
check('context length filled from OpenRouter', ctx === '262144', 'ctx=' + ctx);
check('message is the success one', /262,144|262144/.test(msg2));

console.log('\nconsole:', errs.length ? errs.join(' | ') : 'none');
console.log(fails.length ? '\nFAILED: ' + fails.join(', ') : '\nall detect checks passed');
try { chrome.kill(); } catch (_) {}
process.exit(fails.length ? 1 : 0);
