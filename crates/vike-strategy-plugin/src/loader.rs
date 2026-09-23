//! The loader: `dlopen`, the ABI-version check, the fingerprint check, and the refusals.
//!
//! ⚠ **`dlclose` is never called.** An edit produces a new artifact and a new `dlopen`; the old
//! handle stays mapped and unused. Unloading a library something may still reference is unsafe —
//! see the design doc's "Deliberately absent: `dlclose`" section. The leak is bounded by edits per
//! session and cleared by an ordinary process restart.
//!
//! Only Linux is a real target: both the backtest server and the builder service run on
//! self-hosted Linux boxes (this crate's own module doc / the design's "sandbox constraint"
//! section), and this crate is not in the `windows-cross` compile-witness lane. `#[cfg(unix)]`
//! carries the real implementation; a non-unix build still compiles (`plat::open`/`plat::sym` both
//! return `Err` unconditionally) rather than failing to build at all, in case that ever changes.

use std::ffi::c_void;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};

use crate::abi;
use crate::fingerprint;
use crate::host::PluginVTable;

/// Why a plugin was refused, or could not even be reached.
#[derive(Debug)]
pub enum LoadError {
    /// The path does not name a file at all.
    Missing(PathBuf),
    /// `dlopen` itself failed — the file exists but is not a loadable shared object for this
    /// platform/architecture, or one of its own dependencies could not be resolved. Carries the
    /// DYNAMIC LOADER's own message (`dlerror`), not just the path: which of those causes it was
    /// is a fact only `ld.so` holds, and without it this variant tells an operator nothing they
    /// did not already know (see `plat::open`).
    DlOpen(String),
    /// A required export could not be resolved via `dlsym`.
    NoSymbol(String),
    /// `vike_plugin_abi_version()` disagreed with [`abi::ABI_VERSION`] — a DELIBERATE vtable
    /// change the plugin was not rebuilt against.
    AbiMismatch { host: u32, plugin: u32 },
    /// `vike_plugin_fingerprint()` disagreed with [`fingerprint::FINGERPRINT`] — an ACCIDENTAL
    /// toolchain divergence between the box that built the plugin and this host.
    FingerprintMismatch { host: String, plugin: String },
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Missing(p) => write!(
                f,
                "plugin artifact not found at `{}`. Build it first — the builder service \
                 produces `user_data/plugins/<name>-<sha>.so`; see the design doc's \"Flow\" \
                 section (docs/superpowers/specs/2026-09-21-runtime-loaded-rust-strategies-design.md).",
                p.display()
            ),
            LoadError::DlOpen(e) => write!(f, "failed to open plugin shared object: {e}"),
            LoadError::NoSymbol(name) => write!(f, "plugin is missing required export `{name}`"),
            LoadError::AbiMismatch { host, plugin } => write!(
                f,
                "plugin ABI version {plugin} does not match this host's {host} \
                 (crate::abi::ABI_VERSION) — rebuild the plugin against this host's version"
            ),
            LoadError::FingerprintMismatch { host, plugin } => write!(
                f,
                "plugin toolchain fingerprint `{plugin}` does not match this host's `{host}` — \
                 rebuild the plugin with the same toolchain/target/profile as the host"
            ),
        }
    }
}

impl std::error::Error for LoadError {}

/// A successfully loaded, ABI- and fingerprint-checked plugin, ready to wrap as a
/// [`crate::host::PluginStrategy`].
#[derive(Debug)]
pub struct LoadedPlugin {
    /// Kept only to document ownership of the mapping — never closed, see this module's header.
    #[allow(dead_code)]
    handle: *mut c_void,
    pub path: PathBuf,
    pub vtable: PluginVTable,
}

