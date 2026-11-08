//! Minimal OpenStreetMap PBF reader for extracting a few kinds of feature.
//!
//! Reads the blob framing, zlib or uncompressed blobs, the file header and
//! primitive blocks (nodes, dense nodes, ways, relations, tags). Metadata such
//! as versions, users and timestamps is skipped. Blocks are decompressed and
//! decoded on worker threads; results are combined on the calling thread.
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};

use crate::mbtiles::Bounds;
use crate::mvt::{DecodeError, FieldValue, Fields, zigzag};

/// Limits from the PBF specification.
const MAX_BLOB_HEADER: usize = 64 * 1024;
const MAX_BLOB: usize = 32 << 20;
const SUPPORTED_FEATURES: [&str; 2] = ["OsmSchema-V0.6", "DenseNodes"];

#[derive(Debug)]
pub enum PbfError {
    Io(io::Error),
    Invalid(String),
    Unsupported(String),
}

impl std::fmt::Display for PbfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Invalid(e) => write!(f, "Invalid OpenStreetMap file: {e}"),
            Self::Unsupported(e) => write!(f, "Unsupported OpenStreetMap file: {e}"),
        }
    }
}

impl std::error::Error for PbfError {}

impl From<io::Error> for PbfError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DecodeError> for PbfError {
    fn from(error: DecodeError) -> Self {
        Self::Invalid(error.0.into())
    }
}

fn invalid(message: &str) -> PbfError {
    PbfError::Invalid(message.into())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct OsmHeader {
    pub bbox: Option<Bounds>,
    pub required_features: Vec<String>,
    pub writing_program: Option<String>,
    /// Seconds since 1970 of the data, when the writer recorded it.
    pub replication_timestamp: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Node,
    Way,
    Relation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member<'a> {
    pub kind: MemberKind,
    pub id: i64,
    pub role: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Element<'a> {
    Node { id: i64, lat: f64, lon: f64 },
    Way { id: i64, refs: &'a [i64] },
    Relation { id: i64, members: &'a [Member<'a>] },
}

pub type Tags<'a> = [(&'a str, &'a str)];

pub fn tag<'a>(tags: &Tags<'a>, key: &str) -> Option<&'a str> {
    tags.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Which element types a block visit decodes; the others are skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kinds {
    pub nodes: bool,
    pub ways: bool,
    pub relations: bool,
}

/// One decoded `OSMData` block.
pub struct Block<'a> {
    strings: Vec<&'a str>,
    groups: Vec<&'a [u8]>,
    granularity: i64,
    lat_offset: i64,
    lon_offset: i64,
}

fn packed_into(value: FieldValue<'_>, out: &mut Vec<u64>) -> Result<(), PbfError> {
    match value {
        FieldValue::Bytes(bytes) => {
            let mut fields = Fields::new(bytes);
            while !fields.is_empty() {
                out.push(fields.varint()?);
            }
        }
        FieldValue::Varint(v) => out.push(v),
        _ => return Err(invalid("unexpected field type")),
    }
    Ok(())
}

/// Reusable buffers for decoding one block.
#[derive(Default)]
struct Scratch {
    keys: Vec<u64>,
    values: Vec<u64>,
    refs: Vec<i64>,
    raw: Vec<u64>,
    roles: Vec<u64>,
    types: Vec<u64>,
}

