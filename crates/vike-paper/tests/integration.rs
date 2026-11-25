//! **vike-paper's suites as ONE test binary** (`cargo test -p vike-paper --test integration`).
//!
//! Each suite keeps its file, `tests/<suite>.rs`, with its `//!` contract; here it is a MODULE, so a test's id is
//! `<suite>::<test>` and `cargo test -p vike-paper --test integration -- <suite>::` runs one suite. One binary is
//! compiled and linked instead of one per file, grouped by the rule of `crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation": no member mutates process-global state. `tests/paper_halt_process_wide.rs` stays
//! its own binary: it re-executes ITS OWN binary under `--exact <test>` in a child whose working directory is a
//! synthetic project.
//!
//! ⚠ The manifest turns test discovery OFF (`autotests = false`) so the suites keep their paths: a new
//! `tests/*.rs` runs only once it is a `mod` here or a `[[test]]` row of `crates/vike-paper/Cargo.toml`.

mod combo_engine_fold;
mod multi_paper;
mod paper_amend_is_not_the_venues_amend;
mod paper_halt;
mod restart_fill_ledger;
