//! **Orders across the hop** (B.7 of the wave-2 plan), against the real server and the real client: every
//! command reaches the adapter in order and is acknowledged exactly once, a duplicate order client id is not sent
//! twice, the HALT sentinel is honoured at the daemon's own seam (cancel never gated, a reducing submit
//! admitted), an order that cannot reach an adapter becomes a terminal reject and never vanishes, events
//! come back with their ring cursor in order, and the query-only requests place nothing.
#![cfg(feature = "trade")]

use std::time::{Duration, Instant};

use vike_execution::halt::HALT_REJECT_REASON;
use vike_execution::{ExecutionClient, Ingest, RestoredOrder};
use vike_gatewayhub::testkit::gateway::{TradeKit, trade_keys};
use vike_gatewayhub::testkit::trade::{ExecutionCall, TradeCall, TradeLaneScript};
use vike_gatewayhub_client::proto::{
    AdapterReply, OrderResolution, Request, ResolveState, Response, TradeDown, TradeUp,
};
use vike_hub_protocol::auth::Scope;
use vike_model::AccountTier;
use vike_model::OrderCancelIntent;

use vike_gatewayhub::testkit::{raw_peer, trade_rig};

use raw_peer::{RAW_TIMEOUT, Raw};
use trade_rig::{
    Rig, accepted, cancel_advisories, gateway_order_client_ids, modify_advisories, open, order,
    quick_config, reducing_order, rejects, rig, rig_with, start_args,
};
use vike_gatewayhub_client::test_support::fake_trade_peer::Lane;
use vike_log::poll::wait_until;

const WAIT: Duration = Duration::from_secs(5);

/// Open a lease and start the adapter; the core lane, the session and the scripted adapter's handle.
fn up(
    rig: &Rig,
) -> (
    vike_gatewayhub_client::trade::TradeLease,
    Lane,
    vike_gatewayhub::testkit::trade::ScriptedAdapter,
) {
    let (session, lane) = open(rig, "tradehub-1");
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Running { .. }
    ));
    let adapter = rig.lane.adapter("binance", None).expect("the adapter was started");
    (session, lane, adapter)
}

fn raw_lease(rig: &Rig, client_id: &str) -> Raw {
    let mut raw = Raw::open(rig.gw.addr(), &trade_keys(), Scope::Trade);
    raw.send(&Request::TradeOpen {
        client_id: client_id.to_string(),
        venue: "binance".to_string(),
        account: None,
        tier: AccountTier::Demo,
        resume: None,
        takeover: false,
        row: None,
    });
    match raw.recv::<Response>() {
        Response::TradeOpened(_) => {}
        other => panic!("{other:?}"),
    }
    raw
}

fn start_frame(rid: u64) -> TradeUp {
    let mut start = start_args();
    start.rid = rid;
    start.route_key = "binance".into();
    TradeUp::AdapterStart(start)
}

/// The next non-heartbeat frame.
fn next_down(raw: &mut Raw) -> TradeDown {
    crate::next_down(raw, RAW_TIMEOUT, |f| matches!(f, TradeDown::Heartbeat))
}

