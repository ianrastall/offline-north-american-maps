"""Builds a region's overlay file: flood hazard zones and public lands as vector tiles.

Run on the maintainer's machine; the results are published with the GitHub releases
(release tag `overlays`) and downloaded by the app. Both sources are public domain:

  Flood zones   FEMA National Flood Hazard Layer, the state file from FEMA's Map
                Service Center (msc.fema.gov), layer S_FLD_HAZ_AR.
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
        "java", "-Xmx4g", "-jar", str(PLANETILER), "generate-custom", f"--schema={config}",
        f"--output={out}", "--force", f"--bounds={w},{s},{e},{n}",
        "--tmpdir=" + str(WORK / "tmp"),
    ], check=True)


def build(region_id: str) -> None:
    region = next((r for r in REGIONS if r["id"] == region_id), None)
    if region is None:
        raise SystemExit(f"unknown region {region_id}")
    state = region_id.split("-")[1]
    s = session()
    work = WORK / "regions" / region_id
    work.mkdir(parents=True, exist_ok=True)
    OUT.mkdir(parents=True, exist_ok=True)

    product = fema_product(s, state)
    name = product["product_NAME"]
    print(f"{region['name']}: FEMA {name} ({product.get('product_FILE_SIZE')}), "
          f"effective {product.get('product_EFFECTIVE_DATE_STRING')}", flush=True)
    zip_path = download(s, MSC_DOWNLOAD.format(name=name), DOWNLOADS / "fema" / f"{name}.zip")
    flood = work / "flood.gpkg"
    flood.unlink(missing_ok=True)
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


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    for rid in sys.argv[1:]:
        build(rid)
