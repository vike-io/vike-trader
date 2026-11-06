//! `cli_verbs` -- ONE test binary over what used to be nineteen: the vike-cli integration suites that
//! exercise the CLI's verbs (help, config, secrets, init, node, mcp, backtest reads, indicators) and
//! need NO shared kit — none declares `tests/common/`, which is `tests/shipped_cli/main.rs`'s. The
//! few helpers two members share are this group's own `support.rs`.
//!
//! Cargo links a separate test BINARY per top-level `tests/*.rs` file, so each of the nineteen paid
//! its own link; here they pay one. Each former `tests/<name>.rs` is now `tests/cli_verbs/<name>.rs`
//! (`secrets_cli.rs` with its `#[path]` children in `secrets_cli/`), included below as a plain
//! module: the tests, their names and their bodies are unchanged -- only the binary they link into
//! moved, so a test's id is `cli_verbs <name>::<test>`.
//!
//! ```sh
//! cargo test -p vike-cli --test cli_verbs                  # the whole group
//! cargo test -p vike-cli --test cli_verbs -- secrets_cli:: # one member, by module name
//! ```
//!
//! A sibling of `shipped_cli` rather than part of it: a single test crate is one serial rustc unit,
//! and MEASURED 2026-10-31 folding all thirty-five suites into one doubled the cold wall time; two
//! groups build side by side.
//!
//! The grouping rule is `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation": every member
//! is plain -- no `#![cfg]` feature gate, no `#[ignore]`d live or heavy test, no proptest sidecar, no
//! process-global mutation -- because plain `cargo test` (the `light-consumers` lane in
//! `scripts/ci_feature_suite.sh`) runs one binary's tests as THREADS in one process. The one
//! `#[ignore]` inside (`capability_projection_gate`'s `every_claimed_cli_verb_exists_in_the_published_surface`)
//! is an in-process read of the surface data, parked until its key exists, not a live or heavy test.

mod backtest_flags_schema;
mod backtest_read_cli;
mod bootstrap_daemon_cli;
mod capability_projection_gate;
mod config_set_cli;
mod config_unset_cli;
mod demo_tape_profile;
mod help_cli;
mod indicators_cli;
mod init_cli;
mod mcp_transcript;
mod mcp_unattended_cli;
mod message_literals;
mod node_cli;
mod seal_enforcement;
mod search_walkforward_cli;
mod secrets_cli;
mod settings_layers_reachable;
mod support;
mod user_indicators_cli;
