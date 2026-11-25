//! **vike-config's standalone suites as ONE test binary** (`cargo test -p vike-config --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs` (and its children in `tests/<suite>/`, declared there by
//! `#[path]`, which resolves from that file's own directory), and is a MODULE here, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-config --test integration -- <suite>::` runs one suite. Every
//! test and assertion is the one the suite's own binary ran; one binary links instead of twelve. Grouped by
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global state
//! (each builds its own `tempfile` settings directory and passes any environment as a map), because a
//! threaded `cargo test` runs them all in this one process. `tests/rows.rs` stays the settings-row group's
//! own binary.
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so every file keeps its path. A new
//! `tests/*.rs` is built only as a `mod` here or a `[[test]]` row, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails until it is one or the other.

mod boot;
mod ceilings_are_distinct;
mod flag_registry;
mod layers_are_reachable;
mod load;
mod mirror;
mod params_wire_identity;
mod policy_is_consumed;
mod provenance;
mod removed;
mod settings_are_consumed;
mod shared_book_table;
