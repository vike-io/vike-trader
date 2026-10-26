//! FXCM ForexConnect FFI — the Rust side of the C ABI shim (`shim/fcshim.cpp`).
//!
//! Pure Rust over a C ABI shim — NO Python anywhere. Rescued from the proven browserless
//! login+place+cancel POC (ran a live demo order on acct D251112911, 2026-07-03). This module is the
//! raw FFI + a thin safe wrapper; the vike-model mapping (→ `ExecutionClient`) lives in the sibling
//! `exec` module.
//!
//! ⚠ **THE SDK IS A RUNTIME FACT NOW, NOT A COMPILE-TIME ONE, AND THIS FILE IS WHERE THAT SHOWS.**
//! Until 2026-09-09 every method here existed twice — a `#[cfg(fcsdk)]` arm calling a linked symbol
//! and a `#[cfg(not(fcsdk))]` stub returning [`FxcmError::Unavailable`] — because `build.rs` decided
//! at COMPILE time whether the ForexConnect SDK existed and emitted
//! `cargo:rustc-link-lib=ForexConnect` when it did. The consequence was a hard `DT_NEEDED` on the
//! binary: it did not reach `main` on a box without the libraries, so FXCM could never be part of a
//! universal build and the tree carried a second daemon asset for it.
//!
//! The C++ boundary is now its own shared object, opened at runtime by [`crate::loader`]. Every
//! method below compiles on every box, and the "no SDK" answer comes from
//! `Shim::get()` returning `None` — the SAME [`FxcmError::Unavailable`] the stub used to return, so
//! the start's refusal (`FxcmVenueAdapter` in `crates/bridges/fxcm/src/adapter.rs`) and every caller's
//! handling are unchanged. What changed is
//! that CI now COMPILES this code rather than a stub of it: the FFI signatures are type-checked on
//! every PR instead of only on the one box with the SDK staged.
//!
//! ⚠ The safety property that did NOT change: nothing checks these signatures against the real
//! symbols. They are declared as function-pointer types in [`crate::loader`], and a change to
//! `shim/fcshim.cpp` must land in the same commit as the matching change there. What the move DID
//! buy is that a MISSING symbol is now a diagnostic at load — the loader resolves every REQUIRED
//! one before it returns — rather than a crash on the first call that needed the one a stale shim
//! lacked. (The two OPTIONAL names are the exception that proves the rule: a shim predating them
//! opens, and [`FxcmSession::login`] says so by name instead of failing.)

use crate::loader::Shim;
use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// One side of an FX order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Buy,
    Sell,
}

impl Side {
    /// ForexConnect's `BuySell` code.
    fn code(self) -> &'static str {
        match self {
            Side::Buy => "B",
            Side::Sell => "S",
        }
    }
}

/// Result of placing an order — all ids needed to cancel it.
#[derive(Clone, Debug)]
pub(crate) struct PlacedOrder {
    pub order_id: String,
    pub offer_id: String,
    pub account_id: String,
    /// The limit price the shim SENT (the caller's rate snapped onto the grid, or the pip-distance
    /// price of the unpriced entry), or `0.0` for a market placement - a true-market order carries no
    /// resting price, so its execution price arrives with the fill on the async event lane. The
    /// adapter logs it beside the VENUE's own rate ([`FxcmSession::order_rate`]).
    pub rate: f64,
}

/// What a placement SENT, kept when its reply was lost (the outcome is UNKNOWN) so a later
/// [`FxcmSession::find_order`] can look the order up in the venue's Orders table and tell it from any
/// other: the RequestID the shim sent it with, plus every term the shim checks a candidate row against.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PendingLookup {
    pub request_id: String,
    pub account_id: String,
    pub offer_id: String,
    pub side: Side,
    /// Base units (`lots * base_unit_size`), the unit the venue's `Amount` is in.
    pub amount: i32,
    /// A limit entry (its `rate` is checked), as opposed to a market order.
    pub limit: bool,
    /// The rate the shim sent (`0.0` for a market order).
    pub rate: f64,
}

/// What the venue's Orders table holds for one order ([`FxcmSession::order_rate`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VenueRate {
    pub rate: f64,
    /// The instrument's price grid (`10^-digits`); `0.0` when the shim could not read it.
    pub tick: f64,
}

/// What the shim OBSERVED about a failed `fc_login_ex` — never a reading of what the venue MEANT.
///
/// ⚠ **The split that matters is between a class and a MESSAGE, and it is deliberate.** Every
/// credential-shaped failure measured on 2026-09-22 arrives through ONE callback and is told apart
/// only by its TEXT — a dead or renamed account (`User or connection doesn't exist.`, which FXCM ALSO
/// answered, 2026-10-24 09:11 UTC, to a valid account whose previous session was still being closed),
/// a LIVE
/// account with a wrong password (`Login failed. Incorrect user name or password`, measured against
/// the real host through this very path), and a bad connection name or unreachable host (an
/// `ORA-499` carrying the host-discovery request and an HTTP code). So [`Self::Reported`] carries
/// that text verbatim and this enum classifies nothing about it: a structured split would be a
/// regex over an undocumented, unversioned message set, which is the shape of claim this tree has
/// already watched rot — and note that the first two of those three were not even known to be
/// different until the measurement. What IS structural is everything the shim can see for itself,
/// which is every other variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginFailureClass {
    /// The SDK named a reason; the accompanying message is its own words.
    Reported,
    /// The session reached `Disconnected` with no `onLoginFailed` before it — it came up and went
    /// down. No text exists, and the class is the whole answer.
    Disconnected,
    /// No session-status callback arrived inside the shim's wait window at all. **This is the one
    /// class where a timeout is genuinely the answer** — and before this existed, it is what every
    /// failure looked like from outside.
    NoResponse,
    /// `CO2GTransport::createSession()` returned null: the libraries are there (the shim loaded)
    /// and the SDK did not initialise. ⚠ This used to be a SIGSEGV inside the start rather than a
    /// report — `fc_login` dereferenced the null session on the next line.
    SdkInitFailed,
    /// The login offers several trading sessions and the shim selects none, so it can never
    /// complete. ⚠ Also previously mute: the status matched no arm, so the login hung out its full
    /// wait and returned null saying nothing.
    TradingSessionRequired,
    /// The shim installed on THIS box predates `fc_login_ex`, so the SDK's words were discarded
    /// before Rust could see them. The old behaviour, ADMITTED — and the message names the remedy.
    ShimPredatesErrorReporting,
}

