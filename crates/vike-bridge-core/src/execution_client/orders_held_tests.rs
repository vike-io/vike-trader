//! `ExecutionClientOrdersHeld`, door by door: every order and amend refused in its words with the held client
//! never asked, every other door handed to the held client with that client's answer.

use std::sync::{Arc, Mutex};

use super::*;
use crate::execution_drive::drain_ready;
use vike_execution::event_channel;
use vike_model::events::OrderCanceled;
use vike_venues::AmendSemantics;

const REASON: &str = "orders held: test";

/// The held client: names every door it is asked through, and answers the pure seams with values of its own.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Recorder {
    fn note(&self, door: impl Into<String>) {
        self.0.lock().unwrap().push(door.into());
    }
    fn doors(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl ExecutionClient for Recorder {
    fn submit(&mut self, request: &OrderRequest) {
        self.note(format!("submit {}", request.order_client_id));
    }
    fn cancel(&mut self, order_client_id: &str) {
        self.note(format!("cancel {order_client_id}"));
    }
    fn cancel_with_intent(&mut self, order_client_id: &str, intent: OrderCancelIntent) {
        self.note(format!("cancel_with_intent {order_client_id} {intent:?}"));
    }
    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        self.note(format!("cancel_batch {}", order_client_ids.join(",")));
    }
    fn cancel_batch_with_intent(&mut self, order_client_ids: &[String], intent: OrderCancelIntent) {
        self.note(format!("cancel_batch_with_intent {} {intent:?}", order_client_ids.join(",")));
    }
    fn modify(&mut self, order: &OrderRequest, _: Option<f64>, _: Option<f64>) {
        self.note(format!("modify {}", order.order_client_id));
    }
    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        _: Option<f64>,
        _: Option<f64>,
        _: f64,
    ) -> Result<(), &'static str> {
        self.note(format!("modify_trigger {}", order.order_client_id));
        Ok(())
    }
    fn confirm(&mut self, order_client_id: &str) {
        self.note(format!("confirm {order_client_id}"));
    }
    fn begin_detach(&mut self) {
        self.note("begin_detach");
    }
    fn detach(&mut self) {
        self.note("detach");
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.note("poll_events");
        Some(Event::OrderCanceled(OrderCanceled {
            order_client_id: "polled".to_string(),
            reason: String::new().into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        }))
    }
    fn on_bar(&mut self, bar: &vike_marketdata::Bar) {
        self.note(format!("on_bar {}", bar.timestamp));
    }
    fn on_bar_sides(
        &mut self,
        bar: &vike_marketdata::Bar,
        _sides: Option<&vike_marketdata::BarSides>,
    ) {
        self.note(format!("on_bar_sides {}", bar.timestamp));
    }
    fn amend_semantics(&self) -> Option<AmendSemantics> {
        Some(AmendSemantics::InPlaceRemaining)
    }
    fn wire_id_for(&self, order_client_id: &str) -> Option<String> {
        Some(format!("wire-{order_client_id}"))
    }
    fn adopt_restored(&mut self, orders: &[RestoredOrder]) {
        self.note(format!("adopt_restored {}", orders.len()));
    }
    fn on_contract_value(&mut self, symbol: &str, _value: vike_model::ContractValue) {
        self.note(format!("on_contract_value {symbol}"));
    }
    fn on_quote(&mut self, symbol: &str, _bid: f64, _ask: f64, timestamp: i64) {
        self.note(format!("on_quote {symbol} {timestamp}"));
    }
    fn fill_at_close(&mut self, order_client_ids: &[String], bar_width_ms: i64) {
        self.note(format!("fill_at_close {} {bar_width_ms}", order_client_ids.join(",")));
    }
    fn hold_for_close(&mut self, hold: bool) {
        self.note(format!("hold_for_close {hold}"));
    }
}

type Lane = tokio::sync::mpsc::Receiver<vike_execution::Ingest>;

fn held() -> (ExecutionClientOrdersHeld, Recorder, Lane) {
    let (events, lane) = event_channel(16);
    let inner = Recorder::default();
    let client =
        ExecutionClientOrdersHeld::new(Box::new(inner.clone()), events, "test", "acct-1", REASON);
    (client, inner, lane)
}