#[test]
fn every_call_reaches_the_adapter_in_order_with_its_intent() {
    let rig = rig();
    let (session, _lane, adapter) = up(&rig);
    let mut exec = session.execution_client();
    exec.submit(&order("c1"));
    exec.cancel("c1");
    exec.cancel_with_intent("c2", OrderCancelIntent::RiskOff);
    exec.cancel_batch_with_intent(
        &["c3".to_string(), "c4".to_string()],
        OrderCancelIntent::Routine,
    );
    exec.modify(&order("c5"), Some(2.0), None);
    exec.confirm("c6");
    // One restored order with the venue order id the node holds, one with none: both cross as they were sent.
    let taught = vec![
        RestoredOrder { venue_order_id: Some("v7".into()), ..RestoredOrder::new("c7", "BTCUSDT") },
        RestoredOrder::new("c8", "ETHUSDT"),
    ];
    exec.adopt_restored(&taught);
    exec.submit_batch(&[order("b1"), order("b2")]);
    assert!(wait_until(WAIT, || adapter.calls().len() >= 8), "{:?}", adapter.calls());
    let calls = adapter.calls();
    assert_eq!(calls[0], ExecutionCall::Submit(order("c1")));
    assert_eq!(
        calls[1],
        ExecutionCall::Cancel {
            order_client_id: "c1".into(),
            intent: OrderCancelIntent::Unspecified
        }
    );
    assert_eq!(
        calls[2],
        ExecutionCall::Cancel { order_client_id: "c2".into(), intent: OrderCancelIntent::RiskOff }
    );
    assert_eq!(
        calls[3],
        ExecutionCall::CancelBatch {
            order_client_ids: vec!["c3".into(), "c4".into()],
            intent: OrderCancelIntent::Routine
        }
    );
    assert_eq!(
        calls[4],
        ExecutionCall::Modify { order: order("c5"), new_qty: Some(2.0), new_price: None }
    );
    assert_eq!(calls[5], ExecutionCall::Confirm("c6".into()));
    assert_eq!(calls[6], ExecutionCall::AdoptRestored(taught));
    assert_eq!(calls[7], ExecutionCall::SubmitBatch(vec![order("b1"), order("b2")]));
}

/// A trigger amend crosses the hub to the adapter's `modify_trigger`; an adapter that never heard of one
/// refuses it in words, and the hub tells the node so as the non-terminal advisory. `modify` is never
/// called with the limit alone.
#[test]
fn a_trigger_amend_the_adapter_cannot_move_comes_back_as_an_advisory_in_words() {
    use vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON;
    let rig = rig();
    let (session, mut lane, adapter) = up(&rig);
    let mut exec = session.execution_client();
    assert_eq!(exec.modify_trigger(&order("t1"), None, Some(99.0), 91.0), Ok(()));
    let seen = lane.collect_until(WAIT, 1, |i| i.venue_event().is_some());
    let said = modify_advisories(&seen);
    assert!(
        said.iter().any(|(c, why)| c == "t1" && why.contains(TRIGGER_AMEND_UNSUPPORTED_REASON)),
        "{seen:?}"
    );
    assert!(!adapter.calls().iter().any(|c| matches!(c, ExecutionCall::Modify { .. })));
}

#[test]
fn a_command_is_acknowledged_once_after_it_is_queued_and_a_duplicate_order_client_id_is_not_sent_twice()
 {
    let rig = rig();
    let mut raw = raw_lease(&rig, "tradehub-1");
    raw.send(&start_frame(1));
    assert!(matches!(next_down(&mut raw), TradeDown::AdapterReply { rid: 1, .. }));
    raw.send(&TradeUp::Submit { command_sequence: 1, order: order("c1") });
    raw.send(&TradeUp::Submit { command_sequence: 2, order: order("c1") });
    raw.send(&TradeUp::SubmitBatch { command_sequence: 3, orders: vec![order("c1"), order("c2")] });
    for want in 1..=3 {
        match next_down(&mut raw) {
            TradeDown::CommandAck { command_sequence: cseq } => {
                assert_eq!(cseq, want, "one ack per command, in order")
            }
            other => panic!("{other:?}"),
        }
    }
    let adapter = rig.lane.adapter("binance", None).expect("the adapter");
    assert!(wait_until(WAIT, || adapter.calls().len() >= 2), "{:?}", adapter.calls());
    assert_eq!(
        adapter.calls(),
        vec![
            ExecutionCall::Submit(order("c1")),
            // The batch carried c1 again (a duplicate) and c2: only c2 was sent.
            ExecutionCall::SubmitBatch(vec![order("c2")]),
        ]
    );
}