impl LoginFailureClass {
    /// Map the shim's integer class. An UNKNOWN code is [`Self::NoResponse`] rather than a panic: a
    /// NEWER shim under an older binary may name a class this build has never heard of, and the
    /// honest answer to "what happened" is then the same as for a silent one.
    fn from_code(code: c_int) -> Self {
        match code {
            1 => Self::Reported,
            2 => Self::Disconnected,
            4 => Self::SdkInitFailed,
            5 => Self::TradingSessionRequired,
            _ => Self::NoResponse,
        }
    }

    /// The short name an operator sees in a log line or a reject reason.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Reported => "the venue named a reason",
            Self::Disconnected => "disconnected with no reason given",
            Self::NoResponse => "no answer from ForexConnect",
            Self::SdkInitFailed => "the ForexConnect SDK did not initialise",
            Self::TradingSessionRequired => "a trading session must be selected",
            Self::ShimPredatesErrorReporting => "this box's shim predates login error reporting",
        }
    }
}

/// What replaces the query string of a host-discovery URL the SDK quotes back at us.
///
/// ⚠ **This is a redaction, not tidiness.** The measured `ORA-499` text embeds the whole request —
/// `object='/Hosts.jsp?ID=…&PN=<connection>&SN=ForexConnect&MV=5&LN=<login>&AT=PLAIN'` — where
/// `LN=` is the FXCM LOGIN NAME and `AT=` the auth type. That string reaches a `tracing::error!`,
/// the JSON log file at `trace`, and (through [`crate::exec`]'s refusal loop) an `OrderRejected`
/// reason the core journals. The workspace rule is that secrets never reach `Debug`/`Display`/logs,
/// so the identifying half of the URL is dropped before any of that; the PATH and the `errorCode=`
/// — the two parts that make the message diagnosable — survive.
const REDACTED_QUERY: &str = "?<query redacted: it carries the FXCM login name>";

/// Strip the query string out of any `object='…'` URL the SDK quoted, keeping everything else
/// byte-identical. See [`REDACTED_QUERY`].
///
/// Pure, and tolerant of a TRUNCATED buffer: a message cut off mid-URL has no closing quote, and
/// the redaction must still fire — the query is precisely the part a truncation leaves behind.
fn redact_login_text(text: &str) -> String {
    const OPEN: &str = "object='";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(OPEN) {
        let (head, tail) = rest.split_at(i + OPEN.len());
        out.push_str(head);
        // The quoted value ends at the closing quote, or at the end of a truncated message.
        let end = tail.find('\'').unwrap_or(tail.len());
        let (value, after) = tail.split_at(end);
        match value.find('?') {
            Some(q) => {
                out.push_str(&value[..q]);
                out.push_str(REDACTED_QUERY);
            }
            None => out.push_str(value),
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Errors from the FXCM native layer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum FxcmError {
    /// The ForexConnect shim could not be opened, so there is no SDK to talk to.
    ///
    /// ⚠ This used to mean "built without the SDK" and now means "the shim is not on this box".
    /// Every caller's handling is unchanged — it is still the answer that leaves FXCM on paper —
    /// but the REMEDY moved from a rebuild to an install, which is why
    /// [`crate::sdk_unavailable_reason`] exists to say which rungs were tried.
    Unavailable,
    /// The login returned no session. Carries what the shim OBSERVED and, for
    /// [`LoginFailureClass::Reported`], the SDK's own words.
    ///
    /// ⚠ **This was a UNIT variant carrying nothing**, and its doc read "bad creds / host
    /// unreachable" — the exact conflation being fixed. The SDK distinguishes a dead account from a
    /// bad connection name from an unreachable host, and says so in under a second; this crate
    /// collapsed all three into one mute return, which cost two sessions hours apiece.
    LoginFailed { class: LoginFailureClass, message: String },
    /// A native call returned a non-zero code; carries the SDK error text.
    Native { code: i32, message: String },
    /// A PLACEMENT was sent and its outcome is UNKNOWN (`code` is one of the shim's post-send codes,
    /// [`crate::event_mapper::outcome_is_unknown`]). The same as [`Self::Native`] for every reader of
    /// `code`/`message`, plus the terms the shim sent when it handed back the RequestID (`None` for a
    /// shim that predates that: nothing can then be looked up later).
    SentUnknown { code: i32, message: String, lookup: Option<Box<PendingLookup>> },
}

impl FxcmError {
    /// The request was SENT and its outcome is UNKNOWN ([`crate::event_mapper::outcome_is_unknown`]):
    /// the caller must not turn this into a refusal.
    pub(crate) fn outcome_unknown(&self) -> bool {
        matches!(self, FxcmError::SentUnknown { .. })
            || matches!(self, FxcmError::Native { code, .. } if crate::event_mapper::outcome_is_unknown(*code))
    }

    /// The lost placement's terms, when the shim handed back its RequestID.
    pub(crate) fn into_lookup(self) -> Option<PendingLookup> {
        match self {
            FxcmError::SentUnknown { lookup, .. } => lookup.map(|l| *l),
            _ => None,
        }
    }
}

/// The terms a placement SENT, for [`placement_failure`].
struct Sent {
    side: Side,
    amount: i32,
    limit: bool,
    rate: f64,
}

/// The error a failed placement call returns: [`FxcmError::SentUnknown`] when `rc` is a post-send code
/// (the shim then wrote the RequestID into the order-id buffer and filled the offer and account ids
/// before the send), else the plain [`FxcmError::Native`] it always was.
fn placement_failure(rc: i32, message: String, ids: [&[c_char]; 3], sent: &Sent) -> FxcmError {
    if !crate::event_mapper::outcome_is_unknown(rc) {
        return FxcmError::Native { code: rc, message };
    }
    let [oid, ofid, aid] = ids;
    let request_id = rd(oid);
    let lookup = (!request_id.is_empty()).then(|| {
        Box::new(PendingLookup {
            request_id,
            account_id: rd(aid),
            offer_id: rd(ofid),
            side: sent.side,
            amount: sent.amount,
            limit: sent.limit,
            rate: sent.rate,
        })
    });
    FxcmError::SentUnknown { code: rc, message, lookup }
}

impl std::fmt::Display for FxcmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FxcmError::Unavailable => {
                write!(
                    f,
                    "FXCM ForexConnect shim not loaded (see `vike_fxcm::sdk_unavailable_reason`)"
                )
            }
            // The venue's own words lead, because they are the thing two sessions went looking for.
            FxcmError::LoginFailed { class: LoginFailureClass::Reported, message } => {
                write!(f, "FXCM login failed — the venue said: {message}")
            }
            FxcmError::LoginFailed { class, message } => {
                write!(f, "FXCM login failed ({}): {message}", class.label())
            }
            FxcmError::Native { code, message } | FxcmError::SentUnknown { code, message, .. } => {
                write!(f, "FXCM native error {code}: {message}")
            }
        }
    }
}

