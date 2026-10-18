#![cfg(feature = "trade")]
//! **NEVER-SENT: no order may silently vanish when the lease is down.**
//!
//! B.7's table, driven against the fake gatewayhub with the link actually cut:
//!
//! * a submit made while the link is DOWN is rejected AT ONCE with a terminal `OrderRejected` (not after
//!   the redial), and the peer never sees it;
//! * a modify and a ROUTINE cancel get a NON-terminal advisory (the order, if live, is still resting);
//! * a RISK-OFF or unclassified cancel is NEVER shed: it is held, deduplicated, and flushed at the
//!   reconnect, ahead of anything newer, with no advisory;
//! * a frame the client WROTE that the gatewayhub's watermark proves it never READ is rejected after the
//!   reconnect with the "never received" words, and a frame it did read is left to the venue;
//! * the accounting identity under a flapping link: every submitted coid is either seen by the peer or
//!   rejected on the core's lane, never neither and never both (a double terminal is structurally
//!   impossible).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use vike_exec::{CancelIntent, ExecutionClient};
use vike_gatewayhub_client::proto::{Response, TradeUp, WireCancelIntent};
use vike_gatewayhub_client::trade::{
    LinkState, NEVER_RECEIVED_SUBMIT_REASON, NEVER_SENT_CANCEL_REASON, NEVER_SENT_MODIFY_REASON,
    NEVER_SENT_SUBMIT_REASON,
};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{
    Seen, TradePeer, TradePeerConfig, cancel_advisories, modify_advisories, open, order, rejects,
    wait_ups,
};

const WAIT: Duration = Duration::from_secs(5);

/// Keep the link DOWN: the peer refuses every redial, then the lease is cut. Returns once the client
/// has noticed.
fn take_the_link_down(peer: &TradePeer, session: &vike_gatewayhub_client::trade::TradeSession) {
    peer.refuse_open(Some(Response::Error("down for the test".into())));
    peer.kill_lease();
    assert!(
        TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Down),
        "the client must notice the cut"
    );
}

fn bring_the_link_back(
    peer: &TradePeer,
    session: &vike_gatewayhub_client::trade::TradeSession,
    leases: usize,
) {
    peer.refuse_open(None);
    assert!(peer.wait_leases(leases, WAIT), "the client redials and the peer opens again");
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
}

fn submits(peer: &TradePeer) -> BTreeSet<String> {
    peer.ups()
        .into_iter()
        .flat_map(|f| match f {
            TradeUp::Submit { order, .. } => vec![order.client_order_id],
            TradeUp::SubmitBatch { orders, .. } => {
                orders.into_iter().map(|o| o.client_order_id).collect()
            }
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn a_submit_while_the_link_is_down_is_a_terminal_reject_at_once_and_never_reaches_the_peer() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    take_the_link_down(&peer, &session);

    let mut exec = session.exec_client();
    exec.submit(&order("down-1"));
    let got = lane
        .collect_until(WAIT, 1, |i| !fake_trade_peer::rejects(std::slice::from_ref(i)).is_empty());
    let rejected = rejects(&got);
    assert_eq!(rejected.len(), 1, "exactly one terminal for the one order: {got:?}");
    assert_eq!(rejected[0].0, "down-1");
    assert_eq!(rejected[0].1, NEVER_SENT_SUBMIT_REASON);

    bring_the_link_back(&peer, &session, 2);
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !submits(&peer).contains("down-1"),
        "the rejected order was never sent, not even later"
    );
    assert!(
        lane.drain().iter().all(|i| rejects(std::slice::from_ref(i)).is_empty()),
        "and no second terminal follows the first"
    );
}

#[test]
fn a_modify_and_a_routine_cancel_get_non_terminal_advisories_while_down() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    take_the_link_down(&peer, &session);

    let mut exec = session.exec_client();
    exec.modify(&order("m1"), Some(2.0), None);
    exec.cancel_with_intent("k1", CancelIntent::Routine);
    let got = lane.collect_until(WAIT, 2, |_| true);
    assert_eq!(modify_advisories(&got), [("m1".to_string(), NEVER_SENT_MODIFY_REASON.to_string())]);
    assert_eq!(cancel_advisories(&got), [("k1".to_string(), NEVER_SENT_CANCEL_REASON.to_string())]);
    assert!(rejects(&got).is_empty(), "neither is a terminal event");
}

