//! Downloading, verifying and activating a region's map package.
//!
//! The update never touches the active package until a new one has been fully
//! downloaded, hashed, structurally checked and installed in its own version
//! directory. Interrupted transfers keep their staging file and resume with an
//! HTTP range request only when the server still reports the same strong ETag;
//! otherwise the transfer restarts, so bytes from two different files are never
//! joined.
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::mbtiles::{PackageError, TilePackage};
use crate::package::{PackageRecord, Region, SourceInfo, verify_package};
use crate::search::{SEARCH_FILE, build_index};
use crate::stage_download;
use crate::store::{ActivePackage, PackageStore, StagingState};

pub const USER_AGENT: &str = concat!("OfflineMaps/", env!("CARGO_PKG_VERSION"), " (Jupiter)");

/// What a server reports about a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteInfo {
    pub length: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub accepts_ranges: bool,
}

impl RemoteInfo {
    /// Only strong validators may be used with `If-Range`.
    pub fn strong_etag(&self) -> Option<&str> {
        self.etag.as_deref().filter(|e| !e.starts_with("W/"))
    }
}

pub enum Fetched {
    /// The whole file (200).
    Full {
        info: RemoteInfo,
        body: Box<dyn Read + Send>,
    },
    /// The requested tail (206); `info.length` is the complete file length.
    Partial {
        start: u64,
        info: RemoteInfo,
        body: Box<dyn Read + Send>,
    },
}

pub trait Transport {
    fn probe(&self, url: &str) -> io::Result<RemoteInfo>;
    /// GET `url`. With `resume = Some((offset, etag))` asks for the bytes from
    /// `offset` on, valid only if the file still has `etag`.
    fn fetch(&self, url: &str, resume: Option<(u64, &str)>) -> io::Result<Fetched>;

    /// GET bytes `start..start + length` of `url`. With `etag`, fails with
    /// `InvalidData` if the file no longer has it. Returns the server's view of
    /// the whole file (its length, validators) and the bytes.
    fn fetch_range(
        &self,
        url: &str,
        start: u64,
        length: u64,
        etag: Option<&str>,
    ) -> io::Result<(RemoteInfo, Vec<u8>)> {
        let fetched = match etag {
            Some(etag) => self.fetch(url, Some((start, etag)))?,
            None => self.fetch(url, None)?,
        };
        let (info, mut body, at) = match fetched {
            Fetched::Partial {
                start: at,
                info,
                body,
            } => (info, body, at),
            Fetched::Full { info, body } => (info, body, 0),
        };
        if etag.is_some() && at != start {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The file changed on the server",
            ));
        }
        io::copy(&mut (&mut body).take(start - at), &mut io::sink())?;
        read_exact_range(body, length).map(|bytes| (info, bytes))
    }
}

fn read_exact_range(body: impl Read, length: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(length.min(64 << 20)).unwrap_or(0));
    body.take(length).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "The server sent fewer bytes than asked for",
        ));
    }
    Ok(bytes)
}

/// HTTPS transport using the Windows certificate store (SChannel).
pub struct HttpTransport {
    agent: ureq::Agent,
}

impl Default for HttpTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpTransport {
    pub fn new() -> Self {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .user_agent(USER_AGENT)
            .http_status_as_error(false)
            .max_redirects(5)
            .timeout_resolve(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // A stalled connection fails after a minute without data rather than hanging.
            .timeout_per_read(Some(Duration::from_secs(60)))
            .build()
            .into();
        Self { agent }
    }
}

fn header(response: &ureq::http::Response<ureq::Body>, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn remote_info(response: &ureq::http::Response<ureq::Body>) -> RemoteInfo {
    RemoteInfo {
        length: header(response, "content-length").and_then(|v| v.parse().ok()),
        etag: header(response, "etag"),
        last_modified: header(response, "last-modified"),
        accepts_ranges: header(response, "accept-ranges")
            .is_some_and(|v| v.eq_ignore_ascii_case("bytes")),
    }
}

fn http_error(status: u16) -> io::Error {
    let kind = match status {
        404 | 410 => io::ErrorKind::NotFound,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("Server returned HTTP {status}"))
}

/// Parses `bytes <start>-<end>/<total>`.
pub fn parse_content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end): (u64, u64) = (start.trim().parse().ok()?, end.trim().parse().ok()?);
    let total = match total.trim() {
        "*" => None,
        t => Some(t.parse().ok()?),
    };
    (start <= end && total.is_none_or(|t| end < t)).then_some((start, end, total))
}

