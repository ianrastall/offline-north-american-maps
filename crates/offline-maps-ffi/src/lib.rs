//! C ABI facade over the Offline Maps core for the WinUI shell.
//!
//! Commands and queries pass as protobuf messages (`proto/offline_maps.proto`):
//! the caller sends a serialized `Request`, and the core returns a serialized
//! `Response` in a Rust-owned result buffer. Map tiles, the one bulk transfer,
//! have their own export returning the raw (decompressed) tile bytes.
//!
//! # Contract (ABI_VERSION 1)
//!
//! - Every export uses the C calling convention and never unwinds: panics are
//!   caught, reported as `STATUS_PANIC`, and mark the core unusable. After that
//!   only `om_cancel`, `om_progress`, `om_result_*` and `om_destroy` work;
//!   other calls return `STATUS_UNUSABLE`.
//! - Out pointers are set to null (or zero) before anything else. A result is
//!   set only with `STATUS_OK`, except `om_create`, which may set its error
//!   result with `STATUS_CREATE_FAILED`.
//! - Expected failures (no map installed, a network error) are not statuses:
//!   they arrive as `Response.error` with `STATUS_OK`.
//! - Input bytes stay owned by the caller and are read only during the call.
//!   Requests larger than 1 MiB are rejected.
//! - Result bytes are valid until `om_result_free` and must be copied first.
//!   Each result is freed exactly once; `om_destroy` is called exactly once,
//!   after every other call on that core has returned.
//! - A core may be called from several threads at once. `om_cancel` and
//!   `om_progress` never wait behind a running update.
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use prost::Message;

/// Generated from proto/offline_maps.proto.
#[allow(clippy::all)]
pub mod messages {
    include!(concat!(env!("OUT_DIR"), "/offline_maps.rs"));
}
pub mod service;

use messages as pb;
use pb::ErrorKind;
use service::{Service, error};

pub const ABI_VERSION: u32 = 1;

pub const STATUS_OK: u32 = 0;
pub const STATUS_INVALID_ARGUMENT: u32 = 1;
pub const STATUS_MALFORMED_REQUEST: u32 = 2;
pub const STATUS_PANIC: u32 = 3;
pub const STATUS_UNUSABLE: u32 = 4;
pub const STATUS_CREATE_FAILED: u32 = 5;

const MAX_REQUEST_BYTES: usize = 1 << 20;

pub struct Core {
    service: Service,
    unusable: AtomicBool,
}

/// Bytes owned by Rust until `om_result_free`.
pub struct ResultBuffer(Vec<u8>);

fn into_result(bytes: Vec<u8>) -> *mut ResultBuffer {
    Box::into_raw(Box::new(ResultBuffer(bytes)))
}

/// # Safety
/// `bytes` must be valid for `length` bytes when non-null.
unsafe fn input<'a>(bytes: *const u8, length: usize) -> Option<&'a [u8]> {
    if length > MAX_REQUEST_BYTES || (bytes.is_null() && length > 0) {
        return None;
    }
    if length == 0 {
        return Some(&[]);
    }
    Some(unsafe { std::slice::from_raw_parts(bytes, length) })
}

#[unsafe(no_mangle)]
pub extern "C" fn om_abi_version() -> u32 {
    ABI_VERSION
}

/// Creates a core from a serialized `Config`. On `STATUS_CREATE_FAILED`,
/// `out_error` may hold a `Response` whose `error` explains why.
///
/// # Safety
/// `config` must be valid for `length` bytes; `out_core` and `out_error` must
/// be valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_create(
    config: *const u8,
    length: usize,
    out_core: *mut *mut Core,
    out_error: *mut *mut ResultBuffer,
) -> u32 {
    if out_core.is_null() || out_error.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *out_core = ptr::null_mut();
        *out_error = ptr::null_mut();
    }
    let Some(bytes) = (unsafe { input(config, length) }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    let outcome = catch_unwind(|| {
        let config = pb::Config::decode(bytes).map_err(|_| None)?;
        Service::open(&config).map_err(Some)
    });
    match outcome {
        Ok(Ok(service)) => {
            unsafe {
                *out_core = Box::into_raw(Box::new(Core {
                    service,
                    unusable: AtomicBool::new(false),
                }));
            }
            STATUS_OK
        }
        Ok(Err(None)) => STATUS_MALFORMED_REQUEST,
        Ok(Err(Some(e))) => {
            let response = pb::Response {
                error: Some(e),
                ..Default::default()
            };
            unsafe { *out_error = into_result(response.encode_to_vec()) };
            STATUS_CREATE_FAILED
        }
        Err(_) => STATUS_PANIC,
    }
}

