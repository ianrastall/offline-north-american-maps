// Map surface hosted by the Offline Maps shell (WebView2). Everything is local:
// the page, MapLibre, styles, sprites and glyphs come from the app folder at
// https://map.local/, tiles and stations from the Rust core through the shell.
//
// Shell -> page messages (JSON): init, theme, units, terrain, show,
// clearSelection, places, zoom, fit, center, reload. Page -> shell: ready,
// view, contextmenu, place, key, idle, error.
//
// Terrain (when installed): Terrarium elevation tiles from the core at
// terrain/{z}/{x}/{y}.webp, drawn as hillshading by MapLibre and as contour
// lines by maplibre-contour, which computes them in a worker from the same
// tiles. Both sit under water, roads and labels.
import * as maplibregl from './maplibre/maplibre-gl.mjs';
import mlcontour from './contour/maplibre-contour.mjs';

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
let units = 'imperial';
// { bounds } of the installed terrain, or null; and what is shown of it.
let terrain = null;
let shading = true;
let contours = true;
let demSource = null;
// True between a style's load and the next setStyle: layers can be added.
let styleReady = false;
const styles = {};
const TERRAIN_BEFORE = 'water-river';
// Short credit on the map; the full one is in Map information.
const TERRAIN_CREDIT = 'Terrain: <span title="Mapterhorn: USGS 3DEP, Copernicus GLO-30 © DLR e.V. 2010–2014 and © Airbus Defence and Space GmbH 2014–2018 provided under COPERNICUS by the European Union and ESA">Mapterhorn, USGS, Copernicus</span>';
const TERRAIN_LAYERS = ['terrain-shading', 'contour-lines', 'contour-labels'];
// Contour spacing by zoom: [minor, major] in feet or metres.
const CONTOUR_FEET = { 10: [200, 1000], 11: [100, 500], 12: [50, 250], 14: [20, 100], 16: [10, 50] };
const CONTOUR_METRES = { 10: [50, 250], 11: [25, 100], 12: [20, 100], 14: [10, 50], 16: [5, 25] };
// Map content colors (terrain), not interface colors.
const TERRAIN_COLORS = {
  light: { shadow: '#5c4a32', highlight: '#ffffff', accent: '#6b5a3e', line: '#8c6d46', label: '#6b5235', halo: '#ffffff' },
  dark: { shadow: '#000000', highlight: '#9c9c9c', accent: '#1a1a1a', line: '#a08a6a', label: '#c9b79a', halo: '#1c1b19' },
};

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

function terrainUrl() {
  return absolute('terrain/{z}/{x}/{y}.webp');
}

// The elevation source is created once; its tiles are shared by the
// hillshading and the contour lines.
function ensureDemSource() {
  if (demSource) return demSource;
  demSource = new mlcontour.DemSource({
    url: terrainUrl(),
    encoding: 'terrarium',
    maxzoom: 12,
    worker: true,
    cacheSize: 100,
    timeoutMs: 10000,
  });
  demSource.setupMaplibre(maplibregl);
  return demSource;
}

function contourUrl() {
  const feet = units !== 'metric';
  return ensureDemSource().contourProtocolUrl({
    multiplier: feet ? 3.28084 : 1,
    thresholds: feet ? CONTOUR_FEET : CONTOUR_METRES,
    elevationKey: 'ele',
    levelKey: 'level',
    contourLayer: 'contours',
    overzoom: 1,
  });
}

// Adds or removes the terrain sources and layers to match `terrain`.
function applyTerrain() {
  if (!map || !styleReady) return;
  for (const id of TERRAIN_LAYERS) if (map.getLayer(id)) map.removeLayer(id);
  for (const id of ['terrain-dem', 'contours']) if (map.getSource(id)) map.removeSource(id);
  if (!terrain) return;
  const b = terrain.bounds;
  const bounds = [b.west, b.south, b.east, b.north];
  const colors = TERRAIN_COLORS[theme] ?? TERRAIN_COLORS.dark;
  const before = map.getLayer(TERRAIN_BEFORE) ? TERRAIN_BEFORE : undefined;
  map.addSource('terrain-dem', {
    type: 'raster-dem',
    tiles: [ensureDemSource().sharedDemProtocolUrl],
    encoding: 'terrarium',
    tileSize: 512,
    maxzoom: 12,
    bounds,
    attribution: TERRAIN_CREDIT,
  });
  // The credit is on both sources, so it shows while either layer is visible.
  map.addSource('contours', { type: 'vector', tiles: [contourUrl()], maxzoom: 15, bounds, attribution: TERRAIN_CREDIT });
  map.addLayer({
    id: 'terrain-shading',
    type: 'hillshade',
    source: 'terrain-dem',
    layout: { visibility: shading ? 'visible' : 'none' },
    paint: {
      'hillshade-shadow-color': colors.shadow,
      'hillshade-highlight-color': colors.highlight,
      'hillshade-accent-color': colors.accent,
      'hillshade-exaggeration': theme === 'dark' ? 0.45 : 0.35,
    },
  }, before);
  map.addLayer({
    id: 'contour-lines',
    type: 'line',
    source: 'contours',
    'source-layer': 'contours',
    minzoom: 10,
    layout: { visibility: contours ? 'visible' : 'none' },
    paint: {
      'line-color': colors.line,
      'line-opacity': ['match', ['get', 'level'], 1, 0.7, 0.4],
      'line-width': ['match', ['get', 'level'], 1, 1.2, 0.6],
    },
  }, before);
  map.addLayer({
    id: 'contour-labels',
    type: 'symbol',
    source: 'contours',
    'source-layer': 'contours',
    minzoom: 12,
    filter: ['>', ['get', 'level'], 0],
    layout: {
      visibility: contours ? 'visible' : 'none',
      'symbol-placement': 'line',
      'text-size': 10,
      'text-field': ['concat', ['number-format', ['get', 'ele'], {}], units === 'metric' ? ' m' : ' ft'],
      'text-font': ['noto_sans_regular'],
      'text-max-angle': 30,
    },
    paint: { 'text-color': colors.label, 'text-halo-color': colors.halo, 'text-halo-width': 1 },
  }, before);
}

