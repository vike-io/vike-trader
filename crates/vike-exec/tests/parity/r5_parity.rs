//! R5(a) golden parity: the live-core logic replayed against the FROZEN `fixtures/r5/*.json`
//! bytes. Bit-for-bit on every f64; behavioral identity on FSM edges, dedup, delivery order.
//!
//! ⚠ This header used to say "vs the Python oracle". It re-derives nothing from Python — the
//! exporter is gone and the committed bytes ARE the oracle
//! (`docs/decisions/0021-python-oracle-retired-vike-is-the-reference.md`). The exactness is
//! REQUIRED, not vestigial: a frozen fixture is a claim about THIS code not changing its
//! arithmetic or its FSM unnoticed. "The oracle" below therefore means those bytes, and where
//! this file describes what "the oracle" DID (the halt verdicts) it is describing the Python
//! behaviour those bytes recorded, not a thing that can be re-run.
//!
//! Files: fsm.json (transition matrix + VWAP), risk.json (gate cases + banker's-rounding
//! ties + throttle + clamp), coid.json, bus.json (defer-and-deliver FIFO-not-DFS), and
//! hub.json (12 ExecutionEngine scenarios: replay dedup, out-of-order, unknown coids, symbol/
//! venue filters + balance-mode flip, denials, throttle, normalization, hedge liquidation,
//! snapshot seeding, sim-client roundtrip).

use std::path::PathBuf;
use vike_exec::testing::{RecordingClient, TestExecutionClient};
use vike_exec::{
    Account, BalanceMode, EventBus, EventHandler, ExecutionClient, ExecutionEngine,
    InvalidOrderTransition, ManagedOrder, OrderStatus, Outbox, ReconcileSnapshot, RiskContext,
    RiskGate, RiskLimits, TradingState,
};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCanceled, OrderDenied, OrderExpired, OrderFilled,
    OrderLiquidated, OrderPartiallyFilled, OrderRejected, OrderSubmitted, OrderTriggered,
};
use vike_model::{f64_from_hex_bits, f64_to_hex_bits, OrderRequest};

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r5").join(name);
    // ⚠ The message used to end "run scripts/export_r5_fixtures.py". That script was deleted with
    // the Python purge and regeneration is NOT a supported operation (`fixtures/README.md`); the
    // path is kept as the evidence for where these bytes came from, not as an instruction.
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{path:?} missing — it is committed and cannot be regenerated \
             (its exporter, scripts/export_r5_fixtures.py, is gone); restore it from git"
        )
    });
    serde_json::from_str(&text).unwrap()
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

fn event_name(ev: &Event) -> &'static str {
    match ev {
        Event::Fill(_) => "FillEvent",
        Event::OrderSubmitted(_) => "OrderSubmitted",
        Event::OrderAccepted(_) => "OrderAccepted",
        Event::OrderRejected(_) => "OrderRejected",
        Event::OrderDenied(_) => "OrderDenied",
        Event::OrderTriggered(_) => "OrderTriggered",
        Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
        Event::OrderFilled(_) => "OrderFilled",
        Event::OrderCanceled(_) => "OrderCanceled",
        Event::OrderExpired(_) => "OrderExpired",
        Event::OrderLiquidated(_) => "OrderLiquidated",
        Event::OrderModified(_) => "OrderModified",
        Event::OrderCancelRejected(_) => "OrderCancelRejected",
        Event::OrderModifyRejected(_) => "OrderModifyRejected",
        Event::PositionOpened(_) => "PositionOpened",
        Event::PositionChanged(_) => "PositionChanged",
        Event::PositionClosed(_) => "PositionClosed",
        Event::AccountState(_) => "AccountState",
        Event::Funding(_) => "FundingEvent",
        Event::PositionLiquidated(_) => "PositionLiquidated",
    }
}

// ------------------------------------------------------------------------------------------
// FSM: transition matrix + VWAP accumulation
// ------------------------------------------------------------------------------------------

