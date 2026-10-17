//! **Kill proofs for the hop** (wave 2, W19): each test PLANTS the failure the design says the system
//! survives, against the REAL pieces on both ends (the tradehub's own `RemoteBackend`, a real core, a real
//! gatewayhub server over a scripted venue that outlives it, a proxy a test can cut), and reads the verdict off
//! what the core and the venue actually hold, not off what a component says about itself.
//!
//! | proof | what is planted | R |
//! |---|---|---|
//! | `a_tradehub_holding_only_the_desktops_observe_key_cannot_open_a_strategy_lane` | the strategy-lane dial signs with the observe (or control) key | R25 |
//! | `a_gatewayhub_without_the_trade_class_is_refused_by_feature_name_before_a_mac_is_signed` | an old gatewayhub | 0112, R23 |
//! | `a_gatewayhub_restart_...` | the daemon restarts under a live lease with orders resting | R7, R10 |
//! | `the_trade_link_dropped_...` (4 tests) | the link dies with a submit queued, written-unread, written-unacked-but-read, written-across-an-epoch | R5, B.7 |
//! | `a_trade_link_outage_...` (4 tests) | the dead-man across the three disconnect sources | R6, B.9, W3d |
//! | `a_bridge_panic_...` | the bridge panics inside `submit` | W8b |
//!
//! **Not repeated here, cited:** the live-account lock held by another process refuses the whole live mount
//! under the PRODUCTION arming table (the path that was silently dead until W11c):
//! `crates/vike-tradehub/tests/gateway_refusals.rs`'s
//! `a_live_lock_another_process_holds_refuses_the_whole_mount_in_the_locks_words` and
//! `crates/vike-gatewayhub/tests/trade_live_lock.rs`, which drive `BridgeTrade` over the real `REGISTRY`;
//! the NEVER-SENT and UNKNOWN ledger rules against a fake peer (`crates/vike-gatewayhub-client/tests/never_sent.rs`,
//! `unknown_resolution.rs`); the latch's unit rules (`crates/vike-core/src/runtime/tests/link_deadman_unit.rs`).
//! What they could not show, and these do: the same rules hold through TCP, a real daemon, a real client and a
//! real core at once.
//!
//! **W9c (the account directory and `vars` reaching the bridges) had not landed on the branch this was
//! written against**, so its delivery proof is not here.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vike_core::{CoreConfig, DeadManAction, LinkDeadManConfig};
use vike_data::{DataClient, HistStore, LiveDataSink, MemHistStore, NoopSink};
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Sharing};
use vike_datahub::server::{ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub::strat::{StratHub, StratLimits};
use vike_exec::{Command, StreamStatusUpdate};
use vike_gatewayhub::testkit::gateway::trade_keys;
use vike_gatewayhub::testkit::trade::TradeCall;
use vike_gatewayhub::trade::lease::ADAPTER_FAILED_REASON;
use vike_gatewayhub_client::client::GatewayLink;
use vike_gatewayhub_client::trade::{
    NEVER_RECEIVED_SUBMIT_REASON, NEVER_SENT_MODIFY_REASON, NEVER_SENT_SUBMIT_REASON,
};
use vike_hub_proto::auth::HubKeys;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{FeedStatus, OrderStatus, TradingState};
use vike_tradehub::feeds::{FeedCtors, ProdFeedCtors};
use vike_tradehub::remote_backend::RemoteBackend;
use vike_tradehub_core::{BackendError, VenueBackend as _};
use vike_venues::link_deadman::{STRAT_LINK_STREAM, TRADE_LINK_STREAM};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/hop_rig.rs"]
mod hop_rig;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "../../vike-gatewayhub-client/tests/support/fake_peer.rs"]
mod fake_peer;

use hop_rig::{
    Call, CoreRig, Fate, SYMBOL, Stack, VENUE, coid_of, is_restated, is_terminal, order, wait_until,
};

const WAIT: Duration = Duration::from_secs(15);

fn plain_core() -> CoreConfig {
    CoreConfig { seed_cash: 10_000.0, ..CoreConfig::default() }
}

/// The reasons of every `OrderRejected` the core's lane carried for `coid`, in order.
fn rejects(rig: &CoreRig, coid: &str) -> Vec<String> {
    rig.relayed
        .lock()
        .expect("the relay's record")
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) if r.client_order_id == coid => Some(r.reason.to_string()),
            _ => None,
        })
        .collect()
}

/// How many terminal events the core's lane carried for `coid`: an order must end exactly once.
fn terminals(rig: &CoreRig, coid: &str) -> usize {
    rig.relayed
        .lock()
        .expect("the relay's record")
        .iter()
        .filter(|e| coid_of(e) == Some(coid) && is_terminal(e))
        .count()
}

fn accepted(s: &Stack, coid: &str) {
    assert!(
        s.rig.wait_status(coid, OrderStatus::Accepted, WAIT),
        "{coid} rests: {:?}",
        s.rig.orders()
    );
}

fn link_events(rig: &CoreRig) -> Vec<String> {
    rig.links.lock().expect("the relay's link record").clone()
}

/// Everything the core's lane carried about `coid` and about the link, for a failure message.
fn dump(rig: &CoreRig, coid: &str) -> String {
    let events: Vec<String> = rig
        .relayed
        .lock()
        .expect("the relay's record")
        .iter()
        .filter(|e| coid_of(e) == Some(coid))
        .map(|e| format!("{e:?}"))
        .collect();
    format!(
        "orders {:?}\nevents of {coid}: {events:?}\nlink: {:?}\ncursors: {:?}",
        rig.orders(),
        link_events(rig),
        rig.cursors.lock().expect("cursors")
    )
}
fn wait_link(rig: &CoreRig, what: &str) {
    assert!(
        wait_until(WAIT, || link_events(rig).iter().any(|l| l == what)),
        "the core was told `{what}`: {:?}",
        link_events(rig)
    );
}

// ---- R25: keys ---------------------------------------------------------------------------------------------

