use super::*;
use crate::package::ILLINOIS;
use crate::package::tests::shortbread_package_bytes;
use crate::store::tests::TempDir;
use std::cell::RefCell;
use std::io::Cursor;

#[derive(Default)]
struct Fake {
    file: Vec<u8>,
    etag: Option<String>,
    cut_after: Option<usize>,
    ignore_range: bool,
    cancel_at: Option<usize>,
    last_modified: Option<String>,
    /// Last-Modified reported by GET only (a staler cache than the probe saw).
    fetch_last_modified: Option<String>,
    fetches: Vec<Option<(u64, String)>>,
}

/// In-memory server. The flag is leaked so a response body can raise it.
struct FakeTransport(RefCell<Fake>, &'static AtomicBool);

/// Body that can end early (a dropped connection) or raise the cancel flag
/// once a number of bytes have been read.
struct Body {
    data: Cursor<Vec<u8>>,
    cancel_at: Option<usize>,
    flag: &'static AtomicBool,
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limit = buf.len().min(4096);
        let n = self.data.read(&mut buf[..limit])?;
        if self
            .cancel_at
            .is_some_and(|at| self.data.position() as usize >= at)
        {
            self.flag.store(true, Ordering::Relaxed);
        }
        Ok(n)
    }
}

impl FakeTransport {
    fn new(file: Vec<u8>, etag: &str) -> Self {
        Self(
            RefCell::new(Fake {
                file,
                etag: Some(etag.into()),
                ..Fake::default()
            }),
            Box::leak(Box::new(AtomicBool::new(false))),
        )
    }

    fn info(&self) -> RemoteInfo {
        let f = self.0.borrow();
        RemoteInfo {
            length: Some(f.file.len() as u64),
            etag: f.etag.clone(),
            last_modified: Some(
                f.last_modified
                    .clone()
                    .unwrap_or_else(|| "Sun, 08 Nov 2026 04:12:34 GMT".into()),
            ),
            accepts_ranges: true,
        }
    }

    fn serve(&self, file: Vec<u8>, etag: &str) {
        let mut f = self.0.borrow_mut();
        f.file = file;
        f.etag = Some(etag.into());
    }
}

impl Transport for FakeTransport {
    fn probe(&self, _: &str) -> io::Result<RemoteInfo> {
        Ok(self.info())
    }

    fn fetch(&self, _: &str, resume: Option<(u64, &str)>) -> io::Result<Fetched> {
        let mut info = self.info();
        let mut f = self.0.borrow_mut();
        if let Some(lm) = f.fetch_last_modified.clone() {
            info.last_modified = Some(lm);
        }
        f.fetches.push(resume.map(|(o, e)| (o, e.to_owned())));
        let honoured = resume.filter(|(_, e)| !f.ignore_range && Some(*e) == f.etag.as_deref());
        let start = honoured.map_or(0, |(o, _)| o as usize);
        let mut bytes = f.file[start..].to_vec();
        if let Some(cut) = f.cut_after.take() {
            bytes.truncate(cut);
        }
        let body = Box::new(Body {
            data: Cursor::new(bytes),
            cancel_at: f.cancel_at.take(),
            flag: self.1,
        });
        Ok(match honoured {
            Some(_) => Fetched::Partial {
                start: start as u64,
                info,
                body,
            },
            None => Fetched::Full { info, body },
        })
    }
}

fn options() -> UpdateOptions {
    UpdateOptions {
        available_space: |_| Ok(u64::MAX),
        ..UpdateOptions::default()
    }
}

fn run(
    store: &PackageStore,
    t: &FakeTransport,
    o: &UpdateOptions,
) -> Result<UpdateOutcome, UpdateError> {
    update_region(store, &ILLINOIS, t, o, &mut |_| {}, t.1)
}

fn activated(result: Result<UpdateOutcome, UpdateError>) -> (ActivePackage, Option<String>, usize) {
    match result {
        Ok(UpdateOutcome::Activated {
            package,
            replaced,
            prune_pending,
        }) => (package, replaced, prune_pending),
        other => panic!("expected activation, got {other:?}"),
    }
}

