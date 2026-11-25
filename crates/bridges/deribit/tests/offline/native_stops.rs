//! Deribit's NATIVE trigger orders (stop, stop-limit, take-profit) over the local WebSocket
//! stand-in in `crate::fake_deribit_websocket`. No venue, no credentials, no `#[ignore]`.
//!
//! Before they were wired, `VenueRestDeribit::submit_order` knew two wire types and sent everything that
//! was not a limit as a MARKET order, so a "stop" would have FILLED IMMEDIATELY instead of resting
//! as protection. These pin each way a stop could silently land wrong:
//!
//! * the wire `type` of each trigger kind, with `trigger_price` on the instrument's tick grid, the
//!   `trigger` series from the order's `trigger_by` (`last_price` when it names none) and
//!   `reduce_only` carried;
//! * a trigger order that cannot be sent (no trigger price, a stop-limit with no limit price, an
//!   option), and a kind the bridge builds no order for, is refused BY NAME with nothing written to
//!   the socket;
//! * a resting stop is cancelled by its venue id, and found again by its label when this client did
//!   not place it;
//! * the venue's own words for a held stop (`untriggered`, `stop_market`) read as a resting `Stop`
//!   carrying our order client id in the restart reconcile's open-order reader, and the fill of a stop that
//!   TRIGGERED (a new order, with a new venue id) still names the stop's order client id by its `label`.
//!
//! The venue rows below are the testnet's own (BTC-PERPETUAL, 2026-10-26, `user_id` and the matching
//! engine's internal ids dropped); `tests/deribit_smokes/deribit_native_stop_testnet_smoke.rs` is what read them.
//!
//! The TRAILING stop (wave 3) is pinned at the end the same way: a `trailing` is a `trailing_stop` whose
//! `trigger_offset` is the trail on the tick grid, an activation or a `trailing_limit` is refused by name,
//! and its rows read back as a `Trailing`. Its resting row is the testnet's own (BTC-PERPETUAL, 2026-11-08,
//! `user_id` dropped), captured by `tests/deribit_smokes/deribit_trailing_stop_testnet_smoke.rs`.

use std::assert_matches;

use serde_json::{Value, json};

use vike_bridge_core::rest::VenueRest;
use vike_deribit::execution::event_mapper::map_deribit_private;
use vike_deribit::execution::rest::VenueRestDeribit;
use vike_deribit::execution::resync::map_deribit_history;
use vike_deribit::reconcile::{parse_open_orders, parse_user_trades};
use vike_model::events::Event;
use vike_model::{AssetClass, OrderRequest, OrderStatus, OrderType, SymbolProperties, TriggerBy};

use crate::fake_deribit_websocket::{
    CHILD_ID, FakeDeribit, ORDER_ID, SYMBOL, Session, outcome, transport_against,
};

const ORDER_CLIENT_ID: &str = "vike-stop-1";

/// The instrument fixtures captured from the venue (`tests/fixtures/deribit_get_instruments.json`).
fn fixtures() -> Value {
    serde_json::from_str(include_str!("../fixtures/deribit_get_instruments.json"))
        .expect("the fixture is valid JSON")
}

/// `BTC-PERPETUAL`'s real grid and class: tick 0.5, lot 10, a perpetual.
fn perp_grid() -> SymbolProperties {
    let all = fixtures();
    let row = all["future"]["result"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["instrument_name"] == SYMBOL))
        .expect("the captured BTC-PERPETUAL row");
    vike_deribit::catalog::properties_of(row).expect("a usable grid")
}

/// A captured BTC option row: its name and grid, class `Option`.
fn option_row() -> (String, SymbolProperties) {
    let all = fixtures();
    let row = &all["option_btc"]["result"][0];
    let properties = vike_deribit::catalog::properties_of(row).expect("a usable option grid");
    (row["instrument_name"].as_str().expect("a name").to_string(), properties)
}

fn rest_on(fake: &FakeDeribit, symbol: &str, properties: SymbolProperties) -> VenueRestDeribit {
    VenueRestDeribit::new(transport_against(fake), symbol, properties, "BTC")
}