/// Resolve one symbol and transmute it to the type inference expects from context, or
/// `Err(LoadError::NoSymbol)`.
///
/// A macro rather than a generic function for the same reason `crates/bridges/fxcm/src/loader.rs`
/// has one (`bind!`): nothing checks the transmute against the real symbol either way, so this
/// keeps the unsafe act to ONE textual site — used at every symbol `load` resolves below, a set
/// that TRIPLED at `ABI_VERSION` 3 without adding an unsafe site, which is the whole return on
/// the macro — rather than one per resolved symbol, the same dedup that file argues at its own
/// macro. (This line carried the call-site COUNT until that widening made it wrong.) Defined ahead of `load`
/// (rather than after, with a `use bind;` to hoist it) because a plain `macro_rules!` is scoped
/// textually: it is visible from its declaration to the end of the enclosing module, not before.
macro_rules! bind {
    ($handle:expr, $name:literal, $ty:ty) => {{
        match plat::sym($handle, $name) {
            Ok(p) => Ok(
                // SAFETY: nothing checks this cast — that is the unsafe act, and it is why the
                // exported symbol's real signature and `$ty` must agree. A MISSING symbol is
                // caught by `plat::sym` above, before the cast; the failure mode this cannot see
                // is a symbol present with a DIFFERENT signature (a stale or foreign `.so` built
                // against an older ABI_VERSION — which is exactly what the `abi_version`
                // handshake this function's own callers run FIRST exists to catch before any
                // other symbol is trusted).
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) },
            ),
            Err(_) => Err(LoadError::NoSymbol($name.to_string())),
        }
    }};
}

// The three dispatch SHAPES that recur across `ABI_VERSION` 3's thirteen new exports, named so the
// `bind!` calls below read as a list of symbols rather than as a wall of repeated signatures.
//
// ⚠ These are aliases, not a second definition: each resolves to exactly the `extern "C" fn` type
// the matching `PluginVTable` field declares, so a signature changed on one side and not the other
// is a type error at the assignment rather than a `transmute` nobody checks. The four exports whose
// shape is unique (`on_bar`, `on_order_book`, `on_feed_status`, `on_reference_quote`) are spelled
// out at their own call sites instead — an alias used once is a name to look up, not a shorthand.

/// `on_start` / `on_stop`: the broker and nothing else.
type LifecycleFn = extern "C" fn(*mut c_void, crate::abi::BrokerRef) -> crate::abi::PluginStatus;

/// One borrowed `#[repr(C)]` payload behind a pointer — the `CBar` shape, generalised.
type PayloadFn<T> =
    extern "C" fn(*mut c_void, crate::abi::BrokerRef, *const T) -> crate::abi::PluginStatus;

/// A borrowed `(ptr, len)` string: `on_schedule`'s tag, `on_params_updated`'s JSON document.
type BorrowedStrFn =
    extern "C" fn(*mut c_void, crate::abi::BrokerRef, *const u8, usize) -> crate::abi::PluginStatus;

/// `dlopen` `path` and run the handshake ONLY — the two REQUIRED exports every plugin carries
/// regardless of `ABI_VERSION` (`vike_plugin_abi_version`, `vike_plugin_fingerprint`) — resolving
/// nothing else. Shared by [`load`] (which keeps the handle and resolves the rest of the vtable
/// against it) and [`verify_handshake`] (which needs nothing more than the answer).
///
/// Split out rather than duplicated: the two handshake checks used to be inlined at the top of
/// `load`, and a second, independent copy of them (for a caller that wants the answer without the
/// other fifteen symbols) would have been a second place for the comparison to be spelled
/// slightly differently — the same dedup argument this file's own `bind!` macro doc already makes
/// for a single textual `unsafe` site.
fn open_and_handshake(path: &Path) -> Result<*mut c_void, LoadError> {
    if !path.is_file() {
        return Err(LoadError::Missing(path.to_path_buf()));
    }
    let path_str = path.to_string_lossy().into_owned();
    let handle = plat::open(&path_str).map_err(LoadError::DlOpen)?;

    let abi_version_fn = bind!(handle, "vike_plugin_abi_version", extern "C" fn() -> u32)?;
    let plugin_abi = abi_version_fn();
    if plugin_abi != abi::ABI_VERSION {
        return Err(LoadError::AbiMismatch { host: abi::ABI_VERSION, plugin: plugin_abi });
    }

    let fingerprint_fn =
        bind!(handle, "vike_plugin_fingerprint", extern "C" fn() -> *const c_char)?;
    let plugin_fp = read_c_str(fingerprint_fn());
    if plugin_fp != fingerprint::FINGERPRINT {
        return Err(LoadError::FingerprintMismatch {
            host: fingerprint::FINGERPRINT.to_string(),
            plugin: plugin_fp,
        });
    }
    Ok(handle)
}

