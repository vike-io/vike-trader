#![cfg(feature = "trade")]
//! **The ring cursor: delivery with its position, resume on a flap, resume after a restart, and the
//! guards that keep an event from being folded twice or into the wrong account.**
//!
//! The tradehub journals each event with the cursor it arrived at (`Ingest::GatewayEvent`); this suite
//! holds what the client owes that journal: the cursor is the RING's account (the route key the session
//! names), the position is the one the daemon assigned, a replay is flagged and in order, an event naming
//! another account is never folded in, and a duplicate is delivered once.

use std::sync::mpsc;
use std::time::Duration;

use vike_exec::{GatewayCursor, Ingest};
use vike_gatewayhub_client::proto::{TradeDown, WireCursor};
use vike_gatewayhub_client::trade::LinkState;
use vike_model::events::Event;

#[allow(dead_code, clippy::allow_attributes)]
#[path = "support/fake_trade_peer.rs"]
mod fake_trade_peer;

use fake_trade_peer::{TradePeer, TradePeerConfig, accepted, open, open_with};

const WAIT: Duration = Duration::from_secs(5);

fn gateway_events(got: &[Ingest]) -> Vec<(u64, u64, String, Event)> {
    got.iter()
        .filter_map(|i| match i {
            Ingest::GatewayEvent(g) => {
                Some((g.cursor.gw_epoch, g.cursor.seq, g.route_key.to_string(), g.event.clone()))
            }
            _ => None,
        })
        .collect()
}

fn only_gateway_events(i: &Ingest) -> bool {
    matches!(i, Ingest::GatewayEvent(_))
}

#[test]
fn an_event_arrives_with_its_ring_cursor_and_the_accounts_route_key() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    for coid in ["a", "b", "c"] {
        peer.push_event(accepted(coid));
    }
    let got = lane.collect_until(WAIT, 3, only_gateway_events);
    let events = gateway_events(&got);
    assert_eq!(events.len(), 3, "{got:?}");
    for (i, (epoch, seq, route_key, event)) in events.iter().enumerate() {
        assert_eq!(*epoch, peer.epoch());
        assert_eq!(*seq, i as u64 + 1, "the position the daemon assigned, +1 per event");
        assert_eq!(route_key, "binance", "the RING's account, in the core's spelling");
        assert!(matches!(event, Event::OrderAccepted(_)));
    }
    let cursor = session.cursor().expect("a delivered cursor is held");
    assert_eq!((cursor.gw_epoch, cursor.seq), (peer.epoch(), 3));
    assert_eq!(session.stats().events_delivered, 3);
    assert_eq!(session.stats().events_replayed, 0);
    assert!(
        got.iter().all(|i| !matches!(i, Ingest::Event(_))),
        "a ring event is journaled INSTEAD of a plain event, never beside it"
    );
}

#[test]
fn a_link_flap_resumes_from_the_last_delivered_position_and_replays_in_order_once() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    for coid in ["a", "b", "c"] {
        peer.push_event(accepted(coid));
    }
    let first = lane.collect_until(WAIT, 3, only_gateway_events);
    assert_eq!(first.len(), 3);

    peer.kill_lease();
    peer.push_event_offline(accepted("d")); // produced while the client was away
    peer.push_event_offline(accepted("e"));
    assert!(peer.wait_leases(2, WAIT));

    let opens = peer.opens();
    let resume = opens.last().expect("an open").2.expect("the redial carries a cursor");
    assert_eq!((resume.gw_epoch, resume.seq), (peer.epoch(), 3), "the last DELIVERED position");

    let replayed = lane.collect_until(WAIT, 2, only_gateway_events);
    let seqs: Vec<u64> = gateway_events(&replayed).iter().map(|e| e.1).collect();
    assert_eq!(seqs, [4, 5], "what was missed, in order, nothing older");
    assert_eq!(session.stats().events_replayed, 2);
    assert_eq!(session.stats().events_duplicate, 0);
    // A live event after the replay continues the sequence.
    peer.push_event(accepted("f"));
    let live = lane.collect_until(WAIT, 1, only_gateway_events);
    assert_eq!(gateway_events(&live)[0].1, 6);
}

