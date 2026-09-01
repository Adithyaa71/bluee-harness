// UI audit pass — screenshots every page and measures the things a screenshot
// cannot tell you on its own: contrast of actually-rendered text, whether a
// focus ring exists, hit-target sizes, and which interactive elements have no
// transition.
//
//   node dev/uiaudit.mjs <port> <label> [--head] [--page name]
//
// Shots land in dev/shots/<label>-<page>.png so two rounds are comparable
// side by side. Same CDP plumbing as uicheck.mjs — real Chrome, real paint.

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const LABEL = process.argv[3] || 'audit';
const HEAD = process.argv.includes('--head');
const ONLY = process.argv.includes('--page')
  ? process.argv[process.argv.indexOf('--page') + 1] : null;

const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP_PORT = 9334;
const profile = path.join(os.tmpdir(), 'bluee-uiaudit-profile');

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

async function evaluate(expr) {
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
  const file = path.join(process.cwd(), 'dev', 'shots', `${LABEL}-${name}.png`);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, Buffer.from(r.result.data, 'base64'));
  return path.basename(file);
}

const log = (...a) => console.log(...a);

// Contrast + affordance probe, injected once and reused per page. Reads
// *computed* colour off real nodes, so it measures what actually painted
// rather than what the stylesheet intended.
const PROBE = `
window.__audit = {
  lin(c){ c/=255; return c<=0.04045 ? c/12.92 : Math.pow((c+0.055)/1.055, 2.4); },
  lum(rgb){ const [r,g,b]=rgb; return 0.2126*this.lin(r)+0.7152*this.lin(g)+0.0722*this.lin(b); },
  parse(s){ const m=s.match(/-?[\\d.]+/g); return m ? m.slice(0,3).map(Number) : null; },
  bgOf(el){
    let n=el;
    while(n && n!==document.documentElement){
      const c=getComputedStyle(n).backgroundColor;
      const p=this.parse(c);
      const a=c.match(/[\\d.]+\\)$/);
      if(p && !(c==='rgba(0, 0, 0, 0)') && !(a && parseFloat(a[0])===0)) return p;
      n=n.parentElement;
    }
    return [11,13,17];
  },
  ratio(el){
    const fg=this.parse(getComputedStyle(el).color);
    const bg=this.bgOf(el);
    if(!fg) return null;
    const a=this.lum(fg), b=this.lum(bg);
    const hi=Math.max(a,b), lo=Math.min(a,b);
    return (hi+0.05)/(lo+0.05);
  },
  visible(el){ const r=el.getBoundingClientRect(); return r.width>0 && r.height>0; },
  // Every node carrying its own text, inside the given root.
  texts(root){
    const out=[];
    for(const el of root.querySelectorAll('*')){
      if(!this.visible(el)) continue;
      const own=[...el.childNodes].some(n=>n.nodeType===3 && n.textContent.trim().length>1);
      if(!own) continue;
      const cs=getComputedStyle(el);
      const size=parseFloat(cs.fontSize);
      const weight=parseInt(cs.fontWeight)||400;
      const large = size>=24 || (size>=18.66 && weight>=700);
      const r=this.ratio(el);
      if(r==null) continue;
      out.push({ tag:el.tagName.toLowerCase(), cls:el.className&&el.className.toString().slice(0,28),
                 text:el.textContent.trim().slice(0,32), size:+size.toFixed(1), weight,
                 ratio:+r.toFixed(2), need: large?3:4.5, pass: r >= (large?3:4.5) });
    }
    return out;
  },
};
return 'ok';
`;

const PAGES = [
  ['chat',   '.rb[data-page="chat"]'],
  ['memory', '.rb[data-page="memory"]'],
  ['graph',  '.rb[data-page="graph"]'],
  ['play',   '.rb[data-page="play"]'],
  ['sess',   '.rb[data-page="sess"]'],
  ['set',    '.rb[data-page="set"]'],
];

