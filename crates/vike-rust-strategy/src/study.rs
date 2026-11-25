//! **A research study FILE across the C ABI**: the byte boundary a built study artifact and the
//! process that loaded it talk over (owner's rule, 2026-11-15: a study is a FILE, built, loaded and
//! sandboxed exactly like a strategy file).
//!
//! A study is called ONCE and asks its host for data while it runs, so its ABI is not a vtable of
//! hooks but two exports and one call slot, each carrying BYTES:
//!
//! - `vike_study_run(host, input, output)` — run the study: `input` is the start message, `host` the
//!   slot every request the study makes goes through, `output` where its finished answer is written.
//! - `vike_study_serve(request, output)` — answer one request the HOST makes of an object the study
//!   handed out (the store it passed to its one backtest).
//!
//! What the bytes MEAN is `vike_user_research::file`'s (the typed messages, both halves, in safe
//! code), never this module's: here is only the quarantined act of lending bytes across, so the one
//! crate allowed `unsafe` holds the three sites it takes and nothing typed. The handshake is the
//! strategy file's (`vike_strategy_abi_version`, `vike_strategy_fingerprint`, `vike_strategy_set_log`,
//! [`crate::loader::load_study`]): one builder, one folder, one loader, one ABI number.
//!
//! # Ownership (every crossing in this module)
//!
//! - A [`BytesAbi`] is BORROWED for the one call it is passed to; the callee copies it before it
//!   returns ([`BytesAbi::to_vec`]).
//! - A [`SinkAbi`] is the CALLER's buffer: the callee writes its answer into it ([`SinkAbi::put`]),
//!   and nothing written is freed across.
//! - A panic never crosses: [`run_export`], [`serve_export`] and the host's call slot catch it and
//!   write NOTHING, which the typed layer reads as "no answer".

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// Bytes lent for one call: `(ptr, len)`, valid until the call returns.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BytesAbi {
    pub ptr: *const u8,
    pub len: usize,
}

impl BytesAbi {
    /// Lends `bytes` for one call.
    pub fn of(bytes: &[u8]) -> Self {
        BytesAbi { ptr: bytes.as_ptr(), len: bytes.len() }
    }

    /// The lent bytes, copied. Empty for a zero length, whatever the pointer.
    pub fn to_vec(self) -> Vec<u8> {
        if self.len == 0 || self.ptr.is_null() {
            return Vec::new();
        }
        // SAFETY: the lender keeps `(ptr, len)` alive and unmoved for the duration of the call this
        // value was passed to (this module's ownership rule), and this copy happens inside it.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }.to_vec()
    }
}

/// Where a callee writes its answer: the caller's buffer and the function that appends to it.
#[repr(C)]
pub struct SinkAbi {
    pub sink: *mut c_void,
    pub write: extern "C" fn(*mut c_void, BytesAbi),
}

impl SinkAbi {
    /// A sink appending to `buffer`, which must outlive the call it is passed to.
    pub fn into(buffer: &mut Vec<u8>) -> Self {
        SinkAbi { sink: std::ptr::from_mut(buffer).cast(), write: sink_write }
    }

    /// Append `bytes` to the caller's buffer.
    pub fn put(&self, bytes: &[u8]) {
        (self.write)(self.sink, BytesAbi::of(bytes));
    }
}

/// [`SinkAbi::into`]'s writer.
extern "C" fn sink_write(sink: *mut c_void, bytes: BytesAbi) {
    let bytes = bytes.to_vec();
    // SAFETY: `sink` is the `&mut Vec<u8>` `SinkAbi::into` erased, alive for the call the sink was
    // passed to (its doc), and only that call writes through it.
    let buffer = unsafe { &mut *sink.cast::<Vec<u8>>() };
    buffer.extend_from_slice(&bytes);
}

/// The host's call slot: answer `request` into `answer`.
pub type HostCallFn = extern "C" fn(host: *const c_void, request: BytesAbi, answer: SinkAbi);

/// The host as a study file sees it: an opaque pointer and its call slot.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StudyHostAbi {
    pub host: *const c_void,
    pub call: HostCallFn,
}

