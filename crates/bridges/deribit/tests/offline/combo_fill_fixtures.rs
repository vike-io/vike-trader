//! Combo-fill FIXTURES from the live capture (combo gate 4 — the fill-drop hazard).
//!
//! The three frames below are the VERBATIM `user.trades.any.any.raw` frames Deribit testnet
//! emitted for ONE combo execution on 2026-07-19 (`deribit_combo_fill_probe`: buy 10 USD of
//! `BTC-FS-25DEC26_20JUL26` at net 1199.0, label `gate4-fill-1784494768234`) — the venue's real
//! answer to the design doc's open question 1. The shape they pin:
//!
//!   - N LEG rows: leg `instrument_name`, REAL leg price/fee, `combo_id` + `combo_trade_id` set,
//!     the combo order's `label`, per-row `state` — the per-leg position truth.
//!   - ONE aggregate row under the COMBO instrument itself: the NET price, `trade_id` equal to
//!     the legs' `combo_trade_id`, NO `combo_id` field — a print, not a position (the venue's
//!     `get_positions` after the fill held the two legs ONLY).
//!
//! Two layers under test: the pure mapper (dual-publish per row, verbatim) and the engine fold
//! (leg rows fold into `Account`, the net print is classified out by `owns_fill_symbol`, the one
//! order client id terminalizes once) — captured frames in, venue-matching positions out.

use serde_json::Value;

use vike_deribit::execution::event_mapper::map_deribit_private;
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, EventBusOutbox, EventHandler, RiskGate};
use vike_model::RiskLimits;
use vike_model::events::Event;
use vike_model::{OrderCombo, OrderComboLeg, TimeInForce, build_combo};
use vike_model::{OrderRecord, OrderStatus};

const ORDER_CLIENT_ID: &str = "gate4-fill-1784494768234";
const COMBO_ID: &str = "BTC-FS-25DEC26_20JUL26";

/// The three captured frames, in venue emission order (leg, leg, then the combo net print).
fn captured_frames() -> Vec<Value> {
    [
        // leg 1: SELL the near future at its own price; taker fee charged; combo linkage fields set
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.trades.any.any.raw","data":[{"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":64451.5,"user_id":84789,"trade_id":"258544119","direction":"sell","instrument_name":"BTC-20JUL26","amount":10.0,"order_id":"108338717926","index_price":64466.04,"trade_seq":7396,"api":false,"mark_price":64470.33,"matching_id":null,"tick_direction":2,"profit_loss":0.0,"combo_id":"BTC-FS-25DEC26_20JUL26","mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"combo_trade_id":"258544118","order_type":"limit","fee":8.0e-8,"fee_currency":"BTC","liquidity":"T","risk_reducing":false}]}}"#,
        // leg 2: BUY the far future at its own price
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.trades.any.any.raw","data":[{"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":65650.5,"user_id":84789,"trade_id":"258544120","direction":"buy","instrument_name":"BTC-25DEC26","amount":10.0,"order_id":"108338717928","index_price":64466.04,"trade_seq":2627757,"api":false,"mark_price":65634.83,"matching_id":null,"tick_direction":0,"profit_loss":0.0,"combo_id":"BTC-FS-25DEC26_20JUL26","mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"combo_trade_id":"258544118","order_type":"limit","fee":0.0,"fee_currency":"BTC","liquidity":"T","risk_reducing":false}]}}"#,
        // the combo-instrument aggregate: NET price 1199.0, trade_id == the legs' combo_trade_id,
        // NO combo_id field of its own
        r#"{"jsonrpc":"2.0","method":"subscription","params":{"channel":"user.trades.any.any.raw","data":[{"label":"gate4-fill-1784494768234","timestamp":1784494768235,"state":"filled","price":1199.0,"user_id":84789,"trade_id":"258544118","direction":"buy","instrument_name":"BTC-FS-25DEC26_20JUL26","amount":10.0,"order_id":"108338717925","index_price":64466.04,"trade_seq":1,"api":true,"mark_price":1164.5,"matching_id":null,"tick_direction":1,"profit_loss":null,"mmp":false,"post_only":false,"reduce_only":false,"self_trade":false,"contracts":1.0,"order_type":"limit","fee":0.0,"fee_currency":"BTC","liquidity":"T","risk_reducing":false,"starbase_match_id":204703712348741632,"starbase_timestamp":1784494768235877496}]}}"#,
    ]
    .iter()
    .map(|s| serde_json::from_str(s).expect("captured frame parses"))
    .collect()
}

/// The mounted-symbol default the production deribit exec client uses; none of the combo's
/// symbols match it, which is the whole point of the routing under test.
const ENGINE_SYMBOL: &str = "BTC-PERPETUAL";

