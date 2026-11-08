// Fetches the pinned, hash-verified style, sprite and glyph assets for the
// renderer proof into data/renderer-assets. Run while online; nothing here is
// needed at render time except the extracted files.
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const downloads = join(root, 'data', 'renderer-downloads');
const assets = join(root, 'data', 'renderer-assets');

export const PINNED = [
  {
    name: 'styles',
    url: 'https://github.com/versatiles-org/versatiles-style/releases/download/v7.0.1/styles.tar.gz',
    sha256: '088bbfb96b0807c1d90e2e1a464f680c458e9ba23341547d37ad6e12c14d6e41',
    license: 'MIT (VersaTiles Style 7.0.1)',
  },
  {
    name: 'sprites',
    url: 'https://github.com/versatiles-org/versatiles-style/releases/download/v7.0.1/sprites.tar.gz',
    sha256: '3dfed482a0c48caa2762ae08badcd1c4b5d4a3901c1508ecc30b73a992f10b65',
    license: 'CC0-1.0 (VersaTiles icon sets and rendered sprites)',
  },
  {
    name: 'glyphs',
    url: 'https://github.com/versatiles-org/versatiles-fonts/releases/download/v3.0.0/noto_sans.tar.gz',
    sha256: 'f58be6be2eb3d127b6ff1c90d828b69ff4807d1d88cd13a43930943fda0e334e',
    license: 'Noto Sans: SIL OFL 1.1; VersaTiles Fonts packaging: MIT',
  },
];

const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');

mkdirSync(downloads, { recursive: true });
for (const item of PINNED) {
  const archive = join(downloads, `${item.name}.tar.gz`);
  let bytes = existsSync(archive) ? readFileSync(archive) : null;
  if (!bytes || sha256(bytes) !== item.sha256) {
    console.log(`Downloading ${item.url}`);
    const response = await fetch(item.url, { signal: AbortSignal.timeout(120_000) });
    if (!response.ok) throw new Error(`${item.url}: HTTP ${response.status}`);
    bytes = Buffer.from(await response.arrayBuffer());
    const actual = sha256(bytes);
    if (actual !== item.sha256) throw new Error(`${item.name}: SHA-256 ${actual} does not match pinned ${item.sha256}`);
    writeFileSync(archive, bytes);
  }
  // Glyphs are extracted in full outside the asset tree, then subset.
  const target = join(item.name === 'glyphs' ? downloads : assets, item.name === 'glyphs' ? 'glyphs-full' : item.name);
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
  // Windows' bundled bsdtar accepts drive-letter paths; GNU tar from Git Bash does not.
  const tarExe = process.platform === 'win32' ? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe') : 'tar';
  const tar = spawnSync(tarExe, ['-xzf', archive, '-C', target], { stdio: 'inherit' });
  if (tar.status !== 0) throw new Error(`${item.name}: tar exited with ${tar.status}`);
  console.log(`${item.name}: verified and extracted (${item.license})`);
}

// Copy only the glyph ranges listed in assets/glyph-ranges.json.
const subset = JSON.parse(readFileSync(join(root, 'assets', 'glyph-ranges.json'), 'utf8'));
const glyphs = join(assets, 'glyphs');
rmSync(glyphs, { recursive: true, force: true });
let glyphBytes = 0;
for (const font of subset.fonts) {
  mkdirSync(join(glyphs, font), { recursive: true });
  for (const range of subset.ranges) {
    const from = join(downloads, 'glyphs-full', font, `${range}.pbf`);
    if (!existsSync(from)) throw new Error(`glyph range ${font}/${range} is not in the font package`);
    copyFileSync(from, join(glyphs, font, `${range}.pbf`));
    glyphBytes += statSync(from).size;
  }
}
console.log(`glyphs: ${subset.fonts.length} fonts × ${subset.ranges.length} ranges, ${(glyphBytes / 1e6).toFixed(1)} MB`);
writeFileSync(join(assets, 'manifest.json'), JSON.stringify(PINNED, null, 2));
