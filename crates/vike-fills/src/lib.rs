//! vike-fills — the shared FILL cluster: ONE definition of what a fill costs, when it happens
//! and at what price.
//!
//! - [`broker_sim`] — the cost scalars: `adverse_fill_price` (+ its `MIN_ADVERSE_FACTOR` floor),
//!   `fee`, `funding_charge`.
//! - [`fill_model`] — the tiered fill-price models: bar, L1 tick (spread-crossing), L2 book walk.
//! - [`fill_resolution`] — adverse-first intrabar ordering with the SL/TP bracket cap.
//! - [`staleness`] — the opt-in stale-price wait discipline market orders defer under.
//!
//! # Why this is its own crate
//!
//! The paper exchange (`vike-paper`) and the simulator (`vike-sim`) are the SAME cost model seen
//! from two sides: a paper `ExecutionClient` on a live feed must charge exactly what `SimBroker`
//! charges, or "backtest == paper == live" stops being checkable. A leaf below both, on vike-model
//! alone, gives them one definition without the paper path compiling the simulator. Callers name
//! it directly (`vike_fills::fill_model::…`); nothing re-exports it
//! (`docs/decisions/0087-the-simulator-leaves-the-harness.md`).
//!
//! PARITY RULES: see vike-model — f64 end-to-end, same expression order, no mul_add/fast-math.

pub mod broker_sim;
pub mod fill_model;
pub mod fill_resolution;
pub mod staleness;
