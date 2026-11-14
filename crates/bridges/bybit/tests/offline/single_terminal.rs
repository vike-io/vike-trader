//! One cancel of a resting conditional order must reach the core as ONE terminal word: a cancel.
//!
//! Bybit's v5 `orderStatus` enum: "`Deactivated` UTA: Spot tp/sl order, conditional order, OCO order are
//! cancelled before they are triggered" (bybit-exchange.github.io/docs/v5/enum, read 2026-11-10). It is a
//! cancel, by us or by the venue (its `cancelType` says which), and never an expiry: every reader of the
//! row (the private stream's `map_order`, the history replay, the confirm, the reconcile read) says
//! `OrderCanceled` / `Canceled`.
//!
//! Measured: on v0.1.87 (2026-10-24) a stop's cancel printed `outcome: OrderExpired` (a `Cancelled` row
//! then a `Deactivated` row, read as two words); on v0.1.88 (2026-10-25) a cancelled stop got `Deactivated`
//! alone, and when the push beat the REST answer the order ENDED `EXPIRED`; the v0.1.89 cure (a memory of
//! the cancels THIS client issued) held until the node ran a second bybit account on the same key
//! (the CI box `DEMO` and `INV`, build ba0a4c92ae, 2026-11-10, `pd4bystp1`): the venue's private socket hands
//! every order of the key to both pumps, the other account's pump had noted no cancel, and the ring got
//! `OrderCanceled` then `OrderExpired` again. (Since then the other account's pump drops a row of a book it
//! does not trade: `one_key_two_accounts`.)
//!
//! The frames are scripted, as the pump would hand them over; nothing here opens a socket.

use serde_json::{Value, json};
use vike_bybit::execution::event_mapper::map_bybit_perp;
use vike_bybit::execution::user_data::{REPORTED_TERMINALS_KEPT, perp_frame_decoder};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_model::events::{Event, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, RiskLimits};

/// One `order`-topic row in the shape the venue sends for a conditional (stop) order.
fn row(order_client_id: &str, status: &str) -> Value {
    json!({
        "category": "linear", "symbol": "BTCUSDT", "orderId": "v-oid", "orderLinkId": order_client_id,
        "orderStatus": status, "cancelType": "CancelByUser", "stopOrderType": "Stop",
        "updatedTime": "5"
    })
}

fn frame(rows: Vec<Value>) -> Value {
    json!({"topic": "order", "creationTime": 1, "data": rows})
}