fn sha(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn downloads_verifies_and_activates() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let file = shortbread_package_bytes(b"one");
    let t = FakeTransport::new(file.clone(), "\"v1\"");
    let mut events = Vec::new();
    let result = update_region(
        &store,
        &ILLINOIS,
        &t,
        &options(),
        &mut |p| events.push(p),
        t.1,
    );
    let (package, replaced, prune_pending) = activated(result);
    assert_eq!((replaced, prune_pending), (None, 0));
    assert_eq!(package.record.sha256, sha(&file));
    assert_eq!(package.record.source.etag.as_deref(), Some("\"v1\""));
    assert_eq!(std::fs::read(&package.path).unwrap(), file);
    assert_eq!(store.active("us-il").unwrap().unwrap(), package);
    assert!(store.read_staging("us-il").is_none());
    let size = file.len() as u64;
    assert_eq!(events.first(), Some(&Progress::Checking));
    assert!(events.contains(&Progress::Downloading {
        received: size,
        total: Some(size)
    }));
    assert_eq!(
        events[events.len() - 2..],
        [Progress::Verifying, Progress::Activating]
    );

    // Same ETag and size: nothing is fetched again.
    assert!(matches!(
        run(&store, &t, &options()),
        Ok(UpdateOutcome::UpToDate(_))
    ));
    assert_eq!(t.0.borrow().fetches.len(), 1);
}

#[test]
fn interrupted_download_resumes_from_staging() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let file = shortbread_package_bytes(b"one");
    let t = FakeTransport::new(file.clone(), "\"v1\"");
    t.0.borrow_mut().cut_after = Some(5000);
    assert!(matches!(
        run(&store, &t, &options()),
        Err(UpdateError::Network(_))
    ));
    assert_eq!(store.read_staging("us-il").unwrap().1, 5000);
    assert!(store.active("us-il").unwrap().is_none());

    let (package, _, _) = activated(run(&store, &t, &options()));
    assert_eq!(t.0.borrow().fetches, [None, Some((5000, "\"v1\"".into()))]);
    assert_eq!(package.record.sha256, sha(&file));
}

#[test]
fn changed_file_restarts_instead_of_joining_bytes() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let t = FakeTransport::new(shortbread_package_bytes(b"one"), "\"v1\"");
    t.0.borrow_mut().cut_after = Some(5000);
    assert!(run(&store, &t, &options()).is_err());
    let newer = shortbread_package_bytes(b"two-longer");
    t.serve(newer.clone(), "\"v2\"");
    let (package, _, _) = activated(run(&store, &t, &options()));
    assert_eq!(t.0.borrow().fetches[1], None);
    assert_eq!(package.record.sha256, sha(&newer));
}

#[test]
fn server_ignoring_range_restarts_from_zero() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let file = shortbread_package_bytes(b"three");
    let t = FakeTransport::new(file.clone(), "\"v3\"");
    t.0.borrow_mut().cut_after = Some(3000);
    assert!(run(&store, &t, &options()).is_err());
    t.0.borrow_mut().ignore_range = true;
    let (package, _, _) = activated(run(&store, &t, &options()));
    assert_eq!(t.0.borrow().fetches[1], Some((3000, "\"v3\"".into())));
    assert_eq!(package.record.sha256, sha(&file));
}

#[test]
fn cancellation_keeps_staging_and_previous_package() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let first = shortbread_package_bytes(b"one");
    let t = FakeTransport::new(first.clone(), "\"v1\"");
    activated(run(&store, &t, &options()));
    t.serve(shortbread_package_bytes(b"two"), "\"v2\"");
    t.0.borrow_mut().cancel_at = Some(8192);
    assert!(matches!(
        run(&store, &t, &options()),
        Err(UpdateError::Cancelled)
    ));
    // The first 4 KiB chunk was written before cancellation was seen.
    assert_eq!(store.read_staging("us-il").unwrap().1, 4096);
    assert_eq!(
        store.active("us-il").unwrap().unwrap().record.sha256,
        sha(&first)
    );
}

