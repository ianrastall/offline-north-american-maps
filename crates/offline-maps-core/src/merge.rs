//! Merges vector tiles of the same z/x/y from neighbouring regions' packages.
//!
//! Each Geofabrik package is cut from one region's outline, so a tile on a
//! border exists in both packages with each side's features. A tile may have
//! only one layer of each name, so same-named layers are combined: key and
//! value tables are joined without repeats, each feature's tags are renumbered
//! to match, and features that are byte-for-byte the same after renumbering
//! (the same thing mapped in both extracts) are kept once. Geometry is copied
//! unchanged. Input tiles must be decompressed.
use std::collections::{HashMap, HashSet};

use crate::mvt::{DecodeError, FieldValue, Fields, packed};

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn field_bytes(number: u32, bytes: &[u8], out: &mut Vec<u8>) {
    varint(u64::from(number) << 3 | 2, out);
    varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn field_varint(number: u32, value: u64, out: &mut Vec<u8>) {
    varint(u64::from(number) << 3, out);
    varint(value, out);
}

/// A table of keys or values without repeats.
#[derive(Default)]
struct Table {
    items: Vec<Vec<u8>>,
    index: HashMap<Vec<u8>, u64>,
}

impl Table {
    fn add(&mut self, item: &[u8]) -> u64 {
        if let Some(i) = self.index.get(item) {
            return *i;
        }
        let i = self.items.len() as u64;
        self.items.push(item.to_vec());
        self.index.insert(item.to_vec(), i);
        i
    }
}

struct MergedLayer {
    name: Vec<u8>,
    version: u64,
    extent: u64,
    keys: Table,
    values: Table,
    features: Vec<Vec<u8>>,
    seen: HashSet<Vec<u8>>,
}

/// One layer of one input tile, borrowed.
struct RawLayer<'a> {
    name: &'a [u8],
    version: u64,
    extent: u64,
    keys: Vec<&'a [u8]>,
    values: Vec<&'a [u8]>,
    features: Vec<&'a [u8]>,
}

fn read_layer(buf: &[u8]) -> Result<RawLayer<'_>, DecodeError> {
    let mut layer = RawLayer {
        name: &[],
        version: 2,
        extent: 4096,
        keys: Vec::new(),
        values: Vec::new(),
        features: Vec::new(),
    };
    for field in Fields::new(buf) {
        match field? {
            (1, FieldValue::Bytes(b)) => layer.name = b,
            (2, FieldValue::Bytes(b)) => layer.features.push(b),
            (3, FieldValue::Bytes(b)) => layer.keys.push(b),
            (4, FieldValue::Bytes(b)) => layer.values.push(b),
            (5, FieldValue::Varint(v)) => layer.extent = v,
            (15, FieldValue::Varint(v)) => layer.version = v,
            _ => {}
        }
    }
    Ok(layer)
}

