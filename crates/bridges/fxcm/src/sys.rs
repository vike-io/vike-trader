//! FXCM ForexConnect FFI — native Rust bindings + a C ABI shim (`shim/fcshim.cpp`).
//!
//! Pure Rust + a C ABI shim (`shim/fcshim.cpp`) over the C++ SDK — NO Python anywhere.
//! Rescued from the proven browserless login+place+cancel POC (ran a live demo order on
//! acct D251112911, 2026-07-03). This module is the raw FFI + a thin safe wrapper; the
//! vike-model mapping (→ `ExecutionClient`) lives in the sibling `exec` module.
//!
//! When the ForexConnect SDK is absent (see `build.rs`), every call returns
//! [`FxcmError::Unavailable`] so the workspace still builds on CI / fresh clones.

// The FFI surface below (`cstr`, `Side::code`, `PlacedOrder::rate`, the `LoginFailed`/`Native`
// error variants, `FxcmSession::account`) is exercised only in the real-SDK build (`#[cfg(fcsdk)]`).
// In the stub build (no ForexConnect SDK — e.g. CI, a fresh clone) those items are legitimately
// unused; allow it here rather than threading per-item cfg gates through the whole shim. As a
// standalone `-sys` crate these were public API (never dead); folding into this venue bridge crate
// makes them module-private, so the stub build now sees them as dead code.
#![allow(dead_code)]

use std::ffi::CString;

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
    pub rate: f64,
}

/// Errors from the FXCM native layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FxcmError {
    /// Built without the ForexConnect SDK (stub build).
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
                write!(f, "FXCM ForexConnect SDK not built in (stub build; set FCSDK_DIR)")
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

#[cfg(fcsdk)]
mod ffi {
    use std::ffi::{c_char, c_void};
    extern "C" {
        pub fn fc_login(
            user: *const c_char,
            pw: *const c_char,
            url: *const c_char,
            conn: *const c_char,
        ) -> *mut c_void;
        pub fn fc_account(
            h: *mut c_void,
            acct_out: *mut c_char,
            acct_len: i32,
            bal_out: *mut f64,
        ) -> i32;
        // The instrument's base unit size on the tradable account — the multiplier `fc_place`
        // applies to the lot count. Read-only; 0 on success with `out` filled, <0 on a bad handle /
        // missing account or provider / a non-positive figure (with `err_out` filled).
        pub fn fc_base_unit_size(
            h: *mut c_void,
            instrument: *const c_char,
            out: *mut i32,
            err_out: *mut c_char,
            err_len: i32,
        ) -> i32;
        #[allow(clippy::too_many_arguments)]
        pub fn fc_place_entry(
            h: *mut c_void,
            instrument: *const c_char,
            buysell: *const c_char,
            pips_away: i32,
            lots: i32,
            order_id_out: *mut c_char,
            oid_len: i32,
            offer_id_out: *mut c_char,
            ofid_len: i32,
            acct_out: *mut c_char,
            acct_len: i32,
            rate_out: *mut f64,
            err_out: *mut c_char,
            err_len: i32,
        ) -> i32;
        #[allow(clippy::too_many_arguments)]
        pub fn fc_place_market(
            h: *mut c_void,
            instrument: *const c_char,
            buysell: *const c_char,
            lots: i32,
            order_id_out: *mut c_char,
            oid_len: i32,
            offer_id_out: *mut c_char,
            ofid_len: i32,
            acct_out: *mut c_char,
            acct_len: i32,
            err_out: *mut c_char,
            err_len: i32,
        ) -> i32;
        pub fn fc_delete_order(
            h: *mut c_void,
            order_id: *const c_char,
            account_id: *const c_char,
            offer_id: *const c_char,
            err_out: *mut c_char,
            err_len: i32,
        ) -> i32;
        // Drain ONE pending async order event (fill/cancel/reject) enqueued by the shim's
        // ForexConnect response listener. Returns 1 and fills `out` with the event JSON, 0 when the
        // queue is empty, <0 on error. The shim's queue is mutex-guarded, so this is the async
        // fill lane (audit A3: after a reconnect the listener re-surfaces current trades → they
        // drain here and the core dedups by trade_id).
        pub fn fc_poll_event(h: *mut c_void, out: *mut c_char, out_len: i32) -> i32;
        // Reconcile table snapshots (read-only): fill `out` with a JSON array of the current
        // Orders / Trades table rows and return the FULL length needed (excl. NUL). A return
        // >= `out_len` means the buffer was too small — grow to the returned size and call again
        // (the `snapshot` wrapper's retry). <0 on a bad handle / missing factory.
        pub fn fc_orders(h: *mut c_void, out: *mut c_char, out_len: i32) -> i32;
        pub fn fc_trades(h: *mut c_void, out: *mut c_char, out_len: i32) -> i32;
        pub fn fc_logout(h: *mut c_void);
    }
}