/// **R25: a tradehub holding only the desktop's OBSERVE key (or the control key) cannot open a strategy
/// lane.** The strategy lane is the datahub's `Scope::Trade`, which only the dedicated strategy-lane key
/// signs. The tradehub's feed constructor dials with whatever key it was handed as its strategy-lane key: a
/// datahub that holds all three refuses the dial of the other two, the feed never comes up, and the SAME
/// constructor with the right key comes up on the same server (so the refusal is the key's, not the rig's).
/// Which key a tradehub is HANDED is W1's predicate matrix (`crates/vike-ops/tests/settings_secrets/hub_key_store_gate`).
#[test]
fn a_tradehub_holding_only_the_desktops_observe_key_cannot_open_a_strategy_lane() {
    const OBSERVE: &[u8] = b"desktop-observe-key";
    const CONTROL: &[u8] = b"operator-control-key";
    const LANE: &[u8] = b"the-strategy-lane-key";

    let builder: ClientBuilder = Box::new(
        |_: &str, _: Arc<dyn LiveDataSink>| -> Result<Box<dyn DataClient + Send>, String> {
            Err("this test has no venue".into())
        },
    );
    let broker = FeedBroker::new(builder, |_| Sharing::Shared);
    let hub = StratHub::new(broker, vec![VENUE.to_string()], None, StratLimits::PRODUCTION);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let lanes = ServerLanes { strat: Some(hub), ..ServerLanes::default() };
    let keys = HubKeys::new(OBSERVE.to_vec(), CONTROL.to_vec()).with_trade(LANE.to_vec());
    std::thread::Builder::new()
        .name("r25-datahub".into())
        .spawn(move || {
            let _ = serve_with_lanes(
                listener,
                store,
                None,
                Some(keys),
                None,
                None,
                lanes,
                None,
                false,
                ServerTimings::PRODUCTION,
            );
        })
        .expect("spawn the datahub");

    let dial = |key: &[u8]| {
        let ctors = ProdFeedCtors::new(addr.to_string(), Some(key.to_vec()));
        let mut feed = ctors.strat(VENUE, None, Arc::new(NoopSink)).expect("a feed is built");
        // A subscription is what puts a reader to work on every build of the feed.
        let _ = feed.subscribe_quotes(SYMBOL);
        feed
    };

    // The control: the strategy-lane key opens the lane on this very server.
    let mut right = dial(LANE);
    assert!(wait_until(WAIT, || right.is_up()), "the strategy-lane key opens the lane");
    right.shutdown();

    for (name, key) in [("observe", OBSERVE), ("control", CONTROL)] {
        let mut wrong = dial(key);
        assert!(
            !wait_until(Duration::from_millis(2_500), || wrong.is_up()),
            "the {name} key must not open a strategy lane"
        );
        wrong.shutdown();
    }
}

// ---- 0112: an old gatewayhub --------------------------------------------------------------------------------

/// **A gatewayhub that does not advertise the trade class is refused LOCALLY, by feature name, before any
/// mac is signed** (decision 0112; R23: an old daemon never receives an order-scope signature). The peer
/// here is a wave-1 gatewayhub (the data features, no `gw_trade`): the tradehub's backend reads its welcome,
/// refuses, and every demo/live account is NOT RUNNING (`start_adapter` is an `Unavailable`, never a paper
/// client). The peer's own record is the proof that nothing was signed.
#[test]
fn a_gatewayhub_without_the_trade_class_is_refused_by_feature_name_before_a_mac_is_signed() {
    let peer = fake_peer::FakePeer::start(
        fake_peer::PeerConfig::new().with_features(fake_peer::features(&[VENUE], &[])),
    );
    let link = GatewayLink::new(peer.addr_string(), trade_keys())
        .with_timings(vike_gatewayhub::testkit::gateway::fast_timings());
    let backend = RemoteBackend::connect(
        link,
        hop_rig::accounts(),
        "vike-tradehub-old-daemon".to_string(),
        Duration::from_millis(400),
    );
    let why = backend.gateway_down().expect("the old daemon is refused").to_string();
    assert!(why.contains("gw_trade"), "the refusal names the missing feature: {why}");
    assert!(peer.hellos() >= 1, "it did read the daemon's welcome");
    assert!(
        peer.auths().is_empty(),
        "no Auth frame (so no mac) went to a daemon without the trade class: {:?}",
        peer.auths()
    );
    let label = AccountLabel::Default;
    match backend.start_adapter(hop_rig::start_query(&label), &hop_rig::poll_lane().0, None) {
        Err(BackendError::Unavailable(_)) => {}
        other => panic!("a demo account on an old daemon is NOT RUNNING: {other:?}"),
    }
    assert!(peer.auths().is_empty(), "and the start signed nothing either");
}

// ---- R7, R10: the restart drill -------------------------------------------------------------------------------

fn taught(lane: &hop_rig::ActorLane) -> BTreeSet<String> {
    lane.calls()
        .into_iter()
        .filter_map(|c| match c {
            Call::Adopt(pairs) => Some(pairs),
            _ => None,
        })
        .flatten()
        .map(|(coid, _symbol)| coid)
        .collect()
}

