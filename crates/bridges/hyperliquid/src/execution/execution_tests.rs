//! Pure command→action→event wiring — a fake [`HyperliquidExchange`] (NO network, NO real orders) proves:
//! `submit_batch` is ONE native `order` action of N wires; a market order is emulated as an `Ioc`
//! at ±5% off the fake mid; `cancel`/`modify` build the native `cancelByCloid`/`modify` actions
//! (modify keyed by the resting oid); and every failure path (build error, definite transport
//! error) synthesizes the terminal reject while the ambiguous timeout does NOT.
use super::*;
use serde_json::json;
use std::assert_matches;
use std::collections::VecDeque;
use vike_bridge_core::confirm::DispatchLedger;
use vike_bridge_core::exec_actor::{ExecutionClientActor, ExecutionClientActorCommand};
use vike_bridge_core::execution_drive::drain_ready;
use vike_execution::Ingest;
use vike_execution::event_channel;
use vike_execution::testing::RecordingClient;
use vike_model::{OrderStatus, OrderStatusReport};

/// Pins the routed hyperliquid row of `vike_venues::venue_time_in_force::venue_time_in_force` byte-for-byte.
/// NOTE the Fok→Ioc fold: the OPPOSITE direction of polymarket's Ioc→FOK (step-2 flip).
#[test]
fn hl_time_in_force_row_is_pinned() {
    assert_eq!(hl_time_in_force(TimeInForce::Gtc), "Gtc");
    assert_eq!(hl_time_in_force(TimeInForce::Ioc), "Ioc");
    assert_eq!(hl_time_in_force(TimeInForce::Fok), "Ioc");
    assert_eq!(hl_time_in_force(TimeInForce::Gtd), "Gtc");
    assert_eq!(hl_time_in_force(TimeInForce::Day), "Gtc");
    // post-only is the venue's own `Alo` (add liquidity only)
    assert_eq!(hl_time_in_force(TimeInForce::PostOnly), "Alo");
}

/// POST-ONLY goes out as `Alo` on a plain limit order, and on nothing else: a market order (the
/// emulated `Ioc`) or a trigger order would drop the flag and take liquidity, so the adapter refuses
/// them whole, in words, with nothing sent.
#[test]
fn post_only_rides_the_wire_as_alo_and_only_on_a_limit_order() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let mut po = limit_request("po1", 1, 47_000.0, 0.01);
    po.time_in_force = TimeInForce::PostOnly;
    st.submit(&symbology(), std::slice::from_ref(&po));
    {
        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => match &a.orders[0].order_type {
                OrderKind::Limit(l) => assert_eq!(l.tif, "Alo"),
                other => panic!("expected a limit order, got {other:?}"),
            },
            other => panic!("expected an Order action, got {other:?}"),
        }
    }

    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let mut market = limit_request("po2", 1, 47_000.0, 0.01);
    market.order_type = vike_model::OrderType::Market;
    market.price = None;
    market.time_in_force = TimeInForce::PostOnly;
    let mut stop = stop_request("po3", -1, 46_000.0, None, 0.01);
    stop.time_in_force = TimeInForce::PostOnly;
    st.submit(&symbology(), &[market, stop]);
    assert!(st.exchange.actions.lock().unwrap().is_empty(), "nothing is sent");
    let mut refused = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(Event::OrderRejected(r)) = ingest {
            refused.push(r.reason.to_string());
        }
    }
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(
        refused.iter().all(|r| r.contains("PostOnly needs a plain limit order")),
        "{refused:?}"
    );
}

fn symbology() -> Symbology {
    let meta = json!({"universe":[
        {"name":"BTC","szDecimals":5,"maxLeverage":40},
        {"name":"ETH","szDecimals":4,"maxLeverage":25}
    ]});
    let spot = json!({
        "tokens":[
            {"name":"USDC","szDecimals":8,"index":0},
            {"name":"PURR","szDecimals":0,"index":1},
            {"name":"HYPE","szDecimals":2,"index":150}
        ],
        "universe":[
            {"name":"PURR/USDC","tokens":[1,0],"index":0},
            {"name":"@107","tokens":[150,0],"index":107}
        ]
    });
    Symbology::from_meta(&meta, &spot)
}

#[derive(Default)]
struct FakeExchange {
    actions: Mutex<Vec<Action>>,
    responses: Mutex<VecDeque<Result<Value, VenueApiError>>>,
    mid: Option<f64>,
    /// What an `orderStatus`-by-cloid requery answers, and how many were asked.
    status_oid: Option<u64>,
    /// The `orderStatus` request itself fails (a transport error, not a "not open" answer).
    status_fails: bool,
    /// The venue answers `unknownOid` (a DEFINITIVE not-open) instead of a lagging canceled/filled.
    status_unknown: bool,
    status_calls: Mutex<u32>,
    /// What `frontendOpenOrders` lists (a row's venue coin on its `symbol`), and whether that read
    /// fails (a transport error, not an empty list).
    open_orders: Vec<OrderStatusReport>,
    open_orders_fails: bool,
    /// What `info` answers, keyed by the request's `type`, or `type:coin` for a per-coin read; a read
    /// with no entry FAILS, like a venue that is down.
    info_answers: HashMap<String, Value>,
    /// Every `info` body asked, in order.
    info_asked: Mutex<Vec<Value>>,
    /// The fake signs on mainnet instead of the testnet.
    mainnet: bool,
    /// Asked inside `place`: is `c1` already marked dispatched while its POST is in flight?
    watched_ledger: Option<Arc<DispatchLedger>>,
    dispatched_mid_post: Mutex<Vec<bool>>,
}
impl HyperliquidExchange for FakeExchange {
    fn info(&self, body: &Value) -> Result<Value, VenueApiError> {
        self.info_asked.lock().unwrap().push(body.clone());
        let ty = body.get("type").and_then(Value::as_str).unwrap_or_default();
        let per_coin = body.get("coin").and_then(Value::as_str).map(|c| format!("{ty}:{c}"));
        // A builder dex's read (`meta` with a `dex` param) is scripted as `type:dex:<name>`.
        let per_dex = body.get("dex").and_then(Value::as_str).map(|d| format!("{ty}:dex:{d}"));
        per_dex
            .and_then(|key| self.info_answers.get(&key))
            .or_else(|| per_coin.and_then(|key| self.info_answers.get(&key)))
            .or_else(|| self.info_answers.get(ty))
            .cloned()
            .ok_or_else(|| VenueApiError { code: 0, msg: format!("no scripted answer to {ty}") })
    }
    fn resting_orders(&self) -> Result<Vec<OrderStatusReport>, ()> {
        if self.open_orders_fails { Err(()) } else { Ok(self.open_orders.clone()) }
    }
    fn resting_oid(&self, _cloid: &str) -> Result<order_wire::OrderStatusAnswer, ()> {
        use order_wire::OrderStatusAnswer as A;
        *self.status_calls.lock().unwrap() += 1;
        if self.status_fails {
            return Err(());
        }
        Ok(match (self.status_oid, self.status_unknown) {
            (Some(oid), _) => A::Open(oid),
            (None, true) => A::UnknownOid,
            (None, false) => A::NotOpen,
        })
    }
    fn place(&self, action: &Action) -> Result<Value, VenueApiError> {
        if let Some(ledger) = &self.watched_ledger {
            self.dispatched_mid_post.lock().unwrap().push(ledger.provably_dispatched("c1", None));
        }
        self.actions.lock().unwrap().push(action.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(json!({"status":"ok","response":{"data":{"statuses":[]}}})))
    }
    fn mid(&self, _coin: &str) -> Option<f64> {
        self.mid
    }
    fn network(&self) -> vike_model::VenueNetwork {
        if self.mainnet {
            vike_model::VenueNetwork::Mainnet
        } else {
            vike_model::VenueNetwork::Testnet
        }
    }
    fn master(&self) -> &str {
        "0xmaster"
    }
}

/// An `ExecutionState` at the venue's historical band — so every pre-existing test below keeps
/// asserting the exact prices it always did.
fn state(exchange: FakeExchange, events: EventSender) -> ExecutionState<FakeExchange> {
    state_with_slippage(exchange, events, DEFAULT_MARKET_SLIPPAGE)
}

fn state_with_slippage(
    exchange: FakeExchange,
    events: EventSender,
    slippage: f64,
) -> ExecutionState<FakeExchange> {
    ExecutionState {
        exchange,
        registry: OrderClientIdVenueRegistry::new(),
        events,
        orders: HashMap::new(),
        builder: None,
        slippage,
        margin: crate::account::modes::MarginGuard::default(),
        ledger: Arc::default(),
    }
}

fn limit_request(order_client_id: &str, side: i32, price: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Limit,
        price: Some(price),
        ..Default::default()
    }
}

/// A "stop" request: `trigger` is the activation price; `limit = None` ⇒ stop-MARKET, `Some(px)`
/// ⇒ stop-LIMIT.
fn stop_request(
    order_client_id: &str,
    side: i32,
    trigger: f64,
    limit: Option<f64>,
    qty: f64,
) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side,
        qty,
        order_type: vike_model::OrderType::Stop,
        price: limit,
        trigger_price: Some(trigger),
        ..Default::default()
    }
}

fn kind(e: &Event) -> &'static str {
    match e {
        Event::OrderSubmitted(_) => "Submitted",
        Event::OrderAccepted(_) => "Accepted",
        Event::OrderRejected(_) => "Rejected",
        Event::OrderCanceled(_) => "Canceled",
        Event::OrderCancelRejected(_) => "CancelRejected",
        Event::OrderModified(_) => "Modified",
        Event::OrderModifyRejected(_) => "ModifyRejected",
        Event::OrderFilled(_) => "Filled",
        Event::OrderPartiallyFilled(_) => "PartiallyFilled",
        Event::Fill(_) => "Fill",
        _ => "other",
    }
}

#[test]
fn submit_batch_is_one_native_order_action() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
            "statuses":[{"resting":{"oid":11}},{"resting":{"oid":12}}]
        }}}))])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let reqs = vec![limit_request("c1", 1, 50000.0, 0.01), limit_request("c2", -1, 51000.0, 0.02)];
    st.submit(&symbology(), &reqs);

    let actions = st.exchange.actions.lock().unwrap();
    assert_eq!(actions.len(), 1, "native batch: ONE action for N orders");
    match &actions[0] {
        Action::Order(a) => {
            assert_eq!(a.orders.len(), 2);
            assert_eq!(a.grouping, "na");
            assert_eq!(a.orders[0].asset, 0); // BTC = universe index 0
            assert!(a.orders[0].is_buy);
            assert!(!a.orders[1].is_buy);
            assert_eq!(a.orders[0].limit_px, "50000");
            assert_matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Gtc");
            assert!(a.orders[0].cloid.is_some(), "cloid derived + set on the wire");
        }
        other => panic!("expected an Order action, got {other:?}"),
    }
    drop(actions);

    let evs = drain_ready(|| rx.try_recv().ok());
    assert_eq!(
        evs.iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Submitted", "Accepted", "Accepted"]
    );
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11), "resting oid captured for modify");
}

