//! The `BrokerVTableAbi` thunks: the `extern "C"` frames a strategy file calls back into the host through.

use std::ffi::c_void;

use vike_model::OrderBroker;

use crate::abi::{
    GridOptionAbi, LentBarAbi, LentTradeClosedRowAbi, NameOrIndexAbi, OptionAbi, OrderListLegAbi,
    OrderPlacementAbi, SliceAbi, StrAbi, order_list_of, read_borrowed_str,
};

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
        // *mut c_void` in the SAME call frame that then invokes the strategy file (`StrategyFileHost`'s
        // trait methods, further down) — a live, exclusively-borrowed `&mut $B` for exactly the
        // duration of the strategy file call currently unwinding back through this thunk. The strategy file
        // ABI is synchronous, so no other reference to it can be alive concurrently.
        unsafe { &mut *($ctx as *mut $B) }
    }};
}

/// Read a borrowed `(ptr, len)` pair as the `&str` it is, for the thunk's call only (no copy: a
/// symbol is read on every thunk call), checked rather than lossy: bytes crossing this ABI are
/// supposed to be an exact copy of a Rust `&str`'s bytes, so a decode failure is a CONTRACT VIOLATION worth a loud
/// panic rather than a quietly-corrupted symbol. That panic is only safe to take because every
/// call site below runs inside [`guarded`] — never call this outside one. The unsafe read itself
/// is `abi.rs`'s `read_borrowed_str`, the crate's one spelling of it.
fn read_str<'a>(ptr: *const u8, len: usize) -> &'a str {
    read_borrowed_str(ptr, len, "symbol")
}

/// A read's answer for the symbol or input `key` names (`ABI_VERSION` 15): through the trait's `_at`
/// read `by_index` for an index, through its named read `by_name` for a text (checked, [`read_str`]'s
/// posture, under the read's own `what`). The trait holds the two equal, so this picks only the cost.
fn read_by<'b, B, T>(
    broker: &'b B,
    key: NameOrIndexAbi,
    what: &str,
    by_index: impl FnOnce(&'b B, usize) -> T,
    by_name: impl FnOnce(&'b B, &str) -> T,
) -> T {
    match key.get() {
        Some(index) => by_index(broker, index),
        None => by_name(broker, read_borrowed_str(key.text.ptr, key.text.len, what)),
    }
}

// ---- the `OrderBroker` reads, one instantiation per concrete B a caller monomorphizes ----

pub(super) extern "C" fn t_position<B: OrderBroker>(ctx: *mut c_void, s: NameOrIndexAbi) -> f64 {
    guarded("OrderBroker::position", 0.0, || {
        read_by(&*broker_mut!(ctx, B), s, "symbol", B::position_at, B::position)
    })
}
pub(super) extern "C" fn t_price<B: OrderBroker>(ctx: *mut c_void, s: NameOrIndexAbi) -> f64 {
    guarded("OrderBroker::price", 0.0, || {
        read_by(&*broker_mut!(ctx, B), s, "symbol", B::price_at, B::price)
    })
}
pub(super) extern "C" fn t_equity<B: OrderBroker>(ctx: *mut c_void) -> f64 {
    guarded("OrderBroker::equity", 0.0, || broker_mut!(ctx, B).equity())
}
pub(super) extern "C" fn t_index<B: OrderBroker>(ctx: *mut c_void) -> usize {
    guarded("OrderBroker::index", 0, || broker_mut!(ctx, B).index())
}
pub(super) extern "C" fn t_now<B: OrderBroker>(ctx: *mut c_void) -> i64 {
    guarded("OrderBroker::now", 0, || broker_mut!(ctx, B).now())
}
/// `OrderBroker::bars`, lent: the host's own bars, valid for the dispatch (`ABI_VERSION` 14).
pub(super) extern "C" fn t_bars<B: OrderBroker>(
    ctx: *mut c_void,
    s: NameOrIndexAbi,
) -> SliceAbi<LentBarAbi> {
    guarded("OrderBroker::bars", SliceAbi::empty(), || {
        LentBarAbi::lend_all(read_by(&*broker_mut!(ctx, B), s, "symbol", B::bars_at, B::bars))
    })
}
pub(super) extern "C" fn t_quote_vwap<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
) -> OptionAbi<f64> {
    guarded("OrderBroker::quote_vwap", OptionAbi::<f64>::from(None), || {
        broker_mut!(ctx, B).quote_vwap(read_str(s, n), side, qty).into()
    })
}
pub(super) extern "C" fn t_depth_within_price<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    side: i32,
    limit_px: f64,
) -> f64 {
    guarded("OrderBroker::depth_within_price", 0.0, || {
        broker_mut!(ctx, B).depth_within_price(read_str(s, n), side, limit_px)
    })
}
pub(super) extern "C" fn t_contract_value<B: OrderBroker>(
    ctx: *mut c_void,
    s: NameOrIndexAbi,
) -> crate::abi::ContractValueOptionAbi {
    guarded("OrderBroker::contract_value", crate::abi::ContractValueOptionAbi::from(None), || {
        let b = &*broker_mut!(ctx, B);
        read_by(b, s, "symbol", B::contract_value_at, B::contract_value).into()
    })
}
pub(super) extern "C" fn t_instrument_grid<B: OrderBroker>(
    ctx: *mut c_void,
    s: NameOrIndexAbi,
) -> GridOptionAbi {
    guarded("OrderBroker::instrument_grid", GridOptionAbi::from(None), || {
        let b = &*broker_mut!(ctx, B);
        read_by(b, s, "symbol", B::instrument_grid_at, B::instrument_grid).into()
    })
}

