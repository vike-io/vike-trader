//! Task 6 gate: crash-restart RESTORE from the LATEST journal checkpoint + journal continuation.
//!
//! The mirror image of the Task 5 determinism fence. `replay_offline` restores from the FIRST
//! `Snapshot` and RE-FOLDS the whole post-checkpoint tail to prove determinism; `restore_from_journal`
//! restores from the LATEST `Snapshot` and re-folds only the (usually empty) crash tail after it — the
//! fast restart path. It does NOT fence (a restore is not a verification).
//!
//! Three phases:
//!   1. Run a real core (TestExecutionClient fills + a Submit + a Cancel) spanning a cadence Snapshot +
//!      the exit Snapshot, journaled at `dir`.
//!   2. `restore_from_journal(&dir)` → `Some(state)`; rebuild an engine `from_snapshot` and assert
//!      its `snapshot_state()` hash EQUALS phase-1's final Snapshot hash (clean shutdown ⇒ the latest
//!      Snapshot already reflects everything, the tail is empty, so restore returns the final state).
//!   3. Spawn a NEW core FROM the restored engine with the SAME journal dir + restored order client id session;
//!      send one more fill; shut down. The journal seq must CONTINUE (new records' seq > phase-1's
//!      max — no reset to 0) and the new final Snapshot must reflect the extra fill (+1 seen trade id).

use crate::scratch::Scratch;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::testing::{RecordingClient, TestExecutionClient};
use vike_execution::{Command, Engine, EngineSnapshot, Ingest, OrderIntent};
use vike_journal::{Journal, JournalFileConfig, JournalRecord};
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderCanceled, OrderFilled, TradeId};
use vike_model::{Clock, OrderBracket, OrderRequest, TradingState};

use crate::kit::engines::{sim_engine, sim_engine_with};
use crate::kit::events::sim_bare_fill;
use crate::kit::requests::sim_limit;

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(tag)
}

/// A self-advancing clock (distinct `now_ms` per dispatched message), boxed for `CoreConfig`.
fn counter_clock(from: i64) -> Box<dyn Clock + Send> {
    let t = Arc::new(AtomicI64::new(from));
    Box::new(move || t.fetch_add(1, Ordering::Relaxed))
}

/// The LAST `Snapshot` in the journal — its stored `hash`, engine snapshots, and order client id session/seq.
fn last_snap(recs: &[JournalRecord]) -> (u64, Vec<EngineSnapshot>, String, u64) {
    recs.iter()
        .rev()
        .find_map(|r| match r {
            JournalRecord::Snapshot {
                hash,
                engines,
                order_client_id_session,
                order_client_id_sequence,
                ..
            } => Some((
                *hash,
                engines.clone(),
                order_client_id_session.clone(),
                *order_client_id_sequence,
            )),
            _ => None,
        })
        .expect("a Snapshot is always written on shutdown")
}

/// The highest record seq present across the whole journal.
fn max_sequence(recs: &[JournalRecord]) -> u64 {
    recs.iter().map(vike_journal::record_sequence).max().unwrap_or(0)
}

/// Phase 1: run the source scenario with a filling client, producing a journal at `dir` that spans
/// a cadence Snapshot (`snapshot_every = 4`) + the exit Snapshot. Exec-lane messages, in order:
/// 4 bare fills (t0..t3 — the 4th trips the cadence Snapshot), then Submit(ord1) [TestExecutionClient
/// synthesizes a fill `simt1`], a bare fill t5, and Cancel(ord1). Final `seen_trade_ids` =
/// {t0,t1,t2,t3,simt1,t5} = 6.
fn build_phase1_journal(dir: &Path) {
    let engine = sim_engine_with(TestExecutionClient::new("sim", 100.0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: counter_clock(0),
        order_client_id_session: Some(("cafef00d".into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    sender.blocking_send(sim_bare_fill("t0", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t1", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t2", 1.0, 100.0)).unwrap();
    sender.blocking_send(sim_bare_fill("t3", 1.0, 100.0)).unwrap(); // 4th exec record -> cadence Snapshot
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        "ord1", 1, 1.0, 100.0,
    ))))); // -> fills simt1
    sender.blocking_send(sim_bare_fill("t5", 1.0, 100.0)).unwrap();
    handle.send_command(Command::Order(OrderIntent::Cancel("ord1".into())));
    handle.shutdown_and_join();
}

