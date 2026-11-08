//! Minimal Mapbox Vector Tile (MVT 2.1) reader for building search data.
//!
//! Decodes layers, feature attributes and a single representative point per
//! feature: the first point of a point feature, the middle vertex of a line,
//! or the first vertex of a polygon ring. Rendering does not use this; it only
//! needs to find named things and where they are.
use std::collections::HashMap;
use std::f64::consts::PI;
use std::io::Read;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryType {
    Unknown,
    Point,
    Line,
    Polygon,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub properties: HashMap<String, Value>,
    pub geometry: GeometryType,
    /// Representative point in tile pixel coordinates (0..extent), if any.
    pub point: Option<(i32, i32)>,
}

impl Feature {
    pub fn str(&self, key: &str) -> Option<&str> {
        self.properties.get(key).and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    pub name: String,
    pub extent: u32,
    pub features: Vec<Feature>,
}

#[derive(Debug)]
pub struct DecodeError(pub &'static str);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid vector tile: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Decompresses gzip tiles, then decodes the layers named in `wanted`
/// (all layers when `wanted` is empty).
pub fn decode_tile(data: &[u8], wanted: &[&str]) -> Result<Vec<Layer>, DecodeError> {
    let mut inflated;
    let bytes = if data.starts_with(&[0x1f, 0x8b]) {
        inflated = Vec::with_capacity(data.len() * 4);
        flate2::read::GzDecoder::new(data)
            .take(64 << 20)
            .read_to_end(&mut inflated)
            .map_err(|_| DecodeError("gzip"))?;
        &inflated[..]
    } else {
        data
    };
    let mut layers = Vec::new();
    for field in Fields::new(bytes) {
        let (number, value) = field?;
        if number != 3 {
            continue;
        }
        let FieldValue::Bytes(layer) = value else {
            return Err(DecodeError("layer is not a message"));
        };
        if let Some(layer) = decode_layer(layer, wanted)? {
            layers.push(layer);
        }
    }
    Ok(layers)
}

fn decode_layer(buf: &[u8], wanted: &[&str]) -> Result<Option<Layer>, DecodeError> {
    let mut name = String::new();
    let mut extent = 4096;
    let mut keys = Vec::new();
    let mut values = Vec::new();
    let mut raw_features = Vec::new();
    for field in Fields::new(buf) {
        match field? {
            (1, FieldValue::Bytes(b)) => {
                name = String::from_utf8(b.to_vec()).map_err(|_| DecodeError("layer name"))?;
                if !wanted.is_empty() && !wanted.contains(&name.as_str()) {
                    return Ok(None);
                }
            }
            (2, FieldValue::Bytes(b)) => raw_features.push(b),
            (3, FieldValue::Bytes(b)) => {
                keys.push(String::from_utf8(b.to_vec()).map_err(|_| DecodeError("key"))?)
            }
            (4, FieldValue::Bytes(b)) => values.push(decode_value(b)?),
            (5, FieldValue::Varint(v)) => {
                extent = u32::try_from(v).map_err(|_| DecodeError("extent"))?
            }
            _ => {}
        }
    }
    let features = raw_features
        .into_iter()
        .map(|f| decode_feature(f, &keys, &values))
        .collect::<Result<_, _>>()?;
    Ok(Some(Layer {
        name,
        extent,
        features,
    }))
}

fn decode_value(buf: &[u8]) -> Result<Value, DecodeError> {
    let mut value = Value::Bool(false);
    for field in Fields::new(buf) {
        value = match field? {
            (1, FieldValue::Bytes(b)) => Value::String(
                String::from_utf8(b.to_vec()).map_err(|_| DecodeError("string value"))?,
            ),
            (2, FieldValue::Fixed32(v)) => Value::Number(f64::from(f32::from_bits(v))),
            (3, FieldValue::Fixed64(v)) => Value::Number(f64::from_bits(v)),
            (4 | 5, FieldValue::Varint(v)) => Value::Number(v as i64 as f64),
            (6, FieldValue::Varint(v)) => Value::Number(zigzag(v) as f64),
            (7, FieldValue::Varint(v)) => Value::Bool(v != 0),
            _ => continue,
        };
    }
    Ok(value)
}

fn decode_feature(buf: &[u8], keys: &[String], values: &[Value]) -> Result<Feature, DecodeError> {
    let mut properties = HashMap::new();
    let mut geometry = GeometryType::Unknown;
    let mut commands: &[u8] = &[];
    for field in Fields::new(buf) {
        match field? {
            (2, FieldValue::Bytes(tags)) => {
                let tags = packed(tags)?;
                for pair in tags.as_chunks::<2>().0 {
                    let key = keys.get(pair[0] as usize).ok_or(DecodeError("tag key"))?;
                    let value = values
                        .get(pair[1] as usize)
                        .ok_or(DecodeError("tag value"))?;
                    properties.insert(key.clone(), value.clone());
                }
            }
            (3, FieldValue::Varint(t)) => {
                geometry = match t {
                    1 => GeometryType::Point,
                    2 => GeometryType::Line,
                    3 => GeometryType::Polygon,
                    _ => GeometryType::Unknown,
                }
            }
            (4, FieldValue::Bytes(g)) => commands = g,
            _ => {}
        }
    }
    let point = representative_point(&packed(commands)?, geometry);
    Ok(Feature {
        properties,
        geometry,
        point,
    })
}

/// Walks geometry commands for the first part and picks one vertex.
fn representative_point(commands: &[u64], geometry: GeometryType) -> Option<(i32, i32)> {
    let (mut x, mut y) = (0_i64, 0_i64);
    let mut vertices = Vec::new();
    let mut i = 0;
    while i < commands.len() {
        let (id, count) = (commands[i] & 7, (commands[i] >> 3) as usize);
        i += 1;
        match id {
            1 | 2 => {
                if id == 1 && !vertices.is_empty() {
                    break; // Only the first point/line/ring.
                }
                for _ in 0..count {
                    let (dx, dy) = (*commands.get(i)?, *commands.get(i + 1)?);
                    i += 2;
                    x += zigzag(dx);
                    y += zigzag(dy);
                    vertices.push((x, y));
                }
            }
            7 => break,
            _ => return None,
        }
    }
    let (px, py) = match geometry {
        GeometryType::Line => *vertices.get(vertices.len() / 2)?,
        _ => *vertices.first()?,
    };
    Some((i32::try_from(px).ok()?, i32::try_from(py).ok()?))
}

/// Converts a pixel position in tile `z/x/y` (XYZ, north-up) to longitude/latitude.
pub fn tile_point_to_lon_lat(z: u8, x: u32, y: u32, extent: u32, px: i32, py: i32) -> (f64, f64) {
    let n = f64::from(1_u32 << z);
    let fx = (f64::from(x) + f64::from(px) / f64::from(extent)) / n;
    let fy = (f64::from(y) + f64::from(py) / f64::from(extent)) / n;
    let lon = fx * 360.0 - 180.0;
    let lat = (PI * (1.0 - 2.0 * fy)).sinh().atan().to_degrees();
    (lon, lat)
}

pub(crate) fn zigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

pub(crate) enum FieldValue<'a> {
    Varint(u64),
    Fixed64(u64),
    Bytes(&'a [u8]),
    Fixed32(u32),
}

pub(crate) struct Fields<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub(crate) fn varint(&mut self) -> Result<u64, DecodeError> {
        let mut result = 0_u64;
        for shift in (0..64).step_by(7) {
            let byte = *self
                .buf
                .get(self.pos)
                .ok_or(DecodeError("truncated varint"))?;
            self.pos += 1;
            result |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(result);
            }
        }
        Err(DecodeError("varint too long"))
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len());
        let end = end.ok_or(DecodeError("truncated field"))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u32, FieldValue<'a>), DecodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let result = (|| {
            let key = self.varint()?;
            let number = u32::try_from(key >> 3).map_err(|_| DecodeError("field number"))?;
            let value = match key & 7 {
                0 => FieldValue::Varint(self.varint()?),
                1 => FieldValue::Fixed64(u64::from_le_bytes(self.take(8)?.try_into().unwrap())),
                2 => {
                    let len = usize::try_from(self.varint()?).map_err(|_| DecodeError("length"))?;
                    FieldValue::Bytes(self.take(len)?)
                }
                5 => FieldValue::Fixed32(u32::from_le_bytes(self.take(4)?.try_into().unwrap())),
                _ => return Err(DecodeError("unsupported wire type")),
            };
            Ok((number, value))
        })();
        if result.is_err() {
            self.pos = self.buf.len();
        }
        Some(result)
    }
}