fn dispatch(service: &Service, request: pb::Request) -> pb::Response {
    use pb::request::Command;
    use pb::response::Result as R;
    let request_id = request.request_id;
    let result = match request.command {
        None => Err(error(
            ErrorKind::InvalidArgument,
            "The request has no command.",
        )),
        Some(Command::GetStatus(_)) => Ok(Some(R::Status(service.status()))),
        Some(Command::Search(search)) => service.search(&search).map(|r| Some(R::Search(r))),
        Some(Command::AddPlace(add)) => service.add_place(&add).map(|p| Some(R::Place(p))),
        Some(Command::UpdatePlace(update)) => {
            service.update_place(&update).map(|p| Some(R::Place(p)))
        }
        Some(Command::RemovePlace(remove)) => service.remove_place(remove.id).map(|()| None),
        Some(Command::Update(update)) => service.update(&update).map(|r| Some(R::Update(r))),
        Some(Command::GetStations(_)) => service.stations().map(|s| Some(R::Stations(s))),
        Some(Command::PrepareSearch(_)) => service.prepare_search().map(|()| None),
        Some(Command::ResetPlaces(_)) => service.reset_places().map(|()| None),
        Some(Command::RemoveRegion(remove)) => service.remove_region(&remove).map(|()| None),
        Some(Command::GetCatalog(_)) => Ok(Some(R::Catalog(service.catalog()))),
        Some(Command::GetTerrainTile(tile)) => {
            service.terrain_tile(&tile).map(|t| Some(R::TerrainTile(t)))
        }
        Some(Command::GetOverlayTile(tile)) => {
            service.overlay_tile(&tile).map(|t| Some(R::OverlayTile(t)))
        }
    };
    match result {
        Ok(result) => pb::Response {
            request_id,
            error: None,
            result,
        },
        Err(e) => pb::Response {
            request_id,
            error: Some(e),
            result: None,
        },
    }
}

/// Runs `f` unless the core is unusable, containing panics.
///
/// # Safety
/// `core` must be null or a live pointer from `om_create`.
unsafe fn guarded(core: *const Core, f: impl FnOnce(&Core) -> u32) -> u32 {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    if core.unusable.load(Ordering::Acquire) {
        return STATUS_UNUSABLE;
    }
    match catch_unwind(AssertUnwindSafe(|| f(core))) {
        Ok(status) => status,
        Err(_) => {
            core.unusable.store(true, Ordering::Release);
            STATUS_PANIC
        }
    }
}

/// Runs a serialized `Request` and returns a serialized `Response`.
///
/// # Safety
/// `core` must be live; `request` valid for `length` bytes; `out_result`
/// valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_call(
    core: *const Core,
    request: *const u8,
    length: usize,
    out_result: *mut *mut ResultBuffer,
) -> u32 {
    if out_result.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { *out_result = ptr::null_mut() };
    let Some(bytes) = (unsafe { input(request, length) }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    unsafe {
        guarded(core, |core| {
            let Ok(request) = pb::Request::decode(bytes) else {
                return STATUS_MALFORMED_REQUEST;
            };
            let response = dispatch(&core.service, request);
            *out_result = into_result(response.encode_to_vec());
            STATUS_OK
        })
    }
}

/// The raw vector tile at z/x/y. An empty result means the package has no
/// tile there (or no map is installed).
///
/// # Safety
/// `core` must be live and `out_result` valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_tile(
    core: *const Core,
    z: u32,
    x: u32,
    y: u32,
    out_result: *mut *mut ResultBuffer,
) -> u32 {
    if out_result.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { *out_result = ptr::null_mut() };
    unsafe {
        guarded(core, |core| match core.service.tile(z, x, y) {
            Ok(tile) => {
                *out_result = into_result(tile.unwrap_or_default());
                STATUS_OK
            }
            Err(_) => STATUS_INVALID_ARGUMENT,
        })
    }
}

/// The current update's progress as a serialized `ProgressEvent`. Works after
/// a panic so the shell can still show what happened.
///
/// # Safety
/// `core` must be live and `out_result` valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_progress(core: *const Core, out_result: *mut *mut ResultBuffer) -> u32 {
    if out_result.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe { *out_result = ptr::null_mut() };
    let Some(core) = (unsafe { core.as_ref() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    match catch_unwind(AssertUnwindSafe(|| core.service.progress().encode_to_vec())) {
        Ok(bytes) => {
            unsafe { *out_result = into_result(bytes) };
            STATUS_OK
        }
        Err(_) => STATUS_PANIC,
    }
}

/// Asks the running update to stop. Returns at once.
///
/// # Safety
/// `core` must be null or live.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_cancel(core: *const Core) -> u32 {
    let Some(core) = (unsafe { core.as_ref() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    match catch_unwind(AssertUnwindSafe(|| core.service.cancel())) {
        Ok(()) => STATUS_OK,
        Err(_) => STATUS_PANIC,
    }
}

/// # Safety
/// `result` must be null or a live result; the out pointers valid for writes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_result_bytes(
    result: *const ResultBuffer,
    out_bytes: *mut *const u8,
    out_length: *mut usize,
) -> u32 {
    if out_bytes.is_null() || out_length.is_null() {
        return STATUS_INVALID_ARGUMENT;
    }
    unsafe {
        *out_bytes = ptr::null();
        *out_length = 0;
    }
    let Some(result) = (unsafe { result.as_ref() }) else {
        return STATUS_INVALID_ARGUMENT;
    };
    unsafe {
        *out_bytes = result.0.as_ptr();
        *out_length = result.0.len();
    }
    STATUS_OK
}

/// # Safety
/// `result` must be null or a live result, freed only once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_result_free(result: *mut ResultBuffer) {
    if !result.is_null() {
        drop(unsafe { Box::from_raw(result) });
    }
}

/// Releases the core. Safe after a panic: dropping does not depend on
/// domain invariants (files are closed; an update cannot be running because
/// every call has returned).
///
/// # Safety
/// `core` must be null or live, with no call in progress; destroyed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn om_destroy(core: *mut Core) {
    if !core.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(core) })));
    }
}

#[cfg(test)]
mod tests;