await evaluate("try{ localStorage.clear(); }catch(_){}; location.reload();");
await sleep(3500);
await evaluate(PROBE);

log(`=== UI AUDIT [${LABEL}] @ 1440x900 ===\n`);

// -------------------------------------------------- global, page-independent
log('--- GLOBAL ---');
log('reduced-motion block :', await evaluate(`
  let found=false;
  for(const s of document.styleSheets){
    try{ for(const r of s.cssRules){
      if(r.conditionText && r.conditionText.includes('prefers-reduced-motion')) found=true; } }catch(_){}
  }
  return found ? 'present' : 'MISSING';
`));
log('media queries        :', await evaluate(`
  let n=0;
  for(const s of document.styleSheets){
    try{ for(const r of s.cssRules){ if(r.type===4) n++; } }catch(_){}
  }
  return n;
`));
log('focus-visible rules  :', await evaluate(`
  let n=0;
  for(const s of document.styleSheets){
    try{ for(const r of s.cssRules){
      if(r.selectorText && r.selectorText.includes(':focus-visible')) n++; } }catch(_){}
  }
  return n;
`));
log('buttons w/o :active  :', await evaluate(`
  const sel=[];
  for(const s of document.styleSheets){
    try{ for(const r of s.cssRules){
      if(r.selectorText && r.selectorText.includes(':active')) sel.push(r.selectorText); } }catch(_){}
  }
  return sel.length ? sel.length + ' active rules' : 'NO :active RULES AT ALL';
`));
log('untransitioned btns  :', await evaluate(`
  const bad=[];
  for(const b of document.querySelectorAll('button')){
    if(!window.__audit.visible(b)) continue;
    const cs=getComputedStyle(b);
    if(cs.transitionDuration==='0s') bad.push(b.className||b.id||b.textContent.trim().slice(0,12));
  }
  return bad.length + ' of ' + [...document.querySelectorAll('button')].filter(b=>window.__audit.visible(b)).length
    + (bad.length ? '  e.g. ' + [...new Set(bad)].slice(0,6).join(' | ') : '');
`));
log('small hit targets    :', await evaluate(`
  const small=[];
  for(const b of document.querySelectorAll('button,[role=button]')){
    if(!window.__audit.visible(b)) continue;
    const r=b.getBoundingClientRect();
    if(r.width<44||r.height<44) small.push(Math.round(r.width)+'x'+Math.round(r.height));
  }
  const all=[...document.querySelectorAll('button')].filter(b=>window.__audit.visible(b)).length;
  return small.length + ' of ' + all + ' below 44px';
`));

// -------------------------------------------------- per page
for (const [name, sel] of PAGES) {
  if (ONLY && ONLY !== name) continue;
  await evaluate(`document.querySelector('${sel}')?.click()`);
  await sleep(1400);

  const fails = await evaluate(`
    const t = window.__audit.texts(document.querySelector('#app'));
    const bad = t.filter(x=>!x.pass);
    // Collapse to unique class+size so the list is readable.
    const seen=new Map();
    for(const b of bad){
      const k=b.cls+'|'+b.size;
      if(!seen.has(k)) seen.set(k,{...b,n:1}); else seen.get(k).n++;
    }
    return { total:t.length, bad:bad.length, rows:[...seen.values()]
      .sort((a,b)=>a.ratio-b.ratio).slice(0,10) };
  `);

  log(`\n--- ${name.toUpperCase()} ---`);
  log(`contrast   : ${fails.bad} of ${fails.total} text nodes below AA`);
  for (const r of fails.rows)
    log(`   ${String(r.ratio).padStart(5)}:1  ${String(r.size).padStart(4)}px  x${String(r.n).padEnd(3)} .${r.cls || '(none)'}  "${r.text}"`);
  log(`shot       : ${await shot(name)}`);
}

log('\n--- CONSOLE ---');
log(consoleErrors.length ? consoleErrors.join('\n') : 'clean');

ws.close();
chrome.kill();
process.exit(consoleErrors.length ? 1 : 0);