/// Mapper layer: every captured row dual-publishes verbatim — bare `Fill` (symbol from the row's
/// own `instrument_name`, order client id from `label`, SIGNED fee) + a state-driven `OrderFilled` wrap.
#[test]
fn captured_combo_frames_map_row_verbatim() {
    let mut all = Vec::new();
    for frame in captured_frames() {
        all.extend(map_deribit_private(&frame, "deribit", ENGINE_SYMBOL));
    }
    assert_eq!(all.len(), 6, "3 rows × dual-publish");
    let expect = [
        ("BTC-20JUL26", -1, 10.0, 64451.5, "258544119", 8.0e-8),
        ("BTC-25DEC26", 1, 10.0, 65650.5, "258544120", 0.0),
        (COMBO_ID, 1, 10.0, 1199.0, "258544118", 0.0),
    ];
    for (i, (symbol, side, qty, px, tid, fee)) in expect.iter().enumerate() {
        let Event::Fill(f) = &all[i * 2] else { panic!("even slot {i} must be the bare Fill") };
        assert_eq!(f.symbol.as_str(), *symbol, "row {i} symbol from the row's own instrument_name");
        assert_eq!(f.order_client_id, ORDER_CLIENT_ID, "row {i} order client id from label");
        assert_eq!((f.side, f.last_qty, f.last_price), (*side, *qty, *px), "row {i} economics");
        assert_eq!(f.trade_id.as_str(), *tid, "row {i} venue trade id");
        assert_eq!(f.commission, *fee, "row {i} SIGNED fee carried unchanged");
        let Event::OrderFilled(w) = &all[i * 2 + 1] else {
            panic!("odd slot {i} must be the state=filled wrap")
        };
        assert_eq!(
            w.order_client_id, ORDER_CLIENT_ID,
            "row {i} wrap routes by the combo order client id"
        );
        assert_eq!(w.fill.trade_id.as_str(), *tid);
    }
}

/// Engine layer: fold the captured frames through a production-shaped engine (started on
/// `BTC-PERPETUAL`, the combo registered under its order client id, NO `extra_symbols` — the pure gate-4
/// route). The Account must end exactly where the venue's own `get_positions` ended: the two
/// legs, at leg prices, and NO position under the combo instrument.
#[test]
fn captured_combo_fill_folds_to_venue_matching_positions() {
    let mut engine = Engine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        ENGINE_SYMBOL,
    );
    // the probe's spec: BUY the future spread = +1 far / -1 near (build_combo leaves symbol EMPTY)
    let spec = OrderCombo {
        venue: "deribit".into(),
        side: 1,
        qty: 10.0,
        legs: vec![
            OrderComboLeg { symbol: "BTC-25DEC26".into(), ratio: 1, limit: None },
            OrderComboLeg { symbol: "BTC-20JUL26".into(), ratio: -1, limit: None },
        ],
        net_limit: Some(1201.5),
        time_in_force: TimeInForce::Gtc,
        account: None,
        order_client_id: None,
    };
    let req = build_combo(&spec, ORDER_CLIENT_ID).expect("valid combo spec");
    engine.registry.insert(ORDER_CLIENT_ID.to_string(), OrderRecord::new(req));

    // the production lifecycle ahead of any fill: Submitted (local emitter) then Accepted (venue
    // ack — what the #512 lifecycle smoke observed before its cancel)
    let mut outbox = EventBusOutbox::default();
    engine.on_event(
        &Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: ORDER_CLIENT_ID.into(),
            timestamp: 0,
        }),
        &mut outbox,
    );
    engine.on_event(
        &Event::OrderAccepted(vike_model::events::OrderAccepted {
            order_client_id: ORDER_CLIENT_ID.into(),
            venue_order_id: Some("108338717925".into()),
            timestamp: 0,
        }),
        &mut outbox,
    );
    for frame in captured_frames() {
        for event in map_deribit_private(&frame, "deribit", ENGINE_SYMBOL) {
            engine.on_event(&event, &mut outbox);
        }
    }

    // venue truth after the live fill: [("BTC-20JUL26", -10.0), ("BTC-25DEC26", 10.0)] — legs only
    let key = |s: &str| -> vike_execution::PositionKey {
        ("deribit".into(), s.into(), vike_model::events::PositionSide::Both)
    };
    let near = engine.account.positions.get(&key("BTC-20JUL26")).expect("near leg position");
    assert_eq!(
        (near.size, near.average_price),
        (-10.0, 64451.5),
        "short near leg at its leg price"
    );
    let far = engine.account.positions.get(&key("BTC-25DEC26")).expect("far leg position");
    assert_eq!((far.size, far.average_price), (10.0, 65650.5), "long far leg at its leg price");
    assert!(
        !engine.account.positions.contains_key(&key(COMBO_ID)),
        "NO phantom position under the combo instrument (the net print must not fold)"
    );
    assert_eq!(engine.account.positions.len(), 2, "exactly the venue's two legs");
    // only LEG fees fold (the skipped net print's fee never reaches the Account)
    assert_eq!(engine.account.fees_paid, 8.0e-8, "leg fees only");

    // ONE order downstream, exactly one terminal, in COMBO units (`OrderRecord::apply`'s combo fold).
    // Every row says state=filled, but a LEG row's state is its own child order's (its `order_id`
    // is not the combo's 108338717925), so the first row (BTC-20JUL26) does NOT close the combo.
    // The second leg row completes both legs (10 of 10) and closes it; the combo-instrument row then
    // finds it closed and drops. avg_fill_px is the net per unit, 65650.5 − 64451.5 = 1199.0: the
    // venue's own net print.
    let mo = &engine.registry[ORDER_CLIENT_ID];
    assert_eq!(mo.status, OrderStatus::Filled, "one order client id, one terminal");
    assert_eq!(
        (mo.filled_qty, mo.average_fill_price),
        (10.0, 1199.0),
        "combo units, the venue's net"
    );
}
