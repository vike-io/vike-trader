//! The crash-tail record fold, the refused tail, the pre-v7 snap fallback, and prune then restart.

use vike_core::replay::restore_from_journal;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, ConditionalIntent, ExecutionEngine, OrderIntent};
use vike_journal::{CommandJournal, JournalFileConfig, JournalRecord};

use super::support::{core_config, unique_dir};
use crate::kit::engines::sim_engine;
use crate::kit::events::sim_quote;
use crate::kit::journal::{fnv1a32, records};

/// The CRASH-TAIL half of re-arm-on-restore: restored book membership is a pure fold of the
/// journal's conditional records over the latest Snap's books — NOT the replay core's own book
/// state, which never fires (its book would still hold an arm the live session consumed) and
/// refuses trailing arms (no mark). Hand-builds the crash shape (records AFTER the last Snap,
/// no exit Snap — a clean shutdown always writes one, so this cannot be produced through a core):
/// base books hold a trailing arm a0 (ratcheted extreme) + a fixed a1; the tail then FIRES a0
/// and ARMS a fixed a2. The restore must hold exactly {a1, a2} — a0 re-armed would DOUBLE an
/// already-taken exit — and resume the counter above a2.
#[test]
fn a_crash_tail_fire_and_arm_fold_into_the_restored_books() {
    use vike_journal::{ConditionalRecord, SnapConditional};

    let dir = unique_dir("crash-tail-fold");
    let base_books = vec![
        SnapConditional {
            arm_id: "cafef00da0".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(110.0),
                trigger_by: None,
            },
        },
        SnapConditional {
            arm_id: "cafef00da1".into(),
            terms: ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: Some(95.0),
                trail: None,
                extreme: None,
                trigger_by: None,
            },
        },
    ];
    {
        let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
        let engines = vec![sim_engine().snapshot_state()];
        let hash = vike_exec::state_hash(&engines);
        // the base Snap: 2 arms resting, counter at 2
        j.append_snap(100, &engines, "cafef00d", 0, 2, &base_books, &[], &[], hash).unwrap();
        // crash tail: a0 FIRED live (write-ahead record + the released market sell)...
        j.append_conditional_fire(
            101,
            "cafef00da0",
            105.0,
            &vike_model::OrderRequest {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 2.0,
                order_type: "market".into(),
                ts: 101,
                ..Default::default()
            },
        )
        .unwrap();
        // ...then a NEW fixed stop was armed (write-ahead Cmd + its resolved ConditionalArmed)...
        j.append_cmd(
            102,
            &vike_exec::Ingest::Command(Command::Order(OrderIntent::ArmConditional(
                ConditionalIntent {
                    venue: "sim".into(),
                    symbol: "BTCUSDT".into(),
                    side: -1,
                    qty: 3.0,
                    price: Some(90.0),
                    trail: None,
                    trigger_by: None,
                },
            ))),
        )
        .unwrap();
        j.append_conditional_armed(
            102,
            "cafef00da2",
            &ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 3.0,
                price: Some(90.0),
                trail: None,
                extreme: None,
                trigger_by: None,
            },
        )
        .unwrap();
        j.flush().unwrap();
        // ...and the process died here: no exit Snap.
    }

    let state = restore_from_journal(&dir).unwrap().expect("the Snap is the restore base");
    let ids: Vec<&str> = state.conditionals.iter().map(|c| c.arm_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["cafef00da1", "cafef00da2"],
        "the fired arm is CONSUMED (never re-armed — that would double the exit); the tail arm \
         is re-armed from its ConditionalArmed record"
    );
    assert_eq!(state.conditionals[1].terms.price, Some(90.0), "the tail arm's resolved terms");
    assert!(state.arm_seq >= 3, "the counter resumes above every spent id (got {})", state.arm_seq);
    let _ = std::fs::remove_dir_all(&dir);
}

