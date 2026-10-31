//! **A deposit the pass adopted under `quarantine` replays to the same books** (decision record 0179,
//! Option B). The adoption is not a record of its own: it is the `Event::AccountState` a
//! `Command::ReconcileReports` pass folds through `publish_to`, and that command is write-ahead
//! journaled WITH its policy, so a replay re-runs the pass and re-derives the adoption from the
//! policy the original run had (`ReconPolicy::adopt_balance_increase` rides the payload). Journal
//! VERSION stays 21: the flag is a serde-default key, absent from every flag-false policy.
//!
//! The pass sequence is the live one: a first sync (the cash enters the book), then a deposit
//! beyond the tolerance seen on TWO consecutive passes (the first only holds it, the second adopts
//! it: the amendment of 2026-10-31), with a FLAT book, and no operator and no confirm command
//! anywhere in the journal. The fence is `replay_offline`'s: the reproduced final state hash must
//! equal the source's last `Snap`.
//!
//! **Why the two new conditions replay without a new journaled frame.** The "seen twice" memory is
//! the held `BalanceDrift` row in `CoreThread::recon_alerts`, and the flat-book test reads the
//! engine. `recon_alerts` is in NO `Snap` and in NO `state_hash` (it is a `CoreThread` field,
//! `replay/boot.rs`'s "What it does not restore" says so, and `vike_exec::state_hash` hashes
//! `Vec<EngineSnapshot>`), so the fence never compares it; it compares the cash the decision
//! produced. `replay_offline` starts from the FIRST `Snap` with an empty store and re-folds every
//! later record in order, so the store is rebuilt from the same journaled passes, and the engine's
//! positions and registry are hashed state the base `Snap` carries. The one pass that has no
//! earlier sighting in the replayed tail is the FIRST one, and `Account::restore` leaves the cash
//! baseline `None` (`realized_pnl_at_balance_sync` is not in a `Snap`), so the replay takes it as
//! a first sync: the venue's figure whole, which is what the live run's adoption wrote as well.
//! The three tests pin the decision both ways (adopted; held for a single sighting; held beside an
//! open position) as the same hash on both sides.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_exec::recon::{BalanceTol, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, Command, EngineSnapshot, ExecutionEngine, Ingest, ReconcileReports,
    RiskGate,
};
use vike_journal::{JournalFileConfig, JournalRecord};
use vike_model::Clock;
use vike_model::RiskLimits;
use vike_model::events::Event;

use crate::kit::events::sim_bare_fill;
use crate::kit::journal::records;
use crate::scratch::Scratch;

/// The venue cash the first sync takes.
const FIRST_SYNC: f64 = 5_000.0;
/// The cash after the deposit: +300, far past the default band (1.0 or one basis point).
const DEPOSITED: f64 = 5_300.0;

/// One balance-only pass under the NAMED `quarantine` policy, cash reconcile ON.
fn pass(cash: f64) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconPolicy::from_policy_name("quarantine").expect("a nameable policy"),
        balance: Some(cash),
        generate_missing_orders: false,
        reconcile_balance: true,
        balance_tol: BalanceTol::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

/// The warm-up's closing fill: the sell twin of `sim_bare_fill`.
fn sim_sell_fill(tid: &str, qty: f64, px: f64) -> Event {
    let Event::Fill(mut fill) = sim_bare_fill(tid, qty, px) else {
        unreachable!("sim_bare_fill builds a fill")
    };
    fill.side = -1;
    Event::Fill(fill)
}

/// Four warm-up fills (the fourth trips the cadence `Snap` = the replay BASE) that leave the book
/// FLAT (`flat`) or holding 4 BTC, then the first sync, then the deposit seen `sightings` times,
/// then a clean shutdown (the exit `Snap`).
fn build_journal(dir: &Path, flat: bool, sightings: usize) {
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
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, cfg);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2"] {
        sender.blocking_send(sim_bare_fill(tid, 1.0, 100.0)).unwrap();
    }
    let fourth =
        if flat { sim_sell_fill("t3", 3.0, 100.0) } else { sim_bare_fill("t3", 1.0, 100.0) };
    sender.blocking_send(fourth).unwrap();
    handle.send_command(pass(FIRST_SYNC));
    for _ in 0..sightings {
        handle.send_command(pass(DEPOSITED));
    }
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