impl Transport for HttpTransport {
    fn probe(&self, url: &str) -> io::Result<RemoteInfo> {
        let response = self
            .agent
            .head(url)
            .header("Cache-Control", "no-cache")
            .call()
            .map_err(ureq::Error::into_io)?;
        match response.status().as_u16() {
            200 => Ok(remote_info(&response)),
            status => Err(http_error(status)),
        }
    }

    fn fetch(&self, url: &str, resume: Option<(u64, &str)>) -> io::Result<Fetched> {
        // Geofabrik's caching proxies otherwise answer from entries up to a
        // week old (observed 8 Nov 2026: HEAD reported the 1 Nov build).
        let mut request = self.agent.get(url).header("Cache-Control", "no-cache");
        if let Some((offset, etag)) = resume {
            request = request
                .header("Range", format!("bytes={offset}-"))
                .header("If-Range", etag);
        }
        let response = request.call().map_err(ureq::Error::into_io)?;
        let mut info = remote_info(&response);
        match response.status().as_u16() {
            200 => Ok(Fetched::Full {
                info,
                body: Box::new(response.into_body().into_reader()),
            }),
            206 => {
                let (start, end, total) = header(&response, "content-range")
                    .as_deref()
                    .and_then(parse_content_range)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "Invalid Content-Range")
                    })?;
                if info.length.is_some_and(|l| l != end - start + 1) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Content-Range does not match Content-Length",
                    ));
                }
                info.length = total;
                Ok(Fetched::Partial {
                    start,
                    info,
                    body: Box::new(response.into_body().into_reader()),
                })
            }
            status => Err(http_error(status)),
        }
    }

    fn fetch_range(
        &self,
        url: &str,
        start: u64,
        length: u64,
        etag: Option<&str>,
    ) -> io::Result<(RemoteInfo, Vec<u8>)> {
        if length == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Empty range"));
        }
        let mut request = self
            .agent
            .get(url)
            .header("Range", format!("bytes={start}-{}", start + length - 1));
        if let Some(etag) = etag {
            request = request.header("If-Range", etag);
        }
        let response = request.call().map_err(ureq::Error::into_io)?;
        let mut info = remote_info(&response);
        match response.status().as_u16() {
            206 => {
                let (from, to, total) = header(&response, "content-range")
                    .as_deref()
                    .and_then(parse_content_range)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidData, "Invalid Content-Range")
                    })?;
                if from != start || to != start + length - 1 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "The server sent a different range than asked for",
                    ));
                }
                info.length = total;
                let bytes = read_exact_range(response.into_body().into_reader(), length)?;
                Ok((info, bytes))
            }
            // If-Range did not match: the whole (changed) file would follow.
            200 if etag.is_some() => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The file changed on the server",
            )),
            200 => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The server does not support partial downloads",
            )),
            status => Err(http_error(status)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Checking,
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    Verifying,
    /// Building the offline search index from the package's tiles.
    Indexing {
        done: u64,
        total: u64,
    },
    Activating,
}

pub struct UpdateOptions {
    /// Hard cap on the package size.
    pub max_bytes: u64,
    /// Free space to leave on the drive after the download completes.
    pub reserve_bytes: u64,
    /// Free bytes on the volume holding `path`.
    pub available_space: fn(&Path) -> io::Result<u64>,
    /// Current UTC time, RFC 3339.
    pub clock: fn() -> String,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        Self {
            max_bytes: 2 << 30,
            reserve_bytes: 256 << 20,
            available_space: |path| fs4::available_space(path),
            clock: crate::time::utc_now,
        }
    }
}

#[derive(Debug)]
pub enum UpdateOutcome {
    /// The server has nothing newer than the active package (the same file, or
    /// an older one from a stale cache); the active package is unchanged.
    UpToDate(ActivePackage),
    Activated {
        package: ActivePackage,
        replaced: Option<String>,
        /// Old versions that could not be removed yet (for example, open in
        /// the renderer). They are removed by a later prune.
        prune_pending: usize,
    },
}