fn matrix_fill() -> FillEvent {
    // exporter's fixed fill: t1 / m1 / +0.4 @ 100.5, taker
    FillEvent {
        trade_id: "t1".into(),
        client_order_id: "m1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: 0.4,
        last_px: 100.5,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn matrix_event(name: &str) -> Event {
    let coid = "m1".to_string();
    match name {
        "OrderSubmitted" => Event::OrderSubmitted(OrderSubmitted { client_order_id: coid, ts: 0 }),
        "OrderAccepted" => Event::OrderAccepted(OrderAccepted {
            client_order_id: coid,
            venue_order_id: Some("v9".into()),
            ts: 0,
        }),
        "OrderRejected" => {
            Event::OrderRejected(OrderRejected { client_order_id: coid, reason: "r".into(), ts: 0 })
        }
        "OrderDenied" => {
            Event::OrderDenied(OrderDenied { client_order_id: coid, reason: "d".into(), ts: 0 })
        }
        "OrderTriggered" => Event::OrderTriggered(OrderTriggered { client_order_id: coid, ts: 0 }),
        "OrderPartiallyFilled" => Event::OrderPartiallyFilled(OrderPartiallyFilled {
            client_order_id: coid,
            fill: matrix_fill(),
            ts: 0,
        }),
        "OrderFilled" => {
            Event::OrderFilled(OrderFilled { client_order_id: coid, fill: matrix_fill(), ts: 0 })
        }
        "OrderCanceled" => Event::OrderCanceled(OrderCanceled {
            client_order_id: coid,
            reason: "user".into(),
            ts: 0,
        }),
        "OrderExpired" => Event::OrderExpired(OrderExpired { client_order_id: coid, ts: 0 }),
        "OrderLiquidated" => Event::OrderLiquidated(OrderLiquidated {
            client_order_id: coid,
            liq_price: 90.0,
            ts: 0,
        }),
        "FillEvent" => Event::Fill(matrix_fill()),
        other => panic!("unknown matrix event {other}"),
    }
}

fn base_request() -> OrderRequest {
    serde_json::from_value(serde_json::json!({
        "client_order_id": "m1", "venue": "sim", "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap()
}

#[test]
fn fsm_transition_matrix() {
    let fx = fixture("fsm.json");
    let rows = fx["matrix"].as_array().unwrap();
    assert_eq!(rows.len(), 14 * 11, "14 statuses x 11 events");
    for row in rows {
        let from = OrderStatus::parse(row["from"].as_str().unwrap()).unwrap();
        let ev = matrix_event(row["event"].as_str().unwrap());
        let mut mo = ManagedOrder::new(base_request());
        mo.status = from;
        let result = mo.apply(&ev);
        let what = format!("{} + {}", row["from"], row["event"]);
        if row["ok"].as_bool().unwrap() {
            assert!(result.is_ok(), "{what}: python ok, rust rejected");
            assert_eq!(mo.status.as_str(), row["to"].as_str().unwrap(), "{what}");
            assert_eq!(mo.venue_order_id.as_deref(), row["venue_order_id"].as_str(), "{what}");
            assert_bits(mo.filled_qty, &row["filled_qty"], &format!("{what} filled_qty"));
            assert_bits(mo.avg_fill_px, &row["avg_fill_px"], &format!("{what} avg_px"));
        } else {
            let err: InvalidOrderTransition =
                result.expect_err(&format!("{what}: python raised, rust allowed"));
            assert_eq!(err.status, from);
        }
    }
}

#[test]
fn fsm_vwap_accumulation() {
    let fx = fixture("fsm.json");
    for (i, seq) in fx["vwap"].as_array().unwrap().iter().enumerate() {
        let fills = seq["fills"].as_array().unwrap();
        let n = fills.len();
        let mut mo = ManagedOrder::new(base_request());
        mo.apply(&matrix_event("OrderSubmitted")).unwrap();
        mo.apply(&matrix_event("OrderAccepted")).unwrap();
        for (j, f) in fills.iter().enumerate() {
            let mut fill = matrix_fill();
            fill.trade_id = vike_model::events::TradeId::prefixed("t", j);
            fill.last_qty = f[0].as_f64().unwrap();
            fill.last_px = f[1].as_f64().unwrap();
            let ev = if j == n - 1 {
                Event::OrderFilled(OrderFilled { client_order_id: "m1".into(), fill, ts: 0 })
            } else {
                Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: "m1".into(),
                    fill,
                    ts: 0,
                })
            };
            mo.apply(&ev).unwrap();
            let st = &seq["states"][j];
            assert_eq!(mo.status.as_str(), st["status"].as_str().unwrap());
            assert_bits(mo.filled_qty, &st["filled_qty"], &format!("vwap[{i}][{j}] qty"));
            assert_bits(mo.avg_fill_px, &st["avg_fill_px"], &format!("vwap[{i}][{j}] avg"));
        }
    }
}

// ------------------------------------------------------------------------------------------
// RiskGate
// ------------------------------------------------------------------------------------------

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
        // Phase B knobs stay off — the oracle fixtures predate them
        im_requirement: None,
        im_by_symbol: Default::default(),
        required_free_bp_pct: 0.0,
        // pre-trade impact knobs stay off — the oracle fixtures predate them (and the gate
        // never walks a book through the book-free `check`)
        max_slippage_bps: None,
        require_fillable: false,
        // the fat-finger price collar stays off for the same reason: the oracle fixtures predate
        // it, and an armed collar would deny fixture rows whose price is deliberately far from
        // the mark. Off ⇒ the r5 verdicts are byte-identical.
        price_collar: None,
        collar_by_symbol: Default::default(),
        grid_by_symbol: Default::default(),
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
/// Those recorded verdicts deny EVERY order under `HALTED`, `reduce_only` and flatten legs
/// included, which means a halted core cannot close a position — the panic button disarmed in
/// exactly the situations that reach `HALTED` on their own. vike now admits a POSITION-COVERED
/// reduce there (`vike_model::is_covered_reduce`), so a kill switch stops opening risk without
/// trapping you in it.
///
/// The corpus has **33** `HALTED` cases and the fixture denies all 33. Exactly ONE of them is a
/// covered reduce: case 237 — position `+270.710434`, SELL `209.68192517`, and note
/// `reduce_only: false`, so it is covered by DIRECTION AND MAGNITUDE alone, not by anybody's flag.
/// The other 32 are still denied, byte-identically.
///
/// This is an exhaustive, self-verifying exception rather than a skipped assertion: the loop below
/// asserts vike ADMITS every divergent case, and then asserts the divergence set is EXACTLY this
/// list. Widening the rule fails here with the new indices named, which is the point. Do NOT add an
/// index without stating why the recorded denial is wrong for it.
///
/// ⚠ This paragraph used to end "— or regenerating the fixture — fails here". Regenerating it is no
/// longer a thing anyone can do: the exporter is gone and the bytes are the oracle. The only way
/// this list moves now is a deliberate edit to committed fixture bytes, which is a changed
/// contract rather than a refresh (`fixtures/README.md`).
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

        // THE DELIBERATE DIVERGENCE (see `HALT_DIVERGENCE`). A halted covered reduce is admitted
        // here and denied by the oracle; every other halted case still matches it exactly.
        if ctx.trading_state == TradingState::Halted
            && vike_model::is_covered_reduce(
                request.reduce_only,
                request.side,
                ctx.position_size,
                request.qty,
            )
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
            vike_exec::round_to(value, step),
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
        request.client_order_id = format!("th{t}");
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
            vike_exec::clamp_leverage(requested, max),
            &row["result"],
            &format!("clamp({requested}, {max:?})"),
        );
    }
}

