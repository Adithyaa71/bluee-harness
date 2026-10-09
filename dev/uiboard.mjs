// Stage E of dev/plan-workspaces.md: the workspace board, in real Chrome.
//
//   harness dash 7788 &      then      node dev/uiboard.mjs 7788
//
// No model calls. Real mouse events for drag and resize. Checks the board
// builds from the repo, saves to <repo>/.bluee/board.json, keeps live tiles
// alive across the post-turn refresh, pins a page, takes a tile off and back,
// pops the progress tile out, and restores the layout after a full reload.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();

const repo = path.join(os.tmpdir(), 'bluee-board-check-' + Date.now());
fs.mkdirSync(path.join(repo, 'web'), { recursive: true });
fs.mkdirSync(path.join(repo, '.bluee', 'artifacts', 'stats'), { recursive: true });
fs.writeFileSync(path.join(repo, '.bluee', 'plan.md'), '# Board Test\n> checking tiles\n## Phase 1: One\n- [x] a\n- [ ] b\n## Next\n- b\n');
fs.writeFileSync(path.join(repo, '.bluee', 'artifacts', 'stats', 'index.html'), '<!doctype html><body>stats tile</body>');
fs.writeFileSync(path.join(repo, '.bluee', 'artifacts', 'stats', 'meta.json'), JSON.stringify({ id: 'stats', name: 'Stats', topic: 't',
  kind: 'html', entry: 'index.html', created: '2026-10-09T10:00:00Z', updated: '2026-10-09T10:00:00Z', description: '' }));
fs.writeFileSync(path.join(repo, 'web', 'page.html'), '<!doctype html><body>pinned page</body>');
const id = (await post('/api/roots', { path: repo })).id;
const boardFile = () => JSON.parse(fs.readFileSync(path.join(repo, '.bluee', 'board.json'), 'utf8')).tiles;

const { chrome, target } = await launch(PORT, { cdp: 9376, profile: 'bluee-board' });
let p = await attach(await target(u => !u.includes('only=')));
const open = async () => {
  await p.until("return typeof renderBoard === 'function' && document.readyState === 'complete'", 20000);
  await p.ev("document.querySelector('.rb[data-page=\"play\"]').click(); return 1");
  await sleep(500);
  await p.ev(`await loadRoots?.(); const s=document.querySelector('#pgroot'); s.value='${id}'; s.dispatchEvent(new Event('change')); return 1`);
  await p.until("return document.querySelectorAll('#board .tile').length>=2", 8000);
};
await open();
const tiles = () => p.ev("return [...document.querySelectorAll('#board .tile')].map(t=>({id:t.dataset.id, x:t.offsetLeft, y:t.offsetTop, w:t.offsetWidth, h:t.offsetHeight}))");
let t0 = await tiles();
check('board shows progress and the artifact', t0.map(t => t.id).sort().join() === 'art:stats,plan', t0.map(t => t.id).join());
check('progress tile holds the plan', await p.ev("return /Board Test/.test(document.querySelector('.tile.k-plan').textContent)"));
await sleep(700);
check('layout saved into the repo', boardFile().length === 2);

const box = sel => p.ev(`const r=document.querySelector('${sel}').getBoundingClientRect(); return {x:r.left+r.width/2, y:r.top+r.height/2}`);
const drag = async (from, dx, dy) => {
  await p.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: from.x, y: from.y, button: 'left', clickCount: 1 });
  for (let i = 1; i <= 6; i++) await p.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: from.x + dx * i / 6, y: from.y + dy * i / 6, button: 'left' });
  await p.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: from.x + dx, y: from.y + dy, button: 'left', clickCount: 1 });
};

console.log('drag and resize');
const art0 = t0.find(t => t.id === 'art:stats');
await drag(await box('.tile[data-id="art:stats"] .tt'), -120, 260);
const art1 = (await tiles()).find(t => t.id === 'art:stats');
check('dragged by its header (snapped to 10px)', art1.x === Math.max(0, Math.round((art0.x - 120) / 10) * 10) && art1.y === Math.round((art0.y + 260) / 10) * 10, `${art0.x},${art0.y} -> ${art1.x},${art1.y}`);
await drag(await box('.tile[data-id="art:stats"] .rz'), 150, 90);
const art2 = (await tiles()).find(t => t.id === 'art:stats');
check('resized from the corner', art2.w > art1.w + 100 && art2.h > art1.h + 60, `${art1.w}x${art1.h} -> ${art2.w}x${art2.h}`);
await sleep(700);
const saved = boardFile().find(t => t.id === 'art:stats');
check('move and size saved', saved.x === art2.x && saved.w === art2.w, JSON.stringify(saved));
await p.shot('board-1-tiles');

