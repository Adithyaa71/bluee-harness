// Canvases (dev/plan-canvas.md stages 1-4), in real Chrome.
//
//   harness dash 7788 &      then      node dev/uicanvas.mjs 7788 <data dir> [--live]
//
// Free unless --live (one cheap model turn with Visual on). Plants a session
// whose reply carries a canvas, then checks: a canvas renders in the pane; a
// click inside it becomes a message; full screen / close / Canvas (N); the
// button exists only for a session that HAS canvases; pop-out; the Visual
// switch is remembered per session; agent windows get all of it.
import fs from 'node:fs';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const DATA = process.argv[3];
const LIVE = process.argv.includes('--live');
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();

const PROG = [
  'root = Card([header, chart, list])',
  'header = CardHeader("Laptop prices", "test canvas")',
  'chart = BarChart(["A", "B", "C"], [s1])',
  's1 = Series("Price", [700, 950, 1200])',
  'list = ListBlock([a, b])',
  'a = ListItem("Pick A", "cheapest", null, null, @ToAssistant("Tell me more about A"))',
  'b = ListItem("Pick B", "middle", null, null, @ToAssistant("Tell me more about B"))',
].join('\n');
const REPLY = 'Here is the comparison.\n```openui\n' + PROG + '\n```\nA is cheapest.';

// A session that made one canvas, written straight into the event log.
const SID = '20261017-120000-cv' + Date.now().toString(16).slice(-6);
const ev = (seq, kind, extra) => JSON.stringify({ seq, session_id: SID, ts: '2026-10-17T12:00:0' + seq + 'Z', kind, ...extra });
fs.writeFileSync(path.join(DATA, 'events', SID + '.jsonl'), [
  ev(1, 'session_start', { model: 'test', persona_files: [] }),
  ev(2, 'user_message', { text: 'compare laptops' }),
  ev(3, 'assistant_message', { text: REPLY }),
].join('\n') + '\n');

