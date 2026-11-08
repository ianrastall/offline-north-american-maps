// Loopback-only proof server: serves MapLibre, the bundled style assets and
// vector tiles read directly from an MBTiles file. A strict same-origin CSP
// makes any attempt to reach a remote host fail and get reported to
// /csp-report, which is logged and exposed at /proof/violations.
//
// Usage: node serve.mjs --mbtiles <path> [--port 8765]
import { createServer } from 'node:http';
import { DatabaseSync } from 'node:sqlite';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { dirname, extname, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const args = Object.fromEntries(
  process.argv.slice(2).reduce((pairs, value, i, all) => (i % 2 ? pairs : [...pairs, [value.replace(/^--/, ''), all[i + 1]]]), []),
);
const mbtilesPath = resolve(args.mbtiles ?? join(root, 'data', 'sources', 'illinois-shortbread-1.0.mbtiles'));
const port = Number(args.port ?? 8765);
if (!existsSync(mbtilesPath)) throw new Error(`MBTiles not found: ${mbtilesPath}`);

const db = new DatabaseSync(mbtilesPath, { readOnly: true });
const tileQuery = db.prepare('SELECT tile_data FROM tiles WHERE zoom_level = ? AND tile_column = ? AND tile_row = ?');
const metadata = Object.fromEntries(db.prepare('SELECT name, value FROM metadata').all().map((r) => [r.name, r.value]));

const mounts = {
  '/maplibre/': join(here, 'node_modules', 'maplibre-gl', 'dist'),
  '/styles/': join(root, 'data', 'renderer-assets', 'styles'),
  '/sprites/': join(root, 'data', 'renderer-assets', 'sprites'),
  '/glyphs/': join(root, 'data', 'renderer-assets', 'glyphs'),
  '/': join(here, 'public'),
};
const types = {
  '.html': 'text/html; charset=utf-8', '.mjs': 'text/javascript', '.js': 'text/javascript', '.css': 'text/css',
  '.json': 'application/json', '.png': 'image/png', '.pbf': 'application/x-protobuf', '.map': 'application/json',
};
const csp = [
  "default-src 'self'", "script-src 'self'", "worker-src 'self' blob:", "img-src 'self' data: blob:",
  "style-src 'self' 'unsafe-inline'", "connect-src 'self'", "font-src 'self'", 'report-uri /csp-report',
].join('; ');

const stats = { requests: 0, tiles: 0, emptyTiles: 0, tileBytes: 0, notFound: [], violations: [] };

function send(res, status, body, headers = {}) {
  res.writeHead(status, { 'Content-Security-Policy': csp, 'Cache-Control': 'no-store', ...headers });
  res.end(body);
}

function serveTile(res, z, x, y) {
  if (z < 0 || z > 22 || x < 0 || y < 0 || x >= 2 ** z || y >= 2 ** z) return send(res, 400, 'Bad tile');
  const row = tileQuery.get(z, x, 2 ** z - 1 - y); // MBTiles rows use TMS (south-up) order.
  stats.tiles++;
  if (!row) {
    stats.emptyTiles++;
    return send(res, 204, null);
  }
  const data = Buffer.from(row.tile_data);
  stats.tileBytes += data.length;
  const gzip = data[0] === 0x1f && data[1] === 0x8b;
  send(res, 200, data, { 'Content-Type': 'application/x-protobuf', ...(gzip ? { 'Content-Encoding': 'gzip' } : {}) });
}

function serveGlyphs(res, stack, range) {
  // A fontstack is a comma-separated preference list; use the first available font.
  for (const font of decodeURIComponent(stack).split(',').map((f) => f.trim())) {
    const file = join(mounts['/glyphs/'], font, `${range}.pbf`);
    if (/^[a-z0-9_]+$/.test(font) && /^\d+-\d+$/.test(range) && existsSync(file)) {
      return send(res, 200, readFileSync(file), { 'Content-Type': 'application/x-protobuf' });
    }
  }
  stats.notFound.push(`/glyphs/${stack}/${range}.pbf`);
  send(res, 404, 'No glyphs');
}

function serveStatic(res, pathname) {
  const prefix = Object.keys(mounts).find((p) => pathname.startsWith(p));
  const base = mounts[prefix];
  const relative = decodeURIComponent(pathname.slice(prefix.length)) || 'index.html';
  const file = resolve(base, relative);
  if (!(file + sep).startsWith(base + sep) && file !== base) return send(res, 403, 'Forbidden');
  if (!existsSync(file) || !statSync(file).isFile()) {
    stats.notFound.push(pathname);
    return send(res, 404, 'Not found');
  }
  send(res, 200, readFileSync(file), { 'Content-Type': types[extname(file)] ?? 'application/octet-stream' });
}

createServer((req, res) => {
  stats.requests++;
  const { pathname } = new URL(req.url, 'http://127.0.0.1');
  let m;
  if (req.method === 'POST' && pathname === '/csp-report') {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      stats.violations.push(body);
      console.warn(`CSP violation: ${body}`);
      send(res, 204, null);
    });
    return;
  }
  if ((m = pathname.match(/^\/tiles\/(\d+)\/(\d+)\/(\d+)\.pbf$/))) return serveTile(res, +m[1], +m[2], +m[3]);
  if ((m = pathname.match(/^\/glyphs\/([^/]+)\/([^/]+)\.pbf$/))) return serveGlyphs(res, m[1], m[2]);
  if (pathname === '/proof/metadata') {
    const { json, ...rest } = metadata;
    return send(res, 200, JSON.stringify(rest), { 'Content-Type': 'application/json' });
  }
  if (pathname === '/proof/stats') return send(res, 200, JSON.stringify(stats), { 'Content-Type': 'application/json' });
  serveStatic(res, pathname);
}).listen(port, '127.0.0.1', () => console.log(`Renderer proof: http://127.0.0.1:${port}/ (${mbtilesPath})`));
