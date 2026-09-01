// Focused bug hunt: "graph is missing".
//
// Checks BOTH places the graph canvas can live - the Graph page and the pane
// under Memory - because §22 moves one canvas between hosts rather than having
// two, so "missing" can mean it is drawn somewhere you are not looking.

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

const CDP = 9338;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-graphbug')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,900',
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
ws.addEventListener('message', ev => {
  const m = JSON.parse(ev.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    errs.push('EXCEPTION: ' + (m.params.exceptionDetails?.exception?.description
      || m.params.exceptionDetails?.text));
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    errs.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});
const send = (method, params = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(res => waiting.set(mid, res));
};
await send('Runtime.enable'); await send('Page.enable');
const ev = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails)
    return 'THREW: ' + (r.result.exceptionDetails.exception?.description || 'page threw');
  return r.result?.result?.value;
};
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const f = path.join(process.cwd(), 'dev', 'shots', `bug-${name}.png`);
  fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  return path.basename(f);
}
const log = (...a) => console.log(...a);

await ev("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(4000);

// Where does the canvas live at rest, and is it painted?
const probe = `
  const cv=document.querySelector('#gcv');
  if(!cv) return 'NO #gcv IN THE DOCUMENT AT ALL';
  const r=cv.getBoundingClientRect();
  const cs=getComputedStyle(cv);
  // Is anything actually drawn? Sample the backing store.
  let painted='n/a';
  try{
    const c2=cv.getContext('2d');
    const d=c2.getImageData(0,0,Math.min(cv.width,600),Math.min(cv.height,400)).data;
    let nonbg=0;
    for(let i=0;i<d.length;i+=4){
      // background is #0d0f14
      if(!(d[i]===13&&d[i+1]===15&&d[i+2]===20)) nonbg++;
    }
    painted=nonbg+' non-background pixels sampled';
  }catch(e){ painted='readback failed: '+e.message; }
  return {
    parent: cv.parentElement ? (cv.parentElement.id||cv.parentElement.className||'?') : 'DETACHED',
    rect: Math.round(r.width)+'x'+Math.round(r.height),
    backing: cv.width+'x'+cv.height,
    display: cs.display, visibility: cs.visibility, opacity: cs.opacity,
    painted,
    nodes: (typeof gNodes!=='undefined'&&gNodes)?gNodes.length:'gNodes '+typeof gNodes,
    data: (typeof gData!=='undefined'&&gData&&gData.nodes)?gData.nodes.length:'gData '+typeof gData,
    laidOut: typeof gLaidOut!=='undefined'?gLaidOut:'undef',
    maxDeg: typeof gMaxDeg!=='undefined'?gMaxDeg:'undef',
  };
`;

log('=== BEFORE VISITING ANY GRAPH ===');
log(JSON.stringify(await ev(probe), null, 1));

log('\n=== GRAPH PAGE ===');
log('click     :', await ev("document.querySelector('.rb[data-page=\"graph\"]').click(); return 'ok'"));
await sleep(2500);
log(JSON.stringify(await ev(probe), null, 1));
log('legend    :', await ev("return (document.querySelector('#glegend')||{}).textContent?.trim().slice(0,120)"));
log('view      :', await ev("return typeof gView!=='undefined'?JSON.stringify(gView):'undef'"));
log('shot      :', await shot('graphpage'));

log('\n=== MEMORY PAGE (graph pane underneath) ===');
log('click     :', await ev("document.querySelector('.rb[data-page=\"memory\"]').click(); return 'ok'"));
await sleep(2500);
log(JSON.stringify(await ev(probe), null, 1));
log('mgholder  :', await ev(`
  const h=document.querySelector('#mgholder');
  if(!h) return 'missing';
  const r=h.getBoundingClientRect();
  return Math.round(r.width)+'x'+Math.round(r.height)+' children='+h.children.length;
`));
log('shot      :', await shot('memorygraph'));

log('\n=== BACK TO GRAPH PAGE (does it come home?) ===');
log('click     :', await ev("document.querySelector('.rb[data-page=\"graph\"]').click(); return 'ok'"));
await sleep(2500);
log(JSON.stringify(await ev(probe), null, 1));
log('shot      :', await shot('graphpage2'));

// The bug only appears in one visit order, so the check has to drive both.
// Going straight to Memory sized the canvas correctly and looked fine, which
// is precisely why this survived every earlier round of checking.
log('\n=== ORDER CHECK: does the canvas fit its host either way? ===');
for (const order of [['memory', 'graph'], ['graph', 'memory']]) {
  await ev('location.reload()');
  await sleep(3800);
  for (const page of order) {
    await ev(`document.querySelector('.rb[data-page="${page}"]').click()`);
    await sleep(2200);
  }
  log(order.join(' then ').padEnd(18), await ev(`
    const cv=document.querySelector('#gcv');
    const host=cv.parentElement;
    const c=cv.getBoundingClientRect(), h=host.getBoundingClientRect();
    const over=Math.round(c.height-h.height);
    return 'canvas '+Math.round(c.width)+'x'+Math.round(c.height)+
      ' in #'+(host.id||host.className)+' '+Math.round(h.width)+'x'+Math.round(h.height)+
      (over>2 ? '   CLIPPED by '+over+'px' : '   fits')+
      '  zoom '+(typeof gView!=='undefined'?gView.k.toFixed(2):'?')+
      '  zoomlabels '+(window.__labels||0)+' of '+(gNodes?gNodes.length:0)+' drawn';
  `));
  await shot('order-' + order.join('-'));
}

// Regressions for the three latent bugs found alongside the sizing one.
log('\n=== STALE RINGS AFTER A FAILED SEARCH ===');
log('good search :', await ev(`
  document.querySelector('.rb[data-page="memory"]').click();
  await new Promise(r=>setTimeout(r,1400));
  const f=document.querySelector('#msearch');
  f.value='append-only event log';
  f.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  await new Promise(r=>setTimeout(r,3200));
  return 'rings='+(gMatch?gMatch.size:0)+'  caption="'+
    document.querySelector('#mglink').textContent+'"';
`));
log('bad search  :', await ev(`
  const f=document.querySelector('#msearch');
  f.value='zzzzqqqx nonexistent gibberish';
  f.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  await new Promise(r=>setTimeout(r,3200));
  const rings=gMatch?gMatch.size:0;
  const cap=document.querySelector('#mglink').textContent;
  return 'rings='+rings+'  caption="'+cap+'"'+
    ((rings===0 && !cap) ? '   cleared' : '   still set (search returned rows)');
`));
log('empty path  :', await ev(`
  await gLinkResults([]);
  await new Promise(r=>setTimeout(r,400));
  const rings=gMatch?gMatch.size:0;
  const cap=document.querySelector('#mglink').textContent;
  return 'rings='+rings+'  caption="'+cap+'"'+
    ((rings===0 && !cap) ? '   cleared (correct)' : '   STALE');
`));
log('weak banner :', await ev(`
  const f=document.querySelector('#msearch');
  f.value='zzzzqqqx nonexistent gibberish';
  f.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  await new Promise(r=>setTimeout(r,3200));
  const w=document.querySelector('.mweak');
  const bar=document.querySelector('#mhits .hit .hbar i');
  return (w?'shown':'MISSING')+'  top bar width='+(bar?getComputedStyle(bar).width:'?');
`));

log('\n=== SEND BUTTON vs ATTACHMENTS ===');
log('attach only :', await ev(`
  document.querySelector('.rb[data-page="chat"]').click();
  await new Promise(r=>setTimeout(r,900));
  attachments=[{name:'note.txt',text:'hello'}]; renderAtt();
  await new Promise(r=>setTimeout(r,250));
  const d=document.querySelector('#send').disabled;
  return 'send disabled='+d+(d ? '   WRONG - a file on its own is sendable' : '   correct');
`));
log('remove it   :', await ev(`
  attachments=[]; renderAtt();
  await new Promise(r=>setTimeout(r,250));
  const d=document.querySelector('#send').disabled;
  return 'send disabled='+d+(d ? '   correct' : '   WRONG - nothing left to send');
`));

log('\n=== CONSOLE ===');
log(errs.length ? errs.join('\n') : 'clean');
ws.close(); chrome.kill(); process.exit(0);
