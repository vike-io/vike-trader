//! The pure fill-trigger: WHEN a resting order fills and at WHAT price.
//! Exact port of `core/orders.py::order_fill_price(_granular)`. MOVED here from
//! `vike-backtest::orders` (accounting-upgrade Phase C) so the vike-core OrderEmulationBook
//! emulator can share the ONE trigger oracle with the backtest engines (vike-core must
//! not depend on vike-backtest). Golden-gated by `crates/vike-simulator/tests/parity/r1_parity.rs`
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
//!
//! **The stop-limit and if-touched law** (no Python twin; `StopLimit` and `TakeProfit`). `price`
//! is the trigger T, [`OrderResting::limit`] the limit L, and A the ACTIVATION price, the price the
//! order came alive at: T, or the open when the bar gapped past T (a stop-limit buy's
//! `max(T, open)`; a take-profit buy's `min(T, open)`, a gap down being an improvement; a sell
//! mirrors each). A take-profit with no limit fills at A. Otherwise the LIMIT PART: when the bar
//! reaches L (a buy's `low <= L`, a sell's `high >= L`) the fill is the better of the two, a buy's
//! `min(L, A)` and a sell's `max(L, A)`; when it does not, the order becomes, in place, the plain
//! `Limit` at L ([`OrderResting::rest_as_limit`]) and returns `None`, so it rests on the next bars
//! and books as a maker there. The levels are the caller's, already on the grid: nothing here
//! rounds. The second mutation of `order`, beside the trailing ratchet.
//!
//! **The market-to-limit and auction law** (no Python twin; `MarketToLimit`, `AuctionOpen`,
//! `AuctionClose`). A market-to-limit fills as a market order does, at the open: a bar fills it
//! whole. Its remainder, when a lane fills only part of it (a volume cap, a book that cannot cover
//! it), rests in place as the `Limit` at that first fill's price: the lane sets
//! [`OrderResting::limit`] to it and calls [`OrderResting::rest_as_limit`]. An auction order takes
//! part ONCE, in the bar its caller's session calendar names: the open of the session's first bar,
//! the close of its last. A market one fills at that price; a limit one (`price` its limit) fills
//! there when the price is no worse than its limit, and otherwise this returns `None` and the
//! auction is over: the caller ENDS the order, it never rests. Which bar is the auction is the
//! caller's alone; handed any bar, the law prices it as if it were.
//!
//! **The trailing-limit law** (no Python twin; `TrailingLimit`). It fires as `Trailing` does, on the
//! prior extreme's stop P (`extreme - trail` for a sell, `extreme + trail` for a buy), at the fire
//! price A (P, or the open when the bar gapped past P), and ratchets the extreme only on a bar that
//! does not fire it. At the fire its limit L becomes P less its offset for a sell, P plus it for a buy
//! ([`OrderResting::trailing_limit_at`]), and from there it is the stop-limit's LIMIT PART with that
//! A: the better of L and A when the bar reaches L, else the plain `Limit` at L. Unmeasured: the
//! default is IB's `TRAIL LIMIT` (`lmtPriceOffset`) and Nautilus' `limit_offset`.
//!
//! **The trailing take-profit law** (no Python twin; `TrailingTakeProfit`, IB `TRAIL MIT` / `TRAIL
//! LIT`; IB's description, no evidence). The if-touched mirror of the trailing law: a SELL tracks the
//! LOWEST price since it rests, its trigger T = low + trail, and it fires when the price rises to T, at
//! A = T, or the open when the bar gapped above T (a gap up improves a sell); a BUY tracks the highest
//! price, T = high − trail, and fires on a fall to T, at A = min(T, open). It fires on the T the PRIOR
//! extreme implies, and the extreme ratchets only on a bar that does not fire it. With no
//! [`OrderResting::limit_offset`] it fills at A; with one its limit L is T − offset for a sell and T +
//! offset for a buy ([`OrderResting::trailing_limit_at`]), then the stop-limit's LIMIT PART with that A.
//!
//! **The percent distance** (no Python twin; [`OrderResting::trail_percent`]). Every trailing law here
//! trails by [`OrderResting::trail_distance`] at the extreme it reads: `trail` itself, or extreme × p /
//! 100, so a sell stop sits at extreme − extreme × p / 100. A price trail's arithmetic is unchanged,
//! bit for bit.
//!
//! **The adjustment law** (no Python twin; [`OrderResting::adjust`], IB's adjustable stop and the auto
//! trailing stop). Per event, a `Stop` or `StopLimit` first takes its own law with its current terms
//! (adverse first); then, if it did not fire (a stop-limit that triggered and rested is fired) and the
//! event reached the adjustment's level `at` (a sell stop's high at or above it, a buy stop's low at or
//! below), the order becomes its target IN PLACE, once ([`apply_stop_adjust`]): a stop moves, a
//! stop-limit takes its trigger and limit, a trailing stop takes its distance and the extreme whose
//! stop is the target's (stop + trail for a sell, stop / (1 − p/100) for a percent one; a buy
//! mirrors). The new terms are judged from the NEXT event: a bar has no time inside it. Assumption:
//! IB's "penetrated" is read as touched.
//!
//! **The discretionary limit** (no Python twin; [`OrderResting::discretionary_amount`]). A `Limit` at L
//! with a discretion d is judged as a limit at L + d for a buy and L − d for a sell
//! ([`OrderResting::discretion_limit`]): a falling price meets L + d before L, so the discretionary
//! part fills first (the limit law's range optimism, stated as an assumption), and it books as a
//! TAKER ([`OrderResting::books_as_maker`]).
//!
//! **The opening auction's market-to-limit** (`AuctionOpenToLimit`, IB `MTL` + `AUC`): exactly a market
//! `AuctionOpen`, the open of the bar the caller names; what the lane's volume cap leaves rests in place
//! as the `Limit` at the price the auction booked, which is the lane's.
//!
//! **A [`OrderResting::release`] is never read here, nor a [`OrderResting::activation`].** Whether an
//! order is still held at an event ([`OrderResting::is_held`]), and whether a trailing order has
//! reached its activation level, are each fill lane's test, made before it calls this oracle.

