//! **A journaled `Command::ReconcileReports` replays to the same books** — through the fenced
//! offline replay (`vike_core::replay::replay_offline`) and through the crash restore
//! (`vike_core::replay::restore_from_journal`) of a journal whose exit `Snap` was lost.
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
//!   (`crates/vike-exec/src/recon/resolve.rs`'s `mode_applies_divergence`). That last decision is
//!   made relative to the identity `crates/vike-core/src/runtime/reconcile.rs`'s `reconcile_compute`
//!   stamps from the core's coid GENERATOR, which a replay restores from the journal's own
//!   `coid_session` — the property `ReconPolicy::local_instance_origin`'s doc claims makes the fold
//!   decision the same on any box. A replay that judged against anything else (configuration, a
//!   fresh session) would fold the foreign fill and land a different position.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::{replay_offline, restore_from_journal};
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::recon::{BalanceTol, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, EngineSnapshot, ExecutionEngine, Ingest, ReconcileReports,
    RiskGate, RiskLimits, state_hash,
};
use vike_journal::{JournalFileConfig, JournalRecord};
use vike_model::events::LiquiditySide;
use vike_model::{Clock, FillReport, InstanceOrigin};

use crate::kit::events::sim_bare_fill;
use crate::kit::journal::{copy_journal_files, records};
use crate::scratch::Scratch;

/// The resumed coid session: `<origin>V<8-hex>`, i.e. a generator minting under `ap1`.
const SESSION: &str = "ap1Vcafef00d";
/// The four warm-up fills' total (each 1.0, all before the cadence `Snap` that is the replay BASE).
const WARMUP_QTY: f64 = 4.0;
const OWN_QTY: f64 = 0.5;
const UNTAGGED_QTY: f64 = 0.25;
const FOREIGN_QTY: f64 = 2.0;
/// The venue cash the pass reports, adopted as the first sync.
const VENUE_CASH: f64 = 5_000.0;

/// A sim/BTCUSDT venue fill local state has never folded, echoing `coid`. Built by hand: the coid
/// is the subject.
fn fill_echoing(trade_id: &'static str, coid: &str, qty: f64) -> FillReport {
    FillReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        trade_id: trade_id.into(),
        venue_order_id: trade_id.into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        last_qty: qty,
        last_px: 100.0,
        commission: 0.0,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        ts: 5,
    }
}

/// Run the source session: a journaled sim core resuming [`SESSION`] (and configured with the same
/// origin, as a deployment would be), four bare warm-up fills (the fourth trips the cadence `Snap`
/// = the replay BASE), then ONE reconcile pass, then a clean shutdown (the exit `Snap`).
fn build_journal_with_a_reconcile_pass(dir: &Path) {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        coid_session: Some((SESSION.into(), 0)),
        instance_origin: Some(InstanceOrigin::parse("ap1").expect("a legal origin tag")),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap();
    }
    handle.send_command(Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: vec![
            fill_echoing("r-own", "ap1Vcafef00d7", OWN_QTY),
            fill_echoing("r-untagged", "cafef00d8", UNTAGGED_QTY),
            fill_echoing("r-foreign", "bx2Vdeadbeef0", FOREIGN_QTY),
        ],
        positions: Vec::new(),
        policy: ReconPolicy::hybrid(),
        balance: Some(VENUE_CASH),
        generate_missing_orders: false,
        reconcile_balance: true,
        balance_tol: BalanceTol::default(),
        route_key: None,
    })));
    handle.shutdown_and_join();
}