/// PHASE 2: restore from the LATEST Snapshot reproduces the phase-1 final state (clean shutdown ⇒
/// empty tail ⇒ the exit Snapshot IS the restored state).
#[test]
fn restore_from_latest_snap_reproduces_final_state() {
    let dir = unique_dir("restore");
    build_phase1_journal(&dir);

    let recs = Journal::read_all(&dir).unwrap();
    let snaps = recs.iter().filter(|r| matches!(r, JournalRecord::Snapshot { .. })).count();
    assert!(snaps >= 2, "scenario must span a cadence Snapshot + the exit Snapshot (got {snaps})");
    let (final_hash, final_engines, final_session, _final_sequence) = last_snap(&recs);
    assert_eq!(final_engines.len(), 1, "v1 single-engine scenario");
    assert_eq!(final_engines[0].seen_trade_ids.len(), 6, "t0..t3 + simt1 + t5");

    // Restore from the LATEST snap; NO fence — it's a restore, not a verification.
    let state =
        restore_from_journal(&dir).expect("restore ok").expect("journal has a Snapshot -> Some");
    assert_eq!(state.engines.len(), 1, "v1 single-engine restore");
    assert_eq!(
        state.order_client_id_session, final_session,
        "restored order client id session carries the base's"
    );

    // Rebuild a fresh engine from the restored snapshot; its state hash must equal phase-1's final.
    let rebuilt = Engine::from_snapshot(&state.engines[0], RecordingClient::default());
    let rebuilt_hash = vike_execution::state_hash(&[rebuilt.snapshot_state()]);
    assert_eq!(
        rebuilt_hash, final_hash,
        "restore reproduces phase-1's final state hash bit-for-bit"
    );
}

/// An empty directory (no journal / no Snapshot) restores to `Ok(None)` — nothing to restore.
#[test]
fn restore_from_empty_dir_is_none() {
    let dir = unique_dir("restore-empty");
    std::fs::create_dir_all(&dir).unwrap();
    assert!(restore_from_journal(&dir).unwrap().is_none(), "empty dir -> Ok(None)");
}

/// A journal with records but NO Snapshot restores to `Ok(None)` (nothing to restore from).
#[test]
fn restore_with_records_but_no_snap_is_none() {
    let dir = unique_dir("restore-nosnap");
    {
        let mut j = Journal::open(&dir, JournalFileConfig::default()).unwrap();
        j.append_command(1, &Ingest::Command(Command::Order(OrderIntent::Cancel("x".into()))))
            .unwrap();
        j.flush().unwrap();
    }
    assert!(restore_from_journal(&dir).unwrap().is_none(), "records but no Snapshot -> Ok(None)");
}

