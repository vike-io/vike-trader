//! The position-executor PARAM types that must live in `vike-model`: [`TripleBarrier`] and
//! [`PositionControllerParams`]. They are pure serde data (no logic), and they are transitively part of the
//! [`StrategyParams`](crate::StrategyParams) payload the exec ingest lane carries
//! (`StrategyParams::PositionController(PositionControllerParams)`, whose `barriers` is a `TripleBarrier`),
//! so they must sit in the bottom domain crate below `vike-execution`. The position-executor STATE
//! MACHINE and controller LOGIC that consume them live in the `vike-strategy` crate (above
//! `vike-model`), which imports these back down.
//!
//! The barrier ARITHMETIC lives here too ([`take_profit_price`], [`stop_loss_price`],
//! [`time_barrier_hit`], [`evaluate_barriers`], [`evaluate_barriers_at_price`], [`TripleBarrierKind`],
//! [`TripleBarrierHit`]): the order layer is the one place a level off an entry fill is computed, so the
//! position executor and any strategy language take it from here (decision 0269, item 3: moved
//! unchanged out of `vike-strategy`'s `position_executor`, same derives and serde names).

use serde::{Deserialize, Serialize};
use vike_marketdata::Bar;

use crate::{OrderKind, OrderResting, one_price_bar, order_fill_price};

/// The triple barrier: take-profit, stop-loss, AND a time deadline (spec §2/§3.1).
///
/// Each price leg is an OPTIONAL absolute price OFFSET from the entry fill (see module docs);
/// `time_limit_ms` is the max holding duration. All fields default to `None` (an unbounded, un-armed
/// position) and are `#[serde(default)]`, so a partial JSON is additive.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct TripleBarrier {
    /// Leg 1 — take-profit: favourable price offset from entry. Long exits at `entry + tp`, short at
    /// `entry - tp`. Detected as a reduce-only LIMIT (favourable gap improves the fill).
    #[serde(default)]
    pub take_profit: Option<f64>,
    /// Leg 2 — stop-loss: adverse price offset from entry. Long exits at `entry - sl`, short at
    /// `entry + sl`. Detected as a reduce-only STOP (adverse gap worsens the fill).
    #[serde(default)]
    pub stop_loss: Option<f64>,
    /// Leg 3 — the NEW time barrier: force-exit at market once `now >= entry_timestamp + time_limit_ms`.
    #[serde(default)]
    pub time_limit_ms: Option<i64>,
    /// Optional trailing-stop distance (absolute), ratcheted exactly like
    /// `OrderEmulationBook::add_trailing`: the extreme is seeded from the entry fill and ratchets on
    /// every non-firing check.
    #[serde(default)]
    pub trailing: Option<f64>,
}

impl TripleBarrier {
    /// All barriers armed from raw absolute price offsets (+ ms + trailing distance).
    pub fn new(
        take_profit: Option<f64>,
        stop_loss: Option<f64>,
        time_limit_ms: Option<i64>,
        trailing: Option<f64>,
    ) -> Self {
        TripleBarrier { take_profit, stop_loss, time_limit_ms, trailing }
    }

    /// No barriers (same as [`Default`]): an un-armed position.
    pub fn none() -> Self {
        TripleBarrier::default()
    }

    /// Convenience: build the price legs from BASIS POINTS relative to `ref_price` (e.g. entry price).
    /// `offset = ref_price * bps / 10_000`. `time_limit_ms` passes through unchanged. A `None` bps leg
    /// stays `None`. This is a caller-side ergonomic ONLY — the stored shape is still absolute
    /// offsets, so the pure core stays unit-free and parity-clean (spec Q6).
    pub fn from_bps(
        ref_price: f64,
        take_profit_bps: Option<f64>,
        stop_loss_bps: Option<f64>,
        time_limit_ms: Option<i64>,
        trailing_bps: Option<f64>,
    ) -> Self {
        let to_off = |bps: Option<f64>| bps.map(|b| ref_price * b / 10_000.0);
        TripleBarrier {
            take_profit: to_off(take_profit_bps),
            stop_loss: to_off(stop_loss_bps),
            time_limit_ms,
            trailing: to_off(trailing_bps),
        }
    }
}

