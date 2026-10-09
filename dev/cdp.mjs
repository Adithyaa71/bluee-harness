// Shared real-Chrome helpers for the dev/ui*.mjs checks.
//
//   const { chrome, target, attach } = await launch(port, { cdp: 9360 });
//   const page = await attach(await target(u => !u.includes('only=')));
//   await page.ev("return document.title");
//
// Pages report console errors and exceptions in `page.errs`. Reduced motion is
// switched OFF, because headless Chrome reports `reduce` by default and every
// animation check would otherwise measure the reduced-motion path (§45).
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

export const sleep = ms => new Promise(r => setTimeout(r, ms));

export async function launch(port, { cdp = 9360, profile = 'bluee-ui-profile', size = '1440,900' } = {}) {
  const exe = [
    'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
    'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
  ].find(p => fs.existsSync(p));
  const dir = path.join(os.tmpdir(), profile);
  fs.rmSync(dir, { recursive: true, force: true });
  const chrome = spawn(exe, [
    '--headless=new', `--remote-debugging-port=${cdp}`, `--user-data-dir=${dir}`,
    '--no-first-run', '--no-default-browser-check', '--disable-popup-blocking',
    `--window-size=${size}`, `http://127.0.0.1:${port}/`,
  ], { stdio: 'ignore' });

  async function target(match, tries = 80) {
    for (let i = 0; i < tries; i++) {
      try {
        const l = await fetch(`http://127.0.0.1:${cdp}/json/list`).then(r => r.json());
        const t = l.find(t => t.type === 'page' && t.url.includes(`:${port}`) && match(t.url));
        if (t) return t;
      } catch (_) {}
      await sleep(250);
    }
    return null;
  }
  return { chrome, target, attach };
}

export async function attach(t) {
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
    while (Date.now() - t0 < ms) { if (await ev(expr).catch(() => false)) return true; await sleep(250); }
    return false;
  };
  // Real key presses, so keydown handlers run exactly as they would for a person.
  const key = async (k, code = k, vk = 0) => {
    await send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key: k, code, windowsVirtualKeyCode: vk });
    await send('Input.dispatchKeyEvent', { type: 'keyUp', key: k, code, windowsVirtualKeyCode: vk });
  };
  const type = async text => { await send('Input.insertText', { text }); };
  return { ev, shot, until, key, type, errs, send };
}

export function checker() {
  const fails = [];
  const check = (n, ok, d = '') => { console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '  ' + d : ''}`); if (!ok) fails.push(n); };
  return { check, fails };
}
