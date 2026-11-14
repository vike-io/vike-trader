//! The owner's ruling of 2026-11-14 (the tradehub does not wait for a build), the core's half: a mount whose
//! STRATEGY arrives later (`CoreConfig::mounts_awaiting_strategy`) is PARKED at assembly, never run on its
//! placeholder, and mounts when `AccountChange::AddMounts` brings its strategy; one whose account does not run
//! then waits for that account and mounts when its engine joins.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_core::testkit::{mount_of, test_config};
use vike_core::{LiveBroker, Mount, spawn_core_multi};
use vike_execution::testing::{RecordingClient, dyn_engine};
use vike_execution::{Engine, ExecutionClient};
use vike_model::Strategy;
use vike_model::accounts::account_keys::{AccountLabel, AccountNamed};

use crate::kit::handle::{CoreJoinGuard, wait_for_snapshot};

type EngineBoxed = Engine<Box<dyn ExecutionClient + Send>>;

/// Counts the times the core asked it which symbols it trades: as the mount is installed, never while it is
/// parked.
struct Started(Arc<AtomicUsize>);

impl Strategy<LiveBroker> for Started {
    fn symbols(&self) -> Vec<String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Vec::new()
    }
}

fn binance() -> EngineBoxed {
    dyn_engine("binance", "BTCUSDT", Box::new(RecordingClient::default()))
}

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a label")
}

/// The maker's mount, on `account`, with `strategy`.
fn maker(account: Option<AccountLabel>, strategy: Box<dyn Strategy<LiveBroker> + Send>) -> Mount {
    Mount { account, ..mount_of("binance", "BTCUSDT", "1m", strategy) }
}

const MAKER_ID: &str = "binance__BTCUSDT__1m";

fn mounted(snap: &vike_execution::CoreSnapshot, id: &str) -> bool {
    snap.mounts.iter().any(|m| m.mount_id == id)
}

#[test]
fn a_parked_mount_runs_nothing_until_its_strategy_arrives_and_then_mounts() {
    let placeholder = Arc::<AtomicUsize>::default();
    let real = Arc::<AtomicUsize>::default();
    let config = vike_core::CoreConfig {
        strategy: Some(maker(None, Box::new(Started(Arc::clone(&placeholder))))),
        mounts_awaiting_strategy: vec![MAKER_ID.to_string()],
        ..test_config(10_000.0)
    };
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), config));
    let cell = handle.snapshot_cell();
    // An idle core publishes on a change: an EMPTY parcel hands it nothing and makes it publish.
    handle.event_sender().add_account_mounts(Box::new(Vec::<Mount>::new())).unwrap();
    wait_for_snapshot(&cell, 10, "the core publishes", |s| s.sequence > 0);
    assert!(!mounted(&cell.load_full(), MAKER_ID), "a parked mount has no slot");

    handle
        .event_sender()
        .add_account_mounts(Box::new(vec![maker(None, Box::new(Started(Arc::clone(&real))))]))
        .unwrap();
    wait_for_snapshot(&cell, 10, "the mount runs once its strategy arrived", |s| {
        mounted(s, MAKER_ID)
    });
    handle.shutdown_and_join();
    assert_eq!(placeholder.load(Ordering::SeqCst), 0, "the placeholder was never asked anything");
    assert!(real.load(Ordering::SeqCst) >= 1, "the strategy that arrived was installed");
}

#[test]
fn a_built_mount_whose_account_does_not_run_waits_for_it_and_mounts_when_it_starts() {
    let config = vike_core::CoreConfig {
        strategy: Some(maker(Some(alt()), Box::new(Started(Arc::default())))),
        mounts_awaiting_strategy: vec![MAKER_ID.to_string()],
        not_running: vec![vike_execution::ArmingNotRunningView {
            account: AccountNamed::of("binance", alt()),
            tier: None,
            reason: "FX market closed".to_string(),
            block: Some(vike_model::ArmingBlock::FxMarketClosed),
        }],
        ..test_config(10_000.0)
    };
    let handle = CoreJoinGuard::new(spawn_core_multi(binance(), Vec::new(), config));
    let cell = handle.snapshot_cell();
    let real = Arc::<AtomicUsize>::default();
    // The parcel names no account: the PARKED mount's own (`ALT`) is the one it mounts on.
    handle
        .event_sender()
        .add_account_mounts(Box::new(vec![maker(None, Box::new(Started(Arc::clone(&real))))]))
        .unwrap();
    wait_for_snapshot(&cell, 10, "the mount says it waits for its account", |s| {
        s.recent_events.iter().any(|l| l.contains("waits for its account `binance#ALT`"))
    });
    assert!(!mounted(&cell.load_full(), MAKER_ID), "no slot while its account does not run");

    let mut engine = binance();
    engine.route_key = "binance#ALT".to_string();
    handle.event_sender().add_account_engine(500.0, engine).unwrap();
    wait_for_snapshot(&cell, 10, "the mount runs once its account started", |s| {
        mounted(s, MAKER_ID)
    });
    handle.shutdown_and_join();
    assert!(real.load(Ordering::SeqCst) >= 1, "it was installed, on the account's engine");
}
