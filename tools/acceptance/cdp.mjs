// Minimal Chrome DevTools Protocol client for the app's WebView2 (loopback only).
// node cdp.mjs info <out.json> | shot <out.png>
import { writeFileSync } from 'node:fs';

const [, , cmd, out] = process.argv;
const targets = await (await fetch('http://127.0.0.1:9222/json')).json();
const page = targets.find(t => t.type === 'page' && t.url.startsWith('https://map.local/'));
if (!page) { console.error('no map.local page; targets: ' + targets.map(t => t.url).join(', ')); process.exit(2); }

const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let id = 0;
const pending = new Map();
ws.onmessage = ev => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
};
const send = (method, params = {}) => new Promise(res => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });

if (cmd === 'info') {
  const expr = `(() => {
    const res = performance.getEntriesByType('resource').map(e => ({ url: e.name, status: e.responseStatus, type: e.initiatorType }));
    const nav = performance.getEntriesByType('navigation').map(e => ({ url: e.name, status: e.responseStatus }));
    const remote = res.concat(nav).filter(e => !e.url.startsWith('https://map.local/') && !e.url.startsWith('blob:') && !e.url.startsWith('data:'));
    const kinds = {};
    for (const e of res) {
      const p = new URL(e.url).pathname.split('/')[1] || '';
      const k = p + ':' + e.status;
      kinds[k] = (kinds[k] || 0) + 1;
    }
    const canvas = document.querySelector('canvas.maplibregl-canvas');
    const attrib = document.querySelector('.maplibregl-ctrl-attrib-inner');
    const labels = [...document.querySelectorAll('.maplibregl-marker, .maplibregl-popup')].length;
    return { href: location.href, online: navigator.onLine, resources: res.length, byKind: kinds, remote,
      canvas: canvas ? [canvas.width, canvas.height] : null, attribution: attrib ? attrib.innerText : null, markers: labels };
  })()`;
  const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true });
  writeFileSync(out, JSON.stringify(r.result?.result?.value ?? r, null, 2));
} else if (cmd === 'shot') {
  const r = await send('Page.captureScreenshot', { format: 'png' });
  writeFileSync(out, Buffer.from(r.result.data, 'base64'));
}
ws.close();
