//! The `BrokerVTable` thunks: the `extern "C"` frames a plugin calls back into the host through.

use std::ffi::c_void;

use vike_model::{Broker, HftBroker};

use crate::abi::CBar;

use super::guarded;

/// Recover the concrete `&mut B` a generic thunk's erased `ctx` was built from.
///
/// A macro rather than a generic helper FUNCTION deliberately: a safe fn of shape
/// `fn f<'a, B>(ctx: *mut c_void) -> &'a mut B` would let its caller pick ANY `'a` it likes
/// (lifetime laundering) — the classic unsound-helper shape. Expanding inline at each call site
/// keeps the borrow tied to that call's own stack frame, the same reason `crates/bridges/fxcm`'s
/// `bind!` macro (`src/loader.rs`) exists rather than a same-shaped function.
macro_rules! broker_mut {
    ($ctx:expr, $B:ty) => {{
        // SAFETY: every caller of a generic thunk below builds `$ctx` as `broker as *mut $B as
        // *mut c_void` in the SAME call frame that then invokes the plugin (`PluginStrategy`'s
        // trait methods, further down) — a live, exclusively-borrowed `&mut $B` for exactly the
        // duration of the plugin call currently unwinding back through this thunk. The plugin
        // ABI is synchronous, so no other reference to it can be alive concurrently.
        unsafe { &mut *($ctx as *mut $B) }
    }};
}

/// Read a borrowed `(ptr, len)` pair into an owned `String`, checked rather than lossy — the same
/// argument `CBar::to_bar`'s doc comment makes: bytes crossing this ABI are supposed to be an
/// exact copy of a Rust `&str`'s bytes, so a decode failure is a CONTRACT VIOLATION worth a loud
/// panic rather than a quietly-corrupted symbol. That panic is only safe to take because every
/// call site below runs inside [`guarded`] — never call this outside one.
fn read_str(ptr: *const u8, len: usize) -> String {
    if len == 0 {
        return String::new();
    }
    // SAFETY: `(ptr, len)` is the pair the ABI contract requires — borrowed for the duration of
    // this call only, from a valid `&str`'s bytes (`abi.rs`'s module doc states the contract).
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf8(bytes.to_vec())
        .expect("plugin ABI contract violated: symbol bytes are not valid UTF-8")
}

// ---- the eleven BrokerVTable thunks, one instantiation per concrete B a caller monomorphizes ----

pub(super) extern "C" fn t_submit_market<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_market(&read_str(s, n), side, qty));
}
pub(super) extern "C" fn t_submit_limit<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    price: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_limit(&read_str(s, n), side, qty, price));
}
pub(super) extern "C" fn t_position<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).position(&read_str(s, n)))
}
pub(super) extern "C" fn t_price<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).price(&read_str(s, n)))
}
pub(super) extern "C" fn t_equity<B: Broker>(ctx: *mut c_void) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).equity())
}
pub(super) extern "C" fn t_index<B: Broker>(ctx: *mut c_void) -> usize {
    guarded(0, || broker_mut!(ctx, B).index())
}
pub(super) extern "C" fn t_now<B: Broker>(ctx: *mut c_void) -> i64 {
    guarded(0, || broker_mut!(ctx, B).now())
}
pub(super) extern "C" fn t_bars_len<B: Broker>(ctx: *mut c_void, s: *const u8, n: usize) -> usize {
    guarded(0, || broker_mut!(ctx, B).bars(&read_str(s, n)).len())
}
pub(super) extern "C" fn t_bar_at<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    i: usize,
    out: *mut CBar,
) -> bool {
    guarded(false, || {
        let symbol = read_str(s, n);
        match broker_mut!(ctx, B).bars(&symbol).get(i) {
            Some(bar) => {
                // SAFETY: `out` is the live, aligned `*mut CBar` the guest passed for exactly this
                // call — the contract `BrokerVTable::bar_at`'s own doc states.
                unsafe { *out = CBar::from_bar(bar) };
                true
            }
            None => false,
        }
    })
}
pub(super) extern "C" fn t_quote_vwap<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) -> crate::abi::COptF64 {
    guarded(crate::abi::COptF64::from(None), || {
        broker_mut!(ctx, B).quote_vwap(&read_str(s, n), side, qty).into()
    })
}
pub(super) extern "C" fn t_depth_within_price<B: Broker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    limit_px: f64,
) -> f64 {
    guarded(0.0, || broker_mut!(ctx, B).depth_within_price(&read_str(s, n), side, limit_px))
}

// ---- the four HftBroker thunks (ABI_VERSION 2) — bound `B: HftBroker`, unlike the eleven above ----

pub(super) extern "C" fn t_hft_position<B: HftBroker>(ctx: *mut c_void) -> f64 {
    // UFCS: `Broker::position` and `HftBroker::position` share a name (see this module's header).
    guarded(0.0, || HftBroker::position(broker_mut!(ctx, B)))
}
pub(super) extern "C" fn t_submit_limit_tagged<B: HftBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    price: f64,
) {
    guarded((), || broker_mut!(ctx, B).submit_limit_tagged(&read_str(s, n), side, qty, price));
}
pub(super) extern "C" fn t_modify_tagged<B: HftBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    new_qty: crate::abi::COptF64,
    new_price: crate::abi::COptF64,
) {
    guarded((), || {
        broker_mut!(ctx, B).modify_tagged(&read_str(s, n), new_qty.into(), new_price.into());
    });
}
pub(super) extern "C" fn t_cancel_tagged<B: HftBroker>(ctx: *mut c_void, s: *const u8, n: usize) {
    guarded((), || broker_mut!(ctx, B).cancel_tagged(&read_str(s, n)));
}