/// PHASE 3: spawn a NEW core FROM the restored engine, SAME journal dir + restored order client id session,
/// add one fill, shut down. The journal seq CONTINUES (no reset) and the new final Snapshot extends the
/// state by exactly one seen trade id.
#[test]
fn continuation_resumes_sequence_and_extends_state() {
    let dir = unique_dir("restore-cont");
    build_phase1_journal(&dir);

    let recs1 = Journal::read_all(&dir).unwrap();
    let phase1_max_sequence = max_sequence(&recs1);
    let (_h1, engines1, _s1, _q1) = last_snap(&recs1);
    let phase1_seen = engines1[0].seen_trade_ids.len(); // 6

    // Phase 2: restore.
    let state = restore_from_journal(&dir).unwrap().expect("journal has a Snapshot");

    // Phase 3: continue on the SAME journal from the restored engine + order client id session.
    let engine = Engine::from_snapshot(&state.engines[0], TestExecutionClient::new("sim", 100.0));
    let config = CoreConfig {
        seed_cash: state.engines[0].equity_seed,
        clock: counter_clock(1_000),
        order_client_id_session: Some((
            state.order_client_id_session.clone(),
            state.order_client_id_sequence,
        )),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    handle.event_sender().blocking_send(sim_bare_fill("t6", 1.0, 100.0)).unwrap();
    handle.shutdown_and_join();

    // The journal continued (append resumed, no reset to seq 0).
    let recs2 = Journal::read_all(&dir).unwrap();
    let phase2_max_sequence = max_sequence(&recs2);
    assert!(
        phase2_max_sequence > phase1_max_sequence,
        "journal seq continued: {phase2_max_sequence} must exceed phase-1 max {phase1_max_sequence}"
    );
    let new_fill_sequence = recs2
        .iter()
        .find_map(|r| match r {
            JournalRecord::Command {
                sequence: seq,
                message: Ingest::Event(Event::Fill(f)),
                ..
            } if f.trade_id == "t6" => Some(*seq),
            _ => None,
        })
        .expect("the t6 fill was journaled");
    assert!(
        new_fill_sequence > phase1_max_sequence,
        "new fill seq {new_fill_sequence} continues past phase-1 max {phase1_max_sequence} (no reset)"
    );

    // The new final Snapshot reflects the added fill: exactly one more seen trade id than phase 1.
    let (_h2, engines2, _s2, _q2) = last_snap(&recs2);
    assert_eq!(
        engines2[0].seen_trade_ids.len(),
        phase1_seen + 1,
        "final Snapshot has exactly one more seen trade id"
    );
    assert_eq!(engines2[0].seen_trade_ids.len(), 7, "t0..t3 + simt1 + t5 + t6 = 7");
    assert!(
        engines2[0].seen_trade_ids.iter().any(|t| t == "t6"),
        "the continued snap includes the t6 fill"
    );
}

/// Live-runtime OTO/OCO crash-recovery: a bracket's HELD protective exits (kept OFF the venue until
/// the entry fills) live only in the runtime's side book — not the engine registry — so they must
/// ride the `Snapshot` (`JournalSnapshotContingency`) or a restart mid-bracket would leave a filled entry with no
/// stop-loss / take-profit. This proves `restore_from_journal` brings the whole contingency book
/// back: the entry active, both exits still held WITH their resolved requests, linked to the entry.
#[test]
fn bracket_held_exits_survive_a_restart() {
    let dir = unique_dir("restore-bracket");
    {
        let engine = sim_engine();
        let config = CoreConfig {
            seed_cash: 10_000.0,
            clock: counter_clock(0),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(JournalConfig {
                dir: dir.to_path_buf(),
                file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
                snapshot_every: 64,
            }),
            ..CoreConfig::default()
        };
        let handle = spawn_core(engine, config);
        handle.send_command(Command::Order(OrderIntent::Bracket(Box::new(
            vike_model::OrderBracket {
                venue: "sim".into(),
                symbol: "BTCUSDT".into(),
                side: 1,
                qty: 2.0,
                entry_price: Some(100.0),
                stop_loss: 95.0,
                take_profit: 110.0,
                account: None,
                order_client_id: None,
            },
        ))));
        handle.shutdown_and_join();
    }

    let state =
        restore_from_journal(&dir).unwrap().expect("a clean shutdown always leaves a Snapshot");
    assert_eq!(state.contingencies.len(), 3, "entry + held stop-loss + held take-profit");
    let entry = state.contingencies.iter().find(|c| c.active).expect("exactly one active entry");
    assert!(entry.held_request.is_none(), "the live entry is in the registry, not held");
    assert_eq!(entry.parent, None, "the entry has no parent");
    let held: Vec<_> = state.contingencies.iter().filter(|c| !c.active).collect();
    assert_eq!(held.len(), 2, "both protective exits are held");
    assert!(
        held.iter().all(|c| c.held_request.is_some()),
        "each held exit carries its own request"
    );
    assert!(
        held.iter().all(|c| c.parent.as_deref() == Some(entry.order_client_id.as_str())),
        "both held exits are linked under the entry as their OTO parent"
    );
}

/// The CRITICAL leak, at the crash-recovery boundary: a bracket whose ENTRY is DENIED by the
/// RiskGate (here via `Halted`) must orphan NOTHING — its held exits are dropped, so the exit `Snapshot`
/// carries an EMPTY contingency book and a restore round-trip restores nothing. Before the fix the
/// held exits were re-captured in every `Snapshot` and reappeared after every restart.
#[test]
fn denied_bracket_entry_orphans_nothing_into_the_snap() {
    let dir = unique_dir("restore-denied-bracket");
    {
        let engine = sim_engine();
        let config = CoreConfig {
            seed_cash: 10_000.0,
            clock: counter_clock(0),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(JournalConfig {
                dir: dir.to_path_buf(),
                file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
                snapshot_every: 64,
            }),
            ..CoreConfig::default()
        };
        let handle = spawn_core(engine, config);
        handle.send_command(Command::SetTradingState(TradingState::Halted));
        handle.send_command(Command::Order(OrderIntent::Bracket(Box::new(OrderBracket {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 2.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
        }))));
        handle.shutdown_and_join();
    }

    let state =
        restore_from_journal(&dir).unwrap().expect("a clean shutdown always leaves a Snapshot");
    assert!(
        state.contingencies.is_empty(),
        "a denied bracket entry leaves NO contingency legs in the Snapshot (no orphans)"
    );
}

/// One resting bracket leg built with an EXPLICIT order client id, so the test can inject fills for it by name
/// (a `spawn_core`-minted order client id would be unknowable). `linked` cross-links the two exits.
fn linked_leg(
    order_client_id: &str,
    side: i32,
    order_type: &str,
    price: Option<f64>,
    trigger: Option<f64>,
    parent: Option<&str>,
    linked: &[&str],
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty: 2.0,
        order_type: vike_model::OrderType::from_stored(order_type),
        price,
        trigger_price: trigger,
        reduce_only: parent.is_some(),
        parent_order_id: parent.map(str::to_string),
        contingency_type: Some(if parent.is_some() {
            vike_model::ContingencyType::Oco
        } else {
            vike_model::ContingencyType::Oto
        }),
        linked_order_ids: linked.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

/// The `[OrderAccepted, Fill, OrderFilled]` sequence a venue delivers for a full fill of `order_client_id`.
fn full_fill(order_client_id: &str, side: i32, qty: f64, px: f64) -> Vec<Event> {
    let f = FillEvent {
        // minted by this helper — same `t-<order client id>` bytes as the `format!` it replaced
        trade_id: TradeId::prefixed("t-", order_client_id),
        order_client_id: order_client_id.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "maker".into(),
        timestamp: 1,
        mark_price: Some(px),
        position_side: vike_model::events::PositionSide::Both,
    };
    vec![
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.into(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 1,
        }),
        Event::Fill(f.clone()),
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.into(),
            fill: f,
            timestamp: 1,
        }),
    ]
}

