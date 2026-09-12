//! Runtime resolution of the ForexConnect shim — the reason this crate links nothing proprietary.
//!
//! # What changed, and what it bought
//!
//! Until 2026-09-09 `build.rs` emitted `cargo:rustc-link-lib=ForexConnect` and the C++ shim was
//! compiled into a static archive linked straight into the Rust binary. That made the SDK a
//! **build-time** fact with a **load-time** consequence: the executable carried a hard `DT_NEEDED
//! libForexConnect.so`, so on any box without the library set staged it did not reach `main` at
//! all — exit 127, before a single line of ours ran. A universal binary was therefore impossible,
//! and the tree carried a SECOND daemon asset (`vike-tradehub-fxcm`) for the boxes that trade FX.
//!
//! Now the C++ shim is its own shared object, `libfcshim.so`, which links the SDK the ordinary way.
//! Nothing in the Rust binary references a ForexConnect symbol; this module opens the shim at
//! runtime and resolves the ten `extern "C"` entry points by name. Absent shim ⇒ [`Shim::get`]
//! answers `None` ⇒ every call in [`crate::sys`] returns [`crate::sys::FxcmError::Unavailable`],
//! which is the same answer the old compile-time stub gave and lands on the same refusal in
//! `vike_mount::make_engine`.
//!
//! # ⚠ Why a shim `.so` rather than dlopen-ing the SDK directly
//!
//! The obvious design — `dlopen("libForexConnect.so")` and `dlsym` the one entry point
//! `CO2GTransport::createSession` — was planned and is WRONG, measured rather than reasoned.
//! Compiling `src/shim/fcshim.cpp` against the real SDK on the box that stages it and reading the
//! object's undefined symbols (`nm -u -C`) gives **five** SDK symbols, not one:
//!
//! ```text
//! CO2GTransport::createSession()
//! IAddRef::~IAddRef()
//! IO2GResponseListener::IO2GResponseListener()
//! IO2GSessionStatus::IO2GSessionStatus()
//! typeinfo for IAddRef
//! ```
//!
//! The shim DERIVES from two SDK interfaces to receive callbacks, and those bases have out-of-line
//! constructors and an out-of-line virtual destructor — a key function — so their vtable and
//! typeinfo live in the library rather than being emitted COMDAT-weak in our translation unit.
//! Resolving only `createSession` would leave four undefined symbols; defining them ourselves would
//! put a SECOND `typeinfo for IAddRef` in the process beside the SDK's, which is a One Definition
//! Rule violation that stays quiet exactly until something compares type identity across the
//! boundary. Putting the C++ boundary inside a shared object removes the question: `libfcshim.so`
//! links `-lForexConnect` normally, all five resolve at ITS link time, and what crosses into Rust
//! is a plain C ABI with no mangling, no vtables and no typeinfo.
//!
//! # ⚠ The library search, and why it reads no environment variable
//!
//! `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` is a RATCHET — the set of library
//! crates reading the process environment may shrink and never grow — so this module resolves paths
//! from `std::env::current_exe()` and from the loader's own search, and consults no variable of its
//! own. Three rungs, in order, and the first that opens wins:
//!
//! 1. `<exe_dir>/../lib/<name>` — the INSTALLED PROJECT shape. `<root>/bin/vike` finds
//!    `<root>/lib/libfcshim.so`, which is the directory
//!    `crates/bridges/fxcm/scripts/package-fcsdk-runtime.sh` already fills with the SDK's own
//!    libraries.
//! 2. `<exe_dir>/<name>` — beside the executable. This is the DEV rung: `build.rs` copies the built
//!    shim into the cargo profile directory and its `deps/` sibling, which are `$ORIGIN` for a
//!    binary and for a test binary respectively.
//! 3. the bare name — hand it to the dynamic loader, which searches `LD_LIBRARY_PATH`, the cache
//!    and the default directories. This is the CONTAINER rung and the operator's override:
//!    `crates/bridges/fxcm/scripts/provision-fcsdk.sh` already prints an `LD_LIBRARY_PATH` for the
//!    SDK, and adding the shim's directory to it is how a box points this at a copy of its own.
//!
//! ⚠ **`libfcshim.so` carries `DT_RPATH=$ORIGIN` (old dtags, deliberately), so its own siblings
//! come from wherever it sits.** That is what makes rung 1 work with no help: dropping the shim
//! into the same `<root>/lib` as `libForexConnect.so` resolves the whole ~20-library graph, because
//! `DT_RPATH` — unlike `DT_RUNPATH` — is inherited down the dependency chain. `build.rs` argues
//! that flag where it emits it; do not "modernise" it.
//!
//! # ⚠ What a successful load does NOT prove
//!
//! That the shim opened. It does not prove the credentials work, that FXCM is reachable, or that
//! the SDK's own libraries are the version this shim was built against. [`crate::sys::FxcmSession::login`]
//! is the first thing that talks to the venue, and its failure is a different report on purpose.