use crate::{OrderKind, OrderResting, OrderStopAdjustTo};
use vike_marketdata::Bar;

/// Fill price for `o` against `bar`, or None if it doesn't trigger.
pub fn order_fill_price(o: &mut OrderResting, bar: &Bar) -> Option<f64> {
    match o.kind {
        OrderKind::Market => Some(bar.open),
        OrderKind::MarketClose => Some(bar.close),
        OrderKind::LimitClose => {
            // fills at the close only if the close is at-or-better than the limit
            let price = o.price.expect("limit_close requires price");
            if o.side > 0 {
                if bar.close <= price { Some(bar.close) } else { None }
            } else if bar.close >= price {
                Some(bar.close)
            } else {
                None
            }
        }
        OrderKind::Limit => {
            // buy on a dip / sell on a rally — a gap through the open improves the fill; a
            // discretionary limit is judged at its discretion
            let price = o.discretion_limit(o.price.expect("limit requires price"));
            if o.side > 0 {
                if bar.low <= price { Some(price.min(bar.open)) } else { None }
            } else if bar.high >= price {
                Some(price.max(bar.open))
            } else {
                None
            }
        }
        OrderKind::Stop => {
            // breakout up / breakdown — a gap through the open worsens the fill
            let price = o.price.expect("stop requires price");
            let fill = if o.side > 0 {
                if bar.high >= price { Some(price.max(bar.open)) } else { None }
            } else if bar.low <= price {
                Some(price.min(bar.open))
            } else {
                None
            };
            if fill.is_none() {
                apply_stop_adjust(o, bar.high, bar.low);
            }
            fill
        }
        OrderKind::Trailing => trailing_fire(o, bar).map(|(_, fill)| fill),
        OrderKind::TrailingLimit => {
            // fires as a trailing stop does; its limit is set off the stop level it fired at
            let (stop, activation) = trailing_fire(o, bar)?;
            o.limit = Some(o.trailing_limit_at(stop));
            limit_part(o, bar, activation)
        }
        OrderKind::TrailingTakeProfit => {
            // touched from the favourable side, as a take-profit is; its limit is set off the trigger
            let (trigger, activation) = trailing_touch(o, bar)?;
            if o.limit_offset.is_none() {
                return Some(activation);
            }
            o.limit = Some(o.trailing_limit_at(trigger));
            limit_part(o, bar, activation)
        }
        OrderKind::StopLimit => {
            // triggers as a stop does; the gap past T is the activation price
            let trigger = o.price.expect("stop_limit requires price");
            let activation = if o.side > 0 {
                if bar.high >= trigger { Some(trigger.max(bar.open)) } else { None }
            } else if bar.low <= trigger {
                Some(trigger.min(bar.open))
            } else {
                None
            };
            let Some(activation) = activation else {
                apply_stop_adjust(o, bar.high, bar.low);
                return None;
            };
            limit_part(o, bar, activation)
        }
        OrderKind::TakeProfit => {
            // triggers from the favourable side, as a limit does; a gap past T improves A
            let trigger = o.price.expect("take_profit requires price");
            let activation = if o.side > 0 {
                if bar.low <= trigger { trigger.min(bar.open) } else { return None }
            } else if bar.high >= trigger {
                trigger.max(bar.open)
            } else {
                return None;
            };
            if o.limit.is_none() {
                return Some(activation);
            }
            limit_part(o, bar, activation)
        }
        // a market order's fill; a remainder is the lane's (the module doc's law)
        OrderKind::MarketToLimit => Some(bar.open),
        OrderKind::AuctionOpen => auction_fill(o, bar.open),
        OrderKind::AuctionClose => auction_fill(o, bar.close),
        // a market order at the opening auction; a remainder is the lane's (the module doc's law)
        OrderKind::AuctionOpenToLimit => Some(bar.open),
    }
}

/// **The adjustment law's mutation** (the module doc): a `Stop` or `StopLimit` with a
/// [`OrderResting::adjust`] whose level this event reached (`high` at or above it for a sell stop,
/// `low` at or below for a buy stop) becomes its target in place, once, and its adjustment is taken.
/// Called by every fill lane, on a bar with its high and low and on a quote tick with the quote twice,
/// only for an event that did not fire the order. Nothing happens to an order with no adjustment.
pub fn apply_stop_adjust(o: &mut OrderResting, high: f64, low: f64) {
    let Some(adj) = o.adjust.as_deref().copied() else { return };
    let reached = if o.side < 0 { high >= adj.at } else { low <= adj.at };
    if !reached {
        return;
    }
    o.adjust = None;
    let (stop, limit) = match adj.to {
        OrderStopAdjustTo::Stop { stop } => (stop, None),
        OrderStopAdjustTo::StopLimit { stop, limit } => (stop, Some(limit)),
        OrderStopAdjustTo::Trailing { stop, trail, trail_percent } => {
            let extreme = match (trail, trail_percent) {
                (Some(t), _) if o.side < 0 => stop + t,
                (Some(t), _) => stop - t,
                (None, Some(p)) if o.side < 0 => stop / (1.0 - p / 100.0),
                (None, Some(p)) => stop / (1.0 + p / 100.0),
                // no distance (the doors' `stop_adjust_refusal` refuses it): it stays the stop it targets
                (None, None) => {
                    o.kind = OrderKind::Stop;
                    o.price = Some(stop);
                    o.limit = None;
                    return;
                }
            };
            o.kind = OrderKind::Trailing;
            o.price = None;
            o.limit = None;
            o.trail = trail;
            o.trail_percent = trail_percent;
            o.activation = None;
            o.extreme = Some(extreme);
            return;
        }
    };
    o.kind = if limit.is_some() { OrderKind::StopLimit } else { OrderKind::Stop };
    o.price = Some(stop);
    o.limit = limit;
}

