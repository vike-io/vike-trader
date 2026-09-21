//! LIVE `RiskGate` <-> BACKTEST `SimBroker` reconciliation as a GATE — the risk-model twin of
//! `engine_kernel_parity.rs` (which reconciles the event engine against the vectorized kernel).
//!
//! # Why this exists
//!
//! Four real money bugs were found on 2026-07-19 by manually asking "where is this rule
//! implemented twice?". NOT ONE was caught by a test:
//!
//! * **#458** — `RiskGate` DENIED a below-min CLOSING order that `SimBroker` fills, stranding a
//!   position live that the backtest showed flattening cleanly.
//! * **#458** — the gate's notional omitted the contract multiplier that `SimBroker::apply_fill`
//!   includes, so the same `min_notional` judged a mult != 1 instrument differently on each side.
//! * **#468** — the published snapshot's margin fold skipped positions the gate counts.
//! * **#479** — production passed `SEED_CASH` as a contract multiplier.
//!
//! Since deny-vs-clamp PHASE 2 there IS a shared implementation on the pre-trade lane:
//! **`SimBroker::gate_market_order` calls the literal `vike_exec::RiskGate::check`** — one
//! formula, one code path, one reason-string vocabulary — so the margin/floor ADMISSION verdict
//! is the same function evaluated on both sides and this matrix asserts that the CONTEXT each
//! side builds (`RiskContext` from live account state vs from `SimBroker` state) produces the
//! same decisions. What still genuinely differs is the venue-side FILL lane:
//! `SimBroker::apply_fill`'s point-in-time grid check at fill time (direction-only opening
//! split), which is where the remaining pinned divergence lives. For each row the two decisions
//! must AGREE, or the row must PIN the difference with a named reason.
//!
//! **The pinning is the value.** An unpinned divergence fails the test (`Row::divergence` is
//! `None` exactly when the two sides agree — see [`check`]); a newly-pinned one is a loud,
//! reviewable diff rather than a silent behavior change.
//!
//! # The two decision surfaces, as of this commit
//!
//! `RiskGate::check` (`vike-exec/src/risk.rs`, `check_inner`) — ordered ladder, first hit denies:
//! side validity -> `TradingState` (halt / reduce-only) -> reduce-only overshoot -> tick/lot
//! normalization -> non-positive size -> **min_qty** (bypassed by `is_covered_reduce`) ->
//! **min_notional** (same bypass, over `order_notional(qty, ref_price, multiplier)`) ->
//! `max_notional_per_order` -> `max_total_exposure` -> **buying power** (bypassed by
//! `is_covered_reduce`, credited `closing_credit` on reversals) -> impact veto (book-only, off
//! here) -> throttle (DISARMED in the backtest mount — wall-clock, meaningless in sim time).
//!
//! `SimBroker` — the SAME gate pre-trade, plus the venue-side fill lane:
//! * `gate_market_order` (pre-trade, `submit`/`submit_market_close` only): crosses the order
//!   through the mounted live `RiskGate` over a `RiskContext` mirroring the live
//!   `gate_and_register` construction (`equity_now`, the pending-aware `margin_in_use` fold,
//!   the LEAN reversing `closing_credit`), armed via `EngineParams::risk_limits` or the
//!   `leverage -> im_requirement = 1/leverage` mapping. A refusal is whole and recorded in
//!   `SimBroker::dropped` under the GATE's own reason string. The pre-phase-1 silent
//!   TRUNCATION survives verbatim behind the opt-in `EngineParams::clamp_to_leverage` (which
//!   unmounts the gate).
//! * `apply_fill` (at fill time): snaps price/size to the PIT grid, then rejects an
//!   **opening/increasing** fill below `min_qty`/`min_notional`. The opening split is
//!   DIRECTION-ONLY (`!is_reducing_direction`) — a closing fill ALWAYS executes so a position is
//!   never stranded, and a below-min REVERSAL executes WHOLE.
//!
//! Both sides are expressed here in the shared `vike_model` predicates #481 hoisted
//! (`is_covered_reduce`, `is_reducing_direction`, `order_notional`) so the scenarios speak one
//! vocabulary.
//!
//! # Harness shape
//!
//! Each row drives BOTH sides over one `(order, account-state, limits)` triple:
//! * LIVE — a `RiskGate` built from the row's limits, crossed with a `RiskContext` carrying the
//!   row's position/mark/multiplier/equity. Decision = `RiskVerdict::ok`.
//! * BACKTEST — a 4-bar `StrategyEngine` run (the `properties_fills.rs` / `engine_kernel_parity.rs`
//!   construction): bar 0 submits the SETUP order that establishes the row's position, bar 1
//!   submits the ORDER UNDER TEST, and it fills at bar 2's open. Decision = "did the full
//!   requested qty reach the position".
//!
//! The PIT grid is deliberately time-varying: UNCONSTRAINED up to the test order's fill bar, the
//! row's grid from there on. That is the only way to establish a position that the row's own
//! floors would themselves have refused (the same PIT-widening trick
//! `properties_fills::closing_dust_after_step_widens_still_closes` uses), and it keeps the setup
//! leg from perturbing the decision under test.

use std::sync::{Arc, Mutex};

use proptest::prelude::*;
use vike_backtest::{EngineParams, SimBroker, StrategyEngine};
use vike_exec::{RiskContext, RiskGate, RiskLimits};
use vike_model::{Bar, OrderRequest, Strategy, SymbolProperties};

const SYM: &str = "SYM0";
/// The ACCOUNT lane's second instrument — registered only for rows that declare an
/// `other_position`, so every other row keeps the one-symbol engine it always had.
const SYM2: &str = "SYM1";
const VENUE: &str = "TEST";
const T0: i64 = 1_700_000_000_000;
const BAR_MS: i64 = 60_000;
/// The order under test fills at bar 2's open; the grid arms from that timestamp on.
const TEST_FILL_TS: i64 = T0 + 2 * BAR_MS;

// ---------------------------------------------------------------------------------------------
// Scenario table
// ---------------------------------------------------------------------------------------------

/// A pinned live/backtest disagreement. A row carries `Some` EXACTLY when the two decisions
/// differ; [`check`] fails both ways (an unpinned divergence AND a pin that no longer bites).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Divergence {
    /// KNOWN-INTENTIONAL (#458 residual, documented in `risk.rs`): a below-min REVERSAL
    /// (`|qty| > |position|`, flipping through flat) is NOT an `is_covered_reduce` — it OPENS the
    /// far side — so the gate denies it, while `SimBroker::apply_fill`'s direction-only opening
    /// split executes the flip WHOLE. The gate is deliberately on the conservative side: the
    /// capped flatten is admitted, so nothing is ever stranded. The eventual unification is
    /// SimBroker-side (split a flip and floor-gate its opening half) — do NOT "fix" it here.
    ///
    /// NOTE these rows run with the pre-trade gate UNARMED (no `leverage`, no `risk_limits`),
    /// so the divergence is between the live gate and the FILL lane — the pre-trade lane being
    /// one shared judge since phase 2 cannot retire it.
    BelowMinReversal,
    // `MarginFormulaDivergesPendingPhase2` is GONE — deny-vs-clamp phase 2 retired it: the
    // backtest now calls the actual `RiskGate`, so the margin-lane rows below are agreement
    // assertions (including `required_free_bp_pct`, which flows to the backtest verbatim via
    // `EngineParams::risk_limits`).
}