/// What the core would be handed, as `Kind:order_client_id` words.
fn words(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => format!("OrderCanceled:{}", c.order_client_id),
            Event::OrderExpired(x) => format!("OrderExpired:{}", x.order_client_id),
            Event::OrderAccepted(a) => format!("OrderAccepted:{}", a.order_client_id),
            Event::Fill(f) => format!("Fill:{}", f.order_client_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.fill.order_client_id),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Run frames through ONE decoder, in order, and return everything it handed over.
fn decode_all(frames: &[Value]) -> Vec<String> {
    let decode = perp_frame_decoder("BTCUSDT");
    frames.iter().flat_map(|f| words(&decode(f))).collect()
}

/// The measured case (the CI box, 2026-11-10, `pd4bystp1`): the stop's `Deactivated` row as the OTHER account's
/// pump on the same key reads it, with nothing of its own to go on, and as the history replay and the
/// reconcile read list it. Each says a cancel, with the venue's `cancelType` as the reason.
#[test]
fn a_deactivated_stop_is_a_cancel_to_every_reader() {
    let stream = map_bybit_perp(&frame(vec![row("c1", "Deactivated")]), "bybit", "BTCUSD");
    assert_eq!(words(&stream), ["OrderCanceled:c1"], "the pump of any account on the key");
    let Event::OrderCanceled(cancel) = &stream[0] else { unreachable!() };
    assert_eq!(&*cancel.reason, "CancelByUser", "the venue's cancelType is the reason");

    let closed = json!([{"orderId": "v-oid", "orderLinkId": "c1", "orderStatus": "Deactivated",
        "cancelType": "CancelByTpSlTsClear", "updatedTime": "7"}]);
    let replay =
        vike_bybit::execution::resync::map_bybit_history(&closed, &json!([]), "bybit", "BTCUSDT");
    assert_eq!(words(&replay), ["OrderCanceled:c1"], "the history replay");

    let listed = json!({"list": [{"symbol": "BTCUSDT", "orderId": "v-oid", "orderLinkId": "c1",
        "side": "Sell", "orderType": "Market", "stopOrderType": "Stop", "triggerDirection": 2,
        "triggerPrice": "50000", "qty": "0.001", "cumExecQty": "0", "avgPrice": "0",
        "orderStatus": "Deactivated", "updatedTime": "7"}]});
    let reports = vike_bybit::reconcile::parse_orders(&listed.to_string()).unwrap();
    assert_eq!(reports[0].status, OrderStatus::Canceled, "the reconcile read");
}

/// The same stop's cancel through TWO bybit accounts on one key, as the node runs them: the account that
/// cancelled it answers through its exec loop and its own pump; the other account's pump (the inverse
/// `INV`) reads the same linear row and hands over nothing, since it is not a row of its book
/// (`one_key_two_accounts`).
#[test]
fn a_second_account_on_the_key_hands_over_nothing_of_our_stops_cancel() {
    vike_bybit::history::kept_list_for_tests();
    let (ours, theirs) = (perp_frame_decoder("ETHUSDT"), perp_frame_decoder("BTCUSD"));
    let deactivated = frame(vec![row("c1", "Deactivated")]);
    assert_eq!(words(&ours(&deactivated)), ["OrderCanceled:c1"]);
    assert!(words(&theirs(&deactivated)).is_empty(), "the inverse account's pump");
}

#[test]
fn cancelled_then_deactivated_is_one_cancel() {
    // Two frames, as the live socket delivered them on 2026-10-24 ...
    let got =
        decode_all(&[frame(vec![row("c1", "Cancelled")]), frame(vec![row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"], "the cancel is the only terminal");

    // ... and both rows in ONE frame, which the venue may also do, and the other order.
    let got = decode_all(&[frame(vec![row("c1", "Cancelled"), row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"]);
    let got =
        decode_all(&[frame(vec![row("c1", "Deactivated")]), frame(vec![row("c1", "Cancelled")])]);
    assert_eq!(got, ["OrderCanceled:c1"]);
}

#[test]
fn only_the_order_that_ended_is_remembered() {
    // Another order's cancel, an order's acceptance and a fill's replay are not touched.
    let got = decode_all(&[
        frame(vec![row("c1", "Cancelled")]),
        frame(vec![row("c2", "Deactivated")]),
        frame(vec![row("c3", "New")]),
        frame(vec![row("c1", "Deactivated")]),
    ]);
    assert_eq!(got, ["OrderCanceled:c1", "OrderCanceled:c2", "OrderAccepted:c3"]);

    // A row with no `orderLinkId` names no order, so two of them are two events.
    let got = decode_all(&[frame(vec![row("", "Deactivated"), row("", "Deactivated")])]);
    assert_eq!(got.len(), 2, "{got:?}");

    // The decoder leaves fills alone: a replayed execution row still gives [Fill, OrderFilled]
    // twice (the engine's execId dedup owns that), so the memory is only for the cancel.
    let fill = json!({"topic": "execution", "data": [{
        "execType": "Trade", "symbol": "BTCUSDT", "orderLinkId": "c9", "execId": "E1",
        "side": "Buy", "execPrice": "100", "execQty": "1", "execFee": "0.06",
        "feeCurrency": "USDT", "isMaker": false, "cumExecQty": "1", "orderQty": "1",
        "leavesQty": "0"}]});
    let got = decode_all(&[fill.clone(), fill]);
    assert_eq!(got, ["Fill:c9", "OrderFilled:c9", "Fill:c9", "OrderFilled:c9"]);
}

#[test]
fn the_memory_is_bounded() {
    let decode = perp_frame_decoder("BTCUSDT");
    let order_client_id = |n: usize| format!("c{n}");
    // One more cancelled order than the memory holds: the OLDEST id is forgotten, the rest stay.
    for n in 0..=REPORTED_TERMINALS_KEPT {
        assert_eq!(words(&decode(&frame(vec![row(&order_client_id(n), "Cancelled")]))).len(), 1);
    }
    let again = |n: usize| words(&decode(&frame(vec![row(&order_client_id(n), "Deactivated")])));
    assert!(
        again(REPORTED_TERMINALS_KEPT).is_empty(),
        "the newest cancel is still remembered, so its Deactivated is dropped"
    );
    assert_eq!(
        again(0),
        ["OrderCanceled:c0"],
        "the oldest cancel fell out of the bounded memory (a ring, not a growing set)"
    );
}

/// The engine side: a second terminal on an ended order is dropped by the FSM without a counter, so
/// a repeated cancel never moves an order's state.
#[test]
fn engine_drops_the_second_terminal_without_a_counter() {
    let mut engine = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut outbox = EventBusOutbox::default();
    let req = OrderRequest {
        order_client_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    };
    engine.submit_order(&req, 1, &mut outbox);
    engine.on_event(
        &Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".to_string(), timestamp: 1 }),
        &mut outbox,
    );
    for event in map_bybit_perp(&frame(vec![row("c1", "New")]), "bybit", "BTCUSDT") {
        engine.on_event(&event, &mut outbox);
    }
    assert_eq!(engine.registry.get("c1").unwrap().status, OrderStatus::Accepted);

    // The RAW mapper output (no memory): the cancel, twice.
    for status in ["Cancelled", "Deactivated"] {
        for event in map_bybit_perp(&frame(vec![row("c1", status)]), "bybit", "BTCUSDT") {
            engine.on_event(&event, &mut outbox);
        }
    }
    assert_eq!(
        engine.registry.get("c1").unwrap().status,
        OrderStatus::Canceled,
        "first terminal wins"
    );
    assert_eq!(
        engine.dropped_terminal_on_live, 0,
        "not a lost terminal: the order had already ended"
    );
    assert_eq!(engine.stranded_terminal_drops, 0);
}

// ---------------------------------------------------------------------------------------------
// The whole lane: the WORD a cancelled stop ends with, the exec loop, the stream decoder and the
// history replay together. Same-word duplicates (`OrderCanceled` twice) are on every REST venue and
// harmless: the gatewayhub's first-wins memory and the engine both drop a second terminal.
// ---------------------------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::{ExecutionClientActorCommand, run_loop};
use vike_bridge_core::signer::SignerJson;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bybit::execution::bybit_history_events;
use vike_execution::{Ingest, event_channel};
use vike_model::OrderCancelIntent;

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// The venue, as far as a cancel can see it: a cancel POST that is taken, and `/v5/order/history`
/// answering `history` (`reads` counts how often it was asked).
struct Venue {
    history: Value,
    reads: Arc<AtomicUsize>,
}

impl RestTransportJson for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &dyn SignerJson,
    ) -> Result<Value, VenueApiError> {
        let list = if method.eq_ignore_ascii_case("POST") {
            return Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"orderId": "v-oid"}}));
        } else if path.ends_with("/order/history") {
            self.reads.fetch_add(1, Ordering::SeqCst);
            self.history.clone()
        } else {
            json!([])
        };
        Ok(json!({"retCode": 0, "retMsg": "OK", "result": {"list": list}}))
    }
}

/// A closed-order row of `/v5/order/history`, stamped far enough ahead of "now" that the restart law's
/// spawn floor (`run_loop` drops a replayed row older than its own thread) lets it through.
fn closed_row(order_client_id: &str, status: &str) -> Value {
    let later = vike_model::now_ms() + 3_600_000;
    json!({
        "orderId": "v-oid", "orderLinkId": order_client_id, "orderStatus": status,
        "cancelType": "CancelByUser", "stopOrderType": "", "updatedTime": later.to_string()
    })
}

/// The exec thread of a Bybit start, minus the sockets: `run_loop` over a REST client on a stubbed
/// transport, the gap-sentinel firing 20 ms after a command, and the lane the core reads.
struct Lane {
    sender: vike_execution::EventSender,
    /// The core's end of the lane: the next event, if one is waiting.
    next_event: Box<dyn FnMut() -> Option<Event>>,
    commands: std::sync::mpsc::Sender<ExecutionClientActorCommand>,
    history_reads: Arc<AtomicUsize>,
    exec: std::thread::JoinHandle<()>,
    seen: Vec<Event>,
}

impl Lane {
    fn start(history: Vec<Value>) -> Lane {
        let (sender, mut ingest) = event_channel(256);
        let next_event = Box::new(move || match ingest.try_recv() {
            Ok(Ingest::Event(event)) => Some(event),
            _ => None,
        });
        let (commands, rx) = std::sync::mpsc::channel();
        let reads = Arc::new(AtomicUsize::new(0));
        let rest = |reads: &Arc<AtomicUsize>| {
            let venue = Venue { history: json!(history), reads: Arc::clone(reads) };
            bybit_perp(&fixture(), venue)
        };
        let (submit_rest, history_rest) = (rest(&reads), rest(&reads));
        let lane = sender.clone();
        let exec = std::thread::spawn(move || {
            let replay = || bybit_history_events(&history_rest, "BTCUSDT");
            run_loop(&submit_rest, &lane, rx, replay, Duration::from_millis(20));
        });
        Lane { sender, next_event, commands, history_reads: reads, exec, seen: Vec::new() }
    }

    /// The core issues a cancel: the command reaches the exec thread.
    fn cancel(&self, order_client_id: &str) {
        let intent = OrderCancelIntent::Unspecified;
        let cancel = ExecutionClientActorCommand::Cancel {
            order_client_id: order_client_id.to_string(),
            intent,
        };
        self.commands.send(cancel).expect("the exec thread is alive");
    }

    /// The private-WS pump: decode one frame, hand every event to the lane.
    fn ws(&self, decode: &impl Fn(&Value) -> Vec<Event>, frame: &Value) {
        for event in decode(frame) {
            self.sender.blocking_send(event).expect("the core is listening");
        }
    }

    fn drain(&mut self) {
        while let Some(event) = (self.next_event)() {
            self.seen.push(event);
        }
    }

    /// Wait until the lane holds a terminal for `order_client_id` (the cancel answer or the stream's word).
    fn wait_for_a_terminal(&mut self, order_client_id: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while terminals(&self.seen, order_client_id).is_empty() {
            assert!(
                Instant::now() < deadline,
                "no terminal for {order_client_id}: {:?}",
                self.seen
            );
            std::thread::sleep(Duration::from_millis(2));
            self.drain();
        }
    }

    /// Let the gap-sentinel fire (it re-arms after every command), stop the exec thread and return
    /// everything the core was handed.
    fn finish(self) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.history_reads.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "the gap-sentinel never replayed history");
            std::thread::sleep(Duration::from_millis(2));
        }
        self.commands
            .send(ExecutionClientActorCommand::Shutdown)
            .expect("the exec thread is alive");
        let Lane { exec, mut next_event, mut seen, .. } = self;
        exec.join().expect("the exec thread ends");
        while let Some(event) = next_event() {
            seen.push(event);
        }
        seen
    }
}