/// **A gatewayhub restart under a live lease, with orders resting** (B.7, B.8): the daemon comes back with a
/// new epoch and nothing in memory. The tradehub must (1) start the adapter again with the parameters of the
/// first, (2) re-teach the bridge EVERY resting order of the account, read off the real core's snapshot (a
/// filled order is not re-taught), (3) ask for a reconcile at once, and (4) carry on: events of the new
/// epoch reach the core (their sequence numbers start over; the old cursor must not eat them as
/// duplicates), and a cancel of a re-taught order reaches the venue and ends it.
#[test]
fn a_gatewayhub_restart_is_a_new_epoch_every_resting_order_is_re_taught_and_a_reconcile_is_asked() {
    let mut s = Stack::start(plain_core());
    s.world().fate("done", Fate::Fill);
    for coid in ["r1", "r2", "r3"] {
        s.rig.submit(order(coid, 100.0));
        accepted(&s, coid);
    }
    s.rig.submit(order("done", 101.0));
    assert!(s.rig.wait_status("done", OrderStatus::Filled, WAIT), "{:?}", s.rig.orders());
    let first_epoch = s.rig.cursors.lock().expect("cursors").last().expect("events arrived").0;
    assert_eq!(s.hop.lane.inner.starts(), 1, "one adapter on the first daemon");
    let first_start = s
        .hop
        .lane
        .inner
        .calls()
        .into_iter()
        .find_map(|c| match c {
            TradeCall::Start { start, .. } => Some(start),
            _ => None,
        })
        .expect("the first AdapterStart");

    s.hop.restart_gateway();

    assert!(
        wait_until(WAIT, || s.hop.lane.inner.starts() == 1),
        "the adapter is started again on the NEW daemon: {:?}",
        s.hop.lane.inner.calls()
    );
    let second_start = s
        .hop
        .lane
        .inner
        .calls()
        .into_iter()
        .find_map(|c| match c {
            TradeCall::Start { start, .. } => Some(start),
            _ => None,
        })
        .expect("the second AdapterStart");
    assert_eq!(
        (&second_start.symbol, second_start.recon_enabled, second_start.takes_trigger),
        (&first_start.symbol, first_start.recon_enabled, first_start.takes_trigger),
        "restarted with the parameters of the first"
    );
    let resting: BTreeSet<String> = ["r1", "r2", "r3"].map(String::from).into();
    assert!(
        wait_until(WAIT, || taught(&s.hop.lane) == resting),
        "EVERY resting order, and only those, is re-taught: {:?}",
        s.hop.lane.calls()
    );
    assert!(s.triggers.recv_timeout(WAIT).is_ok(), "an immediate reconcile pass is requested");

    // Carrying on: an order of the new epoch is delivered, under a different epoch than the first events.
    s.rig.submit(order("after", 102.0));
    accepted(&s, "after");
    let epochs: BTreeSet<u64> =
        s.rig.cursors.lock().expect("cursors").iter().map(|(epoch, _)| *epoch).collect();
    assert!(epochs.len() >= 2 && !epochs.is_empty(), "two epochs reached the core: {epochs:?}");
    assert_ne!(
        s.rig.cursors.lock().expect("cursors").last().expect("events").0,
        first_epoch,
        "the latest events carry the new daemon's epoch"
    );
    // A re-taught order is cancellable at the venue after the restart.
    s.rig.cancel("r1");
    assert!(s.rig.wait_status("r1", OrderStatus::Canceled, WAIT), "{:?}", s.rig.orders());
    assert!(!s.world().resting().contains("r1"));
    // Nothing ended twice, nothing vanished.
    for coid in ["r1", "r2", "r3", "done", "after"] {
        assert!(terminals(&s.rig, coid) <= 1, "{coid} ended at most once");
    }
}

// ---- R5: the trade link drops ------------------------------------------------------------------------------------

/// **A submit made while the link is down is a terminal reject at once, and the venue never sees it**; a
/// modify gets a non-terminal advisory (the order keeps its terms and the venue never hears the modify); a
/// cancel is NEVER shed: it is held and flushed at the reconnect, and the resting order ends. (B.7's table,
/// through TCP, a real daemon and a real core.)
#[test]
fn the_trade_link_dropped_with_a_submit_queued_is_a_terminal_reject_and_a_cancel_is_held_and_flushed()
 {
    let s = Stack::start(plain_core());
    s.rig.submit(order("r1", 100.0));
    accepted(&s, "r1");

    s.hop.proxy.cut();
    wait_link(&s.rig, "exec-link down");

    s.rig.submit(order("n1", 101.0));
    assert!(s.rig.wait_status("n1", OrderStatus::Rejected, WAIT), "{:?}", s.rig.orders());
    let why = rejects(&s.rig, "n1");
    assert!(
        why.len() == 1 && why[0].contains(NEVER_SENT_SUBMIT_REASON),
        "the NEVER-SENT words, once: {why:?}"
    );
    s.rig.modify("r1", None, Some(99.0));
    assert!(
        wait_until(WAIT, || {
            s
            .rig
            .relayed
            .lock()
            .expect("record")
            .iter()
            .any(|e| matches!(e, Event::OrderModifyRejected(m) if m.client_order_id == "r1" && m.reason.contains(NEVER_SENT_MODIFY_REASON)))
        }),
        "the modify is an advisory, not a terminal"
    );
    s.rig.cancel("r1");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        s.world().resting().contains("r1"),
        "the cancel waits on the dead link, it is not lost"
    );

    s.hop.proxy.heal();
    assert!(s.rig.wait_status("r1", OrderStatus::Canceled, WAIT), "{:?}", s.rig.orders());
    assert!(!s.world().resting().contains("r1"), "the held cancel reached the venue");
    assert_eq!(s.world().seen_submits(), ["r1"], "n1 never reached the venue");
    assert!(!s.hop.lane.calls().iter().any(|c| matches!(c, Call::Modify(_))), "nor did the modify");
    assert_eq!(terminals(&s.rig, "n1"), 1, "n1 ended exactly once");
}

/// **A frame the daemon READ whose acknowledgement and events were lost is left to the venue**: after the
/// reconnect inside the same epoch the ring replays what the client missed, the order is accepted, and NO
/// false reject was synthesised for an order the venue holds.
#[test]
fn the_trade_link_dropped_after_the_daemon_read_a_submit_replays_it_and_never_rejects_it() {
    let s = Stack::start(plain_core());
    // The client holds a ring position only once it has heard one event: an earlier order gives it one, and
    // a client that has heard nothing yet opens fresh and replays nothing (a separate, documented edge).
    s.rig.submit(order("r0", 99.0));
    accepted(&s, "r0");
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("lost-ack", 100.0));
    assert!(
        wait_until(WAIT, || s.world().seen_submits().contains(&"lost-ack".to_string())),
        "the daemon read it and the venue has it"
    );
    s.hop.proxy.cut();
    s.hop.proxy.heal();

    assert!(
        s.rig.wait_status("lost-ack", OrderStatus::Accepted, WAIT),
        "the ring replays the missed events: {}",
        dump(&s.rig, "lost-ack")
    );
    assert!(rejects(&s.rig, "lost-ack").is_empty(), "no false reject for an order the venue holds");
    assert!(s.world().resting().contains("lost-ack"));
    assert_eq!(s.world().seen_submits(), ["r0", "lost-ack"], "and it was sent exactly once");
}

