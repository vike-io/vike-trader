//! Pure command→action→event wiring — a fake [`HlExchange`] (NO network, NO real orders) proves:
//! `submit_batch` is ONE native `order` action of N wires; a market order is emulated as an `Ioc`
//! at ±5% off the fake mid; `cancel`/`modify` build the native `cancelByCloid`/`modify` actions
//! (modify keyed by the resting oid); and every failure path (build error, definite transport
//! error) synthesizes the terminal reject while the ambiguous timeout does NOT.
use super::*;
use serde_json::json;
use std::assert_matches;
use std::collections::VecDeque;
use vike_exec::Ingest;
use vike_exec::event_channel;
use vike_model::{OrderStatus, OrderStatusReport};

/// Pins the routed hyperliquid row of `vike_venues::venue_tif::venue_tif` byte-for-byte.
/// NOTE the Fok→Ioc fold: the OPPOSITE direction of polymarket's Ioc→FOK (step-2 flip).
#[test]
fn hl_tif_row_is_pinned() {
    assert_eq!(hl_tif(TimeInForce::Gtc), "Gtc");
    assert_eq!(hl_tif(TimeInForce::Ioc), "Ioc");
    assert_eq!(hl_tif(TimeInForce::Fok), "Ioc");
    assert_eq!(hl_tif(TimeInForce::Gtd), "Gtc");
    assert_eq!(hl_tif(TimeInForce::Day), "Gtc");
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
}
impl HlExchange for FakeExchange {
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
}

/// An `ExecState` at the venue's historical band — so every pre-existing test below keeps
/// asserting the exact prices it always did.
fn state(exchange: FakeExchange, events: EventSender) -> ExecState<FakeExchange> {
    state_with_slippage(exchange, events, DEFAULT_MARKET_SLIPPAGE)
}

fn state_with_slippage(
    exchange: FakeExchange,
    events: EventSender,
    slippage: f64,
) -> ExecState<FakeExchange> {
    ExecState {
        exchange,
        registry: CloidRegistry::new(),
        events,
        orders: HashMap::new(),
        builder: None,
        slippage,
    }
}

fn limit_req(coid: &str, side: i32, price: f64, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
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
fn stop_req(coid: &str, side: i32, trigger: f64, limit: Option<f64>, qty: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
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

macro_rules! drain {
    ($rx:expr) => {{
        let mut out: Vec<Event> = Vec::new();
        while let Ok(ing) = $rx.try_recv() {
            if let Ingest::Event(e) = ing {
                out.push(e);
            }
        }
        out
    }};
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
    let reqs = vec![limit_req("c1", 1, 50000.0, 0.01), limit_req("c2", -1, 51000.0, 0.02)];
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

    let evs = drain!(rx);
    assert_eq!(
        evs.iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted", "Submitted", "Accepted", "Accepted"]
    );
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11), "resting oid captured for modify");
}

/// **Any core perp is placeable (decision 0174), proven on the wire builder**: an order on a coin
/// other than the first is signed on THAT coin's asset index with its own `szDecimals` size and its
/// own price clamp (five significant figures, then `6 - szDecimals` decimals), its cancel finds the
/// same asset by coid, and a coin the loaded symbology lacks is a terminal local reject with nothing
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
    let mut eth = limit_req("c-eth", 1, 3000.123456, 0.123456);
    eth.symbol = "ETH".into();
    let mut nope = limit_req("c-nope", 1, 1.0, 1.0);
    nope.symbol = "kNOPE".into();
    st.submit(&symbology(), &[limit_req("c-btc", 1, 50000.0, 0.01), eth, nope]);

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
    let evs = drain!(rx);
    let nope_reject = evs.iter().find_map(|e| match e {
        Event::OrderRejected(r) if r.client_order_id == "c-nope" => Some(r.reason.clone()),
        _ => None,
    });
    assert!(
        nope_reject.is_some_and(|r| r.contains("unknown symbol kNOPE")),
        "a coin the symbology lacks is a terminal reject: {evs:?}"
    );
}