#[derive(Debug)]
pub enum UpdateError {
    Busy,
    Cancelled,
    Network(io::Error),
    Storage(io::Error),
    TooLarge {
        size: u64,
        limit: u64,
    },
    InsufficientSpace {
        needed: u64,
        available: u64,
    },
    /// The download completed but is not a usable package. It was discarded.
    Invalid(PackageError),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const MB: u64 = 1_000_000;
        match self {
            Self::Busy => write!(f, "Another map update is already running."),
            Self::Cancelled => write!(
                f,
                "Update cancelled. The download will resume next time if the file is unchanged."
            ),
            Self::Network(e) => write!(f, "Download failed: {e}. The previous map is unchanged."),
            Self::Storage(e) => write!(
                f,
                "Could not write map files: {e}. The previous map is unchanged."
            ),
            Self::TooLarge { size, limit } => write!(
                f,
                "The map package is {} MB, larger than the {} MB limit.",
                size / MB,
                limit / MB
            ),
            Self::InsufficientSpace { needed, available } => write!(
                f,
                "Not enough disk space: {} MB needed, {} MB available.",
                needed.div_ceil(MB),
                available / MB
            ),
            Self::Invalid(e) => write!(
                f,
                "{e}. The download was discarded; the previous map is unchanged."
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

pub(crate) fn network(error: io::Error) -> UpdateError {
    if error.kind() == io::ErrorKind::Interrupted {
        UpdateError::Cancelled
    } else {
        UpdateError::Network(error)
    }
}

/// Writer that reports cumulative progress.
struct Counting<'a, W> {
    inner: W,
    received: u64,
    total: Option<u64>,
    progress: &'a mut dyn FnMut(Progress),
}

impl<W: Write> Write for Counting<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.received += n as u64;
        (self.progress)(Progress::Downloading {
            received: self.received,
            total: self.total,
        });
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Brings the region's package up to date with `region.package_url`.
pub fn update_region(
    store: &PackageStore,
    region: &Region,
    transport: &dyn Transport,
    options: &UpdateOptions,
    progress: &mut dyn FnMut(Progress),
    cancelled: &AtomicBool,
) -> Result<UpdateOutcome, UpdateError> {
    let id = region.id;
    let url = region.package_url;
    let _lock = store.lock(id).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => UpdateError::Busy,
        _ => UpdateError::Storage(e),
    })?;
    // A broken active package must not block downloading a replacement.
    let active = store.active(id).ok().flatten();

    progress(Progress::Checking);
    let remote = transport.probe(url).map_err(network)?;
    if let Some(active) = active.as_ref().filter(|a| {
        is_same_file(&a.record, url, &remote)
            || is_older(
                remote.last_modified.as_deref(),
                a.record.source.last_modified.as_deref(),
            )
    }) {
        return Ok(UpdateOutcome::UpToDate(active.clone()));
    }
    check_size(remote.length, options)?;

    let source = download_to_staging(
        store, id, url, &remote, transport, options, progress, cancelled,
    )?;
    let part = store.staging_file(id);

    if cancelled.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    progress(Progress::Verifying);
    let downloaded_at = (options.clock)();
    let record = match verify_package(
        &part,
        region,
        source,
        &downloaded_at,
        &downloaded_at,
        cancelled,
    ) {
        Ok(record) => record,
        Err(PackageError::Io(e)) if e.kind() == io::ErrorKind::Interrupted => {
            return Err(UpdateError::Cancelled);
        }
        Err(PackageError::Io(e)) => return Err(UpdateError::Storage(e)),
        Err(e) => {
            let _ = store.clear_staging(id);
            return Err(UpdateError::Invalid(e));
        }
    };
    let record = PackageRecord {
        verified_at: (options.clock)(),
        ..record
    };

    // The transfer may have reached a staler cache than the probe did.
    if let Some(active) = active.as_ref().filter(|a| {
        is_older(
            record.source.last_modified.as_deref(),
            a.record.source.last_modified.as_deref(),
        )
    }) {
        let _ = store.clear_staging(id);
        return Ok(UpdateOutcome::UpToDate(active.clone()));
    }

