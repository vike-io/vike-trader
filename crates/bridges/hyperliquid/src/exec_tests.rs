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

/// Pins the routed hyperliquid row of `vike_model::venues::venue_tif::venue_tif` byte-for-byte.
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
}
impl HlExchange for FakeExchange {
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
        order_type: "limit".into(),
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
        order_type: "stop".into(),
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
        order_type: "market".into(),
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
            order_type: "market".into(),
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
        order_type: "market".into(),
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
/// `modify_is_default_noop_for_deribit` pattern: drive the actual code path, then assert the
/// declaration, in ONE test. (The crate's `caps_test` module cannot do this: every assertion
/// there compares fields of the SAME constant the row defines, so it is a restatement.)
///
/// The bug this pins: `HYPERLIQUID.supported_order_kinds` omitted `"stop_limit"`, so
/// `vike_model::preflight_order` returned `TRIGGER_UNSUPPORTED` and `vike-core` terminally
/// REJECTED the order — while `build_order_wire` builds it perfectly. The sibling test above
/// proves the wire SHAPE using `order_type: "stop"`; this one uses the literal `"stop_limit"`
/// spelling, because that string is what the preflight classifies. The routing keys off
/// `trigger_price.is_some()`, not the spelling, which is exactly why the two never disagreed
/// on the wire and the gap could hide in the declaration alone.
#[test]
fn stop_limit_kind_is_declared_and_built() {
    // 1. The DECLARATION: the row must list the kind, or the core edge refuses the order.
    let caps = vike_model::caps_for("hyperliquid");
    assert!(
        caps.supported_order_kinds.contains(&"stop_limit"),
        "the row must declare stop_limit — preflight_order refuses undeclared trigger kinds"
    );
    // ...and the core-edge preflight must therefore ADMIT it.
    let mut pf = stop_req("pf1", -1, 48_000.0, Some(47_900.0), 0.01);
    pf.order_type = "stop_limit".into();
    assert_eq!(vike_model::preflight_order(&pf), Ok(()), "core edge must admit stop_limit");

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
    req.order_type = "take_profit".into();
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
        halt_path,
    };
    (client, cmds)
}

/// **This client's halt arms, driven for the first time** — and the decision-0099 property they
/// exist to prove: it watches the sentinel its MOUNT handed it (`with_halt_path`), not a file it
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
    let dir = vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-hl-halt")
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
            assert_eq!(r.reason, vike_bridge_core::halt::HALT_REJECT_REASON);
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
    assert!(matches!(cmds.try_recv(), Ok(HlCommand::Submit(_))), "the reducing order goes out");

    // 3. MODIFY, halted: non-terminal advisory, nothing sent.
    client.modify(&limit_req("opening", 1, 50000.0, 0.01), Some(0.02), None);
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["ModifyRejected"]);
    assert!(cmds.try_recv().is_err());

    // 4. Sentinel removed: the SAME opening order is admitted.
    std::fs::remove_file(&sentinel).expect("rm the sentinel");
    client.submit(&limit_req("resumed", 1, 50000.0, 0.01));
    assert!(drain!(rx).is_empty());
    assert!(matches!(cmds.try_recv(), Ok(HlCommand::Submit(_))), "resumed trading goes out");
}

/// A client handed NO path watches nothing — an engaged file elsewhere on the box (here: a path the
/// test engages and never gives the client) changes no verdict. The decision-0099 regression: the
/// process-wide fallback this client used to take is gone.
#[test]
fn a_hyperliquid_client_handed_no_sentinel_watches_nothing() {
    let dir = vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-hl-nohalt")
        .expect("scratch dir");
    let somewhere = dir.path().join("HALT");
    std::fs::write(&somewhere, b"").expect("a sentinel the client was never told about");
    let (events, mut rx) = event_channel(64);
    let (mut client, cmds) = halt_client(events, None);
    client.submit(&limit_req("opening", 1, 50000.0, 0.01));
    assert!(drain!(rx).is_empty());
    assert!(matches!(cmds.try_recv(), Ok(HlCommand::Submit(_))));
}