/// Read a shim-filled C string buffer back into an owned `String`.
#[cfg(fcsdk)]
fn rd(b: &[std::ffi::c_char]) -> String {
    unsafe { std::ffi::CStr::from_ptr(b.as_ptr()).to_string_lossy().into_owned() }
}

#[cfg(fcsdk)]
const BUF: usize = 128;
#[cfg(fcsdk)]
const ERRBUF: usize = 256;

/// A live ForexConnect session. Dropping it logs out.
pub struct FxcmSession {
    #[cfg(fcsdk)]
    h: *mut std::ffi::c_void,
}

impl FxcmSession {
    /// Log in to FXCM.
    ///
    /// - `url`: host discovery URL, e.g. `http://www.fxcorporate.com/Hosts.jsp`
    /// - `conn`: connection name — `"Demo"` or `"Real"`
    #[cfg(fcsdk)]
    pub fn login(user: &str, pw: &str, url: &str, conn: &str) -> Result<Self, FxcmError> {
        let (u, p, ur, c) = (cstr(user)?, cstr(pw)?, cstr(url)?, cstr(conn)?);
        let h = unsafe { ffi::fc_login(u.as_ptr(), p.as_ptr(), ur.as_ptr(), c.as_ptr()) };
        if h.is_null() {
            return Err(FxcmError::LoginFailed);
        }
        Ok(Self { h })
    }

    /// Stub: FXCM was compiled out (no ForexConnect SDK).
    #[cfg(not(fcsdk))]
    pub fn login(_user: &str, _pw: &str, _url: &str, _conn: &str) -> Result<Self, FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// `(account_id, balance)` of the first tradable (non-margin-call) account.
    #[cfg(fcsdk)]
    pub fn account(&self) -> Result<(String, f64), FxcmError> {
        let mut acct = vec![0 as std::ffi::c_char; BUF];
        let mut bal = 0f64;
        let rc = unsafe { ffi::fc_account(self.h, acct.as_mut_ptr(), acct.len() as i32, &mut bal) };
        if rc != 0 {
            return Err(FxcmError::Native { code: rc, message: "fc_account failed".into() });
        }
        Ok((rd(&acct), bal))
    }

