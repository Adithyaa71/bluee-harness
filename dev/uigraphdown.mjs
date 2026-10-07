// The Graph page when the graph server is NOT connected.
//
// Written because the page said "Graph is empty. Run /reduce" while the store
// held 1,322 entities and the server was simply not running (§55). That is the
// worst kind of empty state: it is confident, it is wrong, and it sends you to
// rebuild a database that was never the problem.
//
// Usage: node dev/uigraphdown.mjs <healthy-port> <broken-port>
//   healthy-port  a dash with its MCP servers connected
//   broken-port   a dash started with HARNESS_MCP_CONFIG pointing at a config
//                 whose commands do not exist
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const OK_PORT = process.argv[2] || '7788';
const DOWN_PORT = process.argv[3] || '7789';
const CDP = 9351;
const CHROME = [
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
].find(p => fs.existsSync(p));

const sleep = ms => new Promise(r => setTimeout(r, ms));
const fails = [];
const check = (n, ok, d = '') => {
  console.log(`  ${ok ? 'ok  ' : 'FAIL'}  ${n}${d ? '   ' + d : ''}`);
  if (!ok) fails.push(n);
};

const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-graphdown')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,900',
  `http://127.0.0.1:${OK_PORT}/`,
], { stdio: 'ignore' });

async function page(port) {
  for (let i = 0; i < 80; i++) {
    try {
      const l = await fetch(`http://127.0.0.1:${CDP}/json/list`).then(r => r.json());
      const p = l.find(t => t.type === 'page' && t.url.includes(`:${port}`));
      if (p) return p;
    } catch (_) {}
    await sleep(300);
  }
  throw new Error('no page on ' + port);
}

let ws, id = 0, waiting = new Map(), errs = [];
async function attach(target) {
  if (ws) try { ws.close(); } catch (_) {}
  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise(r => ws.addEventListener('open', r, { once: true }));
  waiting = new Map();
  ws.addEventListener('message', e => {
    const m = JSON.parse(e.data);
    if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
    if (m.method === 'Runtime.exceptionThrown')
      errs.push('EXCEPTION ' + String(m.params.exceptionDetails?.exception?.description).split('\n')[0]);
  });
  await send('Runtime.enable');
  await send('Page.enable');
  // Headless reports `prefers-reduced-motion: reduce`, so without this every
  // transition measured here is the reduced-motion path, not the real one (§45).
  await send('Emulation.setEmulatedMedia',
    { features: [{ name: 'prefers-reduced-motion', value: 'no-preference' }] });
}
const send = (method, params = {}) => {
  const mid = ++id;
  ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(r => waiting.set(mid, r));
};
const ev = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails)
    return 'THREW: ' + (r.result.exceptionDetails.exception?.description || '').split('\n')[0];
  return r.result?.result?.value;
};
async function ready() {
  for (let i = 0; i < 80; i++) {
    if (await ev("return typeof drawGraph === 'function'")) return;
    await sleep(300);
  }
  throw new Error('page script never ran');
}
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  fs.mkdirSync('dev/shots', { recursive: true });
  fs.writeFileSync(path.join('dev', 'shots', name + '.png'), Buffer.from(r.result.data, 'base64'));
  return 'dev/shots/' + name + '.png';
}

await attach(await page(OK_PORT));
await ready();
await sleep(800);

console.log('=== SERVERS UP: the graph must actually draw ===');
const up = await ev(`
  document.querySelector('.rb[data-page="graph"]').click();
  await new Promise(r=>setTimeout(r,2500));
  return JSON.stringify({
    nodes: (gData&&gData.nodes||[]).length,
    drawn: (gNodes||[]).length,
    ready: gReady,
    legend: ($('#glegend').textContent||'').slice(0,60),
  });
`);
const U = JSON.parse(up);
console.log('  ', up);
check('entities load', U.nodes > 0, U.nodes + ' entities');
check('they are drawn', U.drawn > 0, U.drawn + ' nodes');
check('gReady is true when the fetch worked', U.ready === true);
check('legend is populated', /\d/.test(U.legend), JSON.stringify(U.legend));
console.log('  shot:', await shot('graph-up'));

console.log('\n=== SERVERS DOWN: it must say WHICH empty this is ===');
await send('Page.navigate', { url: `http://127.0.0.1:${DOWN_PORT}/` });
await sleep(1500);
await attach(await page(DOWN_PORT));
await ready();
await sleep(800);

const down = await ev(`
  document.querySelector('.rb[data-page="graph"]').click();
  await new Promise(r=>setTimeout(r,2500));
  return JSON.stringify({
    ready: gReady,
    error: (gData&&gData.error)||null,
    nodes: (gData&&gData.nodes||[]).length,
  });
`);
const D = JSON.parse(down);
console.log('  ', down);
check('the failed fetch is recognised as failed', D.ready === false, 'gReady=' + D.ready);
check('the real reason is kept', !!D.error && /not connected|unreachable/i.test(D.error),
  String(D.error).slice(0, 70));
check('it does NOT claim the graph is empty', !/Run \/reduce/i.test(String(D.error || '')));
console.log('  shot:', await shot('graph-down'));

console.log('\n=== SETTINGS -> MCP names the failure and offers a fix ===');
const mcp = await ev(`
  document.querySelector('.rb[data-page="set"]').click();
  await new Promise(r=>setTimeout(r,600));
  const tab=[...document.querySelectorAll('#settabs button')].find(b=>/mcp/i.test(b.textContent));
  if(tab) tab.click();
  await new Promise(r=>setTimeout(r,1400));
  const rows=[...document.querySelectorAll('#mlist .pcard')];
  return JSON.stringify({
    rows: rows.length,
    withReason: rows.filter(r=>/cannot find the path|spawning/i.test(r.textContent)).length,
    banner: !!document.querySelector('#setbody .errmsg'),
    reconnect: !!document.querySelector('#mrecon'),
  });
`);
const M = JSON.parse(mcp);
console.log('  ', mcp);
check('every dead server shows its reason', M.rows > 0 && M.withReason === M.rows,
  M.withReason + '/' + M.rows);
check('a banner says tools are gone', M.banner === true);
check('there is a Reconnect button', M.reconnect === true);
console.log('  shot:', await shot('mcp-down'));

console.log('\nconsole:', errs.length ? errs.join(' | ') : 'clean');
if (errs.length) fails.push('console errors');
console.log(fails.length ? '\nFAILED: ' + fails.join(', ') : '\nall checks passed');
try { chrome.kill(); } catch (_) {}
process.exit(fails.length ? 1 : 0);