/// **W19-D3 (FIXED: `vike-gatewayhub-client`s `trade/cursor.rs` anchor, served by the gatewayhub's `trade/ring.rs`
/// `Resume`, W7/W8), found while writing the test above:** a client that has not yet heard ONE event holds no
/// ring position, so its redial inside the same epoch opens FRESH and the daemon replays nothing. The first
/// order of a quiet account, made just before a link flap, has its `OrderSubmitted` and `OrderAccepted` lost
/// for good (the daemon's ring holds them; nobody asks) while the venue holds the order: the core keeps it
/// `Initialized`, the watchdog's `confirm` answer is a bare `OrderAccepted` the fold drops (W19-D1), and the
/// last-resort reject then retires an order that rests at the venue. No reconcile is forced either (a fresh
/// open is not a `ReplayUnavailable`).
#[test]
fn the_trade_link_dropped_before_the_first_event_replays_it() {
    let s = Stack::start(plain_core());
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("first", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"first".to_string())));
    s.hop.proxy.cut();
    s.hop.proxy.heal();
    assert!(
        s.rig.wait_status("first", OrderStatus::Accepted, WAIT),
        "the ring replays the events of the first order: {}",
        dump(&s.rig, "first")
    );
}

/// **A frame the daemon NEVER read is rejected after the reconnect, with the "never received" words**: the
/// gatewayhub's watermark proves it was not processed (same epoch), so a terminal is safe, and the venue
/// has no such order.
#[test]
fn the_trade_link_dropped_before_the_daemon_read_a_submit_rejects_it_as_never_received() {
    let s = Stack::start(plain_core());
    s.rig.submit(order("r1", 100.0));
    accepted(&s, "r1");
    s.hop.proxy.swallow_up(true);
    s.rig.submit(order("unread", 101.0));
    std::thread::sleep(Duration::from_millis(400));
    assert!(!s.world().seen_submits().contains(&"unread".to_string()), "the daemon never read it");
    s.hop.proxy.cut();
    s.hop.proxy.heal();

    assert!(s.rig.wait_status("unread", OrderStatus::Rejected, WAIT), "{:?}", s.rig.orders());
    let why = rejects(&s.rig, "unread");
    assert!(
        why.len() == 1 && why[0].contains(NEVER_RECEIVED_SUBMIT_REASON),
        "the never-received words, once: {why:?}"
    );
    assert!(
        !s.world().seen_submits().contains(&"unread".to_string()),
        "no phantom order at the venue"
    );
    assert_eq!(s.rig.status("r1"), Some(OrderStatus::Accepted), "the resting order is untouched");
}

/// **A submit written across a gatewayhub RESTART is UNKNOWN, and the venue's answer reaches the core**: the
/// new epoch has no watermark, so nothing is synthesised; the tradehub re-teaches the new daemon and asks it
/// to resolve the order by client order id (`confirm`), whose authoritative answer arrives as an ordinary
/// event on the core's lane. Here the venue HOLDS the order. Where the venue never saw it
/// ([`an_unknown_submit_the_venue_never_saw_is_never_given_a_false_terminal`]) nothing is invented.
///
/// What the core then DOES with that answer is the next test's ([`the_core_ends_a_resolved_unknown_submit_accepted`],
/// W19-D1, fixed).
#[test]
fn the_trade_link_dropped_across_a_gatewayhub_restart_leaves_the_submit_unknown_and_the_venue_answers()
 {
    let mut s = Stack::start(plain_core());
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("x", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"x".to_string())));

    s.hop.restart_gateway();

    assert!(
        wait_until(WAIT, || s.hop.lane.calls().contains(&Call::Confirm("x".to_string()))),
        "the new daemon was asked to look the order up by client order id: {:?}",
        s.hop.lane.calls()
    );
    assert!(
        wait_until(WAIT, || s
            .rig
            .relayed
            .lock()
            .expect("record")
            .iter()
            .any(|e| matches!(e, Event::OrderAccepted(a) if a.client_order_id == "x"))),
        "the venue's authoritative answer reached the core's lane: {}",
        dump(&s.rig, "x")
    );
    assert!(rejects(&s.rig, "x").is_empty(), "UNKNOWN is never turned into a reject");
    assert_eq!(s.world().seen_submits(), ["x"], "and the submit was never re-sent");
}

/// **W19-D1 (FIXED: `ManagedOrder::apply_over_lost_prefix`, which the engine fold opens), found by the test above:**
/// the answer to an UNKNOWN submit is a bare `OrderAccepted` (`vike_bridge_core::resolve_ambiguous_submit`),
/// and the FSM's table accepts it only from `Submitted` (the r5 fixture pins that, so the table did not move). An order whose `OrderSubmitted` event was lost with the gatewayhub's ring is still
/// `Initialized` in the core, so `ExecutionEngine::on_event` DROPPED the venue's answer (a silent
/// `Fold::Dropped`, counted only for terminals) and the order stays `Initialized` until the stuck-order
/// watchdog's last resort rejects it while it rests at the venue. The resolution chain delivers the answer
/// and the fold cannot use it, in exactly the case the chain exists for.
#[test]
fn the_core_ends_a_resolved_unknown_submit_accepted() {
    let mut s = Stack::start(plain_core());
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("x", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"x".to_string())));
    s.hop.restart_gateway();
    assert!(
        s.rig.wait_status("x", OrderStatus::Accepted, WAIT),
        "the venue's answer resolved it: {}",
        dump(&s.rig, "x")
    );
}

