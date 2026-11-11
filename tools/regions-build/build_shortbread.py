"""Builds Shortbread map packages for regions Geofabrik publishes no package for: the
Canadian provinces and territories, and Alaska. For publishing with the app's releases
(tag `regions`) as <id>-shortbread-1.0.mbtiles.

Each package is made from Geofabrik's raw extract of the region with Planetiler's bundled
Shortbread profile (`planetiler.jar shortbread`, Planetiler 0.10.2, pinned in
tools/layers): the same layers as Geofabrik's packages, from the same data. Ocean polygons
come from osmdata.openstreetmap.de (water-polygons-split-3857), which Planetiler downloads
once. Alaska is clipped at the 180th meridian (its outline crosses it, which made
Geofabrik's package span the world); the Near Islands west of it are left out.

Stations, addresses and outdoor places for these regions come from Geofabrik's raw
extracts directly, so only the map packages are built here.

The data is (c) OpenStreetMap contributors, ODbL 1.0.

Usage: data/layer-build/venv/Scripts/python tools/regions-build/build_shortbread.py <out-dir> [id ...]
"""
from __future__ import annotations

import hashlib
import json
import sqlite3
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PLANETILER = ROOT / "data" / "layer-build" / "tools" / "planetiler.jar"
PLANETILER_SHA256 = "f310bd0413e2e4512b27f4046d418664e8e1d3bf31603c2a70e23de06c167e4d"
WORK = ROOT / "data" / "regions-build"
GEOFABRIK = "https://download.geofabrik.de/north-america"

# Catalog ID: (name, Geofabrik path, bounds to clip to or None for the extract's own box)
REGIONS = {
    "ca-ab": ("Alberta", "canada/alberta", None),
    "ca-bc": ("British Columbia", "canada/british-columbia", None),
    "ca-mb": ("Manitoba", "canada/manitoba", None),
    "ca-nb": ("New Brunswick", "canada/new-brunswick", None),
    "ca-nl": ("Newfoundland and Labrador", "canada/newfoundland-and-labrador", None),
    "ca-ns": ("Nova Scotia", "canada/nova-scotia", None),
    "ca-nt": ("Northwest Territories", "canada/northwest-territories", None),
    "ca-nu": ("Nunavut", "canada/nunavut", None),
    "ca-on": ("Ontario", "canada/ontario", None),
    "ca-pe": ("Prince Edward Island", "canada/prince-edward-island", None),
    "ca-qc": ("Quebec", "canada/quebec", None),
    "ca-sk": ("Saskatchewan", "canada/saskatchewan", None),
    "ca-yt": ("Yukon", "canada/yukon", None),
    "us-ak": ("Alaska", "us/alaska", (-180.0, 51.0, -129.9, 71.6)),
}


def log(message: str) -> None:
    print(f"{time.strftime('%H:%M:%S')} {message}", flush=True)


def download(url: str, target: Path) -> None:
    """Downloads `url` to `target` unless a complete copy is there (same length)."""
    req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": "offline-us-maps-maintainer", "Cache-Control": "no-cache"})
    with urllib.request.urlopen(req, timeout=60) as r:
        length = int(r.headers["Content-Length"])
    if target.exists() and target.stat().st_size == length:
        return
    log(f"downloading {url} ({length / 1e6:.0f} MB)")
    partial = target.with_suffix(target.suffix + ".part")
    req = urllib.request.Request(url, headers={"User-Agent": "offline-us-maps-maintainer", "Cache-Control": "no-cache"})
    with urllib.request.urlopen(req, timeout=120) as r, open(partial, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)
    if partial.stat().st_size != length:
        raise RuntimeError(f"{url}: got {partial.stat().st_size} of {length} bytes")
    partial.replace(target)


def build(region_id: str, out: Path) -> dict:
    name, path, clip = REGIONS[region_id]
    sources = WORK / "sources"
    sources.mkdir(parents=True, exist_ok=True)
    pbf = sources / f"{Path(path).name}-latest.osm.pbf"
    download(f"{GEOFABRIK}/{path}-latest.osm.pbf", pbf)
    target = out / f"{region_id}-shortbread-1.0.mbtiles"
    args = [
        "java", "-Xmx24g", "-jar", str(PLANETILER), "shortbread",
        f"--osm_path={pbf}", f"--output={target}", "--force",
        "--download", f"--download_dir={sources / 'planetiler'}", f"--tmpdir={WORK / 'tmp'}",
    ]
    if clip:
        args.append("--bounds=" + ",".join(str(v) for v in clip))
    log(f"building {region_id} ({name})")
    started = time.time()
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)
    db = sqlite3.connect(target)
    meta = dict(db.execute("SELECT name, value FROM metadata"))
    updates = {
        "name": f"Shortbread {name}",
        "description": f"{name}: Shortbread vector tiles built with Planetiler from Geofabrik's extract",
        "attribution": "© OpenStreetMap contributors (ODbL)",
        "license": "Open Database License 1.0",
        "author": "OpenStreetMap contributors; built for Offline US Maps",
    }
    for key, value in updates.items():
        db.execute("DELETE FROM metadata WHERE name = ?", (key,))
        db.execute("INSERT INTO metadata (name, value) VALUES (?, ?)", (key, value))
    db.commit()
    tiles = db.execute("SELECT count(*) FROM tiles").fetchone()[0]
    db.close()
    size = target.stat().st_size
    log(f"  {target.name}: {tiles:,} tiles, {size / 1e6:.0f} MB, {time.time() - started:.0f} s, bounds {meta.get('bounds')}")
    return {"id": region_id, "name": name, "geofabrik": path, "bounds": meta.get("bounds"),
            "package_bytes": size, "tiles": tiles, "pbf_bytes": pbf.stat().st_size}


def main() -> None:
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    with PLANETILER.open("rb") as f:
        if hashlib.file_digest(f, "sha256").hexdigest() != PLANETILER_SHA256:
            raise RuntimeError("planetiler.jar does not match the pinned checksum")
    ids = sys.argv[2:] or list(REGIONS)
    listing_path = out / "built.json"
    listing = json.loads(listing_path.read_text(encoding="utf-8")) if listing_path.exists() else {}
    for region_id in ids:
        listing[region_id] = build(region_id, out)
        listing_path.write_text(json.dumps(listing, indent=1) + "\n", encoding="utf-8")
    log("done")


if __name__ == "__main__":
    main()