use std::ffi::{c_char, c_int, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

/// The shim's file name, per platform. macOS is absent because the SDK is: FXCM ships Windows and
/// Linux x86_64 only, so a mac build has nothing to find and rung 3 fails like the others.
#[cfg(windows)]
const SHIM_FILE: &str = "fcshim.dll";
#[cfg(not(windows))]
const SHIM_FILE: &str = "libfcshim.so";

// ── the platform's two-call dynamic-loader ABI ──────────────────────────────────────────────────
//
// Declared here rather than pulled in as a crate. `libc` already carries the POSIX pair and is a
// workspace dependency, but the Windows pair is not in it, so one of the two platforms would need
// its own declaration anyway — and a crate that exists to wrap these two calls is a dependency in
// the audit surface `deny.toml` covers, bought for about twenty lines. The root `Cargo.toml`'s
// rationale rule is what this paragraph is answering.
#[cfg(unix)]
mod plat {
    use std::ffi::{CString, c_char, c_int, c_void};

    // RTLD_NOW: resolve every symbol at load rather than on first call. A lazy binding would turn a
    // missing entry point into a SIGSEGV at some later moment inside an order path, which is the
    // one place a diagnostic is worth most. RTLD_LOCAL (the absence of RTLD_GLOBAL) keeps the SDK's
    // symbols out of the global namespace, so nothing else in the process can bind to them by
    // accident — this crate reaches them only through the ten C entry points below.
    const RTLD_NOW: c_int = 2;
    const RTLD_LOCAL: c_int = 0;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *mut c_char;
    }

    /// Open a shared object, or `Err(diagnostic)`.
    pub(super) fn open(path: &str) -> Result<*mut c_void, String> {
        let c = CString::new(path).map_err(|_| "interior NUL in the shim path".to_string())?;
        // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_LOCAL) };
        if h.is_null() { Err(last_error()) } else { Ok(h) }
    }

    /// Resolve one symbol, or `Err(diagnostic)`.
    pub(super) fn sym(handle: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|_| "interior NUL in the symbol name".to_string())?;
        // SAFETY: `handle` came from `open` above and is never closed — see `Shim::get`.
        let p = unsafe { dlsym(handle, c.as_ptr()) };
        if p.is_null() { Err(last_error()) } else { Ok(p) }
    }

    fn last_error() -> String {
        // SAFETY: `dlerror` returns either NULL or a pointer to a static, NUL-terminated buffer.
        let e = unsafe { dlerror() };
        if e.is_null() {
            "the dynamic loader reported no error".into()
        } else {
            unsafe { std::ffi::CStr::from_ptr(e) }.to_string_lossy().into_owned()
        }
    }
}

#[cfg(windows)]
mod plat {
    use std::ffi::{CString, c_char, c_void};

    unsafe extern "system" {
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
        fn GetLastError() -> u32;
    }

    pub(super) fn open(path: &str) -> Result<*mut c_void, String> {
        let c = CString::new(path).map_err(|_| "interior NUL in the shim path".to_string())?;
        // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
        let h = unsafe { LoadLibraryA(c.as_ptr()) };
        if h.is_null() { Err(last_error()) } else { Ok(h) }
    }

    pub(super) fn sym(handle: *mut c_void, name: &str) -> Result<*mut c_void, String> {
        let c = CString::new(name).map_err(|_| "interior NUL in the symbol name".to_string())?;
        // SAFETY: `handle` came from `open` above and is never freed — see `Shim::get`.
        let p = unsafe { GetProcAddress(handle, c.as_ptr()) };
        if p.is_null() { Err(last_error()) } else { Ok(p) }
    }

    fn last_error() -> String {
        // SAFETY: no pointers; `GetLastError` reads thread-local state.
        format!("windows error {}", unsafe { GetLastError() })
    }
}

