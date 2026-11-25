#![cfg(feature = "trade")]
//! **UNKNOWN is not NEVER-SENT: a frame that was Written and cannot be accounted for is never turned
//! into a terminal event by the client, and is never sent twice.**
//!
//! Inside one `gateway_epoch` the reconnect's `received_through_command_sequence` is definitive (the gatewayhub's fenced
//! takeover reads the old connection to its end first), so nothing is UNKNOWN there. Across an epoch
//! change the watermark is gone: a Written submit stays UNKNOWN and resolves, in this order, through the
//! venue's own event, `ResolveOrders` (query-only), reconcile, and the stuck-order watchdog's last
//! resort. The client's part is to synthesize NO VERDICT, to ask, and to never re-send (it does state the
//! lost `OrderSubmitted`/`OrderAccepted` ahead of the venue's own fill, cancel or expiry: `implied_prefix` below).

use std::collections::BTreeMap;
use std::time::Duration;

use vike_execution::ExecutionClient;
use vike_gatewayhub_client::proto::{ResolveState, TradeUp};
use vike_gatewayhub_client::trade::{
    OUTCOME_UNKNOWN_CANCEL_REASON, OUTCOME_UNKNOWN_MODIFY_REASON, TradeLinkState,
};
use vike_model::events::{Event, OrderCanceled};

use vike_gatewayhub_client::test_support::fake_trade_peer;

use fake_trade_peer::{
    TradePeer, TradePeerConfig, accepted, accepted_without_id, cancel_advisories,
    modify_advisories, open, order, rejects, wait_ups,
};
use vike_log::poll::wait_until;

const WAIT: Duration = Duration::from_secs(5);

/// How many times each order client id was sent in a `Submit`/`SubmitBatch`.
fn send_counts(peer: &TradePeer) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for frame in peer.ups() {
        let order_client_ids: Vec<String> = match frame {
            TradeUp::Submit { order, .. } => vec![order.order_client_id],
            TradeUp::SubmitBatch { orders, .. } => {
                orders.into_iter().map(|o| o.order_client_id).collect()
            }
            _ => Vec::new(),
        };
        for order_client_id in order_client_ids {
            *counts.entry(order_client_id).or_insert(0) += 1;
        }
    }
    counts
}

#[test]
fn a_submit_written_across_a_gatewayhub_restart_stays_unknown_and_is_asked_about_not_resent() {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("u1"));
    exec.submit_batch(&[order("u2"), order("u3")]);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));

    peer.restart(); // a new epoch, no watermark, an empty ring
    assert!(peer.wait_leases(2, WAIT), "the client redials into the new epoch");
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));

    // Nothing is synthesized: not a reject, not an advisory.
    std::thread::sleep(Duration::from_millis(250));
    let after = lane.drain();
    assert!(rejects(&after).is_empty(), "UNKNOWN is never a synthesized terminal: {after:?}");

    // The client asked about exactly the three order client ids, once.
    let asked = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::ResolveOrders { .. }));
    assert_eq!(asked.len(), 1);
    let TradeUp::ResolveOrders { order_client_ids, .. } = &asked[0] else { unreachable!() };
    assert_eq!(order_client_ids, &["u1", "u2", "u3"]);
    assert!(wait_until(WAIT, || session.resolution_of("u3").is_some()));
    assert_eq!(session.resolution_of("u1"), Some(ResolveState::Querying));
    let mut unknown = session.unknown_orders();
    unknown.sort();
    assert_eq!(unknown, ["u1", "u2", "u3"], "still unknown until the venue speaks");

    // …and the venue's own event retires one of them.
    peer.push_event(accepted("u2"));
    assert!(wait_until(WAIT, || session.unknown_orders() == ["u1", "u3"]));

    // The accounting: each order client id was sent exactly once, in the first epoch, never again.
    let counts = send_counts(&peer);
    assert_eq!(counts.len(), 3);
    assert!(
        counts.values().all(|n| *n == 1),
        "the client never re-sends a Written frame: {counts:?}"
    );
    assert_eq!(session.stats().epoch_changes, 1);
}