/// The exec-level wiring twin of `signing::action::tests::
/// order_wire_carries_builder_when_configured_and_omits_when_not` — proves `ExecState::submit`
/// actually carries `self.builder` onto the sent `Action`, and that the default `None` (no
/// `.env` config) omits it entirely, exactly like before this field existed.
#[test]
fn submit_carries_the_configured_builder_and_omits_it_when_unset() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.builder = Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 });
    st.submit(&symbology(), &[limit_req("c1", 1, 50000.0, 0.01)]);
    let actions = st.exchange.actions.lock().unwrap();
    match &actions[0] {
        Action::Order(a) => assert_eq!(
            a.builder,
            Some(HlBuilderFee { address: "0x0c8d".to_string(), fee_tenths_bp: 0 })
        ),
        other => panic!("expected an Order action, got {other:?}"),
    }
    drop(actions);

    let (tx2, _rx2) = event_channel(64);
    let mut st2 = state(FakeExchange::default(), tx2); // builder: None (the default state)
    st2.submit(&symbology(), &[limit_req("c2", 1, 50000.0, 0.01)]);
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
        client_order_id: "m1".into(),
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
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Filled"]);
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
            client_order_id: "tight".into(),
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
    let req = stop_req("s-tight", -1, 48000.0, None, 0.01);
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
        client_order_id: "m2".into(),
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
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
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
    let req = stop_req("s1", -1, 48000.0, None, 0.01);
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
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);
}

#[test]
fn stop_limit_order_builds_trigger_kind_with_resting_limit() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    // A SELL stop-LIMIT: trigger 48000, rest at limit 47900 once tripped.
    let req = stop_req("s2", -1, 48000.0, Some(47900.0), 0.01);
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

/// **A REAL tie between the adapter and its `VenueCaps` row** — the deribit
/// `modify_answers_with_an_event_and_the_caps_say_it_is_supported` pattern: drive the actual code path, then assert the
/// declaration, in ONE test. (The crate's `caps_test` module cannot do this: every assertion
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
    let caps = vike_venues::caps_for("hyperliquid");
    assert!(
        caps.supported_order_kinds.contains(&"stop_limit"),
        "the row must declare stop_limit — preflight_order refuses undeclared trigger kinds"
    );
    // ...and the core-edge preflight must therefore ADMIT it.
    let mut pf = stop_req("pf1", -1, 48_000.0, Some(47_900.0), 0.01);
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
    let mut req = stop_req("tp1", -1, 70000.0, None, 0.01);
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
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);
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
    let mut req = stop_req("s-mark", -1, 48000.0, None, 0.01);
    req.trigger_by = Some(vike_model::TriggerBy::Mark);
    st.submit(&symbology(), std::slice::from_ref(&req));
    assert_eq!(st.exchange.actions.lock().unwrap().len(), 1, "Mark IS the venue law");
    assert!(!drain!(rx).iter().map(kind).any(|k| k == "Rejected"));

    // Last / Index: denied locally, terminal, no action placed — on stops AND take-profits.
    for (coid, tb) in
        [("s-last", vike_model::TriggerBy::Last), ("s-index", vike_model::TriggerBy::Index)]
    {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange::default(), tx);
        let mut req = stop_req(coid, -1, 48000.0, None, 0.01);
        req.trigger_by = Some(tb);
        st.submit(&symbology(), std::slice::from_ref(&req));
        assert!(st.exchange.actions.lock().unwrap().is_empty(), "{tb:?}: wire never touched");
        let evs = drain!(rx);
        assert_eq!(evs.iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Rejected"]);
        match &evs[1] {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, coid);
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
    st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
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
    let evs = drain!(rx);
    assert!(evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c1")));
}

#[test]
fn cancel_of_an_unknown_order_is_rejected_not_swallowed() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.cancel(std::slice::from_ref(&"ghost".to_string()));
    assert!(st.exchange.actions.lock().unwrap().is_empty(), "nothing to place for an unknown coid");
    match drain!(rx).as_slice() {
        [Event::OrderCancelRejected(e)] => assert_eq!(e.client_order_id, "ghost"),
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
    let req = limit_req("c1", 1, 50000.0, 0.01);
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
    let evs = drain!(rx);
    assert!(evs.iter().any(|e| matches!(
            e,
            Event::OrderModified(m) if m.client_order_id == "c1" && m.venue_order_id.as_deref() == Some("22")
        )));
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(22), "meta oid rolled to the new one");
}