#[test]
fn the_halt_sentinel_refuses_an_opening_submit_admits_a_reducing_one_and_never_gates_a_cancel() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let halt = dir.path().join("HALT");
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]),
        quick_config(),
        TradeKit { state_dir: None, halt_path: Some(halt.clone()) },
    );
    let (session, mut lane, adapter) = up(&rig);
    let mut exec = session.execution_client();

    // No sentinel: an opening submit goes through.
    exec.submit(&order("free"));
    assert!(wait_until(WAIT, || adapter.submitted().len() == 1));

    std::fs::write(&halt, b"").expect("engage the sentinel");
    exec.submit(&order("open"));
    exec.submit(&reducing_order("reduce"));
    exec.submit_batch(&[order("open-b"), reducing_order("reduce-b")]);
    exec.modify(&order("free"), Some(3.0), None);
    exec.cancel("free");
    exec.cancel_batch_with_intent(&["x".to_string()], OrderCancelIntent::RiskOff);
    exec.confirm("free");
    assert!(
        wait_until(WAIT, || {
            let calls = adapter.calls();
            calls.iter().any(|c| matches!(c, ExecutionCall::Confirm(_)))
        }),
        "{:?}",
        adapter.calls()
    );

    // The adapter saw: the reducing orders, the cancels and the confirm; NOT the opening ones, NOT the modify.
    let sent: Vec<String> = adapter.submitted().iter().map(|o| o.order_client_id.clone()).collect();
    assert_eq!(sent, ["free", "reduce", "reduce-b"], "{sent:?}");
    let calls = adapter.calls();
    assert!(calls.iter().any(
        |c| matches!(c, ExecutionCall::Cancel { order_client_id, .. } if order_client_id == "free")
    ));
    assert!(
        calls.iter().any(|c| matches!(c, ExecutionCall::CancelBatch { .. })),
        "cancel is never gated"
    );
    assert!(
        !calls.iter().any(|c| matches!(c, ExecutionCall::Modify { .. })),
        "a modify is gated: {calls:?}"
    );

    // And the core was told, in the bridge's own words: a terminal reject for each refused submit, a
    // non-terminal advisory for the modify.
    let seen = lane.collect_until(WAIT, 3, |i| i.venue_event().is_some());
    let mut refused: Vec<(String, String)> = rejects(&seen);
    refused.sort();
    assert_eq!(
        refused,
        vec![
            ("open".to_string(), HALT_REJECT_REASON.to_string()),
            ("open-b".to_string(), HALT_REJECT_REASON.to_string()),
        ]
    );
    assert_eq!(
        modify_advisories(&seen),
        vec![("free".to_string(), HALT_REJECT_REASON.to_string())]
    );
}

#[test]
fn an_order_that_cannot_reach_an_adapter_is_rejected_terminally_and_never_vanishes() {
    let rig = rig();
    // A lease with NO adapter started.
    let (session, mut lane) = open(&rig, "tradehub-1");
    let mut exec = session.execution_client();
    exec.submit(&order("o1"));
    exec.submit_batch(&[order("o2"), order("o3")]);
    exec.cancel("o1");
    exec.cancel_batch_with_intent(&["o2".to_string()], OrderCancelIntent::Routine);
    exec.modify(&order("o3"), None, Some(9.0));
    let seen = lane.collect_until(WAIT, 7, |i| i.venue_event().is_some());
    let mut rejected: Vec<String> = rejects(&seen).into_iter().map(|(c, _)| c).collect();
    rejected.sort();
    assert_eq!(rejected, ["o1", "o2", "o3"], "every submit has its terminal event: {seen:?}");
    assert!(rejects(&seen).iter().all(|(_, reason)| reason.contains("no venue adapter")));
    let mut cancels: Vec<String> = cancel_advisories(&seen).into_iter().map(|(c, _)| c).collect();
    cancels.sort();
    assert_eq!(cancels, ["o1", "o2"], "a cancel that cannot be delivered is an advisory");
    assert_eq!(modify_advisories(&seen).len(), 1);
    assert_eq!(rig.lane.starts(), 0);
}