/// #460 post-merge review MAJOR (confirmed by this exact probe): the restored `arm_seq` must
/// come from RECORD TRUTH, never from replay-side re-minting. The trailing branch of
/// `ArmConditional` refuses BEFORE `mint_arm_id` when the symbol has no mark, and replay's marks
/// come only from the base Snap (the tail pumps no market data) — so a cadence Snap taken before
/// the symbol ever ticked (arm_seq=K stamped, NO mark), followed live by a mark + a trailing arm
/// (mints a{K}, journals its `ConditionalArmed`), followed by a crash, used to replay with the
/// counter still at K: the resumed session re-minted a{K}, and a stale `DisarmConditional`
/// carrying the old a{K} would disarm the NEW arm — silently removing a fresh stop-loss. The fix
/// fast-forwards the counter to `max(replayed, snap_seed + tail ConditionalArmed count)`.
#[test]
fn a_refused_tail_trailing_arm_cannot_undercount_the_restored_arm_seq() {
    use vike_journal::ConditionalRecord;

    let dir = unique_dir("armseq-undercount");
    const K: u64 = 5;
    {
        let mut j = CommandJournal::open(&dir, JournalFileConfig::default()).unwrap();
        // Base Snap: arm_seq already at K, NO mark for the symbol (a fresh engine snapshot has
        // none), empty books.
        let engines = vec![sim_engine().snapshot_state()];
        let hash = vike_exec::state_hash(&engines);
        j.append_snap(100, &engines, "cafef00d", 0, K, &[], &[], &[], hash).unwrap();
        // Crash tail: live, the mark had arrived (marks are market data — never journaled), so
        // the trailing arm was APPLIED: its write-ahead Cmd + the resolved ConditionalArmed
        // naming a{K}. Replay has no mark and refuses the Cmd — the pre-fix counter never
        // advanced past K.
        j.append_cmd(
            101,
            &vike_exec::Ingest::Command(Command::Order(OrderIntent::ArmConditional(
                ConditionalIntent {
                    venue: "sim".into(),
                    symbol: "BTCUSDT".into(),
                    side: -1,
                    qty: 1.0,
                    price: None,
                    trail: Some(5.0),
                    trigger_by: None,
                },
            ))),
        )
        .unwrap();
        j.append_conditional_armed(
            101,
            &format!("cafef00da{K}"),
            &ConditionalRecord {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: -1,
                qty: 1.0,
                price: None,
                trail: Some(5.0),
                extreme: Some(100.0),
                trigger_by: None,
            },
        )
        .unwrap();
        j.flush().unwrap();
        // crash: no exit Snap.
    }

    let state = restore_from_journal(&dir).unwrap().expect("the Snap is the restore base");
    assert!(
        state.arm_seq > K,
        "the counter must resume ABOVE the spent a{K} (got {}) — record truth, not replay minting",
        state.arm_seq
    );
    // PR-3 bonus, asserted so it can't silently regress: the refused-in-replay trailing arm
    // still RE-ARMS on restore, from its ConditionalArmed record (seed extreme).
    assert_eq!(state.conditionals.len(), 1, "the crash-tail trailing arm is restored");
    assert_eq!(state.conditionals[0].arm_id, format!("cafef00da{K}"));
    assert_eq!(state.conditionals[0].terms.extreme, Some(100.0), "the ARM-record seed extreme");

    // And the next live mint really is a{K+1}, never a duplicate a{K}: restart, arm once, read
    // the new ConditionalArmed id back out of the journal.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let armed_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    let new_id = armed_ids.last().expect("run 2 armed one conditional");
    assert_ne!(new_id, &format!("cafef00da{K}"), "the spent id must never be re-minted");
    assert_eq!(
        armed_ids.iter().filter(|id| *id == new_id).count(),
        1,
        "the new id collides with nothing in the journal"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// #460 post-merge review MINOR: the pre-v7 fallback branch (`Snap.arm_seq` absent -> seed =
/// max-global-record-seq + 1) had zero coverage. Byte-surgery a real journal into the pre-v7
/// shape (strip `arm_seq` — and the v8 `conditionals` — from every Snap payload, restamp the
/// header version to 6), then prove `restore_from_journal` resumes the counter at the fallback
/// bound and a continued session mints ABOVE every pre-existing id.
#[test]
fn a_pre_v7_snap_falls_back_to_max_seq_plus_one_and_never_collides() {
    let dir = unique_dir("pre-v7-fallback");
    // Run 1 (a real core): a mark, one ARM (spends a0), shutdown (exit Snap).
    let handle = spawn_core(sim_engine(), core_config(&dir));
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(95.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();
    let max_seq = records(&dir)
        .iter()
        .map(|r| match r {
            JournalRecord::Cmd { seq, .. }
            | JournalRecord::Snap { seq, .. }
            | JournalRecord::StrategySubmit { seq, .. }
            | JournalRecord::MintedSubmit { seq, .. }
            | JournalRecord::PortfolioSnap { seq, .. }
            | JournalRecord::ConditionalArmed { seq, .. }
            | JournalRecord::ConditionalFire { seq, .. }
            | JournalRecord::ConditionalDisarmed { seq, .. }
            | JournalRecord::MarginCallLiquidate { seq, .. }
            | JournalRecord::GtdExpire { seq, .. }
            | JournalRecord::ScheduleFire { seq, .. } => *seq,
        })
        .max()
        .unwrap();

    // Byte-surgery every segment into the v6 shape: walk the [len][crc][payload] frames, strip
    // `arm_seq`/`conditionals` from each Snap payload, re-frame, restamp the header to 6.
    const HEADER: usize = 16; // magic u32 | version u32 | first_seq u64 (stable format)
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("vjl") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let mut out = bytes[..HEADER].to_vec();
        out[4..8].copy_from_slice(&6u32.to_le_bytes());
        let mut cur = HEADER;
        while cur + 8 <= bytes.len() {
            let len = u32::from_le_bytes(bytes[cur..cur + 4].try_into().unwrap()) as usize;
            if len == 0 || cur + 8 + len > bytes.len() {
                break;
            }
            let payload = &bytes[cur + 8..cur + 8 + len];
            let mut v: serde_json::Value = serde_json::from_slice(payload).unwrap();
            if let Some(snap) = v.get_mut("Snap").and_then(|s| s.as_object_mut()) {
                snap.remove("arm_seq");
                snap.remove("conditionals");
            }
            let doctored = serde_json::to_vec(&v).unwrap();
            out.extend_from_slice(&(doctored.len() as u32).to_le_bytes());
            out.extend_from_slice(&fnv1a32(&doctored).to_le_bytes());
            out.extend_from_slice(&doctored);
            cur += 8 + len;
        }
        out.resize(bytes.len().max(out.len()), 0);
        std::fs::write(&path, out).unwrap();
    }

    // Restore: the doctored Snap stamps no counter, so the fallback bound applies.
    let state = restore_from_journal(&dir).unwrap().expect("the doctored journal keeps its Snap");
    assert_eq!(state.arm_seq, max_seq + 1, "a pre-v7 base falls back to max-global-record-seq + 1");
    assert!(state.conditionals.is_empty(), "a pre-v7 Snap carries no books: restored EMPTY");

    // Continue on the same dir and arm again: the new id must be above every pre-existing one.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        conditionals: state.conditionals.clone(),
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(90.0),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let armed_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(armed_ids.len(), 2, "run 1's arm + run 2's arm");
    assert_eq!(armed_ids[0], "cafef00da0");
    assert_eq!(
        armed_ids[1],
        format!("cafef00da{}", max_seq + 1),
        "the continued session mints at the fallback bound — above every pre-existing id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// THE prune-safety gate (emulator PR-2, defect A): `RestoredState::arm_seq` must come from the
/// latest `Snap`'s STAMPED counter, not from counting currently-readable records —
/// `prune_before_latest_snap` deletes whole early segments (its own doc prescribes
/// prune-then-restore at restart), so the count lands BELOW the ids already spent and the
/// restored session re-mints duplicate arm ids: the disarm key. Scenario: mint many arms across
/// several small segments, snap, PRUNE, restart, arm again — the new id must be distinct from
/// every pre-prune id.
#[test]
fn arm_ids_survive_a_prune_then_restart_without_colliding() {
    let dir = unique_dir("arm-prune-restart");
    let n_arms: u64 = 40;
    // Small segments so the journal ROLLS and prune has early segments to delete; a generous
    // snapshot cadence so most records between snaps are the arms themselves.
    let cfg = CoreConfig {
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 8 * 1024, flush_every: 8 },
            snapshot_every: 16,
        }),
        ..core_config(&dir)
    };
    let handle = spawn_core(sim_engine(), cfg);
    for i in 0..n_arms {
        handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 1.0,
            price: Some(1.0 + i as f64), // far below any mark: never fires
            trail: None,
            trigger_by: None,
        })));
    }
    handle.shutdown_and_join();

    let run1_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    assert_eq!(run1_ids.len() as u64, n_arms, "every arm journaled its ConditionalArmed");

    // PRUNE, exactly as `prune_before_latest_snap`'s doc prescribes at restart (no live core on
    // the dir). The early, arm-bearing segments are deleted; the survivors must be FEWER records
    // than arms minted, or this scenario would not distinguish the two formulas.
    CommandJournal::prune_before_latest_snap(&dir).unwrap();
    let survivors = records(&dir).len() as u64;
    assert!(
        survivors < n_arms,
        "scenario check: pruning must leave fewer readable records ({survivors}) than arm ids \
         spent ({n_arms}) — the record-count formula would restart the counter INSIDE the spent \
         range and collide"
    );

    // Restore: the counter comes from the Snap's stamped arm_seq, unharmed by the prune.
    let state = restore_from_journal(&dir).unwrap().expect("the pruned journal keeps its Snap");
    assert_eq!(
        state.arm_seq, n_arms,
        "RestoredState::arm_seq is the Snap-stamped counter, not the survivor count"
    );

    // Restart onto the same (pruned) journal and arm ONE more.
    let cfg = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        coid_session: Some((state.coid_session.clone(), state.coid_seq)),
        arm_seq: state.arm_seq,
        ..core_config(&dir)
    };
    let engine2 = ExecutionEngine::from_snapshot(&state.engines[0], RecordingClient::default());
    let handle = spawn_core(engine2, cfg);
    handle.send_command(Command::Order(OrderIntent::ArmConditional(ConditionalIntent {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        price: Some(0.5),
        trail: None,
        trigger_by: None,
    })));
    handle.shutdown_and_join();

    let all_ids: Vec<String> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ConditionalArmed { arm_id, .. } => Some(arm_id),
            _ => None,
        })
        .collect();
    let new_id = all_ids.last().expect("run 2 armed one conditional").clone();
    assert_eq!(new_id, format!("cafef00da{n_arms}"), "the counter resumed ABOVE every spent id");
    assert!(
        !run1_ids.contains(&new_id),
        "post-restart arm id {new_id} must differ from every pre-prune id"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
