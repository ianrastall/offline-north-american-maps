"""Writes crates/offline-maps-core/src/regions_data.rs, the catalog of regions: the US states
and territories, Canada and Mexico (with --built), and the regions elsewhere that Geofabrik
publishes a Shortbread package for (with --world).

Inputs (downloaded separately; nothing here touches the network):
  index-v1.json   https://download.geofabrik.de/index-v1.json (region outlines)
  sizes.txt       "<slug> <bytes>" per line: HEAD Content-Length of <slug>-shortbread-1.0.mbtiles
  pbf-sizes.txt   "<slug> <bytes>" per line: HEAD Content-Length of <slug>-latest.osm.pbf
                  (offline-maps-dev probe <url> reports both)
  overlays-summary.json (optional) data/layer-build/out/summary.json from
                  tools/layers/build_overlays.py; regions without a built overlay file get 0
  terrain-sizes.txt "<slug> <bytes>" per line: bytes of Mapterhorn's planet.pmtiles tiles,
                  zoom 0-12, touching the region's outline box (offline-maps-dev
                  terrain-size, which reads the archive's index only)
  extract-boxes.txt "<slug> <west> <south> <east> <north>" per line: the header box of each
                  raw extract (tools/extract-boxes.py)

  built.json      (optional, --built) regions whose packages are built for the app and published
                  with its releases (tools/regions-build): a list of {id, name, source_page,
                  package_url, extras_url, package_box, outline_box, extract_box, package_bytes,
                  extras_bytes, terrain_bytes}; boxes are [west, south, east, north]
  world folder    (optional, --world) the output of tools/world-regions.py: selected.json and
                  that folder's sizes.txt, pbf-sizes.txt, terrain-sizes.txt and extract-boxes.txt

Usage: python -I tools/make-regions.py index-v1.json sizes.txt pbf-sizes.txt terrain-sizes.txt extract-boxes.txt <date YYYY-MM-DD> [overlays-summary.json] [--built built.json] [--world folder]
       then cargo fmt

IDs: ISO 3166-2 for US states, Canadian provinces and Mexican states (us-il, ca-on, mx-que),
ISO 3166-1 for Mexico as a whole (mx). Elsewhere a region's single ISO 3166-2 or 3166-1 code
from Geofabrik's index (de-hh, fr), else its country's code and Geofabrik's ID (in-central-zone),
else Geofabrik's ID (gcc-states). A code already taken falls back to the next form.

Each region's coverage check is its outline's bounding box (Geofabrik's package
bounds equal it): the package must lie within the box widened by OUTER_MARGIN
degrees and must cover five points inside it. Geofabrik cuts the raw extracts with
boundaries that can reach further than the index outline (16 states and Mexico on
11 November 2026), so the outer box also takes in each extract's header box. Illinois keeps its hand-written
entry in package.rs. Alaska is left out: its outline crosses the 180th meridian,
so its package spans the whole width of the world (29.6 GB on 8 November 2026).
Mexico is one region (Geofabrik does not split it), with the ID mx.
"""
import json
import sys
from pathlib import Path

OUTER_MARGIN = 0.5
SKIP = {"alaska"}
HANDWRITTEN = {"illinois": "crate::package::ILLINOIS"}
BASE = "https://download.geofabrik.de/north-america/us"
# Countries outside the US, by Geofabrik ID: (base URL, catalog ID, name).
COUNTRIES = {"mexico": ("https://download.geofabrik.de/north-america", "mx", "All of Mexico")}
# Countries (ISO 3166-1) whose names are mostly written in scripts the map cannot draw yet
# (bundled glyphs are Latin, Greek and Cyrillic; Arabic and Hebrew need right-to-left text),
# and Geofabrik's multi-country regions that are.
NON_LATIN = {
    "AE", "AF", "AM", "BD", "BH", "BT", "CN", "DZ", "EG", "ER", "ET", "GE", "HK", "IL", "IN",
    "IQ", "IR", "JO", "JP", "KH", "KP", "KR", "KW", "LA", "LB", "LK", "LY", "MA", "MM", "MO",
    "MR", "MV", "NP", "OM", "PK", "PS", "QA", "SA", "SD", "SY", "TH", "TN", "TW", "YE",
}
NON_LATIN_IDS = {"gcc-states", "israel-and-palestine"}
NOTE = ("Street names written in this country's script are not shown yet; towns and countries "
        "show their English names where OpenStreetMap has them.")
