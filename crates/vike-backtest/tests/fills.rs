//! `fills` — the opt-in FILL-REALISM knob gates (impact / latency / queue / staleness / properties
//! grid / fee schedule / equity sampling / their composition), ONE test binary over what used to be
//! eight.
//!
//! **Test-binary consolidation (CI cost, not behavior)** — see `tests/parity.rs`'s module doc for
//! the why and the safety rule. Each former `tests/<name>.rs` is now `tests/fills/<name>.rs`,
//! included below as a plain module; the tests, their names and their bodies are byte-unchanged.
//!
//! ```sh
//! cargo test -p vike-backtest --test fills                 # the whole group
//! cargo test -p vike-backtest --test fills -- queue        # one test by name, as before
//! ```
//!
//! Members (each keeps its own module doc):
//! - `impact_slippage` — the opt-in market-impact slippage model (bar lane).
//! - `latency_model` — the two-leg order-latency model through `run_ticks`.
//! - `queue_model` — queue-position gating of resting maker limits through `run_ticks`.
//! - `stale_price_wait` — the stale-price wait discipline (`max_price_staleness_ms`).
//! - `properties_fills` — fills snapped to the point-in-time instrument-properties grid.
//! - `fee_schedule_engine` — `EngineParams::fee_schedule` maker/taker rates in the engine.
//! - `equity_sampling` — the tick-lane equity-curve density knob.
//! - `knob_composition` — the knobs COMPOSED, so a pair cannot silently cancel out.
//!
//! Every member is plain: no `#![cfg]` feature gate, no `#[ignore]`d live/heavy test, no `proptest`
//! regression sidecar, no process-global mutation (`set_var`/`set_current_dir`). Anything failing
//! those checks stayed its own binary on purpose — plain `cargo test` runs one binary's tests as
//! threads in one process, so a process-mutating file must not share one.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod queue_model;` would resolve
// against `tests/` (the root's own directory), not `tests/fills/`.
#[path = "fills/equity_sampling.rs"]
mod equity_sampling;
#[path = "fills/fee_schedule_engine.rs"]
mod fee_schedule_engine;
#[path = "fills/impact_slippage.rs"]
mod impact_slippage;
#[path = "fills/knob_composition.rs"]
mod knob_composition;
#[path = "fills/latency_model.rs"]
mod latency_model;
#[path = "fills/properties_fills.rs"]
mod properties_fills;
#[path = "fills/queue_model.rs"]
mod queue_model;
#[path = "fills/stale_price_wait.rs"]
mod stale_price_wait;