/// **Any core perp is placeable (decision 0174), proven on the wire builder**: an order on a coin
/// other than the first is signed on THAT coin's asset index with its own `szDecimals` size and its
/// own price clamp (five significant figures, then `6 - szDecimals` decimals), its cancel finds the
/// same asset by order client id, and a coin the loaded symbology lacks is a terminal local reject with nothing
/// on the wire (never an order on another coin's asset).
#[test]
fn an_order_on_another_core_coin_is_signed_on_its_own_asset_and_cancels_there() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{
                "statuses":[{"resting":{"oid":21}},{"resting":{"oid":22}}]}}})),
            Ok(json!({"status":"ok","response":{"data":{"statuses":["success"]}}})),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let mut eth = limit_request("c-eth", 1, 3000.123456, 0.123456);
    eth.symbol = "ETH".into();
    let mut nope = limit_request("c-nope", 1, 1.0, 1.0);
    nope.symbol = "kNOPE".into();
    st.submit(&symbology(), &[limit_request("c-btc", 1, 50000.0, 0.01), eth, nope]);

    st.cancel(&["c-eth".to_string()]);
    let actions = st.exchange.actions.lock().unwrap();
    assert_eq!(actions.len(), 2, "the unknown coin put nothing on the wire: one order, one cancel");
    match &actions[0] {
        Action::Order(a) => {
            let wires: Vec<(u32, &str, &str)> =
                a.orders.iter().map(|o| (o.asset, o.limit_px.as_str(), o.sz.as_str())).collect();
            // BTC is universe index 0 (szDecimals 5), ETH index 1 (szDecimals 4: size floored to
            // 4 dp, price rounded to 5 significant figures, then to 6 - 4 = 2 decimals).
            assert_eq!(wires, vec![(0, "50000", "0.01"), (1, "3000.1", "0.1234")]);
        }
        other => panic!("expected an Order action, got {other:?}"),
    }
    match &actions[1] {
        Action::CancelByCloid(c) => {
            let got: Vec<u32> = c.cancels.iter().map(|x| x.asset).collect();
            assert_eq!(
                got,
                vec![1],
                "the cancel is keyed by the ETH asset the order was placed on"
            );
        }
        other => panic!("expected a CancelByCloid, got {other:?}"),
    }
    drop(actions);
    let evs = drain_ready(|| rx.try_recv().ok());
    let nope_reject = evs.iter().find_map(|e| match e {
        Event::OrderRejected(r) if r.order_client_id == "c-nope" => Some(r.reason.clone()),
        _ => None,
    });
    assert!(
        nope_reject.is_some_and(|r| r.contains("unknown symbol kNOPE")),
        "a coin the symbology lacks is a terminal reject: {evs:?}"
    );
}

/// The exec-level wiring twin of `signing::action::tests::
/// order_wire_carries_builder_when_configured_and_omits_when_not` — proves `ExecutionState::submit`
/// actually carries `self.builder` onto the sent `Action`, and that the default `None` (no
/// `.env` config) omits it entirely, exactly like before this field existed.
#[test]
fn submit_carries_the_configured_builder_and_omits_it_when_unset() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.builder = Some(HyperliquidBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 });
    st.submit(&symbology(), &[limit_request("c1", 1, 50000.0, 0.01)]);
    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => assert_eq!(
            a.builder,
            Some(HyperliquidBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 })
        ),
        other => panic!("expected an Order action, got {other:?}"),
    }
    drop(actions);

    let (tx2, _rx2) = event_channel(64);
    let mut st2 = state(FakeExchange::default(), tx2); // builder: None (the default state)
    st2.submit(&symbology(), &[limit_request("c2", 1, 50000.0, 0.01)]);
    let actions2 = st2.exchange.actions.lock().unwrap();
    match &actions2[0] {
        Action::Order(a) => {
            assert!(a.builder.is_none(), "unconfigured ⇒ no builder on the wire")
        }
        other => panic!("expected an Order action, got {other:?}"),
    }
}

#[test]
fn market_order_is_emulated_ioc_at_slippage() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        mid: Some(50000.0),
        responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
            "statuses":[{"filled":{"totalSz":"0.01","avgPx":"52000","oid":9}}]
        }}}))])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let req = OrderRequest {
        order_client_id: "m1".into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 0.01,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    st.submit(&symbology(), std::slice::from_ref(&req));

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => {
            assert_matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Ioc");
            // buy mid*1.05 = 52500, clamped to the BTC grid.
            assert_eq!(a.orders[0].limit_px, "52500");
        }
        other => panic!("{other:?}"),
    }
    drop(actions);
    // `filled` is the venue's whole answer to this order: the accept is said ahead of the fill
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Accepted", "Filled"]
    );
}

// --- An order the venue fills in its ANSWER carries its own accept ------------------------------------
//
// the CI box 2026-10-24 (the b6 drill, testnet account, a crossing limit): the venue answered the place with
// `filled`, the bridge sent `OrderSubmitted` and then `OrderFilled`, and the engine's order table has no
// `Submitted -> Filled` edge: `terminal event dropped on a live order ... status="SUBMITTED"`, the order
// stayed SUBMITTED for good and the next reconcile pass held an `OrphanLocalOrder` for it.

fn market_buy(order_client_id: &str, qty: f64) -> OrderRequest {
    OrderRequest {
        order_client_id: order_client_id.into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    }
}

/// The venue's whole answer to a place that crossed the book: `filled`, and no `resting` row.
fn filled_answer(total_sz: &str, oid: u64) -> Value {
    json!({"status":"ok","response":{"data":{"statuses":[
        {"filled":{"totalSz":total_sz,"avgPx":"52000","oid":oid}}]}}})
}

/// `reqs` submitted as one batch against a fake venue (mid 50000) that answers the place with
/// `answer`: what exec put on the lane, in order, and the exec state left behind.
fn submit_against(
    reqs: &[OrderRequest],
    answer: Value,
) -> (Vec<Event>, ExecutionState<FakeExchange>) {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        mid: Some(50000.0),
        responses: Mutex::new(VecDeque::from([Ok(answer)])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), reqs);
    (drain_ready(|| rx.try_recv().ok()), st)
}

/// The WS pump's part of the same fill through the REAL mapper: the `userFills` row (the bare `Fill`:
/// the fee, and the only thing that moves the position) and the `orderUpdates` `filled` row.
fn websocket_instant_fill(
    st: &ExecutionState<FakeExchange>,
    order_client_id: &str,
    oid: u64,
) -> (Event, Event) {
    let cloid = cloid_from_order_client_id(order_client_id);
    let frame = json!({"channel":"userFills","data":{"isSnapshot":false,"fills":[{
        "coin":"BTC","px":"52000","sz":"0.01","side":"B","time":3,"oid":oid,"cloid":cloid,
        "tid":777,"crossed":true,"fee":"0.1","feeToken":"USDC"}]}});
    let mut fills = crate::execution::user_data::map_frame_to_events(
        &frame,
        &symbology(),
        &st.registry,
        &AtomicBool::new(true),
        0,
    );
    let mut wraps = websocket_row(st, order_client_id, oid, "filled");
    assert_eq!((fills.len(), wraps.len()), (1, 1), "one bare Fill and one wrap");
    (fills.remove(0), wraps.remove(0))
}

/// An engine mounted on BTC that holds `req` as the core registers a submit (`Initialized`), with
/// `events` folded through it in arrival order.
fn fold(req: &OrderRequest, events: &[Event]) -> vike_execution::Engine<RecordingClient> {
    use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
    let mut engine = Engine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(vike_model::RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        "BTC",
    );
    engine.registry.insert(req.order_client_id.clone(), vike_model::OrderRecord::new(req.clone()));
    let mut outbox = EventBusOutbox::default();
    for event in events {
        engine.on_event(event, &mut outbox);
    }
    engine
}

/// THE FIX. The instant fill's lane order is `Submitted`, `Accepted` (naming the venue's oid), the
/// wrap: the order walks the table's own edges and ends `Filled` with the venue's quantity and
/// average price, the position moves on the WS `userFills` row, and nothing is refused. The WS rows
/// of the same fill (the bare `Fill`; the `orderUpdates` wrap, the same oid) are the second delivery
/// of the wrap and are dropped by the engine's dedup, so the response's own average stands.
#[test]
fn an_order_filled_in_the_venues_answer_is_accepted_first_and_ends_filled() {
    let req = market_buy("m1", 0.01);
    let (mut events, st) = submit_against(std::slice::from_ref(&req), filled_answer("0.01", 9));
    assert_eq!(events.iter().map(kind).collect::<Vec<_>>(), ["Submitted", "Accepted", "Filled"]);
    assert_matches!(&events[1], Event::OrderAccepted(a)
        if a.order_client_id == "m1" && a.venue_order_id.as_deref() == Some("9"));
    // the instantly filled order is not a resting one: a modify of it still has no oid to aim at
    assert_eq!(st.orders["m1"].oid, None);

    let (fill, wrap) = websocket_instant_fill(&st, "m1", 9);
    events.extend([fill, wrap]);
    let engine = fold(&req, &events);
    let order = &engine.registry["m1"];
    assert_eq!(order.status, OrderStatus::Filled);
    assert_eq!(order.filled_qty, 0.01);
    assert!((order.average_fill_price - 52000.0).abs() < 1e-6, "avg {}", order.average_fill_price);
    assert_eq!(order.venue_order_id.as_deref(), Some("9"));
    assert_eq!(engine.dropped_terminal_on_live, 0, "no terminal refused on a live order");
    assert!(order.deferred_events.is_empty());
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 0.01);
}

/// THE CONTROL: the lane order before the fix (no accept). It documents what the accept cures and
/// proves the harness CAN strand the order: `Submitted + OrderFilled` is not an edge, so the order
/// rests SUBMITTED with nothing filled while the account (the bare `Fill`) holds the position.
#[test]
fn without_the_accept_the_filled_order_is_stranded_submitted() {
    let req = market_buy("m1", 0.01);
    let (events, st) = submit_against(std::slice::from_ref(&req), filled_answer("0.01", 9));
    let (fill, wrap) = websocket_instant_fill(&st, "m1", 9);
    let mut inverted: Vec<Event> =
        events.into_iter().filter(|e| !matches!(e, Event::OrderAccepted(_))).collect();
    inverted.extend([fill, wrap]);
    let engine = fold(&req, &inverted);
    let order = &engine.registry["m1"];
    assert_eq!(order.status, OrderStatus::Submitted);
    assert_eq!(order.filled_qty, 0.0);
    assert!(engine.dropped_terminal_on_live > 0);
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 0.01);
}

/// An IOC that fills only part of its size is answered `filled` with a smaller `totalSz`: accepted
/// first too, then `PartiallyFilled` with the filled part.
#[test]
fn a_partly_filled_answer_is_accepted_first_and_ends_partially_filled() {
    let req = market_buy("m1", 0.02);
    let (events, _st) = submit_against(std::slice::from_ref(&req), filled_answer("0.005", 9));
    assert_eq!(
        events.iter().map(kind).collect::<Vec<_>>(),
        ["Submitted", "Accepted", "PartiallyFilled"]
    );
    let engine = fold(&req, &events);
    let order = &engine.registry["m1"];
    assert_eq!(order.status, OrderStatus::PartiallyFilled);
    assert_eq!(order.filled_qty, 0.005);
    assert_eq!(engine.dropped_terminal_on_live, 0);
}

