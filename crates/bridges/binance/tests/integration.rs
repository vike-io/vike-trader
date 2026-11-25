//! **vike-binance's top-level test files as ONE test binary** (`cargo test -p vike-binance --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-binance --test integration -- <name>::`
//! runs one file (a live smoke adds `--ignored --nocapture`, as its own `//!` says). One binary is compiled and
//! linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a
//! `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails
//! one that is neither. A file that imports an `exec`-only module carries `#[cfg(feature = "execution")]` here.
//!
//! Still their own binaries:
//! * `offline`, the plain offline group: its `fee_not_read` asserts that a process-wide counter did NOT move,
//!   and the property suites below feed malformed fees through the same mappers on sibling threads;
//! * `binance_smokes`, the smokes folder;
//! * `binance_market_data_smoke`, named by `.github/workflows/live-smokes.yml`, whose guard
//!   (`scripts/refuse_live_credentials.sh`) scans every file of a binary the lane names and refuses a `#[path]`;
//! * `feed_status_journal` and `listenkey_stream_pin`: each installs `vike_log::capture::global`, the process's
//!   one global subscriber.

mod binance_backfill_smoke;
#[cfg(feature = "execution")]
mod binance_capture_smoke;
#[cfg(feature = "execution")]
mod binance_demo_smoke;
#[cfg(feature = "execution")]
mod binance_fullloop_smoke;
#[cfg(feature = "execution")]
mod binance_leverage_smoke;
mod binance_perp_feed_smoke;
#[cfg(feature = "execution")]
mod binance_perp_smoke;
#[cfg(feature = "execution")]
mod binance_post_only_smoke;
#[cfg(feature = "execution")]
mod binance_server_wave1_smoke;
#[cfg(feature = "execution")]
mod binance_spot_userdata_soak;
#[cfg(feature = "execution")]
mod binance_spot_wallet_probe;
#[cfg(feature = "execution")]
mod binance_tif_smoke;
#[cfg(feature = "execution")]
mod binance_userdata_soak;
mod decoder_never_panics;
#[cfg(feature = "execution")]
mod mapper_property_tests;
mod order_client_id_map_failure_repro;
mod typed_frame_property_tests;
