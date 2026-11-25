//! **vike-okx's top-level test files as ONE test binary** (`cargo test -p vike-okx --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-okx --test integration -- <name>::` runs
//! one file (a live smoke adds `--ignored --nocapture`, as its own `//!` says). One binary is compiled and linked
//! instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a
//! `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails
//! one that is neither. A file that imports an `exec`-only module carries `#[cfg(feature = "execution")]` here.
//!
//! Still their own binaries:
//! * `offline`, the plain offline group: its `fee_not_read` asserts that a process-wide counter did NOT move,
//!   and `decoder_never_panics` feeds malformed fees through the same mappers on sibling threads;
//! * `okx_smokes`, the smokes folder;
//! * `okx_fine_tick_price_smoke` and `okx_market_data_smoke`, named by `.github/workflows/live-smokes.yml`, whose
//!   guard (`scripts/refuse_live_credentials.sh`) scans every file of a binary the lane names and refuses a
//!   `#[path]`.

mod decoder_never_panics;
mod kline_fixture;
mod okx_candle_feed_smoke;
#[cfg(feature = "execution")]
mod okx_classes_measure_smoke;
#[cfg(feature = "execution")]
mod okx_exec_client_smoke;
#[cfg(feature = "execution")]
mod okx_flatten_check;
#[cfg(feature = "execution")]
mod okx_leverage_smoke;
#[cfg(feature = "execution")]
mod okx_modes_smoke;
#[cfg(feature = "execution")]
mod okx_perp_smoke;
mod okx_trades_feed_smoke;
#[cfg(feature = "execution")]
mod okx_userdata_soak;
mod order_client_id_map_failure_repro;