#[test]
fn events_the_adapter_produces_reach_the_core_with_their_ring_cursor_in_order() {
    let rig = rig();
    let (session, mut lane, adapter) = up(&rig);
    let epoch = session.opened().expect("opened").gateway_epoch;
    for n in 1..=5 {
        assert!(adapter.emit(accepted(&format!("c{n}"))));
    }
    let seen = lane.collect_until(WAIT, 5, |i| matches!(i, Ingest::GatewayEvent(_)));
    assert_eq!(
        gateway_order_client_ids(&seen),
        (1..=5).map(|n| (n, format!("c{n}"))).collect::<Vec<_>>(),
        "stamped 1..5 in the order the adapter pushed them"
    );
    for ingest in &seen {
        let Ingest::GatewayEvent(g) = ingest else { panic!("{ingest:?}") };
        assert_eq!(g.cursor.gateway_epoch, epoch);
    }
    let cursor = session.cursor().expect("the session keeps the last cursor");
    assert_eq!((cursor.gateway_epoch, cursor.sequence), (epoch, 5));
    let status = session.status().expect("status");
    assert_eq!((status.adapter_up, status.last_sequence, status.ring_floor), (true, 5, 1));
}

#[test]
fn every_order_class_command_is_counted_against_the_budget() {
    let rig = rig();
    let (session, _lane, adapter) = up(&rig);
    let mut exec = session.execution_client();
    exec.submit(&order("c1"));
    exec.submit(&order("c2"));
    exec.cancel("c1");
    exec.modify(&order("c2"), Some(2.0), None);
    assert!(wait_until(WAIT, || adapter.calls().len() >= 4));
    let budget = rig.gw.budget().expect("the trade gateway keeps its budget");
    let counted = |budget: &vike_gatewayhub::budget::ledger::Budget| -> u64 {
        let s = budget.snapshot(Instant::now());
        s.lines.iter().map(|l| l.order_used + l.shared_used).sum::<u64>() + s.unmetered
    };
    assert!(
        counted(budget) >= 4,
        "four order-class units were spent, the ledger saw {}",
        counted(budget)
    );
}

#[test]
fn wire_ids_and_resolve_orders_place_nothing() {
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]).with_resolver("binance"),
        quick_config(),
        TradeKit::default(),
    );
    let (session, _lane, adapter) = up(&rig);
    adapter.set_wire_id("hl-1", "0xhl-1");
    session.prefetch_wire_ids(&["hl-1".to_string(), "bn-2".to_string()]).expect("prefetched");
    let exec = session.execution_client();
    assert_eq!(exec.wire_id_for("hl-1").as_deref(), Some("0xhl-1"));
    assert_eq!(exec.wire_id_for("bn-2"), None, "the venue echoes the order client id: left out");

    let mut exec = session.execution_client();
    exec.submit(&order("t1"));
    assert!(wait_until(WAIT, || adapter.submitted().len() == 1));
    let rows = session.resolve_orders(&["t1".to_string(), "u2".to_string()]).expect("resolved");
    assert_eq!(
        rows,
        vec![
            OrderResolution { order_client_id: "t1".into(), state: ResolveState::Tracked },
            OrderResolution { order_client_id: "u2".into(), state: ResolveState::Querying },
        ]
    );
    let calls = adapter.calls();
    assert!(
        calls.contains(&ExecutionCall::Confirm("u2".into())),
        "the re-query was launched: {calls:?}"
    );
    assert!(
        !calls.contains(&ExecutionCall::Confirm("t1".into())),
        "a tracked order is not re-queried"
    );
    assert_eq!(adapter.submitted().len(), 1, "a resolution never sends an order");
}

#[test]
fn a_venue_with_no_lookup_by_order_client_id_says_no_resolver_and_asks_nothing() {
    let rig = rig();
    let (session, _lane, adapter) = up(&rig);
    let rows = session.resolve_orders(&["u1".to_string()]).expect("resolved");
    assert_eq!(
        rows,
        vec![OrderResolution { order_client_id: "u1".into(), state: ResolveState::NoResolver }]
    );
    assert!(!adapter.calls().iter().any(|c| matches!(c, ExecutionCall::Confirm(_))));
}