impl<'a> Block<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, PbfError> {
        let mut block = Block {
            strings: Vec::new(),
            groups: Vec::new(),
            granularity: 100,
            lat_offset: 0,
            lon_offset: 0,
        };
        for field in Fields::new(data) {
            match field? {
                (1, FieldValue::Bytes(table)) => {
                    for entry in Fields::new(table) {
                        if let (1, FieldValue::Bytes(s)) = entry? {
                            // Invalid UTF-8 is treated as an empty string.
                            block.strings.push(std::str::from_utf8(s).unwrap_or(""));
                        }
                    }
                }
                (2, FieldValue::Bytes(group)) => block.groups.push(group),
                (17, FieldValue::Varint(v)) => block.granularity = v as i64,
                (19, FieldValue::Varint(v)) => block.lat_offset = v as i64,
                (20, FieldValue::Varint(v)) => block.lon_offset = v as i64,
                _ => {}
            }
        }
        if block.granularity <= 0 {
            return Err(invalid("granularity"));
        }
        Ok(block)
    }

    fn string(&self, index: u64) -> Result<&'a str, PbfError> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.strings.get(i))
            .copied()
            .ok_or_else(|| invalid("string index"))
    }

    fn lat(&self, raw: i64) -> f64 {
        1e-9 * (self.lat_offset + self.granularity * raw) as f64
    }

    fn lon(&self, raw: i64) -> f64 {
        1e-9 * (self.lon_offset + self.granularity * raw) as f64
    }

    fn pairs(
        &self,
        keys: &[u64],
        values: &[u64],
        tags: &mut Vec<(&'a str, &'a str)>,
    ) -> Result<(), PbfError> {
        if keys.len() != values.len() {
            return Err(invalid("tag keys and values differ in length"));
        }
        tags.clear();
        for (k, v) in keys.iter().zip(values) {
            tags.push((self.string(*k)?, self.string(*v)?));
        }
        Ok(())
    }

    /// Calls `visit` for every element of the requested kinds.
    pub fn for_each(
        &self,
        kinds: Kinds,
        visit: &mut dyn FnMut(Element<'_>, &Tags<'_>),
    ) -> Result<(), PbfError> {
        let mut scratch = Scratch::default();
        let mut tags: Vec<(&'a str, &'a str)> = Vec::new();
        for group in &self.groups {
            for field in Fields::new(group) {
                match field? {
                    (1, FieldValue::Bytes(node)) if kinds.nodes => {
                        self.node(node, &mut scratch, &mut tags, visit)?
                    }
                    (2, FieldValue::Bytes(dense)) if kinds.nodes => {
                        self.dense(dense, &mut tags, visit)?
                    }
                    (3, FieldValue::Bytes(way)) if kinds.ways => {
                        self.way(way, &mut scratch, &mut tags, visit)?
                    }
                    (4, FieldValue::Bytes(relation)) if kinds.relations => {
                        self.relation(relation, &mut scratch, &mut tags, visit)?
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn node(
        &self,
        buf: &[u8],
        s: &mut Scratch,
        tags: &mut Vec<(&'a str, &'a str)>,
        visit: &mut dyn FnMut(Element<'_>, &Tags<'_>),
    ) -> Result<(), PbfError> {
        let (mut id, mut lat, mut lon) = (0, 0, 0);
        s.keys.clear();
        s.values.clear();
        for field in Fields::new(buf) {
            match field? {
                (1, FieldValue::Varint(v)) => id = zigzag(v),
                (2, value) => packed_into(value, &mut s.keys)?,
                (3, value) => packed_into(value, &mut s.values)?,
                (8, FieldValue::Varint(v)) => lat = zigzag(v),
                (9, FieldValue::Varint(v)) => lon = zigzag(v),
                _ => {}
            }
        }
        self.pairs(&s.keys, &s.values, tags)?;
        visit(
            Element::Node {
                id,
                lat: self.lat(lat),
                lon: self.lon(lon),
            },
            tags,
        );
        Ok(())
    }

    fn dense(
        &self,
        buf: &[u8],
        tags: &mut Vec<(&'a str, &'a str)>,
        visit: &mut dyn FnMut(Element<'_>, &Tags<'_>),
    ) -> Result<(), PbfError> {
        let (mut ids, mut lats, mut lons, mut kvs) = (&[][..], &[][..], &[][..], &[][..]);
        for field in Fields::new(buf) {
            match field? {
                (1, FieldValue::Bytes(b)) => ids = b,
                (8, FieldValue::Bytes(b)) => lats = b,
                (9, FieldValue::Bytes(b)) => lons = b,
                (10, FieldValue::Bytes(b)) => kvs = b,
                _ => {}
            }
        }
        let (mut ids, mut lats, mut lons) =
            (Fields::new(ids), Fields::new(lats), Fields::new(lons));
        let mut kvs = (!kvs.is_empty()).then(|| Fields::new(kvs));
        let (mut id, mut lat, mut lon) = (0_i64, 0_i64, 0_i64);
        while !ids.is_empty() {
            id = id.wrapping_add(zigzag(ids.varint()?));
            lat = lat.wrapping_add(zigzag(lats.varint()?));
            lon = lon.wrapping_add(zigzag(lons.varint()?));
            tags.clear();
            if let Some(kvs) = kvs.as_mut() {
                loop {
                    let key = kvs.varint()?;
                    if key == 0 {
                        break;
                    }
                    tags.push((self.string(key)?, self.string(kvs.varint()?)?));
                }
            }
            visit(
                Element::Node {
                    id,
                    lat: self.lat(lat),
                    lon: self.lon(lon),
                },
                tags,
            );
        }
        if !lats.is_empty() || !lons.is_empty() {
            return Err(invalid("dense node arrays differ in length"));
        }
        Ok(())
    }

    fn way(
        &self,
        buf: &[u8],
        s: &mut Scratch,
        tags: &mut Vec<(&'a str, &'a str)>,
        visit: &mut dyn FnMut(Element<'_>, &Tags<'_>),
    ) -> Result<(), PbfError> {
        let mut id = 0;
        s.keys.clear();
        s.values.clear();
        s.raw.clear();
        for field in Fields::new(buf) {
            match field? {
                (1, FieldValue::Varint(v)) => id = v as i64,
                (2, value) => packed_into(value, &mut s.keys)?,
                (3, value) => packed_into(value, &mut s.values)?,
                (8, value) => packed_into(value, &mut s.raw)?,
                _ => {}
            }
        }
        self.pairs(&s.keys, &s.values, tags)?;
        s.refs.clear();
        let mut node = 0_i64;
        for delta in &s.raw {
            node = node.wrapping_add(zigzag(*delta));
            s.refs.push(node);
        }
        visit(Element::Way { id, refs: &s.refs }, tags);
        Ok(())
    }

    fn relation(
        &self,
        buf: &[u8],
        s: &mut Scratch,
        tags: &mut Vec<(&'a str, &'a str)>,
        visit: &mut dyn FnMut(Element<'_>, &Tags<'_>),
    ) -> Result<(), PbfError> {
        let mut id = 0;
        s.keys.clear();
        s.values.clear();
        s.raw.clear();
        s.roles.clear();
        s.types.clear();
        for field in Fields::new(buf) {
            match field? {
                (1, FieldValue::Varint(v)) => id = v as i64,
                (2, value) => packed_into(value, &mut s.keys)?,
                (3, value) => packed_into(value, &mut s.values)?,
                (8, value) => packed_into(value, &mut s.roles)?,
                (9, value) => packed_into(value, &mut s.raw)?,
                (10, value) => packed_into(value, &mut s.types)?,
                _ => {}
            }
        }
        if s.raw.len() != s.roles.len() || s.raw.len() != s.types.len() {
            return Err(invalid("relation member arrays differ in length"));
        }
        self.pairs(&s.keys, &s.values, tags)?;
        let mut members = Vec::with_capacity(s.raw.len());
        let mut member = 0_i64;
        for ((delta, role), kind) in s.raw.iter().zip(&s.roles).zip(&s.types) {
            member = member.wrapping_add(zigzag(*delta));
            members.push(Member {
                kind: match kind {
                    0 => MemberKind::Node,
                    1 => MemberKind::Way,
                    2 => MemberKind::Relation,
                    _ => return Err(invalid("member type")),
                },
                id: member,
                role: self.string(*role)?,
            });
        }
        visit(
            Element::Relation {
                id,
                members: &members,
            },
            tags,
        );
        Ok(())
    }
}

fn read_exact_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "OpenStreetMap file is truncated",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Reads the next blob: its type and still-compressed body. `None` at the end.
fn read_blob(
    reader: &mut impl Read,
    read: &mut u64,
) -> Result<Option<(String, Vec<u8>)>, PbfError> {
    let mut length = [0; 4];
    if !read_exact_or_eof(reader, &mut length)? {
        return Ok(None);
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_BLOB_HEADER {
        return Err(invalid("blob header too large"));
    }
    let mut header = vec![0; length];
    reader.read_exact(&mut header)?;
    let (mut kind, mut size) = (None, None);
    for field in Fields::new(&header) {
        match field? {
            (1, FieldValue::Bytes(b)) => kind = Some(String::from_utf8_lossy(b).into_owned()),
            (3, FieldValue::Varint(v)) => size = Some(v as usize),
            _ => {}
        }
    }
    let (Some(kind), Some(size)) = (kind, size) else {
        return Err(invalid("blob header"));
    };
    if size > MAX_BLOB {
        return Err(invalid("blob too large"));
    }
    let mut blob = vec![0; size];
    reader.read_exact(&mut blob)?;
    *read += (4 + length + size) as u64;
    Ok(Some((kind, blob)))
}

fn inflate(blob: &[u8]) -> Result<Vec<u8>, PbfError> {
    let mut raw_size = None;
    for field in Fields::new(blob) {
        match field? {
            (2, FieldValue::Varint(v)) => raw_size = Some(v as usize),
            (1, FieldValue::Bytes(raw)) => return Ok(raw.to_vec()),
            (3, FieldValue::Bytes(zlib)) => {
                let expected = raw_size.ok_or_else(|| invalid("blob size"))?;
                if expected > MAX_BLOB {
                    return Err(invalid("blob too large"));
                }
                let mut out = Vec::with_capacity(expected);
                flate2::read::ZlibDecoder::new(zlib)
                    .take(MAX_BLOB as u64 + 1)
                    .read_to_end(&mut out)
                    .map_err(|_| invalid("zlib data"))?;
                if out.len() != expected {
                    return Err(invalid("blob size does not match"));
                }
                return Ok(out);
            }
            (4 | 6 | 7, _) => {
                return Err(PbfError::Unsupported("compression other than zlib".into()));
            }
            _ => {}
        }
    }
    Err(invalid("empty blob"))
}

fn parse_header(data: &[u8]) -> Result<OsmHeader, PbfError> {
    let mut header = OsmHeader::default();
    for field in Fields::new(data) {
        match field? {
            (1, FieldValue::Bytes(bbox)) => {
                let mut edges = [0_i64; 4];
                for edge in Fields::new(bbox) {
                    if let (n @ 1..=4, FieldValue::Varint(v)) = edge? {
                        edges[n as usize - 1] = zigzag(v);
                    }
                }
                let [left, right, top, bottom] = edges.map(|v| v as f64 * 1e-9);
                header.bbox = Bounds::new(left, bottom, right, top);
            }
            (4, FieldValue::Bytes(b)) => header
                .required_features
                .push(String::from_utf8_lossy(b).into_owned()),
            (16, FieldValue::Bytes(b)) => {
                header.writing_program = Some(String::from_utf8_lossy(b).into_owned())
            }
            (32, FieldValue::Varint(v)) => header.replication_timestamp = Some(v as i64),
            _ => {}
        }
    }
    if let Some(feature) = header
        .required_features
        .iter()
        .find(|f| !SUPPORTED_FEATURES.contains(&f.as_str()))
    {
        return Err(PbfError::Unsupported(format!("requires {feature}")));
    }
    Ok(header)
}

/// Reads the whole file. `visit_block` runs on worker threads for each data
/// block; its results are passed to `fold` on this thread, in no particular
/// order. `progress` receives (bytes read, file length).
pub fn scan<T: Send>(
    path: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
    visit_block: &(dyn Fn(&Block<'_>) -> Result<T, PbfError> + Sync),
    fold: &mut dyn FnMut(T),
) -> Result<OsmHeader, PbfError> {
    let file = File::open(path)?;
    let total = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut read = 0;
    let (kind, blob) = read_blob(&mut reader, &mut read)?.ok_or_else(|| invalid("empty file"))?;
    if kind != "OSMHeader" {
        return Err(invalid("missing header"));
    }
    let header = parse_header(&inflate(&blob)?)?;
    let threads = std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .clamp(1, 8);

    let (blob_tx, blob_rx) = mpsc::sync_channel::<Vec<u8>>(threads * 2);
    let blob_rx = Mutex::new(blob_rx);
    let (out_tx, out_rx) = mpsc::channel::<Result<T, PbfError>>();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let (blob_rx, out_tx) = (&blob_rx, out_tx.clone());
            scope.spawn(move || {
                loop {
                    let next = blob_rx
                        .lock()
                        .map_err(|_| ())
                        .and_then(|rx| rx.recv().map_err(|_| ()));
                    let Ok(blob) = next else { break };
                    let result = inflate(&blob)
                        .and_then(|data| Block::parse(&data).and_then(|block| visit_block(&block)));
                    if out_tx.send(result).is_err() {
                        break;
                    }
                }
            });
        }
        drop(out_tx);

        let mut failure: Option<PbfError> = None;
        let mut handle = |result: Result<T, PbfError>, failure: &mut Option<PbfError>| match result
        {
            Ok(value) if failure.is_none() => fold(value),
            Ok(_) => {}
            Err(error) => {
                failure.get_or_insert(error);
            }
        };
        loop {
            while let Ok(result) = out_rx.try_recv() {
                handle(result, &mut failure);
            }
            if failure.is_some() {
                break;
            }
            if cancelled.load(Ordering::Relaxed) {
                failure = Some(PbfError::Io(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Cancelled",
                )));
                break;
            }
            match read_blob(&mut reader, &mut read) {
                Ok(None) => break,
                Ok(Some((kind, blob))) => {
                    if kind == "OSMData" && blob_tx.send(blob).is_err() {
                        break;
                    }
                    progress(read, total);
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        drop(blob_tx);
        for result in out_rx {
            handle(result, &mut failure);
        }
        failure.map_or(Ok(()), Err)
    })?;
    Ok(header)
}

#[cfg(test)]
pub(crate) mod tests {
    //! A small PBF writer for tests.
    use super::*;
    use std::io::Write;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn key(number: u32, wire: u32, out: &mut Vec<u8>) {
        varint(u64::from(number << 3 | wire), out);
    }
    fn bytes(number: u32, data: &[u8], out: &mut Vec<u8>) {
        key(number, 2, out);
        varint(data.len() as u64, out);
        out.extend_from_slice(data);
    }
    fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
        key(number, 0, out);
        varint(v, out);
    }
    fn zz(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }
    fn packed(values: impl IntoIterator<Item = u64>) -> Vec<u8> {
        let mut out = Vec::new();
        for v in values {
            varint(v, &mut out);
        }
        out
    }
    fn deltas(values: &[i64]) -> Vec<u64> {
        let mut last = 0;
        values
            .iter()
            .map(|v| {
                let d = v - last;
                last = *v;
                zz(d)
            })
            .collect()
    }

    pub(crate) enum TestElement<'a> {
        Node(i64, f64, f64, &'a [(&'a str, &'a str)]),
        Way(i64, &'a [i64], &'a [(&'a str, &'a str)]),
        /// Members are ways with the given role.
        Relation(i64, &'a [(i64, &'a str)], &'a [(&'a str, &'a str)]),
    }

    fn blob(kind: &str, data: &[u8], compress: bool, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        if compress {
            let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            z.write_all(data).unwrap();
            uint(2, data.len() as u64, &mut body);
            bytes(3, &z.finish().unwrap(), &mut body);
        } else {
            bytes(1, data, &mut body);
        }
        let mut header = Vec::new();
        bytes(1, kind.as_bytes(), &mut header);
        uint(3, body.len() as u64, &mut header);
        out.extend_from_slice(&(header.len() as u32).to_be_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&body);
    }

    /// Writes a file with a header and one block per slice of elements. Nodes
    /// with tags are written as plain nodes, others as dense nodes.
    pub(crate) fn write_pbf(blocks: &[&[TestElement]], required: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut header = Vec::new();
        let mut bbox = Vec::new();
        for (n, v) in [(1, -91.6), (2, -87.4), (3, 42.6), (4, 36.9)] {
            uint(n, zz((v * 1e9) as i64), &mut bbox);
        }
        bytes(1, &bbox, &mut header);
        for feature in required {
            bytes(4, feature.as_bytes(), &mut header);
        }
        bytes(16, b"test", &mut header);
        blob("OSMHeader", &header, false, &mut out);

        for elements in blocks {
            let mut strings: Vec<String> = vec![String::new()];
            let mut sid = |s: &str| -> u64 {
                if let Some(i) = strings.iter().position(|x| x == s) {
                    return i as u64;
                }
                strings.push(s.to_owned());
                (strings.len() - 1) as u64
            };
            let mut group = Vec::new();
            let (mut dense_ids, mut dense_lat, mut dense_lon) =
                (Vec::new(), Vec::new(), Vec::new());
            let coord = |v: f64| (v * 1e7).round() as i64;
            let tag_arrays = |tags: &[(&str, &str)], sid: &mut dyn FnMut(&str) -> u64| {
                let keys: Vec<u64> = tags.iter().map(|(k, _)| sid(k)).collect();
                let vals: Vec<u64> = tags.iter().map(|(_, v)| sid(v)).collect();
                (packed(keys), packed(vals))
            };
            for element in elements.iter() {
                match element {
                    TestElement::Node(id, lat, lon, []) => {
                        dense_ids.push(*id);
                        dense_lat.push(coord(*lat));
                        dense_lon.push(coord(*lon));
                    }
                    TestElement::Node(id, lat, lon, tags) => {
                        let mut node = Vec::new();
                        uint(1, zz(*id), &mut node);
                        let (k, v) = tag_arrays(tags, &mut sid);
                        bytes(2, &k, &mut node);
                        bytes(3, &v, &mut node);
                        uint(8, zz(coord(*lat)), &mut node);
                        uint(9, zz(coord(*lon)), &mut node);
                        bytes(1, &node, &mut group);
                    }
                    TestElement::Way(id, refs, tags) => {
                        let mut way = Vec::new();
                        uint(1, *id as u64, &mut way);
                        let (k, v) = tag_arrays(tags, &mut sid);
                        bytes(2, &k, &mut way);
                        bytes(3, &v, &mut way);
                        bytes(8, &packed(deltas(refs)), &mut way);
                        bytes(3, &way, &mut group);
                    }
                    TestElement::Relation(id, members, tags) => {
                        let mut relation = Vec::new();
                        uint(1, *id as u64, &mut relation);
                        let (k, v) = tag_arrays(tags, &mut sid);
                        bytes(2, &k, &mut relation);
                        bytes(3, &v, &mut relation);
                        let roles: Vec<u64> = members.iter().map(|(_, r)| sid(r)).collect();
                        let ids: Vec<i64> = members.iter().map(|(m, _)| *m).collect();
                        bytes(8, &packed(roles), &mut relation);
                        bytes(9, &packed(deltas(&ids)), &mut relation);
                        bytes(10, &packed(members.iter().map(|_| 1)), &mut relation);
                        bytes(4, &relation, &mut group);
                    }
                }
            }
            if !dense_ids.is_empty() {
                let mut dense = Vec::new();
                bytes(1, &packed(deltas(&dense_ids)), &mut dense);
                bytes(8, &packed(deltas(&dense_lat)), &mut dense);
                bytes(9, &packed(deltas(&dense_lon)), &mut dense);
                let mut dense_group = Vec::new();
                bytes(2, &dense, &mut dense_group);
                group.splice(0..0, dense_group);
            }
            let mut table = Vec::new();
            for s in &strings {
                bytes(1, s.as_bytes(), &mut table);
            }
            let mut block = Vec::new();
            bytes(1, &table, &mut block);
            bytes(2, &group, &mut block);
            uint(17, 100, &mut block);
            blob("OSMData", &block, true, &mut out);
        }
        out
    }

    fn collect(path: &Path) -> (OsmHeader, Vec<String>) {
        let mut seen = Vec::new();
        let header = scan(
            path,
            &AtomicBool::new(false),
            &mut |_, _| {},
            &|block| {
                let mut out = Vec::new();
                block.for_each(
                    Kinds {
                        nodes: true,
                        ways: true,
                        relations: true,
                    },
                    &mut |element, tags| {
                        let tags: Vec<String> =
                            tags.iter().map(|(k, v)| format!("{k}={v}")).collect();
                        out.push(match element {
                            Element::Node { id, lat, lon } => {
                                format!("n{id} {lat:.4},{lon:.4} {}", tags.join(";"))
                            }
                            Element::Way { id, refs } => {
                                format!("w{id} {refs:?} {}", tags.join(";"))
                            }
                            Element::Relation { id, members } => format!(
                                "r{id} {:?} {}",
                                members.iter().map(|m| (m.id, m.role)).collect::<Vec<_>>(),
                                tags.join(";")
                            ),
                        });
                    },
                )?;
                Ok(out)
            },
            &mut |found| seen.extend(found),
        )
        .unwrap();
        seen.sort();
        (header, seen)
    }

    #[test]
    fn reads_nodes_ways_relations_and_header() {
        let dir = crate::store::tests::TempDir::new();
        let path = dir.0.join("t.osm.pbf");
        let fuel: &[(&str, &str)] = &[("amenity", "fuel"), ("name", "Café Fuel")];
        let data = write_pbf(
            &[
                &[
                    TestElement::Node(1, 40.15, -88.96, &[]),
                    TestElement::Node(2, 40.1501, -88.9601, &[]),
                    TestElement::Node(-3, -12.5, 130.25, fuel),
                ],
                &[TestElement::Way(10, &[1, 2, 1], fuel)],
                &[TestElement::Relation(
                    20,
                    &[(10, "outer")],
                    &[("type", "multipolygon")],
                )],
            ],
            &["OsmSchema-V0.6", "DenseNodes"],
        );
        std::fs::write(&path, data).unwrap();
        let (header, seen) = collect(&path);
        assert_eq!(header.writing_program.as_deref(), Some("test"));
        let bbox = header.bbox.unwrap();
        assert!((bbox.west + 91.6).abs() < 1e-9 && (bbox.north - 42.6).abs() < 1e-9);
        assert_eq!(
            seen,
            [
                "n-3 -12.5000,130.2500 amenity=fuel;name=Café Fuel",
                "n1 40.1500,-88.9600 ",
                "n2 40.1501,-88.9601 ",
                "r20 [(10, \"outer\")] type=multipolygon",
                "w10 [1, 2, 1] amenity=fuel;name=Café Fuel",
            ]
        );
    }

    #[test]
    fn rejects_unsupported_and_damaged_files() {
        let dir = crate::store::tests::TempDir::new();
        let path = dir.0.join("t.osm.pbf");
        let scan_err = |data: &[u8]| {
            std::fs::write(&path, data).unwrap();
            scan(
                &path,
                &AtomicBool::new(false),
                &mut |_, _| {},
                &|_| Ok(()),
                &mut |_| {},
            )
            .unwrap_err()
        };
        let historical = write_pbf(&[], &["OsmSchema-V0.6", "HistoricalInformation"]);
        assert!(matches!(scan_err(&historical), PbfError::Unsupported(_)));

        let good = write_pbf(&[&[TestElement::Node(1, 40.0, -89.0, &[])]], &[]);
        assert!(matches!(scan_err(&good[..good.len() - 3]), PbfError::Io(_)));
        assert!(matches!(
            scan_err(b"not a pbf file at all"),
            PbfError::Invalid(_)
        ));
        let mut corrupt = good.clone();
        let last = corrupt.len() - 10;
        corrupt[last] ^= 0xff;
        assert!(matches!(scan_err(&corrupt), PbfError::Invalid(_)));
    }
}
