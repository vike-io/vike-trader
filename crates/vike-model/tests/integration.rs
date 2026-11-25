//! **vike-model's integration tests as ONE test binary** (`cargo test -p vike-model --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, with its children in `tests/<name>/`; here it is a MODULE, so a
//! test's id is `<name>::<test>` and `cargo test -p vike-model --test integration -- <name>::` runs one file.
//! One binary is compiled and linked instead of one per file, grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global state.
//! Test discovery is OFF (`autotests = false`), so a new `tests/*.rs` builds only as a `mod` here or a
//! `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.
//!
//! ⚠ Named `integration.rs`, never `main.rs`: proptest writes a failure seed BESIDE its source
//! (`tests/<name>.proptest-regressions`) only while no `main.rs` or `lib.rs` sits in a folder above that
//! source; one here would send every seed of the three property suites to a `proptest-regressions/` folder.

mod account_conversion_property_tests;
mod account_keys;
mod fill_parity;
mod host_build;
mod libm_platform_probe;
mod order_client_id_property_tests;
mod scalar_property_tests;
mod test_support;
mod trade_id_cannot_be_empty;