struct Row {
    name: &'static str,
    /// signed position established before the order under test (`0.0` = flat)
    position: f64,
    side: i32,
    qty: f64,
    reduce_only: bool,
    price: f64,
    multiplier: f64,
    /// `0.0` = unconstrained (the `SymbolProperties` convention `nz_step` encodes)
    min_qty: f64,
    min_notional: f64,
    /// LIVE initial-margin fraction (`1/leverage`); `None` = the buying-power check is off
    im: Option<f64>,
    /// `RequiredFreeBuyingPowerPercent` haircut — BOTH sides since phase 2: live via
    /// `RiskLimits`, backtest via `EngineParams::risk_limits` (see [`sim_outcome`])
    free_bp_pct: f64,
    /// BACKTEST margin arming via the `leverage -> im_requirement = 1/leverage` mapping into
    /// the mounted gate (`SimBroker::build_risk_gate`); `None` = no gate
    leverage: Option<f64>,
    /// BACKTEST `EngineParams::clamp_to_leverage` — `false` (the production default) = deny
    /// whole; `true` = the opt-in pre-phase-1 truncation (exercised only by the shape tests)
    clamp: bool,
    /// account equity — LIVE `RiskContext::equity`, BACKTEST `EngineParams::cash`
    equity: f64,
    /// **The ACCOUNT-AGGREGATE ceiling** (`RiskLimits::max_account_exposure`), both sides: LIVE
    /// through the row's own limits, BACKTEST through `EngineParams::risk_limits`. `None` (every
    /// pre-existing row) leaves the lane unarmed and both engines byte-identical to before it
    /// existed.
    account_cap: Option<f64>,
    /// A signed position in a SECOND symbol, established before the order under test — the only
    /// way this harness can say anything about a CROSS-SYMBOL axis at all.
    ///
    /// ⚠ It is what makes the account rows a real reconciliation rather than a restatement of the
    /// per-symbol lane: with one symbol the account sum is empty and `max_account_exposure`
    /// collapses onto the projection `max_total_exposure` already judges. `0.0` (every
    /// pre-existing row) keeps the backtest a ONE-symbol engine, so nothing about those rows
    /// moves — see [`sim_outcome`].
    other_position: f64,
    /// expected LIVE verdict
    live_ok: bool,
    /// expected BACKTEST outcome: the FULL requested qty reached the position
    sim_filled: bool,
    /// `Some` exactly when `live_ok != sim_filled`
    divergence: Option<Divergence>,
}

impl Default for Row {
    fn default() -> Self {
        Row {
            name: "",
            position: 0.0,
            side: 1,
            qty: 1.0,
            reduce_only: false,
            price: 100.0,
            multiplier: 1.0,
            min_qty: 0.0,
            min_notional: 0.0,
            im: None,
            free_bp_pct: 0.0,
            leverage: None,
            clamp: false,
            equity: 1_000_000.0,
            account_cap: None,
            other_position: 0.0,
            live_ok: true,
            sim_filled: true,
            divergence: None,
        }
    }
}

