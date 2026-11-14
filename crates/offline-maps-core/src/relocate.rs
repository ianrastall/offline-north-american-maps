//! Moving the package store to another folder.
//!
//! Only what the store owns is moved: the folders of catalog regions and the
//! saved places files. Anything else in either folder is left alone, so the map
//! folder can be one that holds other things too.
//!
//! On one volume each entry is renamed, which is instant. Across volumes the
//! entries are copied into `<destination>/.offline-maps-moving`, flushed to
//! disk and renamed into place only when everything has copied. The originals
//! are removed afterwards by [`remove_moved`], once the caller has recorded the
//! new location, so an interruption at any point leaves a complete store in at
//! least one of the two folders. A failed or cancelled move leaves the source
//! as it was and removes the partial copy.
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::places::PLACES_FILE;
use crate::regions;

/// Where a copy across volumes is assembled before it is renamed into place.
pub const STAGING_DIR: &str = ".offline-maps-moving";

/// Room left free on the destination beyond the maps themselves.
const SPACE_MARGIN: u64 = 64 * 1024 * 1024;

const COPY_BUFFER: usize = 4 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct MoveProgress {
    pub done: AtomicU64,
    pub total: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutcome {
    /// The source held no maps or saved places.
    Empty,
    /// Renamed on one volume; nothing is left in the source.
    Renamed { bytes: u64 },
    /// Copied to another volume; the originals are still in the source and
    /// are removed with [`remove_moved`] once the new location is recorded.
    Copied { bytes: u64 },
}

#[derive(Debug)]
pub enum MoveError {
    /// The destination is unusable (the same folder, inside the source, not absolute).
    Destination(String),
    /// The destination already holds maps or saved places with these names.
    Conflict(Vec<String>),
    NoSpace {
        needed: u64,
        available: u64,
    },
    Cancelled,
    Io(String),
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Destination(message) | Self::Io(message) => f.write_str(message),
            Self::Conflict(names) => write!(
                f,
                "The new folder already has maps or saved places ({}). Choose another folder, or use it without moving.",
                names.join(", ")
            ),
            Self::NoSpace { needed, available } => write!(
                f,
                "The new folder's drive has {} free, and the maps need {}.",
                megabytes(*available),
                megabytes(*needed)
            ),
            Self::Cancelled => f.write_str("Moving the maps was cancelled."),
        }
    }
}

impl std::error::Error for MoveError {}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.1} GB", mb / 1000.0)
    } else {
        format!("{mb:.0} MB")
    }
}

fn io_error(context: &str, path: &Path, error: &io::Error) -> MoveError {
    MoveError::Io(format!("{context} {}: {error}", path.display()))
}

/// Whether `name` is something the store owns at its root.
pub fn is_store_entry(name: &str) -> bool {
    regions::find(name).is_some()
        || name == PLACES_FILE
        || (name.starts_with("saved-places.unreadable-") && name.ends_with(".json"))
}

/// The store's own entries in `root`, sorted. A missing folder has none.
pub fn store_entries(root: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str()
            && is_store_entry(name)
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Lexical, case-insensitive path components (Windows paths compare that way).
/// `C:\` and its verbatim form `\\?\C:\` compare equal.
fn components(path: &Path) -> Vec<String> {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| match c {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => format!("{}:", d as char),
                _ => c.as_os_str().to_string_lossy().into_owned(),
            },
            _ => c.as_os_str().to_string_lossy().into_owned(),
        })
        .map(|c| c.to_lowercase())
        .collect()
}

/// Follows links and junctions in the part of `path` that exists.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(mut resolved) = fs::canonicalize(existing) {
            resolved.extend(rest.iter().rev());
            return resolved;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

fn starts_with(path: &[String], prefix: &[String]) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == *prefix
}