// ── the C ABI, as function-pointer types ────────────────────────────────────────────────────────
//
// One `type` per entry point in `src/shim/fcshim.cpp`'s `extern "C"` block. These signatures used
// to be an `unsafe extern "C"` declaration block; the safety property is identical and so is the
// hazard — nothing checks them against the real symbol, so a change to the C++ side must be made
// here in the same commit. What is new is that a MISSING symbol is now caught: `Shim::load`
// resolves all ten before returning, so a stale `libfcshim.so` is a diagnostic at open rather than
// a crash on the call that needed the one it lacks.
pub(crate) type FcLogin =
    unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> *mut c_void;
pub(crate) type FcAccount =
    unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut f64) -> c_int;
pub(crate) type FcBaseUnitSize =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int, *mut c_char, c_int) -> c_int;
#[allow(clippy::type_complexity)]
pub(crate) type FcPlaceEntry = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut f64,
    *mut c_char,
    c_int,
) -> c_int;
#[allow(clippy::type_complexity)]
pub(crate) type FcPlaceMarket = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
) -> c_int;
pub(crate) type FcDeleteOrder = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_char,
    c_int,
) -> c_int;
pub(crate) type FcTable = unsafe extern "C" fn(*mut c_void, *mut c_char, c_int) -> c_int;
pub(crate) type FcLogout = unsafe extern "C" fn(*mut c_void);

/// The resolved shim: one handle, ten entry points.
///
/// Held for the life of the process in a [`OnceLock`] and never closed. That is deliberate — a
/// `dlclose` would unmap code a live `FxcmSession` is about to call, and the SDK spawns its own
/// threads, so unloading it is not a thing this crate can do safely. The handle field exists to
/// document the ownership rather than to be used.
pub(crate) struct Shim {
    #[allow(dead_code)]
    handle: *mut c_void,
    /// Where it was found — for the diagnostic, and for an operator asking "which shim is this".
    pub(crate) path: String,
    pub(crate) fc_login: FcLogin,
    pub(crate) fc_account: FcAccount,
    pub(crate) fc_base_unit_size: FcBaseUnitSize,
    pub(crate) fc_place_entry: FcPlaceEntry,
    pub(crate) fc_place_market: FcPlaceMarket,
    pub(crate) fc_delete_order: FcDeleteOrder,
    pub(crate) fc_orders: FcTable,
    pub(crate) fc_trades: FcTable,
    pub(crate) fc_poll_event: FcTable,
    pub(crate) fc_logout: FcLogout,
}

// SAFETY: every field is either a raw pointer into a mapping that is never unmapped, or a function
// pointer into it. The SHIM is shareable; a SESSION is not, and `FxcmSession` stays neither `Send`
// nor `Sync` for exactly that reason (its `h` is a single-owner native handle).
unsafe impl Send for Shim {}
unsafe impl Sync for Shim {}

/// `None` until the first [`Shim::get`], then the verdict for the life of the process.
static SHIM: OnceLock<Result<Shim, String>> = OnceLock::new();