    #[cfg(not(fcsdk))]
    pub fn account(&self) -> Result<(String, f64), FxcmError> {
        Err(FxcmError::Unavailable)
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
    #[cfg(fcsdk)]
    pub fn base_unit_size(&self, instrument: &str) -> Result<i32, FxcmError> {
        let instr = cstr(instrument)?;
        let mut out = 0i32;
        let mut err = vec![0 as std::ffi::c_char; ERRBUF];
        let rc = unsafe {
            ffi::fc_base_unit_size(
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

    /// Stub: no ForexConnect SDK → no session to ask, so no size can be converted. The exec loop
    /// turns this into a terminal `OrderRejected` rather than guessing a multiplier, which is the
    /// same no-silent-substitution rule the rest of the preflight follows.
    #[cfg(not(fcsdk))]
    pub fn base_unit_size(&self, _instrument: &str) -> Result<i32, FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// Place a resting LIMIT entry `pips_away` from market (buy below ask / sell above bid) so it
    /// does not immediately fill. Returns the ids required to cancel it.
    #[cfg(fcsdk)]
    pub fn place_limit_entry(
        &self,
        instrument: &str,
        side: Side,
        pips_away: i32,
        lots: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let instr = cstr(instrument)?;
        let bs = cstr(side.code())?;
        let mut oid = vec![0 as std::ffi::c_char; BUF];
        let mut ofid = vec![0 as std::ffi::c_char; BUF];
        let mut aid = vec![0 as std::ffi::c_char; BUF];
        let mut err = vec![0 as std::ffi::c_char; ERRBUF];
        let mut rate = 0f64;
        let rc = unsafe {
            ffi::fc_place_entry(
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

    #[cfg(not(fcsdk))]
    pub fn place_limit_entry(
        &self,
        _instrument: &str,
        _side: Side,
        _pips_away: i32,
        _lots: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// Place an immediately-executing TRUE MARKET order (ForexConnect `O2G2::Orders::TrueMarketOpen`).
    /// Unlike [`Self::place_limit_entry`] this is expected to FILL; the returned [`PlacedOrder`]
    /// still carries the ids (a market order can still be canceled while unfilled, and the venue
    /// order id routes the async fill back to its client order id), but its `rate` is `0.0` — the
    /// execution price arrives with the fill on the async event lane, not from the placement call.
    #[cfg(fcsdk)]
    pub fn place_market(
        &self,
        instrument: &str,
        side: Side,
        lots: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        let instr = cstr(instrument)?;
        let bs = cstr(side.code())?;
        let mut oid = vec![0 as std::ffi::c_char; BUF];
        let mut ofid = vec![0 as std::ffi::c_char; BUF];
        let mut aid = vec![0 as std::ffi::c_char; BUF];
        let mut err = vec![0 as std::ffi::c_char; ERRBUF];
        let rc = unsafe {
            ffi::fc_place_market(
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

    #[cfg(not(fcsdk))]
    pub fn place_market(
        &self,
        _instrument: &str,
        _side: Side,
        _lots: i32,
    ) -> Result<PlacedOrder, FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// Cancel a resting order (ids come from [`PlacedOrder`]).
    #[cfg(fcsdk)]
    pub fn delete_order(
        &self,
        order_id: &str,
        account_id: &str,
        offer_id: &str,
    ) -> Result<(), FxcmError> {
        let (o, a, f) = (cstr(order_id)?, cstr(account_id)?, cstr(offer_id)?);
        let mut err = vec![0 as std::ffi::c_char; ERRBUF];
        let rc = unsafe {
            ffi::fc_delete_order(
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

    #[cfg(not(fcsdk))]
    pub fn delete_order(
        &self,
        _order_id: &str,
        _account_id: &str,
        _offer_id: &str,
    ) -> Result<(), FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// Drain the next pending async order event (fill/cancel/reject) as a JSON string, or `None`
    /// when the shim's queue is empty. The exec thread polls this between commands and maps each
    /// event via [`super::event_mapper::map_fxcm_event`]. Safe to call repeatedly until it returns `None`.
    #[cfg(fcsdk)]
    pub fn poll_event(&self) -> Result<Option<String>, FxcmError> {
        let mut buf = vec![0 as std::ffi::c_char; 1024];
        let rc = unsafe { ffi::fc_poll_event(self.h, buf.as_mut_ptr(), buf.len() as i32) };
        match rc {
            0 => Ok(None),
            1 => Ok(Some(rd(&buf))),
            code => Err(FxcmError::Native { code, message: "fc_poll_event failed".into() }),
        }
    }

    /// Stub: no ForexConnect SDK → no async events (the exec drain loop is a no-op).
    #[cfg(not(fcsdk))]
    pub fn poll_event(&self) -> Result<Option<String>, FxcmError> {
        Ok(None)
    }

    /// Read one reconcile table snapshot (`fc_orders` / `fc_trades`) as a JSON array string, growing
    /// the buffer once if the first read reports it was too small (the shim returns the FULL length
    /// needed). Read-only; used by the reconcile session thread ([`super::recon_client`]).
    #[cfg(fcsdk)]
    fn snapshot(
        &self,
        f: unsafe extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_char, i32) -> i32,
    ) -> Result<String, FxcmError> {
        let mut cap: usize = 8192;
        loop {
            let mut buf = vec![0 as std::ffi::c_char; cap];
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
    #[cfg(fcsdk)]
    pub fn orders_json(&self) -> Result<String, FxcmError> {
        self.snapshot(ffi::fc_orders)
    }

    /// Stub: no ForexConnect SDK → no table to snapshot.
    #[cfg(not(fcsdk))]
    pub fn orders_json(&self) -> Result<String, FxcmError> {
        Err(FxcmError::Unavailable)
    }

    /// The current Trades table (open positions) as a JSON array string — reconcile derives both the
    /// net position report and the per-open-trade fill report from it. See
    /// [`super::recon_client::parse_positions`] / [`super::recon_client::parse_fills`].
    #[cfg(fcsdk)]
    pub fn trades_json(&self) -> Result<String, FxcmError> {
        self.snapshot(ffi::fc_trades)
    }

    /// Stub: no ForexConnect SDK → no table to snapshot.
    #[cfg(not(fcsdk))]
    pub fn trades_json(&self) -> Result<String, FxcmError> {
        Err(FxcmError::Unavailable)
    }
}

#[cfg(fcsdk)]
impl Drop for FxcmSession {
    fn drop(&mut self) {
        unsafe { ffi::fc_logout(self.h) };
    }
}

// The ForexConnect session is a single-owner native handle; it is not safe to share across
// threads. `FxcmSession` is deliberately neither `Send` nor `Sync` (raw pointer field).

#[cfg(all(test, not(fcsdk)))]
mod tests {
    use super::*;

    #[test]
    fn stub_calls_report_unavailable() {
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
