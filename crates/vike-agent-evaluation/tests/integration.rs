//! **vike-agent-evaluation's integration tests as ONE test binary** (`cargo test -p vike-agent-evaluation --test integration`).
//!
//! Each file keeps its path, `tests/<name>.rs`; here it is a MODULE, so a test's id is `<name>::<test>` and
//! `cargo test -p vike-agent-evaluation --test integration -- <name>::` runs one file. One binary is compiled and
//! linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s "Test-binary
//! consolidation": no member mutates process-global state (the shipped binaries they drive run as child
//! processes, each in a temporary directory of its own). `tests/relative_work_dir.rs` is NOT a member: it changes
//! the process's working directory, so it stays a binary of its own (its `[[test]]` row in
//! `crates/vike-agent-evaluation/Cargo.toml`). Test discovery is OFF (`autotests = false`), so a new `tests/*.rs`
//! builds only as a `mod` here or a `[[test]]` row, and `crates/vike-ops/tests/hygiene/test_registry_gate.rs`'s `every_root_test_file_is_built_where_discovery_is_off`
//! fails until it is one.

mod anthropic_wire;
mod claude_cli;
mod grader;
mod scripted_pipeline;
mod unattended_e2e;
mod unattended_record;
