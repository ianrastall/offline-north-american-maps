//! On-disk package store. Lives outside the installation directory.
//!
//! ```text
//! <root>/<region>/active.json             pointer to the active version (atomic replace)
//! <root>/<region>/versions/<v>/package.mbtiles
//! <root>/<region>/versions/<v>/record.json
//! <root>/<region>/staging/package.part    download in progress
//! <root>/<region>/staging/download.json   what the .part file is a copy of
//! <root>/<region>/staging/lock            held while an update runs
//! ```
//!
//! A version directory is complete before `active.json` names it, and
//! `active.json` is replaced in one rename, so a crash at any point leaves
//! either the previous package or the new one active, never a partial one.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mbtiles::PackageError;
use crate::package::PackageRecord;

pub const PACKAGE_FILE: &str = "package.mbtiles";
pub const RECORD_FILE: &str = "record.json";

#[derive(Debug, Clone, PartialEq)]
pub struct ActivePackage {
    pub version: String,
    pub path: PathBuf,
    pub record: PackageRecord,
}

impl ActivePackage {
    /// Where this version's search index lives (it may not exist yet for
    /// packages installed before search was added).
    pub fn search_index_path(&self) -> PathBuf {
        self.path.with_file_name(crate::search::SEARCH_FILE)
    }
}

/// Describes the partial download in `staging/package.part`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagingState {
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub total: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct Pointer {
    schema: u32,
    version: String,
}

/// Holds the region's update lock until dropped.
#[derive(Debug)]
pub struct UpdateLock {
    _file: File,
}