fn is(x: f64, want: f64) -> bool {
    (x - want).abs() < 1e-9
}

/// The journal's reconcile passes: their record indexes, and the proof that each was written AFTER
/// the base `Snap`, WITH the adopting policy, and that no confirm command exists anywhere (so
/// whatever the books say, the passes alone decided it).
fn assert_passes_are_journaled(recs: &[JournalRecord], want: usize) {
    let base = recs
        .iter()
        .position(|r| matches!(r, JournalRecord::Snap { .. }))
        .expect("the cadence Snap");
    let passes: Vec<usize> = recs
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            matches!(
                r,
                JournalRecord::Cmd { msg: Ingest::Command(Command::ReconcileReports(_)), .. }
            )
            .then_some(i)
        })
        .collect();
    assert_eq!(passes.len(), want, "the first sync and every sighting are journaled");
    assert!(passes.iter().all(|&i| i > base), "all sit in the replayed TAIL, after the base Snap");
    for &i in &passes {
        let JournalRecord::Cmd { msg: Ingest::Command(Command::ReconcileReports(r)), .. } =
            &recs[i]
        else {
            unreachable!("filtered above")
        };
        let policy = serde_json::to_string(&r.policy).expect("the policy serializes");
        assert!(
            policy.contains("\"adopt_balance_increase\":true"),
            "the journaled pass carries the policy that adopts: {policy}"
        );
    }
    assert!(
        !recs.iter().any(|r| matches!(
            r,
            JournalRecord::Cmd { msg: Ingest::Command(Command::ConfirmRecon(_)), .. }
        )),
        "no confirm command: the passes alone decided"
    );
}

/// Build the journal, check its shape, replay it and return `(source balance, replayed balance)`
/// after asserting the fence: the replay reproduces the source's final state hash.
fn replayed_cash(name: &str, flat: bool, sightings: usize) -> (f64, f64) {
    let dir = Scratch::reserved(name);
    build_journal(&dir, flat, sightings);
    let recs = records(&dir);
    assert_passes_are_journaled(&recs, 1 + sightings);

    let all = snaps(&recs);
    let (final_hash, final_engines) = all.last().expect("the exit Snap");
    let src = &final_engines[0].account;
    assert_eq!(src.balance_mode, BalanceMode::Authoritative);

    let out = replay_offline(&dir).expect("the tail replays and passes the fence");
    assert_eq!(out.final_hash, *final_hash, "replay reproduces the source's final state hash");
    (src.balance, out.engines[0].account.balance)
}

#[test]
fn an_adopted_deposit_replays_to_the_same_state_hash_and_cash() {
    // A flat book, the deposit seen on two consecutive passes: the second adopts it.
    let (src, replayed) = replayed_cash("recon-deposit-replay", true, 2);
    assert!(is(src, DEPOSITED), "the pass adopted the deposit; balance {src}");
    assert!(is(replayed, DEPOSITED), "...and the replay reproduced it; balance {replayed}");
}

#[test]
fn a_deposit_seen_only_once_is_held_and_replays_as_held() {
    // The first sighting only holds it: the books stay at the first sync, on both sides.
    let (src, replayed) = replayed_cash("recon-deposit-once-replay", true, 1);
    assert!(is(src, FIRST_SYNC), "one sighting adopts nothing; balance {src}");
    assert!(is(replayed, FIRST_SYNC), "...and the replay holds it too; balance {replayed}");
}

#[test]
fn a_deposit_beside_an_open_position_is_held_and_replays_as_held() {
    // The book still holds 4 BTC (no position row is reported, so no divergence is raised): the
    // same figure on two passes is NOT adopted, and the replay reaches the same hash.
    let (src, replayed) = replayed_cash("recon-deposit-open-book-replay", false, 2);
    assert!(is(src, FIRST_SYNC), "an open position holds the deposit; balance {src}");
    assert!(is(replayed, FIRST_SYNC), "...and the replay holds it too; balance {replayed}");
}