/// `vike_study_run`'s type.
pub type StudyRunFn = extern "C" fn(host: StudyHostAbi, input: BytesAbi, output: SinkAbi);

/// `vike_study_serve`'s type.
pub type StudyServeFn = extern "C" fn(request: BytesAbi, output: SinkAbi);

// ---- the study file's side -------------------------------------------------------------------

/// The host, held by the study file: `Copy`, `Send` and `Sync` (the pointer is kept as an address),
/// so a study's own threads may call it while `vike_study_run` is running. A call after
/// `vike_study_run` returned reaches a host that is gone; the process exits right after it.
#[derive(Clone, Copy, Debug)]
pub struct HostLink {
    host: usize,
    call: HostCallFn,
}

impl HostLink {
    /// Ask the host; its answer, or empty when it had none (it panicked).
    pub fn call(&self, request: &[u8]) -> Vec<u8> {
        let mut answer = Vec::new();
        (self.call)(self.host as *const c_void, BytesAbi::of(request), SinkAbi::into(&mut answer));
        answer
    }
}

/// The body of the template's `vike_study_run`: the start message copied, `run` called with the
/// host, its answer written to `output`. A panic writes nothing.
pub fn run_export(
    host: StudyHostAbi,
    input: BytesAbi,
    output: SinkAbi,
    run: impl FnOnce(HostLink, Vec<u8>) -> Vec<u8>,
) {
    let link = HostLink { host: host.host as usize, call: host.call };
    let input = input.to_vec();
    if let Ok(answer) = catch_unwind(AssertUnwindSafe(|| run(link, input))) {
        output.put(&answer);
    }
}

/// The body of the template's `vike_study_serve`: the request copied, `serve` called, its answer
/// written to `output`. A panic writes nothing.
pub fn serve_export(request: BytesAbi, output: SinkAbi, serve: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let request = request.to_vec();
    if let Ok(answer) = catch_unwind(AssertUnwindSafe(|| serve(request))) {
        output.put(&answer);
    }
}

// ---- the host's side -------------------------------------------------------------------------

/// What a host answers a study file's requests with.
pub type Host<'a> = &'a (dyn Fn(&[u8]) -> Vec<u8> + Sync);

/// A loaded, handshake-checked study file ([`crate::loader::load_study`]). `Send` and `Sync`: two
/// function pointers and a path.
#[derive(Debug, Clone)]
pub struct StudyFileLoaded {
    /// The artifact it was loaded from.
    pub path: PathBuf,
    pub(crate) run: StudyRunFn,
    pub(crate) serve: StudyServeFn,
}

impl StudyFileLoaded {
    /// Run the study over the start message `input`, answering every request it makes with `host`
    /// (from whichever of its threads asks); its finished answer, or empty when it panicked.
    pub fn run(&self, input: &[u8], host: Host<'_>) -> Vec<u8> {
        let abi = StudyHostAbi { host: std::ptr::from_ref(&host).cast(), call: host_call };
        let mut output = Vec::new();
        (self.run)(abi, BytesAbi::of(input), SinkAbi::into(&mut output));
        output
    }

    /// Ask the study file to answer `request` about an object it handed out; empty when it panicked.
    pub fn serve(&self, request: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        (self.serve)(BytesAbi::of(request), SinkAbi::into(&mut output));
        output
    }
}

/// [`StudyFileLoaded::run`]'s call slot: the host closure it lent, called with the request. A panic
/// answers nothing.
extern "C" fn host_call(host: *const c_void, request: BytesAbi, answer: SinkAbi) {
    // SAFETY: `host` is the `&Host` `StudyFileLoaded::run` erased, alive until that call returns;
    // the study file calls this slot only while `vike_study_run` runs (`HostLink`'s doc), and `Host`
    // is `Sync`, so its threads may share it.
    let host = unsafe { &*host.cast::<Host<'_>>() };
    let request = request.to_vec();
    if let Ok(bytes) = catch_unwind(AssertUnwindSafe(|| host(&request))) {
        answer.put(&bytes);
    }
}

#[path = "study_tests.rs"]
#[cfg(test)]
mod study_tests;