pub(crate) fn packed(buf: &[u8]) -> Result<Vec<u64>, DecodeError> {
    let mut fields = Fields::new(buf);
    let mut out = Vec::with_capacity(buf.len());
    while fields.pos < buf.len() {
        out.push(fields.varint()?);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

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

    fn zz(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }

    /// (properties, geometry type, vertices)
    pub(crate) type TestFeature<'a> = (&'a [(&'a str, &'a str)], u64, &'a [(i64, i64)]);

    /// Encodes one layer of features.
    pub(crate) fn encode_layer(name: &str, features: &[TestFeature]) -> Vec<u8> {
        let mut keys: Vec<&str> = Vec::new();
        let mut values: Vec<&str> = Vec::new();
        let mut layer = Vec::new();
        field_bytes(1, name.as_bytes(), &mut layer);
        for (props, kind, vertices) in features {
            let mut tags = Vec::new();
            for (k, v) in *props {
                let ki = keys.iter().position(|x| x == k).unwrap_or_else(|| {
                    keys.push(k);
                    keys.len() - 1
                });
                let vi = values.iter().position(|x| x == v).unwrap_or_else(|| {
                    values.push(v);
                    values.len() - 1
                });
                varint(ki as u64, &mut tags);
                varint(vi as u64, &mut tags);
            }
            let mut geometry = Vec::new();
            let (mut cx, mut cy) = (0, 0);
            for (i, (x, y)) in vertices.iter().enumerate() {
                if i == 0 {
                    varint(1 | (1 << 3), &mut geometry);
                } else if i == 1 {
                    varint(2 | ((vertices.len() as u64 - 1) << 3), &mut geometry);
                }
                varint(zz(x - cx), &mut geometry);
                varint(zz(y - cy), &mut geometry);
                (cx, cy) = (*x, *y);
            }
            let mut feature = Vec::new();
            field_bytes(2, &tags, &mut feature);
            varint(3 << 3, &mut feature);
            varint(*kind, &mut feature);
            field_bytes(4, &geometry, &mut feature);
            field_bytes(2, &feature, &mut layer);
        }
        for k in &keys {
            field_bytes(3, k.as_bytes(), &mut layer);
        }
        for v in &values {
            let mut value = Vec::new();
            // Test convention: "#6" encodes the number 6 as a double.
            match v.strip_prefix('#').and_then(|n| n.parse::<f64>().ok()) {
                Some(number) => {
                    varint(3 << 3 | 1, &mut value);
                    value.extend_from_slice(&number.to_bits().to_le_bytes());
                }
                None => field_bytes(1, v.as_bytes(), &mut value),
            }
            field_bytes(4, &value, &mut layer);
        }
        varint(5 << 3, &mut layer);
        varint(4096, &mut layer);
        varint(15 << 3, &mut layer);
        varint(2, &mut layer);
        let mut tile = Vec::new();
        field_bytes(3, &layer, &mut tile);
        tile
    }

    #[test]
    fn decodes_points_lines_and_properties() {
        let tile = encode_layer(
            "streets",
            &[
                (
                    &[("name", "Main Street"), ("kind", "primary")],
                    2,
                    &[(10, 10), (20, 10), (30, 40)],
                ),
                (&[("name", "Clinton")], 1, &[(100, -5)]),
            ],
        );
        let layers = decode_tile(&tile, &[]).unwrap();
        assert_eq!(layers.len(), 1);
        let layer = &layers[0];
        assert_eq!((layer.name.as_str(), layer.extent), ("streets", 4096));
        assert_eq!(layer.features[0].str("name"), Some("Main Street"));
        assert_eq!(layer.features[0].geometry, GeometryType::Line);
        assert_eq!(layer.features[0].point, Some((20, 10)));
        assert_eq!(layer.features[1].point, Some((100, -5)));
        assert!(decode_tile(&tile, &["pois"]).unwrap().is_empty());
    }

    #[test]
    fn decodes_gzip_and_rejects_garbage() {
        use std::io::Write;
        let tile = encode_layer("pois", &[(&[("name", "Hospital")], 1, &[(1, 2)])]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tile).unwrap();
        let layers = decode_tile(&gz.finish().unwrap(), &["pois"]).unwrap();
        assert_eq!(layers[0].features[0].str("name"), Some("Hospital"));
        assert!(decode_tile(&[0x1a, 0xff, 0xff], &[]).is_err());
        assert!(decode_tile(&[0x1f, 0x8b, 0, 0], &[]).is_err());
    }

    #[test]
    fn converts_tile_pixels_to_coordinates() {
        let (lon, lat) = tile_point_to_lon_lat(0, 0, 0, 4096, 2048, 2048);
        assert!(lon.abs() < 1e-9 && lat.abs() < 1e-9);
        // North-west corner of the Web Mercator world: the projection's latitude limit.
        let (lon, lat) = tile_point_to_lon_lat(1, 0, 0, 4096, 0, 0);
        assert!((lon + 180.0).abs() < 1e-9 && (lat - 85.051_128_779_8).abs() < 1e-9);
        // Chicago Loop (41.8818, -87.6298) lies inside z14 tile 4203/6089
        // (computed independently with the standard slippy-map formula).
        let (west, north) = tile_point_to_lon_lat(14, 4203, 6089, 4096, 0, 0);
        let (east, south) = tile_point_to_lon_lat(14, 4203, 6089, 4096, 4096, 4096);
        assert!((west..east).contains(&-87.6298) && (south..north).contains(&41.8818));
    }
}