#[test]
fn a_duplicate_position_is_delivered_once() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let seq = peer.push_event(accepted("a"));
    let again = TradeDown::Event {
        account: "binance".into(),
        gw_epoch: peer.epoch(),
        seq,
        ts_ms: 10,
        replayed: true,
        event: accepted("a"),
    };
    assert!(peer.send(&again));
    assert!(peer.send(&again));
    peer.push_event(accepted("b"));
    let got = lane.collect_until(WAIT, 2, only_gateway_events);
    let seqs: Vec<u64> = gateway_events(&got).iter().map(|e| e.1).collect();
    assert_eq!(seqs, [1, 2], "the repeated position was dropped, the next one delivered");
    assert!(TradePeer::wait_until(WAIT, || session.stats().events_duplicate == 2));
}

#[test]
fn an_event_for_another_account_is_never_folded_in() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");
    let wrong = TradeDown::Event {
        account: "bybit".into(),
        gw_epoch: peer.epoch(),
        seq: 1,
        ts_ms: 10,
        replayed: false,
        event: accepted("not-mine"),
    };
    assert!(peer.send(&wrong));
    peer.push_event(accepted("mine"));
    let got = lane.collect_until(WAIT, 1, only_gateway_events);
    let events = gateway_events(&got);
    assert_eq!(events.len(), 1);
    let Event::OrderAccepted(a) = &events[0].3 else { panic!("{:?}", events[0].3) };
    assert_eq!(a.client_order_id, "mine");
    assert_eq!(session.stats().events_misrouted, 1);
}

#[test]
fn a_process_restart_resumes_from_the_journals_cursor() {
    let peer = TradePeer::start(TradePeerConfig::new());
    for coid in ["a", "b", "c", "d"] {
        peer.push_event_offline(accepted(coid));
    }
    // The journal's last cursor for this account: seq 2 of the live epoch.
    let journal = GatewayCursor { gw_epoch: peer.epoch(), seq: 2, ts_ms: 20 };
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", |link| link.with_resume(Some(journal)));
    let opens = peer.opens();
    assert_eq!(opens[0].2, Some(WireCursor { gw_epoch: journal.gw_epoch, seq: 2, ts_ms: 20 }));
    let got = lane.collect_until(WAIT, 2, only_gateway_events);
    let seqs: Vec<u64> = gateway_events(&got).iter().map(|e| e.1).collect();
    assert_eq!(seqs, [3, 4], "events strictly AFTER the journaled cursor");
    assert_eq!(session.stats().events_replayed, 2);
}

#[test]
fn a_resume_the_ring_cannot_serve_triggers_a_reconcile_and_a_new_epoch_delivers_anew() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    peer.push_event(accepted("a"));
    let _ = lane.collect_until(WAIT, 1, only_gateway_events);

    peer.restart(); // the ring is gone; the client's resume names the old epoch
    assert!(peer.wait_leases(2, WAIT));
    assert!(
        trigger_rx.recv_timeout(WAIT).is_ok(),
        "ReplayUnavailable must request a reconcile: that is how the missed fills are found"
    );
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));

    // The new epoch's ring starts again at 1 and is delivered whatever the old position was.
    peer.push_event(accepted("b"));
    let got = lane.collect_until(WAIT, 1, only_gateway_events);
    let events = gateway_events(&got);
    assert_eq!((events[0].0, events[0].1), (peer.epoch(), 1));
    let cursor = session.cursor().expect("held");
    assert_eq!(
        (cursor.gw_epoch, cursor.seq),
        (peer.epoch(), 1),
        "the new epoch is the resume point"
    );
}

#[test]
fn a_reconcile_request_from_the_daemon_reaches_the_trigger() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (_session, _lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    assert!(peer.send(&TradeDown::ReconcileNow { reason: "the bridge asked".into() }));
    assert!(trigger_rx.recv_timeout(WAIT).is_ok());
}

