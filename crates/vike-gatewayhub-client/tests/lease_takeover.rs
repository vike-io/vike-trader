#![cfg(feature = "trade")]
//! **One trade client per `(venue, account)`: the lease, its refusal, its explicit takeover, and what a
//! displaced holder does.**
//!
//! A second tradehub on the same account is refused with the holder's KEY ID (never a key); taking the
//! lease over is explicit and fenced; the same client reconnecting is NOT a takeover; and a holder that
//! has been displaced (or finds the lease held, or refused, at a redial) CLOSES for good rather than
//! fighting: two writers on one account is the failure the lease exists to prevent. A closed lease keeps
//! disposing of what it is handed (a reject for an order, an advisory for a held cancel) so nothing
//! vanishes while the operator reads the log.

use std::time::Duration;

use vike_exec::{CancelIntent, ExecutionClient};
use vike_gatewayhub_client::client::TradeOpenError;
use vike_gatewayhub_client::proto::{Response, TradeBye, TradeDown};
use vike_gatewayhub_client::trade::{
    LEASE_CLOSED_CANCEL_REASON, LEASE_CLOSED_SUBMIT_REASON, LinkState, TradeLink,
};

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{
    TradePeer, TradePeerConfig, account, cancel_advisories, fast_trade_timings, lane, open,
    open_with, order, rejects,
};

const WAIT: Duration = Duration::from_secs(5);

fn trade_key_id() -> String {
    fake_trade_peer::trade_keys()
        .key_id(vike_hub_proto::auth::Scope::Trade)
        .expect("the test keys carry a trade key")
}

#[test]
fn a_second_client_is_refused_with_the_holders_key_id_and_the_holder_is_untouched() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (holder, _lane) = open(&peer, "tradehub-1");
    let (events, _lane2) = lane();
    let err = TradeLink::new(peer.link(), account("tradehub-2"), events)
        .with_timings(fast_trade_timings())
        .open()
        .expect_err("the lease is held");
    match err {
        TradeOpenError::LeaseHeld { holder_key_id, .. } => {
            assert_eq!(holder_key_id, trade_key_id());
            assert!(holder_key_id.starts_with("nk-"), "a key id, never a key");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(holder.link_state(), LinkState::Up);
    assert_eq!(peer.leases_opened(), 1);
}

#[test]
fn a_takeover_is_explicit_fenced_and_the_displaced_holder_closes_for_good() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (old, mut old_lane) = open(&peer, "tradehub-1");
    let (new, _new_lane) = open_with(&peer, "tradehub-2", |link| link.with_takeover(true));
    assert_eq!(peer.opens().last().map(|o| o.3), Some(true), "the open said so");
    assert_eq!(new.link_state(), LinkState::Up);

    assert!(
        TradePeer::wait_until(WAIT, || old.link_state() == LinkState::Closed),
        "the displaced holder must close, state {:?}",
        old.link_state()
    );
    let why = old.closed_reason().expect("a reason");
    assert!(why.contains("taken over") && why.contains("nk-"), "{why}");
    let leases = peer.leases_opened();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(peer.leases_opened(), leases, "the old holder does not fight back");

    // Its orders still get a verdict: a terminal reject naming the closed lease, never silence.
    let mut exec = old.exec_client();
    exec.submit(&order("after-takeover"));
    let got = old_lane.collect_until(WAIT, 1, |_| true);
    let rejected = rejects(&got);
    assert_eq!(rejected.len(), 1, "{got:?}");
    assert_eq!(rejected[0].1, LEASE_CLOSED_SUBMIT_REASON);
}

#[test]
fn the_same_client_reconnecting_is_not_a_takeover() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    peer.kill_lease();
    assert!(peer.wait_leases(2, WAIT));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    let opens = peer.opens();
    assert_eq!(opens.len(), 2);
    assert!(
        opens.iter().all(|o| o.1 == "tradehub-1" && !o.3),
        "same client, no takeover: {opens:?}"
    );
}

#[test]
fn a_redial_that_finds_the_lease_held_closes_for_good_and_gives_a_held_cancel_its_last_word() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    peer.refuse_open(Some(Response::Error("down for the test".into())));
    peer.kill_lease();
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Down));
    let mut exec = session.exec_client();
    exec.cancel_with_intent("held-1", CancelIntent::RiskOff); // held for the next link
    assert!(cancel_advisories(&lane.drain()).is_empty());

    // Another client took it while we were away.
    peer.refuse_open(Some(Response::LeaseHeld {
        holder_key_id: "nk-someoneelse".into(),
        since_ms: 1,
    }));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    assert!(session.closed_reason().expect("why").contains("nk-someoneelse"));
    let got = lane.collect_until(WAIT, 1, |_| true);
    assert_eq!(
        cancel_advisories(&got),
        [("held-1".to_string(), LEASE_CLOSED_CANCEL_REASON.to_string())],
        "the held cancel never vanished: it was told the lease is over"
    );
}

#[test]
fn a_redial_the_daemon_refuses_for_the_accounts_own_state_also_closes_for_good() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    peer.refuse_open(Some(Response::TradeRefused {
        block_word: "tier_conflict".into(),
        text: "the account now runs at live".into(),
    }));
    peer.kill_lease();
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    let why = session.closed_reason().expect("why");
    assert!(why.contains("tier_conflict") && why.contains("now runs at live"), "{why}");
}

