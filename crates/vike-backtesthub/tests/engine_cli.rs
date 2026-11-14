//! **The suites that spawn the shipped `backtesthub` engine, ONE test binary** (`--test engine_cli`):
//! `help_cli`, `optimizer_cli` (with its children) and `search_persist_cli`.
//!
//! Grouped (`crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" rule) so the two
//! helpers they share — `tests/support/engine.rs` (the scratch spawn every engine child goes through)
//! and `tests/support/local_datahub.rs` (the key-less loopback datahub) — compile ONCE: as three
//! binaries each compiled its own copy. The lib route was not open: `engine.rs` needs `tempfile` and
//! `local_datahub.rs` `vike-store`, both dev-dependencies. Every suite is a module named after the
//! file it was, so a test's name is unchanged after its module prefix and only the `--test` slot
//! moved: `cargo test -p vike-backtesthub --test engine_cli -- optimizer_cli::`. Every member is plain
//! (no feature gate, no `#[ignore]`, no proptest sidecar, no `env::set_var`/`set_current_dir`: each
//! child runs in its own scratch through `Command::current_dir`). The helpers stay in
//! `tests/support/`, where `search_persist_cli`'s `every_engine_spawn_goes_through_the_scratch_helper`
//! expects the one spawn.

#[path = "support/engine.rs"]
mod engine;
#[path = "support/local_datahub.rs"]
mod local_datahub;

#[path = "engine_cli/help_cli.rs"]
mod help_cli;
#[path = "engine_cli/optimizer_cli.rs"]
mod optimizer_cli;
#[path = "engine_cli/search_persist_cli.rs"]
mod search_persist_cli;