#[test]
fn modify_without_a_resting_oid_is_rejected() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let req = limit_req("c1", 1, 50000.0, 0.01);
    st.modify(&symbology(), &req, None, Some(51000.0)); // never submitted → no oid
    assert!(st.exchange.actions.lock().unwrap().is_empty());
    match drain!(rx).as_slice() {
        [Event::OrderModifyRejected(e)] => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected one OrderModifyRejected, got {other:?}"),
    }
}

/// Submit `c1` (resting oid 11), then answer the modify with `reply`; return the events AFTER the
/// submit's own, plus the state (to read the rolled `oid`).
fn modify_with_reply(reply: Result<Value, VenueApiError>) -> (Vec<Event>, ExecState<FakeExchange>) {
    let (tx, mut rx) = event_channel(64);
    let fake = FakeExchange {
        responses: Mutex::new(VecDeque::from([
            Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":11}}]}}})),
            reply,
        ])),
        ..Default::default()
    };
    let mut st = state(fake, tx);
    let req = limit_req("c1", 1, 50000.0, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    let _ = drain!(rx); // Submitted + Accepted
    st.modify(&symbology(), &req, None, Some(50500.0));
    (drain!(rx), st)
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
            assert_eq!(e.client_order_id, "c1");
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
            assert_eq!(e.client_order_id, "c1");
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
    let (evs, st) = modify_with_reply(Err(VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: "timed out".into(),
    }));
    match evs.as_slice() {
        [Event::OrderModifyRejected(e)] => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.reason.contains("outcome unknown"), "reason: {}", e.reason);
        }
        other => panic!("expected one OrderModifyRejected advisory, got {other:?}"),
    }
    assert_eq!(st.orders.get("c1").unwrap().oid, Some(11));
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
        &[limit_req("c1", 1, 50000.0, 0.01), limit_req("c2", -1, 51000.0, 0.02)],
    );
    assert_eq!(
        drain!(rx).iter().map(kind).collect::<Vec<_>>(),
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
    st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
    assert_eq!(
        drain!(rx).iter().map(kind).collect::<Vec<_>>(),
        vec!["Submitted"],
        "the ambiguous timeout must NOT synthesize a reject (would strand a phantom position)"
    );
}

/// A cancel that timed out after the send may have landed or not: it is not a terminal, and it must
/// not be only a log line - one NON-terminal `OrderCancelRejected` per coid says the outcome is unknown.
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
    st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
    let _ = drain!(rx);
    st.cancel(std::slice::from_ref(&"c1".to_string()));
    match drain!(rx).as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.reason.contains("cancel outcome unknown"), "reason: {}", e.reason);
        }
        other => panic!("expected one OrderCancelRejected advisory, got {other:?}"),
    }
}