/// A reduce-only sell stop of one lot on the perpetual, trigger OFF the 0.5 grid so the snap shows
/// (`wire_price_on` floors to the step, as it does for a limit price: 59000.3 goes out as 59000.0).
fn stop() -> OrderRequest {
    crate::sell_stop(ORDER_CLIENT_ID, 59_000.3, 1_700_000_000_000)
}

/// The one frame the venue received for `method`.
fn sent(fake: &FakeDeribit, method: &str) -> Value {
    let mut all = fake.params_of(method);
    assert_eq!(all.len(), 1, "exactly one {method} reached the venue");
    all.remove(0)
}

/// THE WIRE SHAPE. A stop is a `stop_market` with its trigger on the grid, the default `last_price`
/// series and `reduce_only`; it is a SELL, so `private/sell`; and the venue's `untriggered` answer
/// is an ordinary accept carrying the venue order id.
#[test]
fn a_stop_goes_out_as_a_stop_market_that_rests() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());

    match outcome(rest.submit_order(&stop())) {
        Event::OrderAccepted(a) => {
            assert_eq!(a.order_client_id, ORDER_CLIENT_ID);
            assert_eq!(a.venue_order_id.as_ref().map(|v| v.as_str()), Some(ORDER_ID));
        }
        other => panic!("a held stop is accepted, got {other:?}"),
    }

    assert_eq!(fake.count("private/buy"), 0);
    assert_eq!(
        sent(&fake, "private/sell"),
        json!({
            "instrument_name": SYMBOL,
            "amount": 10.0,
            "type": "stop_market",
            "label": ORDER_CLIENT_ID,
            "post_only": false,
            "trigger_price": 59_000.0,
            "trigger": "last_price",
            "reduce_only": true,
        })
    );
}

/// A buy stop (the exit of a short) goes to `private/buy`, and the order's `trigger_by` picks the
/// series: Mark -> `mark_price`, Index -> `index_price`.
#[test]
fn the_trigger_series_follows_the_order_and_a_buy_stop_buys() {
    for (by, wire) in [
        (TriggerBy::Last, "last_price"),
        (TriggerBy::Mark, "mark_price"),
        (TriggerBy::Index, "index_price"),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, SYMBOL, perp_grid());
        let mut req = stop();
        req.side = 1;
        req.reduce_only = false;
        req.trigger_by = Some(by);
        assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
        assert_eq!(fake.count("private/sell"), 0);
        let params = sent(&fake, "private/buy");
        assert_eq!(params["trigger"], wire, "{by:?}");
        assert!(params.get("reduce_only").is_none(), "not reduce-only unless asked");
    }
}

/// A stop-limit carries BOTH prices on the grid: the trigger and the limit it becomes.
#[test]
fn a_stop_limit_carries_its_trigger_and_its_limit() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = stop();
    req.order_type = OrderType::StopLimit;
    req.price = Some(58_900.2);

    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
    // The whole frame, so the native stop-limit path cannot move (no TIF is sent for a trigger order).
    assert_eq!(
        sent(&fake, "private/sell"),
        json!({
            "instrument_name": SYMBOL,
            "amount": 10.0,
            "type": "stop_limit",
            "label": ORDER_CLIENT_ID,
            "post_only": false,
            "trigger_price": 59_000.0,
            "trigger": "last_price",
            "price": 58_900.0,
            "reduce_only": true,
        })
    );
}

/// A take-profit is `take_market`, or `take_limit` when it names a limit price (hyperliquid's
/// reading of a trigger kind). The whole frame of each is pinned.
#[test]
fn a_take_profit_is_take_market_or_take_limit() {
    for (price, wire) in [(None, "take_market"), (Some(61_000.0), "take_limit")] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, SYMBOL, perp_grid());
        let mut req = stop();
        req.order_type = OrderType::TakeProfit;
        req.trigger_price = Some(61_000.0);
        req.price = price;
        assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
        let mut want = json!({
            "instrument_name": SYMBOL,
            "amount": 10.0,
            "type": wire,
            "label": ORDER_CLIENT_ID,
            "post_only": false,
            "trigger_price": 61_000.0,
            "trigger": "last_price",
            "reduce_only": true,
        });
        if price.is_some() {
            want["price"] = json!(61_000.0);
        }
        assert_eq!(sent(&fake, "private/sell"), want, "{wire}");
    }
}

