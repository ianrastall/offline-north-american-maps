"""Builds a region's overlay file: flood hazard zones and public lands as vector tiles.

Run on the maintainer's machine; the results are published with the GitHub releases
(release tag `overlays`) and downloaded by the app. Both sources are public domain:

  Flood zones   FEMA National Flood Hazard Layer, the state file from FEMA's Map
                Service Center (msc.fema.gov), layer S_FLD_HAZ_AR. Where FEMA lists no
                state file (Alabama, Idaho, Louisiana, Washington and the US Virgin
                Islands, November 2026), the same layer from FEMA's public NFHL map
                service, listed in half-degree cells and read by ID.
  Public lands  USGS PAD-US (Protected Areas Database of the United States), the
                "PAD-US Public Access" feature service published by USGS.

Steps for each region: find and download FEMA's current state file; keep floodways,
1% and 0.2% annual-chance zones and levee-reduced risk areas; page PAD-US owned land
and easements in the region's box; write both as GeoPackages in WGS 84; tile them with
Planetiler (zoom 5-14, layers `flood_zones` and `public_lands`) into
`<out>/<region>-overlays.mbtiles`, with `<region>-overlays.json` describing the sources.

Setup (once; nothing here is committed):
  python -m venv data/layer-build/venv
  data/layer-build/venv/Scripts/python -m pip install -r tools/layers/requirements.txt
  Planetiler 0.10.2 at data/layer-build/tools/planetiler.jar
    (gh release download v0.10.2 --repo onthegomap/planetiler -p planetiler.jar;
     SHA-256 f310bd0413e2e4512b27f4046d418664e8e1d3bf31603c2a70e23de06c167e4d)
  Java 21 or later on PATH.

Usage:
  data/layer-build/venv/Scripts/python tools/layers/build_overlays.py us-il [us-in ...]
  data/layer-build/venv/Scripts/python tools/layers/build_overlays.py --all

Regions whose overlay file already exists are skipped (delete it, or pass --force, to
rebuild). A region that fails is reported and the rest continue; out/summary.json lists
every result. FEMA's download and the intermediate GeoPackages are deleted once a region
is built (--keep keeps them), since all states' FEMA files together run to tens of GB.
"""
from __future__ import annotations

import datetime as dt
import hashlib
import json
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

import geopandas as gpd
import pyogrio
import requests

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "data" / "layer-build"
DOWNLOADS = WORK / "downloads"
OUT = WORK / "out"
PLANETILER = WORK / "tools" / "planetiler.jar"
PLANETILER_SHA256 = "f310bd0413e2e4512b27f4046d418664e8e1d3bf31603c2a70e23de06c167e4d"
TEMPLATE = Path(__file__).with_name("overlays.yml")
REGIONS = json.loads(Path(__file__).with_name("regions.json").read_text(encoding="utf-8"))

USER_AGENT = "OfflineUSMapsLayerBuild/1 (offline map data preparation)"
MSC_SEARCH = "https://msc.fema.gov/portal/advanceSearch"
MSC_DOWNLOAD = ("https://msc.fema.gov/portal/downloadProduct"
                "?productTypeID=NFHL&productSubTypeID=NFHL_STATE_DATA&productID={name}")
NFHL_SERVICE = "https://hazards.fema.gov/arcgis/rest/services/public/NFHL/MapServer/28/query"
# What the overlay keeps of S_FLD_HAZ_AR (see classify).
FLOOD_WHERE = ("SFHA_TF = 'T' OR ZONE_SUBTY LIKE '0.2 PCT%' OR ZONE_SUBTY LIKE '1 PCT DEPTH%' "
               "OR ZONE_SUBTY LIKE '%LEVEE%'")
PADUS = ("https://services.arcgis.com/v01gqwM5QqNysAAi/arcgis/rest/services/"
         "PADUS_Public_Access/FeatureServer/0/query")