#[test]
fn a_risk_off_or_unclassified_cancel_is_held_deduplicated_and_flushed_at_the_reconnect() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    take_the_link_down(&peer, &session);

    let mut exec = session.exec_client();
    exec.cancel_with_intent("r1", CancelIntent::RiskOff);
    exec.cancel_with_intent("r1", CancelIntent::RiskOff); // the dead-man's sweep repeats
    exec.cancel("u1"); // unclassified: flatten-safe, never shed
    exec.cancel_batch_with_intent(&["r1".to_string(), "r2".to_string()], CancelIntent::RiskOff);
    std::thread::sleep(Duration::from_millis(150));
    assert!(cancel_advisories(&lane.drain()).is_empty(), "a held cancel is not an advisory");

    bring_the_link_back(&peer, &session, 2);
    let flushed = wait_ups(&peer, WAIT, 3, |f| {
        matches!(f, TradeUp::Cancel { .. } | TradeUp::CancelBatch { .. })
    });
    let coids: Vec<String> = flushed
        .iter()
        .flat_map(|f| match f {
            TradeUp::Cancel { coid, .. } => vec![coid.clone()],
            TradeUp::CancelBatch { coids, .. } => coids.clone(),
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        coids,
        ["r1", "u1", "r2"],
        "each coid once, in the order held: r1 (twice asked), u1, then the batch's new one"
    );
    assert!(matches!(&flushed[1], TradeUp::Cancel { intent: WireCancelIntent::Unspecified, .. }));
    assert!(cancel_advisories(&lane.drain()).is_empty(), "flushing is not shedding");
}

#[test]
fn a_written_frame_the_watermark_proves_unread_is_rejected_after_the_reconnect_and_a_read_one_is_not()
 {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.exec_client();

    // One order the peer READS (no ack: Written, unacknowledged), then it stops reading.
    exec.submit(&order("read-1"));
    assert!(TradePeer::wait_until(WAIT, || peer.received_through("tradehub-1") >= 1));
    peer.pause_reading();
    exec.submit(&order("lost-2"));
    exec.submit(&order("lost-3"));
    exec.cancel_with_intent("routine-4", CancelIntent::Routine);
    // The client WROTE them (write_all returned into the kernel); the peer never reads them.
    std::thread::sleep(Duration::from_millis(250));
    peer.kill_lease();
    peer.resume_reading();

    assert!(peer.wait_leases(2, WAIT), "the client redials");
    let got = lane.collect_until(WAIT, 3, |_| true);
    let rejected = rejects(&got);
    let coids: Vec<&str> = rejected.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(coids, ["lost-2", "lost-3"], "only the unread submits, in order: {got:?}");
    assert!(rejected.iter().all(|(_, reason)| reason == NEVER_RECEIVED_SUBMIT_REASON));
    assert_eq!(cancel_advisories(&got).len(), 1, "the unread routine cancel is an advisory");
    assert_eq!(
        submits(&peer),
        BTreeSet::from(["read-1".to_string()]),
        "the peer saw the one it read and none of the rejected ones"
    );
}

#[test]
fn every_order_is_either_seen_by_the_peer_or_rejected_never_neither_and_never_both() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.exec_client();
    let mut all = Vec::new();
    // coid -> the reject reason, kept for the failure message.
    let mut rejected: BTreeMap<String, String> = BTreeMap::new();

    for i in 0..60 {
        let coid = format!("o{i}");
        // Cut the link every seventh order; every thirteenth also hides the next frames from the peer.
        if i % 13 == 6 {
            peer.pause_reading();
        }
        exec.submit(&order(&coid));
        all.push(coid);
        if i % 7 == 3 {
            peer.kill_lease();
            peer.resume_reading();
        }
        if i % 5 == 0 {
            std::thread::sleep(Duration::from_millis(15));
        }
        for (coid, reason) in rejects(&lane.drain()) {
            assert!(rejected.insert(coid.clone(), reason).is_none(), "{coid} was rejected TWICE");
        }
    }
    peer.resume_reading();
    // Let the last redial settle and every verdict arrive.
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    std::thread::sleep(Duration::from_millis(400));
    for (coid, reason) in rejects(&lane.drain()) {
        assert!(rejected.insert(coid.clone(), reason).is_none(), "{coid} was rejected TWICE");
    }

    let seen = submits(&peer);
    for coid in &all {
        let sent = seen.contains(coid);
        let reason = rejected.get(coid);
        assert!(
            sent || reason.is_some(),
            "{coid} VANISHED: neither the peer saw it nor was it rejected"
        );
        assert!(
            !(sent && reason.is_some()),
            "{coid} was both sent and rejected (a double verdict): rejected with {reason:?}, \
             read by the peer at (conn, cseq) {:?}; opens (conn, client, resume, takeover): {:?}",
            read_at(&peer, coid),
            peer.opens()
        );
    }
}

