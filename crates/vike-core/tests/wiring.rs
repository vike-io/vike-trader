//! `wiring` — the remaining plain vike-core integration suites (core wiring, live params/schedule,
//! multi-symbol routing and reads, controller, shutdown, xEMM reference lane): ONE test binary over
//! what used to be nineteen. Same shape and same eligibility rule as `tests/recon.rs` beside this
//! file — grouped per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section; test
//! names and bodies unchanged, only the `--test <binary>` slot.
//!
//! NOT here, on purpose: `runtime_smoke.rs` (the big fixture-replay harness stays its own binary
//! for bisectability) and `runtime_latency.rs` (`#[ignore]`d + serial; CI's latency job builds it
//! by name as `--test runtime_latency` — it must never join a group).

// The owned `/tmp` scratch guard every journal directory in this binary is allocated through — ONE
// source file, shared with the crate's own `mod tests` blocks through `src/lib.rs`, pulled in here
// with the same `#[path]` idiom as `tests/common/latency_line.rs`. Its module doc carries the leak
// this closed.
#[path = "../src/scratch.rs"]
mod scratch;

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "wiring/control_boundary.rs"]
mod control_boundary;
#[path = "wiring/controller_live_params.rs"]
mod controller_live_params;
#[path = "wiring/core_ergonomics.rs"]
mod core_ergonomics;
#[path = "wiring/counters_wiring.rs"]
mod counters_wiring;
#[path = "wiring/cross_venue_runtime.rs"]
mod cross_venue_runtime;
#[path = "wiring/live_engine_verbs.rs"]
mod live_engine_verbs;
#[path = "wiring/live_params.rs"]
mod live_params;
#[path = "wiring/live_schedule.rs"]
mod live_schedule;
#[path = "wiring/mark_slot_semantics.rs"]
mod mark_slot_semantics;
#[path = "wiring/mount_attr_durability.rs"]
mod mount_attr_durability;
#[path = "wiring/multi_mount.rs"]
mod multi_mount;
#[path = "wiring/multi_symbol_reads.rs"]
mod multi_symbol_reads;
#[path = "wiring/multi_symbol_routing.rs"]
mod multi_symbol_routing;
#[path = "wiring/readiness_gate_wiring.rs"]
mod readiness_gate_wiring;
#[path = "wiring/rhai_live_mount.rs"]
mod rhai_live_mount;
#[path = "wiring/shutdown_cancel_policy.rs"]
mod shutdown_cancel_policy;
#[path = "wiring/shutdown_teardown_phases.rs"]
mod shutdown_teardown_phases;
#[path = "wiring/sizing_equity_ceiling.rs"]
mod sizing_equity_ceiling;
#[path = "wiring/state_save_timer_wiring.rs"]
mod state_save_timer_wiring;
#[path = "wiring/strategy_state_wiring.rs"]
mod strategy_state_wiring;
#[path = "wiring/xemm_reference_quote.rs"]
mod xemm_reference_quote;