/// ONE native batch whose slots are answered differently: the accept is said for the filled slot only
/// (the resting slot's own `resting` row is its accept), each slot's events keep their slot order, and
/// only the resting order has an oid for a modify to aim at, as before.
#[test]
fn only_the_filled_slot_of_a_batch_gets_a_synthesized_accept() {
    let resting = limit_request("c1", 1, 50000.0, 0.01);
    let instant = market_buy("c2", 0.01);
    let answer = json!({"status":"ok","response":{"data":{"statuses":[
        {"resting":{"oid":11}},
        {"filled":{"totalSz":"0.01","avgPx":"52000","oid":12}}]}}});
    let (events, mut st) = submit_against(&[resting.clone(), instant.clone()], answer);
    assert_eq!(
        events.iter().map(kind).collect::<Vec<_>>(),
        ["Submitted", "Submitted", "Accepted", "Accepted", "Filled"]
    );
    let accepted: Vec<(&str, Option<&str>)> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderAccepted(a) => {
                Some((a.order_client_id.as_str(), a.venue_order_id.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(accepted, [("c1", Some("11")), ("c2", Some("12"))]);
    assert_eq!(st.orders["c1"].oid, Some(11), "the resting order's oid is captured as before");
    assert_eq!(st.orders["c2"].oid, None, "the filled order's is not");

    // and a modify of the filled one is still refused locally, nothing sent
    let (tx, mut rx) = event_channel(8);
    st.events = tx;
    st.modify(&symbology(), &instant, None, Some(50500.0));
    assert_matches!(drain_ready(|| rx.try_recv().ok()).as_slice(), [Event::OrderModifyRejected(m)]
        if m.reason.contains("no resting venue order"));
    assert_eq!(st.exchange.actions.lock().unwrap().len(), 1, "only the place was sent");
}

/// The residual the accept does NOT close, pinned: the WS `filled` row can still beat the response to
/// the engine (an HTTP answer and a socket push race). The engine refuses that first wrap on the
/// `Submitted` order and counts it (a warning in the log), and the response's own `Accepted` and wrap
/// then land the order `Filled` at the venue's own average. Correct state, one counted refusal.
#[test]
fn a_websocket_wrap_that_overtakes_the_answer_costs_one_counted_refusal_and_not_the_order() {
    let req = market_buy("m1", 0.01);
    let (events, st) = submit_against(std::slice::from_ref(&req), filled_answer("0.01", 9));
    let (fill, wrap) = websocket_instant_fill(&st, "m1", 9);
    // Submitted, then the socket's rows, then the answer's Accepted and wrap
    let mut raced = vec![events[0].clone(), fill, wrap];
    raced.extend(events[1..].iter().cloned());
    let engine = fold(&req, &raced);
    let order = &engine.registry["m1"];
    assert_eq!(order.status, OrderStatus::Filled);
    assert_eq!(order.filled_qty, 0.01);
    assert!(
        (order.average_fill_price - 52000.0).abs() < 1e-6,
        "the answer's average, not the row's limit"
    );
    assert_eq!(engine.dropped_terminal_on_live, 1, "the early wrap, counted");
    assert_eq!(engine.position_size(vike_model::events::PositionSide::Both), 0.01);
}

/// THE property that makes this safe to ship onto a live account: an operator who configures
/// nothing gets the exact band this adapter has always priced with. NO network — `market_slippage_for`
/// is the pure decision the composition root calls once, before anything is spawned.
#[test]
fn an_unset_band_is_the_historical_five_percent() {
    assert_eq!(market_slippage_for(None), DEFAULT_MARKET_SLIPPAGE);
    assert_eq!(market_slippage_for(None), 0.05, "the historical hardcoded MARKET_SLIPPAGE");
}

/// Drift alarm: the venue's own literal must itself be a legal band. If a future edit moves
/// `DEFAULT_MARKET_SLIPPAGE` outside the workspace bounds, the unset path would be pricing at a
/// value the config edge would refuse — the two must not be able to disagree.
#[test]
fn the_venue_default_is_within_the_workspace_bounds() {
    assert!(vike_model::market_slippage::is_usable_market_slippage(DEFAULT_MARKET_SLIPPAGE));
    // The ceiling IS this literal, which is what makes the knob a one-way ratchet.
    assert_eq!(DEFAULT_MARKET_SLIPPAGE, vike_model::market_slippage::MAX_MARKET_SLIPPAGE);
}

/// A configured band can only tighten: no value reaches the wire wider than the historical one.
#[test]
fn no_configured_band_can_price_more_aggressively_than_the_default() {
    for absurd in [0.5, 50.0, f64::INFINITY, f64::NAN, -1.0] {
        assert!(
            market_slippage_for(Some(absurd)) <= DEFAULT_MARKET_SLIPPAGE,
            "{absurd} widened the band"
        );
    }
    assert_eq!(market_slippage_for(Some(0.002)), 0.002, "a usable band rides through");
}

/// The tightened band on the real build path, BOTH sides — a buy prices UP, a sell prices DOWN.
/// Mid 50000 at 0.2% ⇒ buy 50100 / sell 49900 (vs 52500 / 47500 at the historical 5%).
#[test]
fn a_tightened_band_prices_both_sides_off_the_mid() {
    for (side, expected) in [(1i32, "50100"), (-1, "49900")] {
        let (tx, _rx) = event_channel(64);
        let fake = FakeExchange { mid: Some(50000.0), ..Default::default() };
        let mut st = state_with_slippage(fake, tx, 0.002);
        let req = OrderRequest {
            order_client_id: "tight".into(),
            venue: VENUE.into(),
            symbol: "BTC".into(),
            side,
            qty: 0.01,
            order_type: vike_model::OrderType::Market,
            ..Default::default()
        };
        st.submit(&symbology(), std::slice::from_ref(&req));
        let actions = st.exchange.actions.lock().unwrap();
        match &actions[0] {
            Action::Order(a) => {
                assert_eq!(a.orders[0].is_buy, side > 0);
                // Still an Ioc — only the band moved, never the order construction.
                assert_matches!(&a.orders[0].order_type, OrderKind::Limit(l) if l.tif == "Ioc");
                assert_eq!(a.orders[0].limit_px, expected, "side {side}");
            }
            other => panic!("{other:?}"),
        }
    }
}

/// The band bounds the PROTECTIVE stop too — the second, easily-forgotten site. A sell stop at
/// trigger 48000 with a 0.2% band prices at 47904, not the historical 45600.
#[test]
fn a_tightened_band_also_bounds_the_protective_stop_market() {
    let (tx, _rx) = event_channel(64);
    let mut st = state_with_slippage(FakeExchange::default(), tx, 0.002);
    let req = stop_request("s-tight", -1, 48000.0, None, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => {
            match &a.orders[0].order_type {
                // The trigger LEVEL is untouched — only the protective bound moved.
                OrderKind::Trigger(t) => {
                    assert!(t.is_market);
                    assert_eq!(t.trigger_px, "48000");
                    assert_eq!(t.tpsl, "sl");
                }
                other => panic!("expected a Trigger, got {other:?}"),
            }
            assert_eq!(a.orders[0].limit_px, "47904");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn market_order_without_a_mid_is_rejected_locally() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange { mid: None, ..Default::default() }, tx);
    let req = OrderRequest {
        order_client_id: "m2".into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side: 1,
        qty: 0.01,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    };
    st.submit(&symbology(), std::slice::from_ref(&req));
    assert!(
        st.exchange.actions.lock().unwrap().is_empty(),
        "no action placed when the build fails"
    );
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Rejected"]
    );
}

#[test]
fn stop_market_order_builds_trigger_kind_at_slippage() {
    let (tx, mut rx) = event_channel(64);
    // The venue parks a stop until it trips → "waitingForTrigger" (mapped to OrderAccepted).
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
            "statuses":["waitingForTrigger"]
        }}}))])),
        ..Default::default() // mid: None — a stop needs NO mid (unlike the emulated market)
    };
    let mut st = state(fake, tx);
    // A protective SELL stop for a long: trigger 48000, no limit ⇒ stop-MARKET.
    let req = stop_request("s1", -1, 48000.0, None, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));

    let actions = st.exchange.actions.lock().unwrap();
    assert_eq!(actions.len(), 1, "one native order action, no mid fetch needed");
    match &actions[0] {
        Action::Order(a) => {
            assert_eq!(a.orders.len(), 1);
            assert!(!a.orders[0].is_buy);
            match &a.orders[0].order_type {
                OrderKind::Trigger(t) => {
                    assert!(t.is_market, "no limit price ⇒ stop-MARKET (is_market=true)");
                    assert_eq!(t.trigger_px, "48000");
                    assert_eq!(t.tpsl, "sl", "a plain stop is a stop-loss");
                }
                other => panic!("expected an OrderKind::Trigger, got {other:?}"),
            }
            // stop-MARKET limit_px = trigger*(1-5% slippage) for a sell = 45600 (aggressive bound).
            assert_eq!(a.orders[0].limit_px, "45600");
        }
        other => panic!("expected an Order action, got {other:?}"),
    }
    drop(actions);
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Accepted"]
    );
}

#[test]
fn stop_limit_order_builds_trigger_kind_with_resting_limit() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    // A SELL stop-LIMIT: trigger 48000, rest at limit 47900 once tripped.
    let req = stop_request("s2", -1, 48000.0, Some(47900.0), 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => match &a.orders[0].order_type {
            OrderKind::Trigger(t) => {
                assert!(!t.is_market, "a limit price present ⇒ stop-LIMIT (is_market=false)");
                assert_eq!(t.trigger_px, "48000");
                assert_eq!(t.tpsl, "sl");
                // rests at the requested limit, NOT a slipped bound.
                assert_eq!(a.orders[0].limit_px, "47900");
            }
            other => panic!("expected an OrderKind::Trigger, got {other:?}"),
        },
        other => panic!("expected an Order action, got {other:?}"),
    }
}

/// **A REAL tie between the adapter and its `VenueCapabilities` row** — the deribit
/// `modify_answers_with_an_event_and_the_capabilities_say_it_is_supported` pattern: drive the actual code path, then assert the
/// declaration, in ONE test. (The crate's `capabilities_test` module cannot do this: every assertion
/// there compares fields of the SAME constant the row defines, so it is a restatement.)
///
/// The bug this pins: `HYPERLIQUID.supported_order_kinds` omitted `"stop_limit"`, so
/// `vike_venues::preflight_order` returned `TRIGGER_UNSUPPORTED` and `vike-core` terminally
/// REJECTED the order — while `build_order_wire` builds it perfectly. The sibling test above
/// proves the wire SHAPE using `order_type: "stop"`; this one uses the literal `"stop_limit"`
/// spelling, because that string is what the preflight classifies. The routing keys off
/// `trigger_price.is_some()`, not the spelling, which is exactly why the two never disagreed
/// on the wire and the gap could hide in the declaration alone.
#[test]
fn stop_limit_kind_is_declared_and_built() {
    // 1. The DECLARATION: the row must list the kind, or the core edge refuses the order.
    let capabilities = vike_venues::capabilities_for("hyperliquid");
    assert!(
        capabilities.supported_order_kinds.contains(&"stop_limit"),
        "the row must declare stop_limit — preflight_order refuses undeclared trigger kinds"
    );
    // ...and the core-edge preflight must therefore ADMIT it.
    let mut pf = stop_request("pf1", -1, 48_000.0, Some(47_900.0), 0.01);
    pf.order_type = vike_model::OrderType::StopLimit;
    assert_eq!(vike_venues::preflight_order(&pf), Ok(()), "core edge must admit stop_limit");

    // 2. The REALITY: the same request through the real `build_order_wire` path rests as a
    //    stop-LIMIT at the requested price (is_market=false), not a slipped market bound.
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.submit(&symbology(), std::slice::from_ref(&pf));
    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => match &a.orders[0].order_type {
            OrderKind::Trigger(t) => {
                assert!(!t.is_market, "price present ⇒ resting stop-LIMIT");
                assert_eq!(t.trigger_px, "48000");
                assert_eq!(t.tpsl, "sl", "stop_limit is a stop-LOSS, not a take-profit");
                assert_eq!(a.orders[0].limit_px, "47900", "rests at the requested limit");
            }
            other => panic!("expected an OrderKind::Trigger, got {other:?}"),
        },
        other => panic!("expected an Order action, got {other:?}"),
    }
}