/// A kind this bridge builds no order for (an unrecognised word; a trigger kind as a combo; a limit combo
/// with no net price) is a terminal reject that NAMES the kind, with nothing on the socket: none used to be
/// refused, and each went out as a MARKET order. A combo is refused before `private/create_combo` registers
/// anything.
#[test]
fn a_kind_the_bridge_does_not_build_is_refused_by_name_and_never_sent_as_market() {
    use vike_model::OrderRefusalId::VenueCapability;
    let legs = vec![vike_model::OrderComboLeg { symbol: SYMBOL.into(), ratio: 1, limit: None }];
    let mut unknown = stop();
    unknown.order_type = OrderType::Unrecognized("market_if_touched".to_string());
    let mut combo_stop = stop();
    combo_stop.combo_legs = legs.clone();
    let mut combo_unpriced_limit = stop();
    combo_unpriced_limit.order_type = OrderType::Limit;
    combo_unpriced_limit.price = None;
    combo_unpriced_limit.combo_legs = legs;
    for (req, why) in [
        (unknown, "deribit builds no `market_if_touched` order"),
        (combo_stop, "deribit builds no `stop` combo order"),
        (combo_unpriced_limit, "a `limit` combo order names no net price"),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, SYMBOL, perp_grid());
        match outcome(rest.submit_order(&req)) {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, ORDER_CLIENT_ID);
                assert!(r.reason.contains(why), "wanted `{why}`, got `{}`", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(VenueCapability.word()), "{why}");
            }
            other => panic!("`{why}` must be a terminal reject, got {other:?}"),
        }
        assert!(fake.methods().is_empty(), "{why}: nothing may reach the socket");
    }
}

/// What the bridge refuses before any frame is written: a terminal reject naming the cause, and
/// NOTHING on the socket.
#[test]
fn an_unsendable_trigger_order_is_refused_by_name_with_nothing_on_the_wire() {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    let (option, option_grid) = option_row();
    assert_eq!(option_grid.asset_class, Some(AssetClass::Option), "the fixture is an option");

    let mut no_trigger = stop();
    no_trigger.trigger_price = None;
    let mut zero_trigger = stop();
    zero_trigger.trigger_price = Some(0.0);
    let mut no_limit = stop();
    no_limit.order_type = OrderType::StopLimit;
    let mut on_option = stop();
    on_option.symbol = option.clone();

    for (req, symbol, grid, why, family) in [
        (no_trigger, SYMBOL.to_string(), perp_grid(), "needs a positive trigger price", NotSent),
        (zero_trigger, SYMBOL.to_string(), perp_grid(), "needs a positive trigger price", NotSent),
        (
            no_limit,
            SYMBOL.to_string(),
            perp_grid(),
            "stop_limit needs a positive limit price",
            NotSent,
        ),
        (on_option, option, option_grid, "no stop order on an option", VenueCapability),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, &symbol, grid);
        match outcome(rest.submit_order(&req)) {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, ORDER_CLIENT_ID);
                assert!(r.reason.contains(why), "wanted `{why}`, got `{}`", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(family.word()), "{why}");
            }
            other => panic!("`{why}` must be a terminal reject, got {other:?}"),
        }
        assert!(fake.methods().is_empty(), "{why}: nothing may reach the socket");
    }
}

/// A plain limit order is untouched by the trigger work: no trigger keys, the limit type.
#[test]
fn a_limit_order_carries_no_trigger_keys() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = stop();
    req.order_type = OrderType::Limit;
    req.price = Some(50_000.0);
    req.trigger_price = None;
    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
    let params = sent(&fake, "private/sell");
    assert_eq!(params["type"], "limit");
    assert!(params.get("trigger").is_none() && params.get("trigger_price").is_none());
}

