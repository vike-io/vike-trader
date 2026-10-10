//! `engine` — the ExecutionEngine fold suites (fills, cancels, amends, hostile-venue folds) as ONE
//! test binary: same shape and eligibility rule as `tests/recon.rs`, grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section.

// `limit_order`: the cancel-path suites' resting limit request (engine group only, so no other
// root compiles it); `support`: the shared `EngineBuilder`.
#[path = "support/limit_order.rs"]
mod limit_order;
#[path = "support/mod.rs"]
mod support;

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "engine/applied_fill_symbol.rs"]
mod applied_fill_symbol;
#[path = "engine/cancel_intent.rs"]
mod cancel_intent;
#[path = "engine/cancel_race_guard.rs"]
mod cancel_race_guard;
#[path = "engine/dropped_events_observability.rs"]
mod dropped_events_observability;
#[path = "engine/fill_coid_routing.rs"]
mod fill_coid_routing;
#[path = "engine/fold_margin_mode.rs"]
mod fold_margin_mode;
#[path = "engine/hostile_venue_fold.rs"]
mod hostile_venue_fold;
#[path = "engine/partial_fill_amend_accounting.rs"]
mod partial_fill_amend_accounting;
#[path = "engine/price_board_wiring.rs"]
mod price_board_wiring;
#[path = "engine/route_key.rs"]
mod route_key;
