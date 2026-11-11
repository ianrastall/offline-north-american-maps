"""Builds the overview built into Offline North American Maps: assets/overview/us-overview.mbtiles.

The overview is drawn under the installed regions so the map is never blank outside them:
coast, lakes and large rivers, country, state and province borders, main highways, cities and
state and province names, at zoom 0-7 (shown enlarged beyond that). It covers the US, Canada
and Mexico. Every feature in a catalog region carries the region's ID (`region`: us-il, ca-on,
mx-que), and the map leaves out the overview inside installed regions, whose own maps show the
same things in detail. The map matches IDs by substring, so the whole-country region mx also
hides every Mexican state's features.

Source: Natural Earth 1:10m (naturalearthdata.com), public domain, downloaded from
naciscdn.org. Run on the maintainer's machine after changing it; the result is committed.

Setup: the venv, Planetiler and Java of tools/layers/build_overlays.py.

Usage:
  data/layer-build/venv/Scripts/python tools/overview/build_overview.py
"""
from __future__ import annotations

import hashlib
import json
import math
import subprocess
import sys
from pathlib import Path

import geopandas as gpd
import pandas as pd
import pyogrio
import requests
import shapely
from shapely.geometry import Point, box

ROOT = Path(__file__).resolve().parents[2]
WORK = ROOT / "data" / "overview-build"
NE = WORK / "ne"
PLANETILER = ROOT / "data" / "layer-build" / "tools" / "planetiler.jar"
PLANETILER_SHA256 = "f310bd0413e2e4512b27f4046d418664e8e1d3bf31603c2a70e23de06c167e4d"
TEMPLATE = Path(__file__).with_name("overview.yml")
OUT = ROOT / "assets" / "overview" / "us-overview.mbtiles"
REGIONS = {r["id"] for r in json.loads((ROOT / "tools" / "layers" / "regions.json").read_text(encoding="utf-8"))}

# West, south, east, north: the 50 states (Alaska's islands west of 180 degrees aside),
# Puerto Rico, the US Virgin Islands, Canada and Mexico, with the bordering parts of Central
# America and Greenland.
BOUNDS = (-180.0, 8.0, -48.0, 84.0)
MAX_ZOOM = 7
FILES = [
    "cultural/ne_10m_admin_1_states_provinces",
    "cultural/ne_10m_admin_0_boundary_lines_land",
    "cultural/ne_10m_roads",
    "cultural/ne_10m_populated_places",
    "physical/ne_10m_ocean",
    "physical/ne_10m_lakes",
    "physical/ne_10m_lakes_north_america",
    "physical/ne_10m_rivers_lake_centerlines",
    "physical/ne_10m_rivers_north_america",
]
TERRITORIES = {"PRI": ("us-pr", "Puerto Rico", "PR"), "VIR": ("us-vi", "U.S. Virgin Islands", "VI")}
# Natural Earth still has the old name and ISO code of Mexico City, and the state of Mexico is
# named in full so that it is not read as the country.
MEXICAN_NAMES = {"DF": "Ciudad de México", "MX": "Estado de México"}
ISO_FIXES = {"MX-DIF": "MX-CMX"}


def fetch() -> None:
    NE.mkdir(parents=True, exist_ok=True)
    for name in FILES:
        target = NE / (Path(name).name + ".zip")
        if not target.exists():
            print(f"downloading {name}")
            response = requests.get(f"https://naciscdn.org/naturalearth/10m/{name}.zip", timeout=300)
            response.raise_for_status()
            target.write_bytes(response.content)


def read(name: str, **kwargs) -> gpd.GeoDataFrame:
    df = pyogrio.read_dataframe(f"zip://{NE / (name + '.zip')}", bbox=BOUNDS, **kwargs)
    return df.to_crs(4326) if df.crs and df.crs.to_epsg() != 4326 else df


def zoom(values: pd.Series) -> pd.Series:
    """Natural Earth's fractional min_zoom as a whole zoom within the overview's range."""
    return values.fillna(MAX_ZOOM).apply(lambda z: min(MAX_ZOOM, max(0, math.floor(z))))


def region_id(adm0: str, postal: str, iso: str) -> str:
    if adm0 in TERRITORIES:
        return TERRITORIES[adm0][0]
    if adm0 == "USA":
        return f"us-{postal.lower()}"
    iso = ISO_FIXES.get(iso, iso)
    if not iso[3:].isalpha():
        return "mx" if adm0 == "MEX" else "ca"  # unnamed islands
    return iso.lower()


def country(region: str) -> str:
    return region.split("-")[0]


# Nunavut is two catalog regions, split at 90 degrees W (one file would be over GitHub's
# 2 GiB limit). The label stays on the half that holds Natural Earth's label point.
NUNAVUT_SPLIT = -90.0


