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
//! buy is that a MISSING symbol is now a diagnostic at load — the loader resolves all ten before it
//! returns — rather than a crash on the first call that needed the one a stale shim lacked.

use crate::loader::Shim;
use std::ffi::{CString, c_char, c_void};

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
    /// `fc_login` returned null (bad creds / host unreachable).
    LoginFailed,
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
            FxcmError::LoginFailed => write!(f, "FXCM login failed"),
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
        // SAFETY: four valid NUL-terminated strings, alive across the call. The shim copies what it
        // needs; nothing it returns borrows them.
        let h = unsafe { (shim.fc_login)(u.as_ptr(), p.as_ptr(), ur.as_ptr(), c.as_ptr()) };
        if h.is_null() {
            return Err(FxcmError::LoginFailed);
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
}