/// Cancelling the held stop: by the venue id this client recorded at the accept (no lookup), and,
/// from a client that never saw it (a restart), by its label first.
#[test]
fn a_held_stop_is_cancelled_by_id_and_found_again_by_label() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(outcome(rest.submit_order(&stop())), Event::OrderAccepted(_));
    rest.cancel_order(ORDER_CLIENT_ID).expect("cancel");
    assert_eq!(sent(&fake, "private/cancel"), json!({ "order_id": ORDER_ID }));
    assert_eq!(fake.count("private/get_order_state_by_label"), 0, "a map hit asks nothing");

    let after_restart = FakeDeribit::spawn(vec![Session::OkForever]);
    let adopted = rest_on(&after_restart, SYMBOL, perp_grid());
    adopted.cancel_order(ORDER_CLIENT_ID).expect("cancel after a restart");
    assert_eq!(after_restart.count("private/get_order_state_by_label"), 1, "the label lookup");
    assert_eq!(sent(&after_restart, "private/cancel"), json!({ "order_id": ORDER_ID }));
}

/// WHEN A STOP-LIMIT HAS FIRED its own id is gone (`private/cancel` answers 10004) but the limit it
/// became RESTS under the same label with a new id. Swallowing the stale id's "not found" reported the
/// cancel done (`OrderCanceled`) for an order the venue still held: the cancel must follow the label to
/// the order that exists now, and cancel that.
#[test]
fn a_cancel_of_a_stop_that_fired_into_a_resting_limit_cancels_the_limit() {
    let fake = FakeDeribit::spawn(vec![Session::TriggeredStop]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = stop();
    req.order_type = OrderType::StopLimit;
    req.price = Some(58_900.0);
    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));

    rest.cancel_order(ORDER_CLIENT_ID).expect("the cancel succeeds");

    let cancelled: Vec<Value> =
        fake.params_of("private/cancel").iter().map(|p| p["order_id"].clone()).collect();
    assert_eq!(
        cancelled,
        vec![json!(ORDER_ID), json!(CHILD_ID)],
        "the stop's id is gone, so the order now held under its label is cancelled too"
    );
    assert_eq!(fake.count("private/get_order_state_by_label"), 1, "one lookup, after the gone id");

    // the new id is remembered: a second cancel goes straight to it
    rest.cancel_order(ORDER_CLIENT_ID).expect("a repeat cancel");
    let cancelled: Vec<Value> =
        fake.params_of("private/cancel").iter().map(|p| p["order_id"].clone()).collect();
    assert_eq!(cancelled.last(), Some(&json!(CHILD_ID)));
    assert_eq!(fake.count("private/get_order_state_by_label"), 1, "no second lookup");
}

/// The control of the test above: when the venue holds nothing NEW under the label (the order is just
/// gone: filled, or cancelled elsewhere) the cancel is the old `Ok(())`, with one lookup and no second
/// cancel.
#[test]
fn a_cancel_of_an_order_that_is_simply_gone_stays_a_no_op() {
    let fake = FakeDeribit::spawn(vec![Session::AlreadyGone]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(outcome(rest.submit_order(&stop())), Event::OrderAccepted(_));

    rest.cancel_order(ORDER_CLIENT_ID).expect("an already-gone order is not an error");

    assert_eq!(fake.count("private/cancel"), 1, "nothing else to cancel");
    assert_eq!(fake.count("private/get_order_state_by_label"), 1);
}

/// A stop's TRIGGER moves natively: `private/edit` takes `trigger_price` (decision 0244, the unified
/// amend). The new trigger is floored to the tick like a limit price, the new amount rides along, the
/// stop stays reduce-only, and a stop_market has no limit `price` to send.
#[test]
fn a_modify_of_a_stop_moves_the_trigger_in_place() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(outcome(rest.submit_order(&stop())), Event::OrderAccepted(_));

    let events = rest.modify_order(&stop(), Some(20.0), Some(58_000.3));

    assert_matches!(events.as_slice(), [Event::OrderModified(_)], "{events:?}");
    let edits = fake.params_of("private/edit");
    assert_eq!(edits.len(), 1, "one edit, no cancel and no new order");
    assert_eq!(edits[0]["order_id"], ORDER_ID);
    assert_eq!(edits[0]["trigger_price"], 58_000.0, "the tick grid floors 58000.3");
    assert_eq!(edits[0]["amount"], 20.0);
    assert_eq!(edits[0]["reduce_only"], true);
    assert!(edits[0].get("price").is_none(), "a stop_market has no limit price: {}", edits[0]);
    assert_eq!(fake.count("private/cancel"), 0);
    assert_eq!(fake.count("private/sell"), 1, "only the original placement");
}