/// Has `price` reached a trailing order's activation `level`? A sell (`side < 0`) protects a long,
/// so the price comes up to it; a buy protects a short, so it comes down to it.
#[must_use]
pub fn trailing_activation_reached(side: i32, price: f64, level: f64) -> bool {
    if side < 0 { price >= level } else { price <= level }
}

/// The trailing law of `Trailing` and `TrailingLimit`: `Some((stop, fill))` when `bar` reaches the
/// stop the PRIOR extreme implies (`fill` that stop, or the open a gap carried past it), else the
/// extreme ratchets with this bar and `None`.
fn trailing_fire(o: &mut OrderResting, bar: &Bar) -> Option<(f64, f64)> {
    // side<0 protects a long (sell-stop trailing the high);
    // side>0 protects a short (buy-stop trailing the low).
    let extreme = o.extreme.expect("trailing requires extreme");
    let trail = o.trail_distance(extreme);
    if o.side < 0 {
        let trigger = extreme - trail;
        if bar.low <= trigger {
            return Some((trigger, trigger.min(bar.open))); // gap-down open fills below the stop
        }
        o.extreme = Some(extreme.max(bar.high));
        None
    } else {
        let trigger = extreme + trail;
        if bar.high >= trigger {
            return Some((trigger, trigger.max(bar.open))); // gap-up open fills above the stop
        }
        o.extreme = Some(extreme.min(bar.low));
        None
    }
}

/// The trailing take-profit law (the module doc): `Some((trigger, fill))` when `bar` reaches the trigger
/// the PRIOR extreme implies from the favourable side (`fill` that trigger, or the open a gap carried
/// past it, an improvement), else the extreme ratchets with this bar and `None`.
fn trailing_touch(o: &mut OrderResting, bar: &Bar) -> Option<(f64, f64)> {
    // side<0 trails the low and sells on a rise off it; side>0 trails the high and buys on a fall.
    let extreme = o.extreme.expect("trailing_take_profit requires extreme");
    let trail = o.trail_distance(extreme);
    if o.side < 0 {
        let trigger = extreme + trail;
        if bar.high >= trigger {
            return Some((trigger, trigger.max(bar.open))); // a gap-up open sells above the trigger
        }
        o.extreme = Some(extreme.min(bar.low));
        None
    } else {
        let trigger = extreme - trail;
        if bar.low <= trigger {
            return Some((trigger, trigger.min(bar.open))); // a gap-down open buys below the trigger
        }
        o.extreme = Some(extreme.max(bar.high));
        None
    }
}

/// An auction order at the `auction` price of the bar its caller named: a market one fills there,
/// a limit one when `auction` is no worse than its limit; `None` means the auction passed it by.
fn auction_fill(o: &OrderResting, auction: f64) -> Option<f64> {
    match o.price {
        None => Some(auction),
        Some(limit) if (o.side > 0 && auction <= limit) || (o.side < 0 && auction >= limit) => {
            Some(auction)
        }
        Some(_) => None,
    }
}

/// The limit part of a triggered `StopLimit` / limit-if-touched `TakeProfit` / `TrailingLimit` /
/// limit-offset `TrailingTakeProfit` (the module doc's laws): the better of L ([`OrderResting::limit`], set by the caller) and `activation`
/// when `bar` reaches L, else the order rests in place as the plain `Limit` at L and this returns
/// `None`. Public for the ONE trailing path walk both books share
/// (`vike_fills::fill_model::trailing_path_fill`, which the simulator and the paper book call), so a
/// trailing limit fired on the path takes exactly this law. Not on any per-message path.
pub fn limit_part(o: &mut OrderResting, bar: &Bar, activation: f64) -> Option<f64> {
    let limit = o.limit.expect("a limit part requires limit");
    if o.side > 0 {
        if bar.low <= limit {
            return Some(limit.min(activation));
        }
    } else if bar.high >= limit {
        return Some(limit.max(activation));
    }
    o.rest_as_limit();
    None
}