/// A client over a command channel NOBODY reads — the shape that lets the halt arms be driven with
/// no `Signer`, no private-WS pump and no dial. A submit that is admitted lands on `cmds`; one the
/// sentinel refuses never does. `join: None` makes `Drop` a no-op join (the send into the dead
/// channel is already `let _ =`).
fn halt_client(
    events: EventSender,
    halt_path: Option<std::path::PathBuf>,
) -> (HyperliquidExecutionClient, Receiver<HlCommand>) {
    let (tx, cmds) = channel();
    let client = HyperliquidExecutionClient {
        tx,
        events,
        join: None,
        funding_stop: Arc::new(AtomicBool::new(false)),
        funding_join: None,
        registry: CloidRegistry::new(),
        halt_path,
    };
    (client, cmds)
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
    let (mut client, cmds) = halt_client(events, Some(sentinel.clone()));
    std::fs::write(&sentinel, b"").expect("engage the sentinel");

    // 1. OPENING, halted: refused terminally, wording shared with every enforcing client.
    client.submit(&limit_req("opening", 1, 50000.0, 0.01));
    let events = drain!(rx);
    match events.as_slice() {
        [Event::OrderRejected(r)] => {
            assert_eq!(r.client_order_id, "opening");
            assert_eq!(r.reason, vike_exec::halt::HALT_REJECT_REASON);
        }
        other => panic!("an opening submit under the handed sentinel must be refused: {other:?}"),
    }
    assert!(cmds.try_recv().is_err(), "a refused order must not reach the exec thread");

    // 1b. …and a native BATCH is refused as a unit when any leg opens.
    client.submit_batch(&[limit_req("b1", 1, 50000.0, 0.01), limit_req("b2", -1, 51000.0, 0.02)]);
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Rejected", "Rejected"]);
    assert!(cmds.try_recv().is_err());

    // 2. REDUCING, still halted: admitted.
    let exit = OrderRequest { reduce_only: true, ..limit_req("exit", -1, 50000.0, 0.01) };
    client.submit(&exit);
    assert!(drain!(rx).is_empty(), "an admitted submit emits nothing of its own here");
    assert_matches!(cmds.try_recv(), Ok(HlCommand::Submit(_)), "the reducing order goes out");

    // 3. MODIFY, halted: non-terminal advisory, nothing sent.
    client.modify(&limit_req("opening", 1, 50000.0, 0.01), Some(0.02), None);
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["ModifyRejected"]);
    assert!(cmds.try_recv().is_err());

    // 4. Sentinel removed: the SAME opening order is admitted.
    std::fs::remove_file(&sentinel).expect("rm the sentinel");
    client.submit(&limit_req("resumed", 1, 50000.0, 0.01));
    assert!(drain!(rx).is_empty());
    assert_matches!(cmds.try_recv(), Ok(HlCommand::Submit(_)), "resumed trading goes out");
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
    let (mut client, cmds) = halt_client(events, None);
    client.submit(&limit_req("opening", 1, 50000.0, 0.01));
    assert!(drain!(rx).is_empty());
    assert_matches!(cmds.try_recv(), Ok(HlCommand::Submit(_)));
}

// --- restoring a previous session's order (decision 0121): the wire id the core asks for, the
// --- registry and asset map `adopt_restored` rebuilds, and a cancel after a restart.

/// `wire_id_for` is the cloid the wire carries for the coid — the very string `submit` registers and
/// the recon client reads back off `frontendOpenOrders`/`userFills` — and it needs no state a
/// restart loses (a fresh client with an empty registry answers it).
#[test]
fn wire_id_for_is_the_cloid_a_submit_puts_on_the_wire() {
    let (events, _rx) = event_channel(64);
    let (client, _cmds) = halt_client(events, None);
    let wire = client.wire_id_for("vike-1").expect("hyperliquid names its orders by a wire id");
    assert_eq!(wire, cloid_from_client_order_id("vike-1"));
    assert_eq!(wire, CloidRegistry::new().register("vike-1"), "what a submit registers");
    assert!(wire.starts_with("0x") && wire.len() == 34, "a 128-bit 0x cloid: {wire}");
    // The row of the restore table that leans on this override: it may skip the echo rule only
    // because this client names the wire id, so the exception and the override travel together.
    assert!(
        vike_venues::venue_restore::WIRE_ID_RESTORE_VENUES.contains(&VENUE),
        "hyperliquid answers wire_id_for, so the restore table must name it as the exception"
    );
}

