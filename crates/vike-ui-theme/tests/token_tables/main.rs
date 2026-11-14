//! **The three token-table suites, ONE test binary** (`--test token_tables`): the `[[value]]`, `[[mode]]`
//! and `[[map]]` tables of `crates/vike-ui-theme/ui-theme.toml`, each planted with every mistake its
//! parser in `crates/vike-ui-theme/src/tokens_gen/` refuses and held equal to what the crate compiles.
//!
//! Grouped per `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section, so the crate
//! links one binary where it linked three. Each suite is a module named after the file it was, so a
//! test's name is unchanged after its module prefix: `cargo test -p vike-ui-theme --test token_tables
//! -- value_table::`. Every member is plain (no feature gate, no `#[ignore]`, no proptest sidecar, no
//! `env::set_var`/`set_current_dir`). `tests/brand_assets.rs` stays its own binary because
//! `.github/workflows/brand-assets.yml` and the regeneration command name it, and
//! `tests/rules_of_use_gate.rs` because the public mirror withholds it.

mod map_table;
mod mode_table;
mod support;
mod value_table;