/// A quantity-only modify of a stop re-sends the RESTING trigger: the venue wants it on every edit of
/// a trigger order, and it must not move.
#[test]
fn a_quantity_only_modify_of_a_stop_keeps_its_trigger() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    assert_matches!(outcome(rest.submit_order(&stop())), Event::OrderAccepted(_));

    let events = rest.modify_order(&stop(), Some(20.0), None);

    assert_matches!(events.as_slice(), [Event::OrderModified(_)], "{events:?}");
    let edit = fake.params_of("private/edit")[0].clone();
    assert_eq!(
        edit["trigger_price"], 59_000.0,
        "the resting 59000.3, floored as the submit floored it"
    );
    assert_eq!(edit["amount"], 20.0);
}

/// A take-profit's trigger is not what the modify seam's one price names, so it is still refused in
/// words and nothing is written (cancel and re-submit); so is a zero or negative trigger on a stop.
#[test]
fn a_modify_that_cannot_be_an_edit_is_refused_and_sends_nothing() {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut take = stop();
    take.order_type = OrderType::TakeProfit;
    let events = rest.modify_order(&take, Some(20.0), None);
    assert_eq!(events.len(), 1);
    assert_matches!(events[0], Event::OrderModifyRejected(_));
    let Event::OrderModifyRejected(r) = &events[0] else { unreachable!() };
    assert_eq!(r.refusal_id.as_deref(), Some(VenueCapability.word()));
    let events = rest.modify_order(&stop(), None, Some(-1.0));
    assert_eq!(events.len(), 1);
    assert_matches!(events[0], Event::OrderModifyRejected(_));
    let Event::OrderModifyRejected(r) = &events[0] else { unreachable!() };
    assert_eq!(r.refusal_id.as_deref(), Some(NotSent.word()));
    assert!(fake.methods().is_empty(), "{:?}", fake.methods());
}

/// A held stop as `private/get_open_orders_by_instrument` (`type: "all"`) answered it on the testnet.
fn held_stop_row() -> Value {
    json!({
        "amount": 10.0, "api": true, "contracts": 1.0, "creation_timestamp": 1793016922483i64,
        "direction": "sell", "instrument_name": SYMBOL, "is_liquidation": false,
        "label": "n0stp71", "last_update_timestamp": 1793016922483i64, "mmp": false,
        "order_id": "SLTS-11047216", "order_state": "untriggered", "order_type": "stop_market",
        "post_only": false, "price": "market_price", "reduce_only": true, "replaced": false,
        "risk_reducing": false, "time_in_force": "good_til_cancelled", "trigger": "last_price",
        "trigger_price": 80274.5, "triggered": false, "web": false,
    })
}