const { chrome, target } = await launch(PORT, { cdp: 9378, profile: 'bluee-canvas' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof replyHtml === 'function' && document.readyState === 'complete'", 20000);
await sleep(500);
const sizeSpy = "window.__cvh=0; addEventListener('message', e => { if(e.source===document.querySelector('#cvframe').contentWindow && e.data && e.data.type==='size') window.__cvh=Math.max(window.__cvh, e.data.height); }); return 1";
await p.ev(sizeSpy);

console.log('a new session has no canvases');
check('no Canvas button in a fresh chat', await p.ev("return getComputedStyle(document.querySelector('#cvbtn')).display==='none'"));

console.log('a canvas in a reply');
await p.ev(`liveBody=null; liveHtml=''; renderEvent({type:'reply', text:${JSON.stringify(REPLY)}}); return 1`);
check('the reply shows a card', await p.ev("return !!document.querySelector('#stream .cvcard')"));
check('text around the block stays text', await p.ev("const b=[...document.querySelectorAll('#stream .msg.ai .body')].pop().textContent; return /comparison/.test(b) && /cheapest/.test(b) && !/root =/.test(b)"));
check('the pane opens beside the chat', await p.until("return document.querySelector('#cvpane').classList.contains('on') && document.querySelector('#v-chat').classList.contains('with-canvas')", 4000));
check('it really draws (canvas reported its height)', await p.until("return window.__cvh>150", 15000), 'h=' + await p.ev("return window.__cvh"));
check('Canvas (1) button appears', await p.ev("return /Canvas \\(1\\)/.test(document.querySelector('#cvbtn').textContent) && getComputedStyle(document.querySelector('#cvbtn')).display!=='none'"));
await sleep(800);
await p.shot('canvas-1-pane');

console.log('a click inside the canvas asks bluee');
await p.ev("window.send=()=>{ window.__sent=document.querySelector('#input').value; }; addEventListener('message', e => { if(e.data && e.data.type==='action') window.__act=JSON.stringify(e.data.event); }); return 1");
// The canvas is sandboxed (opaque origin), so Chrome runs it as its own
// out-of-process frame: a separate CDP target, not a child in the page's tree.
const list = await fetch('http://127.0.0.1:9378/json/list').then(r => r.json());
const ct = list.find(t => /\/canvas$/.test(t.url) && t.webSocketDebuggerUrl);
if (ct) {
  const c = await attach(ct);
  const clicked = await c.ev("const el=[...document.querySelectorAll('*')].find(n=>n.children.length===0 && /Pick B/.test(n.textContent)); if(!el) return 'none'; (el.closest('button,[role=button],a,li,div[tabindex]')||el).click(); return 'clicked';");
  check('found the list item in the canvas', clicked === 'clicked', clicked);
  check('the click became a message', await p.until("return window.__sent==='Tell me more about B'", 5000), await p.ev("return (window.__sent||'') + ' | event ' + (window.__act||'')"));
} else check('canvas frame found', false, list.map(t => t.type + ' ' + t.url).join(' | '));

console.log('full screen, close, reopen');
await p.ev("document.querySelector('#cvfull').click(); return 1");
const fs1 = await p.ev("const r=document.querySelector('#cvpane').getBoundingClientRect(); return {w:r.width, h:r.height, vw:innerWidth, vh:innerHeight}");
check('full screen covers the window', fs1.w === fs1.vw && fs1.h === fs1.vh, JSON.stringify(fs1));
await p.key('Escape', 'Escape', 27);
check('Esc leaves full screen', !(await p.ev("return document.querySelector('#cvpane').classList.contains('full')")));
await p.ev("document.querySelector('#cvclose').click(); return 1");
check('close hides the pane', !(await p.ev("return document.querySelector('#cvpane').classList.contains('on')")));
await p.ev("document.querySelector('#cvbtn').click(); return 1");
await sleep(200);
await p.ev("document.querySelector('#connmenu .crow').click(); return 1");
check('Canvas (1) menu reopens it', await p.ev("return document.querySelector('#cvpane').classList.contains('on')"));

console.log('per session');
await p.ev("location.reload(); return 1");
await sleep(1500);
const p2 = await attach(await target(u => !u.includes('only=')));
await p2.until("return typeof openSession === 'function' && document.readyState === 'complete'", 20000);
await sleep(500);
check('after a new chat, no canvases anywhere', await p2.ev("return getComputedStyle(document.querySelector('#cvbtn')).display==='none' && !document.querySelector('.cvcard') && !document.querySelector('#cvpane').classList.contains('on')"));
await p2.ev(`await openSession('${SID}', false); return 1`);
check('reopening the session brings its Canvas (1) back', await p2.until("return /Canvas \\(1\\)/.test(document.querySelector('#cvbtn').textContent) && getComputedStyle(document.querySelector('#cvbtn')).display!=='none'", 5000));
check('replayed canvases do not pop open by themselves', !(await p2.ev("return document.querySelector('#cvpane').classList.contains('on')")));

console.log('pop out');
await p2.ev("currentSession=" + JSON.stringify(SID) + "; openCanvas(0); document.querySelector('#cvpop').click(); return 1");
const pw = await target(u => u.includes('only=canvas'), 30);
if (pw) {
  const w = await attach(pw);
  check('pop-out window shows the canvas', await w.until("return document.querySelector('#cvpane').classList.contains('on') && /Laptop prices/.test(document.title)", 8000));
  const solo = await w.ev("return {stream:getComputedStyle(document.querySelector('#stream')).display, comp:getComputedStyle(document.querySelector('#composer')).display, pane:document.querySelector('#cvpane').getBoundingClientRect().width, vw:innerWidth}");
  check('and only the canvas', solo.stream === 'none' && solo.comp === 'none' && Math.abs(solo.pane - solo.vw) < 2, JSON.stringify(solo));
  await sleep(1200);
  await w.shot('canvas-2-popout');
} else check('pop-out window shows the canvas', false, 'no window');

console.log('the Visual switch is per session');
check('off by default', !(await p2.ev("return visualOn")));
await p2.ev("document.querySelector('#vistog').click(); return 1");
check('on after a click, remembered for this session', await p2.ev(`return visualOn && localStorage.getItem('bluee.visual.${SID}')==='1' && document.querySelector('#vistog').classList.contains('on')`));
await p2.ev("const i=document.querySelector('#input'); i.value='/visual off'; send(); return 1");
check('/visual off turns it off', !(await p2.ev("return visualOn")));
await p2.ev("document.querySelector('#vistog').click(); return 1");
await p2.ev("location.reload(); return 1");
await sleep(1500);
const p3 = await attach(await target(u => !u.includes('only=')));
await p3.until("return typeof openSession === 'function' && document.readyState === 'complete'", 20000);
check('a new chat starts with it off', !(await p3.ev("return visualOn")));
await p3.ev(`await openSession('${SID}', false); return 1`);
check('reopening the session turns it back on', await p3.ev("return visualOn && document.querySelector('#vistog').classList.contains('on')"));

console.log('agent windows');
const ag = (await post('/api/agents', { name: 'drawer', servers: [] })).agent;
const aw = await target(u => u.includes('only=agent') && u.includes('id=' + ag.id), 40);
const a = await attach(aw);
await a.until("return !!document.querySelector('#aghd .nm')", 15000);
check('agent window has the Visual switch', await a.ev("return getComputedStyle(document.querySelector('#vistog')).display!=='none'"));
await a.ev(sizeSpy);
await a.ev(`liveBody=null; liveHtml=''; renderEvent({type:'reply', text:${JSON.stringify(REPLY)}}); return 1`);
check('agent window draws a canvas', await a.until("return window.__cvh>150", 15000));
check('its Canvas button sits in the agent header', await a.ev("return document.querySelector('#cvbtn').parentNode.id==='aghd'"));
await a.shot('canvas-3-agent');
await post('/api/agents/stop', { id: ag.id });

if (LIVE) {
  console.log('live: the model draws one');
  const ws = new WebSocket(B.replace('http', 'ws') + '/ws/chat');
  await new Promise(r => (ws.onopen = r));
  let reply = '';
  const done = new Promise(r => (ws.onmessage = m => { const e = JSON.parse(m.data); if (e.type === 'reply') reply = e.text; if (e.type === 'done') r(); }));
  ws.send(JSON.stringify({ visual: true, message: 'Compare the monthly price of three streaming services (make up round numbers) as a bar chart with a button to ask which is best value. No tools.' }));
  await done; ws.close();
  const m = reply.match(/```openui[^\n]*\n([\s\S]*?)```/);
  check('the model answered with a canvas block', !!m, reply.slice(0, 160));
  if (m) {
    await p3.ev(sizeSpy);
    await p3.ev(`liveBody=null; liveHtml=''; renderEvent({type:'reply', text:${JSON.stringify(reply)}}); return 1`);
    check("and bluee draws the model's canvas", await p3.until("return window.__cvh>150", 15000));
    await sleep(1000);
    await p3.shot('canvas-4-live');
  }
}

for (const e of [p, p2, p3]) if (e.errs.length) check('console clean', false, e.errs.join(' | '));
chrome.kill();
fs.rmSync(path.join(DATA, 'events', SID + '.jsonl'), { force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