/// `StrategyParams::PositionController` live-params update (the controller → `PositionExecutor`
/// twin of [`crate::SpreadMakerParams`]), consumed by `ControllerHarness::on_params_updated` (in the
/// `vike-strategy` crate). A flat `Copy` bag spanning the TWO layers a re-tune touches: the HARNESS
/// knob (`cooldown_ms`) and the CONTROLLER knobs (the intent template `qty`/`barriers` + the
/// shipped momentum file's `MomentumController`'s `threshold`, `strategies/trend/momentum.rs`).
/// The harness applies `cooldown_ms` itself and
/// forwards the whole bag to `Controller::apply_params`, which keeps each controller the sole
/// authority on which knobs it absorbs. Pure serde (rides the journaled `Command::UpdateParams`);
/// `f64` fields ⇒ `PartialEq`, no `Eq`. RUST-NATIVE — no Python twin.
///
/// The intent template covers `qty` + `barriers` (the size/barrier defaults every opened
/// `PositionExecutorIntent` carries); the entry kind / refresh / retry policies stay at their `PositionExecutorIntent`
/// defaults (market entry, no refresh, no retry) in v1 — the reference controller does not expose
/// them, and the bag can grow additively when a controller does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionControllerParams {
    /// HARNESS knob: post-exit cooldown (ms) before a `(venue, symbol)` may re-open. `0` = none.
    /// Applied by `ControllerHarness::on_params_updated` directly (not by the controller).
    pub cooldown_ms: i64,
    /// CONTROLLER intent-template knob: the position size (units) each opened intent carries.
    pub qty: f64,
    /// CONTROLLER intent-template knob: the triple barrier every opened position is guarded by.
    pub barriers: TripleBarrier,
    /// CONTROLLER decision knob: the momentum file's `MomentumController`'s momentum threshold (the
    /// absolute price move that arms a long/short). A controller that has no such knob ignores it.
    pub threshold: f64,
}

impl PositionControllerParams {
    /// A params bag from its parts (harness cooldown + controller intent template + threshold).
    pub fn new(cooldown_ms: i64, qty: f64, barriers: TripleBarrier, threshold: f64) -> Self {
        PositionControllerParams { cooldown_ms, qty, barriers, threshold }
    }
}

/// Which barrier of the triple fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripleBarrierKind {
    /// Favourable price target reached (leg 1).
    TakeProfit,
    /// Adverse hard stop crossed (leg 2).
    StopLoss,
    /// Trailing stop crossed (a ratcheting variant of the stop).
    Trailing,
    /// Max holding duration elapsed (leg 3).
    Time,
}

/// The result of a barrier check: WHICH barrier fired and the oracle's trigger fill price.
///
/// `exit_price` is the [`crate::order_fill_price`] trigger price (gap-adjusted) for a price
/// barrier, or the mark (`bar.close`) for a time exit. DIAGNOSTIC only: the real close fill is the
/// venue's / sim engine's (as `OrderEmulationBook`'s `OrderEmulationFire::trigger_price`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TripleBarrierHit {
    pub kind: TripleBarrierKind,
    pub exit_price: f64,
}

// --------------------------------------------------------------------------------------------
// Pure barrier-check free functions
// --------------------------------------------------------------------------------------------

/// The absolute take-profit PRICE for a position (`side` +1 long / -1 short) entered at `entry_price`,
/// given a positive price `offset`. Long targets ABOVE (`entry + offset`), short BELOW.
pub fn take_profit_price(entry_price: f64, side: i32, offset: f64) -> f64 {
    if side >= 0 { entry_price + offset } else { entry_price - offset }
}

/// The absolute stop-loss PRICE. Long stops BELOW (`entry - offset`), short ABOVE.
pub fn stop_loss_price(entry_price: f64, side: i32, offset: f64) -> f64 {
    if side >= 0 { entry_price - offset } else { entry_price + offset }
}

/// The time-barrier predicate: `true` once `now` reaches `entry_timestamp + time_limit_ms` (AT the deadline
/// and after, never before). Pure, injected epoch-ms; `saturating_add` guards i64 overflow.
pub fn time_barrier_hit(entry_timestamp: i64, time_limit_ms: i64, now: i64) -> bool {
    now >= entry_timestamp.saturating_add(time_limit_ms)
}

/// The exit side of a position (`+1` long / anything-else short): the OPPOSITE side, reduce-only.
#[inline]
fn exit_side(position_side: i32) -> i32 {
    if position_side >= 0 { -1 } else { 1 }
}

