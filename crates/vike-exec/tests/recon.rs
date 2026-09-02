//! `recon` — vike-exec's reconciliation suite (the resolve/policy/sweep/reap laws): ONE test
//! binary over what used to be nine.
//!
//! Grouped per the rule in `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section:
//! each former `tests/<name>.rs` is now `tests/recon/<name>.rs`, included below as a plain module.
//! Test names and bodies are unchanged — only the binary they link into (`--test recon_policy_pin`
//! is now `--test recon`; `-- <name>` filtering is untouched):
//!
//! ```sh
//! cargo test -p vike-exec --test recon              # the whole group
//! cargo test -p vike-exec --test recon -- policy    # by name substring, as before
//! ```
//!
//! Every member is plain — no crate-level `#![cfg]`, no `#[ignore]`, no proptest sidecar, no
//! process-global mutation (`env::set_var`/`set_current_dir`). That matters because nextest runs
//! each test in its own process, but a plain `cargo test -p vike-exec` runs one binary's tests as
//! THREADS in one process: merging a file that mutates process state would turn independent
//! binaries into a race.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_fixtures;` would
// resolve against `tests/` (the root's own directory), not `tests/recon/`.
#[path = "recon/recon_fixtures.rs"]
mod recon_fixtures;
#[path = "recon/recon_idempotency.rs"]
mod recon_idempotency;
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
