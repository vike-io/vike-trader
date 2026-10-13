//! Emulator PR-6, the widened fence: books, the ratchet exclusion, the counter fence, mark lanes.

use std::path::Path;

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::spawn_core;
use vike_exec::{Command, ConditionalIntent, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};

use super::support::{core_config, unique_dir};
use crate::kit::engines::sim_engine;
use crate::kit::events::{sim_bare_fill, sim_quote};
use crate::kit::journal::records;

// ---------------------------------------------------------------------------------------------
// Emulator PR-6 — the WIDENED fence (the epic closer).
//
// Every PR above widened what REPLAYS; none widened what is CHECKED. `replay_offline` fenced
// exactly one thing — `state_hash(&engines)` — while `restore_from_journal` hands a restart three
// more values nothing looked at: the resting books PR-3 made restorable, and the `coid_seq`/
// `arm_seq` counters. The tests below are the two halves of closing that: a NEGATIVE control
// proving a book divergence used to slip through a green fence, and a POSITIVE one proving the
// single documented exclusion (a trailing arm's ratcheted extreme) is a scalpel, not a hole.
// ---------------------------------------------------------------------------------------------

/// Write `recs` verbatim into a fresh journal at `dst` (valid frames, current header version).
/// The doctoring tests below transform a REAL session's records and re-emit them through this, so
/// only the one field under test differs from a journal a live core would have written.
fn rewrite(dst: &Path, recs: Vec<JournalRecord>) {
    let mut j = CommandJournal::open(dst, JournalFileConfig::default()).unwrap();
    for r in recs {
        match r {
            JournalRecord::Cmd { now_ms, msg, .. } => {
                j.append_cmd(now_ms, &msg).unwrap();
            }
            JournalRecord::Snap {
                now_ms,
                engines,
                coid_session,
                coid_seq,
                arm_seq,
                conditionals,
                contingencies,
                mount_attr,
                hash,
                ..
            } => {
                j.append_snap(
                    now_ms,
                    &engines,
                    &coid_session,
                    coid_seq,
                    arm_seq.unwrap_or(0),
                    &conditionals,
                    &contingencies,
                    &mount_attr,
                    hash,
                )
                .unwrap();
            }
            JournalRecord::MintedSubmit { now_ms, req, .. } => {
                j.append_minted_submit(now_ms, &req, None).unwrap();
            }
            JournalRecord::StrategySubmit { now_ms, mount_id, intent, .. } => {
                j.append_strategy_submit(now_ms, &mount_id, &intent).unwrap();
            }
            JournalRecord::ConditionalArmed { now_ms, arm_id, resolved, .. } => {
                j.append_conditional_armed(now_ms, &arm_id, &resolved).unwrap();
            }
            JournalRecord::ConditionalDisarmed { now_ms, arm_id, .. } => {
                j.append_conditional_disarmed(now_ms, &arm_id).unwrap();
            }
            JournalRecord::ConditionalFire { now_ms, arm_id, trigger_px, req, .. } => {
                j.append_conditional_fire(now_ms, &arm_id, trigger_px, &req).unwrap();
            }
            JournalRecord::PortfolioSnap { now_ms, sample, .. } => {
                j.append_portfolio_snap(now_ms, &sample).unwrap();
            }
            JournalRecord::MarginCallLiquidate { now_ms, req, mount_id, .. } => {
                j.append_margin_call_liquidate(now_ms, &req, mount_id.as_deref(), None).unwrap();
            }
            JournalRecord::GtdExpire { now_ms, coid, engine, .. } => {
                j.append_gtd_expire(now_ms, &coid, engine).unwrap();
            }
            JournalRecord::ScheduleFire { now_ms, mount_id, tag, .. } => {
                j.append_schedule_fire(now_ms, &mount_id, &tag).unwrap();
            }
        }
    }
    j.flush().unwrap();
    drop(j);
}

/// A real session that arms a fixed stop and then DISARMS it, ending with the book empty.
fn build_armed_then_disarmed_journal(dir: &Path) {
    let handle = spawn_core(sim_engine(), core_config(dir));
    let sender = handle.event_sender();
    for t in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(t, 1.0, 100.0)).unwrap();
    }
    // 4th record -> cadence Snap == the replay BASE (books still empty here)
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.send_command(Command::Order(OrderIntent::DisarmConditional {
        arm_id: "cafef00da0".into(),
    }));
    handle.shutdown_and_join();
}