function setTerrainVisibility() {
  if (!map) return;
  const show = (id, on) => map.getLayer(id) && map.setLayoutProperty(id, 'visibility', on ? 'visible' : 'none');
  show('terrain-shading', shading);
  show('contour-lines', contours);
  show('contour-labels', contours);
}

function reportView() {
  const c = map.getCenter();
  post({ kind: 'view', latitude: c.lat, longitude: c.lng, zoom: map.getZoom() });
}

// The installed maps' box with half a degree around it.
// How far the view may move: the installed maps' box with room around it in
// both directions, so that "Show whole map" can fit the box in a window of any
// shape. MapLibre will not zoom out past the point where the view would leave
// these bounds, so a tight margin cut off a tall box in a wide window.
function maxBounds() {
  const b = metadata.bounds;
  const cos = Math.cos((((b.south + b.north) / 2) * Math.PI) / 180);
  const width = b.east - b.west;
  const height = b.north - b.south;
  // The box's larger side, in each direction's degrees (east-west degrees are
  // shorter on the ground by cos(latitude)), plus half a degree.
  const padLon = Math.max(width, height / cos) + 0.5;
  const padLat = Math.max(height, width * cos) + 0.5;
  return [
    [Math.max(-180, b.west - padLon), Math.max(-85, b.south - padLat)],
    [Math.min(180, b.east + padLon), Math.min(85, b.north + padLat)],
  ];
}

function wholeMap() {
  const b = metadata.bounds;
  return [[b.west, b.south], [b.east, b.north]];
}

async function init(message) {
  metadata = message.metadata;
  theme = message.theme;
  animate = message.animate;
  units = message.units;
  readTerrain(message);
  applyColors(message.colors);
  const view = message.view;
  map = new maplibregl.Map({
    container: 'map',
    style: await loadStyle(theme),
    // A saved view, or the whole map on first use.
    ...(view
      ? { center: [view.longitude, view.latitude], zoom: view.zoom }
      : { bounds: wholeMap(), fitBoundsOptions: { padding: 24 } }),
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
    styleReady = true;
    Object.keys(SHIELDS).forEach(addShield);
    addChargingIcon();
    applyTerrain();
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

function setUnits(value) {
  units = value;
  if (scale) map.removeControl(scale);
  scale = new maplibregl.ScaleControl({ unit: units === 'metric' ? 'metric' : 'imperial' });
  map.addControl(scale, 'bottom-left');
}

function readTerrain(message) {
  terrain = message.terrain ?? null;
  shading = message.shading ?? shading;
  contours = message.contours ?? contours;
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
      styleReady = false;
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
    if (!map) return;
    setUnits(message.units);
    // Contours are labelled in the new unit.
    applyTerrain();
  },
  terrain(message) {
    shading = message.shading;
    contours = message.contours;
    setTerrainVisibility();
  },
  zoom(message) {
    if (map) move({ zoom: map.getZoom() + message.delta });
  },
  fit() {
    if (!map) return;
    map.fitBounds(wholeMap(), { padding: 24, duration: animate ? 167 : 0 });
  },
  center(message) {
    if (map) move({ center: [message.longitude, message.latitude], zoom: message.zoom ?? map.getZoom() });
  },
  // A region added, updated or removed, or new stations: drop cached tiles
  // and data, and let the view reach the new area.
  async reload(message) {
    if (!map) return init(message);
    metadata = message.metadata;
    readTerrain(message);
    for (const key of Object.keys(styles)) delete styles[key];
    map.setMaxBounds(maxBounds());
    styleReady = false;
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
