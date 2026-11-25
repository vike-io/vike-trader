//! **vike-bybit's top-level test files as ONE test binary** (`cargo test -p vike-bybit --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, and its `//!` contract; here it is a MODULE, so a test's id is
//! `<name>::<test>` and `cargo test -p vike-bybit --test integration -- <name>::` runs one file (a live smoke adds
//! `--ignored --nocapture`, as its own `//!` says). One binary is compiled and linked instead of one per file, by
//! the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation". Discovery is off
//! (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a `mod` line here or a
//! `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails one that is neither.
//!
//! Still their own binaries:
//! * `offline`, the plain offline group: its `fee_not_read` asserts that a process-wide counter did NOT move,
//!   and the property suites below feed malformed fees through the same mappers on sibling threads;
//! * `bybit_smokes`, the smokes folder;
//! * `bybit_exec_client_smoke`, `bybit_market_data_smoke` and `bybit_reconcile_smoke`, named by
//!   `.github/workflows/live-smokes.yml`, whose guard (`scripts/refuse_live_credentials.sh`) scans every file of a
//!   binary the lane names and refuses a `#[path]`.

mod bybit_capture_smoke;
mod bybit_identity_probe;
mod bybit_isolated_wallet_probe;
mod bybit_klines_live_smoke;
mod bybit_leverage_smoke;
mod bybit_perp_smoke;
mod bybit_post_only_smoke;
mod bybit_tif_smoke;
mod bybit_trades_feed_smoke;
mod bybit_userdata_soak;
mod bybit_watchdog_recovery_smoke;
mod decoder_never_panics;
mod mapper_property_tests;
mod order_client_id_map_failure_repro;
mod typed_frame_property_tests;