/// `adopt_restored` seeds the SHARED registry synchronously (so a WS frame or fill resolves the
/// moment the call returns) and queues an `Adopt` for the exec thread — one per order, in order.
#[test]
fn adopt_restored_seeds_the_registry_and_queues_an_adopt_per_order() {
    let (events, _rx) = event_channel(64);
    let (mut client, cmds) = halt_client(events, None);
    client.adopt_restored(&[
        ("old-1".to_string(), "BTC".to_string()),
        ("old-2".to_string(), "HYPE/USDC".to_string()),
    ]);
    assert_eq!(
        client.registry.resolve(&cloid_from_client_order_id("old-1")).as_deref(),
        Some("old-1")
    );
    assert_eq!(
        client.registry.resolve(&cloid_from_client_order_id("old-2")).as_deref(),
        Some("old-2")
    );
    match (cmds.try_recv(), cmds.try_recv(), cmds.try_recv()) {
        (
            Ok(HlCommand::Adopt { coid: c1, symbol: s1 }),
            Ok(HlCommand::Adopt { coid: c2, symbol: s2 }),
            Err(_),
        ) => assert_eq!(
            [(c1, s1), (c2, s2)],
            [
                ("old-1".to_string(), "BTC".to_string()),
                ("old-2".to_string(), "HYPE/USDC".to_string())
            ]
        ),
        other => panic!("expected exactly two Adopt commands in order, got {other:?}"),
    }
}

/// **The cancel the restore exists for.** After a restart the exec thread's asset map is empty, so a
/// cancel by coid used to die as "no local order mapping to cancel". Once `Adopt` has run it reaches
/// the wire as `cancelByCloid` with the cloid recomputed from the coid and the asset resolved from
/// the UNIFIED symbol (a perp and a spot pair, each).
#[test]
fn a_cancel_after_adopt_reaches_the_wire_with_the_right_asset() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    let sym = symbology();
    st.adopt(&sym, "old-eth", "ETH");
    st.adopt(&sym, "old-hype", "HYPE/USDC");

    st.cancel(&["old-eth".to_string(), "old-hype".to_string()]);

    let actions = st.exchange.actions.lock().unwrap();
    match &actions[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(
                got,
                vec![
                    (1, cloid_from_client_order_id("old-eth")),
                    (10_107, cloid_from_client_order_id("old-hype")),
                ],
                "perp ETH is asset 1, spot @107 is 10000 + 107"
            );
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
    drop(actions);
    let _ = drain!(rx);
}

