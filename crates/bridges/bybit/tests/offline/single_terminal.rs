//! One cancel of a resting conditional order must reach the core as ONE terminal event.
//!
//! Measured on the v0.1.87 node (Bybit DEMO, 2026-10-24): cancelling a resting STOP printed
//! `outcome: OrderExpired`, because Bybit's private `order` topic sends a `Cancelled` row and then a
//! `Deactivated` row for a cancelled conditional order, and the mapper turns the first into
//! `OrderCanceled` and the second into `OrderExpired`. The engine already ignored the second
//! (`engine_drops_the_second_terminal_without_a_counter`), so the order row was right, but the
//! journal and the CLI's outcome line carried both.
//!
//! The cure is `vike_bybit::user_data::perp_frame_decoder`, the decoder the private-WS pump runs on
//! every frame: `map_bybit_perp` plus a bounded memory of the orders it already reported a cancel or
//! an expiry for. A genuine `Deactivated` (nothing before it) is still `OrderExpired`.
//!
//! The frames are scripted, as the pump would hand them over; nothing here opens a socket.

use serde_json::{Value, json};
use vike_bybit::event_mapper::map_bybit_perp;
use vike_bybit::user_data::{REPORTED_TERMINALS_KEPT, perp_frame_decoder};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, EventHandler, ExecutionEngine, Outbox, RiskGate};
use vike_model::events::{Event, OrderSubmitted};
use vike_model::{OrderRequest, OrderStatus, RiskLimits};

/// One `order`-topic row in the shape the venue sends for a conditional (stop) order.
fn row(coid: &str, status: &str) -> Value {
    json!({
        "category": "linear", "symbol": "BTCUSDT", "orderId": "v-oid", "orderLinkId": coid,
        "orderStatus": status, "cancelType": "CancelByUser", "stopOrderType": "Stop",
        "updatedTime": "5"
    })
}

fn frame(rows: Vec<Value>) -> Value {
    json!({"topic": "order", "creationTime": 1, "data": rows})
}

/// What the core would be handed, as `Kind:coid` words.
fn words(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            Event::OrderCanceled(c) => format!("OrderCanceled:{}", c.client_order_id),
            Event::OrderExpired(x) => format!("OrderExpired:{}", x.client_order_id),
            Event::OrderAccepted(a) => format!("OrderAccepted:{}", a.client_order_id),
            Event::Fill(f) => format!("Fill:{}", f.client_order_id),
            Event::OrderFilled(w) => format!("OrderFilled:{}", w.fill.client_order_id),
            other => format!("{other:?}"),
        })
        .collect()
}

/// Run frames through ONE decoder, in order, and return everything it handed over.
fn decode_all(frames: &[Value]) -> Vec<String> {
    let decode = perp_frame_decoder("BTCUSDT", Arc::default());
    frames.iter().flat_map(|f| words(&decode(f))).collect()
}

