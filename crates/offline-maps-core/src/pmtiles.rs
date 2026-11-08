//! Reads the index of a remote PMTiles v3 archive, to find where each tile of
//! an area is without downloading the archive. Tile data is fetched separately
//! (see `terrain`).
//!
//! Format: <https://github.com/protomaps/PMTiles/blob/main/spec/v3/spec.md>.
use std::collections::HashMap;
use std::io::{self, Read};

pub const HEADER_LENGTH: u64 = 127;

/// Tile types and compressions this reader accepts.
pub const TILE_TYPE_WEBP: u8 = 4;
pub const COMPRESSION_NONE: u8 = 1;
const COMPRESSION_GZIP: u8 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub root_offset: u64,
    pub root_length: u64,
    pub leaf_offset: u64,
    pub leaf_length: u64,
    pub data_offset: u64,
    pub data_length: u64,
    pub internal_compression: u8,
    pub tile_compression: u8,
    pub tile_type: u8,
    pub min_zoom: u8,
    pub max_zoom: u8,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("PMTiles: {message}"))
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

impl Header {
    pub fn parse(b: &[u8]) -> io::Result<Self> {
        if b.len() < HEADER_LENGTH as usize || &b[..7] != b"PMTiles" {
            return Err(invalid("not a PMTiles archive"));
        }
        if b[7] != 3 {
            return Err(invalid("only version 3 is supported"));
        }
        Ok(Self {
            root_offset: u64_at(b, 8),
            root_length: u64_at(b, 16),
            leaf_offset: u64_at(b, 40),
            leaf_length: u64_at(b, 48),
            data_offset: u64_at(b, 56),
            data_length: u64_at(b, 64),
            internal_compression: b[97],
            tile_compression: b[98],
            tile_type: b[99],
            min_zoom: b[100],
            max_zoom: b[101],
        })
    }
}