/// The cancel and expiry words the core was handed for `order_client_id`, in arrival order.
fn terminals(events: &[Event], order_client_id: &str) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(c) if c.order_client_id == order_client_id => {
                Some("OrderCanceled")
            }
            Event::OrderExpired(x) if x.order_client_id == order_client_id => Some("OrderExpired"),
            _ => None,
        })
        .collect()
}

/// How the order `c1` ENDS in the core's registry when the lane's events fold into it, in arrival order
/// (the order is submitted and accepted first, as on the node).
fn registry_status_of_c1(lane_events: &[Event]) -> OrderStatus {
    let mut engine = Engine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut outbox = EventBusOutbox::default();
    let req = OrderRequest {
        order_client_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        timestamp: 1,
        ..Default::default()
    };
    engine.submit_order(&req, 1, &mut outbox);
    engine.on_event(
        &Event::OrderSubmitted(OrderSubmitted { order_client_id: "c1".to_string(), timestamp: 1 }),
        &mut outbox,
    );
    for event in map_bybit_perp(&frame(vec![row("c1", "New")]), "bybit", "BTCUSDT") {
        engine.on_event(&event, &mut outbox);
    }
    for event in lane_events {
        engine.on_event(event, &mut outbox);
    }
    engine.registry.get("c1").expect("c1 is registered").status
}

