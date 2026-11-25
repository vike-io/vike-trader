//! **vike-rust-strategy's integration tests as ONE test binary** (`cargo test -p vike-rust-strategy --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`, with its children in `tests/<name>/`; here it is a MODULE, so a
//! test's id is `<name>::<test>` and `cargo test -p vike-rust-strategy --test integration -- <name>::` runs one
//! file. One binary is compiled and linked instead of one per file, and the build script's walk
//! (`build/source_hash.rs`) compiles once: `source_hash.rs` declares it and `sdk_adapter.rs` names it
//! `crate::source_hash::source_hash`. Grouped by the rule of
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no member mutates process-global state (the
//! adapter stand-in, `vike_rust_strategy::sdk::with_adapters`, is per THREAD, and each member's scratch under
//! `CARGO_TARGET_TMPDIR` has a name of its own). Test discovery is OFF (`autotests = false`), so a new
//! `tests/*.rs` builds only as a `mod` here or a `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s
//! `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

mod boundary;
mod equivalence;
mod hook_roster;
mod load_refusals;
mod sdk;
mod sdk_adapter;
mod sdk_deploy;
mod source_hash;
