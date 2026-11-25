//! **A journaled `Command::ReconcileReports` replays to the same books** — through the fenced
//! offline replay (`vike_core::replay::replay_offline`) and through the crash restore
//! (`vike_core::replay::restore_from_journal`) of a journal whose exit `Snapshot` was lost.
//!
//! Before this file nothing under `tests/journal/` or `src/replay/` built that command, so a
//! reconcile pass in a replayed TAIL was never exercised. It is write-ahead journaled like every
//! other exec-lane command (`crates/vike-core/src/runtime/dispatch.rs`'s `dispatch`), and the events
//! it synthesizes are folded straight through `publish_to` and are NOT journaled on their own — so a
//! replay must re-derive them by re-running the whole pass against the restored engine.
//!
//! The pass is chosen to FOLD something on every axis the state hash covers, and to make one fold
//! decision that depends on the core rather than on the payload:
//!
//! - a cash-reconcile first sync (`reconcile_balance: true` on a `Delta` account): `balance` and
//!   `balance_mode` move;
//! - three `MissingFill`s under `hybrid`: this instance's own fill and an untagged one fold into the
//!   position, and a fill tagged with ANOTHER instance's origin is held
//!   (`crates/vike-execution/src/recon/resolve.rs`'s `mode_applies_divergence`). That last decision is
//!   made relative to the identity `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_compute`
//!   stamps from the core's order client id GENERATOR, which a replay restores from the journal's own
//!   `order_client_id_session` — the property `ReconcilePolicy::local_instance_origin`'s doc claims makes the fold
//!   decision the same on any box. A replay that judged against anything else (configuration, a
//!   fresh session) would fold the foreign fill and land a different position.
//!
//! **What a replay cannot re-derive is checkpointed.** A divergence folds on its SECOND pass in a row (decision 0043
//! as amended 2026-11-21), and that memory, the streak, is in no `Snapshot`. The offline replay starts from the first
//! `Snapshot`, before both passes, and rebuilds it. A crash restore starts from the LATEST one, which can sit between
//! the two sightings: re-run from there, the second pass sees the divergence once and folds nothing. So a pass that
//! folds, and an operator's confirm, write a `Snapshot` at once, and the crash tests put the base between the
//! sightings.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, JournalConfig, Mount, spawn_core};
use vike_execution::recon::{ReconcileBalanceTolerance, ReconcilePolicy};
use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, Command, Engine, EngineSnapshot, Ingest, ReconcileReports, RiskGate,
    state_hash,
};
use vike_journal::{
    JournalFileConfig, JournalRecord, JournalSnapshotClosedTrades, JournalSnapshotMountAttribution,
};
use vike_model::RiskLimits;
use vike_model::{Clock, OrderClientIdOrigin};

use crate::kit::doubles::RestOnce;
use crate::kit::engines::sim_engine;
use crate::kit::events::{fill_report_echoing, sim_bare_fill, sim_quote};
use crate::kit::handle::{wait_for, wait_for_snapshot};
use crate::kit::journal::{copy_journal_files, records, snaps};
use crate::scratch::Scratch;

/// The resumed order client id session: `<origin>V<8-hex>`, i.e. a generator minting under `ap1`.
const SESSION: &str = "ap1Vcafef00d";
/// The six warm-up fills' total (each 1.0, all before the cadence `Snapshot` that is the replay BASE).
const WARMUP_QUANTITY: f64 = 6.0;
const OWN_QUANTITY: f64 = 0.5;
const UNTAGGED_QUANTITY: f64 = 0.25;
const FOREIGN_QUANTITY: f64 = 2.0;
/// The venue cash the pass reports, adopted as the first sync.
const VENUE_CASH: f64 = 5_000.0;

/// A journaled sim core resuming [`SESSION`] (and configured with the same origin, as a deployment would be), with a
/// cadence `Snapshot` every six journaled records.
fn spawn_journaled_core(dir: &Path) -> vike_core::CoreHandle {
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some((SESSION.into(), 0)),
        instance_origin: Some(OrderClientIdOrigin::parse("ap1").expect("a legal origin tag")),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 6,
        }),
        ..CoreConfig::default()
    };
    spawn_core(engine, config)
}

