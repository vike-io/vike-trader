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

// The shared builders and test doubles of this crate's integration binaries — ONE copy, included by
// every grouped root and `runtime_smoke.rs` (its module doc carries the contract).
#[path = "support/mod.rs"]
mod kit;

// `#[path]` because this file is a test-target CRATE ROOT (see `tests/recon.rs`).
#[path = "journal/account_trading_state.rs"]
mod account_trading_state;
#[path = "journal/admit_symbol_replay.rs"]
mod admit_symbol_replay;
#[path = "journal/amend_journal.rs"]
mod amend_journal;
#[path = "journal/boot_restore.rs"]
mod boot_restore;
#[path = "journal/conditional_journal.rs"]
mod conditional_journal;
#[path = "journal/double_launch.rs"]
mod double_launch;
#[path = "journal/fee_adjustment_replay.rs"]
mod fee_adjustment_replay;
#[path = "journal/gateway_cursor.rs"]
mod gateway_cursor;
#[path = "journal/gtd_expiry_journal.rs"]
mod gtd_expiry_journal;
#[path = "journal/journal_wiring.rs"]
mod journal_wiring;
#[path = "journal/labelled_refold.rs"]
mod labelled_refold;
#[path = "journal/lost_prefix_refold.rs"]
mod lost_prefix_refold;
#[path = "journal/margin_call_journal.rs"]
mod margin_call_journal;
#[path = "journal/mark_floor_restore.rs"]
mod mark_floor_restore;
#[path = "journal/no_foreign_symbol_is_unchanged.rs"]
mod no_foreign_symbol_is_unchanged;
#[path = "journal/per_symbol_margin_replay.rs"]
mod per_symbol_margin_replay;
#[path = "journal/per_symbol_margin_restore.rs"]
mod per_symbol_margin_restore;
#[path = "journal/quote_rate_replay.rs"]
mod quote_rate_replay;
#[path = "journal/quote_rate_restore.rs"]
mod quote_rate_restore;
#[path = "journal/reconcile_adopts_deposit_replay.rs"]
mod reconcile_adopts_deposit_replay;
#[path = "journal/reconcile_replay.rs"]
mod reconcile_replay;
#[path = "journal/replay_cli.rs"]
mod replay_cli;
#[path = "journal/replay_fence.rs"]
mod replay_fence;
#[path = "journal/restart_restore.rs"]
mod restart_restore;
#[path = "journal/snapshot_tier_restore.rs"]
mod snapshot_tier_restore;
#[path = "journal/unadmit_symbol_replay.rs"]
mod unadmit_symbol_replay;
