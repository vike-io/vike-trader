//! The pure fill-trigger: WHEN a resting order fills and at WHAT price.
//! Exact port of `core/orders.py::order_fill_price(_granular)`. MOVED here from
//! `vike-backtest::orders` (accounting-upgrade Phase C) so the vike-core ConditionalBook
//! emulator can share the ONE trigger oracle with the backtest engines (vike-core must
//! not depend on vike-backtest). Golden-gated by `crates/vike-backtest/tests/parity/r1_parity.rs`
//! against the FROZEN r1 fixture bytes.
//!
//! ⚠ That last sentence read "the r1/r2 backtest parity fixtures" until 2026-08-28, and r2 has not
//! existed for some time: `fixtures/r2/`'s consumer left with `SingleSymbolEngine` (`2d821d5f`)
//! and the orphaned files were then deleted. The r1 half is real and is the whole gate.
//! ⚠ And it is not a comparison against Python. The fixture bytes were exported from the Python
//! app — which is what the port note above records, and why it still cites `core/orders.py` — but
//! every exporter went with `751de662`, so the committed bytes ARE the oracle: the gate asserts
//! that THIS trigger has not changed its arithmetic unnoticed.
//! `docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md` is the verdict.
//!
//! Gap-open normalization: a bar that OPENS past the trigger never traded at the trigger price,
//! so the realistic fill is the (gapped) open — ADVERSE for stops (filled worse than the stop)
//! and FAVOURABLE for limits (price improvement). Within a non-gapping bar this collapses to
//! the trigger price.
//!
//! Trailing stops check the PRIOR extreme's trigger first, then ratchet the extreme with this
//! bar (so a new-high bar can't stop out on its own low). Pure w.r.t. the bar EXCEPT that
//! trailing ratchet — `order` is `&mut` for exactly that reason (same contract as Python).

use crate::{Bar, OrderKind, WorkingOrder};

/// Fill price for `o` against `bar`, or None if it doesn't trigger.
pub fn order_fill_price(o: &mut WorkingOrder, bar: &Bar) -> Option<f64> {
    match o.kind {
        OrderKind::Market => Some(bar.open),
        OrderKind::MarketClose => Some(bar.close),
        OrderKind::LimitClose => {
            // fills at the close only if the close is at-or-better than the limit
            let price = o.price.expect("limit_close requires price");
            if o.side > 0 {
                if bar.close <= price {
                    Some(bar.close)
                } else {
                    None
                }
            } else if bar.close >= price {
                Some(bar.close)
            } else {
                None
            }
        }
        OrderKind::Limit => {
            // buy on a dip / sell on a rally — a gap through the open improves the fill
            let price = o.price.expect("limit requires price");
            if o.side > 0 {
                if bar.low <= price {
                    Some(price.min(bar.open))
                } else {
                    None
                }
            } else if bar.high >= price {
                Some(price.max(bar.open))
            } else {
                None
            }
        }
        OrderKind::Stop => {
            // breakout up / breakdown — a gap through the open worsens the fill
            let price = o.price.expect("stop requires price");
            if o.side > 0 {
                if bar.high >= price {
                    Some(price.max(bar.open))
                } else {
                    None
                }
            } else if bar.low <= price {
                Some(price.min(bar.open))
            } else {
                None
            }
        }
        OrderKind::Trailing => {
            // side<0 protects a long (sell-stop trailing the high);
            // side>0 protects a short (buy-stop trailing the low).
            let trail = o.trail.expect("trailing requires trail");
            let extreme = o.extreme.expect("trailing requires extreme");
            if o.side < 0 {
                let trigger = extreme - trail;
                if bar.low <= trigger {
                    return Some(trigger.min(bar.open)); // gap-down open fills below the stop
                }
                o.extreme = Some(extreme.max(bar.high));
                None
            } else {
                let trigger = extreme + trail;
                if bar.high >= trigger {
                    return Some(trigger.max(bar.open)); // gap-up open fills above the stop
                }
                o.extreme = Some(extreme.min(bar.low));
                None
            }
        }
    }
}

/// Resolve `o` against ordered finer `sub_bars`: `(fill_price, sub_index)` of the FIRST
/// sub-bar that triggers it, or None. market/market_close fill on the first sub-bar;
/// limit/stop/trailing walk chronologically (the trailing extreme ratchets per sub-bar,
/// exactly as it would over the equivalent coarse bar).
pub fn order_fill_price_granular(o: &mut WorkingOrder, sub_bars: &[Bar]) -> Option<(f64, usize)> {
    if sub_bars.is_empty() {
        return None;
    }
    if matches!(o.kind, OrderKind::Market | OrderKind::MarketClose) {
        // Market(-close) fills on the first sub-bar (order_fill_price is always Some there).
        return order_fill_price(o, &sub_bars[0]).map(|p| (p, 0));
    }
    for (i, sub) in sub_bars.iter().enumerate() {
        if let Some(fp) = order_fill_price(o, sub) {
            return Some((fp, i));
        }
    }
    None
}

/// A degenerate one-price [`Bar`] at `px` (`open == high == low == close`), stamped `ts`.
///
/// The tick→bar adapter for this module's bar-shaped oracle: [`order_fill_price`] and everything
/// layered on it read OHLC, so checking a resting order against a single tick price means handing
/// it a bar whose entire range IS that price — a stop crossed by this tick then fires NOW rather
/// than at bar close — intra-bar fidelity the ported Python original explicitly lacked, which is a
/// statement about that bar-close DESIGN and not about anything this code is still compared to.
/// Shared by `vike_core::ConditionalBook::check_price` and
/// `vike_strategy::evaluate_barriers_at_price`, which each carried a byte-identical copy of this
/// literal, and by `vike_run`'s tick-bar synth as its per-tick OHLC sample.
///
/// `volume` is 0.0 and funding/bid/ask/symbol are absent: a bare tick price carries no bar-level
/// context, and a synthetic bar must never invent any.
pub fn one_price_bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}
