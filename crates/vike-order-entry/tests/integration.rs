//! **vike-order-entry's integration tests as ONE test binary** (`cargo test -p vike-order-entry --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, with its children in `tests/<name>/`; here it is a MODULE, so a
//! test's id is `<name>::<test>` and `cargo test -p vike-order-entry --test integration -- <name>::` runs one file
//! (a nextest filter: `binary(integration) & test(/^trade_fit::/)`). One binary is compiled and linked instead of
//! one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member
//! mutates process-global state (each test lays its widget out in an `egui` context, a kittest `Harness`, of its
//! own). Test discovery is OFF (`autotests = false`), so a new `tests/*.rs` builds only as a `mod` here or a
//! `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

mod chain_a11y;
mod cockpit_design;
mod option_chain_characterization;
mod ticket_a11y;
mod trade_actions;
mod trade_fit;
mod trade_no_book;
mod trade_tokens;