/// THE PR-6 gate, negative half: a divergence in the RESTORED BOOKS that the engine `state_hash`
/// cannot see is now caught. Strip the `ConditionalDisarmed` record (the pre-PR-2 hole, and the
/// exact shape a `fold_conditionals` bug would produce) and the restore resurrects an arm the live
/// session had consumed — a restart would re-arm a stop the operator explicitly cancelled.
///
/// The disarm moves NO engine state (it mutates the core's book, not the OMS), so `state_hash` is
/// bit-identical either way: before this PR the doctored journal replayed GREEN. The assertion is
/// deliberately on the `conditionals` FIELD, not merely on `is_err()`, so a future change that
/// starts failing this journal for some unrelated reason cannot silently pass as coverage.
#[test]
fn the_book_fence_catches_a_resurrected_arm() {
    let src = unique_dir("pr6-books-src");
    build_armed_then_disarmed_journal(&src);
    let recs = records(&src);
    assert!(
        recs.iter().any(|r| matches!(r, JournalRecord::ConditionalDisarmed { .. })),
        "precondition: the session really disarmed"
    );
    replay_offline(&src).expect("the intact session passes every fence");

    // The control: the SAME journal minus the disarm record. The `Cmd` carrying the
    // `DisarmConditional` intent stays (the fold answers membership from records, by design), so
    // the ONLY difference is the record whose absence the fence must catch.
    let doctored = unique_dir("pr6-books-doctored");
    rewrite(
        &doctored,
        recs.into_iter()
            .filter(|r| !matches!(r, JournalRecord::ConditionalDisarmed { .. }))
            .collect(),
    );

    match replay_offline(&doctored) {
        Err(vike_core::replay::ReplayError::RestoreMismatch { field, .. }) => {
            assert_eq!(field, "conditionals", "the BOOK fence is what rejected it");
        }
        other => panic!(
            "a resurrected arm must fail the widened fence (it passed the engine hash fence \
             before PR-6), got {other:?}"
        ),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&doctored);
}

/// THE PR-6 gate, positive half: the one documented exclusion is load-bearing. A trailing arm's
/// `extreme` ratchets on non-journaled market data, so the reproduced book carries the
/// `ConditionalArmed` SEED where the live exit `Snap` carries the RATCHETED value — a real,
/// expected, safe-direction difference. `conditionals_hash` clears the field, so the session
/// replays green; a fence that hashed the extreme would reject this perfectly valid journal.
///
/// The test asserts the two values genuinely DIFFER first, so it cannot degenerate into proving
/// nothing once the scenario stops ratcheting.
#[test]
fn the_ratcheted_extreme_is_a_documented_exclusion_not_a_hole() {
    let dir = unique_dir("pr6-extreme");
    let handle = spawn_core(sim_engine(), core_config(&dir));
    let sender = handle.event_sender();
    for t in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(t, 1.0, 100.0)).unwrap();
    }
    handle.tick_sender().quote(sim_quote(100.0, 5)).unwrap(); // the mark the arm seeds from
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: None,
        trail: Some(5.0),
        trigger_by: None,
    })));
    handle.tick_sender().quote(sim_quote(110.0, 6)).unwrap(); // ratchets the extreme 100 -> 110
    handle.shutdown_and_join();

    let recs = records(&dir);
    let seeded = recs
        .iter()
        .find_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => resolved.extreme,
            _ => None,
        })
        .expect("the arm recorded its mark-seeded extreme");
    let recorded = recs
        .iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snap { conditionals, .. } => conditionals.first().and_then(|c| {
                c.terms.extreme.map(|e| (e, vike_core::replay::conditionals_hash(conditionals)))
            }),
            _ => None,
        })
        .expect("the exit Snap captured the still-armed trailing stop");
    assert_eq!(seeded, 100.0, "the ARM record holds the seed");
    assert_eq!(recorded.0, 110.0, "the exit Snap holds the RATCHETED extreme");

    // The reproduced book carries the seed, the recorded one the ratchet — same hash regardless.
    let reproduced = restore_from_journal(&dir).unwrap().expect("a Snap exists");
    assert_eq!(reproduced.conditionals.len(), 1, "the trailing arm is restored");
    assert_eq!(
        vike_core::replay::conditionals_hash(&reproduced.conditionals),
        recorded.1,
        "the ratcheted extreme is excluded, so the reproduced book hashes equal to the recorded one"
    );
    replay_offline(&dir).expect("a ratcheting trailing arm must not fail the widened fence");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The counter half of the widened fence, end to end: rewind nothing, but claim in the exit `Snap`