def split_nunavut(df: gpd.GeoDataFrame) -> gpd.GeoDataFrame:
    row = df[df["region"] == "ca-nu"]
    if row.empty:
        return df
    row = row.iloc[0]
    parts = []
    for suffix, area in (("w", box(-180, -90, NUNAVUT_SPLIT, 90)), ("e", box(NUNAVUT_SPLIT, -90, 180, 90))):
        part = row.copy()
        part["region"] = f"ca-nu-{suffix}"
        part["geometry"] = row.geometry.intersection(area)
        if not area.contains(Point(row["longitude"], row["latitude"])):
            part["name"] = ""
        parts.append(part)
    rest = df[df["region"] != "ca-nu"]
    return gpd.GeoDataFrame(pd.concat([rest, gpd.GeoDataFrame(parts, crs=df.crs)], ignore_index=True), geometry="geometry", crs=df.crs)


def regions() -> gpd.GeoDataFrame:
    """One row per US state or territory, Canadian province or territory and Mexican state,
    with its catalog region."""
    df = read("ne_10m_admin_1_states_provinces", where="adm0_a3 IN ('USA','PRI','VIR','MEX','CAN')")
    df["region"] = [region_id(a, p or "", i or "") for a, p, i in zip(df["adm0_a3"], df["postal"], df["iso_3166_2"])]
    df["name"] = [
        MEXICAN_NAMES.get(p, n) if a == "MEX" else TERRITORIES[a][1] if a in TERRITORIES else n
        for a, p, n in zip(df["adm0_a3"], df["postal"], df["name"])
    ]
    df["abbrev"] = [TERRITORIES[a][2] if a in TERRITORIES else p for a, p in zip(df["adm0_a3"], df["postal"])]
    # Puerto Rico and the Virgin Islands have many parts.
    df = df.dissolve(by="region", aggfunc="first", as_index=False)
    df["name"] = df["name"].fillna("")
    df = split_nunavut(df)
    missing = REGIONS - set(df["region"]) - {"mx"}
    if missing:
        raise RuntimeError(f"no Natural Earth outline for {sorted(missing)}")
    return df[["region", "name", "abbrev", "latitude", "longitude", "geometry"]]


def frame(columns: dict, geometry) -> gpd.GeoDataFrame:
    """A GeoDataFrame from columns (sequences or single values) and geometries, row by row."""
    geometry = list(geometry)
    data = {k: ([v] * len(geometry) if isinstance(v, (str, int, float)) else list(v)) for k, v in columns.items()}
    return gpd.GeoDataFrame(data, geometry=geometry, crs=4326)


def tag(features: gpd.GeoDataFrame, states: gpd.GeoDataFrame) -> gpd.GeoDataFrame:
    """Splits lines and areas at state borders and gives each piece its region (or none)."""
    pieces = gpd.overlay(features, states[["region", "geometry"]], how="identity", keep_geom_type=True)
    pieces["region"] = pieces["region"].fillna("")
    return pieces


def state_borders(states: gpd.GeoDataFrame) -> gpd.GeoDataFrame:
    """The border between each pair of neighbouring states, with both regions in `region`
    ("us-il us-in"; "mx" between Mexican states), so it is left out when either state's own
    map is installed. Coasts are drawn by the water, and borders between countries by the
    country lines."""
    regions, lines = [], []
    for i, j in zip(*states.sindex.query(states.geometry, predicate="intersects")):
        if i >= j:
            continue
        a, b = states.iloc[i], states.iloc[j]
        if country(a["region"]) != country(b["region"]):
            continue
        if a["region"].startswith("ca-nu") and b["region"].startswith("ca-nu"):
            continue  # the split is not a border
        shared = a.geometry.boundary.intersection(b.geometry.boundary)
        parts = [g for g in shapely.get_parts(shared) if g.geom_type in ("LineString", "MultiLineString")]
        if parts:
            regions.append(f"{a['region']} {b['region']}")
            lines.append(shapely.line_merge(shapely.union_all(parts)))
    return frame({"kind": "state", "region": regions}, lines)