/// Without an `Adopt` the cancel is rejected exactly as before (never swallowed, nothing placed);
/// an adopt for a symbol the symbology does not know inserts nothing, so it does not turn a
/// reject into a guess at an asset id.
#[test]
fn a_cancel_without_a_usable_adopt_still_rejects() {
    let (tx, mut rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    st.adopt(&symbology(), "old-1", "NOT/APAIR");
    st.cancel(std::slice::from_ref(&"old-1".to_string()));
    assert!(st.exchange.actions.lock().unwrap().is_empty(), "no asset, so nothing to place");
    match drain!(rx).as_slice() {
        [Event::OrderCancelRejected(e)] => assert_eq!(e.client_order_id, "old-1"),
        other => panic!("expected one OrderCancelRejected, got {other:?}"),
    }
}

// --- cancelling an order this process never placed, by the cloid the venue printed for it (the node's
// --- `recon cancel` of a held `UnknownOrder` row: its id is the hash of a coid nothing stores).

/// A cloid as a `recon ls` row prints it (`0x` and 32 hex digits), and a second order's.
const HEX: &str = "0xb395aaaab395aaaab395aaaab395aaaa";
const OTHER_HEX: &str = "0x00000000000000000000000000000001";

/// One `frontendOpenOrders` row: a resting buy on venue coin `coin` carrying `cloid`.
fn listed(cloid: &str, coin: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: VENUE.into(),
        symbol: coin.into(),
        venue_order_id: "168136755771512".into(),
        client_order_id: Some(cloid.to_string()),
        side: 1,
        order_type: vike_model::OrderType::Limit,
        qty: 0.0002,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: OrderStatus::Accepted,
        ts: 1_000,
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
                cloid_from_client_order_id(HEX),
                "the printed cloid is not hashed a second time"
            );
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
    match drain!(rx).as_slice() {
        [Event::OrderCanceled(c)] => assert_eq!(c.client_order_id, HEX),
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
        match drain!(rx).as_slice() {
            [Event::OrderCancelRejected(e)] => {
                assert_eq!(e.client_order_id, HEX, "{name}");
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
    match drain!(rx).as_slice() {
        [Event::OrderCancelRejected(e)] => {
            assert_eq!(e.client_order_id, HEX);
            assert!(e.reason.contains("listed more than once"), "{}", e.reason);
        }
        other => panic!("expected exactly one OrderCancelRejected, got {other:?}"),
    }
}

/// The new door changes nothing for an id that is ours: a coid this thread placed cancels exactly as
/// `cancel` always did (the cloid is the hash of the coid), and an unknown one still says so.
#[test]
fn cancel_any_of_our_own_coid_is_todays_cancel() {
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
    st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));

    st.cancel_any(&symbology(), "c1");
    st.cancel_any(&symbology(), "ghost");

    match &st.exchange.actions.lock().unwrap()[1..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(got, vec![(0, cloid_from_client_order_id("c1"))]);
        }
        other => panic!("expected the one cancel of c1 after the order, got {other:?}"),
    }
    let evs = drain!(rx);
    assert!(evs.iter().any(|e| matches!(e, Event::OrderCanceled(c) if c.client_order_id == "c1")));
    let ghost: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            Event::OrderCancelRejected(r) if r.client_order_id == "ghost" => {
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
    st.adopt(&symbology(), HEX, "BTC");

    st.cancel_any(&symbology(), HEX);

    match &st.exchange.actions.lock().unwrap()[..] {
        [Action::CancelByCloid(a)] => {
            let got: Vec<(u32, String)> =
                a.cancels.iter().map(|c| (c.asset, c.cloid.clone())).collect();
            assert_eq!(got, vec![(0, cloid_from_client_order_id(HEX))], "the adopted BTC asset");
        }
        other => panic!("expected one CancelByCloid, got {other:?}"),
    }
}

/// The shape of a venue cloid: `0x` and exactly 32 hex digits, nothing a minted coid can be.
#[test]
fn only_0x_and_thirty_two_hex_digits_is_a_venue_cloid() {
    assert!(order_wire::is_wire_cloid(HEX));
    assert!(order_wire::is_wire_cloid(&cloid_from_client_order_id("any")));
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

/// An adopt never clobbers what a live order already knows: re-announcing a coid this session placed
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
    st.submit(&symbology(), std::slice::from_ref(&limit_req("c1", 1, 50000.0, 0.01)));
    st.adopt(&symbology(), "c1", "BTC");
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
        let req = limit_req("c1", 1, 50000.0, 0.01);
        st.submit(&symbology(), std::slice::from_ref(&req));
        let _ = drain!(rx);
        st.modify(&symbology(), &req, None, Some(50500.0));
        let _ = drain!(rx);
        assert!(st.registry.take_retired(11), "{what}: the possibly-dead oid is retired");
        let actions_before = st.exchange.actions.lock().unwrap().len();

        st.modify(&symbology(), &req, None, Some(50600.0));
        match drain!(rx).as_slice() {
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
) -> (ExecState<FakeExchange>, OrderRequest) {
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
    let req = limit_req("c1", 1, 50000.0, 0.01);
    st.submit(&symbology(), std::slice::from_ref(&req));
    st.modify(&symbology(), &req, None, Some(50500.0));
    (st, req)
}

/// Feed one `orderUpdates` row for `c1`'s cloid through the REAL pump mapper.
fn ws_row(st: &ExecState<FakeExchange>, coid: &str, oid: u64, status: &str) -> Vec<Event> {
    let cloid = cloid_from_client_order_id(coid);
    let frame = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"50500","sz":"0.01","oid":oid,"cloid":cloid,"timestamp":1},
        "status":status,"statusTimestamp":2
    }]});
    crate::exec::user_data::map_frame_to_events(
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
    let _ = drain!(rx);
    // unrelated: a foreign cloid, and a non-open row of ours, are not an oid to adopt
    let other = st.registry.register("other");
    let foreign = json!({"channel":"orderUpdates","data":[{
        "order":{"coin":"BTC","side":"B","limitPx":"1","sz":"1","oid":777,"cloid":other,"timestamp":1},
        "status":"open","statusTimestamp":2}]});
    let _ = crate::exec::user_data::map_frame_to_events(
        &foreign,
        &symbology(),
        &st.registry,
        &AtomicBool::new(true),
        0,
    );
    assert_eq!(ws_row(&st, "c1", 33, "triggered").len(), 1);

    // the stale cancel of the retired old oid: dropped, one-shot
    assert!(ws_row(&st, "c1", 11, "canceled").is_empty(), "stale canceled suppressed");
    // the replacement
    assert_eq!(ws_row(&st, "c1", 22, "open").len(), 1);

    st.modify(&symbology(), &req, None, Some(50600.0));
    let actions = st.exchange.actions.lock().unwrap();
    match actions.last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22, "next modify targets the learned oid"),
        other => panic!("expected a Modify, got {other:?}"),
    }
    drop(actions);
    assert_eq!(st.orders["c1"].oid, Some(99), "and then rolls to the reply's oid");
    assert!(matches!(drain!(rx).as_slice(), [Event::OrderModified(_)]));
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 0, "the WS answered; no requery");
}

