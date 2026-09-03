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
//! coid terminalizes once) — captured frames in, venue-matching positions out.

use serde_json::Value;

use vike_deribit::event_mapper::map_deribit_private;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, ManagedOrder, OrderStatus, Outbox,
    RiskGate, RiskLimits,
};
use vike_model::events::Event;
use vike_model::{build_combo, ComboLeg, ComboSpec, TimeInForce};

const COID: &str = "gate4-fill-1784494768234";
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
const MOUNTED: &str = "BTC-PERPETUAL";

/// Mapper layer: every captured row dual-publishes verbatim — bare `Fill` (symbol from the row's
/// own `instrument_name`, coid from `label`, SIGNED fee) + a state-driven `OrderFilled` wrap.
#[test]
fn captured_combo_frames_map_row_verbatim() {
    let mut all = Vec::new();
    for frame in captured_frames() {
        all.extend(map_deribit_private(&frame, "deribit", MOUNTED));
    }
    assert_eq!(all.len(), 6, "3 rows × dual-publish");
    let expect = [
        ("BTC-20JUL26", -1, 10.0, 64451.5, "258544119", 8.0e-8),
        ("BTC-25DEC26", 1, 10.0, 65650.5, "258544120", 0.0),
        (COMBO_ID, 1, 10.0, 1199.0, "258544118", 0.0),
    ];
    for (i, (sym, side, qty, px, tid, fee)) in expect.iter().enumerate() {
        let Event::Fill(f) = &all[i * 2] else { panic!("even slot {i} must be the bare Fill") };
        assert_eq!(f.symbol.as_str(), *sym, "row {i} symbol from the row's own instrument_name");
        assert_eq!(f.client_order_id, COID, "row {i} coid from label");
        assert_eq!((f.side, f.last_qty, f.last_px), (*side, *qty, *px), "row {i} economics");
        assert_eq!(f.trade_id.as_str(), *tid, "row {i} venue trade id");
        assert_eq!(f.commission, *fee, "row {i} SIGNED fee carried unchanged");
        let Event::OrderFilled(w) = &all[i * 2 + 1] else {
            panic!("odd slot {i} must be the state=filled wrap")
        };
        assert_eq!(w.client_order_id, COID, "row {i} wrap routes by the combo coid");
        assert_eq!(w.fill.trade_id.as_str(), *tid);
    }
}

/// Engine layer: fold the captured frames through a production-shaped engine (mounted on
/// `BTC-PERPETUAL`, the combo registered under its coid, NO `extra_symbols` — the pure gate-4
/// route). The Account must end exactly where the venue's own `get_positions` ended: the two
/// legs, at leg prices, and NO position under the combo instrument.
#[test]
fn captured_combo_fill_folds_to_venue_matching_positions() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "deribit", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "deribit",
        MOUNTED,
    );
    // the probe's spec: BUY the future spread = +1 far / -1 near (build_combo leaves symbol EMPTY)
    let spec = ComboSpec {
        venue: "deribit".into(),
        side: 1,
        qty: 10.0,
        legs: vec![
            ComboLeg { symbol: "BTC-25DEC26".into(), ratio: 1 },
            ComboLeg { symbol: "BTC-20JUL26".into(), ratio: -1 },
        ],
        net_limit: Some(1201.5),
        time_in_force: TimeInForce::Gtc,
    };
    let req = build_combo(&spec, COID).expect("valid combo spec");
    engine.registry.insert(COID.to_string(), ManagedOrder::new(req));

    // the production lifecycle ahead of any fill: Submitted (local emitter) then Accepted (venue
    // ack — what the #512 lifecycle smoke observed before its cancel)
    let mut outbox = Outbox::default();
    engine.on_event(
        &Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            client_order_id: COID.into(),
            ts: 0,
        }),
        &mut outbox,
    );
    engine.on_event(
        &Event::OrderAccepted(vike_model::events::OrderAccepted {
            client_order_id: COID.into(),
            venue_order_id: Some("108338717925".into()),
            ts: 0,
        }),
        &mut outbox,
    );
    for frame in captured_frames() {
        for ev in map_deribit_private(&frame, "deribit", MOUNTED) {
            engine.on_event(&ev, &mut outbox);
        }
    }

    // venue truth after the live fill: [("BTC-20JUL26", -10.0), ("BTC-25DEC26", 10.0)] — legs only
    let key = |s: &str| -> vike_exec::PositionKey { ("deribit".into(), s.into(), "BOTH".into()) };
    let near = engine.account.positions.get(&key("BTC-20JUL26")).expect("near leg position");
    assert_eq!((near.size, near.avg_px), (-10.0, 64451.5), "short near leg at its leg price");
    let far = engine.account.positions.get(&key("BTC-25DEC26")).expect("far leg position");
    assert_eq!((far.size, far.avg_px), (10.0, 65650.5), "long far leg at its leg price");
    assert!(
        !engine.account.positions.contains_key(&key(COMBO_ID)),
        "NO phantom position under the combo instrument (the net print must not fold)"
    );
    assert_eq!(engine.account.positions.len(), 2, "exactly the venue's two legs");
    // only LEG fees fold (the skipped net print's fee never reaches the Account)
    assert_eq!(engine.account.fees_paid, 8.0e-8, "leg fees only");

    // ONE order downstream, exactly one terminal. Display caveat, pinned deliberately: Deribit
    // stamps EVERY row state=filled, so the FIRST captured row's wrap (leg 1) terminalizes the
    // FSM and the later wraps drop as replays — filled_qty/avg_fill_px reflect that first LEG row
    // (10.0 @ 64451.5), not the paper twin's Σ-legs aggregate. If the venue's emission order ever
    // changes, these two values change WITH it — that is display-only; the Account above is the
    // position truth either way.
    let mo = &engine.registry[COID];
    assert_eq!(mo.status, OrderStatus::Filled, "one coid, one terminal");
    assert_eq!((mo.filled_qty, mo.avg_fill_px), (10.0, 64451.5), "first-wrap display aggregate");
}