/// Verify that the artifact at `path` still handshakes with THIS host's ABI version and toolchain
/// fingerprint, without resolving the other fifteen dispatch symbols [`load`] needs to actually
/// run it.
///
/// ⚠ **This exists for a cache check, not for a caller that intends to run the plugin.** The
/// strategy-builder service's own cache (`vike-strategy-builder`'s `render::build_plugin`) used to
/// be a bare `Path::exists()` on the content-addressed artifact name, which cannot see an artifact
/// built for a DIFFERENT `ABI_VERSION` or toolchain fingerprint — so a cache hit could hand back
/// something [`load`] would immediately refuse, with no way for an operator to force a rebuild but
/// deleting the file by hand. This is the narrow check that cache now runs before serving a hit:
/// one `dlopen` and exactly the two `dlsym`s the handshake needs, cheap enough to run on every
/// cache hit rather than only when something looks wrong.
///
/// Never closes the handle it opens, on purpose — the same posture [`load`] takes (this module's
/// header): unloading a library something may still reference is unsafe, and a verify-only handle
/// is no exception to that rule just because nothing here goes on to call through it.
///
/// ⚠ **What that leaves behind is one mapping per DISTINCT artifact path, not one per call**, and
/// the difference is worth stating because the looser wording invites the wrong sum. `dlopen` on
/// a path already loaded returns the SAME handle with its refcount bumped and maps nothing new,
/// so a daemon serving one strategy's unchanged artifact a thousand times holds exactly one
/// mapping. The count grows with how many distinct `<name>-<sha>.so` files this process has
/// SERVED FROM CACHE — a fresh edit is a cache MISS and never reaches here — each the size of one
/// plugin artifact (this crate's own module doc / the design doc's measured 674 KiB-2.5 MiB).
/// ⚠ A consequence the builder's retention knob cannot undo: `prune` deleting an artifact does
/// not unmap one this process already verified, so RSS does not fall when it runs. Bounded the
/// same way an edit's stale mapping already is, and by the same argument — but bounded by the
/// re-Run count, not by the Build count.
///
/// Returns `Ok(())` on a match; the specific [`LoadError`] otherwise, so a caller can log which
/// guard actually moved. Any error here — including [`LoadError::DlOpen`] on a file that is not
/// even a valid shared object — means "this is not what would be built now", the same verdict a
/// genuine ABI or fingerprint drift produces.
pub fn verify_handshake(path: &Path) -> Result<(), LoadError> {
    open_and_handshake(path)?;
    Ok(())
}