/// Submit three orders the peer reads and never acknowledges: Written, UNKNOWN to the client.
fn three_written_unacked(peer: &TradePeer, session: &vike_gatewayhub_client::trade::TradeSession) {
    let mut exec = session.exec_client();
    exec.submit(&order("w1"));
    exec.submit_batch(&[order("w2"), order("w3")]);
    assert!(TradePeer::wait_until(WAIT, || peer.received_through("tradehub-1") >= 2));
}

#[test]
fn a_submit_written_and_unacknowledged_when_the_lease_is_taken_over_stays_unknown_and_is_named() {
    // W8b finding 2. The fence drains the old connection AFTER `Bye(Takeover)` is queued, so a frame
    // the daemon read during the drain was executed and its `CmdAck` dropped: the order may be live.
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    three_written_unacked(&peer, &session);
    assert!(peer.send(&TradeDown::Bye(TradeBye::Takeover { by_key_id: "nk-other".into() })));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    let mut unknown = session.unknown_orders();
    unknown.sort();
    assert_eq!(
        unknown,
        ["w1", "w2", "w3"],
        "Written and unanswered: UNKNOWN, named, not forgotten"
    );
    std::thread::sleep(Duration::from_millis(250));
    let got = lane.drain();
    assert!(rejects(&got).is_empty(), "UNKNOWN is never a synthesized terminal: {got:?}");
}

#[test]
fn a_submit_written_when_the_daemon_ends_the_lease_for_a_protocol_violation_is_rejected_unprocessed()
 {
    // W8b finding 2. `Bye(Protocol)` is written after every ack of the frames the daemon processed and
    // the daemon stops reading at the violating frame: whatever is still unacknowledged was NOT read.
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    three_written_unacked(&peer, &session);
    assert!(peer.send(&TradeDown::Bye(TradeBye::Protocol("a frame it could not read".into()))));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    let got = lane.collect_until(WAIT, 3, |_| true);
    let mut rejected = rejects(&got);
    rejected.sort();
    assert_eq!(
        rejected,
        [
            ("w1".to_string(), LEASE_CLOSED_SUBMIT_REASON.to_string()),
            ("w2".to_string(), LEASE_CLOSED_SUBMIT_REASON.to_string()),
            ("w3".to_string(), LEASE_CLOSED_SUBMIT_REASON.to_string()),
        ],
        "provably unprocessed: each has its terminal event"
    );
    assert!(session.unknown_orders().is_empty(), "nothing is both rejected and unknown");
}

#[test]
fn a_submit_written_when_the_link_drops_and_the_redial_finds_the_lease_held_stays_unknown() {
    let peer = TradePeer::start(TradePeerConfig::new().never_ack());
    let (session, mut lane) = open(&peer, "tradehub-1");
    three_written_unacked(&peer, &session);
    peer.refuse_open(Some(Response::LeaseHeld {
        holder_key_id: "nk-someoneelse".into(),
        since_ms: 1,
    }));
    peer.kill_lease();
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed));
    let mut unknown = session.unknown_orders();
    unknown.sort();
    assert_eq!(unknown, ["w1", "w2", "w3"]);
    std::thread::sleep(Duration::from_millis(250));
    assert!(rejects(&lane.drain()).is_empty());
}

#[test]
fn a_refusal_at_a_redial_that_passes_does_not_close_the_lease_for_good() {
    // W8b finding 2: `account_tier` can refuse for a moment (a store read, a row being edited). Closing
    // for good on the first refusal strands the link until the tradehub restarts.
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    peer.refuse_next_opens(
        2,
        Response::TradeRefused {
            block_word: "no_account_row".into(),
            text: "the account table was being rewritten".into(),
        },
    );
    peer.kill_lease();
    assert!(
        TradePeer::wait_until(WAIT, || peer.leases_opened() >= 2
            && session.link_state() == LinkState::Up),
        "two refusals then an accept: the lease is back, state {:?} ({:?})",
        session.link_state(),
        session.closed_reason()
    );
}

#[test]
fn a_run_of_daemon_errors_at_a_redial_is_retried_uncounted_where_refusals_close_at_the_third() {
    // Review 15, finding 3: a gatewayhub whose settings store cannot be read answers a `TradeOpen` with a
    // plain error (not the account's refusal). More of them than the refusal budget (three) must NOT close
    // the lease: only definitive `TradeRefused` answers count. The same run of refusals does close it.
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-1");
    peer.refuse_next_opens(
        6,
        Response::Error("gatewayhub: the account table could not be read just now".into()),
    );
    peer.kill_lease();
    assert!(
        TradePeer::wait_until(WAIT, || peer.leases_opened() >= 2
            && session.link_state() == LinkState::Up),
        "six plain errors then an accept: the lease is back, state {:?} ({:?})",
        session.link_state(),
        session.closed_reason()
    );

    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, _lane) = open(&peer, "tradehub-2");
    peer.refuse_next_opens(
        6,
        Response::TradeRefused {
            block_word: "account_inactive".into(),
            text: "the account was deactivated".into(),
        },
    );
    peer.kill_lease();
    assert!(
        TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed),
        "three refusals in a row close it for good, state {:?}",
        session.link_state()
    );
}
