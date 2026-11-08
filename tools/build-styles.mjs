// Builds the Offline Maps light and dark map styles from the pinned
// VersaTiles 7.0.1 "colorful" styles (MIT). Run `node proof/renderer/fetch-assets.mjs`
// first. Output: assets/styles/offline-maps-{light,dark}.json.
//
// Changes from the source styles:
// - every tile, glyph and sprite URL is a same-origin path;
// - Mercator projection, no sky;
// - OSM/Geofabrik attribution only;
// - the zoom-14 motorway ref text is replaced by route shields: interstates
//   from zoom 10, US/Illinois routes on trunk/primary roads from zoom 12 and
//   secondary roads from zoom 13 (the zooms at which the Shortbread tiles
//   carry refs). Shield images are drawn by the map page on demand.
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const sourceDir = join(root, 'data', 'renderer-assets', 'styles');
const outDir = join(root, 'assets', 'styles');
const SOURCE = 'offline-maps';
export const ATTRIBUTION = '© OpenStreetMap contributors (ODbL) · Geofabrik';

// First line of a possibly multi-line ref ("I 74\nUS 51" -> "I 74").
const firstRef = ['let', 'r', ['get', 'ref'],
  ['let', 'i', ['index-of', '\n', ['var', 'r']],
    ['case', ['>=', ['var', 'i'], 0], ['slice', ['var', 'r'], 0, ['var', 'i']], ['var', 'r']]]];

// Network from the ref prefix; refs with extra words ("I 55 Business") use the plain marker.
function shieldExpression(output) {
  const f = ['var', 'f'];
  const rest = (n) => ['slice', f, n];
  const simple = (n) => ['<', ['index-of', ' ', rest(n)], 0];
  const network = ['case',
    ['all', ['==', ['slice', f, 0, 2], 'I '], simple(2)], 'interstate',
    ['all', ['==', ['slice', f, 0, 3], 'US '], simple(3)], 'us',
    ['all', ['==', ['slice', f, 0, 3], 'IL '], simple(3)], 'state',
    'other'];
  const text = ['case',
    ['==', ['var', 'n'], 'interstate'], rest(2),
    ['any', ['==', ['var', 'n'], 'us'], ['==', ['var', 'n'], 'state']], rest(3),
    f];
  const body = {
    image: ['concat', 'route-', ['var', 'n']],
    text,
    color: ['case', ['==', ['var', 'n'], 'interstate'], '#ffffff', '#111111'],
  }[output];
  return ['let', 'f', firstRef, ['let', 'n', network, body]];
}

function shieldLayer(id, kinds, minzoom) {
  return {
    id,
    type: 'symbol',
    source: SOURCE,
    'source-layer': 'street_labels',
    minzoom,
    filter: ['all', ['has', 'ref'], ['in', ['get', 'kind'], ['literal', kinds]]],
    layout: {
      'symbol-placement': 'line',
      'symbol-spacing': 320,
      'text-field': shieldExpression('text'),
      'text-font': ['noto_sans_bold'],
      'text-size': ['interpolate', ['linear'], ['zoom'], 10, 10, 14, 11, 18, 13],
      'text-rotation-alignment': 'viewport',
      'text-pitch-alignment': 'viewport',
      'text-max-width': 20,
      'icon-image': shieldExpression('image'),
      'icon-rotation-alignment': 'viewport',
      'icon-text-fit': 'both',
      'icon-text-fit-padding': [2, 4, 2, 4],
    },
    paint: { 'text-color': shieldExpression('color') },
  };
}

function build(sourceName, outName, title) {
  const sourcePath = join(sourceDir, sourceName, 'en.json');
  const raw = readFileSync(sourcePath);
  const style = JSON.parse(raw);
  const [oldSource] = Object.keys(style.sources);

  style.name = title;
  style.metadata = {
    'offline-maps:derived-from': `VersaTiles Style 7.0.1 ${sourceName}/en.json (MIT)`,
    'offline-maps:source-sha256': createHash('sha256').update(raw).digest('hex'),
  };
  style.glyphs = '/glyphs/{fontstack}/{range}.pbf';
  style.sprite = [{ id: 'base', url: '/sprites/base' }];
  style.projection = { type: 'mercator' };
  delete style.sky;
  style.sources = {
    [SOURCE]: {
      type: 'vector',
      tiles: ['/tiles/{z}/{x}/{y}.pbf'],
      minzoom: 0,
      maxzoom: 14,
      attribution: ATTRIBUTION,
    },
  };
  style.layers = style.layers
    .filter((layer) => layer.id !== 'label-motorway-shield')
    .map((layer) => (layer.source === oldSource ? { ...layer, source: SOURCE } : layer));
  style.layers.push(
    shieldLayer('route-shield-secondary', ['secondary'], 13),
    shieldLayer('route-shield-major', ['trunk', 'primary'], 12),
    shieldLayer('route-shield-motorway', ['motorway'], 10),
  );

  const text = JSON.stringify(style);
  const remote = text.match(/[a-z]+:\/\/[^"]*/gi);
  if (remote) throw new Error(`${outName}: remote URLs remain: ${remote.join(', ')}`);
  const fonts = new Set(style.layers.flatMap((l) => l.layout?.['text-font'] ?? []));
  for (const font of fonts) {
    if (!['noto_sans_regular', 'noto_sans_bold'].includes(font)) throw new Error(`unexpected font ${font}`);
  }
  mkdirSync(outDir, { recursive: true });
  writeFileSync(join(outDir, `${outName}.json`), JSON.stringify(style, null, 1) + '\n');
  console.log(`${outName}.json: ${style.layers.length} layers, fonts ${[...fonts].join(', ')}`);
}

build('colorful', 'offline-maps-light', 'Offline Maps light');
build('colorful-dark', 'offline-maps-dark', 'Offline Maps dark');