# State FIPS codes, and a county the Map Service Center search accepts for the state.
FIPS = {
    "al": "01", "az": "04", "ar": "05", "ca": "06", "co": "08", "ct": "09", "de": "10",
    "dc": "11", "fl": "12", "ga": "13", "hi": "15", "id": "16", "il": "17", "in": "18",
    "ia": "19", "ks": "20", "ky": "21", "la": "22", "me": "23", "md": "24", "ma": "25",
    "mi": "26", "mn": "27", "ms": "28", "mo": "29", "mt": "30", "ne": "31", "nv": "32",
    "nh": "33", "nj": "34", "nm": "35", "ny": "36", "nc": "37", "nd": "38", "oh": "39",
    "ok": "40", "or": "41", "pa": "42", "ri": "44", "sc": "45", "sd": "46", "tn": "47",
    "tx": "48", "ut": "49", "vt": "50", "va": "51", "wa": "53", "wv": "54", "wi": "55",
    "wy": "56", "pr": "72", "vi": "78",
}
FIRST_COUNTY = {"vi": "010"}

ACCESS = {"OA": "open", "RA": "restricted", "XA": "closed", "UK": "unknown"}


def session() -> requests.Session:
    s = requests.Session()
    s.headers["User-Agent"] = USER_AGENT
    return s


def download(s: requests.Session, url: str, target: Path) -> Path:
    """Downloads to `target` (skipped when it already exists), through a .part file."""
    if target.exists():
        return target
    target.parent.mkdir(parents=True, exist_ok=True)
    part = target.with_suffix(target.suffix + ".part")
    with s.get(url, stream=True, timeout=120) as r:
        r.raise_for_status()
        if "html" in r.headers.get("Content-Type", ""):
            raise RuntimeError(f"{url} returned a web page, not a file")
        with open(part, "wb") as f:
            for chunk in r.iter_content(1 << 20):
                f.write(chunk)
    part.replace(target)
    return target


def fema_product(s: requests.Session, state: str) -> dict:
    """The current NFHL state file listed by the Map Service Center."""
    fips = FIPS[state]
    county = fips + FIRST_COUNTY.get(state, "001")
    form = {
        "utf8": "✓", "affiliate": "fema", "query": "", "selstate": fips,
        "selcounty": county, "selcommunity": county + "C", "jurisdictionkey": "",
        "jurisdictionvalue": "", "searchedCid": county + "C", "searchedDateStart": "",
        "searchedDateEnd": "", "txtstartdate": "", "txtenddate": "", "method": "search",
    }
    r = s.post(MSC_SEARCH, data=form, timeout=120,
               headers={"Accept": "application/json", "X-Requested-With": "XMLHttpRequest"})
    r.raise_for_status()
    items = [i for i in r.json().get("EFFECTIVE", {}).get("NFHL_STATE_DATA", [])
             if i.get("product_SUBTYPE_ID") == "NFHL_STATE_DATA"]
    if not items:
        raise RuntimeError(f"FEMA lists no state flood file for {state.upper()}")
    return max(items, key=lambda i: i["product_NAME"])


def flood_zones(zip_path: Path, name: str, out: Path) -> int:
    src = f"/vsizip/{zip_path.as_posix()}/{name}.gdb"
    df = pyogrio.read_dataframe(src, layer="S_FLD_HAZ_AR",
                                columns=["FLD_ZONE", "ZONE_SUBTY", "SFHA_TF"])
    return classify(df, out)


