// Which free OpenRouter models actually TOOL-CALL under this harness's load?
//
// Written after the free `qwen/qwen3.8-27b:free` went 429 on a shared upstream
// pool and took the whole chain down with it. Two things this measures that a
// one-tool smoke test cannot:
//   1. a realistic toolset - the harness sends ~133 schemas, and a model that
//      tool-calls with one can fall back to prose with a hundred;
//   2. STREAMED, which is what the harness actually does (§12g).
// It also distinguishes "emitted OpenAI tool_calls" from "described a tool call
// in prose/markup" - the second looks like an answer and is useless.
//
//   node dev/freemodels.mjs <openrouter-key> [dashPort] [stream]
//
// Tool names are pulled from a running dashboard when one is up, so the shape
// matches production rather than a guess.
const KEY = process.argv[2];
const STREAM = process.argv[4] === 'stream';
const PORT = process.argv[3] || '7793';

// Real tool names from the running harness, so the shape matches production.
let names = [];
try {
  const d = await fetch(`http://127.0.0.1:${PORT}/api/tools`).then(r => r.json());
  names = (d.tools || []).map(t => t.id);
} catch (_) {}
if (names.length < 20) {
  names = Array.from({ length: 140 }, (_, i) => `filler_tool_${i}`);
}
if (!names.includes('kuzu_graph__graph_stats')) names.unshift('kuzu_graph__graph_stats');

const tools = names.map(n => ({
  type: 'function',
  function: {
    name: n,
    description: n === 'kuzu_graph__graph_stats'
      ? 'Count the entities and relations in the graph database.'
      : `Tool ${n}. Does something unrelated to graphs.`,
    parameters: { type: 'object', properties: { q: { type: 'string' } } },
  },
}));
console.log(`toolset: ${tools.length} tools\n`);

// Every free model that advertises tool support, biggest context first.
// Re-derive with: curl -s https://openrouter.ai/api/v1/models
const MODELS = process.env.MODELS ? process.env.MODELS.split(',') : [
  'deepseek/deepseek-v4-flash-0731:free',
  'nvidia/nemotron-3.5-lightning:free',
  'nvidia/nemotron-3-ultra-550b-a55b:free',
  'dots-studio/dots-3-note-preview:free',
  'inclusionai/ling-3.0-flash-vl:free',
  'inclusionai/ling-3.0-flash-fin:free',
  'nex-agi/nex-n2.5-pro:free',
  'nex-agi/nex-n2.5-mini:free',
  'poolside/laguna-s-2.1:free',
  'poolside/laguna-xs-2.1:free',
  'qwen/qwen3.8-27b:free',
];

for (const model of MODELS) {
  const t0 = Date.now();
  let verdict, detail = '';
  try {
    const res = await fetch('https://openrouter.ai/api/v1/chat/completions', {
      method: 'POST',
      headers: { Authorization: `Bearer ${KEY}`, 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model, stream: STREAM, max_tokens: 300, tools,
        // A persona-sized system prompt, because that is what the harness sends.
        messages: [{ role: 'system', content: 'You are bluee, a personal assistant for Adithya. '.repeat(120) },
                   { role: 'user', content:
          'How many entities and relations are in my graph memory? Call a tool to find out; do not guess.' }],
      }),
      signal: AbortSignal.timeout(90000),
    });
    let body;
    if (STREAM) {
      const raw = await res.text();
      // Reassemble exactly as the harness does: index-keyed fragments.
      const msg = { content: '', tool_calls: [] };
      for (const line of raw.split(/\r?\n/)) {
        if (!line.startsWith('data: ')) continue;
        const p = line.slice(6).trim();
        if (p === '[DONE]') continue;
        let f; try { f = JSON.parse(p); } catch (_) { continue; }
        const d = f.choices?.[0]?.delta; if (!d) continue;
        if (d.content) msg.content += d.content;
        for (const tc of d.tool_calls || []) {
          const i = tc.index ?? 0;
          msg.tool_calls[i] = msg.tool_calls[i] || { function: { name: '', arguments: '' } };
          if (tc.function?.name) msg.tool_calls[i].function.name += tc.function.name;
          if (tc.function?.arguments) msg.tool_calls[i].function.arguments += tc.function.arguments;
        }
      }
      msg.tool_calls = msg.tool_calls.filter(Boolean);
      body = { choices: [{ message: msg }] };
      if (!res.ok) { try { body = JSON.parse(raw); } catch (_) {} }
    } else {
      body = await res.json();
    }
    if (!res.ok) {
      verdict = 'HTTP ' + res.status;
      detail = String(body?.error?.metadata?.raw || body?.error?.message || '').slice(0, 70);
    } else {
      const m = body.choices?.[0]?.message || {};
      const calls = m.tool_calls || [];
      const text = m.content || '';
      const markup = /<[｜|]|<tool_call|<function|```json/i.test(text);
      if (calls.length) {
        verdict = 'TOOL_CALLS';
        detail = calls.map(c => c.function?.name).join(',').slice(0, 60);
      } else if (markup) {
        verdict = 'TEXT MARKUP';       // the deepseek failure mode
        detail = text.replace(/\s+/g, ' ').slice(0, 60);
      } else {
        verdict = 'no call';
        detail = text.replace(/\s+/g, ' ').slice(0, 60);
      }
    }
  } catch (e) {
    verdict = 'ERROR';
    detail = String(e.message).slice(0, 60);
  }
  console.log(`${model.padEnd(42)} ${verdict.padEnd(12)} ${String(Date.now() - t0).padStart(6)}ms  ${detail}`);
}