/// Where the peer read `coid`'s submit, as `(conn, cseq)` pairs, for a failure message.
fn read_at(peer: &TradePeer, coid: &str) -> Vec<(usize, u64)> {
    peer.seen()
        .into_iter()
        .filter_map(|s| match s {
            Seen::Up { conn, frame: TradeUp::Submit { cseq, order, .. } }
                if order.client_order_id == coid =>
            {
                Some((conn, cseq))
            }
            Seen::Up { conn, frame: TradeUp::SubmitBatch { cseq, orders, .. } }
                if orders.iter().any(|o| o.client_order_id == coid) =>
            {
                Some((conn, cseq))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_number_that_cannot_cross_is_a_terminal_reject_and_the_lease_lives_on() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let mut exec = session.exec_client();
    let mut bad = order("nan-1");
    bad.price = Some(f64::NAN);
    exec.submit(&bad);
    exec.submit(&order("fine-2"));
    let got = lane.collect_until(WAIT, 1, |i| !rejects(std::slice::from_ref(i)).is_empty());
    let rejected = rejects(&got);
    assert_eq!(rejected.len(), 1, "{got:?}");
    assert_eq!(rejected[0].0, "nan-1");
    assert_eq!(rejected[0].1, vike_gatewayhub_client::trade::NON_FINITE_SUBMIT_REASON);
    let sent = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Submit { .. }));
    assert_eq!(sent.len(), 1, "only the finite order crossed");
    assert_eq!(
        session.link_state(),
        LinkState::Up,
        "a null on the wire would have ended the lease"
    );
}

fn adapter_start() -> vike_gatewayhub_client::proto::AdapterStart {
    vike_gatewayhub_client::proto::AdapterStart {
        rid: 0,
        symbol: "BTCUSDT".into(),
        declared_legs: Vec::new(),
        recon_enabled: true,
        takes_trigger: false,
        risk: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::Admit,
        withhold: None,
        route_key: "ignored".into(),
    }
}

fn cancel_coids(peer: &TradePeer) -> Vec<String> {
    peer.ups()
        .into_iter()
        .flat_map(|f| match f {
            TradeUp::Cancel { coid, .. } => vec![coid],
            TradeUp::CancelBatch { coids, .. } => coids,
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn held_risk_off_cancels_wait_for_the_adapter_after_a_gatewayhub_restart() {
    // W8b finding 4. A restarted gatewayhub has an empty table: a cancel flushed at the reconnect, before
    // the tradehub has started the adapter again, would land on a session with no adapter, be answered
    // with an advisory and be counted delivered. The orders rest at the venue (D6); the dead-man's cancel
    // must not be spent on nothing.
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    take_the_link_down(&peer, &session);
    let mut exec = session.exec_client();
    exec.cancel_with_intent("r1", CancelIntent::RiskOff);
    exec.cancel_with_intent("r2", CancelIntent::RiskOff);

    peer.restart(); // a new epoch while the link is down
    bring_the_link_back(&peer, &session, 2);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        cancel_coids(&peer).is_empty(),
        "no adapter yet on the new epoch: the held cancels are not spent on it: {:?}",
        cancel_coids(&peer)
    );
    assert!(cancel_advisories(&lane.drain()).is_empty(), "held is not shed");

    let reply = session.start_adapter(adapter_start()).expect("the adapter starts again");
    assert!(matches!(reply, vike_gatewayhub_client::proto::AdapterReply::Live { .. }));
    let flushed = wait_ups(&peer, WAIT, 2, |f| matches!(f, TradeUp::Cancel { .. }));
    let coids: Vec<&str> = flushed
        .iter()
        .filter_map(|f| match f {
            TradeUp::Cancel { coid, .. } => Some(coid.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(coids, ["r1", "r2"], "once the adapter answered, in the order held");
}

#[test]
fn held_cancels_still_flush_at_once_when_the_gatewayhub_did_not_restart() {
    // Same epoch: the adapter is alive, so waiting for one would only delay a risk-off cancel.
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    take_the_link_down(&peer, &session);
    let mut exec = session.exec_client();
    exec.cancel_with_intent("r1", CancelIntent::RiskOff);
    bring_the_link_back(&peer, &session, 2);
    let flushed = wait_ups(&peer, WAIT, 1, |f| matches!(f, TradeUp::Cancel { .. }));
    assert_eq!(flushed.len(), 1);
}
