// Composer pickers (stage 3 of dev/plan-subagents.md), in real Chrome.
//
//   harness dash 7788 &      then      node dev/uipickers.mjs 7788
//
// Real key presses, not handler calls: `/` and `@` menus, filtering, arrows
// (and that the selection scrolls into view), Tab/Enter pick into a chip,
// Backspace removes one, commands only at the start of the main chat. Then two
// cheap model turns: a picked skill reaches the main agent, and a picked
// server is granted to a sub-agent that did not have it.
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { chrome, target } = await launch(PORT, { cdp: 9362, profile: 'bluee-pickers' });
const { check, fails } = checker();

const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof slashMenu === 'function' && document.readyState === 'complete'", 20000);
await sleep(600);
const menu = () => p.ev("const b=document.querySelector('#slash'); return {open:getComputedStyle(b).display!=='none', groups:[...b.querySelectorAll('.shd')].map(x=>x.textContent), items:[...b.querySelectorAll('.sitem')].map(x=>x.dataset.kind+':'+x.dataset.id), sel:b.querySelector('.sitem.sel')?.dataset.id}");
const focus = () => p.ev("const i=document.querySelector('#input'); i.value=''; i.focus(); return 1");

console.log('slash menu');
await focus(); await p.type('/');
await p.until("return getComputedStyle(document.querySelector('#slash')).display!=='none' && document.querySelectorAll('#slash .sitem').length>0", 5000);
let m = await menu();
check('/ opens with commands and skills', m.groups.includes('commands') && m.groups.includes('skills'), m.groups.join(','));
await p.type('graph');
await sleep(300);
m = await menu();
check('typing filters to the graph skill', m.items[0] === 'skill:graph-memory', m.items.slice(0, 3).join(' '));
await p.shot('pickers-1-slash');
await p.key('Tab', 'Tab', 9);
await sleep(200);
let st = await p.ev("return {val:document.querySelector('#input').value, chips:[...document.querySelectorAll('#picks .pk')].map(c=>c.textContent.replace('×','')), open:getComputedStyle(document.querySelector('#slash')).display!=='none'}");
check('Tab picks it into a chip', st.chips.length === 1 && /graph/i.test(st.chips[0]), JSON.stringify(st));
check('token removed from the text', st.val === '', JSON.stringify(st.val));
check('menu closed after picking', !st.open);

console.log('at menu');
await p.type('@kuzu');
await p.until("return document.querySelectorAll('#slash .sitem').length>0", 5000);
m = await menu();
check('@ lists servers and tools', m.groups.includes('servers') && m.groups.includes('tools'), m.groups.join(','));
check('server first for @kuzu', m.items[0] === 'server:kuzu_graph', m.items[0]);
await p.key('Enter', 'Enter', 13);
await sleep(200);
st = await p.ev("return [...document.querySelectorAll('#picks .pk')].map(c=>c.className)");
check('Enter picks the server chip', st.length === 2 && st[1].includes('k-server'), st.join(' | '));

console.log('scrolling');
await p.type('@');
await p.until("return document.querySelectorAll('#slash .sitem').length>20", 5000);
for (let i = 0; i < 25; i++) await p.key('ArrowDown', 'ArrowDown', 40);
const vis = await p.ev("const b=document.querySelector('#slash'), s=b.querySelector('.sitem.sel'); const r=s.getBoundingClientRect(), br=b.getBoundingClientRect(); return {inside: r.top>=br.top-1 && r.bottom<=br.bottom+1, scrolled:b.scrollTop}");
check('arrowed selection stays in view', vis.inside && vis.scrolled > 0, JSON.stringify(vis));
await p.shot('pickers-2-at-scrolled');
await p.key('Escape', 'Escape', 27);
await p.ev("const i=document.querySelector('#input'); i.value=''; i.setSelectionRange(0,0); return 1");

console.log('backspace and mid-text');
await p.key('Backspace', 'Backspace', 8);
st = await p.ev("return document.querySelectorAll('#picks .pk').length");
check('Backspace on empty box removes the last chip', st === 1, 'chips=' + st);
await p.type('please use /gra');
await sleep(400);
m = await menu();
check('/ mid-text offers skills, not commands', m.open && !m.groups.includes('commands') && m.groups.includes('skills'), m.groups.join(','));
await p.key('Escape', 'Escape', 27);

console.log('a picked skill reaches the model');
await p.ev("const i=document.querySelector('#input'); i.value='Which skill did I pick for this message? Answer with its exact name only, no tools.'; i.dispatchEvent(new Event('input')); return 1");
await p.key('Enter', 'Enter', 13);
const got = await p.until("return !busy && [...document.querySelectorAll('#stream .msg.ai .body')].pop()?.textContent.length>0", 90000);
const reply = await p.ev("return [...document.querySelectorAll('#stream .msg.ai .body')].pop()?.textContent||''");
check('model names the picked skill', got && /graph/i.test(reply), reply.slice(0, 120));
check('my bubble shows the chip', await p.ev("return !!document.querySelector('#stream .msg.me .pkline .pk.k-skill')"));
check('chips cleared after sending', await p.ev("return document.querySelectorAll('#picks .pk').length===0"));
const sess = await p.ev("return currentSession");
const evs = await fetch(B + '/api/events?session=' + sess).then(r => r.json());
check('log records the pick', (evs.events || []).some(e => e.kind === 'system' && /picked: skill graph-memory/.test(e.note || '')));

console.log('a picked server is granted to a sub-agent');
const sp = await post('/api/agents', { name: 'picky', purpose: 'picker check', servers: [] });
const wT = await target(u => u.includes('only=agent') && u.includes('id=' + sp.agent.id), 40);
const w = await attach(wT);
await w.until("return !!document.querySelector('#aghd .nm')", 15000);
await w.ev("const i=document.querySelector('#input'); i.value=''; i.focus(); return 1");
await w.type('/');
await w.until("return document.querySelectorAll('#slash .shd').length>0", 5000);
const wm = await w.ev("return [...document.querySelectorAll('#slash .shd')].map(x=>x.textContent)");
check('agent window: / has no commands', !wm.includes('commands') && wm.includes('skills'), wm.join(','));
await w.key('Escape', 'Escape', 27);
await w.ev("const i=document.querySelector('#input'); i.value=''; return 1");
await w.type('@snarevec');
await w.until("return document.querySelector('#slash .sitem')?.dataset.id==='snarevec'", 5000);
await w.key('Enter', 'Enter', 13);
await w.type('Call the snarevec_status tool and tell me its status in one short line.');
await w.key('Enter', 'Enter', 13);
const called = await w.until("return [...document.querySelectorAll('#stream .tool')].some(t=>/snarevec\\.snarevec_status/.test(t.textContent)) && document.querySelector('#aghd').dataset.st!=='running'", 90000);
check('sub-agent used the server it was granted', called,
  await w.ev("return [...document.querySelectorAll('#stream .tool')].map(t=>t.textContent).join(' ')"));
await w.shot('pickers-3-agent-granted');
await post('/api/agents/stop', { id: sp.agent.id });

check('main console clean', p.errs.length === 0, p.errs.join(' | '));
check('agent console clean', w.errs.length === 0, w.errs.join(' | '));
chrome.kill();
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