// ------------------------------------------------------------------------------------------
// ClientOrderIdGenerator
// ------------------------------------------------------------------------------------------

#[test]
fn coid_minter_and_charset() {
    let fx = fixture("coid.json");
    let mut minter = vike_exec::ClientOrderIdGenerator::new(Some(fx["session"].as_str().unwrap()));
    for want in fx["ids"].as_array().unwrap() {
        assert_eq!(minter.generate(), want.as_str().unwrap());
    }
    for row in fx["valid"].as_array().unwrap() {
        let coid = row["coid"].as_str().unwrap();
        assert_eq!(
            vike_exec::is_valid_crypto_coid(coid),
            row["ok"].as_bool().unwrap(),
            "charset({coid:?})"
        );
    }
}

// ------------------------------------------------------------------------------------------
// EventBus defer-and-deliver (FIFO, not DFS)
// ------------------------------------------------------------------------------------------

struct Chaining;

impl EventHandler for Chaining {
    fn on_event(&mut self, event: &Event, outbox: &mut Outbox) -> vike_exec::Fold {
        // e1 -> publish e2 AND e3; e2 -> publish e4 (mirror of the Python fixture handler)
        if let Event::OrderSubmitted(e) = event {
            let pub_sub = |coid: &str| {
                Event::OrderSubmitted(OrderSubmitted { client_order_id: coid.to_string(), ts: 0 })
            };
            match e.client_order_id.as_str() {
                "e1" => {
                    outbox.publish(pub_sub("e2"));
                    outbox.publish(pub_sub("e3"));
                }
                "e2" => outbox.publish(pub_sub("e4")),
                _ => {}
            }
        }
        vike_exec::Fold::Applied
    }
}