/// Resolve `o` against ordered finer `sub_bars`: `(fill_price, sub_index)` of the FIRST
/// sub-bar that triggers it, or None. market/market_close fill on the first sub-bar;
/// limit/stop/trailing walk chronologically (the trailing extreme ratchets per sub-bar,
/// exactly as it would over the equivalent coarse bar), and a stop-limit, if-touched or
/// trailing-limit order that rests as a `Limit` on one sub-bar walks the later ones as that `Limit`. A
/// market-to-limit fills on the first sub-bar as a market order does. An auction order is
/// judged ONCE, never walked: the opening auction (its market-to-limit too) on the first sub-bar's
/// open, the closing one on the LAST sub-bar's close (the coarse bar's own close).
pub fn order_fill_price_granular(o: &mut OrderResting, sub_bars: &[Bar]) -> Option<(f64, usize)> {
    if sub_bars.is_empty() {
        return None;
    }
    if matches!(
        o.kind,
        OrderKind::Market
            | OrderKind::MarketClose
            | OrderKind::MarketToLimit
            | OrderKind::AuctionOpen
            | OrderKind::AuctionOpenToLimit
    ) {
        // Market(-close) fills on the first sub-bar (order_fill_price is always Some there); a
        // limit opening auction is judged there once and may miss.
        return order_fill_price(o, &sub_bars[0]).map(|p| (p, 0));
    }
    if o.kind == OrderKind::AuctionClose {
        let last = sub_bars.len() - 1;
        return order_fill_price(o, &sub_bars[last]).map(|p| (p, last));
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
/// Shared by `vike_core::OrderEmulationBook::check_price` and
/// `crate::evaluate_barriers_at_price`, which each carried a byte-identical copy of this
/// literal, and by `vike_tradehub_core`'s tick-bar synth as its per-tick OHLC sample.
///
/// `volume` is 0.0 and funding/bid/ask/symbol are absent: a bare tick price carries no bar-level
/// context, and a synthetic bar must never invent any.
pub fn one_price_bar(ts: i64, px: f64) -> Bar {
    Bar {
        timestamp: ts,
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

#[cfg(test)]
mod tests {
    use super::*;
    use OrderKind::{
        Limit, Stop, StopLimit, TakeProfit, Trailing, TrailingLimit, TrailingTakeProfit,
    };

    /// (case, kind, side, trigger T, limit L, [open, high, low, close], fill, kind after)
    type Row = (&'static str, OrderKind, i32, f64, Option<f64>, [f64; 4], Option<f64>, OrderKind);

    /// The module doc's law, one row per case. Row 2 is the MEASURED case (`order_harbor_stop_limit`,
    /// 471 of 471: a buy whose limit sits above its trigger fills at the trigger, the limit never
    /// binding, no gap); the gap row beside it is assumption 1, the L-below-T rows assumption 2, the
    /// gap-past-L rows assumption 3, and every sell and if-touched row is the law's mirror, not a
    /// measurement.
    #[rustfmt::skip]
    const ROWS: &[Row] = &[
        ("sl buy, L one tick below T", StopLimit, 1, 101.0, Some(100.0), [99.0, 102.0, 98.0, 101.0], Some(100.0), StopLimit),
        ("sl buy, L above T: fills at A = T (measured)", StopLimit, 1, 101.0, Some(103.0), [99.0, 102.0, 98.0, 101.0], Some(101.0), StopLimit),
        ("sl buy, L above T, gap past T: A = open", StopLimit, 1, 101.0, Some(103.0), [102.0, 104.0, 101.5, 103.0], Some(102.0), StopLimit),
        ("sl buy, gap past L, back to L", StopLimit, 1, 101.0, Some(100.0), [103.0, 105.0, 99.5, 104.0], Some(100.0), StopLimit),
        ("sl buy, gap past L, never back: rests", StopLimit, 1, 101.0, Some(100.0), [103.0, 105.0, 102.0, 104.0], None, Limit),
        ("sl buy, L below T not reached: rests", StopLimit, 1, 101.0, Some(99.0), [100.0, 102.0, 99.5, 101.0], None, Limit),
        ("sl buy, not triggered", StopLimit, 1, 101.0, Some(100.0), [99.0, 100.5, 98.0, 100.0], None, StopLimit),
        ("sl sell, L one tick above T", StopLimit, -1, 99.0, Some(100.0), [101.0, 102.0, 98.0, 99.0], Some(100.0), StopLimit),
        ("sl sell, L below T: fills at A = T", StopLimit, -1, 99.0, Some(97.0), [101.0, 102.0, 98.0, 99.0], Some(99.0), StopLimit),
        ("sl sell, L below T, gap past T: A = open", StopLimit, -1, 99.0, Some(97.0), [98.0, 98.5, 96.0, 97.0], Some(98.0), StopLimit),
        ("sl sell, L above T not reached: rests", StopLimit, -1, 99.0, Some(101.0), [100.0, 100.5, 98.0, 99.0], None, Limit),
        ("sl sell, not triggered", StopLimit, -1, 99.0, Some(100.0), [101.0, 102.0, 99.5, 100.0], None, StopLimit),
        ("mit buy, touched: fills at T", TakeProfit, 1, 99.0, None, [101.0, 102.0, 98.0, 100.0], Some(99.0), TakeProfit),
        ("mit buy, gap down past T: A = open", TakeProfit, 1, 99.0, None, [97.0, 98.0, 96.0, 97.0], Some(97.0), TakeProfit),
        ("mit buy, not touched", TakeProfit, 1, 99.0, None, [101.0, 102.0, 99.5, 100.0], None, TakeProfit),
        ("mit sell, touched: fills at T", TakeProfit, -1, 101.0, None, [99.0, 102.0, 98.0, 100.0], Some(101.0), TakeProfit),
        ("mit sell, gap up past T: A = open", TakeProfit, -1, 101.0, None, [103.0, 104.0, 102.0, 103.0], Some(103.0), TakeProfit),
        ("lit buy, L below T reached", TakeProfit, 1, 99.0, Some(98.5), [101.0, 102.0, 98.0, 100.0], Some(98.5), TakeProfit),
        ("lit buy, L above T: fills at A = T", TakeProfit, 1, 99.0, Some(100.0), [101.0, 102.0, 98.0, 100.0], Some(99.0), TakeProfit),
        ("lit buy, L not reached: rests", TakeProfit, 1, 99.0, Some(98.0), [101.0, 102.0, 98.5, 100.0], None, Limit),
        ("lit sell, L above T reached", TakeProfit, -1, 101.0, Some(101.5), [99.0, 102.0, 98.0, 100.0], Some(101.5), TakeProfit),
        ("lit sell, gap up past T and L: A = open", TakeProfit, -1, 101.0, Some(101.5), [103.0, 104.0, 102.0, 103.0], Some(103.0), TakeProfit),
    ];

    fn order(kind: OrderKind, side: i32, trigger: f64, limit: Option<f64>) -> OrderResting {
        OrderResting { price: Some(trigger), limit, ..OrderResting::new(kind, side, 1.0) }
    }

    #[test]
    fn the_stop_limit_and_if_touched_law() {
        for &(case, kind, side, trigger, limit, [open, high, low, close], fill, after) in ROWS {
            let mut o = order(kind, side, trigger, limit);
            let bar = Bar::ohlcv(0, open, high, low, close, 1.0);
            assert_eq!(order_fill_price(&mut o, &bar), fill, "{case}");
            assert_eq!(o.kind, after, "{case}: kind after");
            let terms = if after == Limit { (limit, None) } else { (Some(trigger), limit) };
            assert_eq!((o.price, o.limit), terms, "{case}: terms after");
        }
    }

    /// The conversion is the plain `Limit` at L from then on: a maker, filled by the `Limit` law.
    #[test]
    fn a_stop_limit_that_rests_is_the_limit_at_its_limit_price() {
        let mut o = order(StopLimit, 1, 101.0, Some(100.0));
        assert!(!o.books_as_maker(), "a fill on the trigger event is a taker");
        assert_eq!(order_fill_price(&mut o, &Bar::ohlcv(0, 103.0, 105.0, 102.0, 104.0, 1.0)), None);
        assert!(o.books_as_maker(), "rested as the Limit");
        // the next bar opens below L: the Limit law's gap improvement
        assert_eq!(
            order_fill_price(&mut o, &Bar::ohlcv(1, 99.0, 101.0, 98.0, 100.0, 1.0)),
            Some(99.0)
        );
    }

    /// (case, kind, side, limit (`price`), [open, high, low, close], fill)
    type EntryRow = (&'static str, OrderKind, i32, Option<f64>, [f64; 4], Option<f64>);

    /// The market-to-limit and auction law, one row per case (the module doc). Every row is the
    /// law's own consequence, not a measurement: the auction price is the bar's open or close
    /// (QuantConnect `MarketOnOpenFill` takes the open too), and a market-to-limit on a bar is a market
    /// order. None of the three is converted here: a bar fills a market-to-limit whole.
    #[rustfmt::skip]
    const ENTRY_ROWS: &[EntryRow] = &[
        ("mtl buy: the open", OrderKind::MarketToLimit, 1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("mtl sell: the open", OrderKind::MarketToLimit, -1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("moo buy: the open", OrderKind::AuctionOpen, 1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("moo sell: the open", OrderKind::AuctionOpen, -1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("loo buy, open at L", OrderKind::AuctionOpen, 1, Some(100.0), [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("loo buy, open below L", OrderKind::AuctionOpen, 1, Some(101.0), [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("loo buy, open above L: the auction passed", OrderKind::AuctionOpen, 1, Some(99.0), [100.0, 102.0, 98.0, 101.0], None),
        ("loo sell, open above L", OrderKind::AuctionOpen, -1, Some(99.0), [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("loo sell, open below L: the auction passed", OrderKind::AuctionOpen, -1, Some(101.0), [100.0, 102.0, 98.0, 101.0], None),
        ("moc buy: the close", OrderKind::AuctionClose, 1, None, [100.0, 102.0, 98.0, 101.0], Some(101.0)),
        ("moc sell: the close", OrderKind::AuctionClose, -1, None, [100.0, 102.0, 98.0, 101.0], Some(101.0)),
        ("loc buy, close below L", OrderKind::AuctionClose, 1, Some(101.5), [100.0, 102.0, 98.0, 101.0], Some(101.0)),
        ("loc buy, close above L, low below it: the auction passed", OrderKind::AuctionClose, 1, Some(99.0), [100.0, 102.0, 98.0, 101.0], None),
        ("loc sell, close at L", OrderKind::AuctionClose, -1, Some(101.0), [100.0, 102.0, 98.0, 101.0], Some(101.0)),
        ("loc sell, close below L: the auction passed", OrderKind::AuctionClose, -1, Some(101.5), [100.0, 102.0, 98.0, 101.0], None),
        ("mtl at the open, buy: the open", OrderKind::AuctionOpenToLimit, 1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
        ("mtl at the open, sell: the open", OrderKind::AuctionOpenToLimit, -1, None, [100.0, 102.0, 98.0, 101.0], Some(100.0)),
    ];

    #[test]
    fn the_market_to_limit_and_auction_law() {
        for &(case, kind, side, limit, [open, high, low, close], fill) in ENTRY_ROWS {
            let mut o = OrderResting { price: limit, ..OrderResting::new(kind, side, 1.0) };
            let bar = Bar::ohlcv(0, open, high, low, close, 1.0);
            assert_eq!(order_fill_price(&mut o, &bar), fill, "{case}");
            assert_eq!((o.kind, o.price, o.limit), (kind, limit, None), "{case}: terms unchanged");
        }
    }

    /// On sub-bars a market-to-limit and an opening auction take the FIRST sub-bar's open, the
    /// closing auction the LAST sub-bar's close, and an auction the first or last sub-bar does not
    /// fill is not walked on to a later one.
    #[test]
    fn an_auction_is_judged_once_on_sub_bars_and_never_walked() {
        let subs = [
            Bar::ohlcv(0, 100.0, 100.5, 99.5, 100.2, 1.0),
            Bar::ohlcv(1, 100.2, 101.0, 98.0, 99.0, 1.0),
            Bar::ohlcv(2, 99.0, 99.5, 97.0, 98.0, 1.0),
        ];
        let granular = |kind, side, limit| {
            let mut o = OrderResting { price: limit, ..OrderResting::new(kind, side, 1.0) };
            order_fill_price_granular(&mut o, &subs)
        };
        use OrderKind::{AuctionClose, AuctionOpen, AuctionOpenToLimit, MarketToLimit};
        assert_eq!(granular(MarketToLimit, 1, None), Some((100.0, 0)));
        assert_eq!(granular(AuctionOpen, 1, None), Some((100.0, 0)));
        assert_eq!(granular(AuctionOpenToLimit, -1, None), Some((100.0, 0)));
        assert_eq!(granular(AuctionClose, -1, None), Some((98.0, 2)));
        // a later sub-bar opens at 99.0, under this limit: the auction already passed it
        assert_eq!(granular(AuctionOpen, 1, Some(99.5)), None);
        // the first sub-bar closes at 100.2, above this sell limit: the close is the LAST sub-bar's
        assert_eq!(granular(AuctionClose, -1, Some(100.0)), None);
        assert_eq!(granular(AuctionClose, 1, Some(98.0)), Some((98.0, 2)));
    }

    /// The remainder law a lane applies after a partial fill: the market-to-limit rests as the
    /// `Limit` at its first fill's price, a maker from then on.
    #[test]
    fn a_market_to_limit_remainder_rests_as_the_limit_at_its_first_fill() {
        let mut o = OrderResting::new(OrderKind::MarketToLimit, 1, 5.0);
        let fill =
            order_fill_price(&mut o, &Bar::ohlcv(0, 100.0, 101.0, 99.0, 100.5, 1.0)).unwrap();
        o.limit = Some(fill);
        o.rest_as_limit();
        assert_eq!((o.kind, o.price, o.limit), (Limit, Some(100.0), None));
        assert!(o.books_as_maker());
    }

    /// (kind, price, limit, extreme) after the bar
    type TrailAfter = (OrderKind, Option<f64>, Option<f64>, f64);
    /// (case, side, extreme, limit offset, [open, high, low, close], fill, after)
    type TrailRow = (&'static str, i32, f64, f64, [f64; 4], Option<f64>, TrailAfter);

    /// The trailing-limit law (the module doc), trail 2: a sell trailing a high of 110 stops at 108, a
    /// buy trailing a low of 90 at 92. Every row is the law's own consequence, not a measurement.
    #[rustfmt::skip]
    const TRAIL_ROWS: &[TrailRow] = &[
        ("sell, not fired: the extreme ratchets", -1, 110.0, 1.0, [109.0, 112.0, 108.5, 111.0], None, (TrailingLimit, None, None, 112.0)),
        ("sell, fired, no gap: A = P, filled at P", -1, 110.0, 1.0, [109.0, 109.5, 106.0, 107.0], Some(108.0), (TrailingLimit, None, Some(107.0), 110.0)),
        ("sell, gap past P, above L: A = open", -1, 110.0, 1.0, [107.5, 107.8, 105.0, 106.0], Some(107.5), (TrailingLimit, None, Some(107.0), 110.0)),
        ("sell, gap past L, back to L", -1, 110.0, 1.0, [106.0, 107.2, 104.0, 105.0], Some(107.0), (TrailingLimit, None, Some(107.0), 110.0)),
        ("sell, gap past L, never back: rests", -1, 110.0, 1.0, [106.0, 106.5, 104.0, 105.0], None, (Limit, Some(107.0), None, 110.0)),
        ("sell, offset 0: the limit is the stop", -1, 110.0, 0.0, [109.0, 109.5, 106.0, 107.0], Some(108.0), (TrailingLimit, None, Some(108.0), 110.0)),
        ("buy, not fired: the extreme ratchets", 1, 90.0, 1.0, [91.0, 91.5, 89.0, 90.0], None, (TrailingLimit, None, None, 89.0)),
        ("buy, fired, no gap: A = P, filled at P", 1, 90.0, 1.0, [91.0, 93.0, 90.5, 92.0], Some(92.0), (TrailingLimit, None, Some(93.0), 90.0)),
        ("buy, gap past L, never back: rests", 1, 90.0, 1.0, [94.0, 96.0, 93.5, 95.0], None, (Limit, Some(93.0), None, 90.0)),
    ];

    #[test]
    fn the_trailing_limit_law() {
        for &(case, side, extreme, offset, [open, high, low, close], fill, after) in TRAIL_ROWS {
            let mut o = OrderResting {
                trail: Some(2.0),
                extreme: Some(extreme),
                limit_offset: Some(offset),
                ..OrderResting::new(TrailingLimit, side, 1.0)
            };
            let bar = Bar::ohlcv(0, open, high, low, close, 1.0);
            assert_eq!(order_fill_price(&mut o, &bar), fill, "{case}");
            assert_eq!((o.kind, o.price, o.limit, o.extreme.unwrap()), after, "{case}: after");
        }
    }

    /// A trailing-limit fires on exactly the bar, and at exactly the price, the plain trailing stop
    /// does whenever its limit is reached, and ratchets the same extreme: the trigger is ONE law.
    #[test]
    fn a_trailing_limit_fires_where_the_trailing_stop_does() {
        let bars = [
            Bar::ohlcv(0, 100.0, 104.0, 99.0, 103.0, 1.0),
            Bar::ohlcv(1, 103.0, 106.0, 102.5, 105.0, 1.0),
            Bar::ohlcv(2, 105.0, 105.5, 102.0, 103.0, 1.0),
        ];
        let walk = |mut o: OrderResting| {
            let fills: Vec<_> = bars.iter().map(|b| order_fill_price(&mut o, b)).collect();
            (fills, o.extreme)
        };
        let plain = OrderResting {
            trail: Some(3.0),
            extreme: Some(101.0),
            ..OrderResting::new(Trailing, -1, 1.0)
        };
        let limited =
            OrderResting { kind: TrailingLimit, limit_offset: Some(5.0), ..plain.clone() };
        assert_eq!(walk(plain), (vec![None, None, Some(103.0)], Some(106.0)));
        assert_eq!(walk(limited), (vec![None, None, Some(103.0)], Some(106.0)));
    }

    /// (kind, limit, extreme) after the bar
    type TouchAfter = (OrderKind, Option<f64>, f64);
    /// (case, side, extreme, limit offset, [open, high, low, close], fill, after)
    type TouchRow = (&'static str, i32, f64, Option<f64>, [f64; 4], Option<f64>, TouchAfter);

    /// The trailing take-profit law (the module doc), trail 2: a sell trailing a low of 90 triggers at
    /// 92, a buy trailing a high of 110 at 108. Every row is the law's own consequence (IB's
    /// description), not a measurement.
    #[rustfmt::skip]
    const TOUCH_ROWS: &[TouchRow] = &[
        ("sell, not touched: the low ratchets", -1, 90.0, None, [91.0, 91.5, 89.0, 90.0], None, (TrailingTakeProfit, None, 89.0)),
        ("sell, touched, no gap: A = T", -1, 90.0, None, [91.0, 93.0, 90.5, 92.0], Some(92.0), (TrailingTakeProfit, None, 90.0)),
        ("sell, gap up past T: A = open", -1, 90.0, None, [94.0, 95.0, 93.5, 94.0], Some(94.0), (TrailingTakeProfit, None, 90.0)),
        ("sell, offset 1, no gap: L 91, A = T", -1, 90.0, Some(1.0), [91.0, 93.0, 90.5, 92.0], Some(92.0), (TrailingTakeProfit, Some(91.0), 90.0)),
        ("sell, offset 1, gap up: A = open", -1, 90.0, Some(1.0), [94.0, 95.0, 93.5, 94.0], Some(94.0), (TrailingTakeProfit, Some(91.0), 90.0)),
        ("buy, not touched: the high ratchets", 1, 110.0, None, [109.0, 112.0, 108.5, 111.0], None, (TrailingTakeProfit, None, 112.0)),
        ("buy, touched, no gap: A = T", 1, 110.0, None, [109.0, 109.5, 107.0, 108.0], Some(108.0), (TrailingTakeProfit, None, 110.0)),
        ("buy, gap down past T: A = open", 1, 110.0, None, [106.0, 107.0, 105.0, 106.0], Some(106.0), (TrailingTakeProfit, None, 110.0)),
        ("buy, offset 0.5, no gap: L 108.5, A = T", 1, 110.0, Some(0.5), [109.0, 109.5, 107.0, 108.0], Some(108.0), (TrailingTakeProfit, Some(108.5), 110.0)),
        ("buy, offset 0.5, gap down: A = open", 1, 110.0, Some(0.5), [106.0, 107.0, 105.0, 106.0], Some(106.0), (TrailingTakeProfit, Some(108.5), 110.0)),
    ];

    #[test]
    fn the_trailing_take_profit_law() {
        for &(case, side, extreme, offset, [open, high, low, close], fill, after) in TOUCH_ROWS {
            let mut o = OrderResting {
                trail: Some(2.0),
                extreme: Some(extreme),
                limit_offset: offset,
                ..OrderResting::new(TrailingTakeProfit, side, 1.0)
            };
            let bar = Bar::ohlcv(0, open, high, low, close, 1.0);
            assert_eq!(order_fill_price(&mut o, &bar), fill, "{case}");
            assert_eq!((o.kind, o.limit, o.extreme.unwrap()), after, "{case}: after");
            assert_eq!(
                o.is_limit_bounded(),
                offset.is_some(),
                "{case}: limit-bounded by its offset"
            );
        }
    }

    /// A percent trail of 2 at an extreme of 100 IS the price trail of 2 there: every trailing kind fires
    /// on the same bar, at the same price, and ratchets the same extreme.
    #[test]
    fn a_percent_trail_is_the_price_trail_of_its_distance_at_the_same_extreme() {
        let bars = [
            [99.0, 99.5, 97.0, 98.0],
            [99.5, 101.0, 98.5, 100.0],
            [101.0, 103.0, 100.5, 102.0],
            [97.0, 97.5, 96.0, 97.0],
        ];
        for (kind, side) in [
            (Trailing, -1),
            (Trailing, 1),
            (TrailingLimit, -1),
            (TrailingTakeProfit, -1),
            (TrailingTakeProfit, 1),
        ] {
            let offset = (kind != Trailing).then_some(0.5);
            let order = |trail, trail_percent| OrderResting {
                trail,
                trail_percent,
                extreme: Some(100.0),
                limit_offset: offset,
                ..OrderResting::new(kind, side, 1.0)
            };
            for [open, high, low, close] in bars {
                let bar = Bar::ohlcv(0, open, high, low, close, 1.0);
                let (mut by_price, mut by_percent) =
                    (order(Some(2.0), None), order(None, Some(2.0)));
                let fills = (
                    order_fill_price(&mut by_price, &bar),
                    order_fill_price(&mut by_percent, &bar),
                );
                assert_eq!(fills.0, fills.1, "{kind:?} {side} {open}");
                assert_eq!(by_price.extreme, by_percent.extreme, "{kind:?} {side} {open}");
                assert_eq!(by_price.kind, by_percent.kind, "{kind:?} {side} {open}");
            }
        }
        let p = OrderResting { trail_percent: Some(2.0), ..OrderResting::new(Trailing, -1, 1.0) };
        assert_eq!(
            p.trail_distance(110.0),
            110.0 * 2.0 / 100.0,
            "the distance moves with the extreme"
        );
    }

    fn adjustable(
        kind: OrderKind,
        side: i32,
        stop: f64,
        limit: Option<f64>,
        at: f64,
        to: OrderStopAdjustTo,
    ) -> OrderResting {
        let adjust = Some(Box::new(crate::OrderStopAdjust { at, to }));
        OrderResting { price: Some(stop), limit, adjust, ..OrderResting::new(kind, side, 1.0) }
    }

    /// The adjustment law (the module doc), a sell stop at 95 adjusted at 105: a bar that neither fires
    /// it nor reaches 105 changes nothing; one that reaches 105 makes it its target IN PLACE, once, and
    /// returns no fill (the new terms are judged from the next bar); a bar that fires it fires the
    /// stop as it stood, adverse first, the adjustment untouched.
    #[test]
    fn each_adjustment_target_takes_the_order_over_in_place_once() {
        let quiet = Bar::ohlcv(0, 101.0, 104.0, 99.0, 103.0, 1.0);
        let reach = Bar::ohlcv(1, 104.0, 106.0, 103.0, 105.0, 1.0);
        let targets = [
            (OrderStopAdjustTo::Stop { stop: 100.0 }, (Stop, Some(100.0), None, None, None)),
            (
                OrderStopAdjustTo::StopLimit { stop: 100.0, limit: 99.0 },
                (StopLimit, Some(100.0), Some(99.0), None, None),
            ),
            (
                OrderStopAdjustTo::Trailing { stop: 100.0, trail: Some(3.0), trail_percent: None },
                (Trailing, None, None, Some(3.0), Some(103.0)),
            ),
        ];
        for kind in [Stop, StopLimit] {
            let limit = (kind == StopLimit).then_some(94.0);
            for (to, after) in targets {
                let mut o = adjustable(kind, -1, 95.0, limit, 105.0, to);
                assert_eq!(order_fill_price(&mut o, &quiet), None, "{kind:?} {to:?}: quiet");
                assert!(o.adjust.is_some() && o.kind == kind, "{kind:?} {to:?}: untouched");
                assert_eq!(order_fill_price(&mut o, &reach), None, "{kind:?} {to:?}: reached");
                assert_eq!(
                    (o.kind, o.price, o.limit, o.trail, o.extreme),
                    after,
                    "{kind:?} {to:?}"
                );
                assert_eq!(o.adjust, None, "{kind:?} {to:?}: once");
                // the next bar judges the new terms: every target fires at 100 on a fall through it
                let fall = Bar::ohlcv(2, 104.0, 104.5, 99.0, 100.0, 1.0);
                assert_eq!(
                    order_fill_price(&mut o, &fall),
                    Some(100.0),
                    "{kind:?} {to:?}: next bar"
                );
            }
            let mut fired = adjustable(kind, -1, 95.0, limit, 105.0, targets[0].0);
            let both = Bar::ohlcv(3, 100.0, 106.0, 93.0, 94.0, 1.0);
            // the stop at 95 (a stop-limit's 94 limit is reached too): the open is above it
            assert_eq!(order_fill_price(&mut fired, &both), Some(95.0), "{kind:?}: the stop first");
            assert!(fired.adjust.is_some(), "{kind:?}: a fired stop is not adjusted");
        }
        // a buy stop mirrors it: at 105, adjusted at 95 by a low at or below it
        let mut buy =
            adjustable(Stop, 1, 105.0, None, 95.0, OrderStopAdjustTo::Stop { stop: 100.0 });
        assert_eq!(order_fill_price(&mut buy, &Bar::ohlcv(0, 97.0, 98.0, 94.5, 96.0, 1.0)), None);
        assert_eq!((buy.kind, buy.price, buy.adjust), (Stop, Some(100.0), None));
        // a percent trailing target: the extreme whose stop is the target's
        let to = OrderStopAdjustTo::Trailing { stop: 98.0, trail: None, trail_percent: Some(2.0) };
        let mut pct = adjustable(Stop, -1, 95.0, None, 105.0, to);
        assert_eq!(order_fill_price(&mut pct, &reach), None);
        let extreme = pct.extreme.unwrap();
        assert_eq!((pct.kind, pct.trail, pct.trail_percent), (Trailing, None, Some(2.0)));
        assert!((extreme - pct.trail_distance(extreme) - 98.0).abs() < 1e-9, "{extreme}");
    }

    /// A discretionary buy at L 100 with a discretion of 1 is a limit at 101: a bar whose low is 100.5
    /// fills it at 101 (the plain limit at 100 does not fill), and it books as a TAKER. A sell mirrors.
    #[test]
    fn a_discretionary_limit_fills_at_its_discretion_as_a_taker() {
        let limit = |side, d| OrderResting {
            price: Some(100.0),
            discretionary_amount: d,
            ..OrderResting::new(Limit, side, 1.0)
        };
        let bar = Bar::ohlcv(0, 102.0, 103.0, 100.5, 101.0, 1.0);
        let mut plain = limit(1, None);
        assert_eq!(order_fill_price(&mut plain, &bar), None);
        assert!(plain.books_as_maker());
        let mut discretionary = limit(1, Some(1.0));
        assert_eq!(order_fill_price(&mut discretionary, &bar), Some(101.0));
        assert!(!discretionary.books_as_maker(), "a discretionary fill takes liquidity");
        let mut sell = limit(-1, Some(1.0));
        let up = Bar::ohlcv(0, 98.0, 99.5, 97.0, 99.0, 1.0);
        assert_eq!(order_fill_price(&mut sell, &up), Some(99.0));
    }

    /// `order_fill_price_granular` is unchanged: the conversion on one sub-bar survives to the next.
    #[test]
    fn a_conversion_survives_the_later_sub_bars() {
        let mut o = order(StopLimit, 1, 101.0, Some(100.0));
        let subs = [
            Bar::ohlcv(0, 100.5, 101.5, 100.5, 101.0, 1.0),
            Bar::ohlcv(1, 101.0, 101.0, 99.5, 100.0, 1.0),
        ];
        assert_eq!(order_fill_price_granular(&mut o, &subs), Some((100.0, 1)));
        assert_eq!(o.kind, Limit);
    }

    /// A sell's activation is reached from below, at the level included; a buy's from above.
    #[test]
    fn a_trailing_activation_is_reached_from_the_favourable_side() {
        assert!(trailing_activation_reached(-1, 104.0, 104.0));
        assert!(!trailing_activation_reached(1, 104.5, 104.0));
        assert!(trailing_activation_reached(1, 103.5, 104.0));
    }
}
