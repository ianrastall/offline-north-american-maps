"""Splits Geofabrik's Mexico downloads into one region per state, for publishing with the
app's releases (tag `regions`).

Geofabrik publishes Mexico as a single region. For each of the 32 states this writes:

  mx-<code>-shortbread-1.0.mbtiles  the tiles of Geofabrik's Mexico Shortbread package that
                                    touch the state, at every zoom (unchanged tile data)
  mx-<code>-latest.osm.pbf          the OpenStreetMap objects in the state, for stations,
                                    addresses and outdoor places: nodes inside, ways with a
                                    node inside (with all their nodes), relations with a
                                    member inside; the header box is the state's box

and states.json with each state's ID, name, box and outline box. States are the
OpenStreetMap boundaries with admin_level=4 and an ISO3166-2 code MX-*, widened by
BUFFER degrees so that features on a border are in both neighbours.

The data is (c) OpenStreetMap contributors, ODbL 1.0; Geofabrik made the tiles.

Usage (tools environment of tools/layers, with osmium installed):
  data/layer-build/venv/Scripts/python tools/regions-build/mexico_states.py \
      data/sources/mexico/mexico-shortbread-1.0.mbtiles data/sources/mexico/mexico-latest.osm.pbf \
      data/regions-build/mexico [--batch N]

Extracts are cut BATCH states per pass over the file (default 8): each state's ID tracker
reserves about 1.7 GB for node IDs. Existing outputs are kept, so a run can be resumed.
"""
from __future__ import annotations

import json
import math
import sqlite3
import sys
import time
from pathlib import Path

import numpy as np
import osmium
import shapely
import shapely.geometry
import shapely.prepared
from shapely import wkb

BUFFER = 0.02  # degrees, about 2 km
GRID = 0.1  # degrees, for the node lookup
MAX_ZOOM = 14


def log(message: str) -> None:
    print(f"{time.strftime('%H:%M:%S')} {message}", flush=True)


def state_outlines(pbf: str) -> dict[str, dict]:
    """The admin_level=4 areas with an ISO 3166-2 code MX-*, as shapely geometries."""
    factory = osmium.geom.WKBFactory()
    states: dict[str, dict] = {}
    processor = osmium.FileProcessor(pbf).with_areas(osmium.filter.TagFilter(("boundary", "administrative")))
    for obj in processor:
        if not obj.is_area():
            continue
        tags = obj.tags
        code = tags.get("ISO3166-2", "")
        if tags.get("admin_level") != "4" or not code.startswith("MX-"):
            continue
        try:
            geometry = wkb.loads(factory.create_multipolygon(obj), hex=True)
        except Exception as e:  # an area that cannot be assembled is reported, not used
            log(f"  {code}: no geometry ({e})")
            continue
        states[code] = {
            "code": code,
            "id": "mx-" + code[3:].lower(),
            "name": tags.get("name:en") or tags.get("name", code),
            "name_es": tags.get("name", ""),
            "geometry": geometry,
        }
    return states


class Lookup:
    """Which states (widened) contain a point, through a grid of candidate lists."""

    def __init__(self, states: list[dict]):
        self.states = states
        self.shapes = [shapely.prepared.prep(s["wide"]) for s in states]
        bounds = np.array([s["wide"].bounds for s in states])
        self.west, self.south = bounds[:, 0].min(), bounds[:, 1].min()
        self.cols = int(math.ceil((bounds[:, 2].max() - self.west) / GRID))
        self.rows = int(math.ceil((bounds[:, 3].max() - self.south) / GRID))
        self.cells: dict[int, tuple[list[int], bool]] = {}
        for r in range(self.rows):
            for c in range(self.cols):
                cell = shapely.box(self.west + c * GRID, self.south + r * GRID,
                                   self.west + (c + 1) * GRID, self.south + (r + 1) * GRID)
                hits = [i for i, s in enumerate(states) if s["wide"].intersects(cell)]
                if hits:
                    full = len(hits) == 1 and states[hits[0]]["wide"].contains(cell)
                    self.cells[r * self.cols + c] = (hits, full)

    def find(self, lon: float, lat: float) -> list[int]:
        c = int((lon - self.west) / GRID)
        r = int((lat - self.south) / GRID)
        if c < 0 or r < 0 or c >= self.cols or r >= self.rows:
            return []
        entry = self.cells.get(r * self.cols + c)
        if entry is None:
            return []
        hits, full = entry
        if full:
            return hits
        point = shapely.Point(lon, lat)
        return [i for i in hits if self.shapes[i].contains(point)]


