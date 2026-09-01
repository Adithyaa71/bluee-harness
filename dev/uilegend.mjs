// Two questions, answered by measurement rather than argument:
//   1. WHEN does #glegend get its text after the Graph page is opened?
//   2. What does drawing all 1062 entities actually cost?
//
// Both matter because the reported "legend is empty" and "hiding 89% by
// default" want opposite fixes if the answers are what I think they are.

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

const CDP = 9339;
const chrome = spawn(CHROME, [
  '--headless=new', `--remote-debugging-port=${CDP}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-legend')}`,
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
await send('Runtime.enable'); await send('Page.enable');
const ev = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return 'THREW: ' + r.result.exceptionDetails.exception?.description;
  return r.result?.result?.value;
};
const log = (...a) => console.log(...a);

await ev("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(4000);

log('=== 1. WHEN DOES THE LEGEND FILL? ===');
log(await ev(`
  const t0=performance.now();
  document.querySelector('.rb[data-page="graph"]').click();
  const marks=[];
  for(let i=0;i<40;i++){
    await new Promise(r=>setTimeout(r,150));
    const el=document.querySelector('#glegend');
    const txt=(el&&el.textContent||'').trim();
    const chips=document.querySelectorAll('#glegend .gk').length;
    marks.push({ms:Math.round(performance.now()-t0),len:txt.length,chips});
    if(chips>0) break;
  }
  const first=marks.find(m=>m.chips>0);
  return 'legend had text after ' + (first?first.ms+'ms':'NEVER within 6s') +
    '   samples: ' + marks.slice(0,6).map(m=>m.ms+'ms len='+m.len+' chips='+m.chips).join(' | ');
`));

log('\n=== 2. WHAT DOES DRAWING EVERYTHING COST? ===');
log('default    :', await ev(`
  return (gNodes?gNodes.length:0)+' nodes / '+(gEdges?gEdges.length:0)+
    ' edges drawn of '+((gData&&gData.nodes||[]).length)+' total';
`));

// Time a layout + render of the full graph.
log('show all   :', await ev(`
  const t0=performance.now();
  gHide.clear(); gLaidOut=false;
  await drawGraph();
  const layout=performance.now()-t0;
  // Then time pure repaint, which is what dragging costs.
  const frames=[];
  for(let i=0;i<20;i++){
    const a=performance.now();
    gView.x+=2; gRender();
    frames.push(performance.now()-a);
  }
  frames.sort((x,y)=>x-y);
  return (gNodes?gNodes.length:0)+' nodes / '+(gEdges?gEdges.length:0)+' edges | '+
    'layout+first draw '+Math.round(layout)+'ms | '+
    'repaint median '+frames[10].toFixed(1)+'ms worst '+frames[19].toFixed(1)+'ms';
`));

log('labels     :', await ev("return (window.__labels||0)+' drawn'"));
log('fit scale  :', await ev("return gView.k.toFixed(3)"));

log('\n=== 3. IS THE SHOW-ALL CONTROL ACTUALLY TRUNCATED? ===');
log(await ev(`
  gHide.clear(); gHide.add('symbol'); gHide.add('file');
  gLaidOut=false; await drawGraph();
  await new Promise(r=>setTimeout(r,600));
  const b=document.querySelector('#glegend .gshowall');
  if(!b) return 'no show-all control present';
  const r=b.getBoundingClientRect();
  const cs=getComputedStyle(b);
  // scrollWidth > clientWidth is the actual definition of clipped text.
  return 'text="'+b.textContent+'"  box '+Math.round(r.width)+'x'+Math.round(r.height)+
    '  scrollW='+b.scrollWidth+' clientW='+b.clientWidth+
    '  overflow='+cs.overflow+'  ' +
    (b.scrollWidth>b.clientWidth+1 ? 'CLIPPED' : 'fully visible');
`));

log('\n=== CONSOLE ===');
log(errs.length ? errs.join('\n') : 'clean');
ws.close(); chrome.kill(); process.exit(0);
