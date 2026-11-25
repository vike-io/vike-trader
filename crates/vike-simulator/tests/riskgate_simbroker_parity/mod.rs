//! LIVE `RiskGate` <-> BACKTEST `OrderBrokerSimulator` reconciliation as a GATE — the risk-model twin of
//! `engine_kernel_parity.rs` (which reconciles the event engine against the vectorized kernel).
//!
//! # Why this exists
//!
//! Four real money bugs were found on 2026-07-19 by manually asking "where is this rule
//! implemented twice?". NOT ONE was caught by a test:
//!
//! * **#458** — `RiskGate` DENIED a below-min CLOSING order that `OrderBrokerSimulator` fills, stranding a
//!   position live that the backtest showed flattening cleanly.
//! * **#458** — the gate's notional omitted the contract multiplier that `OrderBrokerSimulator::apply_fill`
//!   includes, so the same `min_notional` judged a mult != 1 instrument differently on each side.
//! * **#468** — the published snapshot's margin fold skipped positions the gate counts.
//! * **#479** — production passed `SEED_CASH` as a contract multiplier.
//!
//! Since deny-vs-clamp PHASE 2 there IS a shared implementation on the pre-trade lane:
//! **`OrderBrokerSimulator::gate_market_order` calls the literal `vike_execution::RiskGate::check`** — one
//! formula, one code path, one reason-string vocabulary — so the margin/floor ADMISSION verdict
//! is the same function evaluated on both sides and this matrix asserts that the CONTEXT each
//! side builds (`RiskContext` from live account state vs from `OrderBrokerSimulator` state) produces the
//! same decisions. The venue-side FILL lane is `OrderBrokerSimulator::apply_fill`'s point-in-time grid check
//! at fill time (direction-only opening split), and it decides alike too. For each row the two
//! decisions must AGREE: a divergence fails the test, so a new one is a loud, reviewable diff rather
//! than a silent behavior change.
//!
//! # The two decision surfaces, as of this commit
//!
//! `RiskGate::check` (`vike-execution/src/risk.rs`, `check_inner`) — ordered ladder, first hit denies:
//! side validity -> `TradingState` (halt / reduce-only) -> reduce-only overshoot -> tick/lot
//! normalization -> non-positive size -> **min_qty** (bypassed by `is_covered_reduce`) ->
//! **min_notional** (same bypass, over `order_notional(qty, ref_price, multiplier)`) ->
//! `max_notional_per_order` -> `max_total_exposure` -> **buying power** (bypassed by
//! `is_covered_reduce`, credited `closing_credit` on reversals) -> impact veto (book-only, off
//! here) -> throttle (DISARMED in the backtest mount — wall-clock, meaningless in sim time).
//!
//! `OrderBrokerSimulator` — the SAME gate pre-trade, plus the venue-side fill lane:
//! * `gate_market_order` (pre-trade, `submit`/`submit_market_close` only): crosses the order
//!   through the mounted live `RiskGate` over a `RiskContext` mirroring the live
//!   `gate_and_register` construction (`equity_now`, the pending-aware `margin_in_use_priced` fold,
//!   the LEAN reversing `closing_credit`), armed via `StrategyEngineConfig::risk_limits` or the
//!   `leverage -> initial_margin_requirement = 1/leverage` mapping. A refusal is whole and recorded in
//!   `OrderBrokerSimulator::dropped` under the GATE's own reason string. The pre-phase-1 silent
//!   TRUNCATION survives verbatim behind the opt-in `StrategyEngineConfig::clamp_to_leverage` (which
//!   unmounts the gate).
//! * `apply_fill` (at fill time): snaps price/size to the PIT grid, then rejects an
//!   **opening/increasing** fill below `min_qty`/`min_notional`. The opening split is
//!   DIRECTION-ONLY (`!is_reducing_direction`) — a closing fill ALWAYS executes so a position is
//!   never stranded, and a below-min REVERSAL fills its closing half alone.
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

const SYM: &str = "SYM0";
/// The ACCOUNT lane's second instrument — registered only for rows that declare an
/// `other_position`, so every other row keeps the one-symbol engine it always had.
const SYMBOL2: &str = "SYM1";
const VENUE: &str = "TEST";
const T0: i64 = 1_700_000_000_000;
const BAR_MS: i64 = 60_000;
/// The order under test fills at bar 2's open; the grid arms from that timestamp on.
const TEST_FILL_TIMESTAMP: i64 = T0 + 2 * BAR_MS;

// ---------------------------------------------------------------------------------------------
// Scenario table
// ---------------------------------------------------------------------------------------------

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
    initial_margin: Option<f64>,
    /// `RequiredFreeBuyingPowerPercent` haircut — BOTH sides since phase 2: live via
    /// `RiskLimits`, backtest via `StrategyEngineConfig::risk_limits` (see [`sim_outcome`])
    free_bp_percent: f64,
    /// BACKTEST margin arming via the `leverage -> initial_margin_requirement = 1/leverage` mapping into
    /// the mounted gate (`OrderBrokerSimulator::build_risk_gate`); `None` = no gate
    leverage: Option<f64>,
    /// BACKTEST `StrategyEngineConfig::clamp_to_leverage` — `false` (the production default) = deny
    /// whole; `true` = the opt-in pre-phase-1 truncation (exercised only by the shape tests)
    clamp: bool,
    /// account equity — LIVE `RiskContext::equity`, BACKTEST `StrategyEngineConfig::cash`
    equity: f64,
    /// **The ACCOUNT-AGGREGATE ceiling** (`RiskLimits::max_account_exposure`), both sides: LIVE
    /// through the row's own limits, BACKTEST through `StrategyEngineConfig::risk_limits`. `None` (every
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
            initial_margin: None,
            free_bp_percent: 0.0,
            leverage: None,
            clamp: false,
            equity: 1_000_000.0,
            account_cap: None,
            other_position: 0.0,
            live_ok: true,
            sim_filled: true,
        }
    }
}

// A module of the `integration` binary (`tests/integration.rs`), loaded by `#[path]`: each child's
// `#[path]` is relative to THIS file's directory.
#[path = "matrix.rs"]
mod matrix;
#[path = "property_tests.rs"]
mod property_tests;
#[path = "reconcile.rs"]
mod reconcile;
#[path = "shape_checks.rs"]
mod shape_checks;
#[path = "sides.rs"]
mod sides;