/// Nothing streamed: ONE `orderStatus` requery is spent at the next modify. `open` under a new oid
/// is adopted; a second attempt does not ask again.
#[test]
fn an_order_status_requery_learns_the_oid_once() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_oid = Some(22));
    let _ = drain!(rx);
    st.modify(&symbology(), &req, None, Some(50600.0));
    match st.exchange.actions.lock().unwrap().last().unwrap() {
        Action::Modify(m) => assert_eq!(m.oid, 22),
        other => panic!("expected a Modify, got {other:?}"),
    }
    assert_eq!(*st.exchange.status_calls.lock().unwrap(), 1);
    let _ = drain!(rx);

    // a definitive `unknownOid`: still refused inside the window, and not asked a second time
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_unknown = true);
    let _ = drain!(rx);
    for _ in 0..2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        assert!(matches!(drain!(rx).as_slice(), [Event::OrderModifyRejected(_)]));
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
    let _ = drain!(rx);
    for asked in 1..=2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        match drain!(rx).as_slice() {
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
    let _ = drain!(rx);
    for asked in 1..=2 {
        st.modify(&symbology(), &req, None, Some(50600.0));
        assert!(matches!(drain!(rx).as_slice(), [Event::OrderModifyRejected(_)]));
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
    let _ = drain!(rx);
    let _ = ws_row(&st, "c1", 11, "open");
    st.modify(&symbology(), &req, None, Some(50600.0));
    assert!(matches!(drain!(rx).as_slice(), [Event::OrderModifyRejected(_)]), "still refused");
    assert!(st.orders["c1"].unknown_since.is_some(), "the unknown mark stays");
    assert_eq!(st.orders["c1"].oid, Some(11));
    assert!(ws_row(&st, "c1", 11, "canceled").is_empty(), "the stale canceled is still suppressed");
}

/// A failed `orderStatus` request says nothing about the order and must not spend the one-shot: the
/// next modify asks again and adopts what the venue then answers.
#[test]
fn a_failed_order_status_read_does_not_spend_the_requery() {
    let (tx, mut rx) = event_channel(64);
    let (mut st, req) = after_ambiguous_modify(tx, |f| f.status_fails = true);
    let _ = drain!(rx);
    st.modify(&symbology(), &req, None, Some(50600.0));
    assert!(matches!(drain!(rx).as_slice(), [Event::OrderModifyRejected(_)]));
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