fn order(order_client_id: &str) -> OrderRequest {
    OrderRequest {
        timestamp: 7,
        ..crate::execution_drive::limit("test", order_client_id, "BTCUSDT", 0.01, 50_000.0)
    }
}

#[test]
fn every_submit_is_rejected_in_its_words_and_never_reaches_the_held_client() {
    let (mut client, inner, mut lane) = held();
    client.submit(&order("c1"));
    client.submit_batch(&[order("c2"), order("c3")]);
    let said: Vec<_> = drain_ready(|| lane.try_recv().ok())
        .into_iter()
        .map(|event| match event {
            Event::OrderRejected(r) => {
                assert_eq!((r.reason.to_string().as_str(), r.timestamp), (REASON, 7));
                r.order_client_id
            }
            other => panic!("a held submit says OrderRejected, got {other:?}"),
        })
        .collect();
    assert_eq!(said, ["c1", "c2", "c3"]);
    assert!(inner.doors().is_empty(), "nothing reached the held client: {:?}", inner.doors());
}

#[test]
fn every_amend_is_refused_non_terminally_and_never_reaches_the_held_client() {
    let (mut client, inner, mut lane) = held();
    let before = vike_model::now_ms();
    client.modify(&order("c1"), Some(1.0), Some(2.0));
    assert_eq!(client.modify_trigger(&order("c2"), None, None, 3.0), Ok(()));
    let said: Vec<_> = drain_ready(|| lane.try_recv().ok())
        .into_iter()
        .map(|event| match event {
            Event::OrderModifyRejected(r) => {
                assert_eq!(r.reason.to_string().as_str(), REASON);
                // an amend's answer is stamped when made, never at the order's creation (its 7)
                assert!(r.timestamp >= before, "{}", r.timestamp);
                r.order_client_id
            }
            other => panic!("a held amend says OrderModifyRejected, got {other:?}"),
        })
        .collect();
    assert_eq!(said, ["c1", "c2"]);
    assert!(inner.doors().is_empty(), "nothing reached the held client: {:?}", inner.doors());
}

#[test]
fn every_other_door_is_the_held_clients_own() {
    let (mut client, inner, mut lane) = held();
    let ids = ["a".to_string(), "b".to_string()];
    client.cancel("c1");
    client.cancel_with_intent("c2", OrderCancelIntent::RiskOff);
    client.cancel_batch(&ids);
    client.cancel_batch_with_intent(&ids, OrderCancelIntent::Routine);
    client.confirm("c3");
    client.begin_detach();
    client.detach();
    assert!(client.poll_events().is_some(), "the held client's polled event comes back");
    let bar = vike_marketdata::Bar {
        timestamp: 5,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    };
    client.on_bar(&bar);
    client.on_bar_sides(&bar, None);
    client.adopt_restored(&[RestoredOrder::new("c4", "BTCUSDT")]);
    client.on_contract_value("c6", vike_model::ContractValue::default());
    client.on_quote("BTCUSDT", 1.0, 2.0, 6);
    client.fill_at_close(&["c7".to_string()], 8);
    client.hold_for_close(true);
    assert_eq!(client.amend_semantics(), Some(AmendSemantics::InPlaceRemaining));
    assert_eq!(client.wire_id_for("c5").as_deref(), Some("wire-c5"));
    assert_eq!(
        inner.doors(),
        [
            "cancel c1",
            "cancel_with_intent c2 RiskOff",
            "cancel_batch a,b",
            "cancel_batch_with_intent a,b Routine",
            "confirm c3",
            "begin_detach",
            "detach",
            "poll_events",
            "on_bar 5",
            "on_bar_sides 5",
            "adopt_restored 1",
            "on_contract_value c6",
            "on_quote BTCUSDT 6",
            "fill_at_close c7 8",
            "hold_for_close true",
        ]
    );
    assert!(drain_ready(|| lane.try_recv().ok()).is_empty(), "the hold says nothing for them");
}