/// that the live session had spent MORE arm ids than the replay reproduces. That is precisely the
/// #460 bug class (a restored counter below the ids already spent re-mints a DISARM KEY), and
/// before PR-6 `replay_offline` never compared the two at all.
#[test]
fn the_counter_fence_catches_a_reproduced_arm_seq_below_the_recorded_one() {
    let src = unique_dir("pr6-armseq-src");
    build_armed_then_disarmed_journal(&src);
    replay_offline(&src).expect("the intact session passes every fence");

    let mut recs = records(&src);
    let last_snap = recs
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snap { .. }))
        .expect("a clean shutdown always writes an exit Snap");
    if let JournalRecord::Snap { arm_seq, .. } = &mut recs[last_snap] {
        *arm_seq = Some(arm_seq.unwrap_or(0) + 1_000); // ids the replay cannot account for
    }
    let doctored = unique_dir("pr6-armseq-doctored");
    rewrite(&doctored, recs);

    match replay_offline(&doctored) {
        Err(vike_core::replay::ReplayError::RestoreMismatch { field, expected, got }) => {
            assert_eq!(field, "arm_seq", "the COUNTER fence is what rejected it");
            assert!(got < expected, "and it rejected for the undercount direction");
        }
        other => panic!("a reproduced arm_seq below the recorded one must be rejected: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&doctored);
}

/// The MARK-lane arm (w2 `trigger_by`): a `Some(Mark)` arm must ignore the LAST lane entirely and
/// fire only off a mark tick — and the resulting session must still replay to a bit-identical
/// `state_hash`. This is the scenario the whole field exists for: last=99, mark=94, SL=95 fires on
/// a mark-triggering venue (hyperliquid) and on no last-triggering one.
///
/// Both halves matter, and the two lanes deliberately cross at DIFFERENT prices so the recorded
/// `trigger_px` NAMES the lane that fired: the LAST quote at 90 crosses the 95 stop and must not
/// fire it (a leak there would record 90), and the later MARK tick at 94 must (recording 94). A
/// fire COUNT alone could not tell the two apart — either lane leaking still yields exactly one
/// fire, since the first crossing consumes the arm.
#[test]
fn a_mark_lane_arm_ignores_last_fires_on_mark_and_replays_deterministically() {
    let dir = unique_dir("mark-lane-fire");
    let handle = spawn_core(sim_engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th record -> cadence Snap (BASE)

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: Some(vike_model::TriggerBy::Mark),
    })));
    // a LAST print DEEP through the stop: the Mark arm must rest through it (a leak records 90)
    handle.tick_sender().quote(sim_quote(90.0, 5)).unwrap();
    // …and the MARK tick at 94 is what fires it (recording 94)
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 94.0,
        ts: 6,
    });
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed.len(), 1);
    assert_eq!(
        armed[0].trigger_by,
        Some(vike_model::TriggerBy::Mark),
        "the requested source rides the ARM record, so a restore re-arms on the same lane"
    );
    let fired: Vec<f64> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { trigger_px, .. } => Some(*trigger_px),
            _ => None,
        })
        .collect();
    assert_eq!(
        fired,
        vec![94.0],
        "fired exactly once, at the MARK price — 90.0 here would mean the LAST lane leaked"
    );

    replay_offline(&dir).expect("a mark-fired session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The inert-default fence: a `None` (default) arm behaves EXACTLY as it did before `trigger_by`
/// existed — it fires off the LAST lane and a mark tick at the same crossing price never triggers
/// it. Pins that the mark lane added no firing surface to a source-less arm.
#[test]
fn a_default_arm_still_fires_on_last_and_never_on_mark() {
    let dir = unique_dir("default-lane-fire");
    let handle = spawn_core(sim_engine(), core_config(&dir));
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap();

    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    // a MARK tick DEEP through the stop: a default (Last) arm must rest through it (leak = 90)
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 90.0,
        ts: 5,
    });
    // …the LAST quote at 94 is what fires it, exactly as before the field existed
    handle.tick_sender().quote(sim_quote(94.0, 6)).unwrap();
    handle.shutdown_and_join();

    let recs = records(&dir);
    let armed: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { resolved, .. } => Some(resolved),
            _ => None,
        })
        .collect();
    assert_eq!(armed[0].trigger_by, None, "no source requested — the record stays bare");
    let fired: Vec<f64> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalFire { trigger_px, .. } => Some(*trigger_px),
            _ => None,
        })
        .collect();
    assert_eq!(
        fired,
        vec![94.0],
        "fired exactly once, at the LAST price — 90.0 here would mean the MARK lane leaked \
         into a source-less arm"
    );

    replay_offline(&dir).expect("the default path replays exactly as it always did");
    let _ = std::fs::remove_dir_all(&dir);
}

/// An `Index` arm is REFUSED at apply time (the core has no index lane), so no arm and no
/// `ConditionalArmed` record exist — and the session still replays. Never armed inert, never
/// evaluated off a substituted series.
#[test]
fn an_index_arm_is_refused_and_journals_no_arm() {
    let dir = unique_dir("index-refused");
    let handle = spawn_core(sim_engine(), core_config(&dir));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: Some(vike_model::TriggerBy::Index),
    })));
    // a crossing print on BOTH lanes: a refused arm can fire on neither
    handle.tick_sender().quote(sim_quote(94.0, 2)).unwrap();
    handle.market_sender().publish(vike_exec::MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 94.0,
        ts: 3,
    });
    handle.shutdown_and_join();

    let recs = records(&dir);
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalArmed { .. })),
        "the Index request was refused before minting an id — nothing was armed"
    );
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::ConditionalFire { .. })),
        "and nothing fired on either lane"
    );
    replay_offline(&dir).expect("a refused arm leaves a replayable session");
    let _ = std::fs::remove_dir_all(&dir);
}
