// Execute the dashboard's boot path against the REAL running server, with a
// DOM stub. Parsing is not the same as running - this catches runtime errors
// that would leave the page silently stuck on its initial text.
import fs from 'node:fs';

const realSetTimeout = globalThis.setTimeout;   // keep before we stub it

const PORT = process.argv[2];
const JS   = fs.readFileSync(process.argv[3], 'utf8');
const HTML = fs.readFileSync(process.argv[4], 'utf8');

const ids = new Set([...HTML.matchAll(/id="([^"]+)"/g)].map(m => m[1]));
const touched = new Set();
const errors = [];

const ctx2d = new Proxy({}, { get: () => () => {} });
function el(id) {
  return {
    _id: id, textContent: '', innerHTML: '', value: '', src: '', title: '',
    style: new Proxy({}, { get: () => () => {}, set: () => true }),
    dataset: {},
    classList: { add(){}, remove(){}, toggle(){}, contains(){ return false; } },
    addEventListener(){}, focus(){}, click(){}, appendChild(){}, remove(){},
    querySelector(){ return el('inner'); }, querySelectorAll(){ return []; },
    insertAdjacentHTML(){}, getContext(){ return ctx2d; },
    clientWidth: 900, clientHeight: 500, scrollHeight: 100, scrollTop: 0,
    lastElementChild: null, children: [], onclick: null, onchange: null,
  };
}

globalThis.document = {
  querySelector(sel) {
    if (sel.startsWith('#')) {
      const id = sel.slice(1);
      touched.add(id);
      if (!ids.has(id)) return null;          // same as a real browser
      return el(id);
    }
    return el(sel);
  },
  querySelectorAll() { return []; },
  addEventListener() {},
  createElement() { return el('created'); },
};
globalThis.window = globalThis;
globalThis.location = { host: `127.0.0.1:${PORT}`, reload() {} };
globalThis.devicePixelRatio = 1;
try { Object.defineProperty(globalThis,'navigator',{value:{clipboard:{writeText(){}}},configurable:true}); } catch(_) {}
globalThis.WebSocket = class { constructor(){ this.readyState = 0; } send(){} close(){} addEventListener(){} };
globalThis.Terminal = class { constructor(){ this.rows = 24; this.cols = 80; } loadAddon(){} open(){} onData(){} write(){} focus(){} };
globalThis.FitAddon = { FitAddon: class { fit(){} } };
globalThis.ResizeObserver = class { observe(){} };
globalThis.addEventListener = () => {};
globalThis.setInterval = () => 0;
globalThis.requestAnimationFrame = () => 0;
globalThis.setTimeout = (fn) => { try { fn(); } catch (e) { errors.push('setTimeout cb: ' + e.message); } return 0; };

process.on('unhandledRejection', e => errors.push('async: ' + ((e && e.message) || e)));
process.on('uncaughtException',  e => errors.push('uncaught: ' + e.message));

try {
  new Function(JS)();
} catch (e) {
  errors.push('boot threw: ' + e.message);
}

// give the boot fetches real time to hit the server and resolve
await new Promise(r => realSetTimeout(r, 3000));

const missing = [...touched].filter(i => !ids.has(i));
console.log('boot asked for :', [...touched].sort().join(', ') || '(none)');
console.log('missing        :', missing.length ? missing.join(', ') : 'none');
console.log('runtime errors :', errors.length ? errors.join(' | ') : 'none');
console.log('\nVERDICT:', (!missing.length && !errors.length) ? 'BOOT RUNS CLEAN' : 'PROBLEM');
process.exit(missing.length || errors.length ? 1 : 0);