/// Load, ABI-check, and fingerprint-check the plugin at `path`.
///
/// ⚠ **Every dispatch symbol is REQUIRED, and that is deliberate at `ABI_VERSION` 3.** A plugin
/// missing one is refused with [`LoadError::NoSymbol`] rather than loaded with that hook quietly
/// absent — which would reinstate, at load time, exactly the silent divergence the build-time
/// refusal was invented to prevent. Nothing legitimate is caught by this: the cdylib template
/// emits all of them unconditionally, and an artifact built against the two-slot vtable is already
/// refused one check earlier, on its `ABI_VERSION`.
pub fn load(path: &Path) -> Result<LoadedPlugin, LoadError> {
    let handle = open_and_handshake(path)?;

    let vtable = PluginVTable {
        create: bind!(
            handle,
            "vike_plugin_create",
            extern "C" fn(*const u8, usize) -> *mut c_void
        )?,
        destroy: bind!(handle, "vike_plugin_destroy", extern "C" fn(*mut c_void))?,
        warmup: bind!(handle, "vike_plugin_warmup", extern "C" fn(*mut c_void) -> usize)?,
        on_start: bind!(handle, "vike_plugin_on_start", LifecycleFn)?,
        on_bar: bind!(
            handle,
            "vike_plugin_on_bar",
            extern "C" fn(
                *mut c_void,
                crate::abi::BrokerRef,
                *const crate::abi::CBar,
            ) -> crate::abi::PluginStatus
        )?,
        on_quote_tick: bind!(
            handle,
            "vike_plugin_on_quote_tick",
            PayloadFn<crate::abi::CQuoteTick>
        )?,
        on_trade_tick: bind!(
            handle,
            "vike_plugin_on_trade_tick",
            PayloadFn<crate::abi::CTradeTick>
        )?,
        on_order_book: bind!(
            handle,
            "vike_plugin_on_order_book",
            extern "C" fn(
                *mut c_void,
                crate::abi::BrokerRef,
                crate::abi::BookRef,
            ) -> crate::abi::PluginStatus
        )?,
        on_schedule: bind!(handle, "vike_plugin_on_schedule", BorrowedStrFn)?,
        on_fill: bind!(handle, "vike_plugin_on_fill", PayloadFn<crate::abi::CFill>)?,
        on_feed_status: bind!(
            handle,
            "vike_plugin_on_feed_status",
            extern "C" fn(*mut c_void, crate::abi::BrokerRef, u32) -> crate::abi::PluginStatus
        )?,
        on_mark: bind!(handle, "vike_plugin_on_mark", PayloadFn<crate::abi::CMarkTick>)?,
        on_reference_quote: bind!(
            handle,
            "vike_plugin_on_reference_quote",
            extern "C" fn(
                *mut c_void,
                crate::abi::BrokerRef,
                *const u8,
                usize,
                *const crate::abi::CQuoteTick,
            ) -> crate::abi::PluginStatus
        )?,
        on_flow: bind!(handle, "vike_plugin_on_flow", PayloadFn<crate::abi::CFlowToxicity>)?,
        on_order_event: bind!(
            handle,
            "vike_plugin_on_order_event",
            PayloadFn<crate::abi::COrderLifecycle>
        )?,
        on_params_updated: bind!(handle, "vike_plugin_on_params_updated", BorrowedStrFn)?,
        on_stop: bind!(handle, "vike_plugin_on_stop", LifecycleFn)?,
    };

    Ok(LoadedPlugin { handle, path: path.to_path_buf(), vtable })
}

/// Read a NUL-terminated `*const c_char` into an owned `String` — the plugin's handshake
/// fingerprint, and (since the message was added to `LoadError::DlOpen`) `dlerror`'s own text.
///
/// Lossy, deliberately — unlike `CBar::to_bar`'s checked read of a trading SYMBOL (where a
/// contract violation must be a loud, contained failure rather than silently-corrupted data a
/// strategy might then trade on), this string is a DIAGNOSTIC comparison key: a fingerprint with a
/// stray replacement character will simply fail to equal [`fingerprint::FINGERPRINT`] and refuse
/// the load exactly as a genuinely different fingerprint would — there is no downstream consumer
/// for whom "which byte was wrong" matters the way a corrupted order symbol would.
fn read_c_str(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: the plugin ABI contract requires `vike_plugin_fingerprint` to return a pointer to a
    // valid, NUL-terminated, `'static` string (`fingerprint::fingerprint_c_str`'s doc states the
    // contract a real plugin upholds).
    unsafe { std::ffi::CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

#[cfg(unix)]
mod plat {
    use std::ffi::{CString, c_int, c_void};
    use std::os::raw::c_char;

    // RTLD_NOW: resolve every symbol at load rather than on first call, so a missing entry point
    // is a diagnostic here rather than a SIGSEGV inside a later dispatch call. RTLD_LOCAL (the
    // absence of RTLD_GLOBAL) keeps a plugin's own symbols out of the process-wide namespace.
    const RTLD_NOW: c_int = 2;
    const RTLD_LOCAL: c_int = 0;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        /// The loader's own last error message. Declared here rather than reconstructed: a failed
        /// `dlopen` has exactly one actionable cause and only `ld.so` knows it — see [`open`].
        fn dlerror() -> *const c_char;
    }

    pub(super) fn open(path: &str) -> Result<*mut c_void, String> {
        let c = CString::new(path).map_err(|_| "interior NUL in the plugin path".to_string())?;
        // ⚠ **The message used to be `dlopen failed for <path>` and nothing else, which told an
        // operator only what they already knew.** Every real cause of this failure — an
        // unresolved symbol under `RTLD_NOW`, a missing transitive `.so`, a wrong ELF class, a
        // file that is not a shared object at all — is distinguishable ONLY by `dlerror`, and
        // `LoadError::DlOpen` renders whatever this returns verbatim.
        //
        // `dlerror` is read INSIDE this same block, immediately after the call, for two
        // independent reasons: it is per-thread state that the very next libdl call clears (so a
        // later read can come back null and say nothing), and this crate's `unsafe` site count is
        // RATCHETED per file by `crates/vike-ops/tests/unsafe_and_toolchain_gate.rs` — a second
        // `unsafe` block here would be a raise that has to be argued for, and this diagnostic is
        // not worth one when the same block does the job.
        //
        // SAFETY: `c` is a valid, NUL-terminated string for the duration of the `dlopen` call.
        // `dlerror` takes no arguments and returns either null or a pointer to a valid,
        // NUL-terminated message owned by the loader, which `read_c_str` (null-safe) handles.
        let (h, detail) = unsafe {
            let h = dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL);
            let detail = if h.is_null() { super::read_c_str(dlerror()) } else { String::new() };
            (h, detail)
        };
        if h.is_null() {
            // An empty `detail` means the loader offered no message — say so rather than printing
            // a bare trailing colon, which reads like a truncated line.
            let detail = if detail.is_empty() {
                "dlerror() reported no message".to_string()
            } else {
                detail
            };
            Err(format!("dlopen failed for `{path}`: {detail}"))
        } else {
            Ok(h)
        }
    }

    pub(super) fn sym(handle: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|_| "interior NUL in the symbol name".to_string())?;
        // SAFETY: `handle` came from `open` above and is never closed — this module's header.
        let p = unsafe { dlsym(handle, c.as_ptr()) };
        if p.is_null() { Err(format!("symbol `{name}` not found")) } else { Ok(p) }
    }
}

