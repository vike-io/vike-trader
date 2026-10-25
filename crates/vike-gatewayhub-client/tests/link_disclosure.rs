#![cfg(feature = "trade")]
//! **Link loss is DISCLOSED to the core, never acted on by the client (owner ruling D6).**
//!
//! When the lease is lost the strategies mounted on the account must see `FeedStatus::Disconnected` (they
//! pull their resting quotes), the dead-man's trade-link latch must open, and when the link comes back
//! `Live` closes it. A lease that is closed for good opens the latch and leaves it open: no `Live` is
//! coming. The client never cancels a resting order because a link dropped: they sit at the venue and
//! reconcile picks them up after recovery. The gatewayhub's restart is announced separately
//! (`LinkSignal::epoch_changed`), which is where the tradehub's re-teaching and immediate reconcile hang.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_exec::{ExecutionClient, Ingest};
use vike_gatewayhub_client::proto::{Response, TradeUp};
use vike_gatewayhub_client::trade::{
    CoreLinkSignal, LinkSignal, LinkState, MountedKeys, TradeLink,
};
use vike_model::FeedStatus;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{
    TradePeer, TradePeerConfig, account, fast_trade_timings, lane, open, open_with, order,
};

const WAIT: Duration = Duration::from_secs(5);
const STREAM: &str = vike_venues::link_deadman::TRADE_LINK_STREAM;

fn statuses(got: &[Ingest]) -> Vec<(String, String, String, FeedStatus)> {
    got.iter()
        .filter_map(|i| match i {
            Ingest::StreamStatus(s) => {
                Some((s.venue.clone(), s.symbol.clone(), s.stream.clone(), s.status))
            }
            _ => None,
        })
        .collect()
}

/// The `ExecLink` transitions in `got`, as `(route_key, venue, up)`.
fn exec_links(got: &[Ingest]) -> Vec<(String, String, bool)> {
    got.iter()
        .filter_map(|i| match i {
            Ingest::ExecLink(u) => Some((u.route_key.to_string(), u.venue.to_string(), u.up)),
            _ => None,
        })
        .collect()
}

fn is_link(i: &Ingest) -> bool {
    matches!(i, Ingest::StreamStatus(_) | Ingest::ExecLink(_))
}

#[test]
fn a_lost_link_discloses_disconnected_per_mounted_key_and_a_restored_one_discloses_live() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (events, mut core) = lane();
    let keys = MountedKeys::new();
    keys.add("binance", "BTCUSDT");
    keys.add("binance", "ETHUSDT");
    let signal = CoreLinkSignal::new(&events, &account("tradehub-1"), keys);
    let session = TradeLink::new(peer.link(), account("tradehub-1"), events)
        .with_timings(fast_trade_timings())
        .with_signal(Arc::new(signal))
        .open()
        .expect("opens");
    assert!(statuses(&core.drain()).is_empty(), "the first connection announces nothing");

    peer.kill_lease();
    let all_down = core.collect_until(WAIT, 3, is_link);
    assert_eq!(
        exec_links(&all_down),
        [("binance".into(), "binance".into(), false)],
        "the watchdog hears ONE ExecLink down for the account"
    );
    let down = statuses(&all_down);
    assert_eq!(
        down,
        [
            ("binance".into(), "BTCUSDT".into(), STREAM.into(), FeedStatus::Disconnected),
            ("binance".into(), "ETHUSDT".into(), STREAM.into(), FeedStatus::Disconnected),
        ],
        "one Disconnected per mounted key, on the reserved trade-link stream word"
    );

    assert!(peer.wait_leases(2, WAIT));
    let all_up = core.collect_until(WAIT, 3, is_link);
    assert_eq!(
        exec_links(&all_up),
        [("binance".into(), "binance".into(), true)],
        "and one ExecLink up once the link is back"
    );
    let up = statuses(&all_up);
    assert_eq!(up.len(), 2);
    assert!(up.iter().all(|s| s.2 == STREAM && s.3 == FeedStatus::Live), "{up:?}");
    assert_eq!(session.link_state(), LinkState::Up);
}

