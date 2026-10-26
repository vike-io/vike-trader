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
//! runtime and resolves the REQUIRED `extern "C"` entry points by name — plus an OPTIONAL pair a
//! shim built before 2026-09-22 does not carry, whose absence costs a diagnostic and never a
//! start. Absent shim ⇒ [`Shim::get`]
//! answers `None` ⇒ every call in [`crate::sys`] returns [`crate::sys::FxcmError::Unavailable`],
//! which is the same answer the old compile-time stub gave and lands on the same refusal in
//! `FxcmVenueAdapter`'s start (`crates/bridges/fxcm/src/adapter.rs`).
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
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` is a RATCHET — the set of library
//! crates reading the process environment may shrink and never grow — so this module resolves paths
//! from `std::env::current_exe()` and from the loader's own search, and consults no variable of its
//! own. Three rungs, in order, and the first that opens wins:
//!
//! 1. `<exe_dir>/../lib/<name>` — the INSTALLED PROJECT shape. `<root>/bin/vike-backend`
//!    finds `<root>/lib/libfcshim.so`, which is the directory
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
    // accident — this crate reaches them only through the C entry points below.
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
            // SAFETY: `e` is non-null on this branch, and `dlerror` hands back a NUL-terminated
            // buffer that stays valid until the next `dl*` call on this thread; it is copied out
            // (`into_owned`) before any such call.
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
// resolves every REQUIRED one before returning, so a stale `libfcshim.so` is a diagnostic at open
// rather than
// a crash on the call that needed the one it lacks.
pub(crate) type FcLogin =
    unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> *mut c_void;
pub(crate) type FcAccount =
    unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut f64) -> c_int;
pub(crate) type FcBaseUnitSize =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_int, *mut c_char, c_int) -> c_int;
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

// ── the OPTIONAL half: names a shim built before 2026-09-22 does not carry ──────────────────────
//
// ⚠ These two (and the ABI 2 pair after them: `fc_place_entry_at`, `fc_edit_order`, built after 2026-10-16) may
// NEVER join the `bind!` roster above, and the reason is the whole ABI argument.
// `bind!` `?`s out on a missing symbol, so requiring either would make [`Shim::load`] FAIL against
// the `libfcshim.so` the CI box has installed today — [`Shim::get`] would answer `None`, every call in
// [`crate::sys`] would answer `Unavailable`, and a LIVE venue would drop to paper for the sake of a
// diagnostic. Absence is the ORDINARY answer here until an operator reinstalls the shim, so it is
// `None` and the session says so by name.
/// The shim's own ABI version (`fc_abi_version`). Absent ⇒ version 0.
pub(crate) type FcAbiVersion = unsafe extern "C" fn() -> c_int;
/// `fc_login_ex` — login, and on failure fill `(err_out, err_len)` and `*class_out`.
pub(crate) type FcLoginEx = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_char,
    c_int,
    *mut c_int,
) -> *mut c_void;
/// `fc_place_entry_at` (ABI 2) — a resting LIMIT entry at the CALLER's rate. `fc_place_entry` takes a
/// pip distance, which is why a requested limit price could not be honored before this existed.
pub(crate) type FcPlaceEntryAt = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    f64,
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
/// `fc_edit_order` (ABI 2) — amend a resting entry's rate in place (`EditOrder`).
pub(crate) type FcEditOrder = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    f64,
    *mut c_char,
    c_int,
) -> c_int;
/// `fc_order_rate` — the Rate (and the price tick) the Orders table holds for one order, read after a
/// placement or an amend completed. Added after ABI 2 WITHOUT a version bump: bound by name alone.
pub(crate) type FcOrderRate = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *mut f64,
    *mut f64,
    *mut c_char,
    c_int,
) -> c_int;
/// `fc_find_order` — look an order up by the RequestID of a placement whose reply was lost, checked
/// against what that placement sent (offer, side, amount, limit flag, rate). Added after ABI 2 WITHOUT
/// a version bump: bound by name alone, like [`FcOrderRate`].
pub(crate) type FcFindOrder = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    *const c_char,
    c_int,
    c_int,
    f64,
    *mut c_char,
    c_int,
    *mut c_char,
    c_int,
) -> c_int;

/// `fc_set_subscription` — make an instrument's Offers row read a subscription status (the bridge only ever asks for
/// `T`), inside the given total budget in ms; the buffer takes the venue's words. Added after ABI 2 WITHOUT a version
/// bump: bound by name alone, like [`FcOrderRate`].
pub(crate) type FcSetSubscription = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    c_int,
    *mut c_char,
    c_int,
) -> c_int;

/// `fc_close_trade` — close `amount` base units of ONE open trade by its trade id (`TrueMarketClose` + `TradeID`), the
/// close a hedging account needs: an opposite market OPEN there opens a second trade. The buffers are the closing
/// order's id, the offer id, the account id and the error text, each with its length. Added after ABI 2 WITHOUT a
/// version bump: bound by name alone, like [`FcOrderRate`].
pub(crate) type FcCloseTrade = unsafe extern "C" fn(
    *mut c_void,
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

/// The resolved shim: one handle, the required entry points, and whatever optional ones it carries.
///
/// Held for the life of the process in a [`OnceLock`] and never closed. That is deliberate — a
/// `dlclose` would unmap code a live `FxcmSession` is about to call, and the SDK spawns its own
/// threads, so unloading it is not a thing this crate can do safely. The handle field exists to
/// document the ownership rather than to be used.
pub(crate) struct Shim {
    // allow, not expect: the cfg(windows) build of this field is compiled by no Linux lane.
    #[allow(dead_code, clippy::allow_attributes)]
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
    /// What this shim says its own ABI is — `0` for one built before `fc_abi_version` existed.
    ///
    /// `dlsym` proves a NAME exists and can never see that an existing name's MEANING moved; this
    /// integer is the thing that can, and it is why a future contract change has somewhere to go
    /// that is not a second symbol.
    pub(crate) abi_version: c_int,
    /// `Some` only when [`Self::abi_version`] is at least 1 AND the symbol resolved — BOTH, so a
    /// half-built shim degrades to the four-argument [`Self::fc_login`] instead of being guessed at.
    pub(crate) fc_login_ex: Option<FcLoginEx>,
    /// `Some` only when [`Self::abi_version`] is at least 2 AND the symbol resolved. `None` means a
    /// PRICED limit cannot be placed on this box: [`crate::sys::FxcmSession::place_limit_at`] says
    /// so by name and the order is refused, never rested at a rate nobody chose.
    pub(crate) fc_place_entry_at: Option<FcPlaceEntryAt>,
    /// `Some` under the same condition as [`Self::fc_place_entry_at`]. `None` means an amend is
    /// refused (non-terminally: the resting order keeps its terms).
    pub(crate) fc_edit_order: Option<FcEditOrder>,
    /// `Some` when the symbol resolved, whatever the ABI number says (an ABI 2 shim built before it
    /// existed lacks it). `None` only costs the venue-rate echo: [`crate::sys::FxcmSession::order_rate`]
    /// answers `Ok(None)`, nothing is refused.
    pub(crate) fc_order_rate: Option<FcOrderRate>,
    /// `Some` when the symbol resolved. `None` only costs the active `confirm` of a placement whose
    /// reply was lost: it then answers nothing and the order waits for reconcile and the watchdog.
    pub(crate) fc_find_order: Option<FcFindOrder>,
    /// `Some` when the symbol resolved. The login's SYSTEM PROPERTIES as one JSON object, where the
    /// account's currency lives (`BASE_CRNCY`, decision 0183): the Accounts table has no currency column.
    /// An [`FcTable`] by shape (a buffer and its length, the full length back, a negative code with its
    /// reason in the buffer). `None` costs only the account's currency: it is then ASSUMED, loudly
    /// ([`crate::home_conversion::account_ccy`]), and the message names the remedy.
    pub(crate) fc_system_properties: Option<FcTable>,
    /// `Some` when the symbol resolved. The CURRENT Offers table as a JSON array (a refresh-table request),
    /// the one source of a conversion rate and of a symbol's grid. `None` costs the conversion: the rate is
    /// then unavailable and a symbol quoted in another currency stays refused.
    pub(crate) fc_offers: Option<FcTable>,
    /// `Some` when the symbol resolved. Subscribes an instrument for trading (`SetSubscriptionStatus`). `None` costs
    /// the on-demand subscription only: a symbol FXCM holds as view-only or disabled stays refused, in words naming
    /// the remedy, and everything already tradable works as before.
    pub(crate) fc_set_subscription: Option<FcSetSubscription>,
    /// `Some` when the symbol resolved. The account's traits (`maintenance_type`: "0" is a netting account) as one JSON
    /// object, an [`FcTable`] by shape. `None` leaves the account's mode UNKNOWN ([`crate::close::AccountMode`]).
    pub(crate) fc_account_traits: Option<FcTable>,
    /// `Some` when the symbol resolved. The account's open trades with their open times, a fresh bounded read for the
    /// close planner. With [`Self::fc_close_trade`] it is what [`crate::sys::FxcmSession::close_gap`] asks for.
    pub(crate) fc_open_trades: Option<FcTable>,
    /// `Some` when the symbol resolved. Closes one trade by its id. `None` costs every close on a hedging account:
    /// a reduce-only market order is then refused BY NAME, never sent as the opening order that would lock a pair.
    pub(crate) fc_close_trade: Option<FcCloseTrade>,
}

// SAFETY: every field is either a raw pointer into a mapping that is never unmapped, or a function
// pointer into it. The SHIM is shareable; a SESSION is not, and `FxcmSession` stays neither `Send`
// nor `Sync` for exactly that reason (its `h` is a single-owner native handle).
unsafe impl Send for Shim {}
// SAFETY: the same argument as `Send` above: nothing in a `Shim` is mutated after the load, so
// shared references on several threads read only fixed pointers and the fixed `path`.
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

    /// Resolve every REQUIRED entry point, or fail naming the first one that is missing. The optional
    /// pair below never fails a load.
    fn bind(handle: *mut c_void, path: String) -> Result<Shim, String> {
        // SAFETY (every bind below): each cast turns a `dlsym`/`GetProcAddress` result into the signature
        // declared for that name above. Nothing checks the cast — that is the unsafe act, and it is
        // why the C++ side and the type aliases above must change together. A name that is ABSENT
        // is caught by `sym` before the cast, so the failure mode this cannot see is a symbol
        // present with a DIFFERENT signature, i.e. a `libfcshim.so` built from other sources.
        macro_rules! bind {
            ($name:literal, $ty:ty) => {{
                let p = plat::sym(handle, $name).map_err(|e| format!("`{}` {e}", $name))?;
                // SAFETY: `p` is the non-null address `plat::sym` resolved for `$name`, and `$ty`
                // is that symbol's C signature: the type aliases above mirror the shim's C++
                // declarations (the comment above says what this cannot see).
                unsafe { std::mem::transmute::<*mut c_void, $ty>(p) }
            }};
        }
        // ...and the OPTIONAL binder. The ONLY difference that matters is the `?`: this one answers
        // `None` where `bind!` fails the whole load. A name bound here is one a shim installed
        // before it existed legitimately lacks, so its absence must never cost a live start.
        macro_rules! bind_opt {
            ($name:literal, $ty:ty) => {
                plat::sym(handle, $name)
                    .ok()
                    // SAFETY: as in `bind!`: a resolved, non-null symbol and its mirrored signature.
                    .map(|p| unsafe { std::mem::transmute::<*mut c_void, $ty>(p) })
            };
        }
        // SAFETY: `fc_abi_version` takes nothing, returns an `int`, and touches no session state —
        // it is safe to call the moment it resolves, which is why the version can gate the rest.
        let abi_version = bind_opt!("fc_abi_version", FcAbiVersion).map_or(0, |f| unsafe { f() });
        let fc_login_ex = if abi_version >= 1 { bind_opt!("fc_login_ex", FcLoginEx) } else { None };
        // ABI 2: gated on the VERSION as well as on the name, so a half-built shim degrades rather
        // than being guessed at (the same rule `fc_login_ex` follows).
        let fc_place_entry_at =
            if abi_version >= 2 { bind_opt!("fc_place_entry_at", FcPlaceEntryAt) } else { None };
        let fc_edit_order =
            if abi_version >= 2 { bind_opt!("fc_edit_order", FcEditOrder) } else { None };
        // The rate echo is bound by NAME alone: no existing entry point changed meaning, so no ABI
        // bump, and an ABI 2 shim without it simply has no echo.
        let fc_order_rate = bind_opt!("fc_order_rate", FcOrderRate);
        // The same arrangement for the lost-reply lookup behind `confirm`.
        let fc_find_order = bind_opt!("fc_find_order", FcFindOrder);
        // The two reads decision 0183 needs, by NAME alone like the two above (no existing entry point
        // changed meaning): both have the shape of the reconcile table reads, so they reuse `FcTable`.
        let fc_system_properties = bind_opt!("fc_system_properties", FcTable);
        let fc_offers = bind_opt!("fc_offers", FcTable);
        // The instrument subscription, by NAME alone for the same reason.
        let fc_set_subscription = bind_opt!("fc_set_subscription", FcSetSubscription);
        // Closing a trade by its id, by NAME alone for the same reason: a shim installed before these three degrades to
        // a named refusal of a reduce-only market order on a hedging account (`crate::close::route`).
        let fc_account_traits = bind_opt!("fc_account_traits", FcTable);
        let fc_open_trades = bind_opt!("fc_open_trades", FcTable);
        let fc_close_trade = bind_opt!("fc_close_trade", FcCloseTrade);
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
            abi_version,
            fc_login_ex,
            fc_place_entry_at,
            fc_edit_order,
            fc_order_rate,
            fc_find_order,
            fc_system_properties,
            fc_offers,
            fc_set_subscription,
            fc_account_traits,
            fc_open_trades,
            fc_close_trade,
            handle,
            path,
        })
    }
}

/// How to NAME a resolved shim in an operator-facing message.
///
/// ⚠ **[`Shim::path`] is the ladder RUNG that answered, and the last rung is the bare file name.**
/// So on a box whose shim came from `LD_LIBRARY_PATH`, the loader cache or a default directory,
/// `path` is literally `libfcshim.so` — and a message reading "the shim installed at
/// `libfcshim.so`" names no file at all. That is the one thing such a message exists to do: say
/// WHICH file to replace. A bare name is therefore rendered as what it is — a name the dynamic
/// loader resolved — together with the way to find out which file that was.
///
/// A free function rather than a method so [`a_bare_name_is_described_as_one`] can exercise it
/// without a real shim: constructing a [`Shim`] needs ten resolved function pointers, which is
/// exactly the thing no CI box has.
pub(crate) fn describe_shim_path(path: &str) -> String {
    if path == SHIM_FILE {
        format!(
            "the one the dynamic loader resolved for the bare name `{SHIM_FILE}` (rung 3 of the \
             search: `LD_LIBRARY_PATH`, the loader cache, or a default directory — `ldd` on this \
             binary names the file)"
        )
    } else {
        format!("`{path}`")
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

    /// A message naming the shim must name a FILE, and rung 3 of the ladder does not have one.
    ///
    /// ⚠ The defect this pins shipped: `FxcmSession::login`'s old-shim refusal read "the shim
    /// installed at `libfcshim.so`" whenever the dynamic loader had resolved the bare name, which
    /// is the container rung and the operator-override rung — precisely the boxes where "which
    /// file" is the hard question. See [`describe_shim_path`].
    #[test]
    fn a_bare_name_is_described_as_one() {
        let bare = describe_shim_path(SHIM_FILE);
        assert!(
            !bare.contains("installed at") && bare.contains("dynamic loader"),
            "a bare name must be described as a NAME the loader resolved, not as a path: {bare}"
        );
        assert!(bare.contains("ldd"), "…and must say how to find the real file: {bare}");

        // A rung-1 / rung-2 answer IS a path, and is quoted as one with nothing added.
        let installed = format!("/srv/vike/lib/{SHIM_FILE}");
        assert_eq!(describe_shim_path(&installed), format!("`{installed}`"));
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
