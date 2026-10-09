// Stage B of dev/plan-workspaces.md: live previews inside granted folders.
//
//   harness dash 7788 &      then      node dev/uirepoview.mjs 7788
//
// No model calls. Makes a throwaway "repo" in %TEMP% with a page that loads
// its own CSS and JS, plus one artifact in its .bluee/artifacts; grants it;
// then checks the page renders live (assets included), stays sandboxed, the
// repo's artifact card appears when the folder is selected, and pop-out
// opens the right page. Revokes the grant at the end.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const { check, fails } = checker();

const repo = path.join(os.tmpdir(), 'bluee-repo-check-' + Date.now());
fs.mkdirSync(path.join(repo, 'web'), { recursive: true });
fs.writeFileSync(path.join(repo, 'web', 'index.html'),
  '<!doctype html><link rel="stylesheet" href="style.css"><body><h1 id="h">repo page</h1><script src="app.js"></script></body>');
fs.writeFileSync(path.join(repo, 'web', 'style.css'), 'h1{color:rgb(1, 2, 3)}');
fs.writeFileSync(path.join(repo, 'web', 'app.js'), "document.getElementById('h').dataset.js='ran';");
const art = path.join(repo, '.bluee', 'artifacts', 'progress');
fs.mkdirSync(art, { recursive: true });
fs.writeFileSync(path.join(art, 'index.html'), '<!doctype html><body>repo artifact</body>');
fs.writeFileSync(path.join(art, 'meta.json'), JSON.stringify({ id: 'progress', name: 'Progress', topic: 'saas',
  kind: 'html', entry: 'index.html', created: '2026-10-09T10:00:00Z', updated: '2026-10-09T10:00:00Z', description: 'repo check' }));

const g = await post('/api/roots', { path: repo });
const id = g.root?.id || g.id;
check('folder granted', !!id, JSON.stringify(g).slice(0, 120));

const page = await fetch(`${B}/files/${id}/web/index.html`);
check('repo page served', page.ok && /repo page/.test(await page.text()));
check('and sandboxed', /sandbox/.test(page.headers.get('content-security-policy') || ''));
const esc = await fetch(`${B}/files/${id}/..%2F..%2Fwindows%2Fwin.ini`);
check('cannot escape the folder', !esc.ok, String(esc.status));
const list = await fetch(`${B}/api/artifacts?root=${id}`).then(r => r.json());
check("repo's artifacts listed with their url", list.artifacts?.[0]?.url === `/files/${id}/.bluee/artifacts/progress/index.html`, JSON.stringify(list.artifacts?.[0]?.url));

const { chrome, target } = await launch(PORT, { cdp: 9372, profile: 'bluee-repoview' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof loadFiles === 'function' && document.readyState === 'complete'", 20000);
await p.ev("document.querySelector('.rb[data-page=\"play\"]').click(); return 1");
await sleep(800);
await p.ev(`const s=document.querySelector('#pgroot'); await loadRoots?.(); s.value='${id}'; s.dispatchEvent(new Event('change')); return 1`);
// Since stage E a repo opens as a board: its artifact is a tile, not a card.
const card = await p.until("return !!document.querySelector('.tile[data-id=\"art:progress\"] iframe')", 8000);
check('selecting the repo shows its artifact on the board', card);

await p.ev("await openFile('web/index.html'); return 1");
await sleep(1200);
const fr = await p.ev("const f=document.querySelector('#pgframe'); return {src:f.src, shown:f.style.display!=='none', code:document.querySelector('#pgcodewrap').classList.contains('on')}");
check('a repo .html file renders live, not as source', fr.shown && !fr.code && fr.src.includes(`/files/${id}/web/index.html`), JSON.stringify(fr));
await p.shot('repoview-1-live');

// Look inside the rendered page: an iframe is not its own CDP target, so open
// the same URL top-level (sandboxed there too) and check CSS and JS both ran.
await p.ev(`window.open('${B}/files/${id}/web/index.html', '_blank'); return 1`);
const inner = await target(u => u.endsWith(`/files/${id}/web/index.html`), 20);
if (inner) {
  const f = await attach(inner);
  await f.until("return document.readyState==='complete'", 5000);
  const r = await f.ev("const h=document.getElementById('h'); return {color:getComputedStyle(h).color, js:h.dataset.js}");
  check('its own CSS and JS loaded', r.color === 'rgb(1, 2, 3)' && r.js === 'ran', JSON.stringify(r));
} else check('its own CSS and JS loaded', false, 'frame target not found');

await p.ev("document.querySelector('#pgpop').click(); return 1");
const pop = await target(u => u.includes('only=file'), 30);
check('pop-out opens that page in its own window', !!pop && pop.url.includes(encodeURIComponent('web')) || pop?.url.includes('web/index.html'), pop?.url);
if (pop) {
  const w = await attach(pop);
  await w.until("return document.querySelector('#aframe')?.src.includes('/files/')", 5000);
  check('and shows it there', await w.ev("return document.querySelector('#aframe').src.includes('web/index.html')"));
  await w.shot('repoview-2-popout');
}

check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
await post('/api/roots/remove', { id });
fs.rmSync(repo, { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