/// An acknowledgement says the daemon OWNS an order, and a daemon that dies takes its answer with it: the
/// reject of an order it had no adapter for sat in the ring that died with it. Acknowledged and never answered
/// is asked about like Written and never acknowledged; an order the venue answered is not.
#[test]
fn a_submit_acknowledged_and_never_answered_across_an_epoch_change_is_unknown_and_asked_about() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("silent"));
    exec.submit(&order("spoken"));
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));
    peer.push_event(accepted("spoken"));
    // Its answer follows both acknowledgements and the event down the one connection.
    session.status().expect("the peer answers");

    peer.restart();
    assert!(peer.wait_leases(2, WAIT), "the client redials into the new epoch");

    let asked = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::ResolveOrders { .. }));
    assert_eq!(asked.len(), 1);
    let TradeUp::ResolveOrders { order_client_ids, .. } = &asked[0] else { unreachable!() };
    assert_eq!(order_client_ids, &["silent"]);
    assert_eq!(session.unknown_orders(), ["silent"]);
    assert!(rejects(&lane.drain()).is_empty(), "UNKNOWN is never a synthesized terminal");
    assert!(send_counts(&peer).values().all(|n| *n == 1), "nothing is sent again");
}

#[test]
fn inside_one_epoch_the_watermark_is_definitive_so_nothing_is_unknown_and_nothing_is_resent() {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("w1"));
    exec.submit(&order("w2"));
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));

    peer.kill_lease(); // a link flap: the gatewayhub is the same process
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    std::thread::sleep(Duration::from_millis(250));

    assert!(
        rejects(&lane.drain()).is_empty(),
        "both were read: the venue's events, not a reject, follow"
    );
    assert!(session.unknown_orders().is_empty());
    assert!(
        peer.ups().iter().all(|f| !matches!(f, TradeUp::ResolveOrders { .. })),
        "no epoch change, nothing to ask"
    );
    let counts = send_counts(&peer);
    assert!(counts.values().all(|n| *n == 1), "{counts:?}");
    assert_eq!(session.stats().epoch_changes, 0);
}

#[test]
fn a_daemon_that_cannot_resolve_leaves_the_orders_unknown_and_the_client_says_nothing_to_it() {
    let mut features = TradePeerConfig::new().features;
    features.retain(|f| f != "gateway_trade_resolve");
    let peer = TradePeer::start(TradePeerConfig::new().with_features(features).never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("x1"));
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(session.unknown_orders(), ["x1"], "reconcile and the watchdog are the backstop");
    assert!(rejects(&lane.drain()).is_empty());
    assert!(peer.ups().iter().all(|f| !matches!(f, TradeUp::ResolveOrders { .. })));
}

/// D10: `OrderCancelIntent::Routine`'s "a shed cancel must never vanish silently". A routine cancel and a
/// modify the dying daemon never acknowledged may or may not have reached the venue: each gets its
/// NON-terminal "outcome unknown" advisory, and neither is sent again.
#[test]
fn a_written_routine_cancel_or_modify_across_an_epoch_change_says_its_outcome_is_unknown_and_is_not_resent()
 {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("c1", vike_model::OrderCancelIntent::Routine);
    exec.modify(&order("m1"), Some(2.0), None);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));
    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    std::thread::sleep(Duration::from_millis(250));
    let got = lane.drain();
    assert!(rejects(&got).is_empty(), "never a terminal: {got:?}");
    assert_eq!(
        cancel_advisories(&got),
        [("c1".to_string(), OUTCOME_UNKNOWN_CANCEL_REASON.to_string())]
    );
    assert_eq!(
        modify_advisories(&got),
        [("m1".to_string(), OUTCOME_UNKNOWN_MODIFY_REASON.to_string())]
    );
    let count = |keep: fn(&TradeUp) -> bool| peer.ups().iter().filter(|f| keep(f)).count();
    assert_eq!(count(|f| matches!(f, TradeUp::Cancel { .. })), 1, "not sent again");
    assert_eq!(count(|f| matches!(f, TradeUp::Modify { .. })), 1, "not sent again");
}