/// The matrix: opening / closing / reversing x below / at / above each floor x multiplier 1 and
/// != 1 x flat / long / short x `reduce_only` on and off, plus the margin lane.
fn matrix() -> Vec<Row> {
    vec![
        // ---------------------------------------------------------------------------------
        // Baseline: no floors armed. Both sides must admit/fill unconditionally.
        // ---------------------------------------------------------------------------------
        Row { name: "unconstrained/open-long/flat", ..Row::default() },
        Row { name: "unconstrained/open-short/flat", side: -1, ..Row::default() },
        Row {
            name: "unconstrained/close-long",
            position: 5.0,
            side: -1,
            qty: 5.0,
            ..Row::default()
        },
        Row {
            name: "unconstrained/close-short",
            position: -5.0,
            side: 1,
            qty: 5.0,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // min_qty — the #458 anti-stranding rule and its boundaries.
        // ---------------------------------------------------------------------------------
        Row {
            // OPENING below the floor: gate denies `below-min-qty`, apply_fill drops `min_qty`.
            name: "min_qty/opening-below/flat",
            qty: 0.005,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // AT the floor — `<` on both sides, so the boundary value is admitted by both.
            name: "min_qty/opening-at-floor/flat",
            qty: 0.01,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // ABOVE the floor.
            name: "min_qty/opening-above/flat",
            qty: 0.02,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // #458 REGRESSION PIN (long): a covered dust flatten below the floor must reach the
            // venue on BOTH sides. This is the bug — gate denied, SimBroker filled — that
            // stranded a position live.
            name: "min_qty/covered-flatten-below/long",
            position: 0.005,
            side: -1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // ...and the short twin.
            name: "min_qty/covered-flatten-below/short",
            position: -0.005,
            side: 1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // A PARTIAL reduce (qty < |position|) below the floor — covered, so both execute.
            name: "min_qty/partial-reduce-below/long",
            position: 0.02,
            side: -1,
            qty: 0.005,
            min_qty: 0.01,
            ..Row::default()
        },
        Row {
            // KNOWN-INTENTIONAL DIVERGENCE: below-min REVERSAL. 0.008 > |0.005| flips through
            // flat, so the gate refuses (it opens the far side) while apply_fill's
            // direction-only split executes the whole flip.
            name: "min_qty/reversal-below/long",
            position: 0.005,
            side: -1,
            qty: 0.008,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        Row {
            // ...and the short twin of the reversal.
            name: "min_qty/reversal-below/short",
            position: -0.005,
            side: 1,
            qty: 0.008,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // reduce_only — the #481 `is_covered_reduce` contract. SimBroker has NO reduce_only
        // concept; it derives closing-ness from the ACTUAL position, which is exactly why the
        // gate's FLOOR bypass refuses to trust the bare flag.
        // ---------------------------------------------------------------------------------
        Row {
            // FLAT book + reduce_only flag is an OPENING order: the flag must buy nothing at the
            // floor. Both refuse. (Collapsing `is_covered_reduce` to `reduce_only || implicit`
            // would break this row.)
            name: "reduce_only/flat-book-flag-below-floor",
            qty: 0.005,
            reduce_only: true,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // COVERED reduce_only below the floor: both execute.
            name: "reduce_only/covered-below-floor/long",
            position: 5.0,
            side: -1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            ..Row::default()
        },
        Row {
            // #600-P4 FIX, floor lane (long): reduce_only on a SAME-direction ADD that the
            // position "covers" in magnitude (long 5, BUY 2 tagged reduce_only). Pre-fix this was
            // a LATENT ASYMMETRY: `is_covered_reduce`'s flag arm was direction-blind
            // (`reduce_only && |pos| >= |qty|`), so the live gate ADMITTED this exposure-INCREASING
            // order with the floor bypassed, while `apply_fill`'s direction-only split called it
            // the OPENING fill it is and floor-gated it. The flag arm now also requires the
            // reducing direction, so the gate denies `below-min-qty` — AGREEING with the backtest,
            // and with binance/bybit (which reject a reduce_only order that would increase a
            // position). No `Divergence` pin: the two engines now match.
            name: "reduce_only/covered-same-direction-add-below-floor/long",
            position: 5.0,
            side: 1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // ...and the short twin (short 5, SELL 2 tagged reduce_only).
            name: "reduce_only/covered-same-direction-add-below-floor/short",
            position: -5.0,
            side: -1,
            qty: 2.0,
            reduce_only: true,
            min_qty: 10.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // min_notional twin: 0.02 * 100 = 2.0 notional under the 5.0 floor, same shape.
            name: "reduce_only/covered-same-direction-add-below-notional",
            position: 0.05,
            side: 1,
            qty: 0.02,
            reduce_only: true,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // GUARD row: the same same-direction flagged add with NO floors armed is an ordinary
            // opening order — both sides execute it. Pins that the fix is bypass-removal (the add
            // faces the normal opening-order checks), NOT a blanket local deny of the
            // venue-contradictory flag: locally the venue rejects it server-side; the gate just
            // stops trusting the flag to skip its checks.
            name: "reduce_only/covered-same-direction-add-unconstrained",
            position: 5.0,
            side: 1,
            qty: 2.0,
            reduce_only: true,
            ..Row::default()
        },
        Row {
            // reduce_only asserted on an order the position does NOT cover — a reversal. The flag
            // does not launder it past the floor on either side; the divergence is the reversal
            // one above, not a flag one.
            name: "reduce_only/uncovered-reversal-below-floor",
            position: 0.005,
            side: -1,
            qty: 0.008,
            reduce_only: true,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // min_notional, multiplier 1 — the same three boundaries.
        // ---------------------------------------------------------------------------------
        Row {
            name: "min_notional/opening-below/flat",
            qty: 0.02,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // 0.05 * 100 = 5.0 exactly at the floor — `<` on both sides admits it.
            name: "min_notional/opening-at-floor/flat",
            qty: 0.05,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            name: "min_notional/covered-flatten-below/long",
            position: 0.02,
            side: -1,
            qty: 0.02,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            name: "min_notional/reversal-below/long",
            position: 0.02,
            side: -1,
            qty: 0.03,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: true,
            divergence: Some(Divergence::BelowMinReversal),
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // multiplier != 1 — #458's second finding. The gate's notional omitted `ctx.multiplier`
        // while `apply_fill` gates on `rounded * price * multiplier`; these rows are the
        // regression pin. Under the OLD gate the first row denied live and filled in backtest.
        // ---------------------------------------------------------------------------------
        Row {
            // qty*price = 2.0 (under the 5.0 floor) but *10 multiplier = 20.0 -> both admit.
            name: "multiplier10/notional-cleared-by-multiplier",
            qty: 2.0,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            // still under the floor even WITH the multiplier: 0.2 * 1 * 10 = 2.0 < 5.0.
            name: "multiplier10/notional-below-even-with-multiplier",
            qty: 0.2,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // multiplier interacts with the COVERED-reduce bypass identically on both sides.
            name: "multiplier10/covered-flatten-below-notional",
            position: 0.2,
            side: -1,
            qty: 0.2,
            price: 1.0,
            multiplier: 10.0,
            min_notional: 5.0,
            ..Row::default()
        },
        Row {
            // multiplier is NOT part of the min_qty floor on either side (it is a QUANTITY floor).
            name: "multiplier10/min-qty-is-unaffected-by-multiplier",
            qty: 0.005,
            price: 1.0,
            multiplier: 10.0,
            min_qty: 0.01,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // The margin lane. Since deny-vs-clamp PHASE 2 there is ONE formula: the backtest
        // crosses the literal `RiskGate::check` (leverage mapped to `im = 1/leverage`), so
        // every row here is an AGREEMENT assertion — including the knobs that used to exist
        // only live (`required_free_bp_pct`, `closing_credit`).
        // ---------------------------------------------------------------------------------
        Row {
            // Comfortably inside BOTH: order notional 5_000, margin 5_000 <= free 10_000, and
            // 5_000 <= leverage(1.0) * equity(10_000). Both admit in full.
            name: "margin/inside-both-models",
            qty: 50.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // 1_000 units = 100_000 notional. Live denies `insufficient-margin`; the backtest
            // crosses the SAME gate and denies with the SAME string (pinned in
            // `margin_over_budget_denies_whole_by_default` below). (Pre-phase-1 this row's
            // backtest side TRUNCATED to the 100 units that fit; that shape survives only
            // behind `clamp_to_leverage: true` and is pinned by
            // `clamp_knob_restores_the_truncate_shape_verbatim`.)
            name: "margin/over-budget-both-deny-whole",
            qty: 1_000.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // PHASE-2 FLIP (was `Divergence::MarginFormulaDivergesPendingPhase2`, "no backtest
            // twin"): `required_free_bp_pct` now reaches the backtest verbatim through
            // `EngineParams::risk_limits`. A 50% haircut leaves free BP 5_000, so the 6_000
            // order margin denies — on BOTH sides, same formula, same reason.
            name: "margin/required-free-bp-pct-now-backtest-effective",
            qty: 60.0,
            im: Some(1.0),
            free_bp_pct: 0.5,
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // PHASE-2 POSITIVE ASSERTION, reversal WITHIN the closing credit: long 50, sell 100
            // (an uncovered reversal — `is_covered_reduce` is false, so it faces buying power
            // for its FULL size). Order margin 10_000; free = equity 10_000 − margin_used 5_000
            // + closing_credit 2·50·100·1·1 = 10_000 -> 15_000. Both sides admit, through the
            // same `closing_credit` arithmetic (the retired leverage-room formula had no credit
            // concept — it waved every reducing-direction order through unexamined).
            name: "margin/reversal-within-credit-both-admit",
            position: 50.0,
            side: -1,
            qty: 100.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // PHASE-2 POSITIVE ASSERTION, reversal BEYOND the credit: long 50, sell 300. Order
            // margin 30_000 > free 15_000 -> the gate DENIES on both sides. Under the RETIRED
            // backtest formula this row ADMITTED (direction-only "reducing never denied"), so
            // this is the row where the gate's verdict deliberately CHANGED the backtest —
            // asserted positively, exactly as phase 2 intends (live is the judge; the covered
            // flatten still bypasses, so nothing is ever stranded).
            name: "margin/reversal-beyond-credit-both-deny",
            position: 50.0,
            side: -1,
            qty: 300.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // A COVERED reduce bypasses the margin check — the ONE `is_covered_reduce` bypass,
            // evaluated by the one shared gate on both sides. The position is 100 units, not
            // more: that is the row's OWN margin budget (im 1.0 x 10_000 / 100), and the setup
            // order is subject to the gate like any other (see the harness invariant in
            // `check` — an exactly-fitting setup passes whole under deny AND clamp).
            name: "margin/covered-reduce-bypasses-both",
            position: 100.0,
            side: -1,
            qty: 100.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            ..Row::default()
        },
        Row {
            // RESOLVED by the margin-coverage fix: the gate's buying-power bypass now requires
            // POSITION COVERAGE (`is_covered_reduce`) instead of honoring a bare `reduce_only`
            // flag, so a FLAT book + flag is treated as the opening order it is and faces margin.
            // Both sides now refuse a 100_000-notional order against 10_000 of equity — the live
            // gate denies `insufficient-margin`, SimBroker denies whole. This row previously pinned
            // `Divergence::FlatBookReduceOnlyFlagSkipsMargin`, discovered by this very matrix.
            name: "margin/flat-book-reduce-only-flag-now-faces-margin",
            qty: 1_000.0,
            reduce_only: true,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // multiplier != 1 in the margin lane: 50 units * 100 * mult 10 = 50_000 order
            // margin > free 10_000 -> the one gate denies on both sides; what this row adds is
            // that the shared margin formula carries the multiplier — as `max_total_exposure` now
            // also does (fixed; pinned in
            // `max_total_exposure_includes_the_contract_multiplier`).
            name: "margin/multiplier10-over-budget",
            qty: 50.0,
            multiplier: 10.0,
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ---------------------------------------------------------------------------------
        // The ACCOUNT-AGGREGATE lane (`RiskLimits::max_account_exposure`). The only lane in
        // this file that is CROSS-SYMBOL, so every row here carries an `other_position` — with
        // one symbol the account sum is empty and the lane says nothing the per-symbol
        // projection did not already say.
        //
        // The two sides fold that sum in DIFFERENT code: live in
        // `ExecutionEngine::resolved_account_exposure_excluding` (a walk of the position map and
        // the order registry, `+=`), backtest in `SimBroker::gate_order` (`py_sum` over
        // `symbols`). Same numbers or these rows fail — which is the whole reason they are here
        // rather than in either crate's own suite.
        // ---------------------------------------------------------------------------------
        Row {
            // Inside the ceiling: 50 units of the OTHER symbol at 100 = 5 000, plus this order's
            // projected 10 × 100 = 1 000, against 8 000. Both admit — the non-vacuous half.
            name: "account/inside-the-ceiling",
            qty: 10.0,
            account_cap: Some(8_000.0),
            other_position: 50.0,
            ..Row::default()
        },
        Row {
            // Over it: the same 5 000 elsewhere plus 4 000 here is 9 000 against 8 000. Both
            // deny — and NEITHER side's per-symbol cap is armed at all, so `over-max-exposure`
            // cannot be what stopped it.
            name: "account/over-the-ceiling",
            qty: 40.0,
            account_cap: Some(8_000.0),
            other_position: 50.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        Row {
            // The SHORT twin of the row above: the other symbol's position is negative and the
            // sum is GROSS, so it contributes exactly as much as the long did. A netting fold
            // (or a signed one) reads this account as −5 000 and ADMITS — which is the reading
            // `max_account_exposure`'s doc refuses, and this row is where the two engines would
            // be caught disagreeing about it.
            name: "account/gross-not-net-short-elsewhere",
            qty: 40.0,
            account_cap: Some(8_000.0),
            other_position: -50.0,
            live_ok: false,
            sim_filled: false,
            ..Row::default()
        },
        // ⚠ NO covered-reduce row here, and the omission is reasoned rather than an oversight.
        // Reaching one would need an account ALREADY over the ceiling at the moment of the test
        // order, and every setup leg in this harness crosses the same armed gate — so the setup
        // that put the account over the ceiling would itself have been refused, and the row would
        // silently measure a different scenario (the failure mode `check`'s harness invariant
        // exists to catch). The bypass is not a CONTEXT question anyway: `covered_reduce` is
        // computed inside the shared `RiskGate::check_inner`, which both engines call, so what
        // this file reconciles — each side's ctx construction — cannot differ about it.
        // `crates/vike-exec/tests/risk/risk_lane_completion.rs`'s
        // `the_account_ceiling_never_refuses_a_covered_reduce` is where that arm is proven.
    ]
}

// ---------------------------------------------------------------------------------------------
// LIVE side
// ---------------------------------------------------------------------------------------------

fn live_verdict(row: &Row) -> (bool, String) {
    let limits = RiskLimits {
        min_qty: vike_model::nz_step(row.min_qty),
        min_notional: vike_model::nz_step(row.min_notional),
        im_requirement: row.im,
        required_free_bp_pct: row.free_bp_pct,
        max_account_exposure: row.account_cap,
        ..RiskLimits::new()
    };
    // The margin fields exactly as the LIVE engine builds them (`gate_and_register`): the
    // open-book margin fold — one position here — and the reversing closing credit. Before
    // phase 2 this harness left both 0.0, which under-modeled the live side for any row with a
    // position; the backtest's `gate_market_order` mirrors this same construction, so the
    // matrix now compares the two engines' REAL contexts, not simplified ones.
    let (margin_used, closing_credit) = match limits.im_for(SYM) {
        Some(im) => {
            let used = row.position.abs() * row.price * row.multiplier * im;
            let credit = if row.position != 0.0 && row.side as f64 * row.position < 0.0 {
                2.0 * row.position.abs() * row.price * row.multiplier * im
            } else {
                0.0
            };
            (used, credit)
        }
        None => (0.0, 0.0),
    };
    let ctx = RiskContext {
        position_size: row.position,
        mark_price: row.price,
        equity: row.equity,
        multiplier: row.multiplier,
        margin_used,
        closing_credit,
        // The ACCOUNT-aggregate term, built the way the LIVE producer builds it
        // (`ExecutionEngine::resolved_account_exposure_excluding`): GROSS — `|size|`, never netting
        // — over every symbol but the order's, at the mark and the contract multiplier. Written
        // out here rather than taken from a helper for the same reason `margin_used` above is:
        // this side must state the arithmetic it believes in, so that when the backtest's own fold
        // disagrees the row FAILS instead of both sides quietly sharing one bug.
        //
        // The order's own symbol contributes nothing — the gate re-adds it PROJECTED — and this
        // harness has no resting orders, so the producer's second half is exercised by the
        // crate-local suites (`crates/vike-backtest/tests/limit_orders_are_gated.rs` and
        // `crates/vike-exec/tests/risk/risk_lane_completion.rs`) rather than here.
        account_exposure_excl_order: vike_model::gross_notional(
            row.other_position,
            row.price,
            row.multiplier,
        ),
        ..RiskContext::default()
    };
    let req = OrderRequest {
        client_order_id: "parity".to_string(),
        venue: VENUE.to_string(),
        symbol: SYM.to_string(),
        order_type: "market".to_string(),
        side: row.side,
        qty: row.qty,
        reduce_only: row.reduce_only,
        ..Default::default()
    };
    let v = RiskGate::new(limits).check(&req, &ctx);
    (v.ok, v.reason)
}

// ---------------------------------------------------------------------------------------------
// BACKTEST side
// ---------------------------------------------------------------------------------------------

/// bar 0: submit the SETUP order (establishes `position`). bar 1: the setup fill has landed and
/// the ORDER UNDER TEST is submitted. bar 2: the test order fills at the open. bar 3: drain.
struct SetupThenTest {
    position: f64,
    side: i32,
    qty: f64,
    /// The position ACTUALLY established when the order under test is submitted. Recorded rather
    /// than assumed: `gate_market_order` gates the SETUP order too (denies it whole by default,
    /// truncates it under the clamp knob), so a row that arms the gate can silently start from
    /// a different account state than it declares. [`check`] asserts this equals `Row::position`,
    /// which is what turns that failure mode into a loud harness error instead of a mislabelled
    /// parity result.
    observed_before: Arc<Mutex<f64>>,
    /// The signed position to establish in [`SYM2`] — the ACCOUNT lane's other symbol. `0.0` (every
    /// row but that lane's) submits nothing and the run stays single-symbol.
    other_position: f64,
    /// …and what was ACTUALLY established there, for the same reason `observed_before` exists one
    /// field up: the setup leg for the other symbol crosses the SAME armed gate, so a row whose
    /// account ceiling refused its own setup would silently reconcile a different scenario than it
    /// declares. [`check`] asserts it.
    observed_other: Arc<Mutex<f64>>,
    /// ⚠ `on_bar` fires ONCE PER SYMBOL PER STEP, so with the account lane's second series every
    /// arm below would otherwise run twice. Latched rather than keyed on `bar.symbol` because the
    /// engine re-tags each bar `SYM.VENUE` at construction, and a test that parsed that string
    /// would be asserting on the tagging convention rather than on the gate.
    fired: [bool; 2],
}

impl Strategy<SimBroker> for SetupThenTest {
    fn on_bar(&mut self, ctx: &mut SimBroker, _bar: &Bar) {
        match ctx.index {
            0 if !self.fired[0] => {
                self.fired[0] = true;
                if self.position != 0.0 {
                    let side = if self.position > 0.0 { 1 } else { -1 };
                    ctx.submit(SYM, side, self.position.abs(), 0.0, true, None);
                }
                if self.other_position != 0.0 {
                    let side = if self.other_position > 0.0 { 1 } else { -1 };
                    ctx.submit(SYM2, side, self.other_position.abs(), 0.0, true, None);
                }
            }
            1 if !self.fired[1] => {
                self.fired[1] = true;
                *self.observed_before.lock().expect("uncontended") = ctx.position_of(SYM).size;
                // ⚠ Only when the row asked for a second symbol: a single-symbol run has no
                // `SYM2` registered at all, and asking for its position would panic in `idx`.
                if self.other_position != 0.0 {
                    *self.observed_other.lock().expect("uncontended") = ctx.position_of(SYM2).size;
                }
                ctx.submit(SYM, self.side, self.qty, 0.0, true, None);
            }
            _ => {}
        }
    }
}

/// Returns `(position_before_test_order, other_symbol_position, final_position, dropped)` —
/// `dropped` is the engine's strategy-observable refusal channel (`SimBroker::dropped`), where the
/// default deny-whole gate records the live gate's own reason string (unified as of phase 2, e.g.
/// `"insufficient-margin"`). The second element is the ACCOUNT lane's other symbol, `0.0` for every
/// row that declares none; [`check`] asserts it against the row for the same harness reason the
/// first element is asserted.
#[allow(clippy::type_complexity)] // the (before, other, after, dropped-channel) tuple, test-local
fn sim_outcome(row: &Row) -> (f64, f64, f64, Vec<(String, String, f64, f64)>) {
    let bars: Vec<Bar> = (0..4)
        .map(|i| Bar {
            ts: T0 + i as i64 * BAR_MS,
            open: row.price,
            high: row.price,
            low: row.price,
            close: row.price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(SYM.to_string()),
        })
        .collect();

    // UNCONSTRAINED until the test order's own fill bar, then the row's grid — so the setup leg
    // can establish a position the row's floors would themselves have refused (see the module
    // doc). `step_size`/`tick_size` stay 0.0 (unconstrained) throughout: this file reconciles the
    // FLOOR and MARGIN decisions, and `properties_fills.rs` already gates the snapping.
    let (min_qty, min_notional) = (row.min_qty, row.min_notional);
    #[allow(clippy::type_complexity)]
    let grid: Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync> =
        Arc::new(move |_v, _s, ts| {
            if ts >= TEST_FILL_TS {
                Some(SymbolProperties { min_qty, min_notional, ..Default::default() })
            } else {
                Some(SymbolProperties::default())
            }
        });

    let params = EngineParams {
        cash: row.equity,
        multiplier: row.multiplier,
        leverage: row.leverage,
        clamp_to_leverage: row.clamp,
        // PHASE 2: a live-only knob flows to the backtest VERBATIM via `risk_limits` — armed
        // exactly when the row uses one (`free_bp_pct`); otherwise the `leverage -> im = 1/L`
        // convenience mapping is the config under test.
        // …and the ACCOUNT-aggregate ceiling rides the same seam, so a row that arms it arms the
        // backtest's gate too. Both conditions are ORed rather than one being folded into the
        // other because either knob alone must arm the limits (a row with a ceiling and no
        // free-BP haircut is the account lane's ordinary shape).
        risk_limits: (row.free_bp_pct != 0.0 || row.account_cap.is_some()).then(|| RiskLimits {
            im_requirement: row.im,
            required_free_bp_pct: row.free_bp_pct,
            max_account_exposure: row.account_cap,
            ..RiskLimits::new()
        }),
        default_venue: Some(VENUE.to_string()),
        properties: Some(grid),
        ..Default::default()
    };
    let observed_before = Arc::new(Mutex::new(0.0));
    let observed_other = Arc::new(Mutex::new(0.0));
    let strat = SetupThenTest {
        position: row.position,
        side: row.side,
        qty: row.qty,
        observed_before: Arc::clone(&observed_before),
        other_position: row.other_position,
        observed_other: Arc::clone(&observed_other),
        fired: [false; 2],
    };
    // ⚠ A SECOND SERIES only when the row asks for one. Every pre-existing row keeps the
    // one-symbol engine it always had — `on_bar` fires once per symbol per step, so an
    // unconditional second series would change every row's dispatch count, not just its account
    // sum.
    let series = if row.other_position == 0.0 {
        vec![(SYM.to_string(), bars)]
    } else {
        let other: Vec<Bar> =
            bars.iter().map(|b| Bar { symbol: Some(SYM2.to_string()), ..b.clone() }).collect();
        vec![(SYM.to_string(), bars), (SYM2.to_string(), other)]
    };
    let mut e = StrategyEngine::new(series, strat, params);
    let _ = e.run();
    let before = *observed_before.lock().expect("uncontended");
    let other = *observed_other.lock().expect("uncontended");
    (before, other, e.core.sym[0].pos.size, e.core.dropped.clone())
}

// ---------------------------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------------------------

fn check(row: &Row) {
    let (live_ok, reason) = live_verdict(row);
    let (pos_before, other_before, pos_after, _dropped) = sim_outcome(row);

    // HARNESS INVARIANT, the ACCOUNT lane's half: the second symbol must hold what the row says it
    // holds. Its setup leg crosses the same armed gate, and an account ceiling is exactly the knob
    // that can refuse it — so without this a row whose ceiling ate its own setup would reconcile a
    // scenario in which the account is EMPTY and report it as agreement.
    assert!(
        (other_before - row.other_position).abs() <= 1e-12,
        "[{}] HARNESS: the other symbol holds {other_before}, not the declared {}. The account \
         lane's setup leg was itself gated — raise the row's ceiling above what the setup needs.",
        row.name,
        row.other_position
    );

    // HARNESS INVARIANT: the backtest must actually be in the account state the row declares, or
    // the comparison is between two different scenarios. `gate_market_order` gates the SETUP
    // order too (deny-whole by default), so this is a live failure mode, not a theoretical one.
    assert!(
        (pos_before - row.position).abs() <= 1e-12,
        "[{}] HARNESS: the setup leg established position {pos_before}, not the declared {}. \
         The row's account state was silently gated (gate_market_order applies to the setup \
         order as well) — pick a position that fits the row's margin budget.",
        row.name,
        row.position
    );

    // The backtest "did the order execute" decision: the FULL requested signed qty reached the
    // position. A denied-whole (gate_market_order), truncated (clamp knob) or dropped
    // (below-floor) fill all answer `false`; the two gate shapes are pinned separately below.
    let expected_after = pos_before + row.side as f64 * row.qty;
    let sim_filled = (pos_after - expected_after).abs() <= 1e-12;

    assert_eq!(
        live_ok, row.live_ok,
        "[{}] LIVE verdict changed: expected ok={}, got ok={} (reason={reason:?})",
        row.name, row.live_ok, live_ok
    );
    assert_eq!(
        sim_filled, row.sim_filled,
        "[{}] BACKTEST outcome changed: expected filled={}, got filled={} \
         (position {pos_before} -> {pos_after}, expected {expected_after})",
        row.name, row.sim_filled, sim_filled
    );

    // THE RECONCILIATION. The two engines must agree, or the row must say WHY not — and a pin
    // that no longer bites is just as much a failure as an unpinned divergence.
    reconcile(row.name, live_ok, &reason, sim_filled, pos_before, pos_after, row.divergence);
}

/// The shared RECONCILIATION verdict, used by BOTH the hand matrix ([`check`]) and the proptest
/// arms below. The two engines must AGREE (`live_ok == sim_filled`), unless the scenario is a
/// pinned [`Divergence`] (in which case they MUST differ). It panics on an unpinned divergence (a
/// real live-vs-backtest bug — exactly the class #458/#468/#479 this file exists to catch) or on a
/// pin that no longer bites. This is the single source of the agree-or-pin law; the proptest simply
/// feeds it a STRUCTURALLY-computed `expected` instead of a hand-declared one.
fn reconcile(
    name: &str,
    live_ok: bool,
    reason: &str,
    sim_filled: bool,
    pos_before: f64,
    pos_after: f64,
    expected: Option<Divergence>,
) {
    match (live_ok == sim_filled, expected) {
        (true, None) | (false, Some(_)) => {}
        (true, Some(d)) => panic!(
            "[{name}] pinned divergence {d:?} NO LONGER BITES — both engines now agree \
             (live_ok={live_ok}, sim_filled={sim_filled}). If this was fixed on purpose, drop \
             the pin; the pin must never outlive the divergence."
        ),
        (false, None) => panic!(
            "[{name}] UNPINNED LIVE/BACKTEST DIVERGENCE: the gate says ok={live_ok} \
             (reason={reason:?}) but the backtest says filled={sim_filled} \
             (position {pos_before} -> {pos_after}). This is exactly the class of bug this file \
             exists to catch (#458/#468/#479). Do NOT silence it by editing the expectation: \
             either fix the production divergence in its own PR, or add a named `Divergence` \
             variant documenting why the two engines are allowed to differ here."
        ),
    }
}

/// Structural classifier for the FLOOR lane's ONE documented divergence,
/// [`Divergence::BelowMinReversal`]. With `reduce_only = false` (the proptest floor-lane setting)
/// `is_covered_reduce` collapses to `is_implicit_reduce`, so a REVERSAL — one that opposes the
/// position AND overshoots it (`|qty| > |position|`, flipping through flat) — is the only order
/// that is neither opening nor covered: the gate applies its floors to it (it opens the far side),
/// while `SimBroker::apply_fill` fills the flip WHOLE (direction-only). It therefore diverges
/// EXACTLY when such a reversal is below either armed floor, computed here with the gate's OWN
/// arithmetic (`order_notional`, strict `<`) — the same shared vocabulary the matrix speaks. An
/// opening or covered order never diverges (both sides agree), so this returns `None`; if they ever
/// disagree there, [`reconcile`] fires the unpinned-divergence panic — a real finding.
fn expected_floor_divergence(row: &Row) -> Option<Divergence> {
    let reducing = vike_model::is_reducing_direction(row.side, row.position);
    let is_reversal = reducing && row.qty.abs() > row.position.abs();
    if !is_reversal {
        return None;
    }
    let below_qty = row.min_qty > 0.0 && row.qty.abs() < row.min_qty;
    let notional = vike_model::order_notional(row.qty, row.price, row.multiplier);
    let below_notional = row.min_notional > 0.0 && notional < row.min_notional;
    (below_qty || below_notional).then_some(Divergence::BelowMinReversal)
}

#[test]
fn riskgate_and_simbroker_agree_or_pin_every_divergence() {
    for row in matrix() {
        check(&row);
    }
}

// ---------------------------------------------------------------------------------------------
// PROPTEST arms (testing-arch plan Phase 6, target P4) — the hand matrix above, generalized to
// hundreds of random cases per lane. Both reuse the exact `live_verdict` / `sim_outcome` engines
// and the shared `reconcile` verdict; the curated hand rows stay as the human-readable regression
// pins. The lanes mirror the file's two decision surfaces:
//   * FLOOR lane — gate UNARMED, so `apply_fill`'s fill-time floors are the sole decider and can
//     diverge from the live gate (the one documented `BelowMinReversal` pin);
//   * MARGIN lane — gate ARMED, so both sides cross the SAME `RiskGate::check` and must AGREE
//     (phase 2 unified this lane; a disagreement is the #468 context-construction bug class).
// ---------------------------------------------------------------------------------------------
proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// FLOOR LANE: over random (position, side, qty, price, multiplier, floors) with the pre-trade
    /// gate unarmed, the live min_qty/min_notional gate and `apply_fill`'s fill-time floors AGREE,
    /// except the documented below-min REVERSAL — reconciled through the shared [`reconcile`] with
    /// a structurally-classified `expected`. `reduce_only` is held false: the hand rows cover that
    /// axis, and the flag's `is_covered_reduce` interaction is not a floor-lane divergence.
    #[test]
    fn floor_lane_agrees_or_pins_below_min_reversal(
        position in prop_oneof![Just(0.0f64), -20.0f64..20.0],
        side in prop_oneof![Just(1i32), Just(-1i32)],
        qty in 0.0001f64..40.0,
        price in 0.5f64..2_000.0,
        multiplier in 0.1f64..100.0,
        min_qty in prop_oneof![Just(0.0f64), 0.0001f64..10.0],
        min_notional in prop_oneof![Just(0.0f64), 0.01f64..1_000.0],
    ) {
        let row = Row {
            name: "prop/floor",
            position,
            side,
            qty,
            price,
            multiplier,
            min_qty,
            min_notional,
            reduce_only: false,
            im: None,
            leverage: None,
            clamp: false,
            free_bp_pct: 0.0,
            equity: 1_000_000.0,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        let (before, _other, after, _dropped) = sim_outcome(&row);
        // The gate is unarmed, so the setup leg always establishes the declared position exactly.
        prop_assert!(
            (before - position).abs() <= 1e-9 * (1.0 + position.abs()),
            "harness: setup established {before}, not {position}"
        );
        let expected_after = before + side as f64 * qty;
        let sim_filled = (after - expected_after).abs() <= 1e-9 * (1.0 + expected_after.abs());
        reconcile(
            "prop/floor",
            live_ok,
            &reason,
            sim_filled,
            before,
            after,
            expected_floor_divergence(&row),
        );
    }

    /// MARGIN LANE: with the gate ARMED via `leverage`, both sides cross the SAME `RiskGate::check`
    /// over contexts each builds from its own state. Deny-vs-clamp phase 2 unified this lane, so the
    /// two must AGREE for EVERY random (order, position, leverage, multiplier, equity) — there is no
    /// pinned margin divergence. position/qty are scaled to the leverage room so the setup leg fits
    /// and the test order spans both sides of the budget boundary.
    #[test]
    fn margin_lane_always_agrees(
        leverage in 1.0f64..20.0,
        price in 1.0f64..1_000.0,
        multiplier in 1.0f64..10.0,
        equity in 10_000.0f64..1_000_000.0,
        side in prop_oneof![Just(1i32), Just(-1i32)],
        position_frac in -0.95f64..0.95,
        qty_frac in 0.01f64..3.0,
    ) {
        let im = 1.0 / leverage;
        let room = equity / (price * multiplier * im); // = equity * leverage / (price * mult)
        let row = Row {
            name: "prop/margin",
            position: position_frac * room,
            side,
            qty: qty_frac * room,
            price,
            multiplier,
            min_qty: 0.0,
            min_notional: 0.0,
            reduce_only: false,
            im: Some(im),
            leverage: Some(leverage),
            free_bp_pct: 0.0,
            clamp: false,
            equity,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        let (before, _other, after, _dropped) = sim_outcome(&row);
        // The position fits the room by construction; skip (don't fail) the rare boundary case
        // where the gate refuses the setup leg, so `before` would not equal the declared position.
        prop_assume!((before - row.position).abs() <= 1e-6 * (1.0 + row.position.abs()));
        let expected_after = before + side as f64 * row.qty;
        let sim_filled = (after - expected_after).abs() <= 1e-6 * (1.0 + expected_after.abs());
        // No pin: phase 2 made the margin lane one shared judge evaluated on both sides.
        reconcile("prop/margin", live_ok, &reason, sim_filled, before, after, None);
    }
}

// ---------------------------------------------------------------------------------------------
// The gate SHAPE tests (deny-vs-clamp phases 1+2). The matrix asserts DECISIONS; these pin the
// two outcome shapes behind them: the default DENY-WHOLE through the live gate (zero fill, the
// GATE's own reason string) and the opt-in clamp (the pre-phase-1 truncation, verbatim).
// ---------------------------------------------------------------------------------------------

/// A Row for the shape lane: 1_000 units against a 100-unit leverage room
/// (`1.0 * 10_000 / 100`), the over-budget order the matrix's
/// `margin/over-budget-both-deny-whole` row decides on.
fn over_budget_row(clamp: bool) -> Row {
    Row {
        name: "shape/over-budget",
        qty: 1_000.0,
        im: Some(1.0),
        leverage: Some(1.0),
        equity: 10_000.0,
        clamp,
        ..Row::default()
    }
}

/// DEFAULT SHAPE: the order is DENIED WHOLE — zero fill, nothing resized — and the strategy can
/// OBSERVE the denial: `SimBroker::dropped` records the GATE's own reason with the full
/// requested size. UNIFIED AS OF PHASE 2: the string is `"insufficient-margin"` — the SAME
/// string, from the SAME `RiskGate::check`, that live publishes in `OrderDenied` — because the
/// backtest now IS the live gate; the phase-1 stand-in `"insufficient-leverage-room"` (kept
/// distinct back then precisely because the formulas differed) no longer exists.
#[test]
fn margin_over_budget_denies_whole_by_default() {
    let row = over_budget_row(false);
    let (live_ok, reason) = live_verdict(&row);
    assert!(!live_ok && reason == "insufficient-margin", "live must deny outright: {reason:?}");

    let (_, _, pos_after, dropped) = sim_outcome(&row);
    assert_eq!(pos_after, 0.0, "deny-whole must fill NOTHING (no silent resize), got {pos_after}");
    assert_eq!(
        dropped,
        vec![(SYM.to_string(), "insufficient-margin".to_string(), 1_000.0, 0.0)],
        "the denial must be strategy-observable: full requested size, the LIVE reason string"
    );
}

/// OPT-IN SHAPE (`clamp_to_leverage: true`): the pre-phase-1 `cap_to_leverage` truncation,
/// verbatim — the order shrinks to the remaining room and executes silently (nothing in
/// `dropped`). This is the escape hatch, not the default.
#[test]
fn clamp_knob_restores_the_truncate_shape_verbatim() {
    let (_, _, pos_after, dropped) = sim_outcome(&over_budget_row(true));
    // room = leverage*equity / (price*multiplier) = 1.0*10_000 / 100 = 100 units.
    assert!(
        (pos_after - 100.0).abs() <= 1e-9,
        "clamp_to_leverage must TRUNCATE to the remaining room (100 units), got {pos_after}"
    );
    assert!(dropped.is_empty(), "the clamp shape is silent (the historical behavior): {dropped:?}");
}

/// BOUNDARY: an order that EXACTLY fills the margin budget passes WHOLE on both shapes — the
/// deny path is the live gate itself and `has_sufficient_margin` is `<=` (order margin == free
/// buying power admits), while the clamp path's `size <= room` boundary is inclusive too, so
/// deny-mode and clamp-mode fills are identical here.
#[test]
fn exactly_fitting_order_passes_whole_on_both_shapes() {
    for clamp in [false, true] {
        let row = Row {
            name: "shape/exactly-fits",
            qty: 100.0, // = the whole room: 1.0 * 10_000 / 100
            im: Some(1.0),
            leverage: Some(1.0),
            equity: 10_000.0,
            clamp,
            ..Row::default()
        };
        let (live_ok, reason) = live_verdict(&row);
        assert!(live_ok, "live admits the exact boundary (order margin == free BP): {reason:?}");
        let (_, _, pos_after, dropped) = sim_outcome(&row);
        assert_eq!(pos_after, 100.0, "clamp={clamp}: the exact-fit order must fill whole");
        assert!(dropped.is_empty(), "clamp={clamp}: nothing to refuse at the boundary");
    }
}

/// ANTI-STRANDING: a REDUCING order is NEVER denied by the default deny path (nor capped by the
/// clamp), even with the leverage room fully exhausted — the same never-strand-a-position rule
/// as `apply_fill`'s closing-fill floor bypass and this week's #458 fix. Position 100 units IS
/// the whole room, so an opening order would be refused here; the full flatten still executes.
#[test]
fn reducing_order_is_never_denied_even_with_zero_room() {
    let row = Row {
        name: "shape/reduce-with-zero-room",
        position: 100.0, // exactly the room: setup passes the gate as an exact fit
        side: -1,
        qty: 100.0,
        im: Some(1.0),
        leverage: Some(1.0),
        equity: 10_000.0,
        ..Row::default()
    };
    let (pos_before, _other, pos_after, dropped) = sim_outcome(&row);
    assert_eq!(pos_before, 100.0, "harness: the setup leg must land whole");
    assert_eq!(pos_after, 0.0, "the flatten must execute in full");
    assert!(dropped.is_empty(), "a reducing order must never be denied: {dropped:?}");
}

/// FIXED (was a pinned known bug): `max_total_exposure` now values the projected position at
/// `|position + side*qty| * mark_price * ctx.multiplier`, folding the contract multiplier exactly
/// like the `order_notional` (min/max notional) and `initial_margin` (buying power) lanes of the
/// same gate. Before the fix it omitted `ctx.multiplier`, so on a mult=10 instrument the exposure
/// cap measured a TENTH of the real exposure and admitted an order ten times too large.
///
/// `SimBroker` has no exposure cap at all, so this never surfaces as a live/backtest DISAGREEMENT
/// — which is precisely why it keeps its own pin here: the matrix above cannot see it. Per the
/// pin's standing instruction, the fix INVERTS this assertion rather than deleting the test.
#[test]
fn max_total_exposure_includes_the_contract_multiplier() {
    let limits = RiskLimits { max_total_exposure: Some(500.0), ..RiskLimits::new() };
    let ctx = RiskContext { mark_price: 100.0, multiplier: 10.0, ..RiskContext::default() };
    let req = OrderRequest {
        client_order_id: "exposure".to_string(),
        venue: VENUE.to_string(),
        symbol: SYM.to_string(),
        order_type: "market".to_string(),
        side: 1,
        qty: 2.0,
        ..Default::default()
    };

    // TRUE exposure is 2 * 100 * 10 = 2_000, four times the 500 cap.
    assert_eq!(vike_model::order_notional(req.qty, ctx.mark_price, ctx.multiplier), 2_000.0);
    // the gate now measures the full 2_000 (multiplier folded in) and DENIES. <-- fixed.
    let v = RiskGate::new(limits).check(&req, &ctx);
    assert!(
        !v.ok && v.reason == "over-max-exposure",
        "max_total_exposure must fold ctx.multiplier: 2*100*10 = 2_000 > 500 cap must DENY: {v:?}"
    );
}

/// The shared #481 predicates are the vocabulary the matrix speaks; pin that each side's real
/// rule is the one the rows assume, so a change to either predicate's meaning breaks here loudly
/// rather than silently re-labelling scenarios.
#[test]
fn the_two_sides_use_the_two_distinct_shared_predicates() {
    // The gate's FLOOR bypass is `is_covered_reduce` (coverage required, flag not trusted alone).
    assert!(vike_model::is_covered_reduce(false, -1, 5.0, 2.0), "covered reduce");
    assert!(!vike_model::is_covered_reduce(true, 1, 0.0, 2.0), "flat + flag is still opening");
    assert!(!vike_model::is_covered_reduce(false, -1, 0.005, 0.008), "a reversal is not covered");

    // `SimBroker::apply_fill`'s opening split is DIRECTION-ONLY — which is exactly why the
    // reversal rows diverge: the same reversal is "not covered" above but "not opening" here.
    assert!(vike_model::is_reducing_direction(-1, 0.005), "the reversal is still a closing fill");
    assert!(!vike_model::is_reducing_direction(1, 0.0), "flat is opening in either direction");
}
