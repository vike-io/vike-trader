//! The `BrokerVTableAbi` thunks: the `extern "C"` frames a strategy file calls back into the host through.

use std::ffi::c_void;

use vike_model::OrderBroker;

use crate::abi::{
    BarAbi, GridOptionAbi, OptionAbi, OrderPlacementAbi, StrAbi, TradeClosedRowAbi,
    order_list_of_json, read_borrowed_str,
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

/// Read a borrowed `(ptr, len)` pair into an owned `String`, checked rather than lossy — the same
/// argument `BarAbi::to_bar`'s doc comment makes: bytes crossing this ABI are supposed to be an
/// exact copy of a Rust `&str`'s bytes, so a decode failure is a CONTRACT VIOLATION worth a loud
/// panic rather than a quietly-corrupted symbol. That panic is only safe to take because every
/// call site below runs inside [`guarded`] — never call this outside one. The unsafe read itself
/// is `abi.rs`'s `read_borrowed_str`, the crate's one spelling of it.
fn read_str(ptr: *const u8, len: usize) -> String {
    read_borrowed_str(ptr, len, "symbol")
}

// ---- the `OrderBroker` reads, one instantiation per concrete B a caller monomorphizes ----

pub(super) extern "C" fn t_position<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> f64 {
    guarded("OrderBroker::position", 0.0, || broker_mut!(ctx, B).position(&read_str(s, n)))
}
pub(super) extern "C" fn t_price<B: OrderBroker>(ctx: *mut c_void, s: *const u8, n: usize) -> f64 {
    guarded("OrderBroker::price", 0.0, || broker_mut!(ctx, B).price(&read_str(s, n)))
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
pub(super) extern "C" fn t_bars_len<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> usize {
    guarded("OrderBroker::bars", 0, || broker_mut!(ctx, B).bars(&read_str(s, n)).len())
}
pub(super) extern "C" fn t_bar_at<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    i: usize,
    out: *mut BarAbi,
) -> bool {
    guarded("OrderBroker::bars", false, || {
        let symbol = read_str(s, n);
        match broker_mut!(ctx, B).bars(&symbol).get(i) {
            Some(bar) => {
                // SAFETY: `out` is the live, aligned `*mut BarAbi` the guest passed for exactly this
                // call — the contract `BrokerVTableAbi::bar_at`'s own doc states.
                unsafe { *out = BarAbi::from_bar(bar) };
                true
            }
            None => false,
        }
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
        broker_mut!(ctx, B).quote_vwap(&read_str(s, n), side, qty).into()
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
        broker_mut!(ctx, B).depth_within_price(&read_str(s, n), side, limit_px)
    })
}
pub(super) extern "C" fn t_contract_value<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> crate::abi::ContractValueOptionAbi {
    guarded("OrderBroker::contract_value", crate::abi::ContractValueOptionAbi::from(None), || {
        broker_mut!(ctx, B).contract_value(&read_str(s, n)).into()
    })
}
pub(super) extern "C" fn t_instrument_grid<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> GridOptionAbi {
    guarded("OrderBroker::instrument_grid", GridOptionAbi::from(None), || {
        broker_mut!(ctx, B).instrument_grid(&read_str(s, n)).into()
    })
}

// The input history and the closed-trade ledger: each a count and an item at an index, answered by
// value. An item past the count is `None`, and a broker keeping no ledger answers `None` for its
// count, never an empty ledger.

pub(super) extern "C" fn t_input_bars_len<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> usize {
    guarded("OrderBroker::input_bars", 0, || {
        broker_mut!(ctx, B).input_bars(&read_borrowed_str(s, n, "input name")).len()
    })
}
pub(super) extern "C" fn t_input_bar_at<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
    i: usize,
) -> OptionAbi<BarAbi> {
    guarded("OrderBroker::input_bars", OptionAbi::new(None, BarAbi::empty()), || {
        let name = read_borrowed_str(s, n, "input name");
        let bar = broker_mut!(ctx, B).input_bars(&name).get(i).map(BarAbi::from_bar);
        OptionAbi::new(bar, BarAbi::empty())
    })
}
pub(super) extern "C" fn t_closed_trades_len<B: OrderBroker>(ctx: *mut c_void) -> OptionAbi<usize> {
    guarded("OrderBroker::closed_trades", OptionAbi::new(None, 0), || {
        OptionAbi::new(broker_mut!(ctx, B).closed_trades().map(<[_]>::len), 0)
    })
}
pub(super) extern "C" fn t_closed_trade_at<B: OrderBroker>(
    ctx: *mut c_void,
    i: usize,
) -> OptionAbi<TradeClosedRowAbi> {
    guarded("OrderBroker::closed_trades", OptionAbi::new(None, TradeClosedRowAbi::empty()), || {
        let row = broker_mut!(ctx, B).closed_trades().and_then(|rows| rows.get(i));
        OptionAbi::new(row.map(TradeClosedRowAbi::of), TradeClosedRowAbi::empty())
    })
}

// ---- the `OrderBroker` verbs, bound `B: OrderBroker` ----
//
// Each order verb hands the host's broker the symbol the strategy named (`""`: the mount's own),
// as a compiled-in strategy's call does (`ABI_VERSION` 8).
//
// ⚠ An order the host cannot decode (an unknown kind code, an order list that is not the JSON of
// `Vec<(String, OrderListLeg)>`) PANICS here, inside `guarded`, so the dispatch is poisoned and
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
        broker_mut!(ctx, B).place(&symbol, &tag, side, qty, order_of(&order), reduce_only);
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
        broker_mut!(ctx, B).place_exit_leg(&symbol, &exit, &tag, side, qty, order_of(&order));
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
        broker_mut!(ctx, B).place_at_close(&symbol, &tag, side, qty, reduce_only);
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
        broker_mut!(ctx, B).place_close_from_fill(&symbol, &tag, side, qty, level);
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
    s: *const u8,
    n: usize,
) {
    guarded("OrderBroker::place_group", (), || {
        let symbol = read_str(ss, sn);
        let json = read_borrowed_str(s, n, "order list");
        let legs = order_list_of_json(&json).unwrap_or_else(|e| {
            panic!(
                "strategy file ABI contract violated: the order list is not this host's JSON: {e}"
            )
        });
        broker_mut!(ctx, B).place_group(&symbol, legs);
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
        broker_mut!(ctx, B).amend(&tag, new_qty.into(), new_price.into(), new_trigger.into());
    });
}
pub(super) extern "C" fn t_cancel<B: OrderBroker>(ctx: *mut c_void, s: *const u8, n: usize) {
    guarded("OrderBroker::cancel", (), || broker_mut!(ctx, B).cancel(&read_str(s, n)));
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
        broker_mut!(ctx, B).deny(&symbol, &tag, qty, &reason);
    });
}
pub(super) extern "C" fn t_avg_price<B: OrderBroker>(
    ctx: *mut c_void,
    s: *const u8,
    n: usize,
) -> f64 {
    guarded("OrderBroker::average_price", 0.0, || {
        broker_mut!(ctx, B).average_price(&read_str(s, n))
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
