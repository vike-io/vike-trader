//! R5(a) golden parity: the live-core logic replayed against the FROZEN `fixtures/r5/*.json`
//! bytes. Bit-for-bit on every f64; behavioral identity on FSM edges, dedup, delivery order.
//!
//! ⚠ The committed bytes ARE the oracle; the Python exporter is gone
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). The exactness is
//! REQUIRED: a frozen fixture is a claim about THIS code not changing its arithmetic or its FSM
//! unnoticed. What "the oracle" DID (the halt verdicts) is the Python behaviour those bytes
//! recorded, not a thing that can be re-run.
//!
//! Files: fsm.json (transition matrix + VWAP), risk.json (gate cases + banker's-rounding
//! ties + throttle + clamp), order_client_id.json, bus.json (defer-and-deliver FIFO-not-DFS), and
//! hub.json (12 Engine scenarios: replay dedup, out-of-order, unknown order client ids, symbol/
//! venue filters + balance-mode flip, denials, throttle, normalization, hedge liquidation,
//! snapshot seeding, sim-client roundtrip).

use std::path::PathBuf;
use vike_execution::testing::{RecordingClient, TestExecutionClient};
use vike_execution::{
    Account, BalanceMode, Engine, EventBus, EventBusOutbox, EventHandler, ExecutionClient,
    ReconcileSnapshot, RiskContext, RiskGate,
};
use vike_model::RiskLimits;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderDenied, OrderExpired, OrderFilled,
    OrderLiquidated, OrderPartiallyFilled, OrderRejected, OrderSubmitted, OrderTriggered,
    PositionSide,
};
use vike_model::{OrderRecord, OrderStatus, OrderTransitionError, TradingState};
use vike_model::{OrderRequest, f64_from_hex_bits, f64_to_hex_bits};

fn fixture(name: &str) -> serde_json::Value {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r5");
    vike_execution::testing::frozen_fixture(&dir, name, "scripts/export_r5_fixtures.py")
}

fn hex(v: &serde_json::Value) -> String {
    v.as_str().unwrap().to_string()
}

fn assert_bits(actual: f64, expected_hex: &serde_json::Value, what: &str) {
    let want = expected_hex.as_str().unwrap();
    let got = f64_to_hex_bits(actual);
    assert_eq!(
        got,
        want,
        "{what}: got {actual} ({got}), python {} ({want})",
        f64_from_hex_bits(want).unwrap()
    );
}

// --- FSM: transition matrix + VWAP accumulation ---

