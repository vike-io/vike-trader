//! `laws` — the backtest-side LAW gates (liquidation / session / trigger / margin / TIF expiry /
//! settlement / the pre-trade gate on every verb / scheduling), ONE test binary over what used to
//! be ten.
//!
//! **Test-binary consolidation (CI cost, not behavior)** — see `tests/parity.rs`'s module doc for
//! the why and the safety rule. Each former `tests/<name>.rs` is now `tests/laws/<name>.rs`,
//! included below as a plain module; the tests, their names and their bodies are byte-unchanged.
//!
//! ```sh
//! cargo test -p vike-sim --test laws                       # the whole group
//! cargo test -p vike-sim --test laws -- liquidation        # one test by name, as before
//! ```
//!
//! Members (each keeps its own module doc):
//! - `liquidation_law` — the ONE scope-parameterized liquidation law (bar + tick paths).
//! - `session_law` — the session / market-hours law, backtest side (law-map T10).
//! - `trigger_law` — trigger-law wave 2 (law-map A2/A3/A4), backtest side.
//! - `variation_margin` — the opt-in `settlement_period_ms` variation-margin settlement.
//! - `tif_expiry_oms` — a `Gtd` deadline terminalizing through the OMS `ManagedOrder` FSM, not
//!   just the paper book.
//! - `option_expiry_settlement` — `EngineParams::option_specs` cash settlement at expiry.
//! - `resolution_settlement` — `EngineParams::resolution` binary-resolution settlement.
//! - `limit_orders_are_gated` — limit, stop and trailing orders face the pre-trade `[risk]` gate.
//! - `below_min_reversal_count` — `SimBroker::below_min_reversals` can FIRE on a below-min
//!   reversal.
//! - `schedule_time_rule` — the shared wall-clock `TimeRule`, byte-identical in the backtest
//!   `Schedule` and the live `LiveSchedule`.
//!
//! Every member is plain: no `#![cfg]` feature gate, no `#[ignore]`d live/heavy test, no `proptest`
//! regression sidecar, no process-global mutation (`set_var`/`set_current_dir`). Anything failing
//! those checks stayed its own binary on purpose — plain `cargo test` runs one binary's tests as
//! threads in one process, so a process-mutating file must not share one.
//!
//! The tick, bar and calendar builders the members share live in `tests/common/` and are declared
//! here once (`ticks`, `bars`, `calendar`); a member reaches one as `crate::bars::bar`.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod session_law;` would resolve
// against `tests/` (the root's own directory), not `tests/laws/`.
#[path = "common/bars.rs"]
mod bars;
#[path = "common/calendar.rs"]
mod calendar;
#[path = "common/ticks.rs"]
mod ticks;

#[path = "laws/below_min_reversal_count.rs"]
mod below_min_reversal_count;
#[path = "laws/limit_orders_are_gated.rs"]
mod limit_orders_are_gated;
#[path = "laws/liquidation_law.rs"]
mod liquidation_law;
#[path = "laws/option_expiry_settlement.rs"]
mod option_expiry_settlement;
#[path = "laws/resolution_settlement.rs"]
mod resolution_settlement;
#[path = "laws/schedule_time_rule.rs"]
mod schedule_time_rule;
#[path = "laws/session_law.rs"]
mod session_law;
#[path = "laws/tif_expiry_oms.rs"]
mod tif_expiry_oms;
#[path = "laws/trigger_law.rs"]
mod trigger_law;
#[path = "laws/variation_margin.rs"]
mod variation_margin;
