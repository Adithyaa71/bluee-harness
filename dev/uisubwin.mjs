// Sub-agent windows, end to end in real Chrome (stage 2 of dev/plan-subagents.md).
//
//   harness dash 7788 &      then      node dev/uisubwin.mjs 7788
//
// Spends a few cheap model turns. Covers: a window opens on spawn, talking to
// the agent from it, ask_user answered in the window, bluee spawning an agent
// with a background task and being woken with its result, sleep -> wake on the
// next message, and stop -> the window says it ended.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
const CDP = 9351;
const profile = path.join(os.tmpdir(), 'bluee-subwin-profile');
fs.rmSync(profile, { recursive: true, force: true });
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`, `--user-data-dir=${profile}`,
  '--no-first-run', '--no-default-browser-check', '--disable-popup-blocking',
  '--window-size=1440,900', `${B}/`,
], { stdio: 'ignore' });
const sleep = ms => new Promise(r => setTimeout(r, ms));
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());

async function target(match, tries = 80) {
  for (let i = 0; i < tries; i++) {
    try {
      const l = await fetch(`http://127.0.0.1:${CDP}/json/list`).then(r => r.json());
      const t = l.find(t => t.type === 'page' && match(t.url));
      if (t) return t;
    } catch (_) {}
    await sleep(250);
  }
  return null;
}