impl std::error::Error for FxcmError {}

/// The reason a failed table read (`fc_orders` / `fc_trades` / `fc_account`) carries: the text the
/// shim wrote into the caller's buffer on its negative return — the SDK's own words for a refused or
/// timed-out refresh-table request (`fcshim.cpp`'s `refreshTable`) — passed through
/// [`redact_login_text`] like every other SDK string, else `generic`.
///
/// The fallback is load-bearing, not tidiness: a shim built before the table reads became refresh
/// requests (the `/var/lib/vike/lib` one the CI box has installed) writes NOTHING on a
/// negative return — its only negatives were "no handle" / "no account" / "no factory" — and a
/// buffer the shim never touched reads as empty. Pure.
fn failure_text(shim_text: &str, generic: &str) -> String {
    let text = shim_text.trim();
    if text.is_empty() { generic.to_string() } else { redact_login_text(text) }
}

/// Guard against interior NULs before crossing the FFI boundary.
fn cstr(s: &str) -> Result<CString, FxcmError> {
    CString::new(s).map_err(|_| FxcmError::Native {
        code: -100,
        message: "interior NUL byte in argument".into(),
    })
}

/// Read a shim-filled C string buffer back into an owned `String`.
fn rd(b: &[c_char]) -> String {
    // SAFETY: every call site passes a buffer the shim just filled through `cpy`/`emit_json`, both
    // of which NUL-terminate within the length they were given, and the buffer is zero-initialised
    // before the call so an untouched one reads as empty rather than running off the end.
    unsafe { std::ffi::CStr::from_ptr(b.as_ptr()).to_string_lossy().into_owned() }
}

/// The first buffer of a reconcile table read (`fc_orders` / `fc_trades` / `fc_system_properties`).
const TABLE_FIRST_BUFFER: usize = 8192;

/// The buffer of the Offers read (`fc_offers`): 1 MiB, room for thousands of ~240-byte rows. It must hold the
/// WHOLE table the first time, because every `fc_offers` call refreshes the table from the venue
/// ([`Overflow::Fail`]).
const OFFERS_BUFFER: usize = 1 << 20;

/// What a table read does when the table does not fit its buffer (the shim returns the FULL length needed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Overflow {
    /// Read again at the exact size. Right for a read that is the same table the second time (the login-time
    /// properties; a reconcile table, whose second read is a second refresh of a table of a few rows).
    Regrow,
    /// The overflow is an error naming the size, and the shim is NOT called a second time. For a read whose
    /// every call is a venue refresh: a retry would double the wait, and the table returned would be the second
    /// refresh's, not the one the first call measured.
    Fail,
}

/// Run one table read, `call(buffer)` being the shim's entry point over a zeroed buffer of the length it is
/// handed, and return the JSON it wrote. A negative return is a FAILED read and never an empty array (the shim
/// writes the reason into the buffer: see [`failure_text`]); an empty table is the venue saying the table is
/// empty, which the reconcile parsers read as "flat". Pure but for `call`, so a scripted shim drives it.
fn read_table(
    mut call: impl FnMut(&mut [c_char]) -> i32,
    first: usize,
    overflow: Overflow,
) -> Result<String, FxcmError> {
    let mut cap = first;
    loop {
        let mut buf = vec![0 as c_char; cap];
        let rc = call(&mut buf);
        if rc < 0 {
            return Err(FxcmError::Native {
                code: rc,
                message: failure_text(&rd(&buf), "table snapshot failed"),
            });
        }
        let needed = rc as usize;
        if needed < cap {
            return Ok(rd(&buf));
        }
        match overflow {
            // The table didn't fit — retry once at the exact size the shim asked for.
            Overflow::Regrow => cap = needed + 1,
            Overflow::Fail => {
                return Err(FxcmError::Native {
                    code: rc,
                    message: format!(
                        "the table is {needed} bytes and this read's buffer is {cap}; it is not read a \
                         second time (every call refreshes it from the venue)"
                    ),
                });
            }
        }
    }
}

const BUF: usize = 128;
const ERRBUF: usize = 256;

/// `fc_login_ex`'s error buffer — deliberately larger than [`ERRBUF`].
///
/// The most useful login diagnostic the SDK produces is the `ORA-499` for a bad connection name: it
/// carries the entire host-discovery request URL plus an HTTP code, and 256 bytes truncate it
/// mid-query-string, where the cut is invisible. Truncation is safe either way — the shim copies
/// within the length it is handed — so the only thing at stake is whether the message still reads.
const LOGIN_ERRBUF: usize = 1024;

