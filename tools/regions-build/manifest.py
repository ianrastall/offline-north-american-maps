"""Writes the built-regions manifest that tools/make-regions.py reads (--built), from the
outputs of mexico_states.py and build_shortbread.py and the files published with the
`regions` release.

Usage: python -I tools/regions-build/manifest.py <regions-build dir> <offline-maps-dev.exe> <out.json>

Reads <dir>/mexico/states.json and <dir>/built/built.json (either may be missing). Terrain
sizes come from `offline-maps-dev terrain-size` over each package's box; the header boxes
of Geofabrik extracts (used for Canada and Alaska) from tools/extract-boxes.py's reader.
"""
from __future__ import annotations

import importlib.util
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

RELEASE = "https://github.com/ianrastall/offline-north-american-maps/releases/download/regions"
RELEASE_PAGE = "https://github.com/ianrastall/offline-north-american-maps/releases/tag/regions"
GEOFABRIK = "https://download.geofabrik.de/north-america"

spec = importlib.util.spec_from_file_location("extract_boxes", Path(__file__).resolve().parents[1] / "extract-boxes.py")
extract_boxes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(extract_boxes)
header_bbox = extract_boxes.header_bbox


def package_box(path: Path) -> list[float]:
    db = sqlite3.connect(f"file:{path.as_posix()}?mode=ro", uri=True)
    bounds = db.execute("SELECT value FROM metadata WHERE name = 'bounds'").fetchone()[0]
    db.close()
    return [float(v) for v in bounds.split(",")]


def terrain_bytes(dev: str, box: list[float]) -> int:
    out = subprocess.run([dev, "terrain-size", *(str(v) for v in box)], capture_output=True, text=True, check=True)
    return int(out.stdout.split()[3])


def main() -> None:
    root, dev, out_path = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
    entries = []
    states = root / "mexico" / "states.json"
    if states.exists():
        for s in json.loads(states.read_text(encoding="utf-8")):
            package = root / "mexico" / f"{s['id']}-shortbread-1.0.mbtiles"
            box = package_box(package)
            entries.append({
                "id": s["id"], "name": s["name"], "source_page": RELEASE_PAGE,
                "package_url": f"{RELEASE}/{s['id']}-shortbread-1.0.mbtiles",
                "extras_url": f"{RELEASE}/{s['id']}-latest.osm.pbf",
                "package_box": box, "outline_box": s["outline_box"], "extract_box": s["box"],
                "package_bytes": package.stat().st_size,
                "extras_bytes": (root / "mexico" / f"{s['id']}-latest.osm.pbf").stat().st_size,
                "terrain_bytes": terrain_bytes(dev, box),
            })
            print(entries[-1]["id"], entries[-1]["terrain_bytes"], flush=True)
    built = root / "built" / "built.json"
    if built.exists():
        for b in json.loads(built.read_text(encoding="utf-8")).values():
            package = root / "built" / f"{b['id']}-shortbread-1.0.mbtiles"
            box = package_box(package)
            extras_url = f"{GEOFABRIK}/{b['geofabrik']}-latest.osm.pbf"
            entries.append({
                "id": b["id"], "name": b["name"], "source_page": RELEASE_PAGE,
                "package_url": f"{RELEASE}/{b['id']}-shortbread-1.0.mbtiles",
                "extras_url": extras_url,
                "package_box": box, "outline_box": box, "extract_box": list(header_bbox(extras_url)),
                "package_bytes": package.stat().st_size, "extras_bytes": b["pbf_bytes"],
                "terrain_bytes": terrain_bytes(dev, box),
            })
            print(entries[-1]["id"], entries[-1]["terrain_bytes"], flush=True)
    out_path.write_text(json.dumps(entries, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"{len(entries)} built regions -> {out_path}")


if __name__ == "__main__":
    main()