/// **An UNKNOWN submit the venue never saw is NEVER given a false terminal**: the lookup finds nothing and
/// reports nothing, the order stays pending in the core for reconcile and the stuck-order watchdog (the
/// documented residual), and no reject appears.
#[test]
fn an_unknown_submit_the_venue_never_saw_is_never_given_a_false_terminal() {
    let mut s = Stack::start(plain_core());
    s.hop.proxy.swallow_up(true);
    s.rig.submit(order("y", 100.0));
    std::thread::sleep(Duration::from_millis(400));
    assert!(!s.world().seen_submits().contains(&"y".to_string()));

    s.hop.restart_gateway();

    assert!(
        wait_until(WAIT, || s.hop.lane.calls().contains(&Call::Confirm("y".to_string()))),
        "the lookup was asked: {:?}",
        s.hop.lane.calls()
    );
    std::thread::sleep(Duration::from_millis(600));
    let status = s.rig.status("y").expect("the core holds the order");
    assert!(!status.is_terminal(), "no false terminal: {status:?}");
    assert!(rejects(&s.rig, "y").is_empty());
    assert!(!s.world().seen_submits().contains(&"y".to_string()), "the venue never saw it");
}

/// **A terminal the venue's lookup RE-STATES is folded once** (the product half of `no_order_vanishes`' rule 1).
///
/// The daemon's `confirm` of an UNKNOWN submit answers with the venue's authoritative terminal as an ordinary
/// event under the next `seq`, so when the order's own cancel or fill is already in the core (the cancel was
/// released from the ledger's hold just before the lookup, or the live event is simply earlier) the lane
/// carries the ending twice, under two cursors the duplicate guard rightly calls two events. This pins that the
/// CORE is where that is made harmless: a re-stated fill books nothing again, a re-stated cancel changes no
/// order. The re-statement is injected on the lane's own door, behind a barrier (a command queued after it
/// folds after it: one ingest lane).
#[test]
fn the_core_folds_a_restated_terminal_once() {
    let s = Stack::start(plain_core());
    s.world().fate("f", Fate::Fill);
    s.rig.submit(order("f", 100.0));
    s.rig.submit(order("c", 101.0));
    assert!(s.rig.wait_status("f", OrderStatus::Filled, WAIT), "{:?}", s.rig.orders());
    accepted(&s, "c");
    s.rig.cancel("c");
    assert!(s.rig.wait_status("c", OrderStatus::Canceled, WAIT), "{:?}", s.rig.orders());

    let before = s.rig.core().snapshot();
    let door = s.rig.core().event_sender().routed(VENUE);
    for coid in ["f", "c"] {
        let answer = s.world().confirm(coid);
        assert!(
            answer.len() == 1 && is_restated(&answer[0]) && is_terminal(&answer[0]),
            "the lookup re-states the ending: {answer:?}"
        );
        for e in answer {
            door.blocking_send(e).expect("the core takes the lookup's answer");
        }
    }
    s.rig.submit(order("barrier", 102.0));
    assert!(wait_until(WAIT, || s.rig.status("barrier").is_some()), "{:?}", s.rig.orders());
    let after = s.rig.core().snapshot();

    assert_eq!(
        format!(
            "{:?}",
            after.orders.iter().filter(|o| o.client_order_id != "barrier").collect::<Vec<_>>()
        ),
        format!("{:?}", before.orders.iter().collect::<Vec<_>>()),
        "no order moved"
    );
    assert_eq!(after.balance.to_bits(), before.balance.to_bits(), "no money moved");
    assert_eq!(
        format!("{:?}", after.positions),
        format!("{:?}", before.positions),
        "no position moved"
    );
    assert_eq!(s.rig.status("f"), Some(OrderStatus::Filled));
    assert_eq!(s.rig.status("c"), Some(OrderStatus::Canceled));
}

/// What the core BOOKED, for comparing two runs: the positions as the read model states them, and the
/// balance bit for bit.
fn books(rig: &CoreRig) -> (String, u64) {
    let snap = rig.core().snapshot();
    (format!("{:?}", snap.positions), snap.balance.to_bits())
}

/// The commission every fill of these tests carries: with none, a buy opens a position and moves no balance,
/// and a test that reads the balance would see nothing either way.
const FEE: f64 = 0.5;

/// What the core books for an order `x` the venue fills while the link is whole: the reference a run that
/// lost the live events must reach by another road. Returns the booked state after the fill.
fn books_of_a_live_fill() -> (String, u64) {
    let s = Stack::start(plain_core());
    s.world().set_commission(FEE);
    s.world().fate("x", Fate::Fill);
    s.rig.submit(order("x", 100.0));
    assert!(s.rig.wait_status("x", OrderStatus::Filled, WAIT), "{}", dump(&s.rig, "x"));
    assert!(
        wait_until(WAIT, || !s.rig.core().snapshot().positions.is_empty()),
        "the live fill is booked: {}",
        dump(&s.rig, "x")
    );
    books(&s.rig)
}