// The input history and the closed-trade ledger, lent as the bars are. A broker keeping no ledger, or
// serving no whole series, answers `None`, never an empty slice.

pub(super) extern "C" fn t_input_bars<B: OrderBroker>(
    ctx: *mut c_void,
    s: NameOrIndexAbi,
) -> SliceAbi<LentBarAbi> {
    guarded("OrderBroker::input_bars", SliceAbi::empty(), || {
        let b = &*broker_mut!(ctx, B);
        LentBarAbi::lend_all(read_by(b, s, "input name", B::input_bars_at, B::input_bars))
    })
}
pub(super) extern "C" fn t_input_whole<B: OrderBroker>(
    ctx: *mut c_void,
    s: NameOrIndexAbi,
) -> OptionAbi<SliceAbi<LentBarAbi>> {
    guarded("OrderBroker::input_whole", OptionAbi::new(None, SliceAbi::empty()), || {
        let b = &*broker_mut!(ctx, B);
        let whole = read_by(b, s, "input name", B::input_whole_at, B::input_whole);
        OptionAbi::new(whole.map(LentBarAbi::lend_all), SliceAbi::empty())
    })
}
/// `ABI_VERSION` 13; read by the order language (`vike_tactics::Tactics::closed_net`), whose neutral
/// answer `false` only makes it fold the closed rows from the first again.
pub(super) extern "C" fn t_history_only_grows<B: OrderBroker>(ctx: *mut c_void) -> bool {
    guarded("OrderBroker::history_only_grows", false, || broker_mut!(ctx, B).history_only_grows())
}
pub(super) extern "C" fn t_closed_trades<B: OrderBroker>(
    ctx: *mut c_void,
) -> OptionAbi<SliceAbi<LentTradeClosedRowAbi>> {
    guarded("OrderBroker::closed_trades", OptionAbi::new(None, SliceAbi::empty()), || {
        let closed = broker_mut!(ctx, B).closed_trades();
        OptionAbi::new(closed.map(LentTradeClosedRowAbi::lend_all), SliceAbi::empty())
    })
}

// ---- the `OrderBroker` verbs, bound `B: OrderBroker` ----
//
// Each order verb hands the host's broker the symbol the strategy named (`""`: the mount's own),
// as a compiled-in strategy's call does (`ABI_VERSION` 8).
//
// ⚠ An order the host cannot decode (an unknown kind code, an order list leg with a code this host
// does not know) PANICS here, inside `guarded`, so the dispatch is poisoned and
// re-raised on the host's own stack: no order may silently vanish, and a guessed kind is an order
// the strategy never asked for.

fn order_of(o: &OrderPlacementAbi) -> vike_model::OrderPlacement {
    o.to_tagged_order().unwrap_or_else(|| {
        panic!(
            "strategy file ABI contract violated: order kind code {} is not one this host knows",
            o.kind
        )
    })
}