/// The `user.trades.any.any.raw` notification of that stop's execution, 20.9 s later: the stop's
/// own id (`SLTS-11047216`) no longer exists, a NEW market order (`124740642635`) executed, and the
/// trade still carries the stop's `label`.
fn triggered_stops_trade_frame() -> Value {
    json!({"jsonrpc": "2.0", "method": "subscription", "params": {
    "channel": "user.trades.any.any.raw",
    "data": [{
        "amount": 10.0, "api": true, "contracts": 1.0, "direction": "sell", "fee": 6e-8,
        "fee_currency": "BTC", "index_price": 80255.32, "instrument_name": SYMBOL,
        "label": "n0stp71", "liquidity": "T", "mark_price": 80282.62, "mmp": false,
        "order_id": "124740642635", "order_type": "market", "post_only": false,
        "price": 80269.0, "profit_loss": -1e-8, "reduce_only": true, "risk_reducing": false,
        "self_trade": false, "state": "filled", "tick_direction": 2,
        "timestamp": 1793016943155i64, "trade_id": "277772122", "trade_seq": 144844892,
    }]}})
}

/// The venue's open-order read lists a held stop as `order_state: "untriggered"`, `order_type:
/// "stop_market"`, `price: "market_price"`. The restart reconcile keeps it as a LIVE `Stop` that
/// carries our order client id, so a stop the node left resting is SEEN after a restart.
#[test]
fn a_held_stop_reads_as_a_resting_stop() {
    let rows = json!([held_stop_row()]);
    let reports = parse_open_orders(&rows.to_string()).expect("a valid body");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].order_client_id.as_deref(), Some("n0stp71"));
    assert_eq!(reports[0].venue_order_id.as_str(), "SLTS-11047216");
    assert_eq!(reports[0].order_type, OrderType::Stop);
    assert_eq!(reports[0].status, OrderStatus::Accepted);
    assert_eq!(reports[0].side, -1);
    // its terms: the level it rests at; "market_price" is no limit
    let r = &reports[0];
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, Some(80274.5), None, None)
    );
}

/// The other trigger kinds' terms, on the held stop's row with the venue's word changed (the DOCUMENTED order
/// object, not captures): a `stop_limit`'s and a `take_limit`'s numeric `price` is their limit, a `take_market`
/// states `market_price` and so no limit; each keeps its `trigger_price`.
#[test]
fn a_held_trigger_rows_terms_are_its_limit_and_its_trigger() {
    let row = |order_type: &str, price: Value| {
        let mut row = held_stop_row();
        row["order_type"] = json!(order_type);
        row["price"] = price;
        row
    };
    let rows = json!([
        row("stop_limit", json!(80250.0)),
        row("take_limit", json!(80250.0)),
        row("take_market", json!("market_price")),
    ]);
    let terms: Vec<_> = parse_open_orders(&rows.to_string())
        .expect("a valid body")
        .into_iter()
        .map(|r| (r.order_type, r.price, r.trigger_price, r.trail, r.trail_percent))
        .collect();
    assert_eq!(
        terms,
        [
            (OrderType::StopLimit, Some(80250.0), Some(80274.5), None, None),
            (OrderType::TakeProfit, Some(80250.0), Some(80274.5), None, None),
            (OrderType::TakeProfit, None, Some(80274.5), None, None),
        ]
    );
}

/// WHEN A STOP TRIGGERS its execution is a new order with a new id, so nothing about the stop's
/// venue id can join the fill: the `label` does, in both the live notification (the pump's mapper:
/// the bare `Fill` and the `OrderFilled` wrap, both under the stop's order client id) and the REST fill read
/// (the restart reconcile's `FillReport`, whose `venue_order_id` is the CHILD's).
#[test]
fn a_triggered_stops_fill_names_the_stop_by_its_label() {
    let events = map_deribit_private(&triggered_stops_trade_frame(), "deribit", SYMBOL);
    let kinds: Vec<(&str, String)> = events
        .iter()
        .map(|e| match e {
            Event::Fill(f) => ("Fill", f.order_client_id.clone()),
            Event::OrderFilled(w) => ("OrderFilled", w.order_client_id.clone()),
            other => panic!("only the fill and its wrap, got {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        vec![("Fill", "n0stp71".to_string()), ("OrderFilled", "n0stp71".to_string())]
    );
    if let Event::Fill(f) = &events[0] {
        assert_eq!((f.side, f.last_qty, f.last_price), (-1, 10.0, 80269.0));
    }

    let data = triggered_stops_trade_frame()["params"]["data"].to_string();
    let fills = parse_user_trades(&data).expect("a valid body");
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].order_client_id.as_deref(), Some("n0stp71"));
    assert_eq!(fills[0].venue_order_id.as_str(), "124740642635", "the child's id, not SLTS-…");
}