/// Run the source session on [`spawn_journaled_core`]: six bare warm-up fills (the sixth trips the cadence
/// `Snapshot` = the replay BASE), then ONE reconcile pass delivered twice (it folds on its second pass in a row), then
/// a clean shutdown (the exit `Snapshot`). Six, so the base precedes BOTH passes and the offline replay re-runs the
/// streak they build.
fn build_journal_with_a_reconcile_pass(dir: &Path) {
    let handle = spawn_journaled_core(dir);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3", "t4", "t5"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap();
    }
    // The pass twice: the first adopts the cash (a first sync is no divergence) and starts the fills'
    // streaks, the second folds them (two passes in a row: decision 0043 as amended 2026-11-21).
    let pass = ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![
            fill_report_echoing("sim", "r-own", "ap1Vcafef00d7", OWN_QUANTITY),
            fill_report_echoing("sim", "r-untagged", "cafef00d8", UNTAGGED_QUANTITY),
            fill_report_echoing("sim", "r-foreign", "bx2Vdeadbeef0", FOREIGN_QUANTITY),
        ],
        positions: Vec::new(),
        policy: ReconcilePolicy::hybrid(),
        balance: Some(VENUE_CASH),
        generate_missing_orders: false,
        reconcile_balance: true,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    };
    handle.send_command(Command::ReconcileReports(Box::new(pass.clone())));
    handle.send_command(Command::ReconcileReports(Box::new(pass)));
    handle.shutdown_and_join();
}

/// The BTCUSDT position size an engine snapshot carries, `0.0` when there is none.
fn btc_size(e: &EngineSnapshot) -> f64 {
    e.account.positions.iter().find(|(k, _)| k.1.as_str() == "BTCUSDT").map_or(0.0, |(_, p)| p.size)
}

fn is(x: f64, want: f64) -> bool {
    (x - want).abs() < 1e-9
}