/// Rejects a destination that is not absolute, is the source, or contains or
/// is contained by it.
pub fn check_destination(source: &Path, destination: &Path) -> Result<(), MoveError> {
    if !destination.is_absolute() {
        return Err(MoveError::Destination(
            "The new map folder must be a full path.".into(),
        ));
    }
    let from = components(&resolve(source));
    let to = components(&resolve(destination));
    if from == to {
        return Err(MoveError::Destination(
            "That is already the map folder.".into(),
        ));
    }
    if starts_with(&to, &from) || starts_with(&from, &to) {
        return Err(MoveError::Destination(
            "Choose a folder that is not inside the current map folder or around it.".into(),
        ));
    }
    Ok(())
}

fn is_cross_device(error: &io::Error) -> bool {
    // ERROR_NOT_SAME_DEVICE on Windows, EXDEV elsewhere.
    error.kind() == io::ErrorKind::CrossesDevices
        || (cfg!(windows) && error.raw_os_error() == Some(17))
}

/// A tile read that started before the store was closed may hold a file for a
/// moment; retry briefly before giving up.
fn with_retries<T>(mut action: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match action() {
            Err(error)
                if attempt < 20
                    && matches!(error.kind(), io::ErrorKind::PermissionDenied)
                    && !is_cross_device(&error) =>
            {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25 * attempt));
            }
            result => return result,
        }
    }
}

fn tree_size(path: &Path) -> io::Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        let mut total = 0;
        for entry in fs::read_dir(path)? {
            total += tree_size(&entry?.path())?;
        }
        Ok(total)
    } else if meta.is_file() {
        Ok(meta.len())
    } else {
        Ok(0)
    }
}

fn copy_file(
    from: &Path,
    to: &Path,
    progress: &MoveProgress,
    cancel: &AtomicBool,
    buffer: &mut [u8],
) -> Result<(), MoveError> {
    let mut source = File::open(from).map_err(|e| io_error("Could not read", from, &e))?;
    let meta = source
        .metadata()
        .map_err(|e| io_error("Could not read", from, &e))?;
    let mut target = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .map_err(|e| io_error("Could not create", to, &e))?;
    let mut written = 0_u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(MoveError::Cancelled);
        }
        let count = match source.read(buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error("Could not read", from, &e)),
        };
        target
            .write_all(&buffer[..count])
            .map_err(|e| io_error("Could not write", to, &e))?;
        written += count as u64;
        progress.done.fetch_add(count as u64, Ordering::Relaxed);
    }
    if written != meta.len() {
        return Err(MoveError::Io(format!(
            "{} changed while it was being copied.",
            from.display()
        )));
    }
    if let Ok(modified) = meta.modified() {
        let _ = target.set_modified(modified);
    }
    // On disk before the originals can be removed.
    target
        .sync_all()
        .map_err(|e| io_error("Could not write", to, &e))
}

fn copy_tree(
    from: &Path,
    to: &Path,
    progress: &MoveProgress,
    cancel: &AtomicBool,
    buffer: &mut [u8],
) -> Result<(), MoveError> {
    let meta = fs::symlink_metadata(from).map_err(|e| io_error("Could not read", from, &e))?;
    if meta.is_dir() {
        fs::create_dir(to).map_err(|e| io_error("Could not create", to, &e))?;
        let entries = fs::read_dir(from).map_err(|e| io_error("Could not read", from, &e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_error("Could not read", from, &e))?;
            copy_tree(
                &entry.path(),
                &to.join(entry.file_name()),
                progress,
                cancel,
                buffer,
            )?;
        }
        Ok(())
    } else if meta.is_file() {
        copy_file(from, to, progress, cancel, buffer)
    } else {
        Ok(())
    }
}