#[test]
fn the_identity_request_reaches_the_lane_with_the_reconcile_handle_and_a_failure_is_a_warning() {
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]).with_identity_failure("the venue could not say"),
        quick_config(),
        TradeKit::default(),
    );
    let (session, _lane, _adapter) = up(&rig);
    let err = session.record_identity().expect_err("bookkeeping failed");
    assert!(err.text().contains("the venue could not say"), "{err}");
    assert!(rig.lane.calls().iter().any(|c| matches!(
        c,
        TradeCall::RecordIdentity { had_reconcile: true, bound_tier: AccountTier::Demo, .. }
    )));
}

#[test]
fn a_restarted_tradehub_resuming_from_its_journal_cursor_hears_a_recreated_session_not_a_duplicate_guard()
 {
    // W8b finding 1, end to end. The tradehub stops cleanly (final detach: the daemon retires the
    // session) and restarts with the journal's cursor `(E, 3)`. The new session's ring starts at 1; the
    // client's duplicate guard must not read `(E, 1..3)` as the old events again.
    let rig = rig();
    let (first, mut lane1, adapter) = up(&rig);
    for n in 1..=3 {
        assert!(adapter.emit(accepted(&format!("old{n}"))));
    }
    let seen = lane1.collect_until(WAIT, 3, |i| matches!(i, Ingest::GatewayEvent(_)));
    assert_eq!(seen.len(), 3);
    let journal_cursor = first.cursor().expect("three events were delivered");
    assert_eq!(journal_cursor.sequence, 3);
    first.detach();
    assert!(wait_until(WAIT, || adapter.is_detached()), "the final detach tore the adapter down");

    let (second, mut lane2) =
        trade_rig::open_with(&rig, "tradehub-1", |link| link.with_resume(Some(journal_cursor)));
    assert!(matches!(
        second.start_adapter(start_args()).expect("answered"),
        AdapterReply::Running { .. }
    ));
    assert_ne!(
        second.opened().expect("opened").gateway_epoch,
        journal_cursor.gateway_epoch,
        "the retired session's ring is gone and the epoch says so"
    );
    let fresh = rig.lane.adapter("binance", None).expect("the new adapter");
    assert!(fresh.emit(accepted("new1")));
    let seen = lane2.collect_until(WAIT, 1, |i| matches!(i, Ingest::GatewayEvent(_)));
    assert_eq!(
        gateway_order_client_ids(&seen),
        vec![(1, "new1".to_string())],
        "the new session's first event is delivered, not counted as a duplicate"
    );
    assert_eq!(second.stats().events_duplicate, 0);
}

#[test]
fn a_bridge_panic_ends_the_adapter_visibly_every_queued_command_gets_its_event_and_a_start_restarts_it()
 {
    // W8b finding 3. A panic in the bridge's `submit` ends the dispatch thread. The commands already
    // acknowledged and queued behind it used to vanish with the channel, and the next identical
    // `AdapterStart` was answered from the dead adapter.
    let rig = rig();
    let (session, mut lane, adapter) = up(&rig);
    adapter.on_submit(|order, _| {
        if order.order_client_id == "boom" {
            // Long enough for the next commands to be read, acknowledged and queued behind this one.
            std::thread::sleep(Duration::from_millis(300));
            panic!("a bridge bug while submitting");
        }
    });
    let mut exec = session.execution_client();
    exec.submit(&order("boom"));
    exec.submit(&order("queued-1"));
    exec.cancel("queued-k");
    exec.modify(&order("queued-m"), Some(2.0), None);

    let seen = lane.collect_until(WAIT, 4, |i| i.venue_event().is_some());
    let mut rejected: Vec<String> = rejects(&seen).into_iter().map(|(c, _)| c).collect();
    rejected.sort();
    assert_eq!(
        rejected,
        ["boom", "queued-1"],
        "the in-flight and the queued submit each get their terminal event: {seen:?}"
    );
    assert_eq!(cancel_advisories(&seen).len(), 1, "the queued cancel is announced: {seen:?}");
    assert_eq!(modify_advisories(&seen).len(), 1, "the queued modify is announced: {seen:?}");

    // The daemon says so, and a start is a fresh adapter, not the dead one's reply.
    assert!(
        wait_until(WAIT, || session.status().is_ok_and(|s| !s.adapter_up)),
        "the status must report the adapter dead"
    );
    assert!(matches!(
        session.start_adapter(start_args()).expect("answered"),
        AdapterReply::Running { .. }
    ));
    assert_eq!(rig.lane.starts(), 2, "the same start restarted a dead adapter");
    assert!(session.status().expect("status").adapter_up);
    let fresh = rig.lane.adapter("binance", None).expect("the new adapter");
    exec.submit(&order("after"));
    assert!(wait_until(WAIT, || fresh.submitted().iter().any(|o| o.order_client_id == "after")));
    assert!(rejects(&lane.drain()).is_empty(), "the new adapter takes orders again");
}