/// THE FENCE: offline replay of a tail holding a reconcile pass reproduces the source's final
/// state hash, and the books under it — the position the pass folded (with the foreign fill HELD)
/// and the cash it adopted.
#[test]
fn a_journaled_reconcile_pass_replays_to_the_same_state_hash_and_books() {
    let dir = Scratch::reserved("recon-replay");
    build_journal_with_a_reconcile_pass(&dir);
    let recs = records(&dir);

    // NON-VACUOUS, part 1: the reconcile pass is a journaled record AFTER the replay base (the
    // first `Snapshot`), so `replay_offline` re-runs it rather than restoring past it.
    let first_snap = recs
        .iter()
        .position(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("the cadence Snapshot");
    let pass_at = recs
        .iter()
        .position(|r| {
            matches!(
                r,
                JournalRecord::Command {
                    message: Ingest::Command(Command::ReconcileReports(_)),
                    ..
                }
            )
        })
        .expect("the reconcile pass is journaled");
    assert!(
        pass_at > first_snap,
        "the pass must sit in the replayed TAIL, after the base Snapshot"
    );

    // NON-VACUOUS, part 2: the pass changed the fenced state, and changed it the way this test
    // claims — own + untagged folded, foreign held, cash adopted.
    let all = snaps(&recs);
    assert!(all.len() >= 2, "a cadence Snapshot and the exit Snapshot (got {})", all.len());
    let (base_hash, base_engines) = &all[0];
    let (final_hash, final_engines) = all.last().expect("the exit Snapshot");
    assert!(is(btc_size(&base_engines[0]), WARMUP_QUANTITY), "the base holds the warm-up only");
    assert_ne!(base_hash, final_hash, "the pass must change the fenced state");
    let src = &final_engines[0];
    assert!(
        is(btc_size(src), WARMUP_QUANTITY + OWN_QUANTITY + UNTAGGED_QUANTITY),
        "the source folded the own and untagged fills and HELD the foreign one; size {}",
        btc_size(src)
    );
    assert_eq!(src.account.balance_mode, BalanceMode::Authoritative);
    assert!(is(src.account.balance, VENUE_CASH), "the first sync adopted the venue's cash");

    // THE FENCE.
    let out = replay_offline(&dir).expect("a reconcile tail replays and passes the fence");
    assert_eq!(out.final_hash, *final_hash, "replay reproduces the source's final state hash");
    assert_eq!(
        out.order_client_id_session, SESSION,
        "the replay core resumed the journal's own session"
    );
    assert_eq!(out.engines.len(), 1);
    assert_eq!(
        out.engines[0].account.positions, src.account.positions,
        "...and the same position book"
    );
    assert!(is(out.engines[0].account.balance, VENUE_CASH));
    assert_eq!(out.engines[0].account.balance_mode, BalanceMode::Authoritative);
}

/// The venue order this install never sent, terminal with 1.0 filled at 100, that a pass under
/// `generate_missing_orders` adopts as `EXT-sim-web-1` (its fills fell outside the lookback: no fill row names it).
fn placed_on_the_venue() -> vike_model::OrderStatusReport {
    vike_model::OrderStatusReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "web-1".into(),
        order_client_id: None,
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 1.0,
        filled_qty: 1.0,
        average_price: 100.0,
        status: vike_model::OrderStatus::Filled,
        timestamp: 5,
        price: Some(100.0),
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

/// A venue fill naming no order, which a pass under `generate_missing_orders` adopts as `EXT-sim-web-2`.
fn unnamed_fill() -> vike_model::FillReport {
    vike_model::FillReport {
        order_client_id: None,
        ..fill_report_echoing("sim", "web-2", "", OWN_QUANTITY)
    }
}

/// The source session of a crash BETWEEN two sightings: FIVE warm-up fills, so `pass`'s first delivery is the sixth
/// journaled record and the cadence `Snapshot` (the crash restore's base) follows it; the second delivery, which
/// confirms what the first only saw (two passes in a row: decision 0043 as amended 2026-11-21); with `confirm`, the
/// operator's confirm of the row that second delivery held; then a clean stop (the exit `Snapshot`).
fn build_journal_with_the_base_between_two_sightings(
    dir: &Path,
    pass: &ReconcileReports,
    confirm: bool,
) {
    let handle = spawn_journaled_core(dir);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3", "t4"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap();
    }
    handle.send_command(Command::ReconcileReports(Box::new(pass.clone())));
    handle.send_command(Command::ReconcileReports(Box::new(pass.clone())));
    if confirm {
        let cell = handle.snapshot_cell();
        wait_for_snapshot(&cell, 5, "the second sighting holds the row", |s| {
            !s.reconcile.alerts.is_empty()
        });
        let id = cell.load_full().reconcile.alerts[0].id;
        handle.send_command(Command::ConfirmReconcile(id));
    }
    handle.shutdown_and_join();
}

/// PREMISE of a crash between two sightings, read off the intact journal: the pass is journaled twice and a cadence
/// `Snapshot` sits between the two deliveries. Returns that base's engines.
fn the_base_between_the_sightings(recs: &[JournalRecord]) -> &[EngineSnapshot] {
    let passes: Vec<usize> = recs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            matches!(
                r,
                JournalRecord::Command {
                    message: Ingest::Command(Command::ReconcileReports(_)),
                    ..
                }
            )
            .then_some(i)
        })
        .collect();
    assert_eq!(passes.len(), 2, "the pass, delivered twice");
    recs[passes[0]..passes[1]]
        .iter()
        .find_map(|r| match r {
            JournalRecord::Snapshot { engines, .. } => Some(engines.as_slice()),
            _ => None,
        })
        .expect("a cadence Snapshot between the two sightings")
}

/// A copy of `source` with its exit `Snapshot` torn off, the shape a crash after the session's last command leaves:
/// ONE byte cut off the end of the written data, so the final frame claims more bytes than the segment holds and the
/// read stops before it (the plant of `replay_fence.rs`'s torn-tail test).
fn torn_copy(source: &Path, name: &str) -> Scratch {
    let torn = Scratch::reserved(name);
    copy_journal_files(source, &torn);
    let seg = std::fs::read_dir(&*torn)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension().and_then(|s| s.to_str()) == Some("vjl")).then_some(p)
        })
        .next()
        .expect("the scenario writes one segment");
    let bytes = std::fs::read(&seg).unwrap();
    let last_nonzero = bytes.iter().rposition(|&b| b != 0).unwrap();
    std::fs::write(&seg, &bytes[..last_nonzero]).unwrap();
    torn
}

