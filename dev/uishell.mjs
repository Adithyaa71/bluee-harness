// The calmer shell (dev/plan-canvas.md stage 5), in real Chrome.
//
//   harness dash 7788 &      then      node dev/uishell.mjs 7788 <data dir>
//
// Real key presses: Ctrl+B hides the rail, Ctrl+Shift+B the top bar, the edge
// tabs bring them back, Ctrl+= / - / 0 zoom - and all three survive a reload,
// because they are kept by the server (data/ui.json), not localStorage.
import fs from 'node:fs';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const DATA = process.argv[3];
const { check, fails } = checker();
fs.rmSync(path.join(DATA, 'ui.json'), { force: true });

const { chrome, target } = await launch(PORT, { cdp: 9382, profile: 'bluee-shell' });
let p = await attach(await target(u => !u.includes('only=')));
const ready = () => p.until("return typeof applyZoom === 'function' && document.readyState === 'complete'", 20000);
await ready(); await sleep(600);
const ctrl = async (key, code, vk, shift) => {
  const modifiers = 2 | (shift ? 8 : 0);
  await p.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', key, code, windowsVirtualKeyCode: vk, modifiers });
  await p.send('Input.dispatchKeyEvent', { type: 'keyUp', key, code, windowsVirtualKeyCode: vk, modifiers });
};
const railW = () => p.ev("return Math.round(document.querySelector('#rail').getBoundingClientRect().width)");
const topH = () => p.ev("return Math.round(document.querySelector('#topbar').getBoundingClientRect().height)");

console.log('the plain top bar');
const bar = await p.ev("const t=document.querySelector('#topbar'); return {h:t.getBoundingClientRect().height, meta:getComputedStyle(document.querySelector('#tmeta')).display, vis:document.querySelector('#visbtn').textContent, lines:[...t.querySelectorAll('button')].filter(b=>b.offsetParent).map(b=>Math.round(b.getBoundingClientRect().height))}");
check('one line, nothing wraps', bar.h <= 47 && bar.lines.every(h => h <= 30), JSON.stringify(bar));
check('model/session moved to the tooltip', bar.meta === 'none');
check('vision is one button', /vision: (passive|off|active)/.test(bar.vis), bar.vis);
await p.ev("document.querySelector('#visbtn').click(); return 1");
check('its modes open under it', await p.ev("return getComputedStyle(document.querySelector('#vis')).display!=='none'"));
await p.ev("document.querySelector('#vis button[data-v=\"off\"]').click(); return 1");
await sleep(300);
check('picking one updates the button and closes', await p.ev("return /vision: off/.test(document.querySelector('#visbtn').textContent) && getComputedStyle(document.querySelector('#vis')).display==='none'"));
await p.ev("document.querySelector('#vis button[data-v=\"passive\"]').click(); return 1");
await p.shot('shell-1-plain');

console.log('rail and top bar slide away');
check('rail starts at 56', await railW() === 56);
await ctrl('b', 'KeyB', 66);
await sleep(450);
check('Ctrl+B slides the rail away', await railW() === 0, 'w=' + await railW());
check('an edge tab brings it back', await p.ev("return getComputedStyle(document.querySelector('#railtab')).display!=='none'"));
await ctrl('B', 'KeyB', 66, true);
await sleep(450);
check('Ctrl+Shift+B hides the top bar', await topH() === 0, 'h=' + await topH());
await p.shot('shell-2-hidden');
await sleep(600);

console.log('zoom');
await ctrl('=', 'Equal', 187);
check('Ctrl+= zooms in (110%)', await p.ev("return document.documentElement.style.zoom==='1.1' && /110%/.test(document.querySelector('#zoomtoast').textContent)"));
await ctrl('=', 'Equal', 187);
await sleep(600);
const saved = JSON.parse(fs.readFileSync(path.join(DATA, 'ui.json'), 'utf8'));
check('saved by the server, not localStorage', saved.zoom === 1.2 && saved.rail === false && saved.topbar === false, JSON.stringify(saved));

console.log('survives a reload');
await p.ev("location.reload(); return 1");
await sleep(1500);
p = await attach(await target(u => !u.includes('only=')));
await ready(); await sleep(800);
check('rail still hidden', await railW() === 0);
check('top bar still hidden', await topH() === 0);
check('zoom still 120%', await p.ev("return document.documentElement.style.zoom==='1.2'"));
await ctrl('0', 'Digit0', 48);
check('Ctrl+0 resets', await p.ev("return document.documentElement.style.zoom===''"));
await ctrl('-', 'Minus', 189);
check('Ctrl+- zooms out (90%)', await p.ev("return document.documentElement.style.zoom==='0.9'"));
await ctrl('0', 'Digit0', 48);
await p.ev("document.querySelector('#railtab').click(); document.querySelector('#toptab').click(); return 1");
await sleep(450);
check('edge tabs restore both', await railW() === 56 && await topH() >= 40);
await sleep(600);
check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