/// The A3 replay after a reconnect reads the executed child back from the order history (state
/// `filled`, `trigger_order_id` naming the stop) and skips it: the fill wrap already terminalised the
/// stop. A cancelled untriggered stop is absent from that history altogether (measured), so a venue
/// side cancel of a stop is never replayed.
#[test]
fn the_history_replay_skips_the_executed_child_of_a_stop() {
    let child = json!({
        "amount": 10.0, "average_price": 80269.0, "direction": "sell", "filled_amount": 10.0,
        "instrument_name": SYMBOL, "label": "n0stp71", "last_update_timestamp": 1793016943155i64,
        "order_id": "124740642635", "order_state": "filled", "order_type": "market",
        "reduce_only": true, "trigger_order_id": "SLTS-11047216", "triggered": true,
    });
    let trades = json!([triggered_stops_trade_frame()["params"]["data"][0].clone()]);
    let events = map_deribit_history(&json!([child]), &trades, "deribit", SYMBOL);
    assert_eq!(events.len(), 2, "the replayed fill and its wrap; the child order row adds nothing");
}

/// The reduce-only sell stop above as a `trailing` 500.3 behind the market (no activation).
fn trailing() -> OrderRequest {
    let mut req = stop();
    req.order_type = OrderType::Trailing;
    req.trigger_price = None;
    req.trail = Some(500.3);
    req
}

/// THE TRAILING WIRE SHAPE. A `trailing` is a `trailing_stop` whose `trigger_offset` is the trail floored
/// to the 0.5 tick (500.3 goes out as 500.0), with the `trigger` series the venue requires, no
/// `trigger_price` and no `price`, and `reduce_only` carried. The whole frame.
#[test]
fn a_trailing_goes_out_as_a_trailing_stop_with_its_trail_as_the_trigger_offset() {
    let fake = FakeDeribit::spawn(vec![Session::OkForever]);
    let rest = rest_on(&fake, SYMBOL, perp_grid());
    let mut req = trailing();
    req.trigger_by = Some(TriggerBy::Mark);
    assert_matches!(outcome(rest.submit_order(&req)), Event::OrderAccepted(_));
    assert_eq!(
        sent(&fake, "private/sell"),
        json!({
            "instrument_name": SYMBOL,
            "amount": 10.0,
            "type": "trailing_stop",
            "label": ORDER_CLIENT_ID,
            "post_only": false,
            "trigger": "mark_price",
            "trigger_offset": 500.0,
            "reduce_only": true,
        })
    );
}