/// One directory entry: tiles `tile_id..tile_id + run_length` share the data
/// at `offset`; a run length of 0 points to a leaf directory instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub tile_id: u64,
    pub offset: u64,
    pub length: u32,
    pub run_length: u32,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> io::Result<u64> {
    let mut value = 0_u64;
    for shift in (0..64).step_by(7) {
        let byte = *buf
            .get(*pos)
            .ok_or_else(|| invalid("truncated directory"))?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err(invalid("varint too long"))
}

/// Decodes a directory (after decompression).
pub fn parse_directory(buf: &[u8]) -> io::Result<Vec<Entry>> {
    let mut pos = 0;
    let n = usize::try_from(read_varint(buf, &mut pos)?).map_err(|_| invalid("count"))?;
    if n > buf.len() {
        return Err(invalid("entry count larger than the directory"));
    }
    let mut entries = vec![
        Entry {
            tile_id: 0,
            offset: 0,
            length: 0,
            run_length: 0,
        };
        n
    ];
    let mut last = 0_u64;
    for e in &mut entries {
        last = last
            .checked_add(read_varint(buf, &mut pos)?)
            .ok_or_else(|| invalid("tile id overflow"))?;
        e.tile_id = last;
    }
    for e in &mut entries {
        e.run_length = u32::try_from(read_varint(buf, &mut pos)?).map_err(|_| invalid("run"))?;
    }
    for e in &mut entries {
        e.length = u32::try_from(read_varint(buf, &mut pos)?).map_err(|_| invalid("length"))?;
    }
    for i in 0..n {
        let v = read_varint(buf, &mut pos)?;
        entries[i].offset = if v == 0 && i > 0 {
            entries[i - 1].offset + u64::from(entries[i - 1].length)
        } else {
            v.checked_sub(1).ok_or_else(|| invalid("offset"))?
        };
    }
    Ok(entries)
}

/// The Hilbert-curve tile ID of z/x/y.
pub fn tile_id(z: u8, x: u32, y: u32) -> u64 {
    let base: u64 = (0..z).map(|i| 1_u64 << (2 * u32::from(i))).sum();
    let n = 1_u64 << z;
    let (mut x, mut y) = (u64::from(x), u64::from(y));
    let mut d = 0_u64;
    let mut s = n >> 1;
    while s > 0 {
        let rx = u64::from(x & s != 0);
        let ry = u64::from(y & s != 0);
        d += s * s * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                // As in the reference implementation, these may go below
                // zero; only the bits below `s` are read afterwards.
                x = (s - 1).wrapping_sub(x);
                y = (s - 1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    base + d
}

/// The entry covering `id` in a sorted directory, if any.
fn find(entries: &[Entry], id: u64) -> Option<&Entry> {
    let at = entries.partition_point(|e| e.tile_id <= id);
    entries[..at].last()
}

/// Where a tile's bytes are in the archive (absolute offset, length).
pub type Location = (u64, u32);

/// A remote archive's index, read on demand through `fetch(offset, length)`.
pub struct Index<F: FnMut(u64, u64) -> io::Result<Vec<u8>>> {
    header: Header,
    root: Vec<Entry>,
    leaves: HashMap<u64, Vec<Entry>>,
    fetch: F,
}

impl<F: FnMut(u64, u64) -> io::Result<Vec<u8>>> Index<F> {
    /// Reads the header and root directory.
    pub fn open(mut fetch: F) -> io::Result<Self> {
        let header = Header::parse(&fetch(0, HEADER_LENGTH)?)?;
        Self::with_header(header, fetch)
    }

    /// Reads the root directory of an archive whose header was already read.
    pub fn with_header(header: Header, mut fetch: F) -> io::Result<Self> {
        if header.root_length > 16 << 20 || header.root_length == 0 {
            return Err(invalid("root directory size"));
        }
        let raw = fetch(header.root_offset, header.root_length)?;
        let root = parse_directory(&decompress(header.internal_compression, &raw)?)?;
        Ok(Self {
            header,
            root,
            leaves: HashMap::new(),
            fetch,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Where tile z/x/y is, or `None` when the archive has no such tile.
    pub fn locate(&mut self, z: u8, x: u32, y: u32) -> io::Result<Option<Location>> {
        let id = tile_id(z, x, y);
        let mut leaf_key = None;
        for _ in 0..4 {
            let entries = match leaf_key {
                None => &self.root,
                Some(key) => &self.leaves[&key],
            };
            let Some(e) = find(entries, id).copied() else {
                return Ok(None);
            };
            if e.run_length > 0 {
                return Ok((id < e.tile_id + u64::from(e.run_length))
                    .then_some((self.header.data_offset + e.offset, e.length)));
            }
            if !self.leaves.contains_key(&e.offset) {
                if e.offset + u64::from(e.length) > self.header.leaf_length || e.length > 16 << 20 {
                    return Err(invalid("leaf directory out of range"));
                }
                let raw = (self.fetch)(self.header.leaf_offset + e.offset, u64::from(e.length))?;
                let leaf = parse_directory(&decompress(self.header.internal_compression, &raw)?)?;
                self.leaves.insert(e.offset, leaf);
            }
            leaf_key = Some(e.offset);
        }
        Err(invalid("directories nested too deeply"))
    }
}

fn decompress(compression: u8, raw: &[u8]) -> io::Result<Vec<u8>> {
    match compression {
        COMPRESSION_NONE => Ok(raw.to_vec()),
        COMPRESSION_GZIP => {
            let mut out = Vec::with_capacity(raw.len() * 4);
            flate2::read::GzDecoder::new(raw)
                .take(256 << 20)
                .read_to_end(&mut out)?;
            Ok(out)
        }
        _ => Err(invalid("unsupported directory compression")),
    }
}

#[cfg(test)]
mod tests {
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

    fn directory(entries: &[Entry]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(entries.len() as u64, &mut out);
        let mut last = 0;
        for e in entries {
            varint(e.tile_id - last, &mut out);
            last = e.tile_id;
        }
        for e in entries {
            varint(e.run_length.into(), &mut out);
        }
        for e in entries {
            varint(e.length.into(), &mut out);
        }
        for (i, e) in entries.iter().enumerate() {
            let contiguous =
                i > 0 && e.offset == entries[i - 1].offset + u64::from(entries[i - 1].length);
            varint(if contiguous { 0 } else { e.offset + 1 }, &mut out);
        }
        out
    }

    #[test]
    fn tile_ids_follow_the_spec() {
        assert_eq!(tile_id(0, 0, 0), 0);
        assert_eq!(tile_id(1, 0, 0), 1);
        assert_eq!(tile_id(1, 0, 1), 2);
        assert_eq!(tile_id(1, 1, 1), 3);
        assert_eq!(tile_id(1, 1, 0), 4);
        assert_eq!(tile_id(2, 0, 0), 5);
        // From the specification's test suite.
        assert_eq!(tile_id(12, 3423, 1763), 19_078_479);
    }

    #[test]
    fn reads_root_and_leaf_directories() {
        let e = |tile_id, offset, length, run_length| Entry {
            tile_id,
            offset,
            length,
            run_length,
        };
        // Tiles 0 and 1 in the root; tiles 5.. through a leaf.
        let leaf = directory(&[e(5, 300, 10, 1), e(6, 310, 20, 2)]);
        let root = directory(&[
            e(0, 0, 100, 1),
            e(1, 100, 50, 1),
            e(5, 0, leaf.len() as u32, 0),
        ]);
        let mut header = vec![0_u8; HEADER_LENGTH as usize];
        header[..7].copy_from_slice(b"PMTiles");
        header[7] = 3;
        let put =
            |h: &mut Vec<u8>, at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
        put(&mut header, 8, HEADER_LENGTH);
        put(&mut header, 16, root.len() as u64);
        put(&mut header, 40, 1000);
        put(&mut header, 48, leaf.len() as u64);
        put(&mut header, 56, 5000);
        header[97] = COMPRESSION_NONE;
        header[98] = COMPRESSION_NONE;
        header[99] = TILE_TYPE_WEBP;
        let mut file = header.clone();
        file.extend_from_slice(&root);
        file.resize(1000, 0);
        file.extend_from_slice(&leaf);
        let mut requests = 0;
        let mut index = Index::open(|offset, length| {
            requests += 1;
            Ok(file[offset as usize..(offset + length) as usize].to_vec())
        })
        .unwrap();
        assert_eq!(index.header().tile_type, TILE_TYPE_WEBP);
        assert_eq!(index.locate(0, 0, 0).unwrap(), Some((5000, 100)));
        assert_eq!(index.locate(1, 0, 0).unwrap(), Some((5100, 50)));
        assert_eq!(index.locate(1, 0, 1).unwrap(), None);
        // tile_id(2, 0, 0) = 5; (2, 1, 0) = 6 and (2, 1, 1) = 7 share a run;
        // (2, 0, 1) = 8 is past it.
        assert_eq!(tile_id(2, 1, 0), 6);
        assert_eq!(tile_id(2, 0, 1), 8);
        assert_eq!(index.locate(2, 0, 0).unwrap(), Some((5300, 10)));
        assert_eq!(index.locate(2, 1, 0).unwrap(), Some((5310, 20)));
        assert_eq!(index.locate(2, 1, 1).unwrap(), Some((5310, 20)));
        assert_eq!(index.locate(2, 0, 1).unwrap(), None);
        drop(index);
        // Header, root, and the leaf once.
        assert_eq!(requests, 3);
        assert!(Header::parse(b"not pmtiles").is_err());
    }
}