/// The feature with its tags renumbered through `keys` and `values`.
fn renumber(feature: &[u8], keys: &[u64], values: &[u64]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(feature.len() + 8);
    for field in Fields::new(feature) {
        match field? {
            (2, FieldValue::Bytes(b)) => {
                let tags = packed(b)?;
                let (pairs, rest) = tags.as_chunks::<2>();
                if !rest.is_empty() {
                    return Err(DecodeError("odd number of tags"));
                }
                let mut encoded = Vec::with_capacity(b.len() + 4);
                for [key, value] in pairs {
                    let k = usize::try_from(*key).ok().and_then(|i| keys.get(i));
                    let v = usize::try_from(*value).ok().and_then(|i| values.get(i));
                    let (Some(k), Some(v)) = (k, v) else {
                        return Err(DecodeError("tag index out of range"));
                    };
                    varint(*k, &mut encoded);
                    varint(*v, &mut encoded);
                }
                field_bytes(2, &encoded, &mut out);
            }
            (number, FieldValue::Bytes(b)) => field_bytes(number, b, &mut out),
            (number, FieldValue::Varint(v)) => field_varint(number, v, &mut out),
            (number, FieldValue::Fixed64(v)) => {
                varint(u64::from(number) << 3 | 1, &mut out);
                out.extend_from_slice(&v.to_le_bytes());
            }
            (number, FieldValue::Fixed32(v)) => {
                varint(u64::from(number) << 3 | 5, &mut out);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(out)
}

/// Combines decompressed tiles into one. A single tile is returned as it is.
/// A layer whose extent differs from the first layer of that name is left out.
pub fn merge_tiles(tiles: &[&[u8]]) -> Result<Vec<u8>, DecodeError> {
    match tiles {
        [] => return Ok(Vec::new()),
        [one] => return Ok(one.to_vec()),
        _ => {}
    }
    let mut layers: Vec<MergedLayer> = Vec::new();
    for tile in tiles {
        for field in Fields::new(tile) {
            let (3, FieldValue::Bytes(buf)) = field? else {
                continue;
            };
            let raw = read_layer(buf)?;
            let at = match layers.iter().position(|l| l.name == raw.name) {
                Some(at) => at,
                None => {
                    layers.push(MergedLayer {
                        name: raw.name.to_vec(),
                        version: raw.version,
                        extent: raw.extent,
                        keys: Table::default(),
                        values: Table::default(),
                        features: Vec::new(),
                        seen: HashSet::new(),
                    });
                    layers.len() - 1
                }
            };
            let layer = &mut layers[at];
            if layer.extent != raw.extent {
                continue;
            }
            let keys: Vec<u64> = raw.keys.iter().map(|k| layer.keys.add(k)).collect();
            let values: Vec<u64> = raw.values.iter().map(|v| layer.values.add(v)).collect();
            for feature in raw.features {
                let feature = renumber(feature, &keys, &values)?;
                if layer.seen.insert(feature.clone()) {
                    layer.features.push(feature);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(tiles.iter().map(|t| t.len()).sum());
    for layer in layers {
        let mut buf = Vec::new();
        field_varint(15, layer.version, &mut buf);
        field_bytes(1, &layer.name, &mut buf);
        for feature in &layer.features {
            field_bytes(2, feature, &mut buf);
        }
        for key in &layer.keys.items {
            field_bytes(3, key, &mut buf);
        }
        for value in &layer.values.items {
            field_bytes(4, value, &mut buf);
        }
        field_varint(5, layer.extent, &mut buf);
        field_bytes(3, &buf, &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::decode_tile;
    use crate::mvt::tests::encode_layer;

    fn names(tile: &[u8], layer: &str) -> Vec<String> {
        let layers = decode_tile(tile, &[layer]).unwrap();
        let mut names: Vec<String> = layers
            .iter()
            .flat_map(|l| &l.features)
            .map(|f| format!("{}:{}", f.str("name").unwrap(), f.str("kind").unwrap_or("")))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn joins_layers_and_renumbers_tags() {
        let west = [
            encode_layer(
                "place_labels",
                &[
                    (&[("name", "Cairo"), ("kind", "city")], 1, &[(10, 10)]),
                    // Mapped in both extracts: kept once.
                    (&[("name", "Wickliffe"), ("kind", "town")], 1, &[(40, 40)]),
                ],
            ),
            encode_layer("streets", &[(&[("kind", "primary")], 2, &[(0, 0), (5, 5)])]),
        ]
        .concat();
        let east = [
            // Keys and values in a different order than in the west tile.
            encode_layer(
                "place_labels",
                &[
                    (&[("kind", "town"), ("name", "Wickliffe")], 1, &[(40, 40)]),
                    (&[("kind", "village"), ("name", "Bardwell")], 1, &[(90, 90)]),
                ],
            ),
            encode_layer(
                "water_polygons",
                &[(&[("kind", "river")], 3, &[(1, 1), (2, 2)])],
            ),
        ]
        .concat();
        let merged = merge_tiles(&[&west, &east]).unwrap();
        let layers = decode_tile(&merged, &[]).unwrap();
        let layer_names: Vec<&str> = layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(layer_names, ["place_labels", "streets", "water_polygons"]);
        // Wickliffe's tags are in a different order in each tile, so its
        // encodings differ and both copies are kept; the same feature
        // encoded the same way is kept once (see below).
        let places = names(&merged, "place_labels");
        assert!(places.contains(&"Cairo:city".to_owned()));
        assert!(places.contains(&"Bardwell:village".to_owned()));
        assert_eq!(layers[1].features.len(), 1);
        assert_eq!(layers[2].features.len(), 1);

        let twice = merge_tiles(&[&west, &west]).unwrap();
        assert_eq!(
            names(&twice, "place_labels"),
            ["Cairo:city", "Wickliffe:town"]
        );
        assert_eq!(merge_tiles(&[&west]).unwrap(), west);
        assert!(merge_tiles(&[]).unwrap().is_empty());
    }

    #[test]
    fn rejects_bad_tags() {
        // A feature tagged with key index 5 in a layer with one key.
        let mut layer = Vec::new();
        field_bytes(1, b"pois", &mut layer);
        let mut feature = Vec::new();
        field_bytes(2, &[5, 0], &mut feature);
        field_bytes(2, &feature, &mut layer);
        field_bytes(3, b"name", &mut layer);
        let mut tile = Vec::new();
        field_bytes(3, &layer, &mut tile);
        assert!(merge_tiles(&[&tile, &tile]).is_err());
    }
}