pub(super) extern "C" fn t_place<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    order: OrderPlacementAbi,
    reduce_only: bool,
) {
    guarded("OrderBroker::place", (), || {
        let (symbol, tag) = (read_str(ss, sn), read_str(s, n));
        broker_mut!(ctx, B).place(symbol, tag, side, qty, order_of(&order), reduce_only);
    });
}
pub(super) extern "C" fn t_place_exit_leg<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    es: *const u8,
    en: usize,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    order: OrderPlacementAbi,
) {
    guarded("OrderBroker::place_exit_leg", (), || {
        let (symbol, exit, tag) = (read_str(ss, sn), read_str(es, en), read_str(s, n));
        broker_mut!(ctx, B).place_exit_leg(symbol, exit, tag, side, qty, order_of(&order));
    });
}
pub(super) extern "C" fn t_place_at_close<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    reduce_only: bool,
) {
    guarded("OrderBroker::place_at_close", (), || {
        let (symbol, tag) = (read_str(ss, sn), read_str(s, n));
        broker_mut!(ctx, B).place_at_close(symbol, tag, side, qty, reduce_only);
    });
}
pub(super) extern "C" fn t_place_close_from_fill<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    s: *const u8,
    n: usize,
    side: i32,
    qty: f64,
    level: f64,
) {
    guarded("OrderBroker::place_close_from_fill", (), || {
        let (symbol, tag) = (read_str(ss, sn), read_str(s, n));
        broker_mut!(ctx, B).place_close_from_fill(symbol, tag, side, qty, level);
    });
}
pub(super) extern "C" fn t_rests_reduce_only_while_flat<B: OrderBroker>(ctx: *mut c_void) -> bool {
    guarded("OrderBroker::rests_reduce_only_while_flat", false, || {
        broker_mut!(ctx, B).rests_reduce_only_while_flat()
    })
}
pub(super) extern "C" fn t_place_group<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    legs: SliceAbi<OrderListLegAbi>,
) {
    guarded("OrderBroker::place_group", (), || {
        let symbol = read_str(ss, sn);
        let legs = order_list_of(legs).unwrap_or_else(|e| {
            panic!("strategy file ABI contract violated: the order list's {e}")
        });
        broker_mut!(ctx, B).place_group(symbol, legs);
    });
}
pub(super) extern "C" fn t_amend<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    new_qty: OptionAbi<f64>,
    new_price: OptionAbi<f64>,
    new_trigger: OptionAbi<f64>,
) {
    guarded("OrderBroker::amend", (), || {
        let tag = read_str(s, n);
        broker_mut!(ctx, B).amend(tag, new_qty.into(), new_price.into(), new_trigger.into());
    });
}
pub(super) extern "C" fn t_cancel<B: OrderBroker>(ctx: *mut c_void, s: *const u8, n: usize) {
    guarded("OrderBroker::cancel", (), || broker_mut!(ctx, B).cancel(read_str(s, n)));
}
pub(super) extern "C" fn t_deny<B: OrderBroker>(
    ctx: *mut c_void,
    ss: *const u8,
    sn: usize,
    s: *const u8,
    n: usize,
    qty: f64,
    r: *const u8,
    m: usize,
) {
    guarded("OrderBroker::deny", (), || {
        let (symbol, tag, reason) = (read_str(ss, sn), read_str(s, n), read_str(r, m));
        broker_mut!(ctx, B).deny(symbol, tag, qty, reason);
    });
}
/// The forecast is rebuilt through `vike_model::Forecast::new`, the one rule: the guest can only hold
/// one that rule accepted, so a refused term is a contract violation, a panic inside [`guarded`] as an
/// undecodable order is.
pub(super) extern "C" fn t_publish_forecast<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    value: f64,
    horizon_ms: i64,
    confidence: OptionAbi<f64>,
) {
    guarded("OrderBroker::publish_forecast", (), || {
        let symbol = read_str(s, n);
        let forecast = vike_model::Forecast::new(value, horizon_ms, confidence.into())
            .unwrap_or_else(|why| panic!("a forecast the rule refuses crossed the ABI: {why}"));
        broker_mut!(ctx, B).publish_forecast(symbol, forecast);
    });
}
pub(super) extern "C" fn t_avg_price<B: OrderBroker>(ctx: *mut c_void, s: NameOrIndexAbi) -> f64 {
    guarded("OrderBroker::average_price", 0.0, || {
        read_by(&*broker_mut!(ctx, B), s, "symbol", B::average_price_at, B::average_price)
    })
}
/// Input `i` of the host broker's input list (`OrderBroker::input_names`, `ABI_VERSION` 15), borrowed
/// as [`t_symbol_at`]'s symbol is.
pub(super) extern "C" fn t_input_name_at<B: OrderBroker>(
    ctx: *mut c_void,
    i: usize,
) -> OptionAbi<StrAbi> {
    guarded("OrderBroker::input_names", OptionAbi::new(None, StrAbi::empty()), || {
        OptionAbi::<StrAbi>::of(broker_mut!(ctx, B).input_names().get(i).map(String::as_str))
    })
}
/// Symbol `i` of the host broker's universe, borrowed from its list: it changes only between the
/// strategy file's calls, never while one runs.
pub(super) extern "C" fn t_symbol_at<B: OrderBroker>(
    ctx: *mut c_void,
    i: usize,
) -> OptionAbi<StrAbi> {
    guarded("OrderBroker::symbols", OptionAbi::new(None, StrAbi::empty()), || {
        OptionAbi::<StrAbi>::of(broker_mut!(ctx, B).symbols().get(i).map(String::as_str))
    })
}
