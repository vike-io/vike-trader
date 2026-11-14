//! `Ingest::MountReply`, the door a SANDBOXED strategy's answer comes in by (decision 0273 item 3, plan 2b
//! F5): the intents are applied as the named mount's own orders, through the one drain, and a reply for a
//! mount that is gone, or a parcel of another type, applies nothing and says so.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::testkit::mount_of;
use vike_core::{CoreConfig, LiveBroker, MountReplyIntents, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, MountReply, RiskGate};
use vike_model::{OrderBroker, OrderPlacement, RiskLimits, Strategy};

/// A strategy that does nothing in the core: its orders come only through the door.
struct Silent;

impl Strategy<LiveBroker> for Silent {}

fn run(replies: Vec<MountReply>) -> Arc<vike_execution::CoreSnapshot> {
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 1_000_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(mount_of("binance", "BTCUSDT", "1m", Box::new(Silent))),
        ..CoreConfig::default()
    };
    let engine = Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    for reply in replies {
        handle.event_sender().mount_reply(reply).unwrap();
    }
    handle.shutdown_and_join();
    Arc::clone(&cell.load())
}

fn bid(mount_id: &str, applied: &Arc<std::sync::atomic::AtomicUsize>) -> MountReply {
    let applied = Arc::clone(applied);
    let apply = MountReplyIntents {
        write: Box::new(|b: &mut LiveBroker| {
            b.place("", "bid", 1, 2.0, OrderPlacement::Limit { price: 99.0 }, false);
        }),
        applied: Box::new(move || {
            applied.fetch_add(1, Ordering::SeqCst);
        }),
    };
    MountReply { mount_id: mount_id.into(), now: 5, apply: Box::new(apply) }
}

#[test]
fn a_reply_is_applied_as_the_mounts_own_order() {
    let applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let snap = run(vec![bid("binance__BTCUSDT__1m", &applied)]);
    let orders: Vec<_> = snap.orders.iter().map(|o| (o.symbol.as_str(), o.side, o.qty)).collect();
    assert_eq!(orders, vec![("BTCUSDT", 1, 2.0)], "the tagged bid reached the engine");
    assert_eq!(applied.load(Ordering::SeqCst), 1, "the sender is told the reply was applied");
}

#[test]
fn a_reply_for_no_live_mount_or_of_another_type_applies_nothing() {
    let wrong =
        MountReply { mount_id: "binance__BTCUSDT__1m".into(), now: 5, apply: Box::new(7_u8) };
    let applied = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let snap = run(vec![bid("binance__ETHUSDT__1m", &applied), wrong]);
    assert_eq!(
        applied.load(Ordering::SeqCst),
        0,
        "nothing was applied, so nothing is acknowledged"
    );
    assert!(snap.orders.is_empty(), "nothing was applied: {:?}", snap.orders);
    let said = |needle: &str| snap.recent_events.iter().any(|l| l.contains(needle));
    assert!(said("REPLY DROPPED: no live strategy mount with id `binance__ETHUSDT__1m`"));
    assert!(said("REPLY DROPPED: mount `binance__BTCUSDT__1m` was sent a parcel of another type"));
}
