//! `integration` — vike-core's integration suites as ONE test binary, so the shared kit
//! (`tests/support/`) and the scratch guard (`src/scratch.rs`) compile ONCE instead of once per
//! binary (owner's order of 2026-10-31: no helper compiled again in every test binary). It merged
//! the `journal`, `recon`, `wiring`, `runtime_smoke` and `gateway_event_fold` binaries, and then
//! `drawdown_measures_own_pnl` (no shared helper, the same featureless configuration: one link fewer,
//! `tests/integration/<former>.rs` is where a single-file former lives); grouped per
//! `crates/vike-backtest/CLAUDE.md`'s "Test-binary consolidation" section, with `--test integration`
//! in the slot each of those names held:
//!
//! ```sh
//! cargo test -p vike-core --test integration                # every suite here
//! cargo test -p vike-core --test integration -- quarantine  # by name substring, as before
//! ```
//!
//! The members of the three former groups (`tests/{journal,recon,wiring}/<name>.rs`) are FLAT modules
//! below, so their test names did not change (`journal/double_launch.rs` re-runs this binary by its
//! exact name, `double_launch::the_second_launch_child`). `runtime_smoke`, `gateway_event_fold` and
//! `drawdown_measures_own_pnl` keep their own module (their members and tests share items of their root
//! through `use super::*`), so their tests are named `runtime_smoke::<member>::<test>`,
//! `gateway_event_fold::<test>` and `drawdown_measures_own_pnl::<test>`.
//!
//! Every member is plain: no crate-level `#![cfg]`, no proptest sidecar, no process-global mutation
//! (`env::set_var`/`set_current_dir`), because a plain `cargo test` runs one binary's tests as THREADS
//! in one process (nextest gives each its own). The two `#[ignore]`d tests run only by name:
//! `double_launch`'s child (spawned by its parent) and `gateway_event_fold`'s print-only release
//! measurement (its module doc carries the command).
//!
//! NOT here, on purpose: `runtime_latency.rs` (`#[ignore]`d + serial; CI's latency job builds it by name
//! as `--test runtime_latency` — it must never join a group).

// The owned `/tmp` scratch guard every journal directory in this binary is allocated through — ONE
// source file, shared with the crate's own `mod tests` blocks through `src/lib.rs`, pulled in here
// with the same `#[path]` idiom as `tests/common/latency_line.rs`. Its module doc carries the leak
// this closed: 27,471 directories / 211 GB left on the the CI box CI box by the `PathBuf`-returning
// helpers it replaced.
#[path = "../src/scratch.rs"]
mod scratch;

// The shared builders and test doubles of this crate's integration suites — ONE copy (its module doc
// carries the contract).
#[path = "support/mod.rs"]
mod kit;

// `#[path]` because this file is a test-target CRATE ROOT: a bare `mod recon_balance_crossing;` would
// resolve against `tests/` (the root's own directory), not `tests/recon/`.

// The journal/replay/restore suites.
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

// The reconciliation-driver suites.
#[path = "recon/recon_balance_crossing.rs"]
mod recon_balance_crossing;
#[path = "recon/recon_coin_delta.rs"]
mod recon_coin_delta;
#[path = "recon/recon_foreign_balance.rs"]
mod recon_foreign_balance;
#[path = "recon/recon_journal_crosscheck.rs"]
mod recon_journal_crosscheck;
#[path = "recon/recon_pass_branches.rs"]
mod recon_pass_branches;
#[path = "recon/recon_quarantine.rs"]
mod recon_quarantine;
#[path = "recon/restore_pass.rs"]
mod restore_pass;

// Core wiring, live params/schedule, multi-symbol routing and reads, controller, shutdown, the xEMM
// reference lane, and the held-divergence text gate.
#[path = "wiring/admit_symbol.rs"]
mod admit_symbol;
#[path = "wiring/amend_driven_by_the_run_loop.rs"]
mod amend_driven_by_the_run_loop;
#[path = "wiring/control_boundary.rs"]
mod control_boundary;
#[path = "wiring/controller_live_params.rs"]
mod controller_live_params;
#[path = "wiring/core_ergonomics.rs"]
mod core_ergonomics;
#[path = "wiring/counters_wiring.rs"]
mod counters_wiring;
#[path = "wiring/cross_venue_runtime.rs"]
mod cross_venue_runtime;
#[path = "wiring/held_divergence_is_visible_gate.rs"]
mod held_divergence_is_visible_gate;
#[path = "wiring/hot_engine_add.rs"]
mod hot_engine_add;
#[path = "wiring/hot_engine_stop.rs"]
mod hot_engine_stop;
#[path = "wiring/labelled_fanout.rs"]
mod labelled_fanout;
#[path = "wiring/live_engine_verbs.rs"]
mod live_engine_verbs;
#[path = "wiring/live_params.rs"]
mod live_params;
#[path = "wiring/live_schedule.rs"]
mod live_schedule;
#[path = "wiring/mark_floor.rs"]
mod mark_floor;
#[path = "wiring/mount_attr_durability.rs"]
mod mount_attr_durability;
#[path = "wiring/multi_mount.rs"]
mod multi_mount;
#[path = "wiring/multi_symbol_reads.rs"]
mod multi_symbol_reads;
#[path = "wiring/multi_symbol_routing.rs"]
mod multi_symbol_routing;
#[path = "wiring/per_symbol_margin.rs"]
mod per_symbol_margin;
#[path = "wiring/quote_rates.rs"]
mod quote_rates;
#[path = "wiring/readiness_gate_wiring.rs"]
mod readiness_gate_wiring;
#[path = "wiring/rhai_live_mount.rs"]
mod rhai_live_mount;
#[path = "wiring/shutdown_cancel_policy.rs"]
mod shutdown_cancel_policy;
#[path = "wiring/shutdown_teardown_phases.rs"]
mod shutdown_teardown_phases;
#[path = "wiring/sizing_equity_ceiling.rs"]
mod sizing_equity_ceiling;
#[path = "wiring/state_save_timer_wiring.rs"]
mod state_save_timer_wiring;
#[path = "wiring/strategy_state_wiring.rs"]
mod strategy_state_wiring;
#[path = "wiring/xemm_reference_quote.rs"]
mod xemm_reference_quote;

// The R5(b) runtime smoke (the headless full path against the R5(a) golden, the channel contract).
#[path = "runtime_smoke/mod.rs"]
mod runtime_smoke;

// What the `Ingest::GatewayEvent` arm costs the fold: `#[ignore]`d, print-only, run by hand.
#[path = "gateway_event_fold/mod.rs"]
mod gateway_event_fold;

// The drawdown latch measures the engine's own PnL. It builds its engines itself (`eng`,
// `adopt_wallet` are its subject) and takes nothing from the kit.
#[path = "integration/drawdown_measures_own_pnl.rs"]
mod drawdown_measures_own_pnl;