/// Every cancel or expiry word the core was handed for `c1` is `OrderCanceled`, there is at least one,
/// and folded into the core the order ends `Canceled`.
fn ends_canceled(seen: &[Event]) {
    let got = terminals(seen, "c1");
    assert!(
        !got.is_empty() && got.iter().all(|w| *w == "OrderCanceled"),
        "{got:?} want only OrderCanceled: {seen:?}"
    );
    assert_eq!(registry_status_of_c1(seen), OrderStatus::Canceled);
}

#[test]
fn a_cancelled_limit_order_ends_canceled() {
    let mut lane = Lane::start(vec![closed_row("c1", "Cancelled")]);
    let decode = perp_frame_decoder("BTCUSDT");
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1"); // the cancel answer, as on the node
    lane.ws(&decode, &frame(vec![row("c1", "Cancelled")]));
    ends_canceled(&lane.finish());
}

/// The ordinary order of a stop's cancel: the exec loop's answer first, the stream's `Deactivated` after.
#[test]
fn a_cancelled_stop_ends_canceled_and_is_never_called_expired() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = perp_frame_decoder("BTCUSDT");
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1");
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")]));
    ends_canceled(&lane.finish());
}

/// The other order of the same race, MEASURED on the CI box (v0.1.88, 2026-10-25, `b9t88stp`): the venue's
/// `Deactivated` push is read BEFORE the REST answer, and the order then ended `EXPIRED`.
#[test]
fn a_deactivated_that_beats_our_own_cancel_answer_ends_canceled() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = perp_frame_decoder("BTCUSDT");
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")])); // the push lands first...
    lane.wait_for_a_terminal("c1");
    lane.cancel("c1"); // ...and the REST answer second
    ends_canceled(&lane.finish());
}

/// The stream lost the stop's `Deactivated`: the history replay five seconds after the command is the
/// only other place it is read, and it must not call the cancel an expiry either.
#[test]
fn a_replayed_deactivated_for_a_stop_we_cancelled_is_a_cancel() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1");
    ends_canceled(&lane.finish());
}

/// A stop the venue pulled by itself (nobody here cancelled it) is the venue's cancel, in the stream
/// and in every replay of it: any later command re-arms the gap-sentinel, which replays the closed rows.
#[test]
fn a_stop_the_venue_pulled_is_a_cancel_even_when_history_repeats_it() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = perp_frame_decoder("BTCUSDT");
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")]));
    lane.wait_for_a_terminal("c1");
    lane.cancel("some-other-order");
    ends_canceled(&lane.finish());
}
