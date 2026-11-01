//! `integration` — every vike-exec integration suite as ONE test binary: the engine fold, parity,
//! reconciliation and RiskGate groups (`tests/{engine,parity,recon,risk}/<name>.rs`) and the four
//! property suites (`tests/integration/<former>.rs`). They share one configuration (no feature of their
//! own; the self dev-dependency's `test-support`), so one binary links where eight did. Grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section, with `--test integration`
//! in the slot each former binary held:
//!
//! ```sh
//! cargo test -p vike-exec --test integration              # every suite here
//! cargo test -p vike-exec --test integration -- policy    # by name substring, as before
//! ```
//!
//! The group members are FLAT modules below, so their test names did not change. Each property suite
//! is a module of its own (`quote_rate_props::<test>`), and its failure-persistence seed sits BESIDE its
//! source as `tests/integration/<former>.proptest-regressions`: proptest derives the file from the
//! source path (no `lib.rs`/`main.rs` above it, so it falls back to the file beside the source), so a
//! seed that did not move with its test would be read by nothing.
//!
//! Every member is plain: no crate-level `#![cfg]`, no `#[ignore]`, no process-global mutation
//! (`env::set_var`/`set_current_dir`). A plain `cargo test` runs one binary's tests as THREADS in one
//! process (nextest uses a process each), so a member mutating process state would race its
//! neighbours. Fixture paths are `CARGO_MANIFEST_DIR`-anchored, so a member's location does not matter.

// The cancel-path suites' resting limit request and the shared `EngineBuilder` are
// `vike_exec::testing::{resting_limit, EngineBuilder}`, compiled once in the lib behind `test-support`.
// `fill`: the plain taker fill the engine and risk suites fold, venue as a parameter.
#[path = "support/fill.rs"]
mod fill;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_fixtures;` would
// resolve against `tests/`, not `tests/recon/`.

// The ExecutionEngine fold suites (fills, cancels, amends, hostile-venue folds).
#[path = "engine/admit_symbol.rs"]
mod admit_symbol;
#[path = "engine/amend_plan.rs"]
mod amend_plan;
#[path = "engine/applied_fill_symbol.rs"]
mod applied_fill_symbol;
#[path = "engine/cancel_intent.rs"]
mod cancel_intent;
#[path = "engine/cancel_race_guard.rs"]
mod cancel_race_guard;
#[path = "engine/dropped_events_observability.rs"]
mod dropped_events_observability;
#[path = "engine/fee_adjustment_fold.rs"]
mod fee_adjustment_fold;
#[path = "engine/fill_coid_routing.rs"]
mod fill_coid_routing;
#[path = "engine/fold_margin_mode.rs"]
mod fold_margin_mode;
#[path = "engine/hostile_venue_fold.rs"]
mod hostile_venue_fold;
#[path = "engine/ledger_ccy_key.rs"]
mod ledger_ccy_key;
#[path = "engine/lost_prefix_fold.rs"]
mod lost_prefix_fold;
#[path = "engine/modify_snaps_to_grid.rs"]
mod modify_snaps_to_grid;
#[path = "engine/modify_unsupported_venue.rs"]
mod modify_unsupported_venue;
#[path = "engine/nothing_to_reduce.rs"]
mod nothing_to_reduce;
#[path = "engine/ordinary_accept_replay.rs"]
mod ordinary_accept_replay;
#[path = "engine/partial_fill_amend_accounting.rs"]
mod partial_fill_amend_accounting;
#[path = "engine/pending_modify_price.rs"]
mod pending_modify_price;
#[path = "engine/price_board_wiring.rs"]
mod price_board_wiring;
#[path = "engine/quote_currency.rs"]
mod quote_currency;
#[path = "engine/quote_identity.rs"]
mod quote_identity;
#[path = "engine/quote_margin.rs"]
mod quote_margin;
#[path = "engine/quote_money.rs"]
mod quote_money;
#[path = "engine/quote_mount.rs"]
mod quote_mount;
#[path = "engine/quote_review.rs"]
mod quote_review;
#[path = "engine/quote_snapshot.rs"]
mod quote_snapshot;
#[path = "engine/route_key.rs"]
mod route_key;
#[path = "engine/unadmit_symbol.rs"]
mod unadmit_symbol;

// The accounting/equity parity suites, r5 golden fixtures included.
#[path = "parity/account_multi_asset.rs"]
mod account_multi_asset;
#[path = "parity/account_parity.rs"]
mod account_parity;
#[path = "parity/cross_venue_equity_parity.rs"]
mod cross_venue_equity_parity;
#[path = "parity/portfolio_robustness.rs"]
mod portfolio_robustness;
#[path = "parity/r5_parity.rs"]
mod r5_parity;
#[path = "parity/resolve_equity.rs"]
mod resolve_equity;

// The reconciliation suite (the resolve/policy/sweep/reap laws).
#[path = "recon/recon_fixtures.rs"]
mod recon_fixtures;
#[path = "recon/recon_idempotency.rs"]
mod recon_idempotency;
#[path = "recon/recon_instance_origin.rs"]
mod recon_instance_origin;
#[path = "recon/recon_local_order_sweep.rs"]
mod recon_local_order_sweep;
#[path = "recon/recon_local_position_sweep.rs"]
mod recon_local_position_sweep;
#[path = "recon/recon_pnl_parity.rs"]
mod recon_pnl_parity;
#[path = "recon/recon_policy_pin.rs"]
mod recon_policy_pin;
#[path = "recon/reconcile_drift.rs"]
mod reconcile_drift;
#[path = "recon/reconcile_margin_mode.rs"]
mod reconcile_margin_mode;
#[path = "recon/reconcile_reap.rs"]
mod reconcile_reap;

// The RiskGate suites (lane completion, modify-path gating, margin, symbol coherence).
#[path = "risk/contract_value.rs"]
mod contract_value;
#[path = "risk/gate_symbol_coherence.rs"]
mod gate_symbol_coherence;
#[path = "risk/mark_floor.rs"]
mod mark_floor;
#[path = "risk/per_symbol_margin.rs"]
mod per_symbol_margin;
#[path = "risk/quote_gate.rs"]
mod quote_gate;
#[path = "risk/risk_account_ceiling.rs"]
mod risk_account_ceiling;
#[path = "risk/risk_gate_on_modify.rs"]
mod risk_gate_on_modify;
#[path = "risk/risk_lane_common.rs"]
mod risk_lane_common;
#[path = "risk/risk_lane_coverage.rs"]
mod risk_lane_coverage;
#[path = "risk/risk_lane_pricing.rs"]
mod risk_lane_pricing;
#[path = "risk/risk_margin.rs"]
mod risk_margin;

// The property suites, each with its seed (if any) beside it.
#[path = "integration/mount_foreign_props.rs"]
mod mount_foreign_props;
#[path = "integration/order_fsm_props.rs"]
mod order_fsm_props;
#[path = "integration/per_symbol_margin_props.rs"]
mod per_symbol_margin_props;
#[path = "integration/quote_rate_props.rs"]
mod quote_rate_props;