/// The crash restore of `torn` lands the live session's books exactly: the positions, the cash and the state hash of
/// `live`, the intact journal's exit `Snapshot`.
fn assert_the_restore_lands_the_live_books(torn: &Path, live: &(u64, Vec<EngineSnapshot>)) {
    let (live_hash, live_engines) = live;
    let restored = restore_from_journal(torn)
        .expect("restore ok")
        .expect("the torn journal still has a Snapshot");
    assert_eq!(restored.order_client_id_session, SESSION);
    assert_eq!(
        restored.engines[0].account.positions,
        live_engines[0].account.positions,
        "the live position book: restored size {}, live {}",
        btc_size(&restored.engines[0]),
        btc_size(&live_engines[0])
    );
    assert!(
        is(restored.engines[0].account.balance, live_engines[0].account.balance),
        "the live cash: restored {}, live {}",
        restored.engines[0].account.balance,
        live_engines[0].account.balance
    );
    assert_eq!(state_hash(&restored.engines), *live_hash, "the live session's exact final state");
}

/// THE CRASH RESTORE, its base `Snapshot` between a divergence's two sightings. Under `synthesize` with
/// `generate_missing_orders` and a cash first sync, the second pass adopts a venue order this install never sent and a
/// fill naming no order, and the session dies before the next checkpoint. The streak is in no `Snapshot`, so a restore
/// that re-ran that pass from the base would see both once and fold nothing, and every later journaled fill would land
/// on another book; the pass checkpoints what it folded (`crates/vike-core/src/runtime/reconcile.rs`'s
/// `reconcile_reports`), and the restore lands the live positions, cash and adoptions.
#[test]
fn a_crash_after_a_pass_that_folds_its_second_sighting_restores_what_it_folded() {
    let source = Scratch::reserved("recon-restore-between-src");
    let pass = ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: vec![placed_on_the_venue()],
        fills: vec![unnamed_fill()],
        positions: Vec::new(),
        policy: ReconcilePolicy::default(),
        balance: Some(VENUE_CASH),
        generate_missing_orders: true,
        reconcile_balance: true,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    };
    build_journal_with_the_base_between_two_sightings(&source, &pass, false);
    let recs = records(&source);
    let base = the_base_between_the_sightings(&recs);
    let all = snaps(&recs);
    let live = all.last().expect("the exit Snapshot");
    // NON-VACUOUS: the second sighting folded both adoptions onto the book the base holds.
    assert!(is(btc_size(&base[0]), 5.0), "the base holds the warm-up only: {}", btc_size(&base[0]));
    assert!(
        is(btc_size(&live.1[0]), 5.0 + 1.0 + OWN_QUANTITY),
        "the live book holds both adoptions; size {}",
        btc_size(&live.1[0])
    );
    assert!(is(live.1[0].account.balance, VENUE_CASH), "the first sync took the venue's cash");

    let torn = torn_copy(&source, "recon-restore-between-torn");
    assert_the_restore_lands_the_live_books(&torn, live);
}

/// ...and the same crash after an operator's CONFIRM under `quarantine`: the second pass holds a missed fill, the
/// operator confirms it, and the session dies. A restore that re-ran the second pass from the base would hold nothing
/// and replay the journaled confirm as an unknown id; the confirm checkpoints what it folded
/// (`crates/vike-core/src/runtime/reconcile.rs`'s `confirm_reconcile`).
#[test]
fn a_crash_after_a_confirm_of_a_second_sighting_restores_what_it_folded() {
    let source = Scratch::reserved("recon-restore-confirm-src");
    let pass = ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![fill_report_echoing("sim", "r-missed", "ap1Vcafef00d7", OWN_QUANTITY)],
        positions: Vec::new(),
        policy: ReconcilePolicy::of(vike_model::ReconcilePolicyName::Quarantine),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    };
    build_journal_with_the_base_between_two_sightings(&source, &pass, true);
    let recs = records(&source);
    let base = the_base_between_the_sightings(&recs);
    assert!(
        recs.iter().any(|r| matches!(
            r,
            JournalRecord::Command { message: Ingest::Command(Command::ConfirmReconcile(_)), .. }
        )),
        "the confirm is journaled"
    );
    let all = snaps(&recs);
    let live = all.last().expect("the exit Snapshot");
    assert!(is(btc_size(&base[0]), 5.0), "the base holds the warm-up only: {}", btc_size(&base[0]));
    assert!(
        is(btc_size(&live.1[0]), 5.0 + OWN_QUANTITY),
        "the confirm booked the missed fill; size {}",
        btc_size(&live.1[0])
    );

    let torn = torn_copy(&source, "recon-restore-confirm-torn");
    assert_the_restore_lands_the_live_books(&torn, live);
}

