"""Builds the overview built into Offline US Maps: assets/overview/us-overview.mbtiles.

The overview is drawn under the installed regions so the map is never blank outside them:
coast, lakes and large rivers, country and state borders, Interstates, US and Mexican federal
highways, cities and state names, at zoom 0-7 (shown enlarged beyond that). It covers the US
and Mexico. Every feature in a catalog region carries the region's ID (`region`, such as us-il,
or mx for all of Mexico), and the map leaves out the overview inside installed regions, whose
own maps show the same things in detail.

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
# Puerto Rico, the US Virgin Islands and Mexico, with the bordering parts of Canada and
# Central America.
BOUNDS = (-180.0, 8.0, -48.0, 78.0)
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
# Mexico is one catalog region (Geofabrik has one file for it); its states keep their own
# outlines and names. Natural Earth still has the old name of Mexico City, and the state of
# Mexico is named in full so that it is not read as the country.
MEXICO = "mx"
MEXICAN_NAMES = {"DF": "Ciudad de México", "MX": "Estado de México"}


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


def regions() -> gpd.GeoDataFrame:
    """One row per US state or territory and per Mexican state, with its catalog region."""
    df = read("ne_10m_admin_1_states_provinces", where="adm0_a3 IN ('USA','PRI','VIR','MEX')")
    mexican = df["adm0_a3"] == "MEX"
    df["region"] = [
        MEXICO if a == "MEX" else TERRITORIES[a][0] if a in TERRITORIES else f"us-{p.lower()}"
        for a, p in zip(df["adm0_a3"], df["postal"])
    ]
    df["name"] = [
        MEXICAN_NAMES.get(p, n) if a == "MEX" else TERRITORIES[a][1] if a in TERRITORIES else n
        for a, p, n in zip(df["adm0_a3"], df["postal"], df["name"])
    ]
    df["abbrev"] = [TERRITORIES[a][2] if a in TERRITORIES else p for a, p in zip(df["adm0_a3"], df["postal"])]
    # US regions are dissolved (Puerto Rico has many parts); Mexican states stay apart.
    df["key"] = df["region"].where(~mexican, "mx:" + df["adm1_code"].fillna(""))
    df = df.dissolve(by="key", aggfunc="first", as_index=False)
    df["name"] = df["name"].fillna("")
    missing = REGIONS - set(df["region"])
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
        if (a["region"] == MEXICO) != (b["region"] == MEXICO):
            continue
        shared = a.geometry.boundary.intersection(b.geometry.boundary)
        parts = [g for g in shapely.get_parts(shared) if g.geom_type in ("LineString", "MultiLineString")]
        if parts:
            regions.append(a["region"] if a["region"] == b["region"] else f"{a['region']} {b['region']}")
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
    countries = countries[countries["ADM0_A3_L"].isin(["USA", "MEX"]) | countries["ADM0_A3_R"].isin(["USA", "MEX"])]
    country_lines = frame({"kind": "country", "region": ""}, countries.geometry)
    layers["boundaries"] = pd.concat([country_lines, state_borders(states)], ignore_index=True)

    roads = read("ne_10m_roads", where="sov_a3 IN ('USA','MEX')")
    roads = roads[roads["level"].isin(["Interstate", "Federal", "State"]) & (roads["type"] != "Ferry Route")]
    # Natural Earth's "Interstate" level in Mexico is the federal toll motorways (95D).
    kind = roads["level"].str.lower()
    ref = [
        f"MEX {n}" if country == "MEX" and level != "State"
        else f"I-{n}" if level == "Interstate" else f"US {n}" if level == "Federal" else str(n)
        for country, level, n in zip(roads["sov_a3"], roads["level"], roads["name"].fillna(""))
    ]
    roads = frame({"kind": kind.values, "expressway": roads["expressway"].fillna(0).astype(int).values, "ref": ref, "min_zoom": zoom(roads["min_zoom"]).values}, roads.geometry)
    layers["roads"] = tag(roads, states)

    places = read("ne_10m_populated_places", where="ADM0_A3 IN ('USA','PRI','VIR','MEX')")
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
