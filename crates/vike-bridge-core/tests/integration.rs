//! **vike-bridge-core's top-level test files as ONE test binary** (`cargo test -p vike-bridge-core --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-bridge-core --test integration -- <name>::`
//! runs one file. One binary is compiled and linked instead of one per file, by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". Discovery is off (`autotests = false` in this
//! crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a `mod` line here or a `[[test]]` row of its own, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails one that is neither.
//!
//! Still their own binaries, because each sets process-global state a sibling would read under a threaded
//! `cargo test`: `depth_fault_journal` (it installs `vike_log::capture::global`, the process's one global
//! subscriber), and `perp_fill_label` and `link_conformance` (each holds bybit's kept list, which
//! `bridge_conformance`'s bybit cells read without one; the link harness also declares polymarket's process-wide
//! egress).

mod account_label_spellings;
mod bridge_conformance;
mod exec_actor_bulk_cancel;
mod exec_actor_confirm;
mod exec_actor_dead_thread;
mod exec_actor_halt;
mod fill_dedup_at_the_account;
mod format_property_tests;
mod grid_multiple_survives_the_wire;
mod grid_witness_is_unambiguous;
mod live_rest_cancel_reject;
mod live_tier_not_wired_report;
mod market_data_conformance;
mod resync_triggers_reconcile;
mod settings_dir_spellings;
mod wire_quantizer_probe;