# Geofabrik's top-level areas, as continents in the region list.
CONTINENTS = {"Russian Federation": "Europe"}


def north_american_country(code):
    return {"us": "United States", "ca": "Canada", "mx": "Mexico"}[code.split("-")[0]]



def bbox(geometry):
    xs, ys = [], []

    def walk(c):
        if isinstance(c[0], (int, float)):
            xs.append(c[0])
            ys.append(c[1])
        else:
            for item in c:
                walk(item)

    walk(geometry["coordinates"])
    return min(xs), min(ys), max(xs), max(ys)


def display_name(slug):
    if slug == "us-virgin-islands":
        return "US Virgin Islands"
    words = slug.split("-")
    return " ".join(w if w == "of" else w.capitalize() for w in words)


def sizes(path):
    out = {}
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        slug, value = line.split()
        out[slug] = int(value)
    return out


def main():
    args = sys.argv[1:]
    built = []
    if "--built" in args:
        i = args.index("--built")
        built = json.loads(Path(args[i + 1]).read_text(encoding="utf-8"))
        del args[i:i + 2]
    world_dir = None
    if "--world" in args:
        i = args.index("--world")
        world_dir = Path(args[i + 1])
        del args[i:i + 2]
    index_path, shortbread_path, pbf_path, terrain_path, boxes_path, date = args[:6]
    extract_boxes = {}
    for line in Path(boxes_path).read_text(encoding="utf-8").splitlines():
        slug, *values = line.split()
        extract_boxes[slug] = tuple(float(v) for v in values)
    overlays = {}
    if len(args) > 6:
        summary = json.loads(Path(args[6]).read_text(encoding="utf-8"))
        overlays = {k: v.get("bytes", 0) for k, v in summary.items()}
    index = json.loads(Path(index_path).read_text(encoding="utf-8"))
    shortbread, pbf, terrain = sizes(shortbread_path), sizes(pbf_path), sizes(terrain_path)
    regions = []
    for feature in index["features"]:
        props = feature["properties"]
        if props["id"] in COUNTRIES:
            base, code, name = COUNTRIES[props["id"]]
            regions.append((name, props["id"], code, base, bbox(feature["geometry"])))
            continue
        if not props["id"].startswith("us/"):
            continue
        slug = props["id"][3:]
        if slug in SKIP:
            continue
        code = props["iso3166-2"][0].lower()
        regions.append((display_name(slug), slug, code, BASE, bbox(feature["geometry"])))
    # Built regions carry their own addresses, boxes and sizes.
    built_by_id = {b["id"]: b for b in built}
    for b in built:
        regions.append((b["name"], b["id"], b["id"], None, tuple(b["outline_box"])))
        shortbread[b["id"]] = b["package_bytes"]
        pbf[b["id"]] = b["extras_bytes"]
        terrain[b["id"]] = b["terrain_bytes"]
        extract_boxes[b["id"]] = tuple(b["extract_box"])
    # Regions elsewhere, from tools/world-regions.py.
    world = {}
    if world_dir:
        # Keyed "w:<Geofabrik ID>": the country Georgia and the US state are both "georgia".
        for target, name in ((shortbread, "sizes.txt"), (pbf, "pbf-sizes.txt"), (terrain, "terrain-sizes.txt")):
            target.update({f"w:{k}": v for k, v in sizes(world_dir / name).items()})
        for line in (world_dir / "extract-boxes.txt").read_text(encoding="utf-8").splitlines():
            slug, *values = line.split()
            extract_boxes[f"w:{slug}"] = tuple(float(v) for v in values)
        taken = {code for _, _, code, _, _ in regions}
        for w in json.loads((world_dir / "selected.json").read_text(encoding="utf-8")):
            country_iso = w["country_iso"].lower()
            candidates = [w["iso"].lower()] if w["iso"] else []
            candidates += [f"{country_iso}-{w['id']}"] if country_iso else []
            candidates.append(w["id"])
            code = next(c for c in candidates if c and c not in taken)
            taken.add(code)
            continent = CONTINENTS.get(w["continent"], w["continent"])
            country = "Russia" if w["continent"] == "Russian Federation" else w["country"]
            non_latin = w["country_iso"] in NON_LATIN or w["id"] in NON_LATIN_IDS
            world[f"w:{w['id']}"] = (country, continent, NOTE if non_latin else "")
            regions.append((w["name"], f"w:{w['id']}", code, w["url_base"].rsplit("/", 1)[0], tuple(w["box"])))
    regions.sort()

    lines = [
        "// Generated by tools/make-regions.py from Geofabrik's region index; do not edit.",
        f"// Sizes are HEAD Content-Length values of {date}.",
        "use crate::mbtiles::Bounds;",
        "use crate::package::Region;",
        "",
    ]
    names = []
    for name, slug, code, base, (west, south, east, north) in regions:
        const = code.upper().replace("-", "_")
        # The outer box holds the package, the outline's box and the raw extract.
        b = built_by_id.get(slug)
        ow, os_, oe, on = west, south, east, north
        if b:
            pw, ps, pe, pn = b["package_box"]
            ow, os_, oe, on = min(ow, pw), min(os_, ps), max(oe, pe), max(on, pn)
        xw, xs, xe, xn = extract_boxes[slug]
        ow, os_, oe, on = min(ow, xw), min(os_, xs), max(oe, xe), max(on, xn)
        names.append(HANDWRITTEN.get(slug, const))
        file = slug.removeprefix("w:")
        if slug in HANDWRITTEN:
            continue
        w, h = east - west, north - south
        points = [
            ((west + east) / 2, (south + north) / 2),
            (west + 0.1 * w, south + 0.1 * h),
            (east - 0.1 * w, south + 0.1 * h),
            (west + 0.1 * w, north - 0.1 * h),
            (east - 0.1 * w, north - 0.1 * h),
        ]
        point_text = ", ".join(f"({x:.3f}, {y:.3f})" for x, y in points)
        lines += [
            f"pub const {const}: Region = Region {{",
            f'    id: "{code}",',
            f'    name: "{name}",',
            f'    source_page: "{b["source_page"] if b else f"{base}/{file}.html"}",',
            f'    package_url: "{b["package_url"] if b else f"{base}/{file}-shortbread-1.0.mbtiles"}",',
            f'    extras_url: "{b["extras_url"] if b else f"{base}/{file}-latest.osm.pbf"}",',
            "    outer: Bounds {",
            f"        west: {max(-180.0, ow - OUTER_MARGIN):.3f},",
            f"        south: {max(-90.0, os_ - OUTER_MARGIN):.3f},",
            f"        east: {min(180.0, oe + OUTER_MARGIN):.3f},",
            f"        north: {min(90.0, on + OUTER_MARGIN):.3f},",
            "    },",
            f"    required_points: &[{point_text}],",
            f'    country: "{world[slug][0] if slug in world else north_american_country(code)}",',
            f'    continent: "{world[slug][1] if slug in world else "North America"}",',
            f'    note: "{world[slug][2] if slug in world else ""}",',
            f"    package_bytes: {shortbread[slug]},",
            f"    extras_bytes: {pbf[slug]},",
            f"    terrain_bytes: {terrain[slug]},",
            f"    overlay_bytes: {overlays.get(code, 0)},",
            "};",
            "",
        ]
    lines.append(f"/// Every region that can be downloaded, by name.")
    lines.append(f"pub static ALL: [Region; {len(names)}] = [")
    lines += [f"    {n}," for n in names]
    lines += ["];", ""]
    # The same list for the overlay build (tools/layers), with outline boxes.
    listing = [
        {"id": code, "name": name, "slug": slug, "bbox": [round(v, 5) for v in box],
         **({"geofabrik": slug.removeprefix("w:")} if slug in world else {})}
        for name, slug, code, _, box in regions
    ]
    (Path(__file__).resolve().parent / "layers" / "regions.json").write_text(
        json.dumps(listing, indent=1) + "\n", encoding="utf-8", newline="\n")
    out = Path(__file__).resolve().parent.parent / "crates/offline-maps-core/src/regions_data.rs"
    out.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"{len(names)} regions -> {out}")
    il = HANDWRITTEN["illinois"]
    print(f"illinois sizes for {il}: package_bytes {shortbread['illinois']}, extras_bytes {pbf['illinois']}, terrain_bytes {terrain['illinois']}, overlay_bytes {overlays.get('us-il', 0)}")


main()