#[test]
fn a_core_that_has_gone_closes_the_lease_instead_of_redialling_for_ever() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, lane) = open(&peer, "tradehub-1");
    drop(lane); // the core exits: its ingest receiver is gone
    peer.push_event(accepted("a"));
    assert!(
        TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Closed),
        "state: {:?}",
        session.link_state()
    );
    let why = session.closed_reason().expect("a reason");
    assert!(why.contains("core"), "{why}");
    let leases = peer.leases_opened();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(peer.leases_opened(), leases, "no redial storm against a process that is leaving");
}

fn event_at(peer: &TradePeer, seq: u64, coid: &str) -> TradeDown {
    TradeDown::Event {
        account: "binance".into(),
        gw_epoch: peer.epoch(),
        seq,
        ts_ms: i64::try_from(seq).unwrap_or(0) * 10,
        replayed: false,
        event: accepted(coid),
    }
}

#[test]
fn a_seq_jump_nobody_announced_triggers_a_reconcile_and_the_event_is_still_delivered() {
    // W8b finding 5: a frame the daemon could not write used to leave a hole in `seq` that nothing
    // noticed. The ring is contiguous, so a jump inside one epoch with no `ReplayUnavailable` before it
    // is a lost event.
    let peer = TradePeer::start(TradePeerConfig::new());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    peer.push_event(accepted("a")); // seq 1
    let _ = lane.collect_until(WAIT, 1, only_gateway_events);
    assert!(trigger_rx.try_recv().is_err(), "a contiguous stream asks for nothing");

    assert!(peer.send(&event_at(&peer, 3, "c"))); // seq 2 never arrived
    assert!(trigger_rx.recv_timeout(WAIT).is_ok(), "the hole must request a reconcile");
    let got = lane.collect_until(WAIT, 1, only_gateway_events);
    assert_eq!(gateway_events(&got)[0].1, 3, "the event that did arrive is delivered");
    assert_eq!(session.cursor().map(|c| c.seq), Some(3));
}

#[test]
fn a_seq_jump_the_daemon_announced_with_replay_unavailable_asks_for_one_reconcile_only() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (_session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    peer.push_event(accepted("a"));
    let _ = lane.collect_until(WAIT, 1, only_gateway_events);

    assert!(peer.send(&TradeDown::ReplayUnavailable { floor: 3 }));
    assert!(peer.send(&event_at(&peer, 3, "c")));
    assert!(trigger_rx.recv_timeout(WAIT).is_ok(), "the announcement itself reconciles");
    let _ = lane.collect_until(WAIT, 1, only_gateway_events);
    std::thread::sleep(Duration::from_millis(300));
    assert!(trigger_rx.try_recv().is_err(), "an announced jump is not asked about twice");

    // And the allowance is spent: a later UNannounced hole still asks.
    assert!(peer.send(&event_at(&peer, 7, "g")));
    assert!(trigger_rx.recv_timeout(WAIT).is_ok());
}

// ---- a client that has heard NO event yet (W19-D3) ---------------------------------------------------

#[test]
fn a_flap_before_the_first_event_resumes_from_the_ring_start_and_replays_everything() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    assert_eq!(session.cursor(), None, "nothing delivered yet");

    peer.kill_lease();
    peer.push_event_offline(accepted("a")); // produced while the client was away, the first it could hear
    peer.push_event_offline(accepted("b"));
    assert!(peer.wait_leases(2, WAIT));

    let opens = peer.opens();
    let resume = opens.last().expect("an open").2.expect("the redial carries the ring start");
    assert_eq!(
        (resume.gw_epoch, resume.seq),
        (peer.epoch(), 0),
        "a client that heard nothing resumes from the start of the epoch it opened"
    );
    let got = lane.collect_until(WAIT, 2, only_gateway_events);
    let seqs: Vec<u64> = gateway_events(&got).iter().map(|e| e.1).collect();
    assert_eq!(seqs, [1, 2], "the whole ring, in order");
    assert_eq!(session.stats().events_replayed, 2);
    assert!(trigger_rx.try_recv().is_err(), "a replay that serves the gap needs no reconcile");
    peer.push_event(accepted("c"));
    let live = lane.collect_until(WAIT, 1, only_gateway_events);
    assert_eq!(gateway_events(&live)[0].1, 3);
}

