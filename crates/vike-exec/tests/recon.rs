//! `recon` — vike-exec's reconciliation suite (the resolve/policy/sweep/reap laws) as ONE test
//! binary, grouped per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section:
//! each member is `tests/recon/<name>.rs`, included below as a plain module.
//!
//! ```sh
//! cargo test -p vike-exec --test recon              # the whole group
//! cargo test -p vike-exec --test recon -- policy    # by name substring, as before
//! ```
//!
//! Every member is plain: no crate-level `#![cfg]`, no `#[ignore]`, no proptest sidecar, no
//! process-global mutation (`env::set_var`/`set_current_dir`). A plain `cargo test` runs one
//! binary's tests as THREADS in one process (nextest uses a process each), so a member mutating
//! process state would race its neighbours.

// The shared `EngineBuilder`, one copy for every group root.
#[path = "support/mod.rs"]
mod support;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_fixtures;` would
// resolve against `tests/`, not `tests/recon/`.
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