#[test]
fn bus_defer_and_deliver_fifo() {
    let fx = fixture("bus.json");
    let mut bus = EventBus::new();
    let mut handler = Chaining;
    bus.publish(
        Event::OrderSubmitted(OrderSubmitted { client_order_id: "e1".into(), ts: 0 }),
        &mut handler,
    );
    let got: Vec<&str> = bus
        .delivered
        .iter()
        .map(|ev| match ev {
            Event::OrderSubmitted(e) => e.client_order_id.as_str(),
            _ => unreachable!(),
        })
        .collect();
    let want: Vec<&str> =
        fx["delivered"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(got, want, "FIFO defer-and-deliver order (DFS would give e1,e2,e4,e3)");
}

// ------------------------------------------------------------------------------------------
// ExecutionEngine scenarios
// ------------------------------------------------------------------------------------------

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
    fn cancel(&mut self, client_order_id: &str) {
        match self {
            TestClient::Rec(c) => c.cancel(client_order_id),
            TestClient::Sim(c) => c.cancel(client_order_id),
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
            let mut mo: ManagedOrder =
                ManagedOrder::new(serde_json::from_value(o["request"].clone()).unwrap());
            mo.status = OrderStatus::parse(o["status"].as_str().unwrap()).unwrap();
            mo.venue_order_id = o["venue_order_id"].as_str().map(str::to_string);
            mo.filled_qty = o["filled_qty"].as_f64().unwrap();
            mo.avg_fill_px = o["avg_fill_px"].as_f64().unwrap();
            mo
        })
        .collect();
    ReconcileSnapshot {
        positions: pairs("positions"),
        open_orders,
        position_avg_px: pairs("position_avg_px"),
        position_mark_px: pairs("position_mark_px"),
        position_sides: v["position_sides"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_str().unwrap().to_string(), p[1].as_str().unwrap().to_string()))
            .collect(),
        balance: v["balance"].as_f64().unwrap(),
        // R5 fixtures predate the margin-mode carrier (step-2) — no margin info reported, so
        // apply_snapshot carries priors forward (default-flat ⇒ Cross/None, byte-identical).
        position_margin: Vec::new(),
    }
}