#[test]
fn a_link_loss_cancels_nothing_the_resting_orders_sit_at_the_venue() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _core) = fake_trade_peer::open(&peer, "tradehub-1");
    let mut exec = session.exec_client();
    exec.submit(&order("resting-1"));
    assert!(TradePeer::wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));

    peer.kill_lease();
    assert!(peer.wait_leases(2, WAIT));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    std::thread::sleep(Duration::from_millis(300));
    let cancels = peer
        .ups()
        .into_iter()
        .filter(|f| matches!(f, TradeUp::Cancel { .. } | TradeUp::CancelBatch { .. }))
        .count();
    assert_eq!(cancels, 0, "the client adds no auto-cancel on a trade-link loss (D6)");
}

#[derive(Default)]
struct Recorder {
    seen: Mutex<Vec<String>>,
}

impl LinkSignal for Recorder {
    fn link_lost(&self, why: &str) {
        self.seen.lock().expect("lock").push(format!("lost:{}", why.is_empty()));
    }

    fn link_restored(&self) {
        self.seen.lock().expect("lock").push("restored".into());
    }

    fn epoch_changed(&self, old: u64, new: u64) {
        self.seen.lock().expect("lock").push(format!("epoch:{}", new != old));
    }
}

#[test]
fn a_gatewayhub_restart_is_announced_as_an_epoch_change_after_the_link_is_back() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let recorder = Arc::new(Recorder::default());
    let r = Arc::clone(&recorder);
    let (session, _core) = open_with(&peer, "tradehub-1", move |link| link.with_signal(r));
    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    assert!(TradePeer::wait_until(WAIT, || recorder.seen.lock().expect("lock").len() >= 3));
    assert_eq!(
        *recorder.seen.lock().expect("lock"),
        ["lost:false", "restored", "epoch:true"],
        "lost, then restored, then the epoch change - each exactly once"
    );
}

#[test]
fn a_lease_closed_for_good_opens_the_latch_and_never_closes_it() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let recorder = Arc::new(Recorder::default());
    let r = Arc::clone(&recorder);
    let (session, _core) = open_with(&peer, "tradehub-1", move |link| link.with_signal(r));
    peer.refuse_open(Some(Response::LeaseHeld { holder_key_id: "nk-other".into(), since_ms: 1 }));
    peer.kill_lease();
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    std::thread::sleep(Duration::from_millis(300));
    let seen = recorder.seen.lock().expect("lock").clone();
    assert!(
        seen.iter().all(|s| s != "restored"),
        "no Live is coming from a closed lease: {seen:?}"
    );
    assert!(seen.iter().any(|s| s.starts_with("lost:")));
}

#[test]
fn a_holder_displaced_while_connected_opens_the_latch_too() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let recorder = Arc::new(Recorder::default());
    let r = Arc::clone(&recorder);
    let (old, _old_core) = open_with(&peer, "tradehub-1", move |link| link.with_signal(r));
    let (_new, _new_core) = open_with(&peer, "tradehub-2", |link| link.with_takeover(true));
    assert!(TradePeer::wait_until(WAIT, || old.link_state() == LinkState::Closed));
    assert!(
        TradePeer::wait_until(WAIT, || recorder
            .seen
            .lock()
            .expect("lock")
            .iter()
            .any(|s| s.starts_with("lost:"))),
        "the latch must open the moment the lease is taken, and no Live follows"
    );
    assert!(recorder.seen.lock().expect("lock").iter().all(|s| s != "restored"));
}

#[test]
fn the_takeover_flag_reaches_the_daemon_only_when_asked_for() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (_a, _la) = open(&peer, "tradehub-1");
    let (_b, _lb) = open_with(&peer, "tradehub-2", |link| link.with_takeover(true));
    let flags: Vec<bool> = peer.opens().iter().map(|o| o.3).collect();
    assert_eq!(flags, [false, true]);
}
