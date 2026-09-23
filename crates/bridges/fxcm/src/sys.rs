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
//! `vike_mount::make_engine`'s refusal and every caller's handling are unchanged. What changed is
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

/// One side of an FX order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
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
pub struct PlacedOrder {
    pub order_id: String,
    pub offer_id: String,
    pub account_id: String,
    /// The limit price the shim computed (pips away from market), or `0.0` for a market
    /// placement — a true-market order carries no resting price, so its execution price arrives
    /// with the fill on the async event lane instead.
    ///
    /// ⚠ **NOTHING READS THIS, and saying so is the point.** It was invisible until 2026-09-09
    /// because this file carried a blanket `#![allow(dead_code)]` for the compile-time stub build's
    /// sake; with the stub gone the whole file is compiled on every box and the compiler found it.
    /// It is KEPT rather than deleted because it is the only record of what the venue was actually
    /// told — the shim derives the resting rate from the live quote, so the number is not
    /// reconstructible from the request — and a resting order's price belongs in the fill
    /// reconciliation the day that lane wants it. The allow is per-FIELD so it can never spread
    /// back to the file.
    #[allow(dead_code)]
    pub rate: f64,
}

/// What the shim OBSERVED about a failed `fc_login_ex` — never a reading of what the venue MEANT.
///
/// ⚠ **The split that matters is between a class and a MESSAGE, and it is deliberate.** Every
/// credential-shaped failure measured on 2026-09-22 arrives through ONE callback and is told apart
/// only by its TEXT — a dead or renamed account (`User or connection doesn't exist.`), a LIVE
/// account with a wrong password (`Login failed. Incorrect user name or password`, measured against
/// the real host through this very path), and a bad connection name or unreachable host (an
/// `ORA-499` carrying the host-discovery request and an HTTP code). So [`Self::Reported`] carries
/// that text verbatim and this enum classifies nothing about it: a structured split would be a
/// regex over an undocumented, unversioned message set, which is the shape of claim this tree has
/// already watched rot — and note that the first two of those three were not even known to be
/// different until the measurement. What IS structural is everything the shim can see for itself,
/// which is every other variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginFailureClass {
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
    /// and the SDK did not initialise. ⚠ This used to be a SIGSEGV inside the mount rather than a
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FxcmError {
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
            FxcmError::Native { code, message } => {
                write!(f, "FXCM native error {code}: {message}")
            }
        }
    }
}

impl std::error::Error for FxcmError {}

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
pub struct FxcmSession {
    h: *mut c_void,
    /// The shim this session's handle belongs to. Held so `Drop` cannot race the `OnceLock` — and
    /// so a session can never be served by a different shim from the one that created its handle.
    shim: &'static Shim,
}

impl FxcmSession {
    /// Log in to FXCM.
    ///
    /// - `url`: host discovery URL, e.g. `http://www.fxcorporate.com/Hosts.jsp`
    /// - `conn`: connection name — `"Demo"` or `"Real"`
    ///
    /// Returns [`FxcmError::Unavailable`] when the shim is not installed on this box, which is the
    /// ordinary unconfigured state and the reason FXCM stays paper there.
    pub fn login(user: &str, pw: &str, url: &str, conn: &str) -> Result<Self, FxcmError> {
        let shim = Shim::get().ok_or(FxcmError::Unavailable)?;
        let (u, p, ur, c) = (cstr(user)?, cstr(pw)?, cstr(url)?, cstr(conn)?);
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
                Ok(Self { h, shim })
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
        Ok(Self { h, shim })
    }