/// **W2F-D1 (FIXED in the client, `vike-gatewayhub-client`'s `trade/shared.rs` `Shared::deliver_event`; found
/// while triaging the `no_order_vanishes` flake, the sibling of W19-D1):** the venue FILLED an order whose
/// `OrderSubmitted` and `OrderAccepted` died with the old daemon's ring, so the lookup's answer is the
/// TERMINAL event, not a bare `OrderAccepted`. The core holds the order `Initialized` and its table refuses a
/// terminal from there; the client, which knows the coid is UNKNOWN, states the implied
/// `OrderSubmitted`+`OrderAccepted` ahead of the venue's word, so the order ends where the venue left it
/// instead of waiting for the stuck-order watchdog's last resort to REJECT a filled order.
///
/// **W2G-2 (money): and the FILL is booked.** The wrap alone moves only the order's FSM; the position, the fee
/// and the balance are the BARE fill's, which the lookup does not state. The client states the fill the wrap
/// embeds, so the core ends with the position and the balance a live fill books (read off the core, against a
/// run where the fill was live), and the venue's own bare fill arriving afterwards moves nothing (the core's
/// trade-id dedup).
#[test]
fn the_core_ends_a_filled_unknown_submit_filled() {
    let reference = books_of_a_live_fill();
    assert_ne!(reference.1, 10_000f64.to_bits(), "the fee moved the balance: {reference:?}");

    let mut s = Stack::start(plain_core());
    s.world().set_commission(FEE);
    s.world().fate("x", Fate::Fill);
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("x", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"x".to_string())));
    s.hop.restart_gateway();
    assert!(
        s.rig.wait_status("x", OrderStatus::Filled, Duration::from_secs(8)),
        "the venue's terminal answer ended it: {}",
        dump(&s.rig, "x")
    );
    assert_eq!(terminals(&s.rig, "x"), 1, "{}", dump(&s.rig, "x"));
    assert!(rejects(&s.rig, "x").is_empty(), "no phantom reject: {}", dump(&s.rig, "x"));
    assert!(
        wait_until(WAIT, || books(&s.rig) == reference),
        "the fill was BOOKED (position, fee, balance), not only the order's status: got {:?}, a live \
         fill books {reference:?}\n{}",
        books(&s.rig),
        dump(&s.rig, "x")
    );

    // The venue's own bare fill arrives late (the ring replay, a reconcile): the same trade id, booked once.
    let Some(Event::OrderFilled(wrap)) = s.world().confirm("x").into_iter().next() else {
        panic!("the venue restates the fill")
    };
    let door = s.rig.core().event_sender().routed(VENUE);
    door.blocking_send(Event::Fill(wrap.fill)).expect("the core takes the late fill");
    s.rig.submit(order("barrier", 102.0));
    assert!(wait_until(WAIT, || s.rig.status("barrier").is_some()), "{:?}", s.rig.orders());
    assert_eq!(books(&s.rig), reference, "the late bare fill moved nothing: deduped by trade id");
}

/// **W2G-1 (money): a bridge's SYNTHETIC cancel must not end a filled UNKNOWN submit Canceled.** Bybit
/// (110001, 170213) and okx (51400-51402) report a cancel of an order that is gone ("too late", already filled,
/// not found) as SUCCESS, and the shared actor states `OrderCanceled { ts: 0 }`. The core cancels a live order,
/// `Initialized` included; the cancel Written before the daemon died is held again and re-sent after the new
/// adapter is up, AHEAD of the lookup (the re-teaching worker releases the held cancels, then asks
/// `ResolveOrders`). That synthetic answer used to earn the implied prefix and end the order Canceled and flat
/// while the venue held it filled, and cleared UNKNOWN so the lookup's truth was dropped. It is now the
/// advisory it is, the coid stays UNKNOWN, and the lookup's filled wrap ends the order Filled with the fill booked.
#[test]
fn a_synthetic_cancel_of_a_filled_unknown_submit_does_not_end_it_canceled() {
    let reference = books_of_a_live_fill();

    let mut s = Stack::start(plain_core());
    s.world().set_commission(FEE);
    s.world().swallow_cancels_of_gone_orders(true);
    s.world().fate("x", Fate::Fill);
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("x", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"x".to_string())));
    s.rig.cancel("x"); // a sweep or a strategy cancels the order it cannot see the end of
    assert!(
        wait_until(WAIT, || s
            .hop
            .lane
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Cancel(coid, _) if coid == "x"))),
        "the cancel reached the daemon before it died: {:?}",
        s.hop.lane.calls()
    );

    s.hop.restart_gateway();

    // The new daemon's lane starts empty: its Cancel is the held one, sent again, and it arrives BEFORE the
    // lookup (the re-teaching worker releases the held cancels, then asks `ResolveOrders`).
    assert!(
        wait_until(WAIT, || {
            let calls = s.hop.lane.calls();
            let cancel =
                calls.iter().position(|c| matches!(c, Call::Cancel(coid, _) if coid == "x"));
            let confirm = calls.iter().position(|c| *c == Call::Confirm("x".to_string()));
            matches!((cancel, confirm), (Some(c), Some(f)) if c < f)
        }),
        "the held cancel was sent again to the new adapter, ahead of the lookup: {:?}",
        s.hop.lane.calls()
    );
    assert!(
        s.rig.wait_status("x", OrderStatus::Filled, Duration::from_secs(8)),
        "the lookup's word ended it, not the synthetic cancel: {}",
        dump(&s.rig, "x")
    );
    // The held cancel's answer (the bridge's synthetic cancel, however the client states it) travels the
    // same lane as the lookup's word, in EITHER order: the actor answers the cancel on its command thread
    // and the lookup on its own, so the two race to the lane. Wait for the answer to be there instead of
    // sleeping and hoping it has arrived, then judge what the core did with it.
    let answer_of_the_cancel = |e: &Event| {
        matches!(e, Event::OrderCanceled(c) if c.client_order_id == "x")
            || matches!(e, Event::OrderCancelRejected(c) if c.client_order_id == "x")
    };
    assert!(
        wait_until(WAIT, || s
            .rig
            .relayed
            .lock()
            .expect("the relay's record")
            .iter()
            .any(answer_of_the_cancel)),
        "the answer to the held cancel reached the core's lane: {}",
        dump(&s.rig, "x")
    );
    // BARRIER: the answer being in `relayed` only means it left the lane, not that the core FOLDED it. A
    // sentinel order queued behind it on the core's own single-writer inbox is folded only after everything
    // ahead of it, so once it shows a status a late `OrderCanceled` has been applied (or dropped) and the
    // status and books read below are the FINAL ones.
    s.rig.submit(order("barrier", 102.0));
    assert!(wait_until(WAIT, || s.rig.status("barrier").is_some()), "{:?}", s.rig.orders());
    assert_eq!(s.rig.status("x"), Some(OrderStatus::Filled), "{}", dump(&s.rig, "x"));
    // NOT `terminals(.., "x") == 1`: when the fill wins the race the bridge's bare synthetic cancel
    // legitimately follows it on the lane (a no-op in the core), so the lane carries two terminal
    // EVENTS for one ending (measured: ~4 of 70 runs). The ending is the fill, once.
    assert_eq!(
        s.rig
            .relayed
            .lock()
            .expect("the relay's record")
            .iter()
            .filter(|e| matches!(e, Event::OrderFilled(f) if f.client_order_id == "x"))
            .count(),
        1,
        "{}",
        dump(&s.rig, "x")
    );
    assert!(rejects(&s.rig, "x").is_empty(), "no phantom reject: {}", dump(&s.rig, "x"));
    // The synthetic cancel must never be the order's ENDING: if the lookup's fill got there first the client
    // no longer holds the coid UNKNOWN and the cancel reaches the core as the bare re-statement it is (the
    // core folds it as a no-op on the filled order, `the_core_folds_a_restated_terminal_once`); if the cancel
    // got there first the client states it as an advisory. What may never happen is a terminal cancel AHEAD
    // of the fill.
    let (filled_at, canceled_at) = {
        let relayed = s.rig.relayed.lock().expect("the relay's record");
        (
            relayed
                .iter()
                .position(|e| matches!(e, Event::OrderFilled(f) if f.client_order_id == "x")),
            relayed
                .iter()
                .position(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "x")),
        )
    };
    assert!(
        filled_at.is_some_and(|f| canceled_at.is_none_or(|c| f < c)),
        "no terminal cancel reached the core ahead of the fill of a filled order: {}",
        dump(&s.rig, "x")
    );
    assert!(
        wait_until(WAIT, || books(&s.rig) == reference),
        "and the fill is booked: got {:?}, a live fill books {reference:?}\n{}",
        books(&s.rig),
        dump(&s.rig, "x")
    );
}