/// End-to-end journal-determinism gate for the live-runtime OCO/OTO feature (the one-test-per-
/// journal-feature convention): a FULL bracket lifecycle — entry fills → both exits released → the
/// stop fills → the take-profit OCO-canceled — journaled through `spawn_core`, then re-folded by
/// `replay_offline` and fenced against the source's final state. The bracket is submitted as
/// explicit-order-client-id linked orders so the fills can be injected by name. Every fenced effect (the
/// released orders in the registry, the canceled TP) is reproduced from the journaled events + the
/// re-seeded book, so the session replays bit-for-bit.
#[test]
fn a_full_bracket_lifecycle_replays_deterministically() {
    let dir = unique_dir("replay-bracket");
    {
        let engine = sim_engine();
        let config = CoreConfig {
            seed_cash: 10_000.0,
            clock: counter_clock(0),
            order_client_id_session: Some(("cafef00d".into(), 0)),
            journal: Some(JournalConfig {
                dir: dir.to_path_buf(),
                file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
                snapshot_every: 4,
            }),
            ..CoreConfig::default()
        };
        let handle = spawn_core(engine, config);
        let event = handle.event_sender();
        // 4 warmup fills → a cadence Snapshot (the replay BASE) BEFORE the bracket, so the whole bracket
        // lifecycle lives in the re-folded tail.
        for i in 0..4 {
            event.blocking_send(sim_bare_fill(&format!("w{i}"), 1.0, 100.0)).unwrap();
        }
        // the bracket, as explicit-order-client-id linked orders: entry live, sl/tp held.
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(linked_leg(
            "e",
            1,
            "market",
            None,
            None,
            None,
            &["sl", "tp"],
        )))));
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(linked_leg(
            "sl",
            -1,
            "stop",
            None,
            Some(95.0),
            Some("e"),
            &["tp"],
        )))));
        handle.send_command(Command::Order(OrderIntent::Submit(Box::new(linked_leg(
            "tp",
            -1,
            "limit",
            Some(110.0),
            None,
            Some("e"),
            &["sl"],
        )))));
        // entry fills → releases sl + tp
        for e in full_fill("e", 1, 2.0, 100.0) {
            event.blocking_send(e).unwrap();
        }
        // the stop fills → OCO-cancels tp (the core issues the cancel; the venue confirms it next)
        for e in full_fill("sl", -1, 2.0, 95.0) {
            event.blocking_send(e).unwrap();
        }
        event
            .blocking_send(Event::OrderCanceled(OrderCanceled {
                order_client_id: "tp".into(),
                reason: "oco".to_string().into(),
                timestamp: 1,
            }))
            .unwrap();
        handle.shutdown_and_join();
    }

    replay_offline(&dir)
        .expect("a full bracket fire→release→OCO-cancel session must replay deterministically");
    let _ = std::fs::remove_dir_all(&dir);
}
