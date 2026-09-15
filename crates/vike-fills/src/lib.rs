//! vike-fills — the shared FILL cluster: ONE definition of what a fill costs, when it happens
//! and at what price. Extracted VERBATIM out of `vike-backtest` (pure relocation, no behavior
//! change).
//!
//! - [`broker_sim`]  (port of `core/broker_sim.py`)      — the three canonical cost scalars
//!   (`adverse_fill_price` + its `MIN_ADVERSE_FACTOR` floor, `fee`, `funding_charge`).
//! - [`fill_model`]  (port of `core/fill_model.py`)      — the tiered fill-price models: bar,
//!   L1 tick (spread-crossing) and the L2 `book_taker_price` walk.
//! - [`fill_resolution`] (port of `core/fill_resolution.py`) — adverse-first intrabar ordering
//!   with the SL/TP bracket cap.
//! - [`staleness`]   (opt-in; the LEAN `FutureFillModel` analog) — the stale-price wait
//!   discipline market orders defer under.
//!
//! # Why this is its own crate
//!
//! The R7 paper exchange and the backtest engine are the SAME cost model seen from two sides: a
//! paper `ExecutionClient` mounted on a live feed must charge exactly what `SimBroker` charges,
//! or "backtest == paper == live" stops being checkable. While both lived inside `vike-backtest`
//! that was true by co-location; the price was that every consumer who only wanted the paper
//! executor (`vike-mount` / `vike-run` / `vike-tradehub`) compiled the whole simulator to get it.
//! A crate under BOTH is the honest fix — and it is the one crate-reorg Phase 0 explicitly
//! declined to create when it dissolved `vike-paper` back into `vike-backtest` to "dedup the fill
//! model's home" (`docs/superpowers/plans/2026-07-07-phase0-fold-paper-bench.md`, D8). Sharing
//! the core here rather than copying it keeps that dedup intact.
//!
//! `vike-backtest` RE-EXPORTS all four modules at its own root, so every
//! `vike_backtest::fill_model::…` / `crate::broker_sim::…` path resolves exactly as before.
//!
//! PARITY RULES: see vike-model — f64 end-to-end, same expression order, no mul_add/fast-math.

pub mod broker_sim;
pub mod fill_model;
pub mod fill_resolution;
pub mod staleness;
