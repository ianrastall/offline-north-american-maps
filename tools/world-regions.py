"""Chooses the regions outside the US, Canada and Mexico that the catalog offers from Geofabrik,
and writes the inputs tools/make-regions.py needs for them.

A region is offered when it has no subregions in Geofabrik's index, Geofabrik publishes a
Shortbread package for it, and its outline does not cross the 180th meridian (Geofabrik builds
those packages as if they spanned the world: Fiji 4.8 GB, Russia's Far East 58 GB on
14 November 2026; they are to be built clipped, as Alaska is). Overlapping outlines (Berlin
inside Brandenburg's, the Azores inside Portugal's) are reported only: the app merges
overlapping regions, as it does All of Mexico with the Mexican states.

Inputs (data/catalog/world/, written beforehand):
  index-v1.json      https://download.geofabrik.de/index-v1.json
  heads.txt          "<id> <status> [<bytes>]" per region: HEAD of its Shortbread package
  pbf.txt            "<id> <bytes>" per region: HEAD of its raw extract

Outputs (same folder):
  selected.json      [{id, url_base, name, country, country_iso, continent, iso}] for make-regions.py
  sizes.txt, pbf-sizes.txt   as make-regions.py reads them
  extract-boxes.txt  header box of each selected raw extract (64 KB range request each)
  boxes.txt          "<id> <west> <south> <east> <north>" outline boxes, for terrain-size

Usage: python -I tools/world-regions.py data/catalog/world [--no-fetch]
"""
import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
NORTH_AMERICA_KEPT = {"greenland"}


def load_extract_boxes():
    spec = importlib.util.spec_from_file_location("extract_boxes", HERE / "extract-boxes.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.header_bbox


def coords(geometry):
    out = []

    def walk(c):
        if isinstance(c[0], (int, float)):
            out.append((c[0], c[1]))
        else:
            for item in c:
                walk(item)

    walk(geometry["coordinates"])
    return out


def main():
    folder = Path(sys.argv[1])
    fetch = "--no-fetch" not in sys.argv
    index = json.loads((folder / "index-v1.json").read_text(encoding="utf-8"))
    features = {f["properties"]["id"]: f for f in index["features"]}
    parents = {f["properties"].get("parent") for f in index["features"]}
    shortbread = {}
    for line in (folder / "heads.txt").read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[1] == "200":
            shortbread[parts[0]] = int(parts[2])
    pbf = {}
    for line in (folder / "pbf.txt").read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) == 2:
            pbf[parts[0]] = int(parts[1])

    def chain(rid):
        out = []
        while rid:
            out.append(features[rid]["properties"])
            rid = features[rid]["properties"].get("parent")
        return out[::-1]  # continent first

    selected, crossing = [], []
    for rid, f in features.items():
        p = f["properties"]
        if rid in parents or rid not in shortbread or rid not in pbf:
            continue
        lineage = chain(rid)
        continent = lineage[0]
        if continent["id"] == "north-america" and rid not in NORTH_AMERICA_KEPT:
            continue
        points = coords(f["geometry"])
        xs = [x for x, _ in points]
        if max(xs) - min(xs) > 180 or (min(xs) < -170 and max(xs) > 170):
            crossing.append((rid, shortbread[rid]))
            continue
        country = lineage[1] if len(lineage) > 1 else lineage[0]
        country_iso = (country.get("iso3166-1:alpha2") or [""])
        iso = (p.get("iso3166-2") or p.get("iso3166-1:alpha2") or [])
        selected.append({
            "id": rid,
            "url_base": p["urls"]["pbf"].removesuffix("-latest.osm.pbf"),
            "name": p["name"],
            "country": country["name"],
            "country_iso": country_iso[0] if len(country_iso) == 1 else "",
            "continent": continent["name"],
            "iso": iso[0] if len(iso) == 1 else "",
            "box": [min(xs), min(y for _, y in points), max(xs), max(y for _, y in points)],
        })

    # Outlines that mostly lie inside another selected region's, reported for review.
    try:
        from shapely.geometry import shape
        shapes = {s["id"]: shape(features[s["id"]]["geometry"]).buffer(0) for s in selected}
        for a in shapes:
            for b in shapes:
                if a != b and shapes[a].area <= shapes[b].area:
                    if shapes[a].intersection(shapes[b]).area > 0.5 * shapes[a].area:
                        print(f"overlap: {a} lies mostly inside {b}")
    except ImportError:
        print("shapely not available: overlap check skipped")

    selected.sort(key=lambda s: (s["continent"], s["country"], s["name"]))
    (folder / "selected.json").write_text(json.dumps(selected, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    (folder / "sizes.txt").write_text("".join(f"{s['id']} {shortbread[s['id']]}\n" for s in selected), encoding="utf-8")
    (folder / "pbf-sizes.txt").write_text("".join(f"{s['id']} {pbf[s['id']]}\n" for s in selected), encoding="utf-8")
    (folder / "boxes.txt").write_text("".join(f"{s['id']} {' '.join(f'{v:.6f}' for v in s['box'])}\n" for s in selected), encoding="utf-8")
    print(f"{len(selected)} regions selected; left out for crossing the 180th meridian: "
          + ", ".join(f"{r} ({b / 1e9:.1f} GB)" for r, b in sorted(crossing)))
    total = sum(shortbread[s["id"]] for s in selected)
    print(f"maps {total / 1e9:.1f} GB, raw extracts {sum(pbf[s['id']] for s in selected) / 1e9:.1f} GB")
    if fetch:
        header_bbox = load_extract_boxes()
        boxes = folder / "extract-boxes.txt"
        done = {line.split()[0] for line in boxes.read_text(encoding="utf-8").splitlines()} if boxes.exists() else set()
        with boxes.open("a", encoding="utf-8") as out:
            for s in selected:
                if s["id"] in done:
                    continue
                w, so, e, n = header_bbox(s["url_base"] + "-latest.osm.pbf")
                out.write(f"{s['id']} {w:.6f} {so:.6f} {e:.6f} {n:.6f}\n")
                out.flush()
                print(f"extract box {s['id']}", flush=True)


if __name__ == "__main__":
    main()
