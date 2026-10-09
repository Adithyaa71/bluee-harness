// Stage A of dev/plan-workspaces.md, live: bluee creates, edits and reads a
// file in the playground; a sub-agent's relative path lands in its own folder.
//
//   harness dash 7788 &      then      node dev/livewrite.mjs 7788 <data dir>
import fs from 'node:fs';
import path from 'node:path';
import { checker, sleep } from './cdp.mjs';

const PORT = process.argv[2] || '7788';
const DATA = process.argv[3];
const B = `http://127.0.0.1:${PORT}`;
const post = (p, b) => fetch(B + p, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
const get = p => fetch(B + p).then(r => r.json());
const { check, fails } = checker();
const play = path.join(DATA, 'artifacts');
fs.rmSync(path.join(play, 'notes-check'), { recursive: true, force: true });

function turn(text) {
  return new Promise(resolve => {
    const ws = new WebSocket(B.replace('http', 'ws') + '/ws/chat');
    const ev = [];
    ws.onopen = () => ws.send(JSON.stringify({ message: text }));
    ws.onmessage = m => { const e = JSON.parse(m.data); ev.push(e); if (e.type === 'done') { ws.close(); resolve(ev); } };
  });
}

const ev = await turn('In the playground folder: use write_file to create notes-check/hello.md containing exactly "# Hello" then a blank line then "line two". ' +
  'Then use edit_file to change "line two" to "line 2". Then read_file it and reply with its content only.');
const tools = ev.filter(e => e.type === 'tool_call').map(e => e.tool);
check('used write_file then edit_file', tools.includes('write_file') && tools.includes('edit_file'), tools.join(','));
const f = path.join(play, 'notes-check', 'hello.md');
const body = fs.existsSync(f) ? fs.readFileSync(f, 'utf8') : '(missing)';
check('file exists with the edit applied', /# Hello/.test(body) && /line 2/.test(body) && !/line two/.test(body), JSON.stringify(body));

const a = (await post('/api/agents', { name: 'writer', servers: [] })).agent;
await post('/api/agents/say', { id: a.id, text: 'Use write_file with path "hello.txt" (no root) and content "hi from writer". Reply done.' });
for (let i = 0; i < 120; i++) { const x = (await get('/api/agents')).agents.find(z => z.id === a.id); if (x.turns > 0 && x.status !== 'running') break; await sleep(500); }
const own = path.join(play, 'agents', 'writer', 'hello.txt');
check("sub-agent's file is in its own folder", fs.existsSync(own) && fs.readFileSync(own, 'utf8') === 'hi from writer', own);
await post('/api/agents/stop', { id: a.id });
fs.rmSync(path.join(play, 'notes-check'), { recursive: true, force: true });
console.log(fails.length ? `\n${fails.length} FAILED` : '\nall passed');
process.exit(fails.length ? 1 : 0);
