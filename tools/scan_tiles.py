"""Scan an MBTiles vector-tile package without third-party dependencies.

Reports, per zoom level, which street_labels kinds carry route refs, and
collects every code point used in string attribute values so glyph ranges
can be subset to what the package actually needs.

Usage: python -I tools/scan_tiles.py <package.mbtiles> [--codepoints out.json]
"""
import gzip
import json
import sqlite3
import sys
from collections import defaultdict


def varint(buf, i):
    result = shift = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if b < 0x80:
            return result, i
        shift += 7


def fields(buf):
    """Yield (field number, wire type, value) for a protobuf message."""
    i, n = 0, len(buf)
    while i < n:
        key, i = varint(buf, i)
        field, wire = key >> 3, key & 7
        if wire == 0:
            value, i = varint(buf, i)
        elif wire == 2:
            length, i = varint(buf, i)
            value, i = buf[i:i + length], i + length
        elif wire == 1:
            value, i = buf[i:i + 8], i + 8
        elif wire == 5:
            value, i = buf[i:i + 4], i + 4
        else:
            raise ValueError(f"unsupported wire type {wire}")
        yield field, wire, value


def packed(buf):
    i, out = 0, []
    while i < len(buf):
        v, i = varint(buf, i)
        out.append(v)
    return out


def decode_layer(buf):
    name, keys, values, features = None, [], [], []
    for field, _, value in fields(buf):
        if field == 1:
            name = value.decode("utf-8")
        elif field == 3:
            keys.append(value.decode("utf-8"))
        elif field == 4:
            v = None
            for vf, _, vv in fields(value):
                v = vv.decode("utf-8") if vf == 1 else vv
            values.append(v)
        elif field == 2:
            features.append(value)
    return name, keys, values, features


def feature_props(buf, keys, values):
    for field, _, value in fields(buf):
        if field == 2:
            tags = packed(value)
            return {keys[tags[j]]: values[tags[j + 1]] for j in range(0, len(tags), 2)}
    return {}


def main():
    path = sys.argv[1]
    out = sys.argv[sys.argv.index("--codepoints") + 1] if "--codepoints" in sys.argv else None
    db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
    refs = defaultdict(lambda: defaultdict(int))  # zoom -> kind -> features with ref
    labels = defaultdict(lambda: defaultdict(int))  # zoom -> kind -> label features
    codepoints = set()
    tiles = 0
    for zoom, data in db.execute("SELECT zoom_level, tile_data FROM tiles"):
        tiles += 1
        if data[:2] == b"\x1f\x8b":
            data = gzip.decompress(data)
        for field, _, layer in fields(data):
            if field != 3:
                continue
            name, keys, values, features = decode_layer(layer)
            for v in values:
                if isinstance(v, str):
                    codepoints.update(ord(c) for c in v)
            if name != "street_labels":
                continue
            for feature in features:
                props = feature_props(feature, keys, values)
                kind = props.get("kind")
                labels[zoom][kind] += 1
                if props.get("ref"):
                    refs[zoom][kind] += 1
    print(f"tiles scanned: {tiles}")
    for zoom in sorted(labels):
        print(f"z{zoom}: labels {dict(labels[zoom])}")
        print(f"     with ref {dict(refs[zoom])}")
    ranges = sorted({cp // 256 for cp in codepoints})
    print(f"distinct code points: {len(codepoints)}; glyph ranges: {len(ranges)}")
    print("ranges:", [f"{r * 256}-{r * 256 + 255}" for r in ranges])
    if out:
        with open(out, "w", encoding="utf-8") as f:
            json.dump({"codepoints": sorted(codepoints), "ranges": [f"{r * 256}-{r * 256 + 255}" for r in ranges]}, f)


if __name__ == "__main__":
    main()