fn matrix_fill() -> FillEvent {
    // exporter's fixed fill: t1 / m1 / +0.4 @ 100.5, taker
    FillEvent {
        trade_id: "t1".into(),
        order_client_id: "m1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 0.4,
        last_price: 100.5,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: 0,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn matrix_event(name: &str) -> Event {
    let order_client_id = "m1".to_string();
    match name {
        "OrderSubmitted" => Event::OrderSubmitted(OrderSubmitted { order_client_id, timestamp: 0 }),
        "OrderAccepted" => Event::OrderAccepted(OrderAccepted {
            order_client_id,
            venue_order_id: Some("v9".into()),
            timestamp: 0,
        }),
        "OrderRejected" => Event::OrderRejected(OrderRejected {
            order_client_id,
            reason: "r".into(),
            timestamp: 0,
            cause: None,
            refusal_id: None,
        }),
        "OrderDenied" => Event::OrderDenied(OrderDenied {
            order_client_id,
            reason: "d".into(),
            timestamp: 0,
            refusal_id: None,
        }),
        "OrderTriggered" => Event::OrderTriggered(OrderTriggered { order_client_id, timestamp: 0 }),
        "OrderPartiallyFilled" => Event::OrderPartiallyFilled(OrderPartiallyFilled {
            order_client_id,
            fill: matrix_fill(),
            timestamp: 0,
        }),
        "OrderFilled" => {
            Event::OrderFilled(OrderFilled { order_client_id, fill: matrix_fill(), timestamp: 0 })
        }
        "OrderCanceled" => Event::OrderCanceled(OrderCanceled {
            order_client_id,
            reason: "user".into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        }),
        "OrderExpired" => Event::OrderExpired(OrderExpired { order_client_id, timestamp: 0 }),
        "OrderLiquidated" => Event::OrderLiquidated(OrderLiquidated {
            order_client_id,
            liquidation_price: 90.0,
            timestamp: 0,
        }),
        "FillEvent" => Event::Fill(matrix_fill()),
        other => panic!("unknown matrix event {other}"),
    }
}

fn base_request() -> OrderRequest {
    serde_json::from_value(serde_json::json!({
        "order_client_id": "m1", "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap()
}

#[test]
fn state_machine_transition_matrix() {
    let fx = fixture("fsm.json");
    let rows = fx["matrix"].as_array().unwrap();
    assert_eq!(rows.len(), 14 * 11, "14 statuses x 11 events");
    for row in rows {
        let from = OrderStatus::parse(row["from"].as_str().unwrap()).unwrap();
        let event = matrix_event(row["event"].as_str().unwrap());
        let mut mo = OrderRecord::new(base_request());
        mo.status = from;
        let result = mo.apply(&event);
        let what = format!("{} + {}", row["from"], row["event"]);
        if row["ok"].as_bool().unwrap() {
            assert!(result.is_ok(), "{what}: python ok, rust rejected");
            assert_eq!(mo.status.as_str(), row["to"].as_str().unwrap(), "{what}");
            assert_eq!(mo.venue_order_id.as_deref(), row["venue_order_id"].as_str(), "{what}");
            assert_bits(mo.filled_qty, &row["filled_qty"], &format!("{what} filled_qty"));
            assert_bits(
                mo.average_fill_price,
                &row["average_fill_price"],
                &format!("{what} average_price"),
            );
        } else {
            let err: OrderTransitionError =
                result.expect_err(&format!("{what}: python raised, rust allowed"));
            assert_eq!(err.status, from);
        }
    }
}

#[test]
fn state_machine_vwap_accumulation() {
    let fx = fixture("fsm.json");
    for (i, seq) in fx["vwap"].as_array().unwrap().iter().enumerate() {
        let fills = seq["fills"].as_array().unwrap();
        let n = fills.len();
        let mut mo = OrderRecord::new(base_request());
        mo.apply(&matrix_event("OrderSubmitted")).unwrap();
        mo.apply(&matrix_event("OrderAccepted")).unwrap();
        for (j, f) in fills.iter().enumerate() {
            let mut fill = matrix_fill();
            fill.trade_id = vike_model::events::TradeId::prefixed("t", j);
            fill.last_qty = f[0].as_f64().unwrap();
            fill.last_price = f[1].as_f64().unwrap();
            let event = if j == n - 1 {
                Event::OrderFilled(OrderFilled { order_client_id: "m1".into(), fill, timestamp: 0 })
            } else {
                Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    order_client_id: "m1".into(),
                    fill,
                    timestamp: 0,
                })
            };
            mo.apply(&event).unwrap();
            let st = &seq["states"][j];
            assert_eq!(mo.status.as_str(), st["status"].as_str().unwrap());
            assert_bits(mo.filled_qty, &st["filled_qty"], &format!("vwap[{i}][{j}] qty"));
            assert_bits(
                mo.average_fill_price,
                &st["average_fill_price"],
                &format!("vwap[{i}][{j}] avg"),
            );
        }
    }
}

// --- RiskGate ---

fn limits_from(v: &serde_json::Value) -> RiskLimits {
    let f = |k: &str| v[k].as_f64();
    RiskLimits {
        tick_size: f("tick_size"),
        lot_size: f("lot_size"),
        min_notional: f("min_notional"),
        min_qty: None,
        max_notional_per_order: f("max_notional_per_order"),
        max_total_exposure: f("max_total_exposure"),
        max_orders_per_window: v["max_orders_per_window"].as_u64().map(|n| n as usize),
        window_ms: v["window_ms"].as_i64().unwrap_or(1000),
        max_leverage: None,
        block_reduce_only_overshoot: v["block_reduce_only_overshoot"].as_bool().unwrap_or(false),
        // Knobs newer than the oracle fixtures stay off, so the r5 verdicts are byte-identical:
        // Phase B margin knobs,
        initial_margin_requirement: None,
        initial_margin_by_symbol: Default::default(),
        required_free_buying_power_percent: 0.0,
        // pre-trade impact (the book-free `check` never walks a book anyway),
        max_slippage_bps: None,
        require_fillable: false,
        // the fat-finger price collar (armed, it would deny rows priced far from the mark on
        // purpose),
        price_collar: None,
        collar_by_symbol: Default::default(),
        grid_by_symbol: Default::default(),
        deny_unadmitted_symbols: false,
        // the ACCOUNT-aggregate ceiling (it reads the account's OTHER symbols, which a row built
        // from fixture scalars cannot supply),
        max_account_exposure: None,
        // ⚠ and the SIZING-EQUITY ceiling, for a reason of its own: it bounds the equity the
        // ENGINE resolves (`Engine::sizing_equity`), while this harness feeds
        // `RiskContext::equity` from the row directly, so it would have nothing to act on.
        max_sizing_equity: None,
    }
}

fn trading_state(s: &str) -> TradingState {
    match s {
        "ACTIVE" => TradingState::Active,
        "REDUCING" => TradingState::Reducing,
        "HALTED" => TradingState::Halted,
        other => panic!("unknown trading state {other}"),
    }
}

/// The ONE case index where vike DELIBERATELY diverges from the risk verdicts recorded in
/// `fixtures/r5/risk.json` — the frozen R5 oracle.
///
/// Those verdicts deny EVERY order under `HALTED`, `reduce_only` and flatten legs included, so a
/// halted core could not close a position — the panic button disarmed in exactly the situations
/// that reach `HALTED` on their own. vike admits a POSITION-COVERED reduce there
/// (`vike_model::is_covered_reduce`): a kill switch stops opening risk without trapping you in it.
///
/// Of the corpus's **33** `HALTED` cases (the fixture denies all 33) exactly ONE is a covered
/// reduce: case 237 — position `+270.710434`, SELL `209.68192517`, `reduce_only: false`, so covered
/// by DIRECTION AND MAGNITUDE alone. The other 32 are still denied, byte-identically.
///
/// Exhaustive and self-verifying, not a skipped assertion: the loop asserts vike ADMITS every
/// divergent case, then that the divergence set is EXACTLY this list, so widening the rule fails
/// here with the new indices named. Do NOT add an index without stating why the recorded denial is
/// wrong for it. ⚠ The list moves only by a deliberate edit to committed fixture bytes, which is a
/// changed contract, not a refresh (`fixtures/README.md`).
const HALT_DIVERGENCE: &[usize] = &[237];

#[test]
fn risk_gate_cases() {
    let fx = fixture("risk.json");
    let mut diverged: Vec<usize> = Vec::new();
    for (i, case) in fx["cases"].as_array().unwrap().iter().enumerate() {
        let mut gate = RiskGate::new(limits_from(&case["limits"]));
        let request: OrderRequest = serde_json::from_value(case["request"].clone()).unwrap();
        let c = &case["ctx"];
        let ctx = RiskContext {
            position_size: c["position_size"].as_f64().unwrap(),
            mark_price: c["mark_price"].as_f64().unwrap(),
            trading_state: trading_state(c["trading_state"].as_str().unwrap()),
            now_ms: c["now_ms"].as_i64().unwrap(),
            ..RiskContext::default() // Phase B margin fields: unused by the oracle cases
        };
        let v = gate.check(&request, &ctx);

        // THE DELIBERATE DIVERGENCE (see `HALT_DIVERGENCE`); every other halted case matches.
        if ctx.trading_state == TradingState::Halted
            && vike_model::is_covered_reduce(request.side, ctx.position_size, request.qty)
        {
            assert!(
                v.ok,
                "case {i}: a position-covered reduce must be ADMITTED under HALTED — this is the \
                 whole of the divergence from the oracle, and a denial here means the exemption \
                 regressed: {v:?}"
            );
            assert!(
                !case["ok"].as_bool().unwrap(),
                "case {i}: listed as a divergence, but the ORACLE admits it too — then it is not a \
                 divergence and this arm is hiding a real parity failure"
            );
            diverged.push(i);
            continue;
        }

        assert_eq!(v.ok, case["ok"].as_bool().unwrap(), "case {i}: ok mismatch");
        assert_eq!(v.reason, case["reason"].as_str().unwrap(), "case {i}: reason");
        if v.ok {
            let req = v.request.unwrap();
            match (&req.price, case["price"].as_str()) {
                (Some(p), Some(_)) => assert_bits(*p, &case["price"], &format!("case {i} px")),
                (None, None) => {}
                (a, b) => panic!("case {i}: price presence mismatch rust={a:?} py={b:?}"),
            }
            assert_bits(req.qty, &case["qty"], &format!("case {i} qty"));
        }
    }
    assert_eq!(
        diverged, HALT_DIVERGENCE,
        "the set of cases where vike deliberately disagrees with the oracle changed. Every index \
         here must be a HALTED covered reduce that the oracle denies; anything else is a parity \
         regression wearing this exception as a disguise."
    );
}

#[test]
fn risk_rounding_is_bankers() {
    let fx = fixture("risk.json");
    for row in fx["rounding"].as_array().unwrap() {
        let value = row["value"].as_f64().unwrap();
        let step = row["step"].as_f64().unwrap();
        let step = if step > 0.0 { Some(step) } else { None };
        assert_bits(
            vike_model::round_to(value, step),
            &row["result"],
            &format!("round_to({value}, {step:?})"),
        );
    }
}

#[test]
fn risk_throttle_sliding_window() {
    let fx = fixture("risk.json");
    let mut gate = RiskGate::new(RiskLimits {
        max_orders_per_window: Some(3),
        window_ms: 1000,
        min_notional: Some(5.0),
        ..RiskLimits::new()
    });
    for row in fx["throttle"].as_array().unwrap() {
        let t = row["now_ms"].as_i64().unwrap();
        let qty = row["qty"].as_f64().unwrap();
        let mut request = base_request();
        request.order_client_id = format!("th{t}");
        request.qty = qty;
        let ctx = RiskContext { mark_price: 100.0, now_ms: t, ..RiskContext::default() };
        let v = gate.check(&request, &ctx);
        assert_eq!(v.ok, row["ok"].as_bool().unwrap(), "t={t}");
        assert_eq!(v.reason, row["reason"].as_str().unwrap(), "t={t}");
    }
}

#[test]
fn risk_clamp_leverage() {
    let fx = fixture("risk.json");
    for row in fx["clamp_leverage"].as_array().unwrap() {
        let requested = row["requested"].as_f64().unwrap();
        let max = row["max"].as_f64();
        assert_bits(
            vike_model::clamp_leverage(requested, max),
            &row["result"],
            &format!("clamp({requested}, {max:?})"),
        );
    }
}

// --- OrderClientIdGenerator ---

#[test]
fn order_client_id_minter_and_charset() {
    let fx = fixture("coid.json");
    let mut minter = vike_model::orders::client_id::OrderClientIdGenerator::new(Some(
        fx["session"].as_str().unwrap(),
    ));
    for want in fx["ids"].as_array().unwrap() {
        assert_eq!(minter.generate(), want.as_str().unwrap());
    }
    for row in fx["valid"].as_array().unwrap() {
        let order_client_id = row["coid"].as_str().unwrap();
        assert_eq!(
            vike_model::is_valid_crypto_order_client_id(order_client_id),
            row["ok"].as_bool().unwrap(),
            "charset({order_client_id:?})"
        );
    }
}

// --- EventBus defer-and-deliver (FIFO, not DFS) ---

struct Chaining;

impl EventHandler for Chaining {
    fn on_event(
        &mut self,
        event: &Event,
        outbox: &mut EventBusOutbox,
    ) -> vike_execution::EventBusFold {
        // e1 -> publish e2 AND e3; e2 -> publish e4 (mirror of the Python fixture handler)
        if let Event::OrderSubmitted(e) = event {
            let pub_sub = |order_client_id: &str| {
                Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: order_client_id.to_string(),
                    timestamp: 0,
                })
            };
            match e.order_client_id.as_str() {
                "e1" => {
                    outbox.publish(pub_sub("e2"));
                    outbox.publish(pub_sub("e3"));
                }
                "e2" => outbox.publish(pub_sub("e4")),
                _ => {}
            }
        }
        vike_execution::EventBusFold::Applied
    }
}

#[test]
fn bus_defer_and_deliver_fifo() {
    let fx = fixture("bus.json");
    let mut bus = EventBus::new();
    let mut handler = Chaining;
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { order_client_id: "e1".into(), timestamp: 0 }),
        &mut handler,
    );
    let got: Vec<&str> = bus
        .delivered
        .iter()
        .map(|event| match event {
            Event::OrderSubmitted(e) => e.order_client_id.as_str(),
            _ => unreachable!(),
        })
        .collect();
    let want: Vec<&str> =
        fx["delivered"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(got, want, "FIFO defer-and-deliver order (DFS would give e1,e2,e4,e3)");
}

