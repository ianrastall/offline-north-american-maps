// Map surface hosted by the Offline Maps shell (WebView2). Everything is local:
// the page, MapLibre, styles, sprites and glyphs come from the app folder at
// https://map.local/, tiles and stations from the Rust core through the shell.
//
// Shell -> page messages (JSON): init, theme, units, show, clearSelection,
// places, zoom, fit, center, reload. Page -> shell: ready, view, contextmenu,
// place, key, idle, error.
import * as maplibregl from './maplibre/maplibre-gl.mjs';

const host = window.chrome?.webview;
const post = (message) => host?.postMessage(message);
const origin = location.origin;
const absolute = (path) => new URL(path, origin).href.replaceAll('%7B', '{').replaceAll('%7D', '}');

let map = null;
let theme = 'dark';
let animate = true;
let selection = null;
let placeMarkers = [];
let metadata = null;
let scale = null;
const styles = {};

async function loadStyle(name) {
  if (!styles[name]) {
    const style = await fetch(`styles/offline-maps-${name}.json`).then((r) => r.json());
    style.glyphs = absolute(style.glyphs);
    style.sprite = style.sprite.map((s) => ({ ...s, url: absolute(s.url) }));
    for (const source of Object.values(style.sources)) {
      if (source.type === 'geojson') source.data = absolute(source.data);
      if (source.type !== 'vector') continue;
      source.tiles = source.tiles.map(absolute);
      source.minzoom = metadata.minZoom;
      source.maxzoom = metadata.maxZoom;
      const b = metadata.bounds;
      source.bounds = [b.west, b.south, b.east, b.north];
    }
    styles[name] = style;
  }
  // A fresh copy: MapLibre may keep references to the object it is given.
  return structuredClone(styles[name]);
}

function applyColors(colors) {
  if (!colors) return;
  const root = document.documentElement.style;
  for (const [key, value] of Object.entries(colors)) root.setProperty(`--${key}`, value);
  document.documentElement.style.colorScheme = theme;
}

// Route shields: stretchable rounded markers. Interstate: blue with red cap;
// US: white with black border; Illinois: white square-cornered; other: grey.
// These are map content (road signage), not interface colors.
const SHIELDS = {
  'route-interstate': { fill: '#1d4f9c', border: '#ffffff', cap: '#c8102e', radius: 6 },
  'route-us': { fill: '#ffffff', border: '#111111', radius: 6 },
  'route-state': { fill: '#ffffff', border: '#111111', radius: 1 },
  'route-other': { fill: '#eeeeee', border: '#777777', radius: 3 },
};