/// Pump the sim client's synthesized venue events through the bus (the Python `_SimClient`
/// publishes synchronously inside submit; the Rust stub queues and the runtime pumps).
fn pump(bus: &mut EventBus, engine: &mut ExecutionEngine<TestClient>) {
    while let Some(ev) = engine.client.pop_pending() {
        bus.publish(ev, engine);
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
        let mut engine =
            ExecutionEngine::new(account, RiskGate::new(limits), client, venue, symbol);
        let mut bus = EventBus::new();

        for act in sc["actions"].as_array().unwrap() {
            match act["do"].as_str().unwrap() {
                "submit" => {
                    let request: OrderRequest =
                        serde_json::from_value(act["request"].clone()).unwrap();
                    let now_ms = act["now_ms"].as_i64().unwrap();
                    let mut outbox = Outbox::default();
                    engine.submit_order(&request, now_ms, &mut outbox);
                    while let Some(ev) = outbox.0.pop_front() {
                        bus.publish(ev, &mut engine);
                    }
                    pump(&mut bus, &mut engine);
                }
                "cancel" => {
                    engine.cancel_order(act["coid"].as_str().unwrap());
                    pump(&mut bus, &mut engine);
                }
                "event" => {
                    let ev: Event = serde_json::from_value(act["event"].clone()).unwrap();
                    bus.publish(ev, &mut engine);
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
        let acc = &engine.account;
        assert_bits(acc.balance, &e["balance"], &format!("{name}: balance"));
        assert_bits(acc.realized_pnl, &e["realized_pnl"], &format!("{name}: realized"));
        assert_bits(acc.fees_paid, &e["fees_paid"], &format!("{name}: fees"));
        assert_bits(acc.funding_paid, &e["funding_paid"], &format!("{name}: funding"));
        let mode = match acc.balance_mode {
            BalanceMode::Delta => "delta",
            BalanceMode::Authoritative => "authoritative",
        };
        assert_eq!(mode, e["balance_mode"].as_str().unwrap(), "{name}: mode");
        assert_bits(
            acc.equity_all(sc["seed_cash"].as_f64().unwrap()),
            &e["equity_all"],
            &format!("{name}: equity_all"),
        );

        let want_pos = e["positions"].as_array().unwrap();
        assert_eq!(acc.positions.len(), want_pos.len(), "{name}: position count");
        for (((v, s, ps), p), w) in acc.positions.iter().zip(want_pos) {
            assert_eq!(v, w[0].as_str().unwrap(), "{name}: pos venue");
            assert_eq!(s, w[1].as_str().unwrap(), "{name}: pos symbol");
            assert_eq!(ps.to_string(), w[2].as_str().unwrap(), "{name}: pos side");
            assert_bits(p.size, &w[3], &format!("{name}: pos size"));
            assert_bits(p.avg_px, &w[4], &format!("{name}: pos avg"));
        }
        let want_marks = e["marks"].as_array().unwrap();
        assert_eq!(acc.marks_iter().count(), want_marks.len(), "{name}: mark count");
        for (((v, s), px), w) in acc.marks_iter().zip(want_marks) {
            assert_eq!(v, w[0].as_str().unwrap(), "{name}: mark venue");
            assert_eq!(s, w[1].as_str().unwrap(), "{name}: mark symbol");
            assert_bits(*px, &w[2], &format!("{name}: mark px"));
        }

        let want_reg = e["registry"].as_array().unwrap();
        assert_eq!(engine.registry.len(), want_reg.len(), "{name}: registry count");
        for ((coid, mo), w) in engine.registry.iter().zip(want_reg) {
            assert_eq!(coid, w[0].as_str().unwrap(), "{name}: registry order");
            assert_eq!(mo.status.as_str(), w[1].as_str().unwrap(), "{name}: {coid} status");
            assert_bits(mo.filled_qty, &w[2], &format!("{name}: {coid} filled_qty"));
            assert_bits(mo.avg_fill_px, &w[3], &format!("{name}: {coid} avg_px"));
            assert_eq!(
                mo.venue_order_id.as_deref(),
                w[4].as_str(),
                "{name}: {coid} venue_order_id"
            );
        }

        // outbound events: denials + full delivery order
        let denied: Vec<(String, String)> = bus
            .delivered
            .iter()
            .filter_map(|ev| match ev {
                Event::OrderDenied(d) => Some((d.client_order_id.clone(), d.reason.to_string())),
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
        let delivered: Vec<&str> = bus.delivered.iter().map(event_name).collect();
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
            assert_eq!(r.client_order_id, w[0].as_str().unwrap(), "{name}: sub coid");
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
            engine.position_size("BOTH"),
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
