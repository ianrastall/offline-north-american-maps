"""Writes the header bounding box of each region's raw OpenStreetMap extract, an input of
tools/make-regions.py: "<slug> <west> <south> <east> <north>" per line.

Geofabrik cuts each extract with a boundary that can reach beyond the outline in its region
index (Michigan's to 79.0 W, Kansas's to 107.3 W), and the app checks a downloaded extract's
header box against the region's outer box. Only the first 64 KB of each file is read, with a
range request.

Usage: python -I tools/extract-boxes.py <index-v1.json> <out.txt>
"""
import sys
import urllib.request
import zlib
from pathlib import Path


def varint(b, i):
    shift = result = 0
    while True:
        c = b[i]
        i += 1
        result |= (c & 0x7F) << shift
        if c < 0x80:
            return result, i
        shift += 7


def fields(b):
    i, out = 0, []
    while i < len(b):
        key, i = varint(b, i)
        num, wt = key >> 3, key & 7
        if wt == 0:
            v, i = varint(b, i)
        elif wt == 2:
            n, i = varint(b, i)
            v = b[i:i + n]
            i += n
        elif wt == 1:
            v = b[i:i + 8]
            i += 8
        elif wt == 5:
            v = b[i:i + 4]
            i += 4
        else:
            raise ValueError(wt)
        out.append((num, v))
    return out


def zz(n):
    return (n >> 1) ^ -(n & 1)


def header_bbox(url):
    req = urllib.request.Request(url, headers={"User-Agent": "offline-us-maps-catalog", "Range": "bytes=0-65535", "Cache-Control": "no-cache"})
    with urllib.request.urlopen(req, timeout=60) as r:
        b = r.read()
    n = int.from_bytes(b[:4], "big")
    header = dict(fields(b[4:4 + n]))
    size = header[3]
    blob = dict(fields(b[4 + n:4 + n + size]))
    data = zlib.decompress(blob[3]) if 3 in blob else blob[1]
    hb = dict(fields(data))
    box = dict(fields(hb[1]))
    left, right, top, bottom = (zz(box[k]) / 1e9 for k in (1, 2, 3, 4))
    return left, bottom, right, top



US = "https://download.geofabrik.de/north-america/us"
COUNTRIES = {"mexico": "https://download.geofabrik.de/north-america"}


def main():
    import json
    index_path, out_path = sys.argv[1:3]
    slugs = []
    for f in json.loads(Path(index_path).read_text(encoding="utf-8"))["features"]:
        pid = f["properties"]["id"]
        if pid in COUNTRIES:
            slugs.append((pid, COUNTRIES[pid]))
        elif pid.startswith("us/") and pid != "us/alaska":
            slugs.append((pid[3:], US))
    lines = []
    for slug, base in sorted(slugs):
        w, s, e, n = header_bbox(f"{base}/{slug}-latest.osm.pbf")
        lines.append(f"{slug} {w:.6f} {s:.6f} {e:.6f} {n:.6f}")
        print(lines[-1], flush=True)
    Path(out_path).write_text("".join(line + "\n" for line in lines), encoding="utf-8")


main()