#[test]
fn take_profit_order_builds_trigger_kind_with_tp_tpsl() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Ok(json!({"status":"ok","response":{"data":{
            "statuses":["waitingForTrigger"]
        }}}))])),
        ..Default::default() // a take-profit needs NO mid either — it references its own trigger
    };
    let mut st = state(fake, tx);
    // A take-profit SELL (lock a long's gain): order_type "take_profit", trigger 70000, no limit
    // ⇒ tp-MARKET. Same trigger machinery as a stop, only `tpsl` flips to "tp".
    let mut req = stop_request("tp1", -1, 70000.0, None, 0.01);
    req.order_type = vike_model::OrderType::TakeProfit;
    st.submit(&symbology(), std::slice::from_ref(&req));

    let actions = st.exchange.actions.lock().unwrap();
    assert_eq!(actions.len(), 1, "one native order action, no mid fetch needed");
    match &actions[0] {
        Action::Order(a) => match &a.orders[0].order_type {
            OrderKind::Trigger(t) => {
                assert!(t.is_market, "no limit price ⇒ tp-MARKET (is_market=true)");
                assert_eq!(t.trigger_px, "70000");
                assert_eq!(t.tpsl, "tp", "order_type take_profit ⇒ tpsl tp");
                // tp-MARKET sell limit_px = trigger*(1-5%) = 66500 (same aggressive bound).
                assert_eq!(a.orders[0].limit_px, "66500");
            }
            other => panic!("expected an OrderKind::Trigger, got {other:?}"),
        },
        other => panic!("expected an Order action, got {other:?}"),
    }
    drop(actions);
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Accepted"]
    );
}

/// **The wire of every shape that works today, byte for byte**, serialized as it is signed: the native
/// stop-limit path and its neighbours. The guard that refuses every other kind must not move one of them.
#[test]
fn the_wire_of_every_working_kind_is_pinned_byte_for_byte() {
    use vike_model::OrderType::{Limit, Market, Stop, StopLimit, TakeProfit};
    let with = |ty, price: Option<f64>, trigger: Option<f64>| OrderRequest {
        order_client_id: "pin".into(),
        venue: VENUE.into(),
        symbol: "BTC".into(),
        side: -1,
        qty: 0.01,
        order_type: ty,
        price,
        trigger_price: trigger,
        ..Default::default()
    };
    let stop_loss = |is_market: bool, px: &str| {
        format!(
            r#""p":"{px}","s":"0.01","r":false,"t":{{"trigger":{{"isMarket":{is_market},"triggerPx":"48000","tpsl":"sl"}}}}"#
        )
    };
    let take_profit = |is_market: bool, px: &str| {
        format!(
            r#""p":"{px}","s":"0.01","r":false,"t":{{"trigger":{{"isMarket":{is_market},"triggerPx":"70000","tpsl":"tp"}}}}"#
        )
    };
    let cases = [
        (
            with(Market, None, None),
            r#""p":"47500","s":"0.01","r":false,"t":{"limit":{"tif":"Ioc"}}"#.to_string(),
        ),
        (
            with(Limit, Some(47_000.0), None),
            r#""p":"47000","s":"0.01","r":false,"t":{"limit":{"tif":"Gtc"}}"#.to_string(),
        ),
        (with(Stop, None, Some(48_000.0)), stop_loss(true, "45600")),
        (with(Stop, Some(47_900.0), Some(48_000.0)), stop_loss(false, "47900")),
        (with(StopLimit, Some(47_900.0), Some(48_000.0)), stop_loss(false, "47900")),
        (with(TakeProfit, None, Some(70_000.0)), take_profit(true, "66500")),
        (with(TakeProfit, Some(69_900.0), Some(70_000.0)), take_profit(false, "69900")),
        // a market or a limit that carries a trigger is a stop, as it always was
        (with(Market, None, Some(48_000.0)), stop_loss(true, "45600")),
        (with(Limit, Some(47_900.0), Some(48_000.0)), stop_loss(false, "47900")),
    ];
    let cloid = crate::execution::event_mapper::cloid_from_order_client_id("pin");
    for (req, body) in cases {
        let (tx, _rx) = event_channel(64);
        let mut st = state(FakeExchange { mid: Some(50_000.0), ..Default::default() }, tx);
        st.submit(&symbology(), std::slice::from_ref(&req));
        let actions = st.exchange.actions.lock().unwrap();
        let Some(Action::Order(a)) = actions.first() else {
            panic!("{:?}: expected one Order action, got {actions:?}", req.order_type)
        };
        assert_eq!(
            serde_json::to_string(&a.orders[0]).expect("an order wire serializes"),
            format!(r#"{{"a":0,"b":false,{body},"c":"{cloid}"}}"#),
            "{:?} price {:?} trigger {:?}",
            req.order_type,
            req.price,
            req.trigger_price
        );
    }
}

/// A kind this adapter does not build, and a stop-limit short of either price, is a terminal reject that
/// NAMES the kind, with nothing sent. The stop-limit used to rest as a plain limit (no trigger) or go out
/// as a stop-MARKET (no limit). A trailing stop is refused even in the shape the venue documents (an
/// activation, a trail in price, `trigger_by` Mark): its action carries no cloid (`execution/state.rs`'s
/// `trailing_refusal`); a trailing limit has no venue form.
#[test]
fn a_kind_it_does_not_build_or_a_stop_limit_short_of_a_price_is_refused_by_name() {
    let mut unknown = stop_request("u1", -1, 48_000.0, Some(47_900.0), 0.01);
    unknown.order_type = vike_model::OrderType::Unrecognized("market_if_touched".into());
    let mut no_trigger = limit_request("u2", -1, 47_900.0, 0.01);
    no_trigger.order_type = vike_model::OrderType::StopLimit;
    let mut no_limit = stop_request("u3", -1, 48_000.0, None, 0.01);
    no_limit.order_type = vike_model::OrderType::StopLimit;
    let mut trailing = stop_request("u4", -1, 52_000.0, None, 0.01);
    trailing.order_type = vike_model::OrderType::Trailing;
    trailing.trail = Some(500.0);
    trailing.trigger_by = Some(vike_model::TriggerBy::Mark);
    let mut trailing_limit = trailing.clone();
    trailing_limit.order_client_id = "u5".into();
    trailing_limit.order_type = vike_model::OrderType::TrailingLimit;
    trailing_limit.limit_offset = Some(10.0);
    for (req, why) in [
        (unknown, "unsupported order_type `market_if_touched`"),
        (no_trigger, "stop_limit needs a trigger_price and a limit price"),
        (no_limit, "stop_limit needs a trigger_price and a limit price"),
        (trailing, "carries no order client id (cloid)"),
        (trailing_limit, "no trailing limit order"),
    ] {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange { mid: Some(50_000.0), ..Default::default() }, tx);
        st.submit(&symbology(), std::slice::from_ref(&req));
        assert!(st.exchange.actions.lock().unwrap().is_empty(), "{why}: nothing is sent");
        let evs = drain_ready(|| rx.try_recv().ok());
        assert_eq!(evs.iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
        match &evs[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, req.order_client_id);
                assert!(r.reason.contains(why), "wanted `{why}`, got `{}`", r.reason);
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
    }
}

/// Trigger-source law (`vike_bridge_core::trigger`, hyperliquid row): HL has NO trigger-by
/// field and evaluates triggers against MARK by venue law — a requested Last/Index is a LOUD
/// local reject (the wire is never touched), a requested Mark proceeds identically to `None`,
/// and `None` stays byte-identical as ever.
#[test]
fn trigger_by_mark_matches_venue_law_and_last_index_are_denied() {
    // Mark: accepted — same action as an unrequested stop.
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let mut req = stop_request("s-mark", -1, 48000.0, None, 0.01);
    req.trigger_by = Some(vike_model::TriggerBy::Mark);
    st.submit(&symbology(), std::slice::from_ref(&req));
    assert_eq!(st.exchange.actions.lock().unwrap().len(), 1, "Mark IS the venue law");
    assert!(!drain_ready(|| rx.try_recv().ok()).iter().map(kind).any(|k| k == "Rejected"));

    // Last / Index: denied locally, terminal, no action placed — on stops AND take-profits.
    for (order_client_id, tb) in
        [("s-last", vike_model::TriggerBy::Last), ("s-index", vike_model::TriggerBy::Index)]
    {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        let mut req = stop_request(order_client_id, -1, 48000.0, None, 0.01);
        req.trigger_by = Some(tb);
        st.submit(&symbology(), std::slice::from_ref(&req));
        assert!(st.exchange.actions.lock().unwrap().is_empty(), "{tb:?}: wire never touched");
        let evs = drain_ready(|| rx.try_recv().ok());
        assert_eq!(evs.iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
        match &evs[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, order_client_id);
                assert!(r.reason.contains("MARK by venue law"), "{}", r.reason);
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
    }
}

#[test]
fn cancel_uses_native_cancel_by_cloid() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            Ok(json!({"status":"ok","response":{"data":{"statuses":["success"]}}})),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
    st.cancel(std::slice::from_ref(&"c1".to_string()));

    let actions = st.exchange.actions.lock().unwrap();
    assert_eq!(actions.len(), 2);
    match &actions[1] {
        Action::CancelByCloid(a) => {
            assert_eq!(a.cancels.len(), 1);
            assert_eq!(a.cancels[0].asset, 0);
        }
        other => panic!("expected a CancelByCloid action, got {other:?}"),
    }
    drop(actions);
    let evs = drain_ready(|| rx.try_recv().ok());
    assert!(evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "c1")));
}

#[test]
fn cancel_of_an_unknown_order_is_rejected_not_swallowed() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.cancel(std::slice::from_ref(&"ghost".to_string()));
    assert!(
        st.exchange.actions.lock().unwrap().is_empty(),
        "nothing to place for an unknown order client id"
    );
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderCancelRejected(e)] => assert_eq!(e.order_client_id, "ghost"),
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

#[test]
fn modify_is_native_cancel_replace_by_oid() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":22}}]}}})),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let req = limit_request("c1", 1, 50000.0, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    st.modify(&symbology(), &req, None, Some(50500.0));

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[1] {
        Action::Modify(m) => {
            assert_eq!(m.oid, 11, "cancel-replace keyed by the captured resting oid");
            assert_eq!(m.order.limit_px, "50500");
        }
        other => panic!("expected a Modify action, got {other:?}"),
    }
    drop(actions);
    let evs = drain_ready(|| rx.try_recv().ok());
    assert!(evs.iter().any(|e| matches!(
            e,
            Event::OrderModified(m) if m.order_client_id == "c1" && m.venue_order_id.as_deref() == Some("22")
        )));
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(22), "meta oid rolled to the new one");
}

/// A modify of a STOP moves the TRIGGER (the modify seam's one price is the trigger of a stop, as its
/// FSM reads it): the replacement goes out as a stop-MARKET with the new trigger and its slippage
/// bound, and NOT as a stop at the old trigger with a limit price (what the price used to become).
#[test]
fn modify_of_a_stop_moves_the_trigger_in_the_replacement() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":["waitingForTrigger"]}}})),
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":22}}]}}})),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let req = stop_request("s1", -1, 48000.0, None, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    // `oid` is learned from the venue's resting answer; the stop's `waitingForTrigger` carries none,
    // so give the order the oid a reconcile would (the modify needs a venue order to replace).
    st.orders.get_mut("s1").expect("submitted").oid = Some(11);
    st.modify(&symbology(), &req, None, Some(47000.0));

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[1] {
        Action::Modify(m) => {
            assert_eq!(m.oid, 11);
            match &m.order.order_type {
                OrderKind::Trigger(t) => {
                    assert_eq!(t.trigger_px, "47000", "the new trigger");
                    assert!(t.is_market, "still a stop-MARKET: no limit price appeared");
                }
                other => panic!("expected a Trigger, got {other:?}"),
            }
            // 47000 less the 5% slippage bound a sell stop-market carries
            assert_eq!(m.order.limit_px, "44650");
        }
        other => panic!("expected a Modify action, got {other:?}"),
    }
    drop(actions);
    assert!(
        drain_ready(|| rx.try_recv().ok()).iter().any(|e| matches!(e, Event::OrderModified(_)))
    );
}

#[test]
fn modify_without_a_resting_oid_is_rejected() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let req = limit_request("c1", 1, 50000.0, 0.01);
    st.modify(&symbology(), &req, None, Some(51000.0)); // never submitted → no oid
    assert!(st.exchange.actions.lock().unwrap().is_empty());
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderModifyRejected(e)] => assert_eq!(e.order_client_id, "c1"),
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
}