def flood_zones_from_service(s: requests.Session, bbox: list[float], out: Path) -> int:
    """The flood layer from FEMA's map service. The IDs of matching areas are listed in
    half-degree cells (a whole state in one query times out); the areas are then read
    by ID, 100 at a time, since queries that return geometry for a whole cell fail on
    the server (HTTP 500, November 2026). A batch that fails is split in half."""
    west, south, east, north = bbox
    step = 0.5
    cells = []
    x = west
    while x < east:
        y = south
        while y < north:
            cells.append((x, y, min(x + step, east), min(y + step, north)))
            y += step
        x += step
    def request(method: str, what: str, **params) -> dict:
        for attempt in range(5):
            try:
                if method == "post":
                    r = s.post(NFHL_SERVICE, data=params, timeout=300)
                else:
                    r = s.get(NFHL_SERVICE, params=params, timeout=300)
                if r.ok:
                    reply = r.json()
                    if "error" not in reply:
                        return reply
            except (requests.RequestException, ValueError):
                pass
            time.sleep(5 * (attempt + 1))
        raise RuntimeError(f"FEMA map service failed for {what}")

    ids: set[int] = set()
    for number, cell in enumerate(cells, 1):
        where = {"where": FLOOD_WHERE, "geometry": ",".join(f"{v:.5f}" for v in cell),
                 "geometryType": "esriGeometryEnvelope", "inSR": 4326,
                 "spatialRel": "esriSpatialRelIntersects", "f": "json"}
        listed = request("get", f"cell {cell}", returnIdsOnly="true", **where).get("objectIds") or []
        count = request("get", f"cell {cell}", returnCountOnly="true", **where)["count"]
        if len(listed) != count:
            raise RuntimeError(f"FEMA map service listed {len(listed)} of {count} areas in cell {cell}")
        ids.update(listed)
        if number % 20 == 0:
            print(f"  map service: {number}/{len(cells)} cells, {len(ids)} areas", flush=True)
    if not ids:
        raise RuntimeError("FEMA's map service returned no flood areas")

    frames = []

    def fetch(batch: list[int], simplify: bool = False) -> None:
        # Coordinates to six decimals (about 10 cm). A single area too large for the
        # service even so (Alabama has one) is read simplified to 0.5 m, still well
        # below what a zoom 14 tile can show.
        extra = {"maxAllowableOffset": 0.000005} if simplify else {}
        try:
            reply = request("post", f"{len(batch)} areas from {batch[0]}",
                            objectIds=",".join(map(str, batch)), outSR=4326, f="geojson",
                            geometryPrecision=6, outFields="OBJECTID,FLD_ZONE,ZONE_SUBTY,SFHA_TF",
                            **extra)
        except RuntimeError:
            if len(batch) == 1:
                if simplify:
                    raise
                print(f"  map service: area {batch[0]} read simplified", flush=True)
                fetch(batch, simplify=True)
                return
            half = len(batch) // 2
            fetch(batch[:half])
            fetch(batch[half:])
            return
        if reply["features"]:
            frames.append(gpd.GeoDataFrame.from_features(reply["features"], crs=4326))

    ordered = sorted(ids)
    for start in range(0, len(ordered), 100):
        fetch(ordered[start:start + 100])
        if (start // 100) % 50 == 49:
            print(f"  map service: {start + 100}/{len(ordered)} areas read", flush=True)
    return classify(gpd.pd.concat(frames, ignore_index=True), out)


def classify(df: gpd.GeoDataFrame, out: Path) -> int:
    """Keeps floodways, 1% and 0.2% annual-chance zones and levee-reduced areas."""
    subtype = df["ZONE_SUBTY"].fillna("").str.upper()
    kind = (subtype.str.contains("FLOODWAY").map({True: "floodway"})
            .fillna(df["SFHA_TF"].eq("T").map({True: "high"}))
            .fillna(subtype.str.startswith(("0.2 PCT", "1 PCT DEPTH LESS")).map({True: "moderate"}))
            .fillna(subtype.str.contains("LEVEE").map({True: "levee"})))
    df = df.assign(kind=kind, zone=df["FLD_ZONE"]).dropna(subset=["kind"])
    df = df[["kind", "zone", "geometry"]].to_crs(4326)
    df = df[df.geometry.notna() & ~df.geometry.is_empty]
    df.to_file(out, layer="flood_zones", driver="GPKG")
    return len(df)


def public_lands(s: requests.Session, bbox: list[float], out: Path) -> int:
    fields = "Unit_Nm,Pub_Access,MngTp_Desc,MngNm_Desc,DesTp_Desc,Category"
    frames, offset = [], 0
    while True:
        params = {
            "where": "Category IN ('Fee','Easement')", "geometry": ",".join(map(str, bbox)),
            "geometryType": "esriGeometryEnvelope", "inSR": 4326,
            "spatialRel": "esriSpatialRelIntersects", "outFields": fields, "outSR": 4326,
            "orderByFields": "OBJECTID", "resultOffset": offset, "resultRecordCount": 2000,
            "geometryPrecision": 6, "f": "geojson",
        }
        for attempt in range(4):
            r = s.get(PADUS, params=params, timeout=180)
            if r.ok and r.json().get("type") == "FeatureCollection":
                break
            time.sleep(5 * (attempt + 1))
        else:
            raise RuntimeError(f"PAD-US query failed at offset {offset}: {r.status_code} {r.text[:200]}")
        page = r.json()
        if page["features"]:
            frames.append(gpd.GeoDataFrame.from_features(page["features"], crs=4326))
        if not page.get("properties", {}).get("exceededTransferLimit") and len(page["features"]) < 2000:
            break
        offset += len(page["features"])
        time.sleep(0.5)
    df = gpd.pd.concat(frames, ignore_index=True)
    # PAD-US unit names often end in the record's category ("... Refuge Fee");
    # easements are mostly private land under agreement (program codes as names),
    # so only owned land gets a label.
    name = df["Unit_Nm"].fillna("").str.replace(r"\s+(Fee|Easement)$", "", regex=True).str.strip()
    label = name.where(df["Category"].eq("Fee"), "")
    df = gpd.GeoDataFrame({
        "name": name,
        "label": label,
        "access": df["Pub_Access"].map(ACCESS).fillna("unknown"),
        "manager_type": df["MngTp_Desc"].fillna(""),
        "manager": df["MngNm_Desc"].fillna(""),
        "designation": df["DesTp_Desc"].fillna(""),
        "category": df["Category"].str.lower(),
    }, geometry=df.geometry, crs=4326)
    df = df[df.geometry.notna() & ~df.geometry.is_empty]
    df.to_file(out, layer="public_lands", driver="GPKG")
    return len(df)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tile(region: dict, flood: Path, lands: Path, out: Path) -> None:
    if sha256(PLANETILER) != PLANETILER_SHA256:
        raise RuntimeError("planetiler.jar does not match the pinned checksum")
    schema = TEMPLATE.read_text(encoding="utf-8")
    schema = schema.replace("@FLOOD@", flood.as_posix()).replace("@LANDS@", lands.as_posix())
    config = out.with_suffix(".yml")
    config.write_text(schema, encoding="utf-8")
    w, s, e, n = region["bbox"]
    subprocess.run([
        "java", "-Xmx16g", "-jar", str(PLANETILER), "generate-custom", f"--schema={config}",
        f"--output={out}", "--force", f"--bounds={w},{s},{e},{n}",
        "--tmpdir=" + str(WORK / "tmp"),
    ], check=True)


def build(region_id: str, keep: bool) -> dict:
    region = next((r for r in REGIONS if r["id"] == region_id), None)
    if region is None:
        raise SystemExit(f"unknown region {region_id}")
    state = region_id.split("-")[1]
    s = session()
    work = WORK / "regions" / region_id
    work.mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)

    flood = work / "flood.gpkg"
    flood.unlink(missing_ok=True)
    zip_path = None
    try:
        product = fema_product(s, state)
    except RuntimeError as e:
        # No state file: read the national map service for the region's box.
        today = dt.date.today()
        name = "NFHL map service"
        product = {"product_EFFECTIVE_DATE_STRING": f"{today:%m/%d/%Y} (map service)"}
        print(f"{region['name']}: {e}; using FEMA's NFHL map service", flush=True)
        flood_count = flood_zones_from_service(s, region["bbox"], flood)
    else:
        name = product["product_NAME"]
        print(f"{region['name']}: FEMA {name} ({product.get('product_FILE_SIZE')}), "
              f"effective {product.get('product_EFFECTIVE_DATE_STRING')}", flush=True)
        zip_path = download(s, MSC_DOWNLOAD.format(name=name), DOWNLOADS / "fema" / f"{name}.zip")
        flood_count = flood_zones(zip_path, name, flood)
    print(f"  flood zones: {flood_count}", flush=True)

    lands = work / "lands.gpkg"
    lands.unlink(missing_ok=True)
    lands_count = public_lands(s, region["bbox"], lands)
    print(f"  public lands: {lands_count}", flush=True)

    out = OUT / f"{region_id}-overlays.mbtiles"
    tile(region, flood, lands, out)
    built_at = dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    stamp = {
        "flood_product": name,
        "flood_effective": product.get("product_EFFECTIVE_DATE_STRING") or "",
        "public_lands": "USGS PAD-US 4.1",
        "built_at": built_at,
    }
    with sqlite3.connect(out) as db:
        db.executemany("INSERT OR REPLACE INTO metadata (name, value) VALUES (?, ?)",
                       [(f"offline-us-maps:{k}", v) for k, v in stamp.items()])
    manifest = {
        "region": region_id,
        "file": out.name,
        "bytes": out.stat().st_size,
        "sha256": sha256(out),
        "built_at": built_at,
        "flood": {"source": "FEMA National Flood Hazard Layer", "product": name,
                  "effective": product.get("product_EFFECTIVE_DATE_STRING"), "features": flood_count},
        "public_lands": {"source": "USGS PAD-US 4.1 (PAD-US Public Access service)",
                         "features": lands_count},
    }
    out.with_suffix(".json").write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    print(f"  {out.name}: {manifest['bytes'] / 1e6:.1f} MB", flush=True)
    if not keep:
        if zip_path is not None:
            zip_path.unlink(missing_ok=True)
        for f in work.glob("*"):
            f.unlink(missing_ok=True)
    return manifest