#[test]
fn an_order_client_id_the_halt_gate_refused_is_sent_when_the_core_retries_it_after_the_halt_clears()
{
    // W8b finding 6: the dedup set marked an order client id before the HALT decision, so the same order client id retried once
    // HALT was gone was acknowledged and silently never sent.
    let dir = tempfile::tempdir().expect("a temp dir");
    let halt = dir.path().join("HALT");
    let rig = rig_with(
        TradeLaneScript::new(&["binance"]),
        quick_config(),
        TradeKit { state_dir: None, halt_path: Some(halt.clone()) },
    );
    let (session, mut lane, adapter) = up(&rig);
    let mut exec = session.execution_client();

    std::fs::write(&halt, b"").expect("engage the sentinel");
    exec.submit(&order("retry-me"));
    let seen = lane.collect_until(WAIT, 1, |i| i.venue_event().is_some());
    assert_eq!(rejects(&seen).len(), 1, "refused under HALT: {seen:?}");
    assert!(adapter.submitted().is_empty());

    std::fs::remove_file(&halt).expect("clear the sentinel");
    exec.submit(&order("retry-me"));
    assert!(
        wait_until(WAIT, || adapter.submitted().iter().any(|o| o.order_client_id == "retry-me")),
        "the retry of a refused order is a new decision and reaches the adapter"
    );

    // An order that WAS sent is still a duplicate on a second sight.
    exec.submit(&order("retry-me"));
    exec.submit(&order("barrier"));
    assert!(wait_until(WAIT, || adapter
        .submitted()
        .iter()
        .any(|o| o.order_client_id == "barrier")));
    let sent = adapter.submitted().iter().filter(|o| o.order_client_id == "retry-me").count();
    assert_eq!(sent, 1, "and an order client id sent once is sent once");
}

#[test]
fn an_order_naming_another_venue_or_account_is_rejected_at_the_daemon_not_sent_to_the_adapter() {
    // W8b finding 6: `check_order` bounded sizes only, so a frame for account B on A's lease reached A's
    // adapter. One order that names a conflicting identity is refused; the lease is not ended for it.
    let rig = rig();
    let (session, mut lane, adapter) = up(&rig);
    let mut exec = session.execution_client();
    let mut other_venue = order("other-venue");
    other_venue.venue = "bybit".into();
    let mut other_account = order("other-account");
    other_account.account =
        Some(vike_model::accounts::account_keys::AccountLabel::Named("ALT".into()));
    let mut default_account = order("default-account");
    default_account.account = Some(vike_model::accounts::account_keys::AccountLabel::Default);
    exec.submit(&other_venue);
    exec.submit(&other_account);
    exec.submit(&default_account); // the lease IS the default account: this one is fine
    exec.submit(&order("plain")); // naming no account is fine
    let seen = lane.collect_until(WAIT, 2, |i| i.venue_event().is_some());
    let mut refused: Vec<String> = rejects(&seen).into_iter().map(|(c, _)| c).collect();
    refused.sort();
    assert_eq!(refused, ["other-account", "other-venue"], "{seen:?}");
    assert!(wait_until(WAIT, || adapter.submitted().len() == 2));
    let sent: Vec<String> = adapter.submitted().iter().map(|o| o.order_client_id.clone()).collect();
    assert_eq!(sent, ["default-account", "plain"], "only the well-addressed orders reached it");
    assert!(
        rejects(&seen)[0].1.contains("another"),
        "the reason names the problem: {:?}",
        rejects(&seen)
    );
}
