// Per-page probe. Drives one page in a real browser, runs whatever scripted
// interaction that page needs, and reports geometry, contrast and console
// errors. Written so each page's script lives in one place rather than being
// re-typed into ad-hoc one-liners every round.
//
//   node dev/uipage.mjs <port> <page> <label> [--head]
//
// page: memory | graph | play

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.argv[2] || '7788';
const PAGE = process.argv[3] || 'memory';
const LABEL = process.argv[4] || PAGE;
const HEAD = process.argv.includes('--head');
const CHROME = [
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
].find(p => fs.existsSync(p));
if (!CHROME) { console.error('no Chrome or Edge found'); process.exit(2); }

const CDP_PORT = 9337;
const chrome = spawn(CHROME, [
  HEAD ? '--new-window' : '--headless=new',
  `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${path.join(os.tmpdir(), 'bluee-uipage-profile')}`,
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
await evaluate(`document.querySelector('.rb[data-page="${PAGE}"]').click()`);
await sleep(1800);

log(`=== ${PAGE.toUpperCase()} [${LABEL}] ===\n`);

// ------------------------------------------------------------------- memory
if (PAGE === 'memory') {
  log('--- LAYOUT ---');
  log('split       :', await evaluate(`
    const top=document.querySelector('#memtop').getBoundingClientRect();
    const g=document.querySelector('#memgraph').getBoundingClientRect();
    return 'search pane '+Math.round(top.height)+'px / graph '+Math.round(g.height)+'px';
  `));
  log('field       :', await evaluate(`
    const f=document.querySelector('#msearch').getBoundingClientRect();
    return Math.round(f.width)+'x'+Math.round(f.height);
  `));
  log('empty state :', await evaluate(`
    const h=document.querySelector('#mhits');
    return h.children.length===0 ? 'NOTHING - blank area below the field' : h.children.length+' children';
  `));
  log('shot        :', await shot('idle'));

  log('\n--- SEARCH ---');
  await evaluate(`
    const f=document.querySelector('#msearch');
    f.value='append-only event log';
    f.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  `);
  await sleep(2600);
  log('hits        :', await evaluate("return document.querySelectorAll('#mhits .hit').length"));
  log('first hit   :', await evaluate(`
    const h=document.querySelector('#mhits .hit');
    if(!h) return 'none';
    const r=h.getBoundingClientRect();
    const pre=h.querySelector('pre');
    return Math.round(r.width)+'x'+Math.round(r.height)+'  body '+
      (pre?pre.textContent.length:0)+' chars, clamped='+
      (pre?getComputedStyle(pre).webkitLineClamp||'none':'n/a');
  `));
  log('score shown :', await evaluate(`
    const s=document.querySelector('#mhits .hit .sc');
    return s ? s.textContent+'  ('+getComputedStyle(s).color+')' : 'none';
  `));
  log('total px    :', await evaluate(`
    const hs=[...document.querySelectorAll('#mhits .hit')];
    return hs.reduce((a,h)=>a+h.getBoundingClientRect().height,0).toFixed(0)+
      'px of results in a '+Math.round(document.querySelector('#memtop').getBoundingClientRect().height)+'px pane';
  `));
  log('shot        :', await shot('results'));

  // The page is a browser of every tier now, not only a search box. Each
  // tier must list something (or say plainly why not) with no query at all.
  log('\n--- TIERS (no query) ---');
  await evaluate(`document.querySelector('#msearch').value='';`);
  for (const t of ['all', 'recent', 'session', 'code', 'facts']) {
    await evaluate(`document.querySelector('#mtiers button[data-t="${t}"]').click()`);
    await sleep(t === 'facts' ? 1500 : 1200);
    log(`${t.padEnd(8)}    :`, await evaluate(`
      const on=document.querySelector('#mtiers button.on');
      const label=on?on.textContent.trim().replace(/\\s+/g,' '):'?';
      const cards=document.querySelectorAll('#mhits .hit').length;
      const facts=document.querySelectorAll('#mhits .fact').length;
      const count=(document.querySelector('#mhits .mcount')||{}).textContent||'';
      const none=(document.querySelector('#mhits .mnone b')||{}).textContent||'';
      const more=document.querySelector('#mmorebtn');
      return 'chip "'+label+'" | '+(facts?facts+' facts':cards+' cards')+
        (count?' | '+count:'')+(none?' | EMPTY: '+none:'')+(more?' | '+more.textContent:'');
    `));
    if (t === 'all') log('shot        :', await shot('tier-all'));
    if (t === 'all') {
      await evaluate(`document.querySelector('#mmorebtn')?.click()`);
      await sleep(1200);
      log('load more   :', await evaluate("return document.querySelectorAll('#mhits .hit').length+' cards after one click'"));
    }
  }
  await evaluate(`document.querySelector('#mhist').click()`);
  await sleep(1200);
  log('history     :', await evaluate(`
    return document.querySelectorAll('#mhits .fact').length+' facts, '+
      document.querySelectorAll('#mhits .fact.past').length+' marked past';
  `));
  log('shot        :', await shot('tier-facts'));
}

// -------------------------------------------------------------------- graph
if (PAGE === 'graph') {
  log('--- CANVAS ---');
  log('size        :', await evaluate(`
    const c=document.querySelector('#gcv').getBoundingClientRect();
    return Math.round(c.width)+'x'+Math.round(c.height);
  `));
  log('counts      :', await evaluate("return document.querySelector('#ghits')?.textContent||'(none)'"));
  log('legend      :', await evaluate(`
    return [...document.querySelectorAll('#glegend .gk')]
      .map(k=>k.textContent.trim()+(k.classList.contains('off')?' [off]':'')).join(' | ');
  `));
  log('bar controls:', await evaluate(`
    return [...document.querySelectorAll('#gbar > *')]
      .map(e=>e.tagName.toLowerCase()+(e.id?'#'+e.id:'')+' '+Math.round(e.getBoundingClientRect().width)+'px')
      .join(', ');
  `));
  log('shot        :', await shot('idle'));

  log('\n--- SEARCH + PIN ---');
  await evaluate(`
    const f=document.querySelector('#gsearch');
    f.value='Adithya'; f.dispatchEvent(new Event('input',{bubbles:true}));
    f.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
  `);
  await sleep(1200);
  log('after search:', await evaluate("return document.querySelector('#ghits')?.textContent||'(none)'"));
  log('focus card  :', await evaluate(`
    const c=document.querySelector('#gfocus,#gcard,.gfocus');
    return c ? c.textContent.trim().slice(0,80) : 'no focus element found';
  `));
  log('shot        :', await shot('search'));
}

// --------------------------------------------------------------- playground
if (PAGE === 'play') {
  log('--- LAYOUT ---');
  log('panes       :', await evaluate(`
    const g=s=>{const e=document.querySelector(s); if(!e) return s+'=missing';
      const r=e.getBoundingClientRect(); return s+' '+Math.round(r.width)+'x'+Math.round(r.height);};
    return [g('#pgfiles'),g('#pgmain'),g('#pgweb')].join(' | ');
  `));
  log('artifacts   :', await evaluate("return document.querySelectorAll('.acard').length+' cards'"));
  log('empty right :', await evaluate(`
    const b=document.querySelector('#playbody');
    const cards=[...document.querySelectorAll('.acard')];
    if(!cards.length) return 'no cards';
    const last=cards[cards.length-1].getBoundingClientRect();
    const br=b.getBoundingClientRect();
    return Math.round(br.right-last.right)+'px unused to the right, '+
           Math.round(br.bottom-last.bottom)+'px below';
  `));
  log('rename btn  :', await evaluate(`
    const b=document.querySelector('#pgrename');
    if(!b) return 'MISSING';
    const r=b.getBoundingClientRect();
    return 'present '+Math.round(r.width)+'x'+Math.round(r.height)+
      '  onclick='+(b.onclick?'bound':'NOT BOUND')+
      '  listeners=unknown  title="'+b.title+'"';
  `));
  log('shot        :', await shot('idle'));

  log('\n--- RENAME (pencil) ---');
  log('click       :', await evaluate(`
    const before=document.body.innerHTML.length;
    document.querySelector('#pgrename')?.click();
    await new Promise(r=>setTimeout(r,600));
    const inp=document.querySelector('#pgfiles input:not(#pgfilter):not(#pgrootpath)');
    const add=document.querySelector('#pgrootadd');
    return 'dom delta '+(document.body.innerHTML.length-before)+
      ' | rename input '+(inp?'appeared':'NONE')+
      ' | rootadd open='+(add?add.classList.contains('on'):'n/a');
  `));
  log('shot        :', await shot('rename'));

  // Rename on a REAL granted folder. Testing it only on the playground proves
  // the refusal works and nothing else.
  log('\n--- RENAME on a granted folder ---');
  log('switch root :', await evaluate(`
    const sel=document.querySelector('#pgroot');
    const opt=[...sel.options].find(o=>o.value && o.value!=='playground');
    if(!opt) return 'no granted folder to test with';
    sel.value=opt.value; sel.dispatchEvent(new Event('change',{bubbles:true}));
    await new Promise(r=>setTimeout(r,900));
    return 'on "'+opt.textContent.trim()+'" · pencil disabled='+
      document.querySelector('#pgrename').disabled;
  `));
  log('open editor :', await evaluate(`
    const b=document.querySelector('#pgrename');
    if(b.disabled) return 'still disabled - cannot test';
    b.click();
    await new Promise(r=>setTimeout(r,500));
    const inp=[...document.querySelectorAll('#pgfiles input')]
      .find(i=>i.id!=='pgfilter' && i.id!=='pgrootpath');
    return inp ? 'input appeared with value "'+inp.value+'"' : 'NO INPUT - still broken';
  `));
  log('commit      :', await evaluate(`
    const inp=[...document.querySelectorAll('#pgfiles input')]
      .find(i=>i.id!=='pgfilter' && i.id!=='pgrootpath');
    if(!inp) return 'n/a';
    const was=inp.value;
    window.__wasLabel=was;
    inp.value=was+' RENAMED';
    inp.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
    await new Promise(r=>setTimeout(r,1400));
    const sel=document.querySelector('#pgroot');
    const now=[...sel.options].find(o=>o.selected);
    const after=now?now.textContent.trim():'?';
    // Put it back. A check that leaves the machine different every time it runs
    // is not repeatable, and the numbers stop being comparable between rounds.
    await fetch('/api/roots/rename',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({id:sel.value,label:window.__wasLabel})});
    return 'renamed to "'+after+'", restored to "'+window.__wasLabel+'"';
  `));
  log('shot        :', await shot('rename-real'));

  log('\n--- BROWSER PANEL (native) ---');
  await evaluate("document.querySelector('#pgbrowse')?.click()");
  // The first capture starts a real browser from cold, which takes a few
  // seconds exactly once per harness run.
  await sleep(22000);
  log('panel open  :', await evaluate(`
    const w=document.querySelector('#pgweb');
    return w.classList.contains('on')+'  '+Math.round(w.getBoundingClientRect().width)+'px';
  `));
  log('rendered    :', await evaluate(`
    const img=document.querySelector('#webimg');
    if(!img) return 'NO IMAGE: '+(document.querySelector('#webview')||{}).textContent
      .trim().slice(0,180).replace(/\\s+/g,' ');
    return 'image '+img.naturalWidth+'x'+img.naturalHeight+' shown '+
      Math.round(img.getBoundingClientRect().width)+'px';
  `));
  log('navigate    :', await evaluate(`
    const u=document.querySelector('#weburl');
    u.value='example.com';
    u.dispatchEvent(new KeyboardEvent('keydown',{key:'Enter',bubbles:true}));
    await new Promise(r=>setTimeout(r,9000));
    const img=document.querySelector('#webimg');
    return 'url bar="'+u.value+'"  image '+(img?img.naturalWidth+'x'+img.naturalHeight:'none')+
      '  status='+document.querySelector('#webstat').textContent;
  `));
  log('read text   :', await evaluate(`
    document.querySelector('#webtext').click();
    await new Promise(r=>setTimeout(r,3000));
    const pre=document.querySelector('#webview .webtext');
    return pre ? JSON.stringify(pre.textContent.slice(0,70)) : 'no text view';
  `));
  log('shot        :', await shot('browser'));
}

log('\n--- CONSOLE ---');
log(errs.length ? errs.join('\n') : 'clean');
ws.close(); chrome.kill(); process.exit(errs.length ? 1 : 0);