/// The mount ledgers and closed-trade ledgers a `Snapshot` record carries.
fn mount_books(
    rec: &JournalRecord,
) -> (&[JournalSnapshotMountAttribution], &[JournalSnapshotClosedTrades]) {
    match rec {
        JournalRecord::Snapshot { mount_attribution, closed_trades, .. } => {
            (mount_attribution.as_slice(), closed_trades.as_slice())
        }
        other => panic!("not a Snapshot: {other:?}"),
    }
}

/// ...and the same crash after a confirm of a held fill of a MOUNT's order: the mount hears the fill, and the
/// checkpoint the confirm writes holds what it heard (`crates/vike-core/src/runtime/publish.rs`'s
/// `write_snap_after_delivery`). Written before the delivery, it held the fill in the account and not in the mount's
/// ledger or closed trades, and the confirm that caused it is before it, so a restore from it never delivered it.
#[test]
fn a_crash_after_a_confirm_of_a_mounts_held_fill_restores_the_mounts_ledger() {
    let source = Scratch::reserved("recon-restore-mount-src");
    let done = Arc::<AtomicBool>::default();
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        order_client_id_session: Some((SESSION.into(), 0)),
        // ONE resting order the mount mints on its first quote (`RecordingClient` never fills it).
        strategy: Some(Mount {
            order_client_id_code: None,
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy: Box::new(RestOnce { done: Arc::clone(&done) }),
        }),
        // No cadence `Snapshot`: the confirm's and the exit's are the only two.
        journal: Some(JournalConfig {
            dir: source.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: u64::MAX,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(sim_engine(), config);
    handle.tick_sender().quote(sim_quote(100.0, 1)).unwrap();
    let rested = wait_for(&handle, "the mount's order rests", |s| !s.orders.is_empty());
    let order_client_id = rested.orders[0].order_client_id.clone();
    let pass = ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![fill_report_echoing("sim", "r-mount", &order_client_id, 1.0)],
        positions: Vec::new(),
        policy: ReconcilePolicy::of(vike_model::ReconcilePolicyName::Quarantine),
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    };
    handle.send_command(Command::ReconcileReports(Box::new(pass.clone())));
    handle.send_command(Command::ReconcileReports(Box::new(pass)));
    let held = wait_for(&handle, "the second sighting holds the missed fill", |s| {
        s.reconcile
            .alerts
            .iter()
            .any(|a| a.kind == vike_model::ReconcileDivergenceKind::MissingFill)
    });
    let id = held
        .reconcile
        .alerts
        .iter()
        .find(|a| a.kind == vike_model::ReconcileDivergenceKind::MissingFill)
        .expect("held")
        .id;
    handle.send_command(Command::ConfirmReconcile(id));
    handle.shutdown_and_join();

    let recs = records(&source);
    let at: Vec<usize> = recs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, JournalRecord::Snapshot { .. }).then_some(i))
        .collect();
    assert_eq!(at.len(), 2, "the confirm's Snapshot and the exit's");
    let (live_ledgers, live_closed) = mount_books(&recs[at[1]]);
    // NON-VACUOUS: the mount heard the confirmed fill.
    let own = live_ledgers.iter().find(|r| r.venue.is_none()).expect("the mount's own row");
    assert!(is(own.size, 1.0), "the mount's ledger holds the confirmed fill: {own:?}");
    let torn = torn_copy(&source, "recon-restore-mount-torn");
    let restored = restore_from_journal(&torn)
        .expect("restore ok")
        .expect("the torn journal still has the confirm's Snapshot");
    assert_eq!(
        restored.mount_attribution, live_ledgers,
        "the restore holds the mount's live ledger"
    );
    let (_, kept_closed) = mount_books(&recs[at[0]]);
    assert_eq!(
        kept_closed, live_closed,
        "the confirm's checkpoint holds the mount's closed trades"
    );
}