/// D10: "never shed" held only until the ACKNOWLEDGEMENT. The daemon acknowledged a risk-off cancel and died
/// before calling the venue; after the restart the cancel goes to the new daemon again, behind the adapter.
/// One the venue answered before the restart is not.
#[test]
fn an_acknowledged_risk_off_cancel_nothing_answered_is_sent_again_after_a_gatewayhub_restart() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("silent", vike_model::OrderCancelIntent::RiskOff);
    exec.cancel_with_intent("spoken", vike_model::OrderCancelIntent::RiskOff);
    exec.cancel_with_intent("routine", vike_model::OrderCancelIntent::Routine);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 3));
    peer.push_event(Event::OrderCanceled(OrderCanceled {
        order_client_id: "spoken".to_string(),
        reason: "venue".into(),
        timestamp: 9,
        post_only_would_cross: false,
        refusal_id: None,
    }));
    // Its answer follows the acknowledgements and the event down the one connection.
    session.status().expect("the peer answers");

    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    let cancels_of = |peer: &TradePeer, id: &str| {
        peer.ups()
            .iter()
            .filter(
                |f| matches!(f, TradeUp::Cancel { order_client_id, .. } if order_client_id == id),
            )
            .count()
    };
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(cancels_of(&peer, "silent"), 1, "not before the adapter is back on the new epoch");

    session.start_adapter(crate::support::start_args()).expect("the adapter starts again");
    assert!(wait_until(WAIT, || cancels_of(&peer, "silent") == 2), "sent again, once");
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(cancels_of(&peer, "silent"), 2);
    assert!(
        peer.ups().iter().any(|f| matches!(
            f,
            TradeUp::Cancel { order_client_id, intent: vike_model::OrderCancelIntent::RiskOff, .. }
                if order_client_id == "silent"
        )),
        "with its own intent"
    );
    assert_eq!(cancels_of(&peer, "spoken"), 1, "the venue answered it: not sent again");
    assert_eq!(cancels_of(&peer, "routine"), 1, "a routine cancel is never sent twice");
    let got = lane.drain();
    assert!(rejects(&got).is_empty() && cancel_advisories(&got).is_empty(), "{got:?}");
}

#[test]
fn a_written_risk_off_cancel_across_an_epoch_change_is_sent_again_once_the_adapter_is_back() {
    // W8b finding 4: "never shed" is the risk-off rule, and a Written cancel the dying daemon never
    // acknowledged is shed by silence. A cancel names an order and an absolute outcome, so sending it
    // again is idempotent (a cancel of an order that is gone is the venue's advisory).
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.cancel_with_intent("c1", vike_model::OrderCancelIntent::RiskOff);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    std::thread::sleep(Duration::from_millis(250));
    let cancels = |peer: &TradePeer| {
        peer.ups().into_iter().filter(|f| matches!(f, TradeUp::Cancel { .. })).count()
    };
    assert_eq!(cancels(&peer), 1, "not before the adapter is back on the new epoch");

    session.start_adapter(crate::support::start_args()).expect("the adapter starts again");
    assert!(wait_until(WAIT, || cancels(&peer) == 2), "sent again, once");
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(cancels(&peer), 2);
    assert!(lane.drain().is_empty(), "and no advisory: the cancel was delivered, not shed");
}