#[test]
fn cancelled_then_deactivated_is_one_cancel() {
    // Two frames, as the live socket delivers them ...
    let got =
        decode_all(&[frame(vec![row("c1", "Cancelled")]), frame(vec![row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"], "the cancel is the only terminal");

    // ... and both rows in ONE frame, which the venue may also do.
    let got = decode_all(&[frame(vec![row("c1", "Cancelled"), row("c1", "Deactivated")])]);
    assert_eq!(got, ["OrderCanceled:c1"]);
}

#[test]
fn a_lone_deactivated_is_still_an_expiry() {
    // A stop that expired, or one the venue deactivated on its own: no `Cancelled` row before it.
    assert_eq!(decode_all(&[frame(vec![row("c1", "Deactivated")])]), ["OrderExpired:c1"]);
}

#[test]
fn deactivated_before_cancelled_is_still_one_terminal() {
    // The first terminal wins, exactly as the engine would have had it: one event, not two.
    let got =
        decode_all(&[frame(vec![row("c1", "Deactivated")]), frame(vec![row("c1", "Cancelled")])]);
    assert_eq!(got, ["OrderExpired:c1"]);
}

#[test]
fn only_the_order_that_ended_is_remembered() {
    // Another order's expiry, an order's acceptance and a fill's replay are not touched.
    let got = decode_all(&[
        frame(vec![row("c1", "Cancelled")]),
        frame(vec![row("c2", "Deactivated")]),
        frame(vec![row("c3", "New")]),
        frame(vec![row("c1", "Deactivated")]),
    ]);
    assert_eq!(got, ["OrderCanceled:c1", "OrderExpired:c2", "OrderAccepted:c3"]);

    // A row with no `orderLinkId` names no order, so two of them are two events.
    let got = decode_all(&[frame(vec![row("", "Deactivated"), row("", "Deactivated")])]);
    assert_eq!(got.len(), 2, "{got:?}");

    // The decoder leaves fills alone: a replayed execution row still gives [Fill, OrderFilled]
    // twice (the engine's execId dedup owns that), so the memory is only for cancel/expiry.
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
    let decode = perp_frame_decoder("BTCUSDT", Arc::default());
    let coid = |n: usize| format!("c{n}");
    // One more cancelled order than the memory holds: the OLDEST id is forgotten, the rest stay.
    for n in 0..=REPORTED_TERMINALS_KEPT {
        assert_eq!(words(&decode(&frame(vec![row(&coid(n), "Cancelled")]))).len(), 1);
    }
    let again = |n: usize| words(&decode(&frame(vec![row(&coid(n), "Deactivated")])));
    assert!(
        again(REPORTED_TERMINALS_KEPT).is_empty(),
        "the newest cancel is still remembered, so its Deactivated is dropped"
    );
    assert_eq!(
        again(0),
        ["OrderExpired:c0"],
        "the oldest cancel fell out of the bounded memory (a ring, not a growing set)"
    );
}

/// The engine side, measured: the second terminal on an ended order was ALREADY dropped by the FSM
/// without a counter, so no order state was ever wrong. This is why the fix lives in the bridge
/// (the journal and the outcome line), not in the engine.
#[test]
fn engine_drops_the_second_terminal_without_a_counter() {
    let mut eng = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut outbox = Outbox::default();
    let req = OrderRequest {
        client_order_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    eng.submit_order(&req, 1, &mut outbox);
    eng.on_event(
        &Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".to_string(), ts: 1 }),
        &mut outbox,
    );
    for ev in map_bybit_perp(&frame(vec![row("c1", "New")]), "bybit", "BTCUSDT") {
        eng.on_event(&ev, &mut outbox);
    }
    assert_eq!(eng.registry.get("c1").unwrap().status, OrderStatus::Accepted);

    // The RAW mapper output (no memory): a cancel, then the expiry.
    for status in ["Cancelled", "Deactivated"] {
        for ev in map_bybit_perp(&frame(vec![row("c1", status)]), "bybit", "BTCUSDT") {
            eng.on_event(&ev, &mut outbox);
        }
    }
    assert_eq!(
        eng.registry.get("c1").unwrap().status,
        OrderStatus::Canceled,
        "first terminal wins"
    );
    assert_eq!(eng.dropped_terminal_on_live, 0, "not a lost terminal: the order had already ended");
    assert_eq!(eng.stranded_terminal_drops, 0);
}

// ---------------------------------------------------------------------------------------------
// The whole lane: the WORD a cancelled stop ends with.
//
// Measured on the v0.1.88 node (the CI box, Bybit DEMO, 2026-10-25): the stream never sends a cancelled
// stop a `Cancelled` row, ONLY `Deactivated`, which the mapper reads as `OrderExpired` (the pair
// above, `Cancelled` then `Deactivated`, did not occur). A node cancel of a stop therefore put
// `OrderCanceled` (the exec loop's answer, `ts: 0`) and `OrderExpired` (the stream, and again the
// history replay five seconds later) in the node's `recent` ring, and when the stream's push beat
// the REST answer the order ENDED `EXPIRED` in the registry. Same-word duplicates (`OrderCanceled`
// twice) are on every REST venue and harmless: the engine drops a second terminal without a counter.
// The WRONG word is Bybit's own, and these tests drive the exec loop, the stream decoder and the
// history replay together to say what a cancelled stop ends as.
// ---------------------------------------------------------------------------------------------

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::{ExecCommand, run_loop};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::signer::BybitV5Signer;
use vike_bridge_core::transport::VenueApiError;
use vike_bybit::exec::bybit_history_events;
use vike_bybit::per_symbol::{CoidSymbols, PerSymbolRest};
use vike_bybit::transport::BybitTransport;
use vike_bybit::user_data::CancelNotes;
use vike_exec::{CancelIntent, Ingest, event_channel};

use crate::r6_bybit_parity::{bybit_perp, fixture};

/// The venue, as far as a cancel can see it: a cancel POST that answers `cancel_code` (`0` = taken), and
/// `/v5/order/history` answering `history` (`reads` counts how often it was asked).
struct Venue {
    history: Value,
    reads: Arc<AtomicUsize>,
    cancel_code: i64,
}

impl BybitTransport for Venue {
    fn signed(
        &self,
        _base: &str,
        path: &str,
        method: &str,
        _params: &[(&str, Value)],
        _signer: &BybitV5Signer,
    ) -> Result<Value, VenueApiError> {
        let list = if method.eq_ignore_ascii_case("POST") {
            let msg = if self.cancel_code == 0 { "OK" } else { "refused" };
            return Ok(json!({
                "retCode": self.cancel_code, "retMsg": msg, "result": {"orderId": "v-oid"}
            }));
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
fn closed_row(coid: &str, status: &str) -> Value {
    let later = vike_model::now_ms() + 3_600_000;
    json!({
        "orderId": "v-oid", "orderLinkId": coid, "orderStatus": status,
        "cancelType": "CancelByUser", "stopOrderType": "", "updatedTime": later.to_string()
    })
}

/// The exec thread of a Bybit start, minus the sockets: `run_loop` over a REST client on a stubbed
/// transport, the gap-sentinel firing 20 ms after a command, and the lane the core reads. Wired as
/// `crate::exec`'s `run` wires it: ONE `CancelNotes` behind the history replay and (through
/// [`Lane::decoder`]) the pump, and the client's `cancel` noting into it ([`Lane::request_cancel`]).
struct Lane {
    notes: Arc<CancelNotes>,
    sender: vike_exec::EventSender,
    /// The core's end of the lane: the next event, if one is waiting.
    next_event: Box<dyn FnMut() -> Option<Event>>,
    commands: std::sync::mpsc::Sender<ExecCommand>,
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
            let venue = Venue { history: json!(history), reads: Arc::clone(reads), cancel_code: 0 };
            bybit_perp(&fixture(), venue)
        };
        let (submit_rest, history_rest) = (rest(&reads), rest(&reads));
        let lane = sender.clone();
        let notes = Arc::new(CancelNotes::default());
        let replay_notes = Arc::clone(&notes);
        let exec = std::thread::spawn(move || {
            let replay = || bybit_history_events(&history_rest, "BTCUSDT", &replay_notes);
            run_loop(&submit_rest, &lane, rx, replay, Duration::from_millis(20));
        });
        Lane { notes, sender, next_event, commands, history_reads: reads, exec, seen: Vec::new() }
    }

    /// The private-WS pump's decoder, over the start's one memory.
    fn decoder(&self) -> impl Fn(&Value) -> Vec<Event> + use<> {
        perp_frame_decoder("BTCUSDT", Arc::clone(&self.notes))
    }

    /// The core issues a cancel: the client notes it, THEN the command reaches the exec thread. The two
    /// halves are separate so a test can put the venue's push between them.
    fn cancel(&self, coid: &str) {
        self.request_cancel(coid);
        self.queue_cancel(coid);
    }

    fn request_cancel(&self, coid: &str) {
        self.notes.note(coid);
    }

    fn queue_cancel(&self, coid: &str) {
        let intent = CancelIntent::Unspecified;
        let cancel = ExecCommand::Cancel { client_order_id: coid.to_string(), intent };
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

    /// Wait until the lane holds a terminal for `coid` (the cancel answer or the stream's word).
    fn wait_for_a_terminal(&mut self, coid: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while terminals(&self.seen, coid).is_empty() {
            assert!(Instant::now() < deadline, "no terminal for {coid}: {:?}", self.seen);
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
        self.commands.send(ExecCommand::Shutdown).expect("the exec thread is alive");
        let Lane { exec, mut next_event, mut seen, .. } = self;
        exec.join().expect("the exec thread ends");
        while let Some(event) = next_event() {
            seen.push(event);
        }
        seen
    }
}

/// The cancel and expiry words the core was handed for `coid`, in arrival order.
fn terminals(events: &[Event], coid: &str) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::OrderCanceled(c) if c.client_order_id == coid => Some("OrderCanceled"),
            Event::OrderExpired(x) if x.client_order_id == coid => Some("OrderExpired"),
            _ => None,
        })
        .collect()
}

/// How the order `c1` ENDS in the core's registry when the lane's events fold into it, in arrival order
/// (the order is submitted and accepted first, as on the node).
fn registry_status_of_c1(lane_events: &[Event]) -> OrderStatus {
    let mut eng = ExecutionEngine::new(
        Account::new(1.0, "bybit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "bybit",
        "BTCUSDT",
    );
    let mut outbox = Outbox::default();
    let req = OrderRequest {
        client_order_id: "c1".into(),
        venue: "bybit".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ts: 1,
        ..Default::default()
    };
    eng.submit_order(&req, 1, &mut outbox);
    eng.on_event(
        &Event::OrderSubmitted(OrderSubmitted { client_order_id: "c1".to_string(), ts: 1 }),
        &mut outbox,
    );
    for ev in map_bybit_perp(&frame(vec![row("c1", "New")]), "bybit", "BTCUSDT") {
        eng.on_event(&ev, &mut outbox);
    }
    for ev in lane_events {
        eng.on_event(ev, &mut outbox);
    }
    eng.registry.get("c1").expect("c1 is registered").status
}

/// Every cancel or expiry word the core was handed for `c1` is `word`, and there is at least one.
/// (More than one is fine: the engine drops a second terminal, and the same-word duplicate is on every
/// REST venue; it is the WORD that is Bybit's.)
fn only_ever(word: &str, seen: &[Event]) {
    let got = terminals(seen, "c1");
    assert!(
        !got.is_empty() && got.iter().all(|w| *w == word),
        "{got:?} want only {word}: {seen:?}"
    );
}

#[test]
fn a_cancelled_limit_order_ends_canceled() {
    let mut lane = Lane::start(vec![closed_row("c1", "Cancelled")]);
    let decode = lane.decoder();
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1"); // the cancel answer, as on the node
    lane.ws(&decode, &frame(vec![row("c1", "Cancelled")]));
    let seen = lane.finish();
    only_ever("OrderCanceled", &seen);
    assert_eq!(registry_status_of_c1(&seen), OrderStatus::Canceled);
}

/// The ordinary order of a stop's cancel: the exec loop's answer first, the stream's `Deactivated` after.
#[test]
fn a_cancelled_stop_ends_canceled_and_is_never_called_expired() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = lane.decoder();
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1");
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")]));
    let seen = lane.finish();
    only_ever("OrderCanceled", &seen);
    assert_eq!(registry_status_of_c1(&seen), OrderStatus::Canceled);
}

/// The other order of the same race, MEASURED on the CI box (v0.1.88, 2026-10-25, `b9t88stp`): the venue's
/// `Deactivated` push is read BEFORE the REST answer, and the order then ended `EXPIRED`.
#[test]
fn a_deactivated_that_beats_our_own_cancel_answer_ends_canceled() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = lane.decoder();
    lane.request_cancel("c1"); // the client noted the cancel before the command was queued...
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")])); // ...the push lands first...
    lane.wait_for_a_terminal("c1");
    lane.queue_cancel("c1"); // ...and the REST answer second
    let seen = lane.finish();
    only_ever("OrderCanceled", &seen);
    assert_eq!(registry_status_of_c1(&seen), OrderStatus::Canceled);
}

/// The stream lost the stop's `Deactivated`: the history replay five seconds after the command is the
/// only other place it is read, and it must not call a cancel of ours an expiry either.
#[test]
fn a_replayed_deactivated_for_a_stop_we_cancelled_is_a_cancel() {
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    lane.cancel("c1");
    lane.wait_for_a_terminal("c1");
    let seen = lane.finish();
    only_ever("OrderCanceled", &seen);
    assert_eq!(registry_status_of_c1(&seen), OrderStatus::Canceled);
}

/// A stop the venue pulled by itself is still an expiry, in the stream and in every replay of it.
#[test]
fn a_stop_the_venue_pulled_stays_an_expiry_even_when_history_repeats_it() {
    // Nobody cancelled it. Any later command re-arms the gap-sentinel, which replays the venue's
    // closed rows, this one included.
    let mut lane = Lane::start(vec![closed_row("c1", "Deactivated")]);
    let decode = lane.decoder();
    lane.ws(&decode, &frame(vec![row("c1", "Deactivated")]));
    lane.wait_for_a_terminal("c1");
    lane.cancel("some-other-order");
    let seen = lane.finish();
    only_ever("OrderExpired", &seen);
    assert_eq!(registry_status_of_c1(&seen), OrderStatus::Expired);
}

/// A cancel the venue REFUSES leaves the order live, so the router that sent it strikes its note: the
/// order's later `Deactivated` is the venue's own, an expiry. A cancel the venue took keeps its note.
#[test]
fn a_refused_cancel_forgets_its_note() {
    let notes = Arc::new(CancelNotes::default());
    let decode = perp_frame_decoder("BTCUSDT", Arc::clone(&notes));
    let router = |cancel_code| {
        let venue = Venue { history: json!([]), reads: Arc::default(), cancel_code };
        let no_other_symbol = Box::new(|_: &str| Err("no other symbol here".to_string()));
        PerSymbolRest::new(
            "BTCUSDT",
            bybit_perp(&fixture(), venue),
            no_other_symbol,
            Arc::new(CoidSymbols::default()),
        )
        .with_cancel_notes(Arc::clone(&notes))
    };
    notes.note("c1");
    notes.note("c2");
    assert!(router(0).cancel_order("c1").is_ok());
    assert!(router(10006).cancel_order("c2").is_err(), "10006 is a refusal");
    let both = frame(vec![row("c1", "Deactivated"), row("c2", "Deactivated")]);
    assert_eq!(words(&decode(&both)), ["OrderCanceled:c1", "OrderExpired:c2"]);
}