#[test]
fn invalid_download_is_discarded_and_previous_kept() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let first = shortbread_package_bytes(b"one");
    let t = FakeTransport::new(first.clone(), "\"v1\"");
    activated(run(&store, &t, &options()));
    t.serve(vec![7_u8; 50_000], "\"broken\"");
    assert!(matches!(
        run(&store, &t, &options()),
        Err(UpdateError::Invalid(_))
    ));
    assert!(store.read_staging("us-il").is_none());
    let active = store.active("us-il").unwrap().unwrap();
    assert_eq!(active.record.sha256, sha(&first));
    assert_eq!(std::fs::read(&active.path).unwrap(), first);
}

#[test]
fn update_replaces_and_prunes_previous_version() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let t = FakeTransport::new(shortbread_package_bytes(b"one"), "\"v1\"");
    let (first, _, _) = activated(run(&store, &t, &options()));
    t.serve(shortbread_package_bytes(b"two"), "\"v2\"");
    let (package, replaced, prune_pending) = activated(run(&store, &t, &options()));
    assert_eq!(replaced.as_deref(), Some(first.version.as_str()));
    assert_eq!(prune_pending, 0);
    assert!(!first.path.exists());
    assert!(package.path.exists());
}

#[test]
fn refuses_oversized_packages_full_disks_and_concurrent_updates() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let t = FakeTransport::new(shortbread_package_bytes(b"one"), "\"v1\"");
    let small = UpdateOptions {
        max_bytes: 1000,
        ..options()
    };
    assert!(matches!(
        run(&store, &t, &small),
        Err(UpdateError::TooLarge { limit: 1000, .. })
    ));
    let full = UpdateOptions {
        available_space: |_| Ok(1 << 20),
        ..options()
    };
    assert!(matches!(
        run(&store, &t, &full),
        Err(UpdateError::InsufficientSpace { .. })
    ));
    assert!(t.0.borrow().fetches.is_empty());

    let lock = store.lock("us-il").unwrap();
    assert!(matches!(
        run(&store, &t, &options()),
        Err(UpdateError::Busy)
    ));
    drop(lock);
    activated(run(&store, &t, &options()));
}

#[test]
fn content_range_parsing() {
    assert_eq!(
        parse_content_range("bytes 100-999/1000"),
        Some((100, 999, Some(1000)))
    );
    assert_eq!(parse_content_range("bytes 0-0/*"), Some((0, 0, None)));
    for bad in [
        "bytes 5-4/10",
        "bytes 0-10/10",
        "items 0-1/2",
        "bytes 0-1",
        "bytes a-b/c",
    ] {
        assert!(parse_content_range(bad).is_none(), "{bad}");
    }
}

#[test]
fn never_replaces_a_package_with_older_data() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let first = shortbread_package_bytes(b"one");
    let t = FakeTransport::new(first.clone(), "\"v8\"");
    activated(run(&store, &t, &options()));

    // A stale cache reports an older build: nothing is downloaded.
    t.serve(shortbread_package_bytes(b"older"), "\"v1\"");
    t.0.borrow_mut().last_modified = Some("Sun, 01 Nov 2026 04:09:47 GMT".into());
    assert!(matches!(
        run(&store, &t, &options()),
        Ok(UpdateOutcome::UpToDate(_))
    ));
    assert_eq!(t.0.borrow().fetches.len(), 1);

    // The probe sees a newer build but the transfer gets an older one: it is
    // downloaded and verified, then discarded instead of activated.
    {
        let mut f = t.0.borrow_mut();
        f.last_modified = Some("Sun, 15 Nov 2026 04:00:00 GMT".into());
        f.fetch_last_modified = Some("Sun, 01 Nov 2026 04:09:47 GMT".into());
    }
    assert!(matches!(
        run(&store, &t, &options()),
        Ok(UpdateOutcome::UpToDate(_))
    ));
    assert!(store.read_staging("us-il").is_none());
    assert_eq!(
        store.active("us-il").unwrap().unwrap().record.sha256,
        sha(&first)
    );
}