/// **W2F-D1 and the journal: the implied prefix is journaled like any event, so the journal REFOLDS to the
/// same bytes.** The same ending as above on a JOURNALED core: the exit `Snap` holds the order `Filled`, and
/// `replay_offline` re-pumps the raw records through the same fold and passes its state-hash fence, twice,
/// with the same hash.
#[test]
fn a_journal_holding_a_filled_unknown_submit_refolds_to_the_same_bytes() {
    let dir = tempfile::tempdir().expect("a dir");
    let journal = dir.path().join("journal");
    let config = CoreConfig {
        seed_cash: 10_000.0,
        coid_session: Some(("cafef00d".into(), 0)),
        journal: Some(vike_core::JournalConfig {
            dir: journal.clone(),
            file: vike_journal::JournalFileConfig { segment_bytes: 1 << 20, flush_every: 1 },
            snapshot_every: 2,
        }),
        ..CoreConfig::default()
    };
    let mut s = Stack::start(config);
    // Warm the journal so a cadence `Snap` (the replay BASE) precedes the scenario.
    for coid in ["w1", "w2", "w3"] {
        s.rig.submit(order(coid, 99.0));
        accepted(&s, coid);
    }
    s.world().fate("x", Fate::Fill);
    s.hop.proxy.swallow_down(true);
    s.rig.submit(order("x", 100.0));
    assert!(wait_until(WAIT, || s.world().seen_submits().contains(&"x".to_string())));
    s.hop.restart_gateway();
    assert!(
        s.rig.wait_status("x", OrderStatus::Filled, Duration::from_secs(8)),
        "the venue's terminal answer ended it: {}",
        dump(&s.rig, "x")
    );
    s.rig.shutdown();

    let records = vike_journal::CommandJournal::read_all(&journal).expect("the journal reads");
    let exit_status = records
        .into_iter()
        .rev()
        .find_map(|r| match r {
            vike_journal::JournalRecord::Snap { engines, .. } => engines[0]
                .registry
                .iter()
                .find(|(coid, _)| coid == "x")
                .map(|(_, order)| order.status),
            _ => None,
        })
        .expect("a clean stop writes an exit Snap holding the order");
    assert_eq!(exit_status, OrderStatus::Filled);

    let first = vike_core::replay::replay_offline(&journal)
        .expect("the fence holds: the refold lands what the live run did");
    let second = vike_core::replay::replay_offline(&journal).expect("and again");
    assert_eq!(first.final_hash, second.final_hash, "a refold is deterministic");
    assert!(first.snaps_compared >= 1);
}

// ---- R6, B.9, W3d: the link dead-man across the disconnect sources ---------------------------------------------------

fn deadman_core(halt: &Path) -> CoreConfig {
    CoreConfig {
        seed_cash: 10_000.0,
        link_deadman: Some(LinkDeadManConfig {
            grace: Duration::from_millis(1_500),
            action: DeadManAction::CancelAllAndHalt,
            venues: BTreeSet::from([VENUE.to_string()]),
            halt_file: Some(halt.to_path_buf()),
        }),
        ..CoreConfig::default()
    }
}

fn status_update(stream: &str, status: FeedStatus) -> StreamStatusUpdate {
    StreamStatusUpdate {
        venue: VENUE.to_string(),
        symbol: SYMBOL.to_string(),
        stream: stream.to_string(),
        status,
    }
}

fn halted(s: &Stack) -> bool {
    s.rig.core().snapshot().trading_state == TradingState::Halted
}

fn resume(s: &Stack, halt: &Path) {
    let _ = std::fs::remove_file(halt);
    s.rig.core().send_command(Command::SetTradingState(TradingState::Active));
    assert!(
        wait_until(WAIT, || s.rig.core().snapshot().trading_state == TradingState::Active),
        "the operator resumes trading"
    );
}

/// **A trade-link outage longer than the grace trips the dead-man, and the cancel it issues is queued on
/// the dead link and flushes at the reconnect** (C9: a risk-off cancel is never shed). Inside the grace
/// nothing trips. The trade link's own disclosure words reached the core as `Disconnected` then `Live` on
/// the reserved `gateway-trade` word.
#[test]
fn a_trade_link_outage_past_the_grace_trips_the_dead_man_and_its_cancel_flushes_at_the_reconnect() {
    let dir = tempfile::tempdir().expect("a dir");
    let halt = dir.path().join("HALT");
    let s = Stack::start(deadman_core(&halt));
    s.rig.submit(order("r1", 100.0));
    accepted(&s, "r1");

    s.hop.proxy.cut();
    wait_link(&s.rig, "exec-link down");
    wait_link(&s.rig, &format!("status {TRADE_LINK_STREAM} Disconnected"));
    std::thread::sleep(Duration::from_millis(400));
    assert!(!halt.exists() && !halted(&s), "inside the grace nothing trips");

    assert!(
        wait_until(WAIT, || halt.exists() && halted(&s)),
        "past the grace the dead-man cancels and halts"
    );
    assert!(s.world().resting().contains("r1"), "the cancel is queued on the dead link, not lost");

    s.hop.proxy.heal();
    assert!(s.rig.wait_status("r1", OrderStatus::Canceled, WAIT), "{:?}", s.rig.orders());
    assert!(!s.world().resting().contains("r1"), "the held risk-off cancel reached the venue");
    wait_link(&s.rig, "exec-link up");
    wait_link(&s.rig, &format!("status {TRADE_LINK_STREAM} Live"));
}