def build() -> None:
    fetch()
    states = regions()
    area = box(*BOUNDS)
    layers: dict[str, gpd.GeoDataFrame] = {}

    ocean = read("ne_10m_ocean").clip(area)
    ocean = ocean.assign(kind="ocean", region="", min_zoom=0)[["kind", "region", "min_zoom", "geometry"]]
    lakes = pd.concat([read("ne_10m_lakes"), read("ne_10m_lakes_north_america")], ignore_index=True)
    lakes = lakes[lakes["featurecla"].fillna("").str.contains("Lake|Reservoir", regex=True)]
    lakes = frame({"kind": "lake", "min_zoom": zoom(lakes["min_zoom"])}, lakes.geometry)
    lakes = tag(lakes, states)
    layers["water"] = pd.concat([ocean, lakes], ignore_index=True)

    rivers = pd.concat([read("ne_10m_rivers_lake_centerlines"), read("ne_10m_rivers_north_america")], ignore_index=True)
    rivers = rivers[rivers["featurecla"].fillna("").str.startswith("River")]
    rivers = frame({"name": rivers["name"].fillna(""), "min_zoom": zoom(rivers["min_zoom"])}, rivers.geometry)
    layers["rivers"] = tag(rivers, states)

    countries = read("ne_10m_admin_0_boundary_lines_land")
    countries = countries[countries["ADM0_A3_L"].isin(["USA", "MEX", "CAN"]) | countries["ADM0_A3_R"].isin(["USA", "MEX", "CAN"])]
    country_lines = frame({"kind": "country", "region": ""}, countries.geometry)
    layers["boundaries"] = pd.concat([country_lines, state_borders(states)], ignore_index=True)

    roads = read("ne_10m_roads", where="sov_a3 IN ('USA','MEX','CAN')")
    roads = roads[roads["level"].isin(["Interstate", "Federal", "State"]) & (roads["type"] != "Ferry Route")]
    # Natural Earth's "Interstate" level in Mexico is the federal toll motorways (95D).
    kind = roads["level"].str.lower()
    ref = [
        f"MEX {n}" if sov == "MEX" and level != "State"
        else str(n) if sov == "CAN"
        else f"I-{n}" if level == "Interstate" else f"US {n}" if level == "Federal" else str(n)
        for sov, level, n in zip(roads["sov_a3"], roads["level"], roads["name"].fillna(""))
    ]
    roads = frame({"kind": kind.values, "expressway": roads["expressway"].fillna(0).astype(int).values, "ref": ref, "min_zoom": zoom(roads["min_zoom"]).values}, roads.geometry)
    layers["roads"] = tag(roads, states)

    places = read("ne_10m_populated_places", where="ADM0_A3 IN ('USA','PRI','VIR','MEX','CAN')")
    capital = ["country" if c == "Admin-0 capital" else "state" if c == "Admin-1 capital" else "none" for c in places["FEATURECLA"]]
    places = frame({"name": places["NAME"], "rank": places["SCALERANK"].astype(int), "capital": capital, "min_zoom": zoom(places["MIN_ZOOM"])}, places.geometry)
    places = gpd.sjoin(places, states[["region", "geometry"]], how="left", predicate="within").drop(columns="index_right")
    places["region"] = places["region"].fillna("")
    layers["places"] = places

    named = states[states["name"] != ""]
    state_labels = frame({"kind": "state", "name": named["name"], "abbrev": named["abbrev"].fillna(""), "region": named["region"]}, [Point(x, y) for x, y in zip(named["longitude"], named["latitude"])])
    country_labels = frame({"kind": "country", "name": ["Canada", "Mexico"], "abbrev": "", "region": ""}, [Point(-100.0, 56.0), Point(-102.5, 23.5)])
    layers["labels"] = pd.concat([state_labels, country_labels], ignore_index=True)

    source = WORK / "overview.gpkg"
    source.unlink(missing_ok=True)
    for name, df in layers.items():
        df = gpd.GeoDataFrame(df, geometry="geometry", crs=4326)
        df["layer"] = name
        if "min_zoom" not in df:
            df["min_zoom"] = 0
        df["min_zoom"] = df["min_zoom"].astype(int)
        print(f"{name}: {len(df)} features")
        pyogrio.write_dataframe(df, source, layer=name)

    with PLANETILER.open("rb") as f:
        if hashlib.file_digest(f, "sha256").hexdigest() != PLANETILER_SHA256:
            raise RuntimeError("planetiler.jar does not match the pinned checksum")
    config = WORK / "overview.yml"
    config.write_text(TEMPLATE.read_text(encoding="utf-8").replace("@SOURCE@", source.as_posix()), encoding="utf-8")
    OUT.parent.mkdir(parents=True, exist_ok=True)
    w, s, e, n = BOUNDS
    subprocess.run([
        "java", "-Xmx4g", "-jar", str(PLANETILER), "generate-custom", f"--schema={config}",
        f"--output={OUT}", "--force", f"--bounds={w},{s},{e},{n}", "--tmpdir=" + str(WORK / "tmp"),
    ], check=True)
    print(f"wrote {OUT} ({OUT.stat().st_size / 1e6:.1f} MB)")


if __name__ == "__main__":
    build()
    sys.exit(0)