#[test]
fn the_ring_start_follows_a_gatewayhub_restart_so_a_second_flap_before_any_event_still_replays() {
    let peer = TradePeer::start(TradePeerConfig::new());
    let (session, mut lane) = open(&peer, "tradehub-1");

    peer.restart(); // a new epoch, still nothing heard
    assert!(peer.wait_leases(2, WAIT));
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    let first_epoch = peer.epoch();

    peer.kill_lease();
    peer.push_event_offline(accepted("a"));
    assert!(peer.wait_leases(3, WAIT));
    let resume = peer.opens().last().expect("an open").2.expect("a cursor");
    assert_eq!((resume.gw_epoch, resume.seq), (first_epoch, 0), "the NEW epoch's ring start");
    let got = lane.collect_until(WAIT, 1, only_gateway_events);
    assert_eq!(gateway_events(&got)[0].1, 1);
}

#[test]
fn a_ring_that_was_not_empty_at_the_first_open_has_no_position_to_replay_so_a_flap_reconciles() {
    let peer = TradePeer::start(TradePeerConfig::new());
    peer.push_event_offline(accepted("before")); // the session's history, before this client attached
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    assert!(
        trigger_rx.try_recv().is_err(),
        "the first open owes nothing: the boot reconcile covers it"
    );

    peer.kill_lease();
    peer.push_event_offline(accepted("during"));
    assert!(peer.wait_leases(2, WAIT));
    assert_eq!(peer.opens().last().expect("an open").2, None, "no position the client could name");
    assert!(
        trigger_rx.recv_timeout(WAIT).is_ok(),
        "events may have been missed that no cursor names: reconcile finds them"
    );
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    assert_eq!(
        gateway_events(&lane.drain()).len(),
        0,
        "and nothing is replayed from before this client"
    );
}

#[test]
fn a_daemon_that_does_not_say_what_its_ring_holds_is_never_an_anchor_so_a_flap_reconciles() {
    // W19g: an older daemon omits `TradeOpened.event_ring_floor`. The client read it as 0 = "empty ring" and
    // anchored on a ring that may have been full, replaying history it had not seen. Unknown is a gap.
    let peer = TradePeer::start(TradePeerConfig::new().without_ring_floor());
    let (trigger_tx, trigger_rx) = mpsc::channel();
    let (session, mut lane) =
        open_with(&peer, "tradehub-1", move |link| link.with_trigger(trigger_tx));
    assert_eq!(session.opened().expect("an open reply").event_ring_floor, None);
    assert!(
        trigger_rx.try_recv().is_err(),
        "the first open owes nothing: the boot reconcile covers it"
    );

    peer.kill_lease();
    peer.push_event_offline(accepted("during"));
    assert!(peer.wait_leases(2, WAIT));
    assert_eq!(
        peer.opens().last().expect("an open").2,
        None,
        "no anchor: the ring's start is not known"
    );
    assert!(
        trigger_rx.recv_timeout(WAIT).is_ok(),
        "events may have been missed that no cursor names: reconcile finds them"
    );
    assert!(TradePeer::wait_until(WAIT, || session.link_state() == LinkState::Up));
    assert_eq!(
        gateway_events(&lane.drain()).len(),
        0,
        "nothing is replayed from a position never held"
    );
}

#[test]
fn the_ring_floor_is_additive_on_the_wire_absent_means_unknown_and_zero_means_empty() {
    use vike_gatewayhub_client::proto::TradeOpened;
    let old = r#"{"gw_epoch":1,"lease_epoch":2,"received_through_cseq":3,"project_token":"","heartbeat_ms":2000}"#;
    let opened: TradeOpened =
        serde_json::from_str(old).expect("an older daemon's reply still decodes");
    assert_eq!(opened.event_ring_floor, None, "absent is unknown, not 0 = empty");
    assert!(
        !serde_json::to_string(&opened).unwrap().contains("event_ring_floor"),
        "and unknown is not sent as a value"
    );
    let empty = TradeOpened { event_ring_floor: Some(0), ..opened };
    assert!(serde_json::to_string(&empty).unwrap().contains(r#""event_ring_floor":0"#));
}
