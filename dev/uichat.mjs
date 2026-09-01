// Chat page probe. The general audit only ever sees the empty state, because a
// fresh page has no conversation in it - so message rendering, tool chips, the
// composer's states and the slash menu were never actually looked at.
//
// This loads a real past session read-only, then exercises the composer.
//
//   node dev/uichat.mjs <port> <label>

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const LABEL = process.argv[3] || 'chat';
const HEAD = process.argv.includes('--head');
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP_PORT = 9336;
const chrome = spawn(CHROME, [
  HEAD ? '--new-window' : '--headless=new',
  `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-uichat-profile')}`,
  '--no-first-run', '--no-default-browser-check', '--window-size=1440,900',
  `http://127.0.0.1:${PORT}/`,
], { stdio: 'ignore' });

const sleep = ms => new Promise(r => setTimeout(r, ms));
async function target() {
  for (let i = 0; i < 60; i++) {
    try {
      const l = await fetch(`http://127.0.0.1:${CDP_PORT}/json/list`).then(r => r.json());
      const pg = l.find(t => t.type === 'page' && t.url.includes(`:${PORT}`));
      if (pg) return pg;
    } catch (_) {}
    await sleep(300);
  }
  throw new Error('no page over CDP');
}
const t = await target();
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => ws.addEventListener('open', r, { once: true }));
let id = 0; const waiting = new Map(); const errs = [];
ws.addEventListener('message', ev => {
  const m = JSON.parse(ev.data);
  if (m.id && waiting.has(m.id)) { waiting.get(m.id)(m); waiting.delete(m.id); }
  if (m.method === 'Runtime.exceptionThrown')
    errs.push('exception: ' + (m.params.exceptionDetails?.exception?.description || m.params.exceptionDetails?.text));
  if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error')
    errs.push('console.error: ' + m.params.args.map(a => a.value ?? a.description).join(' '));
});
const send = (method, params = {}) => {
  const mid = ++id; ws.send(JSON.stringify({ id: mid, method, params }));
  return new Promise(res => waiting.set(mid, res));
};
await send('Runtime.enable'); await send('Page.enable');
const evaluate = async expr => {
  const r = await send('Runtime.evaluate',
    { expression: `(async()=>{ ${expr} })()`, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails)
    throw new Error(r.result.exceptionDetails.exception?.description || 'page threw');
  return r.result?.result?.value;
};
async function shot(name) {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  const f = path.join(process.cwd(), 'dev', 'shots', `${LABEL}-${name}.png`);
  fs.mkdirSync(path.dirname(f), { recursive: true });
  fs.writeFileSync(f, Buffer.from(r.result.data, 'base64'));
  return path.basename(f);
}
const log = (...a) => console.log(...a);

await evaluate("try{localStorage.clear()}catch(_){}; location.reload();");
await sleep(3500);

log(`=== CHAT PAGE [${LABEL}] ===\n`);

// ---------------------------------------------------------------- composer
log('--- COMPOSER, EMPTY ---');
log('send disabled  :', await evaluate("return document.querySelector('#send').disabled"));
log('send looks     :', await evaluate(`
  const s=getComputedStyle(document.querySelector('#send'));
  return s.backgroundColor+' / '+s.color;
`));
log('composer bar   :', await evaluate(`
  const r=document.querySelector('.cwrap').getBoundingClientRect();
  const row=document.querySelector('.crow').getBoundingClientRect();
  return Math.round(r.width)+'x'+Math.round(r.height)+'  buttons row '+Math.round(row.height)+'px';
`));
log('textarea rows  :', await evaluate(`
  const t=document.querySelector('#input');
  return Math.round(t.getBoundingClientRect().height)+'px  placeholder="'+t.placeholder+'"';
`));

await evaluate("const t=document.querySelector('#input'); t.value='hello'; t.dispatchEvent(new Event('input',{bubbles:true}));");
await sleep(300);
log('\n--- COMPOSER, TYPED ---');
log('send disabled  :', await evaluate("return document.querySelector('#send').disabled"));
log('any change?    :', await evaluate(`
  const s=getComputedStyle(document.querySelector('#send'));
  return s.backgroundColor+' / '+s.color;
`));

// slash menu
await evaluate("const t=document.querySelector('#input'); t.value='/'; t.dispatchEvent(new Event('input',{bubbles:true}));");
await sleep(400);
log('\n--- SLASH MENU ---');
log('open           :', await evaluate("return getComputedStyle(document.querySelector('#slash')).display!=='none'"));
log('items          :', await evaluate("return document.querySelectorAll('#slash .sitem').length"));
log('size           :', await evaluate(`
  const r=document.querySelector('#slash').getBoundingClientRect();
  return Math.round(r.width)+'x'+Math.round(r.height);
`));
log('shot           :', await shot('slash'));
await evaluate("const t=document.querySelector('#input'); t.value=''; t.dispatchEvent(new Event('input',{bubbles:true})); document.querySelector('#slash').style.display='none';");

// ------------------------------------------------- a real conversation
await evaluate("document.querySelector('.rb[data-page=\"sess\"]').click()");
await sleep(1600);
const opened = await evaluate(`
  // pick the session with the most tool calls, so chips and results render
  const cards=[...document.querySelectorAll('#sessbody .scard')];
  let best=null,bn=-1;
  for(const c of cards){
    const m=(c.querySelector('.smeta')||{}).textContent||'';
    const n=parseInt((m.match(/(\\d+) tool calls/)||[])[1]||'0',10);
    if(n>bn){bn=n;best=c;}
  }
  if(!best) return 'no sessions';
  best.querySelector('.s-read').click();
  return 'opened one with '+bn+' tool calls';
`);
log('\n--- READ A REAL SESSION ---');
log('picked         :', opened);
await sleep(2500);

log('messages       :', await evaluate("return document.querySelectorAll('#stream .msg').length"));
log('tool chips     :', await evaluate("return document.querySelectorAll('#stream .tool').length"));
log('measure        :', await evaluate(`
  const ms=[...document.querySelectorAll('#stream .msg')];
  if(!ms.length) return 'none';
  const b=ms[0].querySelector('.body').getBoundingClientRect();
  const av=ms[0].querySelector('.av').getBoundingClientRect();
  const gaps=[];
  for(let i=1;i<ms.length;i++){
    gaps.push(Math.round(ms[i].getBoundingClientRect().top - ms[i-1].getBoundingClientRect().bottom));
  }
  return 'body '+Math.round(b.width)+'px wide · avatar '+Math.round(av.width)+'px · gaps ['+gaps.join(',')+']';
`));
log('chars per line :', await evaluate(`
  // rough measure of line length in the reading column
  const p=document.querySelector('#stream .msg .body p');
  if(!p) return 'n/a';
  const w=p.getBoundingClientRect().width;
  const cs=getComputedStyle(p);
  const cv=document.createElement('canvas').getContext('2d');
  cv.font=cs.fontWeight+' '+cs.fontSize+' '+cs.fontFamily;
  const em=cv.measureText('abcdefghijklmnopqrstuvwxyz').width/26;
  return Math.round(w/em)+' characters (55-75 is the comfortable band)';
`));
log('user vs ai     :', await evaluate(`
  const me=document.querySelector('#stream .msg.me'), ai=document.querySelector('#stream .msg.ai');
  if(!me||!ai) return 'n/a';
  const g=e=>{const s=getComputedStyle(e); return s.backgroundColor;};
  const gb=e=>{const s=getComputedStyle(e.querySelector('.body')); return s.backgroundColor+' pad '+s.padding;};
  return 'me avatar '+g(me.querySelector('.av'))+' body '+gb(me)+
       ' | ai avatar '+g(ai.querySelector('.av'))+' body '+gb(ai);
`));
log('shot           :', await shot('convo'));

// ------------------------------------------------------------ sticky scroll
// The thing worth proving: a message arriving while you are scrolled up must
// NOT drag the viewport down. That is behaviour, so it needs a real scroll
// container and a real event - a stub would report success either way.
log('\n--- STICKY SCROLL ---');
log('at bottom      :', await evaluate(`
  const st=document.querySelector('#stream');
  st.scrollTop=st.scrollHeight; st.dispatchEvent(new Event('scroll'));
  await new Promise(r=>setTimeout(r,150));
  return 'jump visible=' + document.querySelector('#jump').classList.contains('on');
`));
log('scrolled up    :', await evaluate(`
  const st=document.querySelector('#stream');
  st.scrollTop=0; st.dispatchEvent(new Event('scroll'));
  await new Promise(r=>setTimeout(r,150));
  return 'jump visible=' + document.querySelector('#jump').classList.contains('on') +
    '  scrollTop=' + Math.round(st.scrollTop);
`));
log('msg while away :', await evaluate(`
  const st=document.querySelector('#stream');
  const before=Math.round(st.scrollTop);
  addMsg('ai','<p>a line arriving while you are reading back</p>');
  await new Promise(r=>setTimeout(r,200));
  const after=Math.round(st.scrollTop);
  return 'scrollTop ' + before + ' -> ' + after +
    (after===before ? '   HELD (correct)' : '   YANKED (bug)');
`));
log('press latest   :', await evaluate(`
  document.querySelector('#jump').click();
  await new Promise(r=>setTimeout(r,300));
  const st=document.querySelector('#stream');
  return 'at bottom=' + (st.scrollHeight-st.scrollTop-st.clientHeight<80) +
    '  jump hidden=' + !document.querySelector('#jump').classList.contains('on');
`));

log('\n--- CONSOLE ---');
log(errs.length ? errs.join('\n') : 'clean');
ws.close(); chrome.kill(); process.exit(errs.length ? 1 : 0);