async function attach(t) {
  const ws = new WebSocket(t.webSocketDebuggerUrl);
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
  const send = (m, p = {}) => { const mid = ++id; ws.send(JSON.stringify({ id: mid, method: m, params: p })); return new Promise(r => waiting.set(mid, r)); };
  await send('Runtime.enable'); await send('Page.enable');
  await send('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
  const ev = async expr => {
    const r = await send('Runtime.evaluate', { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
    if (r.result?.exceptionDetails) throw new Error(r.result.exceptionDetails.exception?.description || 'threw');
    return r.result?.result?.value;
  };
  const shot = async name => {
    const r = await send('Page.captureScreenshot', { format: 'png' });
    const f = path.join(process.cwd(), 'dev', 'shots', name + '.png');
    fs.mkdirSync(path.dirname(f), { recursive: true });
    fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  };
  const until = async (expr, ms = 90000) => {
    const t0 = Date.now();
    while (Date.now() - t0 < ms) { if (await ev(expr).catch(() => false)) return true; await sleep(300); }
    return false;
  };
  return { ev, shot, until, errs };
}

const fails = [];
const check = (n, ok, d = '') => { console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '  ' + d : ''}`); if (!ok) fails.push(n); };

const mainT = await target(u => u.startsWith(B) && !u.includes('only='));
const main = await attach(mainT);
await main.until("return typeof agentsFeed === 'function' && document.readyState === 'complete'", 20000);
await sleep(800);   // feed socket connects

// 1. Spawn from the API -> the main window opens the agent's window.
console.log('spawn opens a window');
const sp = await post('/api/agents', { name: 'tester', purpose: 'ui check', servers: [] });
const id = sp.agent.id;
const winT = await target(u => u.includes('only=agent') && u.includes('id=' + id), 40);
check('agent window opened', !!winT, winT?.url);
if (!winT) { chrome.kill(); process.exit(1); }
const w = await attach(winT);
await w.until("return !!document.querySelector('#aghd .nm') && document.querySelector('#aghd .nm').textContent === 'tester'", 15000);
const hd = await w.ev("const h=document.querySelector('#aghd');const c=document.querySelector('#composer');return {nm:h.querySelector('.nm').textContent, st:h.dataset.st, tm:h.querySelector('.tm').textContent, comp:c.getBoundingClientRect().height, rail:document.querySelector('#rail').getBoundingClientRect().width, ph:document.querySelector('#input').placeholder}");
check('header names it', hd.nm === 'tester', JSON.stringify(hd));
check('timers shown 45m / 2h', hd.tm === 'sleeps 45m · ends 2h', hd.tm);
check('composer visible', hd.comp > 40, 'h=' + hd.comp);
check('no rail in the window', hd.rail === 0);
check('placeholder says bluee sees it', /bluee sees/.test(hd.ph));
await w.shot('subwin-1-open');

// 2. Talk to it from its window.
console.log('talk in the window');
await w.ev("const i=document.querySelector('#input'); i.value='Reply with exactly the word PONG and nothing else. No tools.'; i.dispatchEvent(new Event('input')); send(); return 1");
const replied = await w.until("return [...document.querySelectorAll('#stream .msg.ai .body')].some(b=>/PONG/.test(b.textContent)) && document.querySelector('#aghd').dataset.st==='ready'");
check('reply streamed into the window', replied);
const mine = await w.ev("return document.querySelectorAll('#stream .msg.me').length");
check('my message drawn once', mine === 1, 'me=' + mine);

// 3. ask_user: the question appears, answering it unblocks the agent.
console.log('ask_user in the window');
await w.ev("const i=document.querySelector('#input'); i.value='Use your ask_user tool to ask me what my favourite colour is. Then reply with only that colour, lowercase.'; i.dispatchEvent(new Event('input')); send(); return 1");
const asked = await w.until("return !!document.querySelector('.qcard:not(.done) .qin')");
check('question card shown', asked);
const mainSaw = await main.until("return [...document.querySelectorAll('#stream .note')].some(n=>/asks you/.test(n.textContent))", 5000);
check('main window told me it asked', mainSaw);
await w.shot('subwin-2-question');
await w.ev("const i=document.querySelector('.qcard .qin'); i.value='teal'; document.querySelector('.qcard .qgo').click(); return 1");
const answered = await w.until("return [...document.querySelectorAll('#stream .msg.ai .body')].some(b=>/^\\s*teal\\s*$/i.test(b.textContent.trim()) || /\\bteal\\b/i.test(b.textContent) && !b.querySelector('.qcard')) && document.querySelector('#aghd').dataset.st==='ready'");
check('answer reached the agent', answered);

// 4. Sleep, then a message wakes it with its memory intact.
console.log('sleep and wake');
await post('/api/agents/sleep', { id });
const slept = await w.until("return document.querySelector('#aghd').dataset.st==='sleeping'", 5000);
check('header shows sleeping', slept);
await w.ev("const i=document.querySelector('#input'); i.value='What colour did I tell you? One word.'; i.dispatchEvent(new Event('input')); send(); return 1");
const woke = await w.until("const b=[...document.querySelectorAll('#stream .msg.ai .body')].pop(); return b && /teal/i.test(b.textContent) && document.querySelector('#aghd').dataset.st==='ready'");
check('woke and remembered', woke);

// 5. bluee spawns a worker with a background task and is woken with the result.
console.log('bluee delegates in the background');
await main.ev("const i=document.querySelector('#input'); i.value='Spawn a sub-agent named counter with servers [] and the task: reply with the number 7 and nothing else. Do not wait for it - just tell me you started it.'; i.dispatchEvent(new Event('input')); send(); return 1");
const cT = await target(u => u.includes('only=agent') && !u.includes('id=' + id), 200);
check('counter got its own window', !!cT);
const woken = await main.until("return [...document.querySelectorAll('#stream .note')].some(n=>/results arrived/.test(n.textContent)) && !busy && [...document.querySelectorAll('#stream .msg.ai .body')].pop()?.textContent.includes('7')", 150000);
check('bluee was woken and reported the 7', woken,
  await main.ev("return ([...document.querySelectorAll('#stream .msg.ai .body')].pop()?.textContent||'').slice(0,140)"));
if (cT) {
  const c = await attach(cT);
  await c.until("return [...document.querySelectorAll('.from-tag')].length>0", 15000);
  check('counter window labels the job "from bluee"', await c.ev("return document.querySelectorAll('.from-tag').length>0"));
  await c.shot('subwin-3-from-bluee');
  check('counter window console clean', c.errs.length === 0, c.errs.join(' | '));
}
await main.shot('subwin-4-main-woken');

// 6. Stop -> the window says it ended and stops taking input.
console.log('stop');
await post('/api/agents/stop', { id });
const ended = await w.until("return document.querySelector('#aghd').dataset.st==='ended' && !!document.querySelector('#agended') && document.querySelector('#input').disabled", 5000);
check('window shows ended, input disabled', ended);
await w.shot('subwin-5-ended');

check('agent window console clean', w.errs.length === 0, w.errs.join(' | '));
check('main window console clean', main.errs.length === 0, main.errs.join(' | '));
for (const a of (await fetch(B + '/api/agents').then(r => r.json())).agents) await post('/api/agents/stop', { id: a.id });
chrome.kill();
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
