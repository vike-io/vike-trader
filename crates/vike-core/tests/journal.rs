//! `journal` — vike-core's journal/replay/restore suite: ONE test binary over what used to be
//! seven. Same shape and same eligibility rule as `tests/recon.rs` beside this file — grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section; test names and bodies
//! unchanged, only the `--test <binary>` slot (`--test journal_wiring` is now `--test journal`).

// The owned `/tmp` scratch guard every journal directory in this binary is allocated through — ONE
// source file, shared with the crate's own `mod tests` blocks through `src/lib.rs`, pulled in here
// with the same `#[path]` idiom as `tests/common/latency_line.rs`. Its module doc carries the leak
// this closed: 27,471 directories / 211 GB left on the the CI box CI box by the `PathBuf`-returning
// helpers it replaced.
#[path = "../src/scratch.rs"]
mod scratch;

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "journal/conditional_journal.rs"]
mod conditional_journal;
#[path = "journal/gtd_expiry_journal.rs"]
mod gtd_expiry_journal;
#[path = "journal/journal_wiring.rs"]
mod journal_wiring;
#[path = "journal/margin_call_journal.rs"]
mod margin_call_journal;
#[path = "journal/replay_cli.rs"]
mod replay_cli;
#[path = "journal/replay_fence.rs"]
mod replay_fence;
#[path = "journal/restart_restore.rs"]
mod restart_restore;
