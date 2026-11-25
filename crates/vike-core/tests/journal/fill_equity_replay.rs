//! **The balance a pass takes for a fill the book dropped replays to the same books.** The adoption is the
//! `Event::AccountState` a journaled `Command::ReconcileReports` folds, and what grants it, the dropped fill, is a journaled
//! event, so a replay re-derives it. The grant is in no `Snapshot` and needs not be: a replay's first pass after its base `Snapshot`
//! has no cash anchor (`Account::restore`) and takes the venue's figure anyway, which is what the live run's grant wrote.
//! The anchor equity reads is in no `Snapshot` either, and no hashed field holds equity: the fence compares the cash.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, JournalConfig, spawn_core};
use vike_execution::recon::{ReconcileBalanceTolerance, ReconcilePolicy};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Command, Engine, ReconcileReports, RiskGate};
use vike_journal::JournalFileConfig;
use vike_model::Clock;
use vike_model::RiskLimits;
use vike_model::events::Event;

use crate::kit::events::sim_bare_fill;
use crate::kit::journal::{records, snaps};
use crate::scratch::Scratch;

/// Declared wallet cash (`vike_venues::balance_is_wallet_cash`), so the book's open position does not hold the grant.
const VENUE: &str = "bybit";
/// The venue's linear perpetual (decision 0290's user name; the bare `BTCUSDT` is the spot pair).
const SYMBOL: &str = "BTCUSDT.P";
const FIRST_SYNC: f64 = 5_000.0;
const AFTER_THE_DROPPED_FILL: f64 = 4_700.0;

fn pass(cash: f64) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: VENUE.into(),
        since: 0,
        orders: Vec::new(),
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconcilePolicy::of(vike_model::ReconcilePolicyName::Quarantine),
        balance: Some(cash),
        generate_missing_orders: false,
        reconcile_balance: true,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
        admitted_symbols: None,
        lists_trigger_orders: false,
    }))
}

fn fill(tid: &str, symbol: &str) -> Event {
    let Event::Fill(mut fill) = sim_bare_fill(tid, 1.0, 100.0) else {
        unreachable!("sim_bare_fill builds a fill")
    };
    fill.venue = VENUE.into();
    fill.symbol = symbol.into();
    Event::Fill(fill)
}

/// Four fills of the mounted symbol (the fourth trips the cadence `Snapshot` = the replay BASE), the first sync, a fill on a
/// symbol the engine does not accept, the pass that takes the venue's lower figure, then a clean shutdown.
fn build_journal(dir: &Path) {
    let engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    let t = Arc::new(AtomicI64::new(0));
    let clock: Box<dyn Clock + Send> = Box::new(move || t.fetch_add(1, Ordering::Relaxed));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock,
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 8 },
            snapshot_every: 4,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let sender = handle.event_sender();
    for tid in ["t0", "t1", "t2", "t3"] {
        sender.blocking_send(fill(tid, SYMBOL)).unwrap();
    }
    handle.send_command(pass(FIRST_SYNC));
    sender.blocking_send(fill("hand-1", "ETHUSDT")).unwrap();
    handle.send_command(pass(AFTER_THE_DROPPED_FILL));
    handle.shutdown_and_join();
}

#[test]
fn the_balance_taken_for_a_dropped_fill_replays_to_the_same_state_hash_and_cash() {
    let dir = Scratch::reserved("fill_equity_replay");
    build_journal(&dir);

    let all = snaps(&records(&dir));
    let (final_hash, final_engines) = all.last().expect("the exit Snapshot");
    let src = &final_engines[0].account;
    assert_eq!(src.balance, AFTER_THE_DROPPED_FILL, "the live run took the venue's figure");

    let out = replay_offline(&dir).expect("the tail replays and passes the fence");
    assert_eq!(out.final_hash, *final_hash, "replay reproduces the source's final state hash");
    assert_eq!(out.engines[0].account.balance, AFTER_THE_DROPPED_FILL);
}