/// **What a reconcile pass folds is reported, on the account whose pass found it** (C.1, C.1d): each fill the pass
/// folds is reported in the fold of the `ReconcileReports` record. None of its ids was minted by this session (it
/// minted nothing), so every one is FOREIGN, with the pass's account as its provenance, no mount and no strategy.
#[test]
fn a_reconcile_pass_reports_what_it_folds_on_the_passs_account() {
    let dir = Scratch::reserved("recon-reports");
    build_journal_with_a_reconcile_pass(&dir);
    let recs = records(&dir);
    // The pass's SECOND delivery folds (two passes in a row), so its reports name that record.
    let pass = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Command {
                sequence,
                message: Ingest::Command(Command::ReconcileReports(_)),
                ..
            } => Some(*sequence),
            _ => None,
        })
        .next_back()
        .expect("the pass is journaled");
    let reports: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ExecutionReport { report, .. } => Some(report),
            _ => None,
        })
        .collect();
    let folded: Vec<&str> = reports.iter().map(|r| r.order_client_id.as_str()).collect();
    assert!(folded.contains(&"ap1Vcafef00d7") && folded.contains(&"cafef00d8"), "{folded:?}");
    for r in &reports {
        assert_eq!(r.caused_by_sequence, pass, "the pass's fold: {r:?}");
        assert_eq!(
            (r.ownership, r.account.as_deref(), r.mount_id.as_deref(), r.strategy_code),
            (vike_model::orders::ExecutionOwnership::Foreign, Some("DEFAULT"), None, None),
            "{r:?}"
        );
        assert!(r.fill.is_some(), "a reconcile fill: {r:?}");
    }
}

/// **An order the reconciler ADOPTS reads FOREIGN** (C.1d; the owner's rule: a foreign order is never touched, and its
/// fills count in the account's results, marked, never in a strategy's). A venue order this install never sent is
/// folded under a reserved `EXT-…` id both ways a pass adopts one: an `UnknownOrder`'s cumulative fill and a
/// `MissingFill` naming no order. Its accept is dropped (no engine registry ever holds an `EXT-…` id) and no session
/// minted it, so each report is FOREIGN, on the pass's account, with no mount, no strategy and no order state.
#[test]
fn a_reconcile_adopted_external_order_reads_foreign() {
    let dir = Scratch::reserved("recon-ext-foreign");
    let engine = Engine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let config = CoreConfig {
        seed_cash: 10_000.0,
        order_client_id_session: Some((SESSION.into(), 0)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 1_000,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let pass = ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: vec![placed_on_the_venue()],
        fills: vec![unnamed_fill()],
        positions: Vec::new(),
        policy: ReconcilePolicy::default(),
        balance: None,
        generate_missing_orders: true,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    };
    // Twice: the adoptions fold on their second pass in a row (decision 0043 as amended 2026-11-21).
    handle.send_command(Command::ReconcileReports(Box::new(pass.clone())));
    handle.send_command(Command::ReconcileReports(Box::new(pass)));
    handle.shutdown_and_join();
    let reports: Vec<_> = records(&dir)
        .into_iter()
        .filter_map(|r| match r {
            JournalRecord::ExecutionReport { report, .. } => Some(report),
            _ => None,
        })
        .collect();
    let adopted: Vec<&str> = reports.iter().map(|r| r.order_client_id.as_str()).collect();
    assert!(
        adopted.contains(&"EXT-sim-web-1") && adopted.contains(&"EXT-sim-web-2"),
        "both adoptions are reported: {adopted:?}"
    );
    for r in &reports {
        assert_eq!(
            (r.ownership, r.account.as_deref(), r.mount_id.as_deref(), r.strategy_code),
            (vike_model::orders::ExecutionOwnership::Foreign, Some("DEFAULT"), None, None),
            "{r:?}"
        );
        assert!(r.fill.is_some() && r.order.is_none(), "a fill, and no registry holds it: {r:?}");
    }
}
