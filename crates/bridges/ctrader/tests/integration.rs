//! **vike-ctrader's top-level test files as ONE test binary** (`cargo test -p vike-ctrader --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, and its `//!` contract; here it is a MODULE, so a test's id is
//! `<name>::<test>` and `cargo test -p vike-ctrader --test integration -- <name>::` runs one file (a live smoke adds
//! `--ignored --nocapture`, as its own `//!` says). One binary is compiled and linked instead of one per file, by
//! the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". Discovery is off
//! (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a `mod` line here or a
//! `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails one that is neither.
//!
//! Still their own binaries:
//! * `offline`, the plain offline group (its module doc carries what may join it);
//! * `ctrader_reconcile_smoke`, named by `.github/workflows/live-smokes.yml`, whose guard
//!   (`scripts/refuse_live_credentials.sh`) scans every file of a binary the lane names and refuses a `#[path]`;
//! * `ctrader_cfd_units_smoke`, which reads its own argv: its positional words (`XAUUSD US500 sell steps=2`) are
//!   also libtest filters, and in a shared binary `sell` would select every other ignored smoke whose name holds
//!   it, each placing real demo orders.

mod ctrader_bound_exit_smoke;
mod ctrader_catalog_smoke;
mod ctrader_close_all_smoke;
mod ctrader_currency_smoke;
mod ctrader_demo_smoke;
mod ctrader_history_smoke;
mod ctrader_leave_flat_smoke;
mod ctrader_live_mount_smoke;
mod ctrader_market_data_smoke;
mod ctrader_seed_smoke;
mod ctrader_stop_limit_smoke;
mod ctrader_swap_sign_smoke;
mod ctrader_symbol_state_smoke;
mod decoder_never_panics;
mod framing_property_tests;