// --- Engine scenarios ---

enum TestClient {
    Rec(RecordingClient),
    Sim(TestExecutionClient),
}

impl ExecutionClient for TestClient {
    fn submit(&mut self, request: &OrderRequest) {
        match self {
            TestClient::Rec(c) => c.submit(request),
            TestClient::Sim(c) => c.submit(request),
        }
    }
    fn cancel(&mut self, order_client_id: &str) {
        match self {
            TestClient::Rec(c) => c.cancel(order_client_id),
            TestClient::Sim(c) => c.cancel(order_client_id),
        }
    }
    fn detach(&mut self) {
        match self {
            TestClient::Rec(c) => c.detach(),
            TestClient::Sim(c) => c.detach(),
        }
    }
}

impl TestClient {
    fn pop_pending(&mut self) -> Option<Event> {
        match self {
            TestClient::Sim(c) => c.pending.pop_front(),
            TestClient::Rec(_) => None,
        }
    }
}

fn snapshot_from(v: &serde_json::Value) -> ReconcileSnapshot {
    let pairs = |key: &str| -> Vec<(String, f64)> {
        v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_f64().unwrap()))
            .collect()
    };
    let open_orders = v["open_orders"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| {
            let mut mo: OrderRecord =
                OrderRecord::new(serde_json::from_value(o["request"].clone()).unwrap());
            mo.status = OrderStatus::parse(o["status"].as_str().unwrap()).unwrap();
            mo.venue_order_id = o["venue_order_id"].as_str().map(str::to_string);
            mo.filled_qty = o["filled_qty"].as_f64().unwrap();
            mo.average_fill_price = o["average_fill_price"].as_f64().unwrap();
            mo
        })
        .collect();
    ReconcileSnapshot {
        positions: pairs("positions"),
        open_orders,
        position_average_price: pairs("position_average_price"),
        position_mark_price: pairs("position_mark_price"),
        position_sides: v["position_sides"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let side: PositionSide = serde_json::from_value(p[1].clone()).unwrap();
                (p[0].as_str().unwrap().to_string(), side)
            })
            .collect(),
        balance: v["balance"].as_f64().unwrap(),
        // R5 fixtures predate the margin-mode carrier: apply_snapshot carries priors forward
        // (default-flat ⇒ Cross/None, byte-identical).
        position_margin: Vec::new(),
    }
}

