//! **vike-backfill's suites as ONE test binary** (`cargo test -p vike-backfill --features
//! databento,tardis,vikedata,vike-archive --test integration`, the feature union CI runs).
//!
//! Each suite keeps its file, `tests/<suite>.rs` (`dukascopy_backfill`'s children in `tests/dukascopy_backfill/`),
//! with its `//!` contract; here it is a MODULE, so a test's id is `<suite>::<test>` and
//! `--test integration -- <suite>::` runs one suite. One binary is compiled and linked instead of nine, grouped by
//! the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global
//! state. Every member keeps its own `#![cfg(feature = …)]`, so a build without a feature compiles that suite out,
//! as its own binary did. The `#[ignore]`d smokes run by hand as `--test integration -- --ignored <smoke>::`
//! (`eod_smoke`, `pmxt_smoke`, and the vendor-credit `databento_smoke` and `tardis_smoke`).
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so the suites keep their paths: a new
//! `tests/*.rs` runs only once it is a `mod` here or a `[[test]]` row of `crates/vike-backfill/Cargo.toml`.

mod argv_accepted;
mod databento_smoke;
mod dukascopy_backfill;
mod eod_smoke;
mod help_cli;
mod pmxt_smoke;
mod tardis_smoke;
mod vikedata_captured_pages;
mod vikedata_loader_parity;