#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn region_dir(&self, region_id: &str) -> PathBuf {
        self.root.join(region_id)
    }

    pub fn staging_dir(&self, region_id: &str) -> PathBuf {
        self.region_dir(region_id).join("staging")
    }

    pub fn staging_file(&self, region_id: &str) -> PathBuf {
        self.staging_dir(region_id).join("package.part")
    }

    pub fn version_dir(&self, region_id: &str, version: &str) -> PathBuf {
        self.region_dir(region_id).join("versions").join(version)
    }

    /// Takes the region's update lock. Returns `WouldBlock` if another update
    /// holds it. The OS releases the lock if the process exits.
    pub fn lock(&self, region_id: &str) -> io::Result<UpdateLock> {
        let dir = self.staging_dir(region_id);
        fs::create_dir_all(&dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(UpdateLock { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another map update is running",
            )),
            Err(fs::TryLockError::Error(error)) => Err(error),
        }
    }

    /// The active package, if any. A pointer to a missing or inconsistent
    /// version is an error so the caller can offer a fresh download.
    pub fn active(&self, region_id: &str) -> Result<Option<ActivePackage>, PackageError> {
        let pointer_path = self.region_dir(region_id).join("active.json");
        let text = match fs::read_to_string(&pointer_path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let pointer: Pointer = serde_json::from_str(&text).map_err(|e| {
            PackageError::Invalid(format!("active package pointer is unreadable: {e}"))
        })?;
        if pointer.schema != 1 || !is_version_name(&pointer.version) {
            return Err(PackageError::Invalid(
                "active package pointer is invalid".into(),
            ));
        }
        let dir = self.version_dir(region_id, &pointer.version);
        let record = PackageRecord::from_json(&fs::read_to_string(dir.join(RECORD_FILE))?)?;
        let path = dir.join(PACKAGE_FILE);
        let length = fs::metadata(&path)?.len();
        if length != record.byte_length || record.region_id != region_id {
            return Err(PackageError::Invalid(
                "active package does not match its record".into(),
            ));
        }
        Ok(Some(ActivePackage {
            version: pointer.version,
            path,
            record,
        }))
    }

    /// Moves a verified package into a new version directory with its record.
    /// Does not activate it.
    pub fn install_version(
        &self,
        region_id: &str,
        version: &str,
        package: &Path,
        search_index: Option<&Path>,
        record: &PackageRecord,
    ) -> io::Result<PathBuf> {
        if !is_version_name(version) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid version name",
            ));
        }
        let dir = self.version_dir(region_id, version);
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fs::rename(package, dir.join(PACKAGE_FILE))?;
        if let Some(index) = search_index {
            fs::rename(index, dir.join(crate::search::SEARCH_FILE))?;
        }
        write_atomic(&dir.join(RECORD_FILE), record.to_json().as_bytes())?;
        Ok(dir)
    }

    /// Makes `version` the active package in a single rename.
    pub fn activate(&self, region_id: &str, version: &str) -> io::Result<()> {
        let dir = self.version_dir(region_id, version);
        if !dir.join(PACKAGE_FILE).is_file() || !dir.join(RECORD_FILE).is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Version to activate is incomplete",
            ));
        }
        let pointer = serde_json::to_vec(&Pointer {
            schema: 1,
            version: version.to_owned(),
        })
        .map_err(io::Error::other)?;
        write_atomic(&self.region_dir(region_id).join("active.json"), &pointer)
    }

    /// Removes every version except the active one. Best effort: a version
    /// still open elsewhere (for example by the renderer) is left for a later
    /// prune. Returns the versions that could not be removed.
    pub fn prune(&self, region_id: &str) -> Vec<(String, io::Error)> {
        let active = self.active(region_id).ok().flatten().map(|a| a.version);
        let Ok(entries) = fs::read_dir(self.region_dir(region_id).join("versions")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (Some(&name) != active.as_ref())
                    .then(|| fs::remove_dir_all(entry.path()).err().map(|e| (name, e)))
                    .flatten()
            })
            .collect()
    }

    pub fn read_staging(&self, region_id: &str) -> Option<(StagingState, u64)> {
        let dir = self.staging_dir(region_id);
        let state: StagingState =
            serde_json::from_str(&fs::read_to_string(dir.join("download.json")).ok()?).ok()?;
        let length = fs::metadata(dir.join("package.part")).ok()?.len();
        Some((state, length))
    }

    pub fn write_staging(&self, region_id: &str, state: &StagingState) -> io::Result<()> {
        let dir = self.staging_dir(region_id);
        fs::create_dir_all(&dir)?;
        let json = serde_json::to_vec_pretty(state).map_err(io::Error::other)?;
        write_atomic(&dir.join("download.json"), &json)
    }

    pub fn clear_staging(&self, region_id: &str) -> io::Result<()> {
        let dir = self.staging_dir(region_id);
        for name in [
            "package.part",
            "download.json",
            "search.sqlite",
            "search.sqlite.tmp",
        ] {
            match fs::remove_file(dir.join(name)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }
}

/// Version names are generated (`YYYYMMDDTHHMMSSZ-<hash prefix>`); anything
/// else in a pointer is rejected so it cannot address outside the store.
fn is_version_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Writes a sibling temporary file, flushes it to disk and renames it over
/// `path`, so readers see the old or the new content, never a mix.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mbtiles::tests::TempFile;

    pub(crate) struct TempDir(pub PathBuf);
    impl TempDir {
        pub(crate) fn new() -> Self {
            // Reuse the unique temp-path scheme; the file itself never exists.
            let path = TempFile::new("dir").0.clone();
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn version_names_cannot_escape_the_store() {
        for bad in ["", "..", "../x", "a/b", "a\\b", "C:", &"x".repeat(65)] {
            assert!(!is_version_name(bad), "{bad}");
        }
        assert!(is_version_name("20261108T065633Z-17c12c9500b5"));
    }

    #[test]
    fn lock_is_exclusive_until_dropped() {
        let dir = TempDir::new();
        let store = PackageStore::new(&dir.0);
        let lock = store.lock("us-il").unwrap();
        assert_eq!(
            store.lock("us-il").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(lock);
        assert!(store.lock("us-il").is_ok());
    }

    #[test]
    fn staging_round_trip_and_clear() {
        let dir = TempDir::new();
        let store = PackageStore::new(&dir.0);
        assert!(store.read_staging("us-il").is_none());
        let state = StagingState {
            url: "http://example.test/a".into(),
            etag: Some("\"e\"".into()),
            last_modified: None,
            total: Some(10),
        };
        store.write_staging("us-il", &state).unwrap();
        fs::write(store.staging_file("us-il"), b"abc").unwrap();
        assert_eq!(store.read_staging("us-il"), Some((state, 3)));
        store.clear_staging("us-il").unwrap();
        store.clear_staging("us-il").unwrap();
        assert!(store.read_staging("us-il").is_none());
    }

    #[test]
    fn missing_or_corrupt_pointer() {
        let dir = TempDir::new();
        let store = PackageStore::new(&dir.0);
        assert!(store.active("us-il").unwrap().is_none());
        fs::create_dir_all(store.region_dir("us-il")).unwrap();
        fs::write(store.region_dir("us-il").join("active.json"), b"{").unwrap();
        assert!(store.active("us-il").is_err());
        fs::write(
            store.region_dir("us-il").join("active.json"),
            br#"{"schema":1,"version":"../../elsewhere"}"#,
        )
        .unwrap();
        assert!(store.active("us-il").is_err());
        assert!(store.activate("us-il", "20261108T000000Z-missing").is_err());
    }
}
