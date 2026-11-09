//! Offline map package foundations. No network, credentials, or UI dependencies.
pub mod addresses;
pub mod download;
pub mod extras;
pub mod mbtiles;
pub mod merge;
pub mod mvt;
pub mod osm_pbf;
pub mod overlays;
pub mod package;
pub mod places;
pub mod pmtiles;
pub mod regions;
pub mod search;
pub mod store;
pub mod terrain;
pub mod time;

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};

pub const ILLINOIS_SOURCE_PAGE: &str =
    "https://download.geofabrik.de/north-america/us/illinois.html";
pub const ILLINOIS_OSM_PBF: &str =
    "https://download.geofabrik.de/north-america/us/illinois-latest.osm.pbf";
pub const OSM_ATTRIBUTION: &str = "© OpenStreetMap contributors";
pub const OSM_LICENSE: &str = "https://www.openstreetmap.org/copyright";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    latitude: f64,
    longitude: f64,
}

impl Coordinates {
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        (latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude))
        .then_some(Self {
            latitude,
            longitude,
        })
    }

    pub fn latitude(self) -> f64 {
        self.latitude
    }
    pub fn longitude(self) -> f64 {
        self.longitude
    }
}

/// An unverified or incomplete download must never be reported as ready offline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageState {
    Missing,
    Downloading,
    Downloaded,
    Preparing,
    Ready,
    Failed,
}

impl PackageState {
    pub fn can_open_offline(self) -> bool {
        self == Self::Ready
    }
}

/// Copy a response body to caller-owned staging storage with bounded memory.
///
/// `limit` is a hard byte budget; `expected` is an optional exact response size.
/// Cancellation is checked between reads, not during a blocking read. The HTTP
/// adapter must also set read timeouts and support cancellation of requests.
/// On any error the caller must retain or remove staging data, never promote it
/// to a ready map. Success proves byte transfer only, not format or map validity.
pub fn stage_download(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected: Option<u64>,
    limit: u64,
    cancelled: &AtomicBool,
) -> io::Result<u64> {
    if limit == 0 || expected.is_some_and(|size| size == 0 || size > limit) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid download size or byte budget",
        ));
    }
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Download cancelled",
            ));
        }
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Download cancelled",
            ));
        }
        if count == 0 {
            break;
        }
        let next = total
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Download size overflow"))?;
        if next > limit || expected.is_some_and(|size| next > size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Download exceeds declared size or byte budget",
            ));
        }
        writer.write_all(&buffer[..count])?;
        total = next;
    }
    if total == 0 || expected.is_some_and(|size| total != size) {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Empty or incomplete download",
        ));
    }
    writer.flush()?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn transfer(bytes: &[u8], expected: Option<u64>, limit: u64) -> io::Result<u64> {
        stage_download(
            &mut Cursor::new(bytes),
            &mut Vec::new(),
            expected,
            limit,
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn rejects_invalid_coordinates() {
        for (lat, lon) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (90.1, 0.0),
            (0.0, -180.1),
        ] {
            assert!(Coordinates::new(lat, lon).is_none());
        }
        assert!(Coordinates::new(41.8781, -87.6298).is_some());
    }

    #[test]
    fn only_prepared_packages_open_offline() {
        for state in [
            PackageState::Missing,
            PackageState::Downloading,
            PackageState::Downloaded,
            PackageState::Preparing,
            PackageState::Failed,
        ] {
            assert!(!state.can_open_offline());
        }
        assert!(PackageState::Ready.can_open_offline());
    }

    #[test]
    fn copies_more_than_one_buffer() {
        let input = vec![42_u8; 150_000];
        let mut output = Vec::new();
        assert_eq!(
            stage_download(
                &mut Cursor::new(&input),
                &mut output,
                Some(150_000),
                150_000,
                &AtomicBool::new(false)
            )
            .unwrap(),
            150_000
        );
        assert_eq!(output, input);
    }

    #[test]
    fn accepts_unknown_length_within_budget() {
        assert_eq!(transfer(b"map", None, 3).unwrap(), 3);
    }

    #[test]
    fn rejects_truncation_and_empty_bodies() {
        assert_eq!(
            transfer(b"map", Some(4), 10).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(transfer(b"", None, 10).is_err());
    }

    #[test]
    fn rejects_overlong_bodies_and_invalid_budgets() {
        assert!(transfer(b"maps", Some(3), 10).is_err());
        assert!(transfer(b"maps", None, 3).is_err());
        assert!(transfer(b"map", Some(20), 10).is_err());
        assert!(transfer(b"map", None, 0).is_err());
    }

    #[test]
    fn cancellation_does_not_write() {
        let mut output = Vec::new();
        let error = stage_download(
            &mut Cursor::new(b"map"),
            &mut output,
            None,
            10,
            &AtomicBool::new(true),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(output.is_empty());
    }

    #[test]
    fn propagates_disk_errors() {
        struct FullDisk;
        impl Write for FullDisk {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("Disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(
            stage_download(
                &mut Cursor::new(b"map"),
                &mut FullDisk,
                None,
                10,
                &AtomicBool::new(false)
            )
            .is_err()
        );
    }
}