    // Search is part of a ready package: build it before activation.
    let index = store.staging_dir(id).join(SEARCH_FILE);
    let indexed = TilePackage::open(&part).and_then(|package| {
        build_index(
            &package,
            &record.sha256,
            &index,
            cancelled,
            &mut |done, total| progress(Progress::Indexing { done, total }),
        )
    });
    match indexed {
        Ok(_) => {}
        Err(PackageError::Io(e)) if e.kind() == io::ErrorKind::Interrupted => {
            return Err(UpdateError::Cancelled);
        }
        Err(PackageError::Io(e)) => return Err(UpdateError::Storage(e)),
        Err(e) => {
            let _ = store.clear_staging(id);
            return Err(UpdateError::Invalid(e));
        }
    }

    progress(Progress::Activating);
    let version = version_name(&record);
    store
        .install_version(id, &version, &part, Some(&index), &record)
        .map_err(UpdateError::Storage)?;
    store.activate(id, &version).map_err(UpdateError::Storage)?;
    let _ = store.clear_staging(id);
    let package = store
        .active(id)
        .map_err(|e| UpdateError::Storage(io::Error::other(e.to_string())))?
        .ok_or_else(|| UpdateError::Storage(io::Error::other("Activated package is missing")))?;
    let prune_pending = store.prune(id).len();
    Ok(UpdateOutcome::Activated {
        package,
        replaced: active.map(|a| a.version),
        prune_pending,
    })
}

/// Downloads `url` into the staging file of `id` (a region, or a region's
/// extras), resuming a previous partial copy only when the server still
/// reports the same file. Returns what the server said about the stored file.
#[allow(clippy::too_many_arguments)]
pub(crate) fn download_to_staging(
    store: &PackageStore,
    id: &str,
    url: &str,
    remote: &RemoteInfo,
    transport: &dyn Transport,
    options: &UpdateOptions,
    progress: &mut dyn FnMut(Progress),
    cancelled: &AtomicBool,
) -> Result<SourceInfo, UpdateError> {
    // Decide whether the staging file can be resumed.
    let part = store.staging_file(id);
    let mut resume_from = 0;
    match store.read_staging(id) {
        Some((state, length))
            if state.url == url
                && remote.strong_etag().is_some()
                && state.etag == remote.etag
                && state.total == remote.length
                && remote.length.is_some_and(|total| length <= total) =>
        {
            resume_from = length;
        }
        _ => store.clear_staging(id).map_err(UpdateError::Storage)?,
    }
    let mut source = SourceInfo {
        url: url.to_owned(),
        etag: remote.etag.clone(),
        last_modified: remote.last_modified.clone(),
    };
    let mut total = remote.length;

    if total != Some(resume_from) || resume_from == 0 {
        check_space(store, id, total.map(|t| t - resume_from), options)?;
        store
            .write_staging(id, &staging_state(url, remote))
            .map_err(UpdateError::Storage)?;
        let etag = remote
            .strong_etag()
            .filter(|_| resume_from > 0 && remote.accepts_ranges);
        let fetched = transport
            .fetch(url, etag.map(|e| (resume_from, e)))
            .map_err(network)?;
        let (start, info, mut body) = match fetched {
            Fetched::Partial { start, info, body }
                if start == resume_from && info.length == total =>
            {
                (start, info, body)
            }
            Fetched::Partial { .. } => {
                store.clear_staging(id).map_err(UpdateError::Storage)?;
                return Err(UpdateError::Network(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Server returned an unexpected byte range; the download will restart",
                )));
            }
            // The server sent the whole file: it ignored the range or the
            // file changed since the probe. Start over with what it sent.
            Fetched::Full { info, body } => {
                if start_over(&info, remote) || resume_from > 0 {
                    check_size(info.length, options)?;
                    check_space(store, id, info.length, options)?;
                    store
                        .write_staging(id, &staging_state(url, &info))
                        .map_err(UpdateError::Storage)?;
                }
                (0, info, body)
            }
        };
        if start == 0 {
            source.etag = info.etag.clone();
            source.last_modified = info.last_modified.clone();
            total = info.length;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .map_err(UpdateError::Storage)?;
        file.set_len(start).map_err(UpdateError::Storage)?;
        file.seek(io::SeekFrom::End(0))
            .map_err(UpdateError::Storage)?;
        let remaining = total.map(|t| t - start);
        let mut writer = Counting {
            inner: io::BufWriter::with_capacity(1 << 20, &mut file),
            received: start,
            total,
            progress,
        };
        let budget = remaining.unwrap_or(options.max_bytes - start);
        let result = stage_download(&mut body, &mut writer, remaining, budget, cancelled);
        let flushed = writer.flush();
        drop(writer);
        let synced = file.sync_all();
        drop(file);
        match result {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(UpdateError::Cancelled),
            // More data than declared: the staging copy is not trustworthy.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                let _ = store.clear_staging(id);
                return Err(UpdateError::Network(e));
            }
            // Truncated or failed transfer: keep the staging file for resume.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(UpdateError::Network(e));
            }
            Err(e) => return Err(classify_transfer_error(e)),
        }
        flushed.map_err(UpdateError::Storage)?;
        synced.map_err(UpdateError::Storage)?;
    }
    Ok(source)
}