impl Shim {
    /// The loaded shim, or `None` if it could not be opened.
    ///
    /// Resolution happens once. A failure is remembered with its diagnostic rather than retried:
    /// the answer cannot change without the process restarting (the ladder reads
    /// `current_exe()` and the loader's own search, both fixed for a process), and retrying a
    /// `dlopen` per order would put a filesystem walk on the exec path.
    pub(crate) fn get() -> Option<&'static Shim> {
        SHIM.get_or_init(Self::load).as_ref().ok()
    }

    /// The diagnostic from the last (and only) load attempt, or `None` if it succeeded.
    ///
    /// Used by [`crate::sdk_available`]'s caller-facing report and by the live smokes, so a box
    /// that expected FXCM to work is told WHICH rungs were tried rather than "unavailable".
    pub(crate) fn failure() -> Option<&'static str> {
        SHIM.get_or_init(Self::load).as_ref().err().map(String::as_str)
    }

    fn load() -> Result<Shim, String> {
        let mut tried: Vec<String> = Vec::new();
        for cand in candidates() {
            match plat::open(&cand) {
                // ⚠ An OPEN that succeeds and a BIND that then fails is a DIFFERENT failure from
                // "not found", and it stops the ladder rather than falling through to the next
                // rung: a file of that name exists and is not the shim this build expects, which is
                // a fact an operator has to fix rather than a rung to skip. Falling through would
                // hide a stale `libfcshim.so` behind whatever the loader found next.
                Ok(h) => {
                    return Self::bind(h, cand.clone()).map_err(|e| {
                        format!(
                            "`{cand}` opened but is not this build's shim: {e}. \
                             FXCM stays PAPER. Rebuild or reinstall it — \
                             `docs/ops/fxcm-forexconnect.md` is the operator page."
                        )
                    });
                }
                Err(e) => tried.push(format!("  {cand}: {e}")),
            }
        }
        Err(format!(
            "the FXCM shim `{SHIM_FILE}` could not be opened. FXCM stays PAPER.\n{}\n\
             Install it beside the ForexConnect libraries in `<project>/lib/`, or put its \
             directory on the loader's search path. `docs/ops/fxcm-forexconnect.md` is the \
             operator page.",
            tried.join("\n")
        ))
    }

    /// Resolve all ten entry points, or fail naming the first one that is missing.
    fn bind(handle: *mut c_void, path: String) -> Result<Shim, String> {
        // SAFETY (all ten): each cast turns a `dlsym`/`GetProcAddress` result into the signature
        // declared for that name above. Nothing checks the cast — that is the unsafe act, and it is
        // why the C++ side and the type aliases above must change together. A name that is ABSENT
        // is caught by `sym` before the cast, so the failure mode this cannot see is a symbol
        // present with a DIFFERENT signature, i.e. a `libfcshim.so` built from other sources.
        macro_rules! bind {
            ($name:literal, $ty:ty) => {{
                let p = plat::sym(handle, $name).map_err(|e| format!("`{}` {e}", $name))?;
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        Ok(Shim {
            fc_login: bind!("fc_login", FcLogin),
            fc_account: bind!("fc_account", FcAccount),
            fc_base_unit_size: bind!("fc_base_unit_size", FcBaseUnitSize),
            fc_place_entry: bind!("fc_place_entry", FcPlaceEntry),
            fc_place_market: bind!("fc_place_market", FcPlaceMarket),
            fc_delete_order: bind!("fc_delete_order", FcDeleteOrder),
            fc_orders: bind!("fc_orders", FcTable),
            fc_trades: bind!("fc_trades", FcTable),
            fc_poll_event: bind!("fc_poll_event", FcTable),
            fc_logout: bind!("fc_logout", FcLogout),
            handle,
            path,
        })
    }
}

/// The search ladder, in order. See this module's header for what each rung is for.
///
/// Kept a free function so [`candidates_are_ordered_and_relative`] can read it without a process
/// whose `current_exe()` says anything in particular.
fn candidates() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let mut lib: PathBuf = dir.to_path_buf();
        lib.pop();
        lib.push("lib");
        lib.push(SHIM_FILE);
        out.push(lib.to_string_lossy().into_owned());
        out.push(dir.join(SHIM_FILE).to_string_lossy().into_owned());
    }
    // ⚠ Always LAST and always present, even when `current_exe()` failed: this is the rung the
    // loader's own search answers, so it is the one that cannot depend on anything about us.
    out.push(SHIM_FILE.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder ends at the bare name and every earlier rung is derived from the executable.
    ///
    /// This is the property that keeps the loader out of `LIBRARY_PIN`: no rung reads an
    /// environment variable, so nothing here is a `Layer::Library` row for the settings registry to
    /// refuse. A future rung that consulted one would fail CI at that gate rather than here — this
    /// case exists so the intent is written down beside the code that has it.
    #[test]
    fn candidates_are_ordered_and_relative() {
        let c = candidates();
        assert_eq!(c.last().map(String::as_str), Some(SHIM_FILE), "the bare name must be last");
        assert!(c.len() >= 2 || std::env::current_exe().is_err());
        // Rung 1 is the sibling `lib/` directory, rung 2 is beside the executable.
        if c.len() == 3 {
            assert!(c[0].ends_with(SHIM_FILE) && c[0].contains("lib"), "rung 1: {}", c[0]);
            assert!(c[1].ends_with(SHIM_FILE), "rung 2: {}", c[1]);
        }
    }

    /// A load that fails says WHICH rungs were tried, not merely that it failed.
    ///
    /// On every CI box there is no shim to find, so this is the real message an operator reads —
    /// which is why it is asserted rather than assumed. On a box that HAS one the load succeeds and
    /// there is no message; the test says so rather than pretending to check both.
    #[test]
    fn a_failure_names_every_rung_it_tried() {
        match Shim::failure() {
            None => assert!(Shim::get().is_some(), "no failure recorded, so it must have loaded"),
            Some(msg) => {
                assert!(msg.contains(SHIM_FILE), "the message must name the file: {msg}");
                assert!(msg.contains("PAPER"), "the message must say what it costs: {msg}");
                assert!(
                    msg.contains("fxcm-forexconnect.md"),
                    "the message must name the operator page: {msg}"
                );
            }
        }
    }
}