    /// `(account_id, balance)` of the first tradable (non-margin-call) account.
    pub fn account(&self) -> Result<(String, f64), FxcmError> {
        let mut acct = vec![0 as c_char; BUF];
        let mut bal = 0f64;
        // SAFETY: `self.h` came from this shim's `fc_login`; the buffer and its length agree.
        let rc = unsafe {
            (self.shim.fc_account)(self.h, acct.as_mut_ptr(), acct.len() as i32, &mut bal)
        };
        if rc != 0 {
            return Err(FxcmError::Native { code: rc, message: "fc_account failed".into() });
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
    pub fn base_unit_size(&self, instrument: &str) -> Result<i32, FxcmError> {
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
    pub fn place_limit_entry(
        &self,
        instrument: &str,
        side: Side,
        pips_away: i32,
        lots: i32,
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
            return Err(FxcmError::Native { code: rc, message: rd(&err) });
        }
        Ok(PlacedOrder { order_id: rd(&oid), offer_id: rd(&ofid), account_id: rd(&aid), rate })
    }

    /// Place an immediately-executing TRUE MARKET order (ForexConnect `O2G2::Orders::TrueMarketOpen`).
    /// Unlike [`Self::place_limit_entry`] this is expected to FILL; the returned [`PlacedOrder`]
    /// still carries the ids (a market order can still be canceled while unfilled, and the venue
    /// order id routes the async fill back to its client order id), but its `rate` is `0.0` — the
    /// execution price arrives with the fill on the async event lane, not from the placement call.
    pub fn place_market(
        &self,
        instrument: &str,
        side: Side,
        lots: i32,
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
            return Err(FxcmError::Native { code: rc, message: rd(&err) });
        }
        Ok(PlacedOrder { order_id: rd(&oid), offer_id: rd(&ofid), account_id: rd(&aid), rate: 0.0 })
    }

    /// Cancel a resting order (ids come from [`PlacedOrder`]).
    pub fn delete_order(
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
    pub fn poll_event(&self) -> Result<Option<String>, FxcmError> {
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
        let mut cap: usize = 8192;
        loop {
            let mut buf = vec![0 as c_char; cap];
            // SAFETY: `f` is one of this shim's own table entry points, and the buffer is passed
            // with the length it was allocated at.
            let rc = unsafe { f(self.h, buf.as_mut_ptr(), cap as i32) };
            if rc < 0 {
                return Err(FxcmError::Native {
                    code: rc,
                    message: "table snapshot failed".into(),
                });
            }
            let needed = rc as usize;
            if needed < cap {
                return Ok(rd(&buf));
            }
            cap = needed + 1; // the table didn't fit — retry once at the exact size the shim asked for
        }
    }

    /// The current Orders table (resting entry/limit/stop working orders) as a JSON array string —
    /// the reconcile order-status snapshot. See [`super::recon_client::parse_orders`].
    pub fn orders_json(&self) -> Result<String, FxcmError> {
        self.snapshot(self.shim.fc_orders)
    }

    /// The current Trades table (open positions) as a JSON array string — reconcile derives both the
    /// net position report and the per-open-trade fill report from it. See
    /// [`super::recon_client::parse_positions`] / [`super::recon_client::parse_fills`].
    pub fn trades_json(&self) -> Result<String, FxcmError> {
        self.snapshot(self.shim.fc_trades)
    }
}

impl Drop for FxcmSession {
    fn drop(&mut self) {
        // SAFETY: `self.h` is this shim's own live handle and is dropped exactly once — `FxcmSession`
        // is neither `Copy` nor `Clone`, so no second owner can log the same handle out.
        unsafe { (self.shim.fc_logout)(self.h) };
    }
}

// The ForexConnect session is a single-owner native handle; it is not safe to share across
// threads. `FxcmSession` is deliberately neither `Send` nor `Sync` (raw pointer field).

#[cfg(test)]
mod tests {
    use super::*;

    /// With no shim installed — which is every CI box and every fresh clone — a login is
    /// `Unavailable` rather than a panic, a hang, or a link error.
    ///
    /// ⚠ Conditional on the shim's absence rather than unconditional, and this is the difference
    /// the whole change is about: before 2026-09-09 this test was `#[cfg(all(test, not(fcsdk)))]`,
    /// so on the ONE box that had the SDK it was not compiled at all. Now it compiles everywhere
    /// and asserts the right thing for the box it is on.
    #[test]
    fn a_box_without_the_shim_reports_unavailable() {
        if crate::sdk_available() {
            return; // this box HAS the shim — `crates/bridges/fxcm/tests/fxcm_live_smoke.rs` is its test
        }
        assert_eq!(
            FxcmSession::login("u", "p", "http://x", "Demo").err(),
            Some(FxcmError::Unavailable)
        );
    }

    #[test]
    fn side_codes() {
        assert_eq!(Side::Buy.code(), "B");
        assert_eq!(Side::Sell.code(), "S");
    }

    /// The two messages measured against the SDK's own sample clients on 2026-09-22, and the third
    /// class the same ORA-499 family produces. All three are [`LoginFailureClass::Reported`] and are
    /// told apart by the TEXT — so what this pins is that the text SURVIVES to the operator, which
    /// is the whole deliverable.
    #[test]
    fn a_reported_failure_shows_the_venues_own_words() {
        let dead_account = FxcmError::LoginFailed {
            class: LoginFailureClass::Reported,
            message: "User or connection doesn't exist.".into(),
        };
        let shown = dead_account.to_string();
        assert!(
            shown.contains("User or connection doesn't exist."),
            "the SDK's own text must reach the operator verbatim: {shown}"
        );
        assert!(shown.contains("the venue said"), "…and be attributed to the venue: {shown}");

        // The bad-connection-name / unreachable-host family: a different string, so an operator
        // reading the message can tell it from the one above without this crate parsing anything.
        let bad_connection = FxcmError::LoginFailed {
            class: LoginFailureClass::Reported,
            message: redact_login_text(
                "ORA-499: Unable to obtain station descriptor. HTTP request failed \
                 object='/Hosts.jsp?ID=abc&PN=NotAConnection&SN=ForexConnect&MV=5&LN=D999&AT=PLAIN' \
                 errorCode=503",
            ),
        };
        let shown = bad_connection.to_string();
        assert!(shown.contains("ORA-499"), "the SDK's code must survive: {shown}");
        assert!(shown.contains("errorCode=503"), "…and so must the HTTP code: {shown}");
        assert_ne!(shown, dead_account.to_string(), "the three classes must read differently");
    }

    /// Every class, so a loop over them cannot silently cover five of six.
    ///
    /// Held against the enum by [`pinned_label`], whose `match` is exhaustive and carries no `_`
    /// arm: a new [`LoginFailureClass`] is a COMPILE error there, and the author fixing it is
    /// looking straight at this array.
    const ALL_CLASSES: [LoginFailureClass; 6] = [
        LoginFailureClass::Reported,
        LoginFailureClass::Disconnected,
        LoginFailureClass::NoResponse,
        LoginFailureClass::SdkInitFailed,
        LoginFailureClass::TradingSessionRequired,
        LoginFailureClass::ShimPredatesErrorReporting,
    ];

    /// The six labels, spelled AGAIN, by hand, independently of [`LoginFailureClass::label`].
    ///
    /// ⚠ **This exists because the assertion it feeds could not fail, and a mutation proved it.**
    /// The old check was `assert!(shown.contains(class.label()))` — but [`FxcmError`]'s `Display`
    /// BUILDS `shown` by calling `class.label()`, so the test compared a renderer's output to its
    /// own input, and `contains("")` is true of every string. MEASURED 2026-09-22: with all six
    /// arms of `label()` emptied the test PASSED, while three sibling mutations in this file were
    /// killed. A renderer can only be checked against a SECOND, independent statement of the same
    /// fact, which is what this is.
    ///
    /// It is the one hand copy in this crate that is the point rather than the hazard — and it is
    /// not unguarded either: `crates/vike-ops/tests/fxcm_login_triage_gate.rs` holds
    /// `docs/ops/fxcm-forexconnect.md`'s operator triage table against the SAME arms, so the code,
    /// this pin and the page are three spellings that cannot drift apart in silence.
    ///
    /// The `match` is exhaustive and has NO catch-all: that is the only mechanism by which a pin
    /// keeps up with the enum it pins.
    fn pinned_label(class: LoginFailureClass) -> &'static str {
        match class {
            LoginFailureClass::Reported => "the venue named a reason",
            LoginFailureClass::Disconnected => "disconnected with no reason given",
            LoginFailureClass::NoResponse => "no answer from ForexConnect",
            LoginFailureClass::SdkInitFailed => "the ForexConnect SDK did not initialise",
            LoginFailureClass::TradingSessionRequired => "a trading session must be selected",
            LoginFailureClass::ShimPredatesErrorReporting => {
                "this box's shim predates login error reporting"
            }
        }
    }

    /// Every class NAMES itself, in the words [`pinned_label`] pins — including the two the shim
    /// used to answer with a 30-second silence, and the one an OLD installed shim gives.
    #[test]
    fn every_login_failure_class_says_what_it_is() {
        for class in ALL_CLASSES {
            let expected = pinned_label(class);
            assert!(!expected.is_empty(), "{class:?} must have a non-empty label to render");
            assert_eq!(
                class.label(),
                expected,
                "{class:?}'s label moved. It is read by an operator mid-incident and copied into \
                 `docs/ops/fxcm-forexconnect.md`'s triage table, so changing it is a three-place \
                 edit: here, `label()`, and that page."
            );

            // ⚠ `Reported` is the ONE class whose `Display` deliberately omits the label: the
            // venue's own words lead instead, which is the whole deliverable. Its rendering is
            // asserted by `a_reported_failure_shows_the_venues_own_words`.
            if class == LoginFailureClass::Reported {
                continue;
            }
            let shown = FxcmError::LoginFailed { class, message: "detail".into() }.to_string();
            assert!(shown.contains(expected), "{class:?} must render its label: {shown}");
            assert!(shown.contains("detail"), "{class:?} must carry its message: {shown}");
        }
    }

    /// No two classes read the same, so an operator who has the label has the class.
    ///
    /// The equality above catches any divergence between `label()` and the pin, and it is blind to
    /// exactly one thing: a COLLISION introduced on both sides at once. Two classes given the same
    /// sentence satisfy six equality checks and leave a log line that names an answer without
    /// identifying it, which is the whole failure this diagnostic exists to end.
    ///
    /// The loop's own coverage is held by [`ALL_CLASSES`]'s declared length, not by a count here —
    /// a `seen.len()` assertion after an unconditional `push` is true by construction, which is
    /// the shape of assertion this file has already been caught writing once.
    #[test]
    fn no_two_login_failure_classes_read_the_same() {
        let mut seen: Vec<&str> = Vec::new();
        for class in ALL_CLASSES {
            let label = class.label();
            assert!(
                !seen.contains(&label),
                "{class:?} renders `{label}`, which another class already renders — an operator \
                 reading a log line could not tell them apart"
            );
            seen.push(label);
        }
    }

    /// The shim's integer classes map one-for-one, and an UNKNOWN code — a NEWER shim under an
    /// older binary — degrades to "no answer" rather than being folded into a class it is not.
    #[test]
    fn an_unknown_class_code_is_not_silently_adopted() {
        assert_eq!(LoginFailureClass::from_code(1), LoginFailureClass::Reported);
        assert_eq!(LoginFailureClass::from_code(2), LoginFailureClass::Disconnected);
        assert_eq!(LoginFailureClass::from_code(3), LoginFailureClass::NoResponse);
        assert_eq!(LoginFailureClass::from_code(4), LoginFailureClass::SdkInitFailed);
        assert_eq!(LoginFailureClass::from_code(5), LoginFailureClass::TradingSessionRequired);
        for unknown in [0, 6, 99, -1] {
            assert_eq!(
                LoginFailureClass::from_code(unknown),
                LoginFailureClass::NoResponse,
                "class {unknown} is not a class this build knows"
            );
        }
    }

    /// The login NAME never reaches a log line or a journalled reject reason, and the parts that
    /// make the message diagnosable always do.
    #[test]
    fn the_login_name_is_redacted_out_of_a_quoted_request_url() {
        let raw = "ORA-499: Unable to obtain station descriptor. HTTP request failed \
                   object='/Hosts.jsp?ID=nonce&PN=Demo&SN=ForexConnect&MV=5&LN=D000000000&AT=PLAIN' \
                   errorCode=503";
        let red = redact_login_text(raw);
        assert!(!red.contains("D000000000"), "the login name must not survive: {red}");
        assert!(!red.contains("LN="), "nor the parameter that carries it: {red}");
        assert!(red.contains("object='/Hosts.jsp"), "the request PATH must survive: {red}");
        assert!(red.contains("errorCode=503"), "and so must the HTTP code: {red}");
        assert!(red.contains("ORA-499"), "and the SDK's own code: {red}");
        assert!(red.contains(REDACTED_QUERY), "the cut must be visible, not silent: {red}");

        // A TRUNCATED buffer has no closing quote, and the query is exactly what a truncation
        // leaves behind — so the redaction has to fire there too rather than fall through.
        let cut = "HTTP request failed object='/Hosts.jsp?ID=nonce&LN=D2511";
        let red = redact_login_text(cut);
        assert!(!red.contains("D2511"), "a truncated URL must still be redacted: {red}");
        assert!(red.contains("object='/Hosts.jsp"), "…keeping the path: {red}");

        // A message with no URL in it — every OTHER login failure — is passed through untouched.
        let plain = "User or connection doesn't exist.";
        assert_eq!(redact_login_text(plain), plain);
    }
}