/// Disk errors (from the writer) and network errors (from the body reader)
/// both arrive as `io::Error`; storage-type kinds are attributed to the disk.
fn classify_transfer_error(error: io::Error) -> UpdateError {
    match error.kind() {
        io::ErrorKind::StorageFull
        | io::ErrorKind::PermissionDenied
        | io::ErrorKind::ReadOnlyFilesystem
        | io::ErrorKind::QuotaExceeded => UpdateError::Storage(error),
        _ => UpdateError::Network(error),
    }
}

fn start_over(info: &RemoteInfo, probed: &RemoteInfo) -> bool {
    info.etag != probed.etag || info.length != probed.length
}

fn staging_state(url: &str, info: &RemoteInfo) -> StagingState {
    StagingState {
        url: url.to_owned(),
        etag: info.etag.clone(),
        last_modified: info.last_modified.clone(),
        total: info.length,
    }
}

/// True when `last_modified` is strictly older than `current`.
/// Unparseable or missing dates never count as older.
pub(crate) fn is_older(last_modified: Option<&str>, current: Option<&str>) -> bool {
    let parse = |v: Option<&str>| v.and_then(crate::time::parse_http_date);
    matches!(
        (parse(last_modified), parse(current)),
        (Some(remote), Some(current)) if remote < current
    )
}

fn is_same_file(record: &PackageRecord, url: &str, remote: &RemoteInfo) -> bool {
    record.source.url == url
        && remote.length == Some(record.byte_length)
        && match (&remote.etag, &record.source.etag) {
            (Some(a), Some(b)) => a == b,
            _ => {
                remote.last_modified.is_some()
                    && remote.last_modified == record.source.last_modified
            }
        }
}

pub(crate) fn check_size(length: Option<u64>, options: &UpdateOptions) -> Result<(), UpdateError> {
    match length {
        Some(size) if size > options.max_bytes => Err(UpdateError::TooLarge {
            size,
            limit: options.max_bytes,
        }),
        Some(0) => Err(UpdateError::Network(io::Error::new(
            io::ErrorKind::InvalidData,
            "Server reported an empty file",
        ))),
        _ => Ok(()),
    }
}

fn check_space(
    store: &PackageStore,
    id: &str,
    remaining: Option<u64>,
    options: &UpdateOptions,
) -> Result<(), UpdateError> {
    let dir = store.staging_dir(id);
    std::fs::create_dir_all(&dir).map_err(UpdateError::Storage)?;
    let available = (options.available_space)(&dir).map_err(UpdateError::Storage)?;
    let needed = remaining.unwrap_or(options.max_bytes) + options.reserve_bytes;
    if available < needed {
        return Err(UpdateError::InsufficientSpace { needed, available });
    }
    Ok(())
}

/// `20261108T071240Z-17c12c9500b5` from the verification time and hash.
fn version_name(record: &PackageRecord) -> String {
    let stamp: String = record
        .verified_at
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("{stamp}-{}", &record.sha256[..12])
}

#[cfg(test)]
#[path = "download_tests.rs"]
pub(crate) mod tests;