/// Submit `c1` (resting oid 11), then answer the modify with `reply`; return the events AFTER the
/// submit's own, plus the state (to read the rolled `oid`).
fn modify_with_reply(
    reply: Result<Value, VenueApiError>,
) -> (Vec<Event>, ExecutionState<FakeExchange>) {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            reply,
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let req = limit_request("c1", 1, 50000.0, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    let _ = drain_ready(|| rx.try_recv().ok()); // Submitted + Accepted
    st.modify(&symbology(), &req, None, Some(50500.0));
    (drain_ready(|| rx.try_recv().ok()), st)
}

/// `/exchange` answers HTTP 200 to a refused modify, so `place` returns `Ok`: a top-level
/// `{status:"err"}` must surface as a NON-terminal `OrderModifyRejected` with the venue's reason
/// (never an `OrderModified`), and the resting oid must NOT roll.
#[test]
fn modify_top_level_err_is_rejected_not_modified() {
    let (evs, st) =
        modify_with_reply(Ok(json!({"status":"err","response":"Order has zero size."})));
    match evs.as_slice() {
        [Event::OrderModifyRejected(e)] => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(e.reason.as_str(), "Order has zero size.");
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11), "a refused modify keeps the oid");
}

/// The per-order flavour of the same refusal: `statuses[0].error` inside an `ok` envelope.
#[test]
fn modify_status_error_is_rejected_not_modified() {
    let (evs, st) = modify_with_reply(Ok(
        json!({"status":"ok","response":{"type":"order","data":{"statuses":[{"error":"Cannot modify canceled or filled order"}]}}}),
    ));
    match evs.as_slice() {
        [Event::OrderModifyRejected(e)] => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(e.reason.as_str(), "Cannot modify canceled or filled order");
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11));
}

/// A replacement that crosses the book answers `filled`, which carries the new oid: the modify is
/// applied, the oid rolls, and the fill itself arrives through the WS lanes.
#[test]
fn modify_filled_status_is_applied_and_rolls_the_oid() {
    let (evs, st) = modify_with_reply(Ok(
        json!({"status":"ok","response":{"data":{"statuses":[{"filled":{"totalSz":"0.01","avgPx":"50500","oid":33}}]}}}),
    ));
    assert!(
        matches!(
            evs.as_slice(),
            [Event::OrderModified(m)] if m.venue_order_id.as_deref() == Some("33")
        ),
        "got {evs:?}"
    );
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(33));
}

/// A post-send timeout may have applied the replace: it is neither success nor a terminal failure,
/// and it must not be only a log line — one explicit NON-terminal advisory says the outcome is unknown.
#[test]
fn modify_ambiguous_timeout_is_an_explicit_unknown_advisory() {
    let before = vike_model::now_ms();
    let (evs, st) = modify_with_reply(Err(VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: "timed out".into(),
    }));
    match evs.as_slice() {
        [Event::OrderModifyRejected(e)] => {
            assert_eq!(e.order_client_id, "c1");
            assert_eq!(
                e.reason.as_str(),
                "modify outcome unknown (timed out after send; the venue may have applied it): timed out"
            );
            // the owner's 2026-11-12 ruling: stamped when the timeout was had, never at the order's own
            // time (this order's is 0)
            assert!(e.timestamp >= before, "{}", e.timestamp);
        }
        other => panic!("expected one OrderModifyRejected advisory, got {other:?}"),
    }
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11));
}

/// A definite send failure keeps the transport's words, stamped when it was had.
#[test]
fn modify_definite_send_failure_is_rejected_in_its_own_words() {
    let before = vike_model::now_ms();
    let (evs, st) =
        modify_with_reply(Err(VenueApiError { code: 0, msg: "connect refused".into() }));
    match evs.as_slice() {
        [Event::OrderModifyRejected(e)] => {
            assert_eq!((e.order_client_id.as_str(), e.reason.as_str()), ("c1", "connect refused"));
            assert!(e.timestamp >= before, "{}", e.timestamp);
        }
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11));
    assert!(
        st.orders.get("c1").unwrap().unknown_since.is_none(),
        "a definite failure leaves no doubt"
    );
}

#[test]
fn definite_transport_error_rejects_every_order() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Err(VenueApiError {
            code: 500,
            msg: "boom".into(),
        })])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(
        &symbology(),
        &[limit_request("c1", 1, 50000.0, 0.01), limit_request("c2", -1, 51000.0, 0.02)],
    );
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Submitted", "Rejected", "Rejected"],
        "a definite failure rejects every order (none vanishes)"
    );
}

#[test]
fn ambiguous_timeout_never_rejects() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Err(VenueApiError {
            code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
            msg: "timed out".into(),
        })])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted"],
        "the ambiguous timeout must NOT synthesize a reject (would strand a phantom position)"
    );
}

/// What the confirm's "absent" stands on (decision 0171): an order client id is dispatched once its POST RETURNED,
/// whatever came back, and not while it is in flight or when it never went out.
#[test]
fn an_order_client_id_is_dispatched_only_after_its_post_returned() {
    let timed_out = VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: "timed out".into(),
    };
    for answer in [
        Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
        Err(timed_out),
    ] {
        let (tx, _rx) = event_channel(64);
        let ledger = Arc::new(DispatchLedger::default());
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([answer])),
            watched_ledger: Some(ledger.clone()),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        st.ledger = ledger.clone();
        st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
        assert_eq!(*st.exchange.dispatched_mid_post.lock().unwrap(), [false], "in flight");
        assert!(ledger.provably_dispatched("c1", None), "the POST returned");
    }

    // Refused before the wire: nothing was sent, and the terminal is already said.
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 0, 50000.0, 0.01)));
    assert!(!st.ledger.provably_dispatched("c1", None), "never sent");
}

/// A cancel that timed out after the send may have landed or not: it is not a terminal, and it must
/// not be only a log line - one NON-terminal `OrderCancelRejected` per order client id says the outcome is unknown.
#[test]
fn cancel_ambiguous_timeout_is_an_explicit_unknown_advisory() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "timed out".into(),
            }),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
    let _ = drain_ready(|| rx.try_recv().ok());
    st.cancel(std::slice::from_ref(&"c1".to_string()));
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.order_client_id, "c1");
            assert!(e.reason.contains("cancel outcome unknown"), "reason: {}", e.reason);
        }
        other => panic!("expected one OrderCancelRejected advisory, got {other:?}"),
    }
}

/// The client the start builds (the shared actor with this venue's hooks over `registry`, `client.rs`'s
/// `with_venue_hooks`) on a command loop that hands every command but `Shutdown` to `commands`: the shape that
/// lets the halt arms and the restore seam be driven with no `Signer`, no private-WS pump and no dial. A
/// command the client queues lands on `commands`, in order; one it refuses never does.
fn client_over(
    events: EventSender,
    halt_path: Option<std::path::PathBuf>,
    registry: OrderClientIdVenueRegistry,
) -> (ExecutionClientActor, Receiver<ExecutionClientActorCommand>) {
    let (tx, commands) = channel();
    let actor = ExecutionClientActor::spawn("hyperliquid-exec-test", events, move |rx| {
        while let Ok(command) = rx.recv() {
            if matches!(command, ExecutionClientActorCommand::Shutdown) {
                break;
            }
            let _ = tx.send(command);
        }
    });
    let actor = super::client::with_venue_hooks(actor, registry);
    (if let Some(path) = halt_path { actor.with_halt_path(path) } else { actor }, commands)
}

/// [`client_over`] with a fresh registry.
fn halt_client(
    events: EventSender,
    halt_path: Option<std::path::PathBuf>,
) -> (ExecutionClientActor, Receiver<ExecutionClientActorCommand>) {
    client_over(events, halt_path, OrderClientIdVenueRegistry::new())
}

/// The next command the client queued (it crosses the actor's thread first).
fn queued(commands: &Receiver<ExecutionClientActorCommand>) -> ExecutionClientActorCommand {
    commands.recv_timeout(std::time::Duration::from_secs(5)).expect("a command reached the loop")
}

/// The order client id of a queued single submit.
fn submitted(command: ExecutionClientActorCommand) -> String {
    match command {
        ExecutionClientActorCommand::Submit(request, _) => request.order_client_id,
        _ => panic!("expected a single Submit"),
    }
}

/// `confirm` reaches the read this client was handed, on a worker of its own, and the answer lands on
/// the event lane, through the `Box<dyn>` the engine holds it by (a wrapper that left `confirm` out would
/// inherit the trait's no-op and nothing here would come back). Dropping the client joins the worker.
#[test]
fn confirm_runs_the_read_and_its_answer_lands_on_the_lane() {
    let (events, mut rx) = event_channel(64);
    let (client, _commands) = halt_client(events, None);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&asked);
    let client = client.with_confirm(Arc::new(move |order_client_id: &str| {
        seen.lock().unwrap().push(order_client_id.to_string());
        vec![Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some("77".to_string().into()),
            timestamp: 5,
        })]
    }));
    let mut boxed: Box<dyn ExecutionClient + Send> = Box::new(client);
    boxed.confirm("c1");
    drop(boxed);
    assert_eq!(*asked.lock().unwrap(), ["c1"]);
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        ["Accepted"]
    );
}

/// With no read wired `confirm` is the clean no-op; nothing lands.
#[test]
fn confirm_with_no_read_wired_is_a_no_op() {
    let (events, mut rx) = event_channel(64);
    let (mut client, _commands) = halt_client(events, None);
    client.confirm("c1");
    client.detach();
    assert!(drain_ready(|| rx.try_recv().ok()).is_empty());
}

/// The answers the watchdog's confirm exists for: a reply that names no status for an order, a short
/// list and a status word not known here are NO terminal. A terminal reject here is what stranded a
/// resting order; the confirm reads what the answer did not say.
#[test]
fn an_answer_that_says_nothing_about_an_order_rejects_nothing() {
    for (shape, answer) in [
        ("no statuses", json!({"status": "ok", "response": {"type": "order"}})),
        ("short list", json!({"status": "ok", "response": {"data": {"statuses": []}}})),
        (
            "unknown status",
            json!({"status": "ok", "response": {"data": {"statuses": [{"newWord": {}}]}}}),
        ),
    ] {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([Ok(answer)])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
        assert_eq!(
            drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
            vec!["Submitted"],
            "{shape}"
        );
    }
}