def state_header(state: dict, timestamp: str) -> osmium.io.Header:
    """The state's box and the source's data date (shown as "data of" in the app)."""
    header = osmium.io.Header()
    w, s, e, n = state["wide"].bounds
    header.add_box(osmium.osm.Box(osmium.osm.Location(w, s), osmium.osm.Location(e, n)))
    header.set("generator", "Offline North American Maps tools/regions-build/mexico_states.py")
    if timestamp:
        header.set("osmosis_replication_timestamp", timestamp)
    return header


def source_timestamp(pbf: str) -> str:
    return osmium.io.Reader(pbf, osmium.osm.osm_entity_bits.NOTHING).header().get("osmosis_replication_timestamp", "")


def split_extract(pbf: str, states: list[dict], out: Path) -> None:
    timestamp = source_timestamp(pbf)
    lookup = Lookup(states)
    trackers = [osmium.IdTracker() for _ in states]
    log("pass 1: nodes, ways and relations by state")
    count = 0
    for obj in osmium.FileProcessor(pbf).with_locations():
        count += 1
        if count % 20_000_000 == 0:
            log(f"  {count:,} objects")
        if obj.is_node():
            if obj.location.valid():
                for i in lookup.find(obj.location.lon, obj.location.lat):
                    trackers[i].add_node(obj.id)
        elif obj.is_way():
            nodes = obj.nodes
            if len(nodes) == 0:
                continue
            seen = set()
            for n in (nodes[0], nodes[len(nodes) // 2], nodes[-1]):
                if n.location.valid():
                    seen.update(lookup.find(n.location.lon, n.location.lat))
            for i in seen:
                trackers[i].add_way(obj.id)
                trackers[i].add_references(obj)
        elif obj.is_relation():
            for i, tracker in enumerate(trackers):
                if tracker.contains_any_references(obj):
                    tracker.add_relation(obj.id)
    log(f"pass 1 done: {count:,} objects")
    for state, tracker in zip(states, trackers):
        path = out / f"{state['id']}-latest.osm.pbf"
        writer = osmium.SimpleWriter(str(path), header=state_header(state, timestamp), overwrite=True)
        written = 0
        for obj in osmium.FileProcessor(pbf).with_filter(tracker.id_filter()):
            if obj.is_node():
                writer.add_node(obj)
            elif obj.is_way():
                writer.add_way(obj)
            else:
                writer.add_relation(obj)
            written += 1
        writer.close()
        state["pbf_bytes"] = path.stat().st_size
        log(f"  {path.name}: {written:,} objects, {state['pbf_bytes'] / 1e6:.1f} MB")


def tile_bounds(z: int, x: np.ndarray, y: np.ndarray):
    n = 2.0 ** z
    west = x / n * 360.0 - 180.0
    east = (x + 1) / n * 360.0 - 180.0
    north = np.degrees(np.arctan(np.sinh(np.pi * (1 - 2 * y / n))))
    south = np.degrees(np.arctan(np.sinh(np.pi * (1 - 2 * (y + 1) / n))))
    return west, south, east, north


def tile_range(z: int, w: float, s: float, e: float, n: float):
    count = 2 ** z

    def tx(lon):
        return min(count - 1, max(0, int((lon + 180.0) / 360.0 * count)))

    def ty(lat):
        lat = max(-85.05112878, min(85.05112878, lat))
        r = math.radians(lat)
        return min(count - 1, max(0, int((1 - math.asinh(math.tan(r)) / math.pi) / 2 * count)))

    return tx(w), tx(e), ty(n), ty(s)


def split_tiles(mbtiles: str, states: list[dict], out: Path) -> None:
    states = [s for s in states if not (out / f"{s['id']}-shortbread-1.0.mbtiles").exists()]
    source = sqlite3.connect(f"file:{mbtiles}?mode=ro", uri=True)
    metadata = dict(source.execute("SELECT name, value FROM metadata"))
    encoding = source.execute("PRAGMA encoding").fetchone()[0]
    source.close()
    for state in states:
        path = out / f"{state['id']}-shortbread-1.0.mbtiles"
        path.unlink(missing_ok=True)
        db = sqlite3.connect(path)
        # ATTACH needs the same text encoding as the source.
        db.execute(f"PRAGMA encoding = '{encoding}'")
        db.executescript(
            "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;"
            "CREATE TABLE metadata (name TEXT, value TEXT);"
            "CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);"
            "CREATE TEMP TABLE wanted (z INTEGER, x INTEGER, y INTEGER);")
        db.execute(f"ATTACH DATABASE '{Path(mbtiles).as_posix()}' AS src")
        shape = state["wide"]
        w, s, e, n = shape.bounds
        for z in range(MAX_ZOOM + 1):
            x0, x1, y0, y1 = tile_range(z, w, s, e, n)
            xs, ys = np.meshgrid(np.arange(x0, x1 + 1), np.arange(y0, y1 + 1))
            xs, ys = xs.ravel(), ys.ravel()
            tw, ts, te, tn = tile_bounds(z, xs, ys)
            boxes = shapely.box(tw, ts, te, tn)
            hit = shapely.intersects(shape, boxes)
            rows = [(z, int(x), int((2 ** z - 1) - y)) for x, y in zip(xs[hit], ys[hit])]
            db.executemany("INSERT INTO wanted VALUES (?, ?, ?)", rows)
        db.execute(
            "INSERT INTO tiles SELECT t.zoom_level, t.tile_column, t.tile_row, t.tile_data "
            "FROM wanted w JOIN src.tiles t ON t.zoom_level = w.z AND t.tile_column = w.x AND t.tile_row = w.y")
        meta = dict(metadata)
        meta.update({
            "name": f"Shortbread {state['name']}",
            "bounds": f"{w:.6f},{s:.6f},{e:.6f},{n:.6f}",
            "center": f"{(w + e) / 2:.6f},{(s + n) / 2:.6f},7",
            "description": f"{state['name']}: tiles of Geofabrik's Mexico Shortbread package that touch the state",
            "attribution": "© OpenStreetMap contributors (ODbL); tiles by Geofabrik",
            "license": "Open Database License 1.0",
        })
        db.executemany("INSERT INTO metadata VALUES (?, ?)", meta.items())
        db.execute("CREATE UNIQUE INDEX tile_index ON tiles (zoom_level, tile_column, tile_row)")
        db.commit()
        tiles = db.execute("SELECT count(*) FROM tiles").fetchone()[0]
        db.execute("DETACH DATABASE src")
        db.close()
        sqlite3.connect(path).execute("VACUUM").connection.close()
        state["package_bytes"] = path.stat().st_size
        state["tiles"] = tiles
        log(f"  {path.name}: {tiles:,} tiles, {state['package_bytes'] / 1e6:.1f} MB")


def main() -> None:
    args = sys.argv[1:]
    batch = 8
    if "--batch" in args:
        i = args.index("--batch")
        batch = int(args[i + 1])
        del args[i:i + 2]
    mbtiles, pbf, out_dir = args[:3]
    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    cache = out / "outlines.json"
    if cache.exists():
        log("reading state outlines from outlines.json")
        found = json.loads(cache.read_text(encoding="utf-8"))
        for state in found.values():
            state["geometry"] = wkb.loads(state["geometry"], hex=True)
    else:
        log("reading state outlines")
        found = state_outlines(pbf)
        cache.write_text(json.dumps({k: {**v, "geometry": v["geometry"].wkb_hex} for k, v in found.items()}), encoding="utf-8")
    states = sorted(found.values(), key=lambda s: s["name"])
    log(f"{len(states)} states: {', '.join(s['code'] for s in states)}")
    if len(states) != 32:
        raise SystemExit("expected 32 states")
    for state in states:
        state["wide"] = state["geometry"].buffer(BUFFER)
    split_tiles(mbtiles, states, out)
    todo = [s for s in states if not (out / f"{s['id']}-latest.osm.pbf").exists()]
    for start in range(0, len(todo), batch):
        part = todo[start:start + batch]
        log(f"extracts for {', '.join(s['id'] for s in part)}")
        split_extract(pbf, part, out)
    for state in states:
        state["package_bytes"] = (out / f"{state['id']}-shortbread-1.0.mbtiles").stat().st_size
        state["pbf_bytes"] = (out / f"{state['id']}-latest.osm.pbf").stat().st_size
        db = sqlite3.connect(out / f"{state['id']}-shortbread-1.0.mbtiles")
        state["tiles"] = db.execute("SELECT count(*) FROM tiles").fetchone()[0]
        db.close()
    listing = [{
        "id": s["id"], "code": s["code"], "name": s["name"], "name_es": s["name_es"],
        "outline_box": [round(v, 6) for v in s["geometry"].bounds],
        "box": [round(v, 6) for v in s["wide"].bounds],
        "package_bytes": s["package_bytes"], "pbf_bytes": s["pbf_bytes"], "tiles": s["tiles"],
    } for s in states]
    (out / "states.json").write_text(json.dumps(listing, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    shapes = {"type": "FeatureCollection", "features": [
        {"type": "Feature", "properties": {"id": s["id"], "name": s["name"]},
         "geometry": shapely.geometry.mapping(s["geometry"].simplify(0.005))} for s in states]}
    (out / "states.geojson").write_text(json.dumps(shapes), encoding="utf-8")
    log("done")


if __name__ == "__main__":
    main()
