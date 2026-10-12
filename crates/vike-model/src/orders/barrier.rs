//! The position-executor PARAM types that must live in `vike-model`: [`TripleBarrier`] and
//! [`ControllerParams`]. They are pure serde data (no logic), and they are transitively part of the
//! [`StrategyParams`](crate::StrategyParams) payload the exec ingest lane carries
//! (`StrategyParams::PositionController(ControllerParams)`, whose `barriers` is a `TripleBarrier`),
//! so they must sit in the bottom domain crate below `vike-exec`. The position-executor STATE
//! MACHINE and controller LOGIC that consume them live in the `vike-strategy` crate (above
//! `vike-model`), which imports these back down.

use serde::{Deserialize, Serialize};

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
    /// Leg 3 — the NEW time barrier: force-exit at market once `now >= entry_ts + time_limit_ms`.
    #[serde(default)]
    pub time_limit_ms: Option<i64>,
    /// Optional trailing-stop distance (absolute), ratcheted exactly like
    /// `ConditionalBook::add_trailing`: the extreme is seeded from the entry fill and ratchets on
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

    /// Convenience: build the price legs from BASIS POINTS relative to `ref_px` (e.g. entry price).
    /// `offset = ref_px * bps / 10_000`. `time_limit_ms` passes through unchanged. A `None` bps leg
    /// stays `None`. This is a caller-side ergonomic ONLY — the stored shape is still absolute
    /// offsets, so the pure core stays unit-free and parity-clean (spec Q6).
    pub fn from_bps(
        ref_px: f64,
        take_profit_bps: Option<f64>,
        stop_loss_bps: Option<f64>,
        time_limit_ms: Option<i64>,
        trailing_bps: Option<f64>,
    ) -> Self {
        let to_off = |bps: Option<f64>| bps.map(|b| ref_px * b / 10_000.0);
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
/// reference `MomentumController`'s `threshold`). The harness applies `cooldown_ms` itself and
/// forwards the whole bag to `Controller::apply_params`, which keeps each controller the sole
/// authority on which knobs it absorbs. Pure serde (rides the journaled `Command::UpdateParams`);
/// `f64` fields ⇒ `PartialEq`, no `Eq`. RUST-NATIVE — no Python twin.
///
/// The intent template covers `qty` + `barriers` (the size/barrier defaults every opened
/// `PositionIntent` carries); the entry kind / refresh / retry policies stay at their `PositionIntent`
/// defaults (market entry, no refresh, no retry) in v1 — the reference controller does not expose
/// them, and the bag can grow additively when a controller does.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ControllerParams {
    /// HARNESS knob: post-exit cooldown (ms) before a `(venue, symbol)` may re-open. `0` = none.
    /// Applied by `ControllerHarness::on_params_updated` directly (not by the controller).
    pub cooldown_ms: i64,
    /// CONTROLLER intent-template knob: the position size (units) each opened intent carries.
    pub qty: f64,
    /// CONTROLLER intent-template knob: the triple barrier every opened position is guarded by.
    pub barriers: TripleBarrier,
    /// CONTROLLER decision knob: the reference `MomentumController`'s momentum threshold (the
    /// absolute price move that arms a long/short). A controller that has no such knob ignores it.
    pub threshold: f64,
}

impl ControllerParams {
    /// A params bag from its parts (harness cooldown + controller intent template + threshold).
    pub fn new(cooldown_ms: i64, qty: f64, barriers: TripleBarrier, threshold: f64) -> Self {
        ControllerParams { cooldown_ms, qty, barriers, threshold }
    }
}