function addShield(id) {
  const spec = SHIELDS[id];
  if (!spec || map.hasImage(id)) return;
  const ratio = 2, w = 24 * ratio, h = 20 * ratio, line = 1.5 * ratio, r = spec.radius * ratio;
  const ctx = new OffscreenCanvas(w, h).getContext('2d');
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

// Charging stations: green disc with a white lightning bolt (map content).
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

function reportView() {
  const c = map.getCenter();
  post({ kind: 'view', latitude: c.lat, longitude: c.lng, zoom: map.getZoom() });
}

// The installed maps' box with half a degree around it.
function maxBounds() {
  const b = metadata.bounds;
  const pad = 0.5;
  return [[b.west - pad, b.south - pad], [b.east + pad, b.north + pad]];
}

async function init(message) {
  metadata = message.metadata;
  theme = message.theme;
  animate = message.animate;
  applyColors(message.colors);
  const b = metadata.bounds;
  const view = message.view;
  map = new maplibregl.Map({
    container: 'map',
    style: await loadStyle(theme),
    center: view ? [view.longitude, view.latitude] : [(b.west + b.east) / 2, (b.south + b.north) / 2],
    zoom: view ? view.zoom : 7,
    minZoom: Math.max(0, metadata.minZoom),
    maxZoom: 19,
    maxBounds: maxBounds(),
    attributionControl: { compact: false },
    pitchWithRotate: false,
    dragRotate: false,
    touchPitch: false,
  });
  map.touchZoomRotate.disableRotation();
  setUnits(message.units);
  map.getCanvas().setAttribute('aria-label', 'Map. Arrow keys move, plus and minus zoom.');
  map.on('style.load', () => {
    Object.keys(SHIELDS).forEach(addShield);
    addChargingIcon();
  });
  map.on('styleimagemissing', ({ id }) => (id === 'station-charging' ? addChargingIcon() : addShield(id)));
  map.on('moveend', reportView);
  map.on('error', (e) => post({ kind: 'error', message: String(e.error?.message ?? e.error ?? e) }));
  map.on('contextmenu', (e) => {
    post({ kind: 'contextmenu', latitude: e.lngLat.lat, longitude: e.lngLat.lng, x: e.point.x, y: e.point.y });
  });
  map.once('idle', () => {
    reportView();
    post({ kind: 'idle' });
  });
}

function setUnits(units) {
  if (scale) map.removeControl(scale);
  scale = new maplibregl.ScaleControl({ unit: units === 'metric' ? 'metric' : 'imperial' });
  map.addControl(scale, 'bottom-left');
}

function move(options) {
  map.easeTo({ ...options, duration: animate ? 167 : 0 });
}

const handlers = {
  init,
  async theme(message) {
    applyColors(message.colors);
    animate = message.animate;
    if (message.theme !== theme && map) {
      theme = message.theme;
      map.setStyle(await loadStyle(theme), { diff: false });
    }
  },
  show(message) {
    if (!map) return;
    selection?.remove();
    const element = document.createElement('div');
    element.className = 'marker-selected';
    element.title = message.label ?? '';
    selection = new maplibregl.Marker({ element }).setLngLat([message.longitude, message.latitude]).addTo(map);
    // The item's own zoom: a town is shown whole, an address at street level.
    move({ center: [message.longitude, message.latitude], zoom: message.zoom ?? 15 });
  },
  clearSelection() {
    selection?.remove();
    selection = null;
  },
  places(message) {
    if (!map) return;
    placeMarkers.forEach((m) => m.remove());
    placeMarkers = message.items.map((p) => {
      const element = document.createElement('div');
      element.className = p.covered ? 'marker-place' : 'marker-place outside';
      element.title = p.name;
      element.addEventListener('click', (e) => {
        e.stopPropagation();
        post({ kind: 'place', id: p.id });
      });
      return new maplibregl.Marker({ element }).setLngLat([p.longitude, p.latitude]).addTo(map);
    });
  },
  units(message) {
    if (map) setUnits(message.units);
  },
  zoom(message) {
    if (map) move({ zoom: map.getZoom() + message.delta });
  },
  fit() {
    if (!map) return;
    const b = metadata.bounds;
    map.fitBounds([[b.west, b.south], [b.east, b.north]], { padding: 24, duration: animate ? 167 : 0 });
  },
  center(message) {
    if (map) move({ center: [message.longitude, message.latitude], zoom: message.zoom ?? map.getZoom() });
  },
  // A region added, updated or removed, or new stations: drop cached tiles
  // and data, and let the view reach the new area.
  async reload(message) {
    if (!map) return init(message);
    metadata = message.metadata;
    for (const key of Object.keys(styles)) delete styles[key];
    map.setMaxBounds(maxBounds());
    map.setStyle(await loadStyle(theme), { diff: false });
  },
};

host?.addEventListener('message', (event) => {
  const message = event.data;
  const handler = handlers[message?.kind];
  if (handler) {
    Promise.resolve(handler(message)).catch((e) => post({ kind: 'error', message: String(e) }));
  }
});

window.addEventListener('contextmenu', (e) => e.preventDefault());

// Shortcuts the shell owns; the web view does not pass them to the window's menus.
const SHORTCUTS = new Set(['Ctrl+F', 'Ctrl+,', 'Ctrl+B', 'Ctrl+D', 'Ctrl+Shift+C', 'Ctrl+0', 'F11']);
window.addEventListener('keydown', (e) => {
  const key = `${e.ctrlKey ? 'Ctrl+' : ''}${e.shiftKey && e.ctrlKey ? 'Shift+' : ''}${e.key.length === 1 ? e.key.toUpperCase() : e.key}`;
  if (SHORTCUTS.has(key)) {
    e.preventDefault();
    post({ kind: 'key', key });
  } else if (e.ctrlKey && (e.key === '=' || e.key === '+' || e.key === '-')) {
    e.preventDefault();
    handlers.zoom({ delta: e.key === '-' ? -1 : 1 });
  }
});
post({ kind: 'ready' });
