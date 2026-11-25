//! **The strategy lab's integration tests as ONE test binary** (`cargo test -p vike-strategy-lab --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, with its children in `tests/<name>/`; here it is a MODULE, so a
//! test's id is `<name>::<test>` and `cargo test -p vike-strategy-lab --test integration -- <name>::` runs one
//! file. A child names what its file imported as `super::<name>`. One binary is compiled and linked instead of one
//! per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member
//! mutates process-global state (each member's scratch under `CARGO_TARGET_TMPDIR` has a name of its own). Test
//! discovery is OFF (`autotests = false`), so a new `tests/*.rs` builds only as a `mod` here or a `[[test]]` row,
//! and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

mod example;
mod gallery;
mod headers;
mod makers;
mod param_gates;
mod param_keys_gate;
mod sdk_adapters;
mod takers;

// The roster both param gates check (`param_gates.rs` and `param_keys_gate.rs` name it `crate::roster`), compiled
// once. Each gate reads part of it, so an `allow`.
#[allow(dead_code, clippy::allow_attributes)]
#[path = "param_gates/roster.rs"]
mod roster;
