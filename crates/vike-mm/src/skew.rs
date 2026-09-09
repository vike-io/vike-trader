//! Inventory-skew size shaping + the fill-rate breaker's round-trip netting — the pure risk
//! helpers of [`SpreadMaker`](crate::SpreadMaker), isolated so they unit-test without a runtime.
//! Split out of the crate root; behavior is byte-identical (whole functions moved verbatim).

use std::collections::VecDeque;

/// One recorded fill for the sliding-window netting: `side` (+1 bid/buy, −1 ask/sell), `size`, and
/// the fill's EVENT timestamp (epoch-ms). Wall-clock is never read — expiry/windowing is driven by
/// the event ts on fills and quote ticks, matching the runtime's `now_ms`-fed determinism.
#[derive(Clone, Copy)]
pub(crate) struct FillRec {
    pub(crate) side: i32,
    pub(crate) size: f64,
    pub(crate) ts: i64,
}

/// Pure inventory-skew size multipliers — the whole risk logic, isolated so it can be unit-tested
/// without a runtime. Returns `(bid_mult, ask_mult)` to apply to the base quote size.
///
/// Let `imbalance = clamp((position − target) / max_inventory, −1, 1)`. Then
/// `bid_mult = max(1 − skew·imbalance, 0)` and `ask_mult = max(1 + skew·imbalance, 0)`.
///
/// Properties (all covered by the `tests` module):
/// - **neutral**: `skew <= 0` (or a degenerate `max_inventory <= 0`) → exactly `(1.0, 1.0)` for
///   any position, reproducing the fixed-size path bit-for-bit;
/// - **directional**: long (`position > target`) shrinks the bid and grows the ask; short mirrors;
/// - **symmetric** about the target: `bid(+d) == ask(−d)` and `ask(+d) == bid(−d)`;
/// - **monotonic**: bid non-increasing, ask non-decreasing as position rises;
/// - **clamped**: beyond `±max_inventory` the multipliers saturate at `(1∓skew)`, floored at `0`.
pub(crate) fn skew_multipliers(
    position: f64,
    target: f64,
    max_inventory: f64,
    skew: f64,
) -> (f64, f64) {
    // Disabled (skew <= 0) or a degenerate band (<= 0) → neutral: BOTH multipliers exactly 1.0.
    // This is what makes the neutral default reduce to fixed-size behavior, and it guards the
    // division below against a zero/negative band. `<=` (not `== 0.0`) keeps it float-lint clean
    // and treats a nonsensical negative skew as "off" rather than anti-skewing.
    if skew <= 0.0 || max_inventory <= 0.0 {
        return (1.0, 1.0);
    }
    // signed, unit-normalized inventory imbalance, saturating at ±1 outside the band
    let imbalance = ((position - target) / max_inventory).clamp(-1.0, 1.0);
    // long (imbalance > 0) → smaller bid, larger ask (lean to sell the position down); short → the
    // mirror. Floored at 0 so an over-unit `skew` can never request a negative size.
    let bid = (1.0 - skew * imbalance).max(0.0);
    let ask = (1.0 + skew * imbalance).max(0.0);
    (bid, ask)
}

/// Pure round-trip netting — the whole trip decision, isolated so it can be unit-tested without a
/// runtime. Sums the SIGNED fill size within the window `[now − window_ms, now]`: `+size` per bid
/// (buy, `side > 0`) fill, `−size` per ask (sell, `side < 0`) fill.
///
/// The sign IS the round-trip cancellation: a bid fill (`+size`) later offset by an equal ask fill
/// (`−size`) sums back toward `0` — a completed, inventory-neutral round-trip that must NOT trip
/// the guard. Only NET one-directional accumulation survives the sum:
/// - result `>= +threshold` → bids are being repeatedly hit without offsetting asks (adverse
///   selection on the bid, e.g. a falling market lifting the bid) → suppress the BID;
/// - result `<= −threshold` → the mirror → suppress the ASK.
///
/// EVENT time only: `now` and every `f.ts` are fill/quote event stamps, never wall-clock.
pub(crate) fn net_signed_fills(fills: &VecDeque<FillRec>, now: i64, window_ms: i64) -> f64 {
    let cutoff = now - window_ms;
    fills.iter().filter(|f| f.ts >= cutoff).map(|f| (f.side.signum() as f64) * f.size).sum()
}
