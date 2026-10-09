// Playground bug fixes, in real Chrome. No model calls.
//
//   harness dash 7788 &      then      node dev/uiplaybugs.mjs 7788 <data dir>
//
// Plants two artifacts with the SAME name in different topics, then checks:
// an open artifact survives the refresh that runs after every turn, and
// reloads when bluee changes it; pop-out opens the one you were looking at;
// and an artifact opened in its own tab cannot reach bluee's API.
import fs from 'node:fs';
import path from 'node:path';
import { launch, attach, sleep, checker } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const DATA = process.argv[3];
if (!DATA) { console.error('usage: node dev/uiplaybugs.mjs <port> <data dir>'); process.exit(2); }
const B = `http://127.0.0.1:${PORT}`;
const { check, fails } = checker();

function plant(id, topic, body, updated) {
  const dir = path.join(DATA, 'artifacts', id);
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, 'index.html'), `<!doctype html><title>${id}</title><body>${body}</body>`);
  fs.writeFileSync(path.join(dir, 'meta.json'), JSON.stringify({
    id, name: 'Chart', topic, kind: 'html', entry: 'index.html',
    created: '2026-10-09T10:00:00Z', updated, description: 'bug check',
  }));
}
plant('zz-chart-a', 'trading', 'version one', '2026-10-09T10:00:00Z');
plant('zz-chart-b', 'crypto', 'the other chart', '2026-10-09T10:00:00Z');

const { chrome, target } = await launch(PORT, { cdp: 9370, profile: 'bluee-playbugs' });
const p = await attach(await target(u => !u.includes('only=')));
await p.until("return typeof loadPlayground === 'function' && document.readyState === 'complete'", 20000);
await p.ev("document.querySelector('.rb[data-page=\"play\"]').click(); return 1");
await p.until("return !!document.querySelector('.acard[data-id=\"zz-chart-a\"]')", 8000);

console.log('an open artifact survives a refresh');
await p.ev("window.__loads=0; document.querySelector('#aframe').addEventListener('load',()=>window.__loads++); openArtifact('zz-chart-a'); return 1");
await p.until("return window.__loads>=1", 5000);
await p.ev("await loadPlayground(); return 1");
await sleep(300);
check('still open after the post-turn refresh', await p.ev("return document.querySelector('#aview').classList.contains('on')"));
check('and not reloaded when nothing changed', await p.ev("return window.__loads") === 1);

plant('zz-chart-a', 'trading', 'version two', '2026-10-09T11:00:00Z');
await p.ev("await loadPlayground(); return 1");
check('reloads when bluee changed it', await p.until("return window.__loads===2", 5000));
await p.shot('playbugs-1-open');

console.log('pop-out picks the right one');
await p.ev("openArtifact('zz-chart-b'); document.querySelector('#apop').click(); return 1");
const pop = await target(u => u.includes('only=artifact'), 30);
check('pop-out opened chart-b, not the first "Chart"', !!pop && pop.url.includes('zz-chart-b'), pop?.url);

console.log('an artifact on its own cannot reach bluee');
const res = await fetch(B + '/artifacts/zz-chart-a/');
check('served with a sandbox policy', /sandbox/.test(res.headers.get('content-security-policy') || ''), res.headers.get('content-security-policy'));
await p.ev(`window.open('${B}/artifacts/zz-chart-a/', '_blank'); return 1`);
const tab = await target(u => u.endsWith('/artifacts/zz-chart-a/'), 30);
const t = await attach(tab);
await t.until("return document.readyState === 'complete'", 5000);
const probe = await t.ev("try { const r = await fetch('/api/stats'); await r.json(); return 'reachable'; } catch (e) { return 'blocked'; }");
check('its script cannot read bluee\'s API', probe === 'blocked', probe);
check('but the page itself still renders', /version two/.test(await t.ev("return document.body.textContent")));

check('console clean', p.errs.length === 0, p.errs.join(' | '));
chrome.kill();
for (const id of ['zz-chart-a', 'zz-chart-b']) fs.rmSync(path.join(DATA, 'artifacts', id), { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED: ${fails.join(', ')}` : '\nall passed');
process.exit(fails.length ? 1 : 0);
