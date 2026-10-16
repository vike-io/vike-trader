//! Emulator-journal PR-1 gate: an emulated conditional's FIRE is journaled, and a session that
//! fires one replays to an IDENTICAL `state_hash`.
//!
//! Before this, `replay.rs`'s module doc named `submit_fired` as an unjournaled residual: the
//! trigger is a runtime reaction to a bar/tick, market data is deliberately never journaled, so
//! the released order simply did not exist in the record — the determinism fence caught the
//! divergence as `ReplayError::HashMismatch`. `conditional_fire_session_replays_deterministically`
//! is that scenario, green; `an_unjournaled_fire_would_have_mismatched` documents WHY it used to
//! fail by fencing the same journal with the fire record filtered out.
//!
//! The companion guards: a session that arms NOTHING writes neither new record kind (the
//! byte-identity posture — the two variants only ever appear for sessions that actually use
//! conditionals), and the ARM writes its RESOLVED terms (`ConditionalArmed`) including a trailing
//! arm's mark-seeded extreme, which the write-ahead `Cmd` for the intent cannot carry.
//!
//! Both firing call sites are covered: the TICK path (`conditionals_on_ticks`, most tests here)
//! and the CLOSED-BAR path (`the_bar_path_journals_its_fire_and_replays`). And
//! `arm_ids_do_not_collide_across_a_restart` fences the arm-id uniqueness the restored coid
//! session would otherwise break.
//!
//! Emulator PR-2 adds the DISARM half: `the_disarm_is_journaled_and_the_session_replays`
//! (the `ConditionalDisarmed` record + the determinism fence over a disarm session),
//! `a_disarmed_conditional_stays_disarmed_across_a_restart` (the restart sentinel), and
//! `arm_ids_survive_a_prune_then_restart_without_colliding` (the prune-safe `Snap.arm_seq`
//! resume — the record-count formula this replaces undercounted after
//! `prune_before_latest_snap` deleted early segments).
//!
//! Emulator PR-3 adds RE-ARM-ON-RESTORE: the books ride `Snap.conditionals` and come back via
//! `RestoredState::conditionals` -> `CoreConfig::conditionals`.
//! `armed_conditionals_survive_a_restart_with_the_trailing_extreme_preserved` is the flipped
//! sentinel (the restart used to DROP armed conditionals; now both a fixed and a trailing stop —
//! ratcheted extreme included — survive and fire identically), and
//! `a_crash_tail_fire_and_arm_fold_into_the_restored_books` gates the crash-tail record fold
//! (a fired arm never resurrects; a tail arm re-arms from its `ConditionalArmed`).

#[path = "conditional_journal/crash_tail_and_prune.rs"]
mod crash_tail_and_prune;
#[path = "conditional_journal/disarm_and_restart.rs"]
mod disarm_and_restart;
#[path = "conditional_journal/fire_journaling.rs"]
mod fire_journaling;
#[path = "conditional_journal/support.rs"]
mod support;
#[path = "conditional_journal/widened_fence.rs"]
mod widened_fence;