/// Pump the sim client's queued venue events through the bus (the recorded Python client
/// published them synchronously inside submit).
fn pump(bus: &mut EventBus, engine: &mut Engine<TestClient>) {
    while let Some(event) = engine.client.pop_pending() {
        bus.publish(event, engine);
    }
}

#[test]
fn execution_engine_scenarios() {
    let fx = fixture("hub.json");
    for sc in fx["scenarios"].as_array().unwrap() {
        let name = sc["name"].as_str().unwrap();
        let venue = sc["venue"].as_str().unwrap();
        let symbol = sc["symbol"].as_str().unwrap();
        let account =
            Account::new(sc["multiplier"].as_f64().unwrap(), venue, None, BalanceMode::Delta);
        let limits =
            if sc["limits"].is_null() { RiskLimits::new() } else { limits_from(&sc["limits"]) };
        let client = match sc["client"].as_str().unwrap() {
            "sim" => {
                TestClient::Sim(TestExecutionClient::new(venue, sc["sim_mark"].as_f64().unwrap()))
            }
            _ => TestClient::Rec(RecordingClient::default()),
        };
        let mut engine = Engine::new(account, RiskGate::new(limits), client, venue, symbol);
        let mut bus = EventBus::new();
        // The two sums the `fees_paid` fence below is stated over (decision 0021, the third instance's
        // second site): the forced-close fees this engine really charges (an accepted symbol, a trade id
        // not folded before, a position that was open), and the commissions of the scenario's fill events.
        let (mut liquidation_fees, mut fill_fees) = (0.0_f64, 0.0_f64);
        let mut liquidation_seen = std::collections::HashSet::<String>::new();

        for act in sc["actions"].as_array().unwrap() {
            match act["do"].as_str().unwrap() {
                "submit" => {
                    let request: OrderRequest =
                        serde_json::from_value(act["request"].clone()).unwrap();
                    let now_ms = act["now_ms"].as_i64().unwrap();
                    let mut outbox = EventBusOutbox::default();
                    engine.submit_order(&request, now_ms, &mut outbox);
                    while let Some(event) = outbox.0.pop_front() {
                        bus.publish(event, &mut engine);
                    }
                    pump(&mut bus, &mut engine);
                }
                "cancel" => {
                    engine.cancel_order(act["coid"].as_str().unwrap());
                    pump(&mut bus, &mut engine);
                }
                "event" => {
                    let event: Event = serde_json::from_value(act["event"].clone()).unwrap();
                    match &event {
                        Event::Fill(f) => fill_fees += f.commission,
                        // The engine's own rule (`fold.rs`'s PositionLiquidated arm and
                        // `Account::apply_liquidation`): a symbol it does not accept is dropped, a
                        // non-empty trade id is folded once, and a flat position charges nothing.
                        Event::PositionLiquidated(l)
                            if engine.accepts_symbol(&l.symbol)
                                && (l.trade_id.is_empty()
                                    || liquidation_seen.insert(l.trade_id.to_string()))
                                && engine
                                    .account
                                    .positions
                                    .get(&(l.venue, l.symbol, l.position_side))
                                    .is_some_and(|p| p.size != 0.0) =>
                        {
                            liquidation_fees += l.fee;
                        }
                        _ => {}
                    }
                    bus.publish(event, &mut engine);
                }
                "set_state" => {
                    engine.trading_state = trading_state(act["state"].as_str().unwrap());
                }
                "snapshot" => engine.apply_snapshot(&snapshot_from(&act["snapshot"])),
                "shutdown" => engine.shutdown(),
                other => panic!("unknown action {other}"),
            }
        }

        let e = &sc["expect"];
        let account_value = &engine.account;
        assert_bits(account_value.balance, &e["balance"], &format!("{name}: balance"));
        assert_bits(account_value.realized_pnl, &e["realized_pnl"], &format!("{name}: realized"));
        // ⚠ THE FENCE on `fees_paid` (decision 0021, the third instance's second site). The frozen bytes
        // were exported while a forced-close fee reached `balance` and NOT `fees_paid`; this port books it
        // into both (FEES-AUDIT 2026-10-26 §4.8 a), so for the scenario that liquidates an open position
        // the oracle's figure is no longer the whole of ours. The comparison stays EXACT, bits against bits,
        // against a stated function of the oracle's own values: its `fees_paid` plus the forced-close fees
        // the scenario really charges. The other guards: the oracle's raw value is still gated (for the
        // liquidating scenario, equal to its fills' commissions; for every other scenario it is the whole
        // comparison, since they charge no forced-close fee), a control fails if the relaxation is a no-op,
        // and the committed fixture bytes are untouched.
        let oracle_fees = f64_from_hex_bits(e["fees_paid"].as_str().unwrap()).unwrap();
        assert_bits(
            account_value.fees_paid,
            &serde_json::json!(f64_to_hex_bits(oracle_fees + liquidation_fees)),
            &format!("{name}: fees"),
        );
        if name == "liquidation_hedge" {
            assert_bits(
                fill_fees,
                &e["fees_paid"],
                &format!("{name}: the oracle's fees_paid is its fills'"),
            );
            assert!(
                liquidation_fees > 0.0,
                "{name}: the control scenario must really liquidate with a fee"
            );
            assert_ne!(
                f64_to_hex_bits(account_value.fees_paid),
                e["fees_paid"].as_str().unwrap(),
                "{name}: the fence is a no-op here, so it proves nothing"
            );
        } else {
            assert_eq!(
                liquidation_fees, 0.0,
                "{name}: only the liquidation scenario relaxes fees_paid"
            );
        }
        assert_bits(account_value.funding_paid, &e["funding_paid"], &format!("{name}: funding"));
        let mode = match account_value.balance_mode {
            BalanceMode::Delta => "delta",
            BalanceMode::Authoritative => "authoritative",
        };
        assert_eq!(mode, e["balance_mode"].as_str().unwrap(), "{name}: mode");
        assert_bits(
            account_value.equity_all(sc["seed_cash"].as_f64().unwrap()),
            &e["equity_all"],
            &format!("{name}: equity_all"),
        );

        let want_pos = e["positions"].as_array().unwrap();
        assert_eq!(account_value.positions.len(), want_pos.len(), "{name}: position count");
        for (((v, s, ps), p), w) in account_value.positions.iter().zip(want_pos) {
            assert_eq!(v, w[0].as_str().unwrap(), "{name}: pos venue");
            assert_eq!(s, w[1].as_str().unwrap(), "{name}: pos symbol");
            assert_eq!(ps.to_string(), w[2].as_str().unwrap(), "{name}: pos side");
            assert_bits(p.size, &w[3], &format!("{name}: pos size"));
            assert_bits(p.average_price, &w[4], &format!("{name}: pos avg"));
        }
        let want_marks = e["marks"].as_array().unwrap();
        assert_eq!(account_value.marks_iter().count(), want_marks.len(), "{name}: mark count");
        for (((v, s), px), w) in account_value.marks_iter().zip(want_marks) {
            assert_eq!(v, w[0].as_str().unwrap(), "{name}: mark venue");
            assert_eq!(s, w[1].as_str().unwrap(), "{name}: mark symbol");
            assert_bits(*px, &w[2], &format!("{name}: mark px"));
        }

        let want_reg = e["registry"].as_array().unwrap();
        assert_eq!(engine.registry.len(), want_reg.len(), "{name}: registry count");
        for ((order_client_id, mo), w) in engine.registry.iter().zip(want_reg) {
            assert_eq!(order_client_id, w[0].as_str().unwrap(), "{name}: registry order");
            assert_eq!(
                mo.status.as_str(),
                w[1].as_str().unwrap(),
                "{name}: {order_client_id} status"
            );
            assert_bits(mo.filled_qty, &w[2], &format!("{name}: {order_client_id} filled_qty"));
            assert_bits(
                mo.average_fill_price,
                &w[3],
                &format!("{name}: {order_client_id} average_price"),
            );
            assert_eq!(
                mo.venue_order_id.as_deref(),
                w[4].as_str(),
                "{name}: {order_client_id} venue_order_id"
            );
        }

        // outbound events: denials + full delivery order
        let denied: Vec<(String, String)> = bus
            .delivered
            .iter()
            .filter_map(|event| match event {
                Event::OrderDenied(d) => Some((d.order_client_id.clone(), d.reason.to_string())),
                _ => None,
            })
            .collect();
        let want_denied: Vec<(String, String)> = e["denied"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| (hex(&d["coid"]), hex(&d["reason"])))
            .collect();
        assert_eq!(denied, want_denied, "{name}: denied");
        let delivered: Vec<&str> = bus.delivered.iter().map(Event::wire_tag).collect();
        let want_delivered: Vec<&str> =
            e["delivered"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(delivered, want_delivered, "{name}: delivery order");

        // client interactions
        let (subs, cancels, detached) = match &engine.client {
            TestClient::Rec(c) => (&c.submissions, &c.cancels, c.detached),
            TestClient::Sim(c) => (&c.submissions, &c.cancels, c.detached),
        };
        let want_subs = e["submissions"].as_array().unwrap();
        assert_eq!(subs.len(), want_subs.len(), "{name}: submission count");
        for (r, w) in subs.iter().zip(want_subs) {
            assert_eq!(r.order_client_id, w[0].as_str().unwrap(), "{name}: sub order client id");
            match (&r.price, w[1].as_str()) {
                (Some(p), Some(_)) => assert_bits(*p, &w[1], &format!("{name}: sub px")),
                (None, None) => {}
                (a, b) => panic!("{name}: sub price mismatch rust={a:?} py={b:?}"),
            }
            assert_bits(r.qty, &w[2], &format!("{name}: sub qty"));
        }
        let want_cancels: Vec<&str> =
            e["cancels"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let got_cancels: Vec<&str> = cancels.iter().map(String::as_str).collect();
        assert_eq!(got_cancels, want_cancels, "{name}: cancels");
        assert_eq!(detached, e["detached"].as_bool().unwrap(), "{name}: detached");

        assert_bits(
            engine.position_size(PositionSide::Both),
            &e["position_size"],
            &format!("{name}: position_size"),
        );
        assert_bits(
            engine.total_exposure(),
            &e["total_exposure"],
            &format!("{name}: total_exposure"),
        );
    }
}