/// A live ForexConnect session. Dropping it logs out.
pub(crate) struct FxcmSession {
    h: *mut c_void,
    /// The shim this session's handle belongs to. Held so `Drop` cannot race the `OnceLock` — and
    /// so a session can never be served by a different shim from the one that created its handle.
    shim: &'static Shim,
    /// The connection name this session logged in on (`"Demo"` or `"Real"`, the venue's setting, unvalidated):
    /// the subscription write is made on `"Demo"` only ([`crate::subscription::subscribe_gap`]).
    connection: String,
}

impl FxcmSession {
    /// Log in to FXCM.
    ///
    /// - `url`: host discovery URL, e.g. `http://www.fxcorporate.com/Hosts.jsp`
    /// - `conn`: connection name — `"Demo"` or `"Real"`
    ///
    /// Returns [`FxcmError::Unavailable`] when the shim is not installed on this box, which is the
    /// ordinary unconfigured state and the reason FXCM stays paper there.
    pub(crate) fn login(user: &str, pw: &str, url: &str, conn: &str) -> Result<Self, FxcmError> {
        let shim = Shim::get().ok_or(FxcmError::Unavailable)?;
        let (u, p, ur, c) = (cstr(user)?, cstr(pw)?, cstr(url)?, cstr(conn)?);
        settle_after_logout();
        let Some(login_ex) = shim.fc_login_ex else {
            // ⚠ THE OLD-SHIM PATH, and it is the MODAL case for the first release after this
            // change rather than an edge: `.github/workflows/release.yml` attaches `libfcshim.so`
            // as an asset and `deploy/` is forbidden from ever naming it, so a new binary meets
            // whatever shim an operator last installed by hand. This call is byte-identical to the
            // one this function has always made — same four-argument symbol, same arguments — and
            // the failure is the old message plus the remedy, rather than a crash or a drop to
            // paper.
            //
            // SAFETY: four valid NUL-terminated strings, alive across the call. The shim copies
            // what it needs; nothing it returns borrows them.
            let h = unsafe { (shim.fc_login)(u.as_ptr(), p.as_ptr(), ur.as_ptr(), c.as_ptr()) };
            return if h.is_null() {
                Err(FxcmError::LoginFailed {
                    class: LoginFailureClass::ShimPredatesErrorReporting,
                    // ⚠ `describe_shim_path`, never `shim.path` — the path field is the ladder
                    // RUNG that answered, and rung 3 is the bare file name, so on a box whose shim
                    // came off `LD_LIBRARY_PATH` this message used to name no file at all. Saying
                    // which file to replace is the whole job it has.
                    message: format!(
                        "the shim this process opened — {} — exports no `fc_login_ex`, so the \
                         SDK's own account of this failure was discarded before it could be read. \
                         Reinstall it with `just fxcm-package <root> <shim>`; \
                         `docs/ops/fxcm-forexconnect.md` is the operator page.",
                        crate::loader::describe_shim_path(&shim.path)
                    ),
                })
            } else {
                Ok(Self { h, shim, connection: conn.to_string() })
            };
        };
        let mut err = vec![0 as c_char; LOGIN_ERRBUF];
        let mut class: c_int = 0;
        // SAFETY: as the four-argument call above, plus an error buffer passed with its own length
        // (the shim NUL-terminates within it through `cpy`, like every other error buffer here) and
        // a live `c_int` for the class.
        let h = unsafe {
            login_ex(
                u.as_ptr(),
                p.as_ptr(),
                ur.as_ptr(),
                c.as_ptr(),
                err.as_mut_ptr(),
                err.len() as c_int,
                &mut class,
            )
        };
        if h.is_null() {
            return Err(FxcmError::LoginFailed {
                class: LoginFailureClass::from_code(class),
                message: redact_login_text(&rd(&err)),
            });
        }
        Ok(Self { h, shim, connection: conn.to_string() })
    }

    /// `(account_id, balance)` of the first tradable (non-margin-call) account.
    pub(crate) fn account(&self) -> Result<(String, f64), FxcmError> {
        let mut acct = vec![0 as c_char; BUF];
        let mut bal = 0f64;
        // SAFETY: `self.h` came from this shim's `fc_login`; the buffer and its length agree.
        let rc = unsafe {
            (self.shim.fc_account)(self.h, acct.as_mut_ptr(), acct.len() as i32, &mut bal)
        };
        if rc != 0 {
            // On a failure the shim writes the reason into `acct` (see `fc_account`'s doc).
            return Err(FxcmError::Native {
                code: rc,
                message: failure_text(&rd(&acct), "fc_account failed"),
            });
        }
        Ok((rd(&acct), bal))
    }

