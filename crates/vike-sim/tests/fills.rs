//! `fills` — the opt-in FILL-REALISM knob gates (impact / latency / queue / staleness / properties
//! grid / fee schedule / equity sampling / their composition) and the fill-path gates beside them
//! (the shared gate sequence, the funding seam, book replay, maker quotes), ONE test binary over
//! what used to be thirteen.
//!
//! **Test-binary consolidation (CI cost, not behavior)** — see `tests/parity.rs`'s module doc for
//! the why and the safety rule. Each former `tests/<name>.rs` is now `tests/fills/<name>.rs`,
//! included below as a plain module; the tests, their names and their bodies are byte-unchanged.
//!
//! ```sh
//! cargo test -p vike-sim --test fills                 # the whole group
//! cargo test -p vike-sim --test fills -- queue        # one test by name, as before
//! ```
//!
//! Members (each keeps its own module doc):
//! - `impact_slippage` — the opt-in market-impact slippage model (bar lane).
//! - `impact_lanes` — the same model on the TICK and L2 replay lanes, and the terms each is
//!   charged (the L2 walk has already paid the temporary half).
//! - `latency_model` — the two-leg order-latency model through `run_ticks`.
//! - `queue_model` — queue-position gating of resting maker limits through `run_ticks`.
//! - `stale_price_wait` — the stale-price wait discipline (`max_price_staleness_ms`).
//! - `properties_fills` — fills snapped to the point-in-time instrument-properties grid.
//! - `fee_schedule_engine` — `EngineParams::fee_schedule` maker/taker rates in the engine.
//! - `equity_sampling` — the tick-lane equity-curve density knob.
//! - `knob_composition` — the knobs COMPOSED, so a pair cannot silently cancel out.
//! - `fill_gate_sequence` — the per-order gate sequence is ONE sequence, shared by the frozen and
//!   the queued tick lanes.
//! - `funding_source` — the opt-in `EngineParams::funding_source` perp-funding seam.
//! - `book_replay` — the `Tick::Book` fold through `run_ticks` (anchor/delta/gap integrity).
//! - `maker_backtest` — the `vike-mm` `SpreadMaker` through `SimBroker`'s `HftBroker` impl.
//!
//! Every member is plain: no `#![cfg]` feature gate, no `#[ignore]`d live/heavy test, no `proptest`
//! regression sidecar, no process-global mutation (`set_var`/`set_current_dir`). Anything failing
//! those checks stayed its own binary on purpose — plain `cargo test` runs one binary's tests as
//! threads in one process, so a process-mutating file must not share one.
//!
//! The tick, bar and calendar builders the members share live in `tests/common/` and are declared
//! here once (`ticks`, `bars`, `calendar`); a member reaches one as `crate::ticks::quote`.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod queue_model;` would resolve
// against `tests/` (the root's own directory), not `tests/fills/`.
#[path = "common/bars.rs"]
mod bars;
#[path = "common/calendar.rs"]
mod calendar;
#[path = "common/ticks.rs"]
mod ticks;

#[path = "fills/book_replay.rs"]
mod book_replay;
#[path = "fills/equity_sampling.rs"]
mod equity_sampling;
#[path = "fills/fee_schedule_engine.rs"]
mod fee_schedule_engine;
#[path = "fills/fill_gate_sequence.rs"]
mod fill_gate_sequence;
#[path = "fills/funding_source.rs"]
mod funding_source;
#[path = "fills/impact_lanes.rs"]
mod impact_lanes;
#[path = "fills/impact_slippage.rs"]
mod impact_slippage;
#[path = "fills/knob_composition.rs"]
mod knob_composition;
#[path = "fills/latency_model.rs"]
mod latency_model;
#[path = "fills/maker_backtest.rs"]
mod maker_backtest;
#[path = "fills/properties_fills.rs"]
mod properties_fills;
#[path = "fills/queue_model.rs"]
mod queue_model;
#[path = "fills/stale_price_wait.rs"]
mod stale_price_wait;
