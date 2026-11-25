//! **vike-indicators' single-file suites as ONE test binary** (`cargo test -p vike-indicators --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-indicators --test integration -- <suite>::` runs one suite. One binary
//! is compiled and linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation": no suite mutates process-global state. Discovery is off (`autotests = false` in
//! this crate's `Cargo.toml`), so a `tests/*.rs` file that neither a `mod` line here nor a `[[test]]` row names
//! would compile into nothing; `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s
//! `every_root_test_file_is_built_where_discovery_is_off` fails on it.
//!
//! The other rows stay as they were: the three groups over a shared helper, `fast`, `parity` and `streamers`
//! (each declares its `tests/common/` helper once at its own root), `libm_platform_probe` (the per-crate probe
//! decision 0074 names, run as `--test libm_platform_probe` in every crate that has one) and `talib_probe`
//! (excluded from the published `.crate`, so no module of a shipped root may name it).

mod all_indicators_exact;
mod clone_box;
mod empty_extreme_is_nan;
mod feature_parity;
mod license_copy;
mod models_values;
mod oscillators_values;
mod outlier_recovery;
mod overlays_values;
mod readme_counts;
mod readme_plain_door;
mod realized_vol;
mod vectorize_hash;
mod window_pin;
