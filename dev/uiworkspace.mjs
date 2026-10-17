// The Workspace tidy-up (dev/plan-canvas.md stage 6), in real Chrome. Free.
//
//   harness dash 7788 &      then      node dev/uiworkspace.mjs 7788 <data dir>
//
// The rail says Workspace; the title is a switcher that lists folders, grant
// and the topic filter; files start hidden and the choice survives a reload
// (server-side); the browser is a tile on a project board and its own window
// otherwise.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const DATA = process.argv[3];
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();
fs.rmSync(path.join(DATA, 'ui.json'), { force: true });

const repo = path.join(os.tmpdir(), 'bluee-ws-check-' + Date.now());
fs.mkdirSync(path.join(repo, '.bluee'), { recursive: true });
fs.writeFileSync(path.join(repo, '.bluee', 'plan.md'), '# WS Test\n## Phase 1: One\n- [ ] a\n');
const id = (await post('/api/roots', { path: repo })).id;

const { chrome, target } = await launch(PORT, { cdp: 9384, profile: 'bluee-ws' });
let p = await attach(await target(u => !u.includes('only=')));
const ready = () => p.until("return typeof wsPaint === 'function' && document.readyState === 'complete'", 20000);
await ready(); await sleep(500);

check('rail calls it Workspace', await p.ev("return document.querySelector('.rb[data-page=\"play\"]').dataset.tip==='Workspace'"));
await p.ev("document.querySelector('.rb[data-page=\"play\"]').click(); return 1");
await sleep(700);
const bar = await p.ev("return {name:document.querySelector('#wsname').textContent, chips:getComputedStyle(document.querySelector('#topics')).display, files:document.querySelector('#pgfiles').classList.contains('hide'), seg:!!document.querySelector('#playbar .seg')}");
check('title is the switcher, on Playground', bar.name === 'Playground', bar.name);
check('no topic chip row, no files/browser text toggles', bar.chips === 'none' && !bar.seg);
check('files hidden by default', bar.files);
await p.shot('ws-1-bar');

await p.ev("document.querySelector('#wsbtn').click(); return 1");
await sleep(200);
const menu = await p.ev("return [...document.querySelectorAll('#connmenu .crow')].map(r=>r.dataset.k+':'+r.dataset.v)");
check('switcher lists playground, the granted folder and grant', menu.includes('root:playground') && menu.includes('root:' + id) && menu.includes('grant:'), menu.join(' '));
await p.shot('ws-2-menu');
await p.ev(`document.querySelector('#connmenu .crow[data-v="${id}"]').click(); return 1`);
check('picking a folder switches to it', await p.until(`return document.querySelector('#wsname').textContent==='${path.basename(repo)}' && !!document.querySelector('#board .tile.k-plan')`, 8000));

console.log('browser');
await p.ev("document.querySelector('#pgbrowse').click(); return 1");
check('on a project board the browser is a tile', await p.until("return !!document.querySelector('#board .tile.k-browser iframe')", 6000));
check('its frame is bluee\'s browser view', await p.ev("return /only=browser/.test(document.querySelector('.tile.k-browser iframe').src)"));
await sleep(800);
const saved = JSON.parse(fs.readFileSync(path.join(repo, '.bluee', 'board.json'), 'utf8')).tiles;
check('and stays on the board', saved.some(t => t.kind === 'browser'));
await p.ev("const s=document.querySelector('#pgroot'); s.value='playground'; s.dispatchEvent(new Event('change')); return 1");
await sleep(500);
await p.ev("document.querySelector('#pgbrowse').click(); return 1");
check('elsewhere it opens in its own window', !!(await target(u => u.includes('only=browser') && !u.includes('/files/'), 30)));

console.log('files');
await p.ev("document.querySelector('#pgtoggle').click(); return 1");
check('files open from the button', await p.ev("return !document.querySelector('#pgfiles').classList.contains('hide')"));
await sleep(700);
await p.ev("location.reload(); return 1");
await sleep(1500);
p = await attach(await target(u => !u.includes('only=')));
await ready(); await sleep(800);
check('kept open after a reload (server-side)', await p.ev("return !document.querySelector('#pgfiles').classList.contains('hide')"));
await p.ev("document.querySelector('#pgtoggle').click(); return 1");
await sleep(500);
check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
await post('/api/roots/remove', { id });
fs.rmSync(repo, { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