/// **This client's halt arms, driven for the first time** — and the decision-0099 property they
/// exist to prove: it watches the sentinel its START handed it (`with_halt_path`), not a file it
/// resolves for itself. Until now no test could reach `submit`'s, `submit_batch`'s or `modify`'s
/// refusal, because a client could not be built without a `Signer` and a dialled private-WS pump;
/// the builder seam is what made one constructible over a dead channel.
///
/// Four phases in ONE sentinel, each the other's control: engaged + OPENING is refused with the
/// shared wording and never reaches the exec thread; engaged + REDUCING is admitted (a halt never
/// traps a position); a MODIFY is refused with the non-terminal advisory; and with the sentinel
/// removed the same opening order is admitted, which a client that had simply stopped submitting
/// would fail.
#[test]
fn a_hyperliquid_client_watches_the_sentinel_it_was_handed() {
    let dir = vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-hl-halt")
        .expect("scratch dir for the sentinel");
    let sentinel = dir.path().join("HALT");
    let (events, mut rx) = event_channel(64);
    let (mut client, commands) = halt_client(events, Some(sentinel.clone()));
    std::fs::write(&sentinel, b"").expect("engage the sentinel");

    // 1. OPENING, halted: refused terminally, wording shared with every enforcing client.
    client.submit(&limit_request("opening", 1, 50000.0, 0.01));
    let events = drain_ready(|| rx.try_recv().ok());
    match events.as_slice() {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.order_client_id, "opening");
            assert_eq!(r.reason, vike_execution::halt::HALT_REJECT_REASON);
        }
        other => panic!("an opening submit under the handed sentinel must be refused: {other:?}"),
    }

    // 1b. …and a native BATCH is refused as a unit when any leg opens.
    client.submit_batch(&[
        limit_request("b1", 1, 50000.0, 0.01),
        limit_request("b2", -1, 51000.0, 0.02),
    ]);
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["Rejected", "Rejected"]
    );

    // 2. REDUCING, still halted: admitted, and said in the shared actor's line (F-6).
    let exit = OrderRequest { reduce_only: true, ..limit_request("exit", -1, 50000.0, 0.01) };
    let ((), log) = vike_log::capture::captured(|| client.submit(&exit));
    assert!(
        drain_ready(|| rx.try_recv().ok()).is_empty(),
        "an admitted submit emits nothing of its own here"
    );
    // The loop sees commands in order, so this first one also proves nothing refused above was queued.
    assert_eq!(
        submitted(queued(&commands)),
        "exit",
        "the reducing order goes out, and nothing before it"
    );
    let admits: Vec<_> = log.iter().filter(|e| e.target == "vike_bridge_core::halt").collect();
    let [admit] = admits.as_slice() else { panic!("one HALT admit line: {log:?}") };
    assert_eq!(admit.level, tracing::Level::WARN);
    assert_eq!(admit.field("order_client_id"), Some("exit"));
    assert!(admit.message.starts_with("HALT engaged: admitting a reduce_only submit"), "{admit:?}");

    // 3. MODIFY, halted: non-terminal advisory, nothing sent.
    client.modify(&limit_request("opening", 1, 50000.0, 0.01), Some(0.02), None);
    assert_eq!(
        drain_ready(|| rx.try_recv().ok()).iter().map(kind).collect::<Vec<_>>(),
        vec!["ModifyRejected"]
    );

    // 4. Sentinel removed: the SAME opening order is admitted (and the halted modify was never queued).
    std::fs::remove_file(&sentinel).expect("rm the sentinel");
    client.submit(&limit_request("resumed", 1, 50000.0, 0.01));
    assert_eq!(submitted(queued(&commands)), "resumed", "resumed trading goes out");
    assert!(drain_ready(|| rx.try_recv().ok()).is_empty());
}

/// A client handed NO path watches nothing — an engaged file elsewhere on the box (here: a path the
/// test engages and never gives the client) changes no verdict. The decision-0099 regression: the
/// process-wide fallback this client used to take is gone.
#[test]
fn a_hyperliquid_client_handed_no_sentinel_watches_nothing() {
    let dir = vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-hl-nohalt")
        .expect("scratch dir");
    let somewhere = dir.path().join("HALT");
    std::fs::write(&somewhere, b"").expect("a sentinel the client was never told about");
    let (events, mut rx) = event_channel(64);
    let (mut client, commands) = halt_client(events, None);
    client.submit(&limit_request("opening", 1, 50000.0, 0.01));
    assert_eq!(submitted(queued(&commands)), "opening");
    assert!(drain_ready(|| rx.try_recv().ok()).is_empty());
}

/// **An order too old to sign is refused by name and a younger one is untouched.**
/// `crates/bridges/hyperliquid/src/execution/actor.rs`'s `run` asks this before the universe read and the
/// signature; the old loop signed whatever it dequeued.
#[test]
fn an_order_past_its_wait_is_refused_before_it_can_be_signed() {
    use super::actor::refused_too_old;
    use std::time::{Duration, Instant};
    use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
    let (events, mut rx) = event_channel(64);
    let (a, b) = (limit_request("a", 1, 50000.0, 0.01), limit_request("b", 1, 50000.0, 0.01));
    let young = Instant::now() - (MAX_SUBMIT_WAIT - Duration::from_secs(1));
    assert_eq!(refused_too_old(&events, vec![a.clone()], young).len(), 1);
    assert!(drain_ready(|| rx.try_recv().ok()).is_empty(), "a younger order says nothing here");

    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    assert!(refused_too_old(&events, vec![a, b], old).is_empty());
    let said = drain_ready(|| rx.try_recv().ok());
    assert_eq!(
        said.iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Rejected", "Submitted", "Rejected"],
        "{said:?}"
    );
    assert_matches!(&said[1], Event::OrderRejected(r) if r.reason.contains("NOT sent"));
}

/// **The age is asked again at the signature**: a re-read universe, the margin guard and an emulated market's mid sit
/// in front of it, and that time counts. The old order is refused by name and left out; the close beside it is signed.
#[test]
fn an_order_past_its_wait_at_the_signature_is_left_out_and_a_close_is_not() {
    use std::time::{Duration, Instant};
    use vike_bridge_core::exec_actor::{MAX_SUBMIT_WAIT, serving_submit};
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange { mid: Some(50000.0), ..Default::default() }, tx);
    let leg = OrderRequest {
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        price: None,
        ..limit_request("leg", -1, 50000.0, 0.01)
    };
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    serving_submit(old, || st.submit(&symbology(), &[limit_request("a", 1, 50000.0, 0.01), leg]));
    {
        let actions = st.exchange.actions.lock().unwrap();
        match actions.as_slice() {
            [Action::Order(a)] => assert_eq!(a.orders.len(), 1, "the close alone: {a:?}"),
            other => panic!("expected one order action, got {other:?}"),
        }
    }
    let said = drain_ready(|| rx.try_recv().ok());
    assert!(
        said.iter().any(|e| matches!(e, Event::OrderRejected(r)
            if r.order_client_id == "a" && r.reason.contains("NOT sent"))),
        "{said:?}"
    );
}

/// **A close past its wait still goes, alone or inside a batch**, and an opening order beside it is refused: a flatten
/// leg refused for its age would leave the position open with nothing asking again.
#[test]
fn a_close_past_its_wait_is_signed_and_an_order_beside_it_is_not() {
    use super::actor::refused_too_old;
    use std::time::{Duration, Instant};
    use vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT;
    let (events, mut rx) = event_channel(64);
    let leg = OrderRequest {
        order_type: vike_model::OrderType::Market,
        reduce_only: true,
        price: None,
        ..limit_request("leg", -1, 50000.0, 0.01)
    };
    let old = Instant::now() - MAX_SUBMIT_WAIT - Duration::from_secs(1);
    let still = refused_too_old(&events, vec![limit_request("a", 1, 50000.0, 0.01), leg], old);
    assert_eq!(still.iter().map(|r| r.order_client_id.as_str()).collect::<Vec<_>>(), ["leg"]);
    let said = drain_ready(|| rx.try_recv().ok());
    assert_matches!(said.as_slice(), [Event::OrderSubmitted(s), Event::OrderRejected(r)]
        if s.order_client_id == "a" && r.order_client_id == "a");
}

// --- restoring a previous session's order (decision 0121): the wire id the core asks for, the
// --- registry and asset map `adopt_restored` rebuilds, and a cancel after a restart.

/// `wire_id_for` is the cloid the wire carries for the order client id — the very string `submit` registers and
/// the recon client reads back off `frontendOpenOrders`/`userFills` — and it needs no state a
/// restart loses (a fresh client with an empty registry answers it).
#[test]
fn wire_id_for_is_the_cloid_a_submit_puts_on_the_wire() {
    let (events, _rx) = event_channel(64);
    let (client, _commands) = halt_client(events, None);
    let wire = client.wire_id_for("vike-1").expect("hyperliquid names its orders by a wire id");
    assert_eq!(wire, cloid_from_order_client_id("vike-1"));
    assert_eq!(
        wire,
        OrderClientIdVenueRegistry::new().register("vike-1"),
        "what a submit registers"
    );
    assert!(wire.starts_with("0x") && wire.len() == 34, "a 128-bit 0x cloid: {wire}");
    // The row of the restore table that leans on this override: it may skip the echo rule only
    // because this client names the wire id, so the exception and the override travel together.
    assert!(
        vike_venues::venue_restore::WIRE_ID_RESTORE_VENUES.contains(&VENUE),
        "hyperliquid answers wire_id_for, so the restore table must name it as the exception"
    );
}

/// **An order placed before a restart, resting now.** A restart empties the map, so the first pass's
/// row keeps the hash, which is exactly what the core's translation asks `wire_id_for` about (decision
/// 0121): were it `None` or anything else, the restore would find nothing. The adoption teaches the
/// client, and from the next pass the row reads by the order client id, so the order is not an `UnknownOrder`
/// beside an `OrphanLocalOrder` for the rest of its life. It stays RESTORED for the first-snapshot floor.
#[test]
fn after_a_restart_a_row_keeps_the_hash_until_the_adoption_names_it() {
    use crate::reconcile::{ReconcileClientHyperliquid, parse_orders};
    let (events, _rx) = event_channel(64);
    let registry = OrderClientIdVenueRegistry::new();
    let (mut client, _commands) = client_over(events, None, registry.clone());
    let recon = ReconcileClientHyperliquid::new(
        crate::test_support::seed_transport(vike_model::VenueNetwork::Testnet),
        "0xmaster",
        vike_model::AssetClass::CryptoPerp,
    )
    .with_cloids(registry.clone());
    let wire = client.wire_id_for("old-1").expect("hyperliquid names a wire id");
    let body = format!(
        r#"[{{"coin":"BTC","limitPx":"38123.0","oid":42177013,"orderType":"Limit","origSz":"0.0003",
             "reduceOnly":false,"side":"B","sz":"0.0003","timestamp":1794013444000,"tif":"Gtc","cloid":"{wire}"}}]"#
    );
    let pass = || {
        let mut rows = parse_orders(&body).expect("a frontendOpenOrders body");
        recon.name_order(&mut rows[0]);
        rows
    };

    let first = pass();
    assert_eq!(
        first[0].order_client_id.as_deref(),
        Some(wire.as_str()),
        "an empty map names nothing"
    );

    client.adopt_restored(&[vike_execution::RestoredOrder::new("old-1", "BTC")]);
    let second = pass();
    assert_eq!(second[0].order_client_id.as_deref(), Some("old-1"));
    assert!(registry.is_restored(&wire), "named back, but no proof of authorship");

    let mut engine = vike_execution::testing::EngineBuilder {
        venue: VENUE.to_string(),
        symbol: "BTC".to_string(),
        ..Default::default()
    }
    .build();
    assert_eq!(engine.reregister_orders(&second), 1, "what the pass's adoption wrote");
    let local = engine.local_view();
    let held = vike_execution::recon::diff(&second, &[], &[], &local.as_view(), None);
    assert!(held.is_empty(), "the second pass raises nothing: {held:?}");
}

/// `adopt_restored` seeds the SHARED registry synchronously (so a WS frame or fill resolves the
/// moment the call returns) and queues an `Adopt` for the exec thread — one per order, in order; the
/// loop reads the venue order id as HL's numeric oid, and one that is not a number teaches none.
#[test]
fn adopt_restored_seeds_the_registry_and_queues_an_adopt_per_order() {
    let (events, _rx) = event_channel(64);
    let registry = OrderClientIdVenueRegistry::new();
    let (mut client, commands) = client_over(events, None, registry.clone());
    let numbered = vike_execution::RestoredOrder {
        venue_order_id: Some("42177013".into()),
        ..vike_execution::RestoredOrder::new("old-1", "BTC")
    };
    let worded = vike_execution::RestoredOrder {
        venue_order_id: Some("not-a-number".into()),
        ..vike_execution::RestoredOrder::new("old-2", "HYPE/USDC")
    };
    client.adopt_restored(&[numbered.clone(), worded.clone()]);
    assert_eq!(registry.resolve(&cloid_from_order_client_id("old-1")).as_deref(), Some("old-1"));
    assert_eq!(registry.resolve(&cloid_from_order_client_id("old-2")).as_deref(), Some("old-2"));
    client.detach();
    let adopted: Vec<_> = commands
        .try_iter()
        .map(|command| match command {
            ExecutionClientActorCommand::Adopt(order) => order,
            _ => panic!("only Adopt commands"),
        })
        .collect();
    assert_eq!(adopted, [numbered, worded], "exactly two Adopt commands, in order");
    let order_ids: Vec<_> = adopted.iter().map(super::actor::taught_oid).collect();
    assert_eq!(order_ids, [Some(42_177_013), None]);
}

