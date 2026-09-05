//! `recon` — vike-core's reconciliation-driver suite: ONE test binary over what used to be seven.
//!
//! Grouped per the rule in `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section:
//! each former `tests/<name>.rs` is now `tests/recon/<name>.rs`, included below as a plain module.
//! The tests, their names and their bodies are unchanged — only the binary they link into. Run one
//! by name exactly as before, with the group binary in the `--test` slot:
//!
//! ```sh
//! cargo test -p vike-core --test recon                # the whole group
//! cargo test -p vike-core --test recon -- quarantine  # by name substring, as before
//! ```
//!
//! Every member is plain — no crate-level `#![cfg]`, no `#[ignore]`, no proptest sidecar, no
//! process-global mutation (`env::set_var`/`set_current_dir`). That matters because nextest runs
//! each test in its own process, but the feature lanes and a local `cargo test -p vike-core` run
//! one binary's tests as THREADS in one process: merging a file that mutates process state would
//! turn independent binaries into a race.

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_balance;` would
// resolve against `tests/` (the root's own directory), not `tests/recon/`.
#[path = "recon/recon_audit_gating.rs"]
mod recon_audit_gating;
#[path = "recon/recon_balance.rs"]
mod recon_balance;
#[path = "recon/recon_coin_delta.rs"]
mod recon_coin_delta;
#[path = "recon/recon_continuous_audit.rs"]
mod recon_continuous_audit;
#[path = "recon/recon_journal_crosscheck.rs"]
mod recon_journal_crosscheck;
#[path = "recon/recon_journal_view.rs"]
mod recon_journal_view;
#[path = "recon/recon_manager.rs"]
mod recon_manager;
#[path = "recon/recon_quarantine.rs"]
mod recon_quarantine;