fn remove_path(path: &Path) -> io::Result<()> {
    let result = with_retries(|| match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    });
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Renames every entry; on a failure part-way the renamed ones are put back.
fn rename_all(source: &Path, destination: &Path, names: &[String]) -> io::Result<()> {
    for (index, name) in names.iter().enumerate() {
        if let Err(error) = with_retries(|| fs::rename(source.join(name), destination.join(name))) {
            for done in &names[..index] {
                let _ = fs::rename(destination.join(done), source.join(done));
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Moves the store's entries from `source` to `destination` (created if
/// missing). The store must be closed: nothing may write to it meanwhile.
pub fn move_store(
    source: &Path,
    destination: &Path,
    progress: &MoveProgress,
    cancel: &AtomicBool,
) -> Result<MoveOutcome, MoveError> {
    check_destination(source, destination)?;
    let names = store_entries(source).map_err(|e| io_error("Could not read", source, &e))?;
    fs::create_dir_all(destination).map_err(|e| io_error("Could not create", destination, &e))?;
    let staging = destination.join(STAGING_DIR);
    // Left by a move that was interrupted; never part of a store.
    remove_path(&staging).map_err(|e| io_error("Could not remove", &staging, &e))?;
    if names.is_empty() {
        return Ok(MoveOutcome::Empty);
    }
    let conflicts: Vec<String> = names
        .iter()
        .filter(|name| fs::symlink_metadata(destination.join(name)).is_ok())
        .cloned()
        .collect();
    if !conflicts.is_empty() {
        return Err(MoveError::Conflict(conflicts));
    }

    let mut total = 0;
    for name in &names {
        let path = source.join(name);
        total += tree_size(&path).map_err(|e| io_error("Could not read", &path, &e))?;
    }
    progress.total.store(total, Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    if cancel.load(Ordering::Relaxed) {
        return Err(MoveError::Cancelled);
    }

    match rename_all(source, destination, &names) {
        Ok(()) => {
            progress.done.store(total, Ordering::Relaxed);
            return Ok(MoveOutcome::Renamed { bytes: total });
        }
        Err(error) if is_cross_device(&error) => {}
        Err(error) => return Err(io_error("Could not move the maps to", destination, &error)),
    }

    if let Ok(available) = fs4::available_space(destination)
        && available < total.saturating_add(SPACE_MARGIN)
    {
        return Err(MoveError::NoSpace {
            needed: total,
            available,
        });
    }
    fs::create_dir(&staging).map_err(|e| io_error("Could not create", &staging, &e))?;
    let mut buffer = vec![0_u8; COPY_BUFFER];
    let mut promoted = 0;
    let result = (|| {
        for name in &names {
            copy_tree(
                &source.join(name),
                &staging.join(name),
                progress,
                cancel,
                &mut buffer,
            )?;
        }
        for name in &names {
            let target = destination.join(name);
            fs::rename(staging.join(name), &target)
                .map_err(|e| io_error("Could not create", &target, &e))?;
            promoted += 1;
        }
        Ok(())
    })();
    if let Err(error) = result {
        for name in &names[..promoted] {
            let _ = remove_path(&destination.join(name));
        }
        let _ = remove_path(&staging);
        return Err(error);
    }
    let _ = fs::remove_dir(&staging);
    Ok(MoveOutcome::Copied { bytes: total })
}

/// After a [`MoveOutcome::Copied`] move: removes the originals that now exist
/// in `destination`, then the source folder if nothing else is left in it.
/// Returns the entries that could not be removed.
pub fn remove_moved(source: &Path, destination: &Path) -> Vec<(String, io::Error)> {
    let names = match store_entries(source) {
        Ok(names) => names,
        Err(error) => return vec![(source.display().to_string(), error)],
    };
    let mut failed = Vec::new();
    for name in names {
        // Never remove the only copy.
        if fs::symlink_metadata(destination.join(&name)).is_err() {
            continue;
        }
        if let Err(error) = remove_path(&source.join(&name)) {
            failed.push((name, error));
        }
    }
    let _ = fs::remove_dir(source);
    failed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "offline-maps-relocate-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn populate(root: &Path) {
        fs::create_dir_all(root.join("us-il/versions/v1")).unwrap();
        fs::write(root.join("us-il/active.json"), b"{\"version\":\"v1\"}").unwrap();
        fs::write(
            root.join("us-il/versions/v1/package.mbtiles"),
            vec![7_u8; 10_000],
        )
        .unwrap();
        fs::write(root.join(PLACES_FILE), b"{}").unwrap();
        fs::write(root.join("notes.txt"), b"not the store's").unwrap();
    }

    #[test]
    fn copies_only_store_entries_and_removes_originals_after() {
        let temp = TempDir::new("copy");
        let source = temp.0.join("from");
        let destination = temp.0.join("to");
        populate(&source);
        let progress = MoveProgress::default();
        let mut buffer = vec![0_u8; 1024];
        // The copy path, exercised directly (one volume would rename).
        fs::create_dir_all(&destination).unwrap();
        for name in store_entries(&source).unwrap() {
            copy_tree(
                &source.join(&name),
                &destination.join(&name),
                &progress,
                &AtomicBool::new(false),
                &mut buffer,
            )
            .unwrap();
        }
        assert_eq!(progress.done.load(Ordering::Relaxed), 10_000 + 16 + 2);
        assert_eq!(
            fs::read(destination.join("us-il/versions/v1/package.mbtiles")).unwrap(),
            vec![7_u8; 10_000]
        );
        assert!(!destination.join("notes.txt").exists());
        assert!(remove_moved(&source, &destination).is_empty());
        assert!(!source.join("us-il").exists());
        assert!(!source.join(PLACES_FILE).exists());
        // The folder stays because it has something that is not the store's.
        assert!(source.join("notes.txt").exists());
    }

    #[test]
    fn renames_on_one_volume() {
        let temp = TempDir::new("rename");
        let source = temp.0.join("from");
        let destination = temp.0.join("to");
        populate(&source);
        let outcome = move_store(
            &source,
            &destination,
            &MoveProgress::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Renamed {
                bytes: 10_000 + 16 + 2
            }
        );
        assert!(destination.join("us-il/active.json").exists());
        assert!(destination.join(PLACES_FILE).exists());
        assert!(!source.join("us-il").exists());
        assert!(source.join("notes.txt").exists());
        assert!(!destination.join(STAGING_DIR).exists());
    }

    #[test]
    fn refuses_conflicts_and_nested_folders() {
        let temp = TempDir::new("conflict");
        let source = temp.0.join("from");
        let destination = temp.0.join("to");
        populate(&source);
        fs::create_dir_all(destination.join("us-il")).unwrap();
        let none = AtomicBool::new(false);
        match move_store(&source, &destination, &MoveProgress::default(), &none) {
            Err(MoveError::Conflict(names)) => assert_eq!(names, ["us-il"]),
            other => panic!("{other:?}"),
        }
        assert!(source.join("us-il/active.json").exists());
        for bad in [source.clone(), source.join("inner"), temp.0.clone()] {
            assert!(matches!(
                move_store(&source, &bad, &MoveProgress::default(), &none),
                Err(MoveError::Destination(_))
            ));
        }
    }

    #[test]
    fn empty_source_moves_nothing() {
        let temp = TempDir::new("empty");
        let destination = temp.0.join("to");
        let outcome = move_store(
            &temp.0.join("missing"),
            &destination,
            &MoveProgress::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome, MoveOutcome::Empty);
        assert!(destination.is_dir());
    }

    #[test]
    fn cancelled_copy_stops() {
        let temp = TempDir::new("cancel");
        let source = temp.0.join("from");
        populate(&source);
        let to = temp.0.join("copy.mbtiles");
        let result = copy_file(
            &source.join("us-il/versions/v1/package.mbtiles"),
            &to,
            &MoveProgress::default(),
            &AtomicBool::new(true),
            &mut [0_u8; 1024],
        );
        assert!(matches!(result, Err(MoveError::Cancelled)));
    }

    #[test]
    fn remove_moved_keeps_entries_missing_from_destination() {
        let temp = TempDir::new("keep");
        let source = temp.0.join("from");
        let destination = temp.0.join("to");
        populate(&source);
        fs::create_dir_all(&destination).unwrap();
        assert!(remove_moved(&source, &destination).is_empty());
        assert!(source.join("us-il/active.json").exists());
        assert!(source.join(PLACES_FILE).exists());
    }
}