/// **A modify after a restart.** An adopted order used to have no `oid`, so a native modify of it was
/// refused ("no resting venue order to modify"); with the oid the core taught it is the venue's
/// cancel-replace of exactly that order, as in the session that placed it.
#[test]
fn a_modify_after_an_adopt_with_the_taught_oid_reaches_the_wire() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Ok(
            json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":22}}]}}}),
        )])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.adopt(&symbology(), "old-btc", "BTC", Some(11));
    st.modify(&symbology(), &limit_request("old-btc", 1, 50000.0, 0.01), None, Some(50500.0));
    match st.exchange.actions.lock().unwrap().as_slice() {
        [Action::Modify(m)] => assert_eq!(m.oid, 11, "keyed by the oid the core taught"),
        other => panic!("expected one Modify action, got {other:?}"),
    }
    assert_eq!(st.orders.get("old-btc").unwrap().oid, Some(22), "rolled to the replacement's");
    let _ = drain_ready(|| rx.try_recv().ok());
}

/// **The cancel the restore exists for.** After a restart the exec thread's asset map is empty, so a
/// cancel by order client id used to die as "no local order mapping to cancel". Once `Adopt` has run it reaches
/// the wire as `cancelByCloid` with the cloid recomputed from the order client id and the asset resolved from
/// the UNIFIED symbol (a perp and a spot pair, each).
#[test]
fn a_cancel_after_adopt_reaches_the_wire_with_the_right_asset() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let symbol = symbology();
    st.adopt(&symbol, "old-eth", "ETH", None);
    st.adopt(&symbol, "old-hype", "HYPE/USDC", None);

    st.cancel(&["old-eth".to_string(), "old-hype".to_string()]);

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(
                got,
                vec![
                    (1, cloid_from_order_client_id("old-eth")),
                    (10_107, cloid_from_order_client_id("old-hype")),
                ],
                "perp ETH is asset 1, spot @107 is 10000 + 107"
            );
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
    drop(actions);
    let _ = drain_ready(|| rx.try_recv().ok());
}

/// Without an `Adopt` the cancel is rejected exactly as before (never swallowed, nothing placed);
/// an adopt for a symbol the symbology does not know inserts nothing, so it does not turn a
/// reject into a guess at an asset id.
#[test]
fn a_cancel_without_a_usable_adopt_still_rejects() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.adopt(&symbology(), "old-1", "NOT/APAIR", None);
    st.cancel(std::slice::from_ref(&"old-1".to_string()));
    assert!(st.exchange.actions.lock().unwrap().is_empty(), "no asset, so nothing to place");
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderCancelRejected(e)] => assert_eq!(e.order_client_id, "old-1"),
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

// --- cancelling an order this process never placed, by the cloid the venue printed for it (the node's
// --- `recon cancel` of a held `UnknownOrder` row: its id is the hash of an order client id nothing stores).

/// A cloid as a `recon ls` row prints it (`0x` and 32 hex digits), and a second order's.
const HEX: &str = "0xb395aaaab395aaaab395aaaab395aaaa";
const OTHER_HEX: &str = "0x00000000000000000000000000000001";

/// One `frontendOpenOrders` row: a resting buy on venue coin `coin` carrying `cloid`.
fn listed(cloid: &str, coin: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: coin.into(),
        venue_order_id: "168136755771512".into(),
        order_client_id: Some(cloid.to_string()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 0.0002,
        filled_qty: 0.0,
        average_price: 0.0,
        status: OrderStatus::Accepted,
        timestamp: 1_000,
        price: None,
        trigger_price: None,
        trail: None,
        trail_percent: None,
    }
}

fn cancel_ok() -> Mutex<VecDeque<Result<Value, VenueApiError>>> {
    Mutex::new(VecDeque::from([Ok(
        json!({"status":"ok","response":{"data":{"statuses":["success"]}}}),
    )]))
}

/// **The cancel of an order this process never placed.** The id is sent VERBATIM on the asset of the
/// coin the account's open-order list shows for it (hashing it again would name another order, and
/// the exec thread has no entry to take the asset from), the other listed order is left alone, and
/// it ends in exactly one `OrderCanceled` naming the hex, with nothing added to the asset map.
#[test]
fn a_venue_cloid_is_cancelled_verbatim_on_the_listed_asset() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        open_orders: vec![listed(OTHER_HEX, "ETH"), listed(HEX, "BTC")],
        responses: cancel_ok(),
        ..Default::default()
    };
    let mut st = state(fake, tx);

    st.cancel_any(&symbology(), HEX);

    match &st.exchange.actions.lock().unwrap()[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, &str)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.as_str())).collect();
            assert_eq!(got, vec![(0, HEX)], "BTC is asset 0; only the named order");
            assert_ne!(
                a.cancels[0].cloid,
                cloid_from_order_client_id(HEX),
                "the printed cloid is not hashed a second time"
            );
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderCanceled(c)] => assert_eq!(c.order_client_id, HEX),
        other => panic!("expected exactly one OrderCanceled, got {other:?}"),
    }
    assert!(st.orders.is_empty(), "a venue cloid is not an order this thread tracks");
}

/// A spot coin (`@107`) takes its asset from the symbology like a perp does: 10000 + 107.
#[test]
fn a_venue_cloid_on_a_spot_coin_is_cancelled_on_the_spot_asset() {
    let (tx, _rx) = event_channel(64);
    let fake = FakeExchange {
        open_orders: vec![listed(HEX, "@107")],
        responses: cancel_ok(),
        ..Default::default()
    };
    let mut st = state(fake, tx);

    st.cancel_any(&symbology(), HEX);

    match &st.exchange.actions.lock().unwrap()[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, &str)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.as_str())).collect();
            assert_eq!(got, vec![(10_107, HEX)]);
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
}

/// Every way the order cannot be shown to exist is ONE `OrderCancelRejected` naming the hex and NOTHING
/// on the wire: the order is not listed (gone, filled, or another account's), the list cannot be read,
/// the listed coin is one the symbology lacks, and only a DIFFERENT cloid is listed (never the
/// neighbour's order).
#[test]
fn a_venue_cloid_that_cannot_be_shown_to_rest_is_rejected_and_sends_nothing() {
    let cases = [
        ("not listed", FakeExchange::default(), "no resting order"),
        (
            "the list cannot be read",
            FakeExchange {
                open_orders: vec![listed(HEX, "BTC")],
                open_orders_fails: true,
                ..Default::default()
            },
            "could not be read",
        ),
        (
            "an unknown coin",
            FakeExchange { open_orders: vec![listed(HEX, "kNOPE")], ..Default::default() },
            "does not know",
        ),
        (
            "only a different cloid is listed",
            FakeExchange { open_orders: vec![listed(OTHER_HEX, "BTC")], ..Default::default() },
            "no resting order",
        ),
    ];
    for (name, fake, words) in cases {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(fake, tx);

        st.cancel_any(&symbology(), HEX);

        assert!(st.exchange.actions.lock().unwrap().is_empty(), "{name}: nothing placed");
        match drain_ready(|| rx.try_recv().ok()).as_slice() {
            [Event::OrderCancelRejected(e)] => {
                assert_eq!(e.order_client_id, HEX, "{name}");
                assert!(e.reason.contains(words), "{name}: {}", e.reason);
            }
            other => panic!("{name}: expected one OrderCancelRejected, got {other:?}"),
        }
    }
}

/// The venue is not known to keep a cloid unique across coins: two listed rows carrying the SAME
/// cloid on different coins name no single order, so nothing is sent (never "the first one") and the
/// operator hears it once, naming the id and saying it is listed more than once.
#[test]
fn a_venue_cloid_listed_twice_is_rejected_and_sends_nothing() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        open_orders: vec![listed(HEX, "BTC"), listed(HEX, "ETH")],
        responses: cancel_ok(),
        ..Default::default()
    };
    let mut st = state(fake, tx);

    st.cancel_any(&symbology(), HEX);

    assert!(st.exchange.actions.lock().unwrap().is_empty(), "neither coin's order was cancelled");
    match drain_ready(|| rx.try_recv().ok()).as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.order_client_id, HEX);
            assert!(e.reason.contains("listed more than once"), "{}", e.reason);
        }
        other => panic!("expected exactly one OrderCancelRejected, got {other:?}"),
    }
}

/// The new door changes nothing for an id that is ours: an order client id this thread placed cancels exactly as
/// `cancel` always did (the cloid is the hash of the order client id), and an unknown one still says so.
#[test]
fn cancel_any_of_our_own_order_client_id_is_todays_cancel() {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        // a listing that would let the new path cancel something, to show it is not consulted
        open_orders: vec![listed(HEX, "BTC")],
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            Ok(json!({"status":"ok","response":{"data":{"statuses":["success"]}}})),
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));

    st.cancel_any(&symbology(), "c1");
    st.cancel_any(&symbology(), "ghost");

    match &st.exchange.actions.lock().unwrap()[1..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(got, vec![(0, cloid_from_order_client_id("c1"))]);
        }
        other => panic!("expected the one cancel of c1 after the order, got {other:?}"),
    }
    let evs = drain_ready(|| rx.try_recv().ok());
    assert!(evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.order_client_id == "c1")));
    let ghost: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderCancelRejected(r) if r.order_client_id == "ghost" => {
                Some(r.reason.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(ghost, vec!["no local order mapping to cancel"]);
}

/// A hex id this thread DOES hold (an adopted restore) is not taken for a foreign one: it keeps the
/// path it had, which hashes the held id as it always did.
#[test]
fn a_hex_id_the_thread_holds_takes_the_old_cancel_path() {
    let (tx, _rx) = event_channel(64);
    let fake = FakeExchange {
        open_orders: vec![listed(HEX, "ETH")],
        responses: cancel_ok(),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.adopt(&symbology(), HEX, "BTC", None);

    st.cancel_any(&symbology(), HEX);

    match &st.exchange.actions.lock().unwrap()[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(got, vec![(0, cloid_from_order_client_id(HEX))], "the adopted BTC asset");
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
}

/// The shape of a venue cloid: `0x` and exactly 32 hex digits, nothing a minted order client id can be.
#[test]
fn only_0x_and_thirty_two_hex_digits_is_a_venue_cloid() {
    assert!(order_wire::is_wire_cloid(HEX));
    assert!(order_wire::is_wire_cloid(&cloid_from_order_client_id("any")));
    assert!(order_wire::is_wire_cloid("0xABCDEFabcdef0123456789ABCDEFabcd"));
    assert!(!order_wire::is_wire_cloid(&HEX[..HEX.len() - 1]), "31 digits");
    assert!(!order_wire::is_wire_cloid(&format!("{HEX}0")), "33 digits");
    assert!(!order_wire::is_wire_cloid(&HEX[2..]), "no 0x prefix");
    assert!(!order_wire::is_wire_cloid("0xg395aaaab395aaaab395aaaab395aaaa"), "not hex");
    assert!(
        !order_wire::is_wire_cloid("0Xb395aaaab395aaaab395aaaab395aaaa"),
        "prefix is lower-case"
    );
    assert!(!order_wire::is_wire_cloid(""));
    assert!(!order_wire::is_wire_cloid("c1"));
}

/// An adopt never clobbers what a live order already knows: re-announcing an order client id this session placed
/// (its `oid` captured from the response) keeps the `oid` a later modify needs.
#[test]
fn an_adopt_keeps_the_oid_of_an_order_this_session_placed() {
    let (tx, _rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([Ok(
            json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}}),
        )])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    st.submit(&symbology(), std::slice::from_ref(&limit_request("c1", 1, 50000.0, 0.01)));
    // A taught oid never overwrites the one this session's own accept named.
    st.adopt(&symbology(), "c1", "BTC", Some(99));
    assert_eq!(st.orders["c1"].oid, Some(11));
}

/// After a modify whose outcome is UNKNOWN (post-send timeout, or an `Applied` reply naming no oid)
/// the held oid may be dead: it is RETIRED (the stale `canceled` for it is suppressed, one-shot) and
/// the next modify is refused with an explicit reason instead of aiming a replace at it.
#[test]
fn an_unknown_modify_retires_the_old_oid_and_refuses_the_next_modify() {
    let timeout = Err(VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: "timed out".into(),
    });
    let no_oid = Ok(json!({"status":"ok","response":{"data":{"statuses":[{}]}}}));
    for (what, reply) in [("timeout", timeout), ("no new oid", no_oid)] {
        let (tx, mut rx) = event_channel(64);
        let fake = FakeExchange {
            responses: Mutex::new(VecDeque::from([
                Ok(
                    json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}}),
                ),
                reply,
            ])),
            ..Default::default()
        };
        let mut st = state(fake, tx);
        let req = limit_request("c1", 1, 50000.0, 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));
        let _ = drain_ready(|| rx.try_recv().ok());
        st.modify(&symbology(), &req, None, Some(50500.0));
        let _ = drain_ready(|| rx.try_recv().ok());
        assert!(st.registry.take_retired(11), "{what}: the possibly-dead oid is retired");
        let actions_before = st.exchange.actions.lock().unwrap().len();

        st.modify(&symbology(), &req, None, Some(50600.0));
        match drain_ready(|| rx.try_recv().ok()).as_slice() {
            [Event::OrderModifyRejected(e)] => {
                assert!(
                    e.reason.contains("outcome of the previous modify unknown"),
                    "{what}: {}",
                    e.reason
                )
            }
            other => panic!("{what}: expected one OrderModifyRejected, got {other:?}"),
        }
        assert_eq!(
            st.exchange.actions.lock().unwrap().len(),
            actions_before,
            "{what}: nothing was sent at a possibly-dead oid"
        );
    }
}