/// What the venue's trailing stop cannot carry is refused BY NAME with nothing on the socket: an
/// activation (deribit has none), a trail of zero or one under the tick, a `trailing_limit` (no
/// trailing stop whose fill is a limit), and a trailing stop on an option.
#[test]
fn a_trailing_the_venue_cannot_hold_is_refused_by_name_with_nothing_on_the_wire() {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    let (option, option_grid) = option_row();
    let mut activated = trailing();
    activated.trigger_price = Some(61_000.0);
    let mut no_trail = trailing();
    no_trail.trail = None;
    let mut under_a_tick = trailing();
    under_a_tick.trail = Some(0.3);
    let mut trailing_limit = trailing();
    trailing_limit.order_type = OrderType::TrailingLimit;
    trailing_limit.limit_offset = Some(10.0);
    let mut on_option = trailing();
    on_option.symbol = option.clone();
    for (req, symbol, grid, why, family) in [
        (
            activated,
            SYMBOL.to_string(),
            perp_grid(),
            "trailing_stop has no activation price",
            VenueCapability,
        ),
        (no_trail, SYMBOL.to_string(), perp_grid(), "trailing needs a trail above zero", NotSent),
        (
            under_a_tick,
            SYMBOL.to_string(),
            perp_grid(),
            "the trail 0.3 is under the instrument's tick",
            NotSent,
        ),
        (
            trailing_limit,
            SYMBOL.to_string(),
            perp_grid(),
            "deribit builds no `trailing_limit` order",
            VenueCapability,
        ),
        (on_option, option, option_grid, "no trailing order on an option", VenueCapability),
    ] {
        let fake = FakeDeribit::spawn(vec![Session::OkForever]);
        let rest = rest_on(&fake, &symbol, grid);
        match outcome(rest.submit_order(&req)) {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, ORDER_CLIENT_ID);
                assert!(r.reason.contains(why), "wanted `{why}`, got `{}`", r.reason);
                assert_eq!(r.refusal_id.as_deref(), Some(family.word()), "{why}");
            }
            other => panic!("`{why}` must be a terminal reject, got {other:?}"),
        }
        assert!(fake.methods().is_empty(), "{why}: nothing may reach the socket");
    }
}

/// The testnet's row for a resting reduce-only SELL trailing stop (BTC-PERPETUAL, 2026-11-08, `user_id`
/// dropped), as `deribit_trailing_stop_testnet_smoke` captured it: `order_type` `trailing_stop`, `untriggered`,
/// `price` `market_price`, the trail as `trigger_offset` and the stop the venue keeps as `trigger_price`, its
/// id in `TSTS-` (a stop's is `SLTS-`).
const CAPTURED_TRAILING_ROW: &str = r#"{"amount":10.0,"api":true,"contracts":1.0,"creation_timestamp":1794160422678,"direction":"sell","instrument_name":"BTC-PERPETUAL","is_liquidation":false,"label":"n0tr22665t","last_update_timestamp":1794160422678,"mmp":false,"order_id":"TSTS-11058763","order_state":"untriggered","order_type":"trailing_stop","post_only":false,"price":"market_price","reduce_only":true,"replaced":false,"risk_reducing":false,"time_in_force":"good_til_cancelled","trigger":"last_price","trigger_offset":37845.5,"trigger_price":37844.5,"trigger_reference_price":75690.0,"triggered":false,"web":false}"#;

/// A resting trailing stop as the open-order read lists it (the captured row) reads as a resting `Trailing`
/// under our order client id, and that row cancelled while it waited (the stop's cancel words) ends the order, as a
/// stop's does.
#[test]
fn a_held_trailing_stop_reads_back_as_a_trailing_and_its_cancel_ends_it() {
    let mut row: Value =
        serde_json::from_str(CAPTURED_TRAILING_ROW).expect("the captured row parses");
    let reports = parse_open_orders(&json!([row.clone()]).to_string()).expect("a valid body");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].order_client_id.as_deref(), Some("n0tr22665t"));
    assert_eq!(reports[0].venue_order_id.as_str(), "TSTS-11058763");
    assert_eq!(reports[0].order_type, OrderType::Trailing);
    assert_eq!(reports[0].status, OrderStatus::Accepted);
    // its terms: the trail is `trigger_offset`; the captured `trigger_price` is the venue's moving stop,
    // not an activation, so it is not stated
    let r = &reports[0];
    assert_eq!(
        (r.price, r.trigger_price, r.trail, r.trail_percent),
        (None, None, Some(37845.5), None)
    );

    row["order_state"] = json!("cancelled");
    row["cancel_reason"] = json!("user_request");
    let frame = json!({"jsonrpc": "2.0", "method": "subscription", "params": {
        "channel": "user.orders.any.any.raw", "data": row}});
    match map_deribit_private(&frame, "deribit", SYMBOL).as_slice() {
        [Event::OrderCanceled(c)] => assert_eq!(c.order_client_id, "n0tr22665t"),
        other => panic!("the trailing stop's own cancelled row ends it, got {other:?}"),
    }
}