#[test]
fn a_held_risk_off_cancel_reaches_the_restarted_daemon_after_every_teaching_frame_never_before() {
    // W13b finding 1. A venue whose order-id map the restart emptied (hyperliquid, polymarket, ig,
    // ctrader) answers a cancel of an order it was not taught with "no venue order id": the held risk-off
    // cancels must go out BEHIND the boot teaching and BEHIND the teaching of the orders minted since boot
    // (what the tradehub's re-teaching worker queues after the adapter answer).
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    // Written first (so the ledger re-holds the boot teaching AFTER the cancels at the reconnect).
    exec.cancel_with_intent("c1", vike_model::OrderCancelIntent::RiskOff);
    exec.adopt_restored(&[vike_execution::RestoredOrder::new("boot-order", "BTCUSDT")]);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));

    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    std::thread::sleep(Duration::from_millis(250));
    let before = peer.ups().len();

    session.restart_adapter(crate::support::start_args()).expect("the adapter starts again");
    // The re-teaching worker: the orders minted since boot, then the release.
    exec.adopt_restored(&[vike_execution::RestoredOrder::new("minted-order", "BTCUSDT")]);
    // A second sweep of the dead-man, mid-restart: it queues behind the teaching too.
    exec.cancel_with_intent("c2", vike_model::OrderCancelIntent::RiskOff);
    session.release_held();

    let cancels = |peer: &TradePeer| {
        peer.ups()[before..].iter().filter(|f| matches!(f, TradeUp::Cancel { .. })).count()
    };
    assert!(wait_until(WAIT, || cancels(&peer) == 2), "both cancels go out");
    let ups = peer.ups();
    let ups = &ups[before..];
    let first_cancel =
        ups.iter().position(|f| matches!(f, TradeUp::Cancel { .. })).expect("cancel");
    let taught: Vec<String> = ups
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match f {
            TradeUp::AdoptRestored { pairs, .. } => {
                assert!(i < first_cancel, "teaching after a cancel: {ups:?}");
                Some(pairs.iter().map(|p| p.0.clone()).collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect();
    assert!(taught.contains(&"boot-order".to_string()), "{ups:?}");
    assert!(taught.contains(&"minted-order".to_string()), "{ups:?}");
    let order_client_ids: Vec<String> = ups
        .iter()
        .filter_map(|f| match f {
            TradeUp::Cancel { order_client_id, .. } => Some(order_client_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(order_client_ids, ["c1", "c2"], "held cancels first, in the order held");
    assert!(lane.drain().is_empty(), "no advisory: every cancel was delivered, none shed");
}

/// **An advisory about a COMMAND resolves nothing (W19-D2).** A cancel of a submit lost across a restart
/// reaches the new daemon, the venue says "unknown order", and the daemon's `OrderCancelRejected` names the
/// order client id; so does a refused modify. Neither is the venue's word about the ORDER, so the submit stays UNKNOWN
/// (and stays named, for the tradehub's re-ask) until an event that resolves it arrives.
#[test]
fn a_cancel_or_modify_advisory_does_not_un_name_an_unknown_submit() {
    use vike_model::events::{Event, OrderCancelRejected, OrderModifyRejected, OrderSubmitted};

    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, _lane) = open(&peer, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit_batch(&[order("a1"), order("a2"), order("a3"), order("a4")]);
    assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
    peer.restart();
    assert!(peer.wait_leases(2, WAIT));
    assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
    let mut unknown = session.unknown_orders();
    unknown.sort();
    assert_eq!(unknown, ["a1", "a2", "a3", "a4"]);

    let delivered_before = session.stats().events_delivered;
    peer.push_event(Event::OrderCancelRejected(OrderCancelRejected {
        order_client_id: "a1".to_string(),
        reason: "unknown order".into(),
        timestamp: 5,
        refusal_id: None,
    }));
    peer.push_event(Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: "a2".to_string(),
        reason: "unknown order".into(),
        timestamp: 6,
        refusal_id: None,
    }));
    peer.push_event(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: "a3".to_string(),
        timestamp: 7,
    }));
    assert!(
        wait_until(WAIT, || session.stats().events_delivered >= delivered_before + 3),
        "the three events reached the core"
    );
    let mut unknown = session.unknown_orders();
    unknown.sort();
    assert_eq!(
        unknown,
        ["a1", "a2", "a3", "a4"],
        "two advisories and the bridge's own `OrderSubmitted` named the order client ids and resolved none"
    );

    peer.push_event(accepted("a1"));
    assert!(wait_until(WAIT, || session.unknown_orders() == ["a2", "a3", "a4"]));
}

// ---- W2F-D1: the venue's terminal answer to an UNKNOWN submit ---------------------------------------------------

mod implied_prefix {
    use super::*;
    use vike_execution::Ingest;
    use vike_model::EventRingCursor;
    use vike_model::events::{
        Event, FillEvent, OrderCanceled, OrderExpired, OrderFilled, OrderRejected, TradeId,
    };

    fn fill(order_client_id: &str) -> FillEvent {
        FillEvent {
            trade_id: TradeId::prefixed("t", order_client_id),
            order_client_id: order_client_id.to_string(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            last_qty: 1.0,
            last_price: 100.0,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".to_string().into(),
            timestamp: 9,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        }
    }

    fn filled(order_client_id: &str) -> Event {
        Event::OrderFilled(OrderFilled {
            order_client_id: order_client_id.to_string(),
            fill: fill(order_client_id),
            timestamp: 9,
        })
    }

    fn canceled(order_client_id: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: "venue".into(),
            timestamp: 9,
            post_only_would_cross: false,
            refusal_id: None,
        })
    }

    /// What `vike_bridge_core::exec_actor::cancel_event(order client id, ExecutionClientActorCancelOutcome::Canceled)` emits: the bridge's
    /// SYNTHETIC answer to a cancel its REST call reported as done, with no venue time and no reason. Bybit
    /// (110001, 170213) and okx (51400-51402) report "too late / filled / not found" as that success.
    fn synthetic_cancel(order_client_id: &str) -> Event {
        Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: String::new().into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        })
    }

    /// `(event, cursor)` of every venue event the lane carried, in arrival order (`None`: a plain event).
    fn carried(lane: &mut fake_trade_peer::Lane) -> Vec<(Event, Option<EventRingCursor>)> {
        lane.drain()
            .into_iter()
            .filter_map(|i| match i {
                Ingest::GatewayEvent(g) => Some((g.event, Some(g.cursor))),
                Ingest::Event(e) => Some((e, None)),
                _ => None,
            })
            .collect()
    }

    fn kind(e: &Event) -> &'static str {
        match e {
            Event::OrderSubmitted(_) => "submitted",
            Event::OrderAccepted(_) => "accepted",
            Event::OrderFilled(_) => "filled",
            Event::OrderPartiallyFilled(_) => "partial",
            Event::OrderCanceled(_) => "canceled",
            Event::OrderExpired(_) => "expired",
            Event::OrderRejected(_) => "rejected",
            Event::OrderCancelRejected(_) => "cancel_rejected",
            Event::Fill(_) => "fill",
            _ => "other",
        }
    }

    /// Three Written submits across a restart: `p1..p3` are UNKNOWN in the new epoch.
    fn unknowns() -> (TradePeer, vike_gatewayhub_client::trade::TradeLease, fake_trade_peer::Lane) {
        let peer = TradePeer::start(TradePeerConfig::new().never_ack());
        let (session, mut lane) = open(&peer, "tradehub-1");
        let mut exec = session.execution_client();
        exec.submit_batch(&[order("p1"), order("p2"), order("p3")]);
        assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
        peer.restart();
        assert!(peer.wait_leases(2, WAIT));
        assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
        assert_eq!(session.unknown_orders().len(), 3);
        let _ = lane.drain();
        (peer, session, lane)
    }

    /// **The venue's terminal answer to an UNKNOWN submit is stated after the order's lost prefix.** The
    /// core holds such an order `Initialized` and refuses a fill, cancel or expiry from there; the client
    /// states the implied `OrderSubmitted` and `OrderAccepted` (no venue id) first, at the answer's own
    /// cursor-less, then the answer itself with its ring cursor, and the order client id stops being UNKNOWN. A FILLED
    /// wrap carries the fill it books, so that fill is stated too (W2G-2): `fill_stated` below.
    #[test]
    fn a_terminal_answer_to_an_unknown_submit_is_preceded_by_its_lost_prefix() {
        let (peer, session, mut lane) = unknowns();
        peer.push_event(canceled("p2"));
        peer.push_event(Event::OrderExpired(OrderExpired {
            order_client_id: "p3".to_string(),
            timestamp: 9,
        }));
        assert!(wait_until(WAIT, || session.unknown_orders() == ["p1"]));
        let got = carried(&mut lane);
        let kinds: Vec<&str> = got.iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(
            kinds,
            ["submitted", "accepted", "canceled", "submitted", "accepted", "expired"],
            "{got:?}"
        );
        for chunk in got.chunks(3) {
            assert!(
                chunk[0].1.is_none() && chunk[1].1.is_none() && chunk[2].1.is_some(),
                "the prefix is plain; the answer keeps its ring cursor: {chunk:?}"
            );
        }
        let Event::OrderAccepted(a) = &got[1].0 else { unreachable!() };
        assert_eq!(a.venue_order_id, None, "no venue order is known, none is invented");
        assert_eq!(a.timestamp, 9, "the answer's own time");
        assert_eq!(session.stats().prefixes_synthesized, 2);
    }

    /// A venue-side reject needs no prefix (the table takes it from `Initialized`; an implied accept
    /// would make it illegal), a venue-named accept is the fold's own door, and the lookup's SECOND word
    /// on an order already answered adds nothing.
    #[test]
    fn a_reject_an_accept_and_a_repeated_terminal_get_no_prefix() {
        let (peer, session, mut lane) = unknowns();
        peer.push_event(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: "p1".to_string(),
            reason: "venue".into(),
            timestamp: 9,
            refusal_id: None,
        }));
        peer.push_event(accepted("p2"));
        peer.push_event(canceled("p3"));
        peer.push_event(canceled("p3")); // the lookup re-states the ending
        assert!(wait_until(WAIT, || session.unknown_orders().is_empty()));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 4));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(
            kinds,
            ["rejected", "accepted", "submitted", "accepted", "canceled", "canceled"],
            "only the first terminal of p3 carries the prefix"
        );
        assert_eq!(session.stats().prefixes_synthesized, 1);
    }

    /// The venue's live order is a bare `Fill` and then its `OrderFilled` wrap: the prefix goes ahead of
    /// the FIRST of them (the bare fill resolves the order client id), and the wrap, arriving after, adds none.
    #[test]
    fn a_bare_fill_then_its_wrap_carry_the_prefix_once() {
        let (peer, session, mut lane) = unknowns();
        peer.push_event(Event::Fill(fill("p1")));
        peer.push_event(filled("p1"));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 2));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["submitted", "accepted", "fill", "filled"]);
        assert_eq!(session.stats().prefixes_synthesized, 1);
    }

    /// An order the client is NOT unsure of (acknowledged inside its epoch) gets nothing added.
    #[test]
    fn a_terminal_for_an_order_that_is_not_unknown_is_delivered_alone() {
        let peer = TradePeer::start(TradePeerConfig::new());
        let (session, mut lane) = open(&peer, "tradehub-1");
        let mut exec = session.execution_client();
        exec.submit(&order("k1"));
        assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
        let _ = lane.drain();
        peer.push_event(filled("k1"));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 1));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["filled"]);
        assert_eq!(session.stats().prefixes_synthesized, 0);
    }

    /// **W2G-1 (money): a bridge's SYNTHETIC cancel is its answer to OUR cancel, not the venue's word on the
    /// order.** A REST venue reports "too late / already filled / not found" as success and the bridge states
    /// `OrderCanceled { ts: 0, reason: "" }`; a cancel held across the outage and flushed after the epoch
    /// change gets exactly that for an order that FILLED. Treated as the venue's terminal word it earned the
    /// implied prefix and ended a filled order Canceled and flat. It is now stated as the advisory it is (a
    /// non-terminal `OrderCancelRejected`), the order client id stays UNKNOWN, and the lookup, the venue's own event or
    /// reconcile decides. The same shape for an order we did NOT cancel is a venue stream's cancel
    /// (polymarket and ibkr state theirs with `ts: 0`): prefixed as before.
    #[test]
    fn a_synthetic_cancel_answer_to_our_own_cancel_is_no_resolution_and_earns_no_prefix() {
        let (peer, session, mut lane) = unknowns();
        let mut exec = session.execution_client();
        exec.cancel_with_intent("p1", vike_model::OrderCancelIntent::Routine);
        assert!(
            wait_until(WAIT, || peer.ups().iter().any(
                |f| matches!(f, TradeUp::Cancel { order_client_id, .. } if order_client_id == "p1")
            )),
            "the cancel reached the daemon"
        );

        peer.push_event(synthetic_cancel("p1"));
        peer.push_event(synthetic_cancel("p2"));
        assert!(wait_until(WAIT, || session.unknown_orders() == ["p1", "p3"]));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(
            kinds,
            ["cancel_rejected", "submitted", "accepted", "canceled"],
            "p1: the advisory alone; p2 (no cancel of ours): prefix and cancel"
        );
        assert_eq!(session.stats().prefixes_synthesized, 1);

        // The venue's own, stamped word on p1 afterwards still resolves it, prefix first.
        peer.push_event(canceled("p1"));
        assert!(wait_until(WAIT, || session.unknown_orders() == ["p3"]));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["submitted", "accepted", "canceled"]);
    }

    /// **W2G-2 (money): a FILLED wrap restating an UNKNOWN submit states the fill it carries.** The wrap
    /// alone moves only the order's FSM; the position, the fee and the balance are booked by the BARE `Fill`
    /// (the dual-publish contract sends both), and the lookup states the wrap only. The wrap embeds the full
    /// `FillEvent`, so the bare fill stated ahead of it (after the prefix, cursor-less like the prefix) is
    /// exact, and the core's trade-id dedup drops the venue's own bare fill if it arrives later.
    #[test]
    fn a_filled_wrap_for_an_unknown_submit_states_the_fill_it_carries_after_the_prefix() {
        let (peer, session, mut lane) = unknowns();
        peer.push_event(filled("p1"));
        peer.push_event(Event::OrderPartiallyFilled(vike_model::events::OrderPartiallyFilled {
            order_client_id: "p2".to_string(),
            fill: fill("p2"),
            timestamp: 9,
        }));
        assert!(wait_until(WAIT, || session.unknown_orders() == ["p3"]));
        let got = carried(&mut lane);
        let kinds: Vec<&str> = got.iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(
            kinds,
            ["submitted", "accepted", "fill", "filled", "submitted", "accepted", "fill", "partial"],
            "{got:?}"
        );
        for chunk in got.chunks(4) {
            assert!(
                chunk[0].1.is_none()
                    && chunk[1].1.is_none()
                    && chunk[2].1.is_none()
                    && chunk[3].1.is_some(),
                "the prefix and the fill are plain; the wrap keeps its ring cursor: {chunk:?}"
            );
        }
        let (Event::Fill(stated), Event::OrderFilled(wrap)) = (&got[2].0, &got[3].0) else {
            panic!("a fill and its wrap: {got:?}")
        };
        assert_eq!(stated, &wrap.fill, "the stated fill IS the wrap's, trade id and all");

        // A live bare fill arriving later is delivered as it is (the core's dedup drops it by trade id).
        peer.push_event(Event::Fill(fill("p1")));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 3));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["fill"]);
    }

    /// **W2G-4: an id-less OPTIMISTIC accept is not a resolution.** IG's `unresolved_confirm` (the lookup
    /// failed) states `OrderAccepted` with no venue order, and the fold refuses it from
    /// `Initialized` (`apply_over_lost_prefix` opens only for an accept that names one). Clearing UNKNOWN on
    /// it left the next fill or cancel with no prefix, deferred in the core while the watchdog asked for ever.
    #[test]
    fn an_id_less_optimistic_accept_leaves_the_order_unknown_so_the_next_word_gets_its_prefix() {
        let (peer, session, mut lane) = unknowns();
        peer.push_event(accepted_without_id("p1"));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 1));
        assert_eq!(session.unknown_orders().len(), 3, "the optimistic accept resolved nothing");
        peer.push_event(canceled("p1"));
        assert!(wait_until(WAIT, || session.unknown_orders() == ["p2", "p3"]));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["accepted", "submitted", "accepted", "canceled"]);
    }

    /// **W2G-6: an order the client already saw END is not marked UNKNOWN again.** Its submit was Written
    /// and unacknowledged when the daemon died, but the venue's cancel reached the core first: marking it
    /// made the lookup's re-statement earn a prefix the core refuses on a Canceled order (a "may be
    /// STRANDED" warning, a counter and two journal records for nothing), and kept it in `unknown_orders()`
    /// and in the re-ask.
    #[test]
    fn an_order_the_client_saw_end_is_not_unknown_after_the_restart() {
        let peer = TradePeer::start(TradePeerConfig::new().never_ack());
        let (session, mut lane) = open(&peer, "tradehub-1");
        let mut exec = session.execution_client();
        exec.submit_batch(&[order("q"), order("r")]);
        assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
        peer.push_event(canceled("q"));
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 1));

        peer.restart();
        assert!(peer.wait_leases(2, WAIT));
        assert!(wait_until(WAIT, || session.link_state() == TradeLinkState::Up));
        assert_eq!(session.unknown_orders(), ["r"], "q ended, only r is in doubt");
        let asked = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::ResolveOrders { .. }));
        let TradeUp::ResolveOrders { order_client_ids, .. } = &asked[0] else { unreachable!() };
        assert_eq!(order_client_ids, &["r"], "and only r is asked about");

        let _ = lane.drain();
        peer.push_event(canceled("q")); // the lookup would restate it
        assert!(wait_until(WAIT, || session.stats().events_delivered >= 2));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["canceled"], "no prefix for an order already ended");
        assert_eq!(session.stats().prefixes_synthesized, 0);
    }

    /// **W2G-5: the order client id is UNKNOWN before the reader hears anything.** A daemon that rebuilt the session
    /// itself (a supervised restore) has the answer in the new ring already, and replays it right behind
    /// `TradeOpened`. The reader was started BEFORE the order client ids were marked, so the answer could be delivered
    /// first: no prefix, and the order client id marked UNKNOWN after the event that resolved it, for good.
    #[test]
    fn an_answer_already_in_a_restored_ring_finds_its_order_client_id_unknown() {
        let peer = TradePeer::start(TradePeerConfig::new().never_ack().replaying_a_restored_ring());
        let (session, mut lane) = open(&peer, "tradehub-1");
        let mut exec = session.execution_client();
        exec.submit(&order("x1"));
        assert!(wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
        let _ = lane.drain();

        // The daemon is rebuilt with the answer already in its ring, and the client's redial is held off
        // until it is (the dial schedule retries a refused open).
        peer.refuse_open(Some(vike_gatewayhub_client::proto::Response::Error(
            "down for the test".into(),
        )));
        peer.restart();
        peer.push_event_offline(canceled("x1"));
        peer.refuse_open(None);
        assert!(peer.wait_leases(2, WAIT));

        assert!(wait_until(WAIT, || session.stats().events_delivered >= 1));
        let kinds: Vec<&str> = carried(&mut lane).iter().map(|(e, _)| kind(e)).collect();
        assert_eq!(kinds, ["submitted", "accepted", "canceled"], "the prefix went first");
        assert!(session.unknown_orders().is_empty(), "and the answer cleared it, once");
    }
}