/// Submit `c1` (oid 11), then time its modify out: the new oid is unknown to exec.
fn after_ambiguous_modify(
    tx: EventSender,
    fake_tail: impl FnOnce(&mut FakeExchange),
) -> (ExecutionState<FakeExchange>, OrderRequest) {
    let mut fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            Err(VenueApiError {
                code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
                msg: "timed out".into(),
            }),
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":99}}]}}})),
        ])),
        ..Default::default()
    };
    fake_tail(&mut fake);
    let mut st = state(fake, tx);
    let req = limit_request("c1", 1, 50000.0, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    st.modify(&symbology(), &req, None, Some(50500.0));
    (st, req)
}

/// Feed one `orderUpdates` row for `c1`'s cloid through the REAL pump mapper.
fn websocket_row(
    st: &ExecutionState<FakeExchange>,
    order_client_id: &str,
    oid: u64,
    status: &str,
) -> Vec<Event> {
    let cloid = cloid_from_order_client_id(order_client_id);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50500","sz":"0.01","oid":oid,"cloid":cloid,"timestamp":1},
        "status":status,"statusTimestamp":2
    }]});
    crate::execution::user_data::map_frame_to_events(
        &frame,
        &symbology(),
        &st.registry,
        &AtomicBool::new(true),
        0,
    )
}

/// The reply timed out, then the WS streams the replacement `open` (same cloid, NEW oid 22): the next
/// modify is no longer refused and targets 22; the old oid's stale `canceled` is still suppressed
/// (once); an update for another order, or a non-open row, teaches nothing.
#[test]
fn a_ws_open_with_a_new_oid_after_an_ambiguous_modify_is_learned() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |_| {});
    let _ = drain_ready(|| rx.try_recv().ok());
    // unrelated: a foreign cloid, and a non-open row of ours, are not an oid to adopt
    let other = st.registry.register("other");
    let foreign = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"1","sz":"1","oid":777,"cloid":other,"timestamp":1},
        "status":"open","statusTimestamp":2}]});
    let _ = crate::execution::user_data::map_frame_to_events(
        &foreign,
        &symbology(),
        &st.registry,
        &AtomicBool::new(true),
        0,
    );
    assert_eq!(websocket_row(&st, "c1", 33, "triggered").len(), 1);

    // the stale cancel of the retired old oid: dropped, one-shot
    assert!(websocket_row(&st, "c1", 11, "canceled").is_empty(), "stale canceled suppressed");
    // the replacement
    assert_eq!(websocket_row(&st, "c1", 22, "open").len(), 1);

    st.modify(&symbology(), &req, None, Some(50600.0));
    let actions = st.exchange.actions.lock().unwrap();
    match actions.last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22, "next modify targets the learned oid"),
        other => panic!("expected a Modify, got {other:?}"),
    }
    drop(actions);
    assert_eq!(st.orders["c1"].oid, Some(99), "and then rolls to the reply's oid");
    assert!(matches!(drain_ready(|| rx.try_recv().ok()).as_slice(), [Event::OrderModified(_)]));
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 0, "the WS answered; no requery");
}

/// Nothing streamed: ONE `orderStatus` requery is spent at the next modify. `open` under a new oid
/// is adopted; a second attempt does not ask again.
#[test]
fn an_order_status_requery_learns_the_oid_once() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_oid = Some(22));
    let _ = drain_ready(|| rx.try_recv().ok());
    st.modify(&symbology(), &req, None, Some(50600.0));
    match st.exchange.actions.lock().unwrap().last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22),
        other => panic!("expected a Modify, got {other:?}"),
    }
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 1);
    let _ = drain_ready(|| rx.try_recv().ok());

    // a definitive `unknownOid`: still refused inside the window, and not asked a second time
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_unknown = true);
    let _ = drain_ready(|| rx.try_recv().ok());
    for _ in 0..2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        assert!(matches!(
            drain_ready(|| rx.try_recv().ok()).as_slice(),
            [Event::OrderModifyRejected(_)]
        ));
    }
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 1, "one-shot");
}

/// The venue says the order is open under the HELD oid, but the unknown mark is younger than
/// `RETIRED_TTL`: the first modify may still be in flight (or `/info` lags it), so NOTHING is lifted --
/// the retirement, the mark and the oid stay, the next modify is still refused with the clear text,
/// and the requery is not spent (the next modify asks again).
#[test]
fn a_requery_that_finds_the_same_oid_inside_the_window_lifts_nothing() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_oid = Some(11));
    let _ = drain_ready(|| rx.try_recv().ok());
    for asked in 1..=2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        match drain_ready(|| rx.try_recv().ok()).as_slice() {
            [Event::OrderModifyRejected(e)] => {
                assert!(e.reason.contains("outcome of the previous modify unknown"), "{}", e.reason)
            }
            other => panic!("expected one OrderModifyRejected, got {other:?}"),
        }
        assert_eq!(*st.exchange.status_calls.lock().unwrap(), asked, "not spent: asked again");
    }
    assert!(st.orders["c1"].unknown_since.is_some(), "the unknown mark stays");
    assert_eq!(st.orders["c1"].oid, Some(11));
    assert_eq!(st.exchange.actions.lock().unwrap().len(), 2, "no modify was sent at the old oid");
    assert!(st.registry.take_retired(11), "the retirement stays");
}

/// ...and once `RETIRED_TTL` has passed since the mark, open-under-the-held-oid is believed: the
/// modify proceeds on it and the retirement is withdrawn, so a real cancel of that oid is not swallowed.
#[test]
fn a_requery_that_finds_the_same_oid_after_the_window_unretires_it() {
    let (tx, _rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| {
        f.status_oid = Some(11);
        // the replace re-reports the same oid, so its own reply does not retire it again
        *f.responses.lock().unwrap().back_mut().unwrap() =
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}}));
    });
    st.orders.get_mut("c1").unwrap().unknown_since = std::time::Instant::now()
        .checked_sub(super::registry::RETIRED_TTL + std::time::Duration::from_secs(1));
    st.modify(&symbology(), &req, None, Some(50600.0));
    match st.exchange.actions.lock().unwrap().last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 11),
        other => panic!("expected a Modify, got {other:?}"),
    }
    assert!(!st.registry.take_retired(11));
}

/// A `canceled`/`filled`/`replaced` status (or any reply that is not an open order) can lag or flip
/// under a replace: it settles nothing and must not spend the one-shot. Only the venue's definitive
/// `unknownOid` does (`an_order_status_requery_learns_the_oid_once`).
#[test]
fn a_not_open_order_status_does_not_spend_the_requery() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |_| {});
    let _ = drain_ready(|| rx.try_recv().ok());
    for asked in 1..=2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        assert!(matches!(
            drain_ready(|| rx.try_recv().ok()).as_slice(),
            [Event::OrderModifyRejected(_)]
        ));
        assert_eq!(*st.exchange.status_calls.lock().unwrap(), asked, "asked again each time");
    }
    st.exchange.status_oid = Some(22);
    st.modify(&symbology(), &req, None, Some(50600.0));
    match st.exchange.actions.lock().unwrap().last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22, "the later answer is still learned"),
        other => panic!("expected a Modify, got {other:?}"),
    }
}

/// A WS `open` row under the HELD oid proves nothing (it may predate the modify, or the modify may
/// not be processed yet): it must not lift the old oid's retirement nor clear the unknown mark, so the
/// real stale `canceled` of the old oid is still dropped and the next modify is still refused.
#[test]
fn a_ws_open_under_the_held_oid_lifts_nothing() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |_| {});
    let _ = drain_ready(|| rx.try_recv().ok());
    let _ = websocket_row(&st, "c1", 11, "open");
    st.modify(&symbology(), &req, None, Some(50600.0));
    assert!(
        matches!(drain_ready(|| rx.try_recv().ok()).as_slice(), [Event::OrderModifyRejected(_)]),
        "still refused"
    );
    assert!(st.orders["c1"].unknown_since.is_some(), "the unknown mark stays");
    assert_eq!(st.orders["c1"].oid, Some(11));
    assert!(
        websocket_row(&st, "c1", 11, "canceled").is_empty(),
        "the stale canceled is still suppressed"
    );
}

/// A failed `orderStatus` request says nothing about the order and must not spend the one-shot: the
/// next modify asks again and adopts what the venue then answers.
#[test]
fn a_failed_order_status_read_does_not_spend_the_requery() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_fails = true);
    let _ = drain_ready(|| rx.try_recv().ok());
    st.modify(&symbology(), &req, None, Some(50600.0));
    assert!(matches!(
        drain_ready(|| rx.try_recv().ok()).as_slice(),
        [Event::OrderModifyRejected(_)]
    ));
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 1);
    st.exchange.status_fails = false;
    st.exchange.status_oid = Some(22);
    st.modify(&symbology(), &req, None, Some(50600.0));
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 2, "asked again");
    match st.exchange.actions.lock().unwrap().last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22),
        other => panic!("expected a Modify, got {other:?}"),
    }
}

#[test]
fn order_status_reply_yields_only_an_open_oid() {
    use order_wire::OrderStatusAnswer::{NotOpen, Open, UnknownOid};
    use order_wire::parse_order_status as p;
    let open = json!({"status":"order","order":{"order":{"oid":22,"cloid":"0x1"},"status":"open"}});
    assert_eq!(p(&open), Open(22));
    let filled = json!({"status":"order","order":{"order":{"oid":22},"status":"filled"}});
    assert_eq!(p(&filled), NotOpen);
    assert_eq!(p(&json!({"status":"unknownOid"})), UnknownOid);
    assert_eq!(p(&json!({"status":"err"})), NotOpen);
}

// The margin guard and the universe reload share this fake venue and live in their own file.
#[path = "exec_margin_tests.rs"]
mod margin_guard;