console.log('refresh keeps live tiles');
await p.ev("window.__l=0; document.querySelector('.tile[data-id=\"art:stats\"] iframe').addEventListener('load',()=>window.__l++); await loadPlayground(); return 1");
await sleep(600);
check('a refresh does not reload an unchanged tile', await p.ev("return window.__l") === 0);
check('nor move it', (await tiles()).find(t => t.id === 'art:stats').x === art2.x);

console.log('pin a page');
await p.ev("await openFile('web/page.html'); return 1");
check('pin button shown for a repo page', await p.ev("return getComputedStyle(document.querySelector('#pgpin')).display!=='none'"));
await p.ev("document.querySelector('#pgpin').click(); return 1");
await p.until("return document.querySelector('#pgpin').classList.contains('on')", 5000);
await p.ev("document.querySelector('#pgback').click(); await loadPlayground(); return 1");
check('pinned page is on the board', await p.until("return !!document.querySelector('.tile[data-id=\"file:web/page.html\"] iframe')", 5000));
const fit = await p.ev("const t=document.querySelector('.tile[data-id=\"file:web/page.html\"]'); return {x:t.offsetLeft, w:t.offsetWidth, bw:document.querySelector('#board').clientWidth}");
check('and fits inside the board', fit.x + fit.w <= fit.bw, JSON.stringify(fit));
check('and the rest kept their places', (await tiles()).find(t => t.id === 'art:stats')?.x === art2.x);

console.log('off and back on');
await p.ev("document.querySelector('.tile[data-id=\"art:stats\"] button[data-a=\"off\"]').click(); return 1");
await sleep(700);
await p.ev("await loadPlayground(); return 1");
check('a tile taken off stays off after a refresh', !(await p.ev("return !!document.querySelector('.tile[data-id=\"art:stats\"]')")));
await p.ev("document.querySelector('#bdadd').click(); return 1");
await sleep(200);
const offered = await p.ev("return [...document.querySelectorAll('#connmenu .crow')].map(r=>r.dataset.k+':'+r.dataset.r)");
check('+ tile offers it back', offered.includes('artifact:stats'), offered.join(' '));
await p.ev("document.querySelector('#connmenu .crow[data-r=\"stats\"]').click(); return 1");
check('and puts it back', await p.until("return !!document.querySelector('.tile[data-id=\"art:stats\"]')", 5000));

console.log('shrink to an icon and back');
const full = (await tiles()).find(t => t.id === 'art:stats');
await p.ev("document.querySelector('.tile[data-id=\"art:stats\"] button[data-a=\"min\"]').click(); return 1");
const mini = (await tiles()).find(t => t.id === 'art:stats');
check('shrinks to a small icon', mini.w === 112 && mini.h === 92 && await p.ev("return document.querySelector('.tile[data-id=\"art:stats\"]').classList.contains('mini')"), `${mini.w}x${mini.h}`);
await drag(await box('.tile[data-id="art:stats"] .tt'), 60, 40);
check('the icon can be moved', (await tiles()).find(t => t.id === 'art:stats').x !== mini.x);
check('and stays an icon after a drag', await p.ev("return document.querySelector('.tile[data-id=\"art:stats\"]').classList.contains('mini')"));
await p.shot('board-2-icon');
await sleep(600);
check('icon state saved', boardFile().find(t => t.id === 'art:stats').min === true);
const c = await box('.tile[data-id="art:stats"] .tt');
await p.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: c.x, y: c.y, button: 'left', clickCount: 1 });
await p.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: c.x, y: c.y, button: 'left', clickCount: 1 });
const back = (await tiles()).find(t => t.id === 'art:stats');
check('a click opens it at its old size', back.w === full.w && back.h === full.h, `${back.w}x${back.h}`);

console.log('pop out');
await p.ev("document.querySelector('.tile.k-plan button[data-a=\"pop\"]').click(); return 1");
const pw = await target(u => u.includes('only=plan'), 30);
if (pw) {
  const w = await attach(pw);
  check('progress pops out on its own', await w.until("return /Board Test/.test(document.querySelector('#playbody').textContent)", 6000));
} else check('progress pops out on its own', false, 'no window');

console.log('layout survives a reload');
// Move a tile and reload IMMEDIATELY - inside the save debounce - so the
// pagehide flush is what has to keep it.
await drag(await box('.tile[data-id="art:stats"] .tt'), 0, 40);
const before = await tiles();
await p.ev("location.reload(); return 1");
await sleep(1500);
p = await attach(await target(u => !u.includes('only=') && !u.includes('/files/')));
await open();
const after = await tiles();
const same = before.every(b => { const a = after.find(x => x.id === b.id); return a && a.x === b.x && a.y === b.y && a.w === b.w; });
check('every tile back where it was', same && after.length === before.length, JSON.stringify(after.map(t => [t.id, t.x, t.y])));
check('console clean', p.errs.length === 0, p.errs.join(' | '));

chrome.kill();
await post('/api/roots/remove', { id });
fs.rmSync(repo, { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
