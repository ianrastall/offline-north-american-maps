// Offline North American Maps renderer page. Loads a bundled Offline North American Maps style and resolves
// its same-origin paths; draws route shield and charging-station images.
import * as maplibregl from '/maplibre/maplibre-gl.mjs';

const origin = location.origin;
const params = new URLSearchParams(location.search);
const theme = params.get('theme') === 'dark' ? 'dark' : 'light';
const status = document.getElementById('status');
const errors = [];

const [style, { metadata }] = await Promise.all([
  fetch(`/styles/offline-maps-${theme}.json`).then((r) => r.json()),
  fetch('/proof/metadata').then((r) => r.json()),
]);
const absolute = (path) => new URL(path, origin).href.replaceAll('%7B', '{').replaceAll('%7D', '}');
const b = metadata.bounds;

style.glyphs = absolute(style.glyphs);
style.sprite = style.sprite.map((s) => ({ ...s, url: absolute(s.url) }));
for (const source of Object.values(style.sources)) {
  if (source.type === 'geojson') source.data = absolute(source.data);
  if (source.type !== 'vector') continue;
  source.tiles = source.tiles.map(absolute);
  source.minzoom = metadata.min_zoom;
  source.maxzoom = metadata.max_zoom;
  source.bounds = [b.west, b.south, b.east, b.north];
}
const loadable = [
  style.glyphs,
  ...style.sprite.map((s) => s.url),
  ...Object.values(style.sources).flatMap((s) => s.tiles ?? (typeof s.data === 'string' ? [s.data] : [])),
];
const remote = loadable.filter((url) => new URL(url.replace(/[{}]/g, '')).origin !== origin);

const pad = 0.5;
const map = new maplibregl.Map({
  container: 'map',
  style,
  center: [(b.west + b.east) / 2, (b.south + b.north) / 2],
  zoom: 7,
  hash: true,
  maxBounds: [[b.west - pad, b.south - pad], [b.east + pad, b.north + pad]],
  attributionControl: { compact: false },
});
map.addControl(new maplibregl.NavigationControl());
map.addControl(new maplibregl.ScaleControl({ unit: 'imperial' }));
map.on('error', (e) => errors.push(String(e.error?.message ?? e.error ?? e)));

// Route shields: stretchable rounded markers. Interstate: blue with red cap;
// US: white with black border; Illinois: white square-cornered; other: grey.
const SHIELDS = {
  'route-interstate': { fill: '#1d4f9c', border: '#ffffff', cap: '#c8102e', radius: 6 },
  'route-us': { fill: '#ffffff', border: '#111111', radius: 6 },
  'route-state': { fill: '#ffffff', border: '#111111', radius: 1 },
  'route-other': { fill: '#eeeeee', border: '#777777', radius: 3 },
};
// Registered as soon as the style is parsed, before any tile is laid out, so
// no shield is placed as bare text; the missing-image event is a fallback.
map.on('style.load', () => {
  Object.keys(SHIELDS).forEach(addShield);
  addChargingIcon();
});
map.on('styleimagemissing', ({ id }) => (id === 'station-charging' ? addChargingIcon() : addShield(id)));

// Charging stations: green disc with a white lightning bolt.
function addChargingIcon() {
  if (map.hasImage('station-charging')) return;
  const ratio = 2, size = 32 * ratio;
  const ctx = new OffscreenCanvas(size, size).getContext('2d');
  ctx.beginPath();
  ctx.arc(size / 2, size / 2, size / 2 - 2 * ratio, 0, Math.PI * 2);
  ctx.fillStyle = '#15803d';
  ctx.fill();
  ctx.lineWidth = 2 * ratio;
  ctx.strokeStyle = '#ffffff';
  ctx.stroke();
  const p = (x, y) => [x * ratio, y * ratio];
  ctx.beginPath();
  ctx.moveTo(...p(18, 6));
  [p(9, 18), p(15, 18), p(13, 26), p(23, 13), p(17, 13), p(19, 6)].forEach((pt) => ctx.lineTo(...pt));
  ctx.closePath();
  ctx.fillStyle = '#ffffff';
  ctx.fill();
  map.addImage('station-charging', ctx.getImageData(0, 0, size, size), { pixelRatio: ratio });
}

function addShield(id) {
  const spec = SHIELDS[id];
  if (!spec || map.hasImage(id)) return;
  const ratio = 2, w = 24 * ratio, h = 20 * ratio, line = 1.5 * ratio, r = spec.radius * ratio;
  const canvas = new OffscreenCanvas(w, h);
  const ctx = canvas.getContext('2d');
  const shape = () => { ctx.beginPath(); ctx.roundRect(line / 2, line / 2, w - line, h - line, r); };
  shape();
  ctx.fillStyle = spec.fill;
  ctx.fill();
  if (spec.cap) {
    ctx.save();
    shape();
    ctx.clip();
    ctx.fillStyle = spec.cap;
    ctx.fillRect(0, 0, w, 5 * ratio);
    ctx.restore();
  }
  shape();
  ctx.lineWidth = line;
  ctx.strokeStyle = spec.border;
  ctx.stroke();
  const top = (spec.cap ? 6 : 3) * ratio;
  map.addImage(id, ctx.getImageData(0, 0, w, h), {
    pixelRatio: ratio,
    stretchX: [[8 * ratio, 16 * ratio]],
    stretchY: [[top + ratio, h - 4 * ratio]],
    content: [4 * ratio, top, w - 4 * ratio, h - 3 * ratio],
  });
}

const started = performance.now();
let firstIdleMs = null;
map.on('idle', () => {
  firstIdleMs ??= Math.round(performance.now() - started);
  status.textContent = `z${map.getZoom().toFixed(2)} · ${theme} · first idle ${firstIdleMs} ms · errors ${errors.length}`;
});

// Inspection hook for automated checks.
window.proof = { map, errors, remoteUrlsInStyle: remote, get firstIdleMs() { return firstIdleMs; } };