#[cfg(not(unix))]
mod plat {
    use std::ffi::c_void;

    pub(super) fn open(_path: &str) -> Result<*mut c_void, String> {
        Err("plugin loading is only supported on Linux (the backtest server and the builder \
             service both run on self-hosted Linux boxes; see the design doc's sandbox \
             section) — this platform cannot open a plugin"
            .to_string())
    }

    pub(super) fn sym(_handle: *mut c_void, _name: &str) -> Result<*mut c_void, String> {
        Err("plugin loading is only supported on Linux".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_path_is_refused_before_any_dlopen_is_attempted() {
        let err = load(Path::new("/does/not/exist/anywhere.so")).unwrap_err();
        assert!(matches!(err, LoadError::Missing(_)));
        assert!(err.to_string().to_lowercase().contains("build"));
    }

    /// [`verify_handshake`] shares `load`'s own `Missing` check — a cache probe over a path that
    /// does not exist is not a `DlOpen` failure, it is the same `Missing` a real load would report.
    /// The real dlopen coverage (a genuinely stale artifact rebuilt by
    /// `vike-strategy-builder`'s `render::build_plugin`) lives in that crate's own
    /// `tests/build_errors.rs`, for the same reason `load`'s does not live here.
    #[test]
    fn verify_handshake_reports_missing_the_same_way_load_does() {
        let err = verify_handshake(Path::new("/does/not/exist/anywhere.so")).unwrap_err();
        assert!(matches!(err, LoadError::Missing(_)));
    }

    /// The real dlopen coverage (a plugin whose `on_bar` reaches `PluginStatus`/`BrokerRef`/
    /// `CBar`) lives in `tests/load_refusals.rs`, which needs a compiled fixture this crate's own
    /// `build.rs` produces — no unit test in THIS module dlopens a real `.so`.
    #[test]
    fn load_error_display_names_both_sides_of_a_mismatch() {
        let e = LoadError::AbiMismatch { host: 1, plugin: 2 };
        let msg = e.to_string();
        assert!(msg.contains('1') && msg.contains('2'), "must name both versions: {msg}");

        let e = LoadError::FingerprintMismatch { host: "H".to_string(), plugin: "P".to_string() };
        let msg = e.to_string();
        assert!(msg.contains('H') && msg.contains('P'), "must name both fingerprints: {msg}");
    }
}