/// Every `Snap` of `recs`, in order, as `(hash, engines)`.
fn snaps(recs: &[JournalRecord]) -> Vec<(u64, Vec<EngineSnapshot>)> {
    recs.iter()
        .filter_map(|r| match r {
            JournalRecord::Snap { hash, engines, .. } => Some((*hash, engines.clone())),
            _ => None,
        })
        .collect()
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
    // first `Snap`), so `replay_offline` re-runs it rather than restoring past it.
    let first_snap = recs
        .iter()
        .position(|r| matches!(r, JournalRecord::Snap { .. }))
        .expect("the cadence Snap");
    let pass_at = recs
        .iter()
        .position(|r| {
            matches!(
                r,
                JournalRecord::Cmd { msg: Ingest::Command(Command::ReconcileReports(_)), .. }
            )
        })
        .expect("the reconcile pass is journaled");
    assert!(pass_at > first_snap, "the pass must sit in the replayed TAIL, after the base Snap");

    // NON-VACUOUS, part 2: the pass changed the fenced state, and changed it the way this test
    // claims — own + untagged folded, foreign held, cash adopted.
    let all = snaps(&recs);
    assert!(all.len() >= 2, "a cadence Snap and the exit Snap (got {})", all.len());
    let (base_hash, base_engines) = &all[0];
    let (final_hash, final_engines) = all.last().expect("the exit Snap");
    assert!(is(btc_size(&base_engines[0]), WARMUP_QTY), "the base holds the warm-up only");
    assert_ne!(base_hash, final_hash, "the pass must change the fenced state");
    let src = &final_engines[0];
    assert!(
        is(btc_size(src), WARMUP_QTY + OWN_QTY + UNTAGGED_QTY),
        "the source folded the own and untagged fills and HELD the foreign one; size {}",
        btc_size(src)
    );
    assert_eq!(src.account.balance_mode, BalanceMode::Authoritative);
    assert!(is(src.account.balance, VENUE_CASH), "the first sync adopted the venue's cash");

    // THE FENCE.
    let out = replay_offline(&dir).expect("a reconcile tail replays and passes the fence");
    assert_eq!(out.final_hash, *final_hash, "replay reproduces the source's final state hash");
    assert_eq!(out.coid_session, SESSION, "the replay core resumed the journal's own session");
    assert_eq!(out.engines.len(), 1);
    assert_eq!(
        out.engines[0].account.positions, src.account.positions,
        "...and the same position book"
    );
    assert!(is(out.engines[0].account.balance, VENUE_CASH));
    assert_eq!(out.engines[0].account.balance_mode, BalanceMode::Authoritative);
}

/// THE CRASH RESTORE: the same journal with its exit `Snap` torn off — the shape a crash after the
/// pass leaves — restores from the cadence `Snap` and re-folds the reconcile pass in the crash tail,
/// landing the books the live session had.
#[test]
fn a_crash_restore_refolds_a_reconcile_pass_in_its_tail() {
    let src_dir = Scratch::reserved("recon-restore-src");
    build_journal_with_a_reconcile_pass(&src_dir);
    let all = snaps(&records(&src_dir));
    let (live_hash, live_engines) = all.last().expect("the exit Snap");

    let torn = Scratch::reserved("recon-restore-torn");
    copy_journal_files(&src_dir, &torn);
    // Cut ONE byte off the end of the written data, so the final frame (the exit `Snap`) claims more
    // bytes than the segment holds and the read stops before it — the same plant as
    // `replay_fence.rs`'s torn-tail test.
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

    // PREMISE: what survived is the cadence `Snap` followed by the reconcile pass, and nothing after.
    let kept = records(&torn);
    let last_snap = kept
        .iter()
        .rposition(|r| matches!(r, JournalRecord::Snap { .. }))
        .expect("the cadence Snap survives");
    assert!(
        matches!(
            &kept[last_snap + 1..],
            [JournalRecord::Cmd { msg: Ingest::Command(Command::ReconcileReports(_)), .. }]
        ),
        "the crash tail is exactly the reconcile pass"
    );

    let restored = restore_from_journal(&torn)
        .expect("restore ok")
        .expect("the torn journal still has a Snap");
    assert_eq!(restored.coid_session, SESSION);
    assert_eq!(
        state_hash(&restored.engines),
        *live_hash,
        "the restore re-folds the pass to the live session's exact final state"
    );
    assert_eq!(restored.engines[0].account.positions, live_engines[0].account.positions);
    assert!(
        is(btc_size(&restored.engines[0]), WARMUP_QTY + OWN_QTY + UNTAGGED_QTY),
        "the foreign fill stays HELD after a restore; size {}",
        btc_size(&restored.engines[0])
    );
}