def main(args: list[str]) -> None:
    flags = {a for a in args if a.startswith("--")}
    ids = [r["id"] for r in REGIONS] if "--all" in flags else [a for a in args if not a.startswith("--")]
    if not ids:
        raise SystemExit(__doc__)
    OUT.mkdir(parents=True, exist_ok=True)
    summary_path = OUT / "summary.json"
    summary = json.loads(summary_path.read_text(encoding="utf-8")) if summary_path.exists() else {}
    for rid in ids:
        out = OUT / f"{rid}-overlays.mbtiles"
        if out.exists() and out.with_suffix(".json").exists() and "--force" not in flags:
            print(f"{rid}: already built, skipped", flush=True)
            summary[rid] = json.loads(out.with_suffix(".json").read_text(encoding="utf-8"))
            continue
        started = time.monotonic()
        try:
            summary[rid] = build(rid, keep="--keep" in flags)
            summary[rid]["seconds"] = round(time.monotonic() - started)
        except Exception as e:  # report it and go on with the next region
            print(f"{rid}: FAILED: {type(e).__name__}: {e}", flush=True)
            summary[rid] = {"region": rid, "error": f"{type(e).__name__}: {e}"}
        summary_path.write_text(json.dumps(summary, indent=1) + "\n", encoding="utf-8")
    failed = [r for r, v in summary.items() if "error" in v]
    total = sum(v.get("bytes", 0) for v in summary.values())
    print(f"done: {len(summary) - len(failed)} built, {len(failed)} failed {failed}; {total / 1e9:.2f} GB", flush=True)


if __name__ == "__main__":
    main(sys.argv[1:])