    /// `instrument`'s base unit size on this session's tradable account — the number the shim
    /// multiplies a lot count by before placing (`fcshim.cpp`'s `fc_place`).
    ///
    /// ⚠ **This is the figure that makes `qty` mean the same thing on the way out and on the way
    /// back.** `OrderRequest::qty` is base units everywhere in this workspace and a fill's
    /// `last_qty` is base units too, but the shim's `Amount` is `baseUnit * lots` — so a caller
    /// sending the units it holds elsewhere placed that many LOTS.
    /// [`crate::event_mapper::lots_for`] divides by this and refuses a size that is not an exact
    /// multiple.
    ///
    /// Per-instrument AND per-ACCOUNT, which is why it cannot come from
    /// `crates/bridges/fxcm/src/catalog.rs`'s static `FX_TABLE` and why only a live session can
    /// answer. [`crate::exec`] caches it per instrument for the life of the session, which is the
    /// scope over which the account cannot change.
    pub(crate) fn base_unit_size(&self, instrument: &str) -> Result<i32, FxcmError> {
        let instr = cstr(instrument)?;
        let mut out = 0i32;
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: as `account` above, plus `instr` alive across the call and `out` a live i32.
        let rc = unsafe {
            (self.shim.fc_base_unit_size)(
                self.h,
                instr.as_ptr(),
                &mut out,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            return Err(FxcmError::Native { code: rc, message: rd(&err) });
        }
        Ok(out)
    }

    /// Place a resting LIMIT entry `pips_away` from market (buy below ask / sell above bid) so it
    /// does not immediately fill. Returns the ids required to cancel it.
    pub(crate) fn place_limit_entry(
        &self,
        instrument: &str,
        side: Side,
        pips_away: i32,
        lots: i32,
        amount: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let instr = cstr(instrument)?;
        let bs = cstr(side.code())?;
        let mut oid = vec![0 as c_char; BUF];
        let mut ofid = vec![0 as c_char; BUF];
        let mut aid = vec![0 as c_char; BUF];
        let mut err = vec![0 as c_char; ERRBUF];
        let mut rate = 0f64;
        // SAFETY: every buffer is passed with its own length and outlives the call.
        let rc = unsafe {
            (self.shim.fc_place_entry)(
                self.h,
                instr.as_ptr(),
                bs.as_ptr(),
                pips_away,
                lots,
                oid.as_mut_ptr(),
                oid.len() as i32,
                ofid.as_mut_ptr(),
                ofid.len() as i32,
                aid.as_mut_ptr(),
                aid.len() as i32,
                &mut rate,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            let sent = Sent { side, amount, limit: true, rate };
            return Err(placement_failure(rc, rd(&err), [&oid, &ofid, &aid], &sent));
        }
        Ok(PlacedOrder { order_id: rd(&oid), offer_id: rd(&ofid), account_id: rd(&aid), rate })
    }

    /// Place a resting LIMIT entry AT `rate` — the price the caller chose (`fc_place_entry_at`, ABI 2).
    ///
    /// The shim refuses a rate that is off the instrument's price grid (code -10) rather than
    /// rounding it, and the VENUE refuses one on the wrong side of the market (-6, its own words).
    /// A shim that predates ABI 2 has no such entry point: the answer is then an error naming the
    /// remedy, never the fixed-distance entry, which would rest at a price nobody asked for.
    pub(crate) fn place_limit_at(
        &self,
        instrument: &str,
        side: Side,
        rate: f64,
        lots: i32,
        amount: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let Some(place_at) = self.shim.fc_place_entry_at else {
            return Err(
                self.shim_predates("fc_place_entry_at", "place a limit at a requested price")
            );
        };
        let instr = cstr(instrument)?;
        let bs = cstr(side.code())?;
        let mut oid = vec![0 as c_char; BUF];
        let mut ofid = vec![0 as c_char; BUF];
        let mut aid = vec![0 as c_char; BUF];
        let mut err = vec![0 as c_char; ERRBUF];
        let mut sent = 0f64;
        // SAFETY: as `place_limit_entry` above, with the requested rate passed by value.
        let rc = unsafe {
            place_at(
                self.h,
                instr.as_ptr(),
                bs.as_ptr(),
                rate,
                lots,
                oid.as_mut_ptr(),
                oid.len() as i32,
                ofid.as_mut_ptr(),
                ofid.len() as i32,
                aid.as_mut_ptr(),
                aid.len() as i32,
                &mut sent,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            let sent = Sent { side, amount, limit: true, rate: sent };
            let message = failure_text(&rd(&err), "fc_place_entry_at failed");
            return Err(placement_failure(rc, message, [&oid, &ofid, &aid], &sent));
        }
        Ok(PlacedOrder {
            order_id: rd(&oid),
            offer_id: rd(&ofid),
            account_id: rd(&aid),
            rate: sent,
        })
    }

    /// Amend the RATE of a resting entry order in place (`fc_edit_order`, ABI 2). Price only; the
    /// order keeps its venue id. Same grid rule and same old-shim answer as [`Self::place_limit_at`].
    pub(crate) fn edit_order_rate(
        &self,
        order_id: &str,
        account_id: &str,
        instrument: &str,
        rate: f64,
    ) -> Result<(), FxcmError> {
        let Some(edit) = self.shim.fc_edit_order else {
            return Err(self.shim_predates("fc_edit_order", "amend a resting order"));
        };
        let (o, a, i) = (cstr(order_id)?, cstr(account_id)?, cstr(instrument)?);
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: three valid strings alive across the call, the rate by value, and the error buffer
        // with its length.
        let rc = unsafe {
            edit(
                self.h,
                o.as_ptr(),
                a.as_ptr(),
                i.as_ptr(),
                rate,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            return Err(FxcmError::Native {
                code: rc,
                message: failure_text(&rd(&err), "fc_edit_order failed"),
            });
        }
        Ok(())
    }

    /// The Rate the VENUE's Orders table holds for `order_id` right now (`fc_order_rate`, a fresh
    /// refresh-table read), plus the instrument's price tick (`0.0` when the shim could not read it).
    ///
    /// `Ok(None)` when this shim has no such export (installed before it existed): the echo is a
    /// diagnostic, so its absence is never a refusal. `Err` is a failed read (the order missing from
    /// the table, a refresh timeout), and callers log it - they never fail the order over it.
    pub(crate) fn order_rate(
        &self,
        order_id: &str,
        account_id: &str,
    ) -> Result<Option<VenueRate>, FxcmError> {
        let Some(read) = self.shim.fc_order_rate else {
            return Ok(None);
        };
        let (o, a) = (cstr(order_id)?, cstr(account_id)?);
        let (mut rate, mut tick) = (0f64, 0f64);
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: two valid strings alive across the call, two out-doubles on this stack frame, and
        // the error buffer with its length.
        let rc = unsafe {
            read(
                self.h,
                o.as_ptr(),
                a.as_ptr(),
                &mut rate,
                &mut tick,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            return Err(FxcmError::Native {
                code: rc,
                message: failure_text(&rd(&err), "fc_order_rate failed"),
            });
        }
        Ok(Some(VenueRate { rate, tick }))
    }

    /// Look an order up in a FRESH Orders table by the RequestID of a placement whose reply was lost
    /// (`fc_find_order`), checked against everything that placement sent. `Ok(Some(order_id))` found
    /// exactly one; `Ok(None)` the venue does not hold it (it never rested, or a market order filled
    /// and left the table); `Err` could not tell: a refresh failure, more than one row fitting
    /// (`-13`, none claimed), or a shim without the export. The active `confirm` is its caller. Never
    /// from an SDK callback thread; bounded in the shim (`LOOKUP_REFRESH_MS`).
    pub(crate) fn find_order(&self, p: &PendingLookup) -> Result<Option<String>, FxcmError> {
        let Some(find) = self.shim.fc_find_order else {
            return Err(FxcmError::Native {
                code: -101,
                message: format!(
                    "the shim this process opened — {} — exports no `fc_find_order`, so a placement \
                     whose reply was lost cannot be looked up again. Reinstall the shim built from \
                     this release with `just fxcm-package <root> <shim>`",
                    crate::loader::describe_shim_path(&self.shim.path),
                ),
            });
        };
        let (rq, ac, of, bs) =
            (cstr(&p.request_id)?, cstr(&p.account_id)?, cstr(&p.offer_id)?, cstr(p.side.code())?);
        let mut oid = vec![0 as c_char; BUF];
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: four valid strings alive across the call, the amount, flag and rate by value, and
        // two buffers each passed with its own length.
        let rc = unsafe {
            find(
                self.h,
                rq.as_ptr(),
                ac.as_ptr(),
                of.as_ptr(),
                bs.as_ptr(),
                p.amount,
                c_int::from(p.limit),
                p.rate,
                oid.as_mut_ptr(),
                oid.len() as i32,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        match rc {
            0 => Ok(Some(rd(&oid))),
            -3 => Ok(None),
            code => Err(FxcmError::Native {
                code,
                message: failure_text(&rd(&err), "fc_find_order failed"),
            }),
        }
    }

    /// The refusal a shim built before ABI 2 earns for an entry point it lacks. Names the file, the
    /// version it reports and the remedy: this is what an operator reads in an `OrderRejected`.
    fn shim_predates(&self, symbol: &str, what: &str) -> FxcmError {
        FxcmError::Native {
            code: -101,
            message: format!(
                "the shim this process opened — {} — reports ABI version {} and exports no \
                 `{symbol}`, so it cannot {what}. Reinstall the shim built from this release with \
                 `just fxcm-package <root> <shim>`; `docs/ops/fxcm-forexconnect.md` is the \
                 operator page. Market orders and unpriced limits are unaffected",
                crate::loader::describe_shim_path(&self.shim.path),
                self.shim.abi_version,
            ),
        }
    }

    /// Place an immediately-executing TRUE MARKET order (ForexConnect `O2G2::Orders::TrueMarketOpen`).
    /// Unlike [`Self::place_limit_entry`] this is expected to FILL; the returned [`PlacedOrder`]
    /// still carries the ids (a market order can still be canceled while unfilled, and the venue
    /// order id routes the async fill back to its client order id), but its `rate` is `0.0` — the
    /// execution price arrives with the fill on the async event lane, not from the placement call.
    pub(crate) fn place_market(
        &self,
        instrument: &str,
        side: Side,
        lots: i32,
        amount: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let instr = cstr(instrument)?;
        let bs = cstr(side.code())?;
        let mut oid = vec![0 as c_char; BUF];
        let mut ofid = vec![0 as c_char; BUF];
        let mut aid = vec![0 as c_char; BUF];
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: as `place_limit_entry` above.
        let rc = unsafe {
            (self.shim.fc_place_market)(
                self.h,
                instr.as_ptr(),
                bs.as_ptr(),
                lots,
                oid.as_mut_ptr(),
                oid.len() as i32,
                ofid.as_mut_ptr(),
                ofid.len() as i32,
                aid.as_mut_ptr(),
                aid.len() as i32,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            let sent = Sent { side, amount, limit: false, rate: 0.0 };
            return Err(placement_failure(rc, rd(&err), [&oid, &ofid, &aid], &sent));
        }
        Ok(PlacedOrder { order_id: rd(&oid), offer_id: rd(&ofid), account_id: rd(&aid), rate: 0.0 })
    }

    /// Cancel a resting order (ids come from [`PlacedOrder`]).
    pub(crate) fn delete_order(
        &self,
        order_id: &str,
        account_id: &str,
        offer_id: &str,
    ) -> Result<(), FxcmError> {
        let (o, a, f) = (cstr(order_id)?, cstr(account_id)?, cstr(offer_id)?);
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: three valid strings alive across the call, plus the error buffer and its length.
        let rc = unsafe {
            (self.shim.fc_delete_order)(
                self.h,
                o.as_ptr(),
                a.as_ptr(),
                f.as_ptr(),
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            return Err(FxcmError::Native { code: rc, message: rd(&err) });
        }
        Ok(())
    }

    /// Drain the next pending async order event (fill/cancel/reject) as a JSON string, or `None`
    /// when the shim's queue is empty. The exec thread polls this between commands and maps each
    /// event via [`super::event_mapper::map_fxcm_event`]. Safe to call repeatedly until it returns `None`.
    pub(crate) fn poll_event(&self) -> Result<Option<String>, FxcmError> {
        let mut buf = vec![0 as c_char; 1024];
        // SAFETY: the buffer is passed with its own length and outlives the call.
        let rc = unsafe { (self.shim.fc_poll_event)(self.h, buf.as_mut_ptr(), buf.len() as i32) };
        match rc {
            0 => Ok(None),
            1 => Ok(Some(rd(&buf))),
            code => Err(FxcmError::Native { code, message: "fc_poll_event failed".into() }),
        }
    }

    /// Read one reconcile table snapshot (`fc_orders` / `fc_trades`) as a JSON array string, growing
    /// the buffer once if the first read reports it was too small (the shim returns the FULL length
    /// needed). Read-only; used by the reconcile session thread ([`super::recon_client`]).
    fn snapshot(&self, f: crate::loader::FcTable) -> Result<String, FxcmError> {
        self.snapshot_in(f, TABLE_FIRST_BUFFER, Overflow::Regrow)
    }

    /// [`Self::snapshot`] with the first buffer's size and what an overflow does ([`read_table`]).
    fn snapshot_in(
        &self,
        f: crate::loader::FcTable,
        first: usize,
        overflow: Overflow,
    ) -> Result<String, FxcmError> {
        read_table(
            |buf| {
                // SAFETY: `f` is one of this shim's own table entry points, and the buffer is passed
                // with the length it was allocated at.
                unsafe { f(self.h, buf.as_mut_ptr(), buf.len() as i32) }
            },
            first,
            overflow,
        )
    }

    /// The current Orders table (resting entry/limit/stop working orders) as a JSON array string —
    /// the reconcile order-status snapshot. See [`super::recon_client::parse_orders`].
    pub(crate) fn orders_json(&self) -> Result<String, FxcmError> {
        self.snapshot(self.shim.fc_orders)
    }

    /// The current Trades table (open positions) as a JSON array string — reconcile derives both the
    /// net position report and the per-open-trade fill report from it. See
    /// [`super::recon_client::parse_positions`] / [`super::recon_client::parse_fills`].
    pub(crate) fn trades_json(&self) -> Result<String, FxcmError> {
        self.snapshot(self.shim.fc_trades)
    }

    /// The login's SYSTEM PROPERTIES as one JSON object of strings (`fc_system_properties`): where the
    /// account's currency lives (`BASE_CRNCY`, [`crate::home_conversion::account_ccy`]). `Ok(None)` when this
    /// shim has no such export (installed before it existed): the caller says so by name and falls back, it
    /// never treats the absence as a currency. A login-time read, so it asks the venue nothing.
    pub(crate) fn system_properties_json(&self) -> Result<Option<String>, FxcmError> {
        self.shim.fc_system_properties.map(|f| self.snapshot(f)).transpose()
    }

    /// The CURRENT Offers table as a JSON array (`fc_offers`, a refresh-table request): the source of a
    /// conversion rate and of a symbol's grid ([`crate::home_conversion::facts_from_offers`]). `Ok(None)`
    /// when this shim has no such export, like [`Self::system_properties_json`].
    ///
    /// ONE refresh per call: the buffer is [`OFFERS_BUFFER`] (room for thousands of rows; FXCM lists a few
    /// hundred instruments at most), and a table that still does not fit is an error, never a second call, because
    /// the shim refreshes the table from the venue on EVERY call (a regrow-and-retry would be a second refresh and
    /// the table returned would be the second one).
    pub(crate) fn offers_json(&self) -> Result<Option<String>, FxcmError> {
        self.shim.fc_offers.map(|f| self.snapshot_in(f, OFFERS_BUFFER, Overflow::Fail)).transpose()
    }

    /// The account's TRAITS as one JSON object (`fc_account_traits`): `maintenance_type` ("0" is a netting account),
    /// `account_kind`, `hedge_margin_pct`. A login-time read of the row `firstAccount` holds, so it asks the venue
    /// nothing. `Ok(None)` when this shim has no such export (installed before it existed): the account's mode is then
    /// UNKNOWN ([`crate::close::AccountMode`]), never assumed.
    pub(crate) fn account_traits_json(&self) -> Result<Option<String>, FxcmError> {
        self.shim.fc_account_traits.map(|f| self.snapshot(f)).transpose()
    }

    /// **Why this session cannot close a trade by its id**, or `None` when it can: the shim exports no `fc_open_trades`
    /// or no `fc_close_trade` (installed before they existed). Names the file and the remedy, and says what the missing
    /// close costs: on a hedging account an opposite market order OPENS a second trade, so the bridge refuses a
    /// reduce-only market order instead of sending it.
    pub(crate) fn close_gap(&self) -> Option<String> {
        let missing: Vec<&str> = [
            ("fc_open_trades", self.shim.fc_open_trades.is_none()),
            ("fc_close_trade", self.shim.fc_close_trade.is_none()),
        ]
        .into_iter()
        .filter_map(|(name, absent)| absent.then_some(name))
        .collect();
        (!missing.is_empty()).then(|| {
            format!(
                "the shim this process opened — {} — exports no `{}`, so it cannot close a trade by its id. On a hedging \
                 account an opposite market order OPENS a second trade instead of closing the first, so this order is \
                 refused rather than sent. Reinstall the shim built from this release with \
                 `just fxcm-package <root> <shim>`; `docs/ops/fxcm-forexconnect.md` is the operator page",
                crate::loader::describe_shim_path(&self.shim.path),
                missing.join("`, `"),
            )
        })
    }

    /// The account's OPEN trades as a JSON array (`fc_open_trades`, a fresh bounded read): the close planner's input
    /// ([`crate::close::parse_open_trades`]). A shim without the export answers [`Self::close_gap`]'s words.
    pub(crate) fn open_trades_json(&self) -> Result<String, FxcmError> {
        match self.shim.fc_open_trades {
            Some(f) => self.snapshot(f),
            None => {
                Err(FxcmError::Native { code: -101, message: self.close_gap().unwrap_or_default() })
            }
        }
    }

    /// Close `amount` base units of the open trade `trade_id` (`fc_close_trade`: `TrueMarketClose` + `TradeID`). The
    /// returned [`PlacedOrder`] carries the CLOSING order's ids; the close itself arrives on the event lane as a
    /// ClosedTrades fill. A reply that never came is [`FxcmError::SentUnknown`] with NO lookup (a market close fills at
    /// once and leaves the Orders table, so nothing can find it later): the close may have happened.
    pub(crate) fn close_trade(
        &self,
        trade_id: &str,
        amount: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let Some(close) = self.shim.fc_close_trade else {
            return Err(FxcmError::Native {
                code: -101,
                message: self.close_gap().unwrap_or_default(),
            });
        };
        let id = cstr(trade_id)?;
        let mut oid = vec![0 as c_char; BUF];
        let mut ofid = vec![0 as c_char; BUF];
        let mut aid = vec![0 as c_char; BUF];
        let mut err = vec![0 as c_char; ERRBUF];
        // SAFETY: a valid trade id string alive across the call, the amount by value, and four buffers each passed with
        // its own length that outlive the call.
        let rc = unsafe {
            close(
                self.h,
                id.as_ptr(),
                amount,
                oid.as_mut_ptr(),
                oid.len() as i32,
                ofid.as_mut_ptr(),
                ofid.len() as i32,
                aid.as_mut_ptr(),
                aid.len() as i32,
                err.as_mut_ptr(),
                err.len() as i32,
            )
        };
        if rc != 0 {
            let message = failure_text(&rd(&err), "fc_close_trade failed");
            return Err(if crate::event_mapper::outcome_is_unknown(rc) {
                FxcmError::SentUnknown { code: rc, message, lookup: None }
            } else {
                FxcmError::Native { code: rc, message }
            });
        }
        Ok(PlacedOrder { order_id: rd(&oid), offer_id: rd(&ofid), account_id: rd(&aid), rate: 0.0 })
    }

    /// **Why this session cannot make an instrument tradable itself**, or `None` when it can: the shim exports no
    /// `fc_set_subscription` (installed before it existed), or the session is not on the `Demo` connection
    /// ([`crate::subscription::subscribe_gap`]). Asked ONCE per session by the exec thread, which then neither asks
    /// the shim nor logs the same sentence at every order.
    pub(crate) fn cannot_subscribe(&self) -> Option<String> {
        let without_export = self
            .shim
            .fc_set_subscription
            .is_none()
            .then(|| crate::loader::describe_shim_path(&self.shim.path));
        crate::subscription::subscribe_gap(&self.connection, without_export.as_deref())
    }

    /// **Make `instrument` (`EUR/USD`) tradable on this login** (`fc_set_subscription`, FXCM's own
    /// `SetSubscriptionStatus`), inside `budget_ms` in total. Returns at once when the row already reads `T`.
    ///
    /// # Errors
    /// [`Self::cannot_subscribe`] (nothing is sent: a `transient` failure, no verdict on the symbol), or
    /// [`crate::subscription::answer_from_shim`]'s reading of what the shim returned.
    pub(crate) fn subscribe_tradable(
        &self,
        instrument: &str,
        budget_ms: i32,
    ) -> Result<(), crate::subscription::Failure> {
        let failed = |why: String| crate::subscription::Failure { why, transient: false };
        if let Some(gap) = self.cannot_subscribe() {
            // NOTHING was sent, so this is no verdict on the symbol (`transient`): an order is still placed and the
            // shim's own offer lookup judges the row (a row that is `T` trades exactly as it did before this export
            // existed), and the facts lane is not remembered by the node as a refusal of the symbol.
            return Err(crate::subscription::Failure {
                why: format!(
                    "fxcm: the bridge cannot subscribe `{instrument}` for trading itself: {gap}"
                ),
                transient: true,
            });
        }
        let Some(set) = self.shim.fc_set_subscription else {
            return Err(failed("fxcm: the shim exports no `fc_set_subscription`".to_string()));
        };
        let (i, s) = match (cstr(instrument), cstr(crate::subscription::TRADABLE)) {
            (Ok(i), Ok(s)) => (i, s),
            (Err(e), _) | (_, Err(e)) => return Err(failed(e.to_string())),
        };
        let mut out = vec![0 as c_char; ERRBUF];
        // SAFETY: two valid strings alive across the call, the budget by value, and the buffer with its length.
        let rc = unsafe {
            set(self.h, i.as_ptr(), s.as_ptr(), budget_ms, out.as_mut_ptr(), out.len() as i32)
        };
        crate::subscription::answer_from_shim(instrument, rc, &failure_text(&rd(&out), ""))
    }
}

impl Drop for FxcmSession {
    fn drop(&mut self) {
        // SAFETY: `self.h` is this shim's own live handle and is dropped exactly once — `FxcmSession`
        // is neither `Copy` nor `Clone`, so no second owner can log the same handle out.
        unsafe { (self.shim.fc_logout)(self.h) };
        *LAST_LOGOUT.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
    }
}

/// When the last session of this process finished its `fc_logout`.
static LAST_LOGOUT: Mutex<Option<Instant>> = Mutex::new(None);

/// How long a login waits after a logout. FXCM still counts an account's previous session as open for a
/// moment after the client-side logout returns: 2026-10-24 09:11 UTC the replacement start's login, made
/// right after the old adapter was torn down, was answered `User or connection doesn't exist.` for a VALID
/// account. Five seconds is a guess at "a moment" (UNMEASURED); a login that still meets the conflict is
/// retried on the normal schedule, so this only makes the first failure rarer. It costs nothing when no
/// session was logged out within the last five seconds, which includes every login of a fresh process.
const LOGOUT_SETTLE: Duration = Duration::from_secs(5);

/// How much of [`LOGOUT_SETTLE`] is left at `now`, given when the last logout finished. Pure.
fn settle_wait(last_logout: Option<Instant>, now: Instant) -> Duration {
    last_logout.map_or(Duration::ZERO, |at| {
        LOGOUT_SETTLE.saturating_sub(now.saturating_duration_since(at))
    })
}

/// Sleep out what is left of [`LOGOUT_SETTLE`] since the last logout, before a login.
fn settle_after_logout() {
    let last = *LAST_LOGOUT.lock().unwrap_or_else(PoisonError::into_inner);
    let wait = settle_wait(last, Instant::now());
    if !wait.is_zero() {
        tracing::info!(
            venue = "fxcm",
            wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "fxcm: a session was logged out moments ago; the next login waits for the venue to close it"
        );
        std::thread::sleep(wait);
    }
}

// The ForexConnect session is a single-owner native handle; it is not safe to share across
// threads. `FxcmSession` is deliberately neither `Send` nor `Sync` (raw pointer field).

#[path = "sys_tests.rs"]
#[cfg(test)]
mod sys_tests;

#[path = "sys_props.rs"]
#[cfg(test)]
mod sys_props;