/// Evaluate ALL armed `barriers` for a position (`side`, `entry_price`, `entry_timestamp`) against `now` and
/// `bar`, returning the FIRST hit per the precedence below, or `None` if the position survives.
///
/// Price barriers go through [`crate::order_fill_price`] (the gap-open oracle): stop-loss as a
/// reduce-only STOP, take-profit as a reduce-only LIMIT, trailing as a TRAILING order whose extreme
/// lives in `trail_extreme`, RATCHETED in place and seeded from `entry_price` when `None` (as
/// `OrderEmulationBook::add_trailing` arms off the entry mark). Time uses [`time_barrier_hit`].
///
/// ## Precedence (fixed, deterministic): StopLoss → Trailing → TakeProfit → Time
/// The intrabar path is unknown, so the ADVERSE barriers are assumed to hit first (the backtest
/// engines' no-look-ahead-optimism convention): a bar straddling BOTH the stop and the target
/// resolves to the stop, and a position past its deadline that ALSO crossed a price barrier reports
/// the price barrier (time is always evaluated LAST).
pub fn evaluate_barriers(
    entry_price: f64,
    side: i32,
    entry_timestamp: i64,
    barriers: &TripleBarrier,
    now: i64,
    bar: &Bar,
    trail_extreme: &mut Option<f64>,
) -> Option<TripleBarrierHit> {
    let exit = exit_side(side);

    // 1. Hard stop-loss — the adverse barrier, checked first (pessimistic).
    if let Some(offset) = barriers.stop_loss {
        let mut o = OrderResting::new(OrderKind::Stop, exit, 0.0);
        o.price = Some(stop_loss_price(entry_price, side, offset));
        if let Some(fp) = order_fill_price(&mut o, bar) {
            return Some(TripleBarrierHit { kind: TripleBarrierKind::StopLoss, exit_price: fp });
        }
    }

    // 2. Trailing stop — also protective; seed + ratchet the extreme in place.
    if let Some(trail) = barriers.trailing {
        let seed = *trail_extreme.get_or_insert(entry_price);
        let mut o = OrderResting::new(OrderKind::Trailing, exit, 0.0);
        o.trail = Some(trail);
        o.extreme = Some(seed);
        let hit = order_fill_price(&mut o, bar);
        // On a non-fire `order_fill_price` ratcheted `o.extreme`; on a fire it left it unchanged.
        // Persist it either way (moot on a fire — the position is closing).
        *trail_extreme = o.extreme;
        if let Some(fp) = hit {
            return Some(TripleBarrierHit { kind: TripleBarrierKind::Trailing, exit_price: fp });
        }
    }

    // 3. Take-profit — the favourable barrier, only after the stops are cleared.
    if let Some(offset) = barriers.take_profit {
        let mut o = OrderResting::new(OrderKind::Limit, exit, 0.0);
        o.price = Some(take_profit_price(entry_price, side, offset));
        if let Some(fp) = order_fill_price(&mut o, bar) {
            return Some(TripleBarrierHit { kind: TripleBarrierKind::TakeProfit, exit_price: fp });
        }
    }

    // 4. Time barrier — the clock catch-all, checked last.
    if let Some(limit) = barriers.time_limit_ms
        && time_barrier_hit(entry_timestamp, limit, now)
    {
        return Some(TripleBarrierHit { kind: TripleBarrierKind::Time, exit_price: bar.close });
    }

    None
}

/// [`evaluate_barriers`] at a single tick `px` — a degenerate OHLC bar at `px` (open=high=low=close),
/// mirroring `OrderEmulationBook::check_price`. The convenience for the quote/trade tick path.
pub fn evaluate_barriers_at_price(
    entry_price: f64,
    side: i32,
    entry_timestamp: i64,
    barriers: &TripleBarrier,
    now: i64,
    px: f64,
    trail_extreme: &mut Option<f64>,
) -> Option<TripleBarrierHit> {
    evaluate_barriers(
        entry_price,
        side,
        entry_timestamp,
        barriers,
        now,
        &one_price_bar(now, px),
        trail_extreme,
    )
}

#[cfg(test)]
#[path = "triple_barrier_tests.rs"]
mod triple_barrier_tests;
