// Loads a published VersaTiles Shortbread style and points every remote URL
// (tiles, glyphs, sprites) at the loopback proof server.
import * as maplibregl from '/maplibre/maplibre-gl.mjs';

const origin = location.origin;
const params = new URLSearchParams(location.search);
const styleName = params.get('style') ?? 'colorful';
const status = document.getElementById('status');
const errors = [];

const [style, meta] = await Promise.all([
  fetch(`/styles/${styleName}/en.json`).then((r) => r.json()),
  fetch('/proof/metadata').then((r) => r.json()),
]);
const bounds = meta.bounds.split(',').map(Number);
const [lon, lat, zoom] = meta.center.split(',').map(Number);

style.glyphs = `${origin}/glyphs/{fontstack}/{range}.pbf`;
style.sprite = style.sprite.map((s) => ({ id: s.id, url: `${origin}/sprites/${s.id}` }));
style.projection = { type: 'mercator' };
for (const source of Object.values(style.sources)) {
  if (source.type !== 'vector') continue;
  source.tiles = [`${origin}/tiles/{z}/{x}/{y}.pbf`];
  source.minzoom = Number(meta.minzoom);
  source.maxzoom = Number(meta.maxzoom);
  source.bounds = bounds;
  source.attribution = '© OpenStreetMap contributors (ODbL) · Tiles: Geofabrik Shortbread';
}
// URLs the renderer will load; every one must be on this origin.
const loadable = [style.glyphs, ...style.sprite.map((s) => s.url), ...Object.values(style.sources).flatMap((s) => s.tiles ?? [s.url ?? []])];
const remote = loadable.filter((url) => new URL(url, origin).origin !== origin);

const pad = 0.5;
const map = new maplibregl.Map({
  container: 'map',
  style,
  center: [lon, lat],
  zoom,
  hash: true,
  maxBounds: [[bounds[0] - pad, bounds[1] - pad], [bounds[2] + pad, bounds[3] + pad]],
  attributionControl: { compact: false },
});
map.addControl(new maplibregl.NavigationControl());
map.addControl(new maplibregl.ScaleControl({ unit: 'imperial' }));
map.on('error', (e) => errors.push(String(e.error?.message ?? e.error ?? e)));

const started = performance.now();
let firstIdleMs = null;
map.on('idle', () => {
  firstIdleMs ??= Math.round(performance.now() - started);
  status.textContent = `z${map.getZoom().toFixed(2)} · ${styleName} · first idle ${firstIdleMs} ms · errors ${errors.length}`;
});

// Inspection hook for automated checks.
window.proof = { map, errors, remoteUrlsInStyle: remote, get firstIdleMs() { return firstIdleMs; } };
