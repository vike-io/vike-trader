//! **vike-deribit's top-level test files as ONE test binary** (`cargo test -p vike-deribit --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, its `//!` contract and its children in `tests/<name>/`; here it is
//! a MODULE, so a test's id is `<name>::<test>` and `cargo test -p vike-deribit --test integration -- <name>::`
//! runs one file (a live smoke adds `--ignored --nocapture`, as its own `//!` says). One binary is compiled and
//! linked instead of one per file, by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation".
//! Discovery is off (`autotests = false` in this crate's `Cargo.toml`), so a new `tests/*.rs` builds only as a
//! `mod` line here or a `[[test]]` row of its own, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs` fails
//! one that is neither.
//!
//! Still their own binaries: `offline`, the plain offline group (its module doc carries what may join it), and
//! `deribit_smokes`, the smokes folder.

mod decoder_never_panics;
mod deribit_any_symbol_testnet_smoke;
mod deribit_classes_testnet_smoke;
mod deribit_combo_fill_probe;
mod deribit_depth_live_smoke;
mod deribit_klines_live_smoke;
mod deribit_post_only_smoke;
mod deribit_smoke;
mod deribit_tif_smoke;
mod deribit_userdata_soak;
mod markprice_options_live_smoke;
mod ticker_bidask_live_smoke;
