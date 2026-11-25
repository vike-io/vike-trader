//! **vike-ml-neural-training's integration tests as ONE test binary**
//! (`cargo test -p vike-ml-neural-training --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-ml-neural-training --test integration -- <name>::` runs one file. One binary is compiled
//! and linked instead of one per file, and `common` (the seeded rows) compiles once: a member names it
//! `crate::common`. Grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": no
//! member mutates process-global state unguarded (`burn`'s generator is process-global, and every network is
//! seeded and built under the crate's one lock, `crates/vike-ml-neural-training/src/lib.rs`'s `seeded`; the
//! trainer runs as a child process; each member's scratch under `CARGO_TARGET_TMPDIR` has a name of its own),
//! as its files already ran as threads of one process each. Test discovery is OFF
//! (`autotests = false`), so a new `tests/*.rs` builds only as a `mod` here or a `[[test]]` row, and
//! `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off` fails until it is one.

mod common;

mod compression;
mod deeplob;
mod flow;
mod onnx_export;
mod options_networks;
mod sharpe_study;
mod trainer_process;
mod training;