/// **A flap shorter than the grace trips nothing** (the `Live` on the same word closes the window).
#[test]
fn a_trade_link_flap_inside_the_grace_trips_nothing() {
    let dir = tempfile::tempdir().expect("a dir");
    let halt = dir.path().join("HALT");
    let s = Stack::start(deadman_core(&halt));
    s.rig.submit(order("r1", 100.0));
    accepted(&s, "r1");

    s.hop.proxy.cut();
    wait_link(&s.rig, "exec-link down");
    std::thread::sleep(Duration::from_millis(300));
    s.hop.proxy.heal();
    wait_link(&s.rig, "exec-link up");
    std::thread::sleep(Duration::from_millis(2_300));
    assert!(!halt.exists() && !halted(&s), "the window closed before the grace");
    assert_eq!(s.rig.status("r1"), Some(OrderStatus::Accepted), "r1 is untouched");
}

/// **A `Live` from another source does not clear the trade link's outage** (B.9, W3b): the venue's own feed
/// and the strategy lane both report `Live` while the trade link is down; the trade-link window stays open
/// and trips at its grace.
#[test]
fn a_strat_or_venue_live_does_not_clear_a_trade_link_outage() {
    let dir = tempfile::tempdir().expect("a dir");
    let halt = dir.path().join("HALT");
    let s = Stack::start(deadman_core(&halt));

    s.hop.proxy.cut();
    wait_link(&s.rig, &format!("status {TRADE_LINK_STREAM} Disconnected"));
    let ticks = s.rig.core().tick_sender();
    let _ = ticks.stream_status(status_update("ticks", FeedStatus::Live));
    let _ = ticks.stream_status(status_update(STRAT_LINK_STREAM, FeedStatus::Live));

    assert!(
        wait_until(WAIT, || halt.exists() && halted(&s)),
        "the trade link's outage still trips: another source's Live closed only its own window"
    );
}

/// **Each source acts once for its own outage** (W3d). The trade link's outage trips; the operator clears
/// HALT and resumes while the link is STILL down; the venue's own feed then goes down and ITS window trips
/// the dead-man again (the first source's trip does not suppress the second's); neither re-fires for the
/// outage it already acted on; and a NEW outage of the venue source acts again.
#[test]
fn a_trade_link_outage_and_a_venue_outage_each_act_once_for_their_own_outage() {
    let dir = tempfile::tempdir().expect("a dir");
    let halt = dir.path().join("HALT");
    let s = Stack::start(deadman_core(&halt));
    let ticks = s.rig.core().tick_sender();

    s.hop.proxy.cut();
    wait_link(&s.rig, &format!("status {TRADE_LINK_STREAM} Disconnected"));
    assert!(wait_until(WAIT, || halt.exists() && halted(&s)), "the trade link's outage trips");

    resume(&s, &halt);
    let _ = ticks.stream_status(status_update("ticks", FeedStatus::Disconnected));
    assert!(
        wait_until(WAIT, || halt.exists() && halted(&s)),
        "the venue's outage trips on its own, though the trade link's already did"
    );

    resume(&s, &halt);
    std::thread::sleep(Duration::from_millis(2_600));
    assert!(
        !halt.exists() && !halted(&s),
        "neither source re-fires for an outage it already acted on"
    );

    let _ = ticks.stream_status(status_update("ticks", FeedStatus::Live));
    let _ = ticks.stream_status(status_update("ticks", FeedStatus::Disconnected));
    assert!(
        wait_until(WAIT, || halt.exists() && halted(&s)),
        "a NEW outage of the venue source acts again"
    );
}

// ---- W8b: a bridge panic ---------------------------------------------------------------------------------------------

/// **A bridge that panics mid-order ends that order in a terminal reject, never a vanished one** (W8b):
/// the daemon catches the panic, answers the command in hand (and every later command) with a terminal
/// reject through the ring, and keeps the resting order's state honest (neither lost nor falsely ended).
/// The venue never saw the panicking order or the ones after it, and each ended exactly once.
#[test]
fn a_bridge_panic_mid_order_ends_in_a_terminal_reject_and_never_a_vanished_order() {
    let s = Stack::start(plain_core());
    s.rig.submit(order("ok1", 100.0));
    accepted(&s, "ok1");
    s.hop.lane.poison("boom");

    s.rig.submit(order("boom", 101.0));
    assert!(s.rig.wait_status("boom", OrderStatus::Rejected, WAIT), "{:?}", s.rig.orders());
    s.rig.submit(order("after", 102.0));
    assert!(s.rig.wait_status("after", OrderStatus::Rejected, WAIT), "{:?}", s.rig.orders());

    let why = rejects(&s.rig, "boom");
    assert!(
        why.len() == 1 && why[0].contains(ADAPTER_FAILED_REASON),
        "the order in hand gets the daemon's adapter-failed words, once: {why:?}"
    );
    for coid in ["boom", "after"] {
        assert_eq!(rejects(&s.rig, coid).len(), 1, "{coid} was rejected once");
        assert_eq!(terminals(&s.rig, coid), 1, "{coid} ended exactly once");
    }
    assert_eq!(
        s.world().seen_submits(),
        ["ok1"],
        "the venue saw neither the panic nor what followed"
    );
    assert_eq!(s.rig.status("ok1"), Some(OrderStatus::Accepted), "the resting order is honest");
}
