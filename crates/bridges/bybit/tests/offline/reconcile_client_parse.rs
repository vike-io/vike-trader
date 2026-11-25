//! Pure parse tests for the bybit `ReconcileClient` report parsers (Task 12) — NO network, just
//! representative JSON bodies shaped like Bybit V5's real `result` objects (the `{retCode,
//! retMsg, result}` envelope already unwrapped by the client — see `recon_client::unwrap_result`)
//! for `/v5/order/realtime`, `/v5/execution/list`, `/v5/position/list`. Asserts the tricky
//! mappings the brief calls out explicitly: Buy/Sell → signed qty (positions are wire-UNSIGNED,
//! `size` + `side`), hedge `positionIdx` → `position_side`, venue order-status normalization to
//! the `OrderStatus::parse` FSM vocabulary, and the maker/taker liquidity flag. Venue ids use
//! Bybit's real wire FORMAT — `orderId`/`execId` are UUIDs (per the V5 docs), `orderLinkId` is our
//! own client id — so a format quirk cannot slip past a toy `"o1"`.

use vike_bybit::reconcile::{
    parse_fee_rate, parse_fills, parse_orders, parse_positions, parse_wallet_balance,
};
use vike_model::events::{LiquiditySide, PositionSide};
use vike_model::{FeeSchedule, MarginMode, OrderStatus, OrderType};

/// `/v5/account/fee-rate` `result.list[0]` -> FeeSchedule from maker/takerFeeRate (fractions).
#[test]
fn parses_fee_rate_into_fee_schedule() {
    let body =
        r#"{"list":[{"symbol":"BTCUSDT","takerFeeRate":"0.00055","makerFeeRate":"0.0002"}]}"#;
    let s = parse_fee_rate(body).unwrap().expect("fee row present");
    assert_eq!(s, FeeSchedule::PercentMakerTaker { maker_bps: 2.0, taker_bps: 5.5 });
}

/// An empty fee-rate list -> None (fail-soft to the static default).
#[test]
fn empty_fee_rate_list_is_none() {
    assert_eq!(parse_fee_rate(r#"{"list":[]}"#).unwrap(), None);
}

/// The exact shape from the task brief: New/PartiallyFilled/Filled/Cancelled → the FSM vocabulary,
/// an empty `orderLinkId` normalizes to `None` (externally-placed order).
#[test]
fn parses_open_order_new_status_and_empty_client_id() {
    let body = r#"{"category":"linear","list":[
        {"symbol":"BTCUSDT","orderId":"5bd7b284-89fa-4087-8306-9a17381218f3","orderLinkId":"","side":"Buy","orderType":"Limit","qty":"0.010","cumExecQty":"0.000","avgPrice":"0","orderStatus":"New","updatedTime":"1700000000000","positionIdx":0}
    ],"nextPageCursor":""}"#;
    let reports = parse_orders(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.venue_order_id.as_str(), "5bd7b284-89fa-4087-8306-9a17381218f3");
    assert_eq!(r.order_client_id, None, "empty orderLinkId normalizes to None");
    assert_eq!(r.side, 1, "Buy -> +1");
    assert_eq!(r.order_type, vike_model::OrderType::Limit);
    assert_eq!(r.qty, 0.01);
    assert_eq!(r.filled_qty, 0.0);
    assert_eq!(r.average_price, 0.0);
    assert_eq!(r.status, vike_model::OrderStatus::Accepted, "New -> ACCEPTED");
    assert_eq!(r.timestamp, 1700000000000);
}

#[test]
fn parses_order_row_every_field_and_status_normalization() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","orderId":"cebf9f04-b25e-4f26-bbdb-55c78a28e1d8","orderLinkId":"c2","side":"Sell","orderType":"Market","qty":"0.500","cumExecQty":"0.500","avgPrice":"30010.5","orderStatus":"Filled","updatedTime":"1700000002000","positionIdx":0},
        {"symbol":"BTCUSDT","orderId":"58a42c41-f5d1-48f0-a086-0dc29ebc7ed6","orderLinkId":"c3","side":"Buy","orderType":"Limit","qty":"0.200","cumExecQty":"0.100","avgPrice":"29500.0","orderStatus":"PartiallyFilled","updatedTime":"1700000003000","positionIdx":0},
        {"symbol":"BTCUSDT","orderId":"8966f129-23bc-453d-8eec-6bbad6ce497f","orderLinkId":"c4","side":"Sell","orderType":"Limit","qty":"0.100","cumExecQty":"0.000","avgPrice":"0","orderStatus":"Cancelled","updatedTime":"1700000004000","positionIdx":0}
    ]}"#;
    let reports = parse_orders(body).unwrap();
    assert_eq!(reports.len(), 3);

    let filled = &reports[0];
    assert_eq!(filled.side, -1, "Sell -> -1");
    assert_eq!(filled.qty, 0.5);
    assert_eq!(filled.filled_qty, 0.5);
    assert_eq!(filled.average_price, 30010.5);
    assert_eq!(filled.status, vike_model::OrderStatus::Filled);
    assert_eq!(filled.order_client_id.as_deref(), Some("c2"));

    let partial = &reports[1];
    assert_eq!(partial.side, 1, "Buy -> +1");
    assert_eq!(partial.status, vike_model::OrderStatus::PartiallyFilled);

    let canceled = &reports[2];
    assert_eq!(canceled.status, vike_model::OrderStatus::Canceled, "Cancelled -> CANCELED");
}

/// REAL `order/realtime` answer (Bybit DEMO, linear, no `orderFilter`, recorded 2026-10-24 by the
/// batch-6 drill; envelope unwrapped, nothing else edited): a long BTCUSDT 0.001 with a reduce-only
/// CONDITIONAL market stop (`stopOrderType:"Stop"`, `Untriggered`), a reduce-only LIMIT take-profit and
/// a plain limit. The stop must read as a stop — it is the row the node's `POSITION WITHOUT STOP`
/// warning could not see — and the other two must stay what they were.
#[test]
fn recorded_conditional_stop_reads_as_a_stop_and_the_limits_stay_limits() {
    let body = r#"{"category":"linear","list":[{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1792811914196","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"80924.3","leavesQty":"0.001","leavesValue":"0","marketUnit":"","orderId":"574d70ee-5014-4a90-afc9-aa47d3e5e42d","orderIv":"","orderLinkId":"b6bybitstop","orderStatus":"Untriggered","orderType":"Market","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"0","qty":"0.001","reduceOnly":true,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"Stop","symbol":"BTCUSDT","takeProfit":"","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"60693.2","updatedTime":"1792811914196"},{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByUser","createdTime":"1792811902192","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"80924.3","leavesQty":"0.001","leavesValue":"60.7","marketUnit":"","orderId":"a42cc36c-b21e-4042-b91f-4fd403022b7c","orderIv":"","orderLinkId":"b6bybitlim","orderStatus":"New","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"60700","qty":"0.001","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"","triggerDirection":0,"triggerPrice":"","updatedTime":"1792811902192"},{"avgPrice":"","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByUser","createdTime":"1792811897491","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0","cumFeeDetail":{},"isLeverage":"","lastPriceOnCreated":"80924.3","leavesQty":"0.001","leavesValue":"101.2","marketUnit":"","orderId":"a69a4457-883f-4acf-a984-ce758c7f6d54","orderIv":"","orderLinkId":"b6bybittp","orderStatus":"New","orderType":"Limit","parentOrderLinkId":"","placeType":"","positionIdx":0,"price":"101200","qty":"0.001","reduceOnly":true,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"","stopOrderType":"","symbol":"BTCUSDT","takeProfit":"","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","tpslMode":"","triggerBy":"","triggerDirection":0,"triggerPrice":"","updatedTime":"1792811897491"}],"nextPageCursor":"574d70ee-5014-4a90-afc9-aa47d3e5e42d%3A1792811914196%2Ca69a4457-883f-4acf-a984-ce758c7f6d54%3A1792811897491"}"#;
    let reports = parse_orders(body).unwrap();
    assert_eq!(reports.len(), 3);

    // The fields the stop-cover rule reads: a live STOP kind, the exit side, the open quantity.
    let stop = &reports[0];
    assert_eq!(stop.order_client_id.as_deref(), Some("b6bybitstop"));
    assert_eq!(stop.venue_order_id.as_str(), "574d70ee-5014-4a90-afc9-aa47d3e5e42d");
    assert_eq!(stop.order_type, OrderType::Stop, "stopOrderType:Stop on a Market row is a Stop");
    assert_eq!(stop.status, OrderStatus::Accepted, "Untriggered is live");
    assert!(stop.status.is_live());
    assert_eq!(stop.side, -1, "a SELL stop closes the long");
    assert_eq!(stop.qty, 0.001);
    assert_eq!(stop.filled_qty, 0.0);

    // Unchanged from before the change: an empty `stopOrderType` is the plain `orderType`.
    assert_eq!(reports[1].order_type, OrderType::Limit);
    assert_eq!(reports[2].order_type, OrderType::Limit, "a reduce-only limit TP is no stop");

    // The recorded terms: the stop's level (its `price` "0" is none), each limit's price.
    let terms: Vec<_> =
        reports.iter().map(|r| (r.price, r.trigger_price, r.trail, r.trail_percent)).collect();
    assert_eq!(
        terms,
        [
            (None, Some(60693.2), None, None),
            (Some(60700.0), None, None, None),
            (Some(101200.0), None, None, None),
        ]
    );
}

/// One conditional-looking `order/realtime` row, `direction: None` leaving `triggerDirection` out.
fn kind_of(
    order_type: &str,
    stop_order_type: &str,
    side: &str,
    direction: Option<i64>,
) -> OrderType {
    let direction = direction.map(|d| format!(r#","triggerDirection":{d}"#)).unwrap_or_default();
    let body = format!(
        r#"{{"list":[{{"symbol":"BTCUSDT","orderId":"o","orderLinkId":"c","side":"{side}","orderType":"{order_type}","stopOrderType":"{stop_order_type}","qty":"0.001","cumExecQty":"0","avgPrice":"","orderStatus":"Untriggered","triggerPrice":"60000","updatedTime":"1"{direction}}}]}}"#
    );
    parse_orders(&body).unwrap().remove(0).order_type
}

/// Every documented `stopOrderType` word (`/v5/enum`) and what it becomes. Only `Stop` was recorded;
/// the rest are the documented words.
#[test]
fn stop_order_type_words_map_to_the_kinds_the_shared_model_has() {
    for stop_loss in ["StopLoss", "PartialStopLoss"] {
        assert_eq!(kind_of("Market", stop_loss, "Sell", Some(2)), OrderType::Stop, "{stop_loss}");
        assert_eq!(
            kind_of("Limit", stop_loss, "Sell", Some(2)),
            OrderType::StopLimit,
            "{stop_loss}"
        );
        // the venue named it a stop loss: its trigger direction is not second-guessed
        assert_eq!(kind_of("Market", stop_loss, "Sell", Some(1)), OrderType::Stop, "{stop_loss}");
        assert_eq!(kind_of("Market", stop_loss, "Sell", None), OrderType::Stop, "{stop_loss}");
    }
    for take_profit in ["TakeProfit", "PartialTakeProfit"] {
        assert_eq!(kind_of("Market", take_profit, "Sell", Some(1)), OrderType::TakeProfit);
        assert_eq!(kind_of("Limit", take_profit, "Sell", Some(1)), OrderType::TakeProfit);
    }
    assert_eq!(
        kind_of("Market", "TrailingStop", "Sell", Some(2)),
        OrderType::Unrecognized("trailing_stop".into()),
        "no trailing twin: kept as a stop-named word, like binance's trailing_stop_market"
    );
    assert_eq!(kind_of("Limit", "Stop", "Buy", Some(1)), OrderType::StopLimit);
    // a plain `Stop` needs a usable direction: none is not a stop (fails closed)
    assert_eq!(kind_of("Market", "Stop", "Sell", None), OrderType::Market);
}

/// A plain conditional (`Stop`) is a stop only when it fires in the loss direction of the exit it is
/// (BUY on a rise = 1, SELL on a fall = 2); in the PROFIT direction (BUY on a fall, SELL on a rise) it is a
/// `TakeProfit`, whether it fires as a `Market` or a `Limit`, and never cover. FAILS CLOSED: a row with no
/// usable direction (the field missing, or `0`) cannot say which it is, so it is neither and the warning stays.
#[test]
fn a_plain_conditional_is_a_stop_in_the_loss_direction_and_a_take_profit_in_the_profit_direction() {
    assert_eq!(kind_of("Market", "Stop", "Sell", Some(2)), OrderType::Stop);
    assert_eq!(kind_of("Market", "Stop", "Buy", Some(1)), OrderType::Stop);
    assert_eq!(kind_of("Limit", "Stop", "Sell", Some(2)), OrderType::StopLimit);
    assert_eq!(kind_of("Limit", "Stop", "Buy", Some(1)), OrderType::StopLimit);
    for side in ["Sell", "Buy"] {
        assert_eq!(kind_of("Market", "Stop", side, None), OrderType::Market, "{side}, no field");
        assert_eq!(kind_of("Market", "Stop", side, Some(0)), OrderType::Market, "{side}, 0");
        assert_eq!(kind_of("Limit", "Stop", side, None), OrderType::Limit, "{side}, no field");
        assert_eq!(kind_of("Limit", "Stop", side, Some(0)), OrderType::Limit, "{side}, 0");
    }
    for order_type in ["Market", "Limit"] {
        let sell_on_a_rise = kind_of(order_type, "Stop", "Sell", Some(1));
        assert_eq!(sell_on_a_rise, OrderType::TakeProfit, "{order_type}: sell on a rise");
        let buy_on_a_fall = kind_of(order_type, "Stop", "Buy", Some(2));
        assert_eq!(buy_on_a_fall, OrderType::TakeProfit, "{order_type}: buy on a fall");
    }
}

/// Two resting take-profits in bybit's documented `order/realtime` row shape (the recorded stop-limit rows'
/// keys, PROFIT-direction values: not captures): a limit-if-touched BUY firing on a fall and a
/// market-if-touched SELL firing on a rise, each a LIVE `TakeProfit` carrying its order client id; and the two RECORDED
/// loss-direction rows beside them read exactly as they always did.
#[test]
fn documented_take_profit_rows_read_as_live_take_profits_beside_the_recorded_stops() {
    let take_profits = [
        r#"{"avgPrice":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794120446348","cumExecQty":"0","leavesQty":"0.001","orderId":"tp-lit-buy","orderLinkId":"vttplb1","orderStatus":"Untriggered","orderType":"Limit","positionIdx":0,"price":"73527.1","qty":"0.001","reduceOnly":false,"side":"Buy","stopOrderType":"Stop","symbol":"BTCUSDT","timeInForce":"GTC","triggerBy":"LastPrice","triggerDirection":2,"triggerPrice":"73507.1","updatedTime":"1794120446348"}"#,
        r#"{"avgPrice":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createType":"CreateByStopOrder","createdTime":"1794120447759","cumExecQty":"0","leavesQty":"0.001","orderId":"tp-mit-sell","orderLinkId":"vttpms1","orderStatus":"Untriggered","orderType":"Market","positionIdx":0,"price":"0","qty":"0.001","reduceOnly":false,"side":"Sell","stopOrderType":"Stop","symbol":"BTCUSDT","timeInForce":"IOC","triggerBy":"LastPrice","triggerDirection":1,"triggerPrice":"78056.1","updatedTime":"1794120447759"}"#,
    ];
    let rows: Vec<&str> =
        take_profits.into_iter().chain([super::reconcile_paging::RECORDED_STOP_ROW]).collect();
    let body = format!(r#"{{"list":[{}]}}"#, rows.join(","));
    let reports = parse_orders(&body).expect("a well-formed order/realtime body");
    let kinds: Vec<(Option<&str>, i32, &OrderType, bool)> = reports
        .iter()
        .map(|r| (r.order_client_id.as_deref(), r.side, &r.order_type, r.status.is_live()))
        .collect();
    assert_eq!(
        kinds,
        [
            (Some("vttplb1"), 1, &OrderType::TakeProfit, true),
            (Some("vttpms1"), -1, &OrderType::TakeProfit, true),
            (Some("b6bybitstop"), -1, &OrderType::Stop, true),
        ]
    );
}

/// The five SPOT conditionals `bybit_spot_conditional_smoke` rested on the DEMO venue (`DOGEUSDT`, 2026-11-08;
/// its `CAPTURED` lines verbatim): the venue states `triggerDirection` 0 and `triggerBy` "", and the row
/// carries `basePrice` (the last price at placement, 0.07102 for all five). A trigger above it fires on a rise,
/// below on a fall, so each types as the linear row with that direction: the two stop-limits (a BUY over, a
/// SELL under) are `StopLimit`, the limit-if-touched BUY under and the market-if-touched SELL over and BUY under
/// are `TakeProfit`.
const CAPTURED_SPOT_CONDITIONAL_ROWS: [&str; 5] = [
    r#"{"activationPrice":"0","avgPrice":"0.00000","basePrice":"0.07102","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createdTime":"1794160402261","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0.000000","cumFeeDetail":{},"isLeverage":"0","lastPriceOnCreated":"","leavesQty":"95.5","leavesValue":"7.004925","marketUnit":"","orderId":"2343981776202108160","orderIv":"","orderLinkId":"vtscsb60402173","orderStatus":"Untriggered","orderType":"Limit","placeType":"","positionIdx":0,"price":"0.07335","qty":"95.5","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"0","stopOrderType":"Stop","symbol":"DOGEUSDT","takeProfit":"0","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","trailingPercentage":"0","trailingValue":"0","triggerBy":"","triggerDirection":0,"triggerPrice":"0.07315","updatedTime":"1794160402261"}"#,
    r#"{"activationPrice":"0","avgPrice":"0.00000","basePrice":"0.07102","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createdTime":"1794160404262","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0.000000","cumFeeDetail":{},"isLeverage":"0","lastPriceOnCreated":"","leavesQty":"102","leavesValue":"7.006380","marketUnit":"","orderId":"2343981792996101376","orderIv":"","orderLinkId":"vtscss60404175","orderStatus":"Untriggered","orderType":"Limit","placeType":"","positionIdx":0,"price":"0.06869","qty":"102.0","reduceOnly":false,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"0","stopOrderType":"Stop","symbol":"DOGEUSDT","takeProfit":"0","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","trailingPercentage":"0","trailingValue":"0","triggerBy":"","triggerDirection":0,"triggerPrice":"0.06889","updatedTime":"1794160404262"}"#,
    r#"{"activationPrice":"0","avgPrice":"0.00000","basePrice":"0.07102","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createdTime":"1794160406239","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0.000000","cumFeeDetail":{},"isLeverage":"0","lastPriceOnCreated":"","leavesQty":"101.4","leavesValue":"7.005726","marketUnit":"","orderId":"2343981809571990784","orderIv":"","orderLinkId":"vtsclb60406151","orderStatus":"Untriggered","orderType":"Limit","placeType":"","positionIdx":0,"price":"0.06909","qty":"101.4","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"0","stopOrderType":"Stop","symbol":"DOGEUSDT","takeProfit":"0","timeInForce":"GTC","tpLimitPrice":"0","tpTriggerBy":"","trailingPercentage":"0","trailingValue":"0","triggerBy":"","triggerDirection":0,"triggerPrice":"0.06889","updatedTime":"1794160406239"}"#,
    r#"{"activationPrice":"0","avgPrice":"0.00000","basePrice":"0.07102","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createdTime":"1794160408218","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0.000000","cumFeeDetail":{},"isLeverage":"0","lastPriceOnCreated":"","leavesQty":"95.7","leavesValue":"0.000000","marketUnit":"baseCoin","orderId":"2343981826173046016","orderIv":"","orderLinkId":"vtscms60408130","orderStatus":"Untriggered","orderType":"Market","placeType":"","positionIdx":0,"price":"0","qty":"95.7","reduceOnly":false,"rejectReason":"EC_NoError","side":"Sell","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"0","stopOrderType":"Stop","symbol":"DOGEUSDT","takeProfit":"0","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","trailingPercentage":"0","trailingValue":"0","triggerBy":"","triggerDirection":0,"triggerPrice":"0.07315","updatedTime":"1794160408218"}"#,
    r#"{"activationPrice":"0","avgPrice":"0.00000","basePrice":"0.07102","blockTradeId":"","cancelType":"UNKNOWN","closeOnTrigger":false,"createdTime":"1794160409808","cumExecFee":"0","cumExecQty":"0","cumExecValue":"0.000000","cumFeeDetail":{},"isLeverage":"0","lastPriceOnCreated":"","leavesQty":"101.7","leavesValue":"7.006113","marketUnit":"baseCoin","orderId":"2343981839510932736","orderIv":"","orderLinkId":"vtscmb60409720","orderStatus":"Untriggered","orderType":"Market","placeType":"","positionIdx":0,"price":"0","qty":"101.7","reduceOnly":false,"rejectReason":"EC_NoError","side":"Buy","slLimitPrice":"0","slTriggerBy":"","smpGroup":"0","smpOrderId":"","smpType":"None","stopLoss":"0","stopOrderType":"Stop","symbol":"DOGEUSDT","takeProfit":"0","timeInForce":"IOC","tpLimitPrice":"0","tpTriggerBy":"","trailingPercentage":"0","trailingValue":"0","triggerBy":"","triggerDirection":0,"triggerPrice":"0.06889","updatedTime":"1794160409808"}"#,
];

#[test]
fn captured_spot_conditionals_read_their_direction_off_the_base_price() {
    let body = format!(r#"{{"list":[{}]}}"#, CAPTURED_SPOT_CONDITIONAL_ROWS.join(","));
    let reports = parse_orders(&body).expect("the captured order/realtime rows");
    let kinds: Vec<(Option<&str>, i32, &OrderType, bool)> = reports
        .iter()
        .map(|r| (r.order_client_id.as_deref(), r.side, &r.order_type, r.status.is_live()))
        .collect();
    assert_eq!(
        kinds,
        [
            (Some("vtscsb60402173"), 1, &OrderType::StopLimit, true),
            (Some("vtscss60404175"), -1, &OrderType::StopLimit, true),
            (Some("vtsclb60406151"), 1, &OrderType::TakeProfit, true),
            (Some("vtscms60408130"), -1, &OrderType::TakeProfit, true),
            (Some("vtscmb60409720"), 1, &OrderType::TakeProfit, true),
        ]
    );
}

/// The `basePrice` reading fills a `0` only: a direction the venue STATES wins over it, and a row whose
/// trigger sits AT its base price (or with no usable trigger) still says nothing and fails closed.
#[test]
fn a_stated_direction_wins_over_the_base_price_and_an_equal_one_fails_closed() {
    let spot_sell = CAPTURED_SPOT_CONDITIONAL_ROWS[1]; // a SELL stop-limit, trigger 0.06889 under 0.07102
    let one = |row: String| parse_orders(&format!(r#"{{"list":[{row}]}}"#)).unwrap().remove(0);
    let stated = spot_sell.replace(r#""triggerDirection":0"#, r#""triggerDirection":1"#);
    assert_eq!(
        one(stated).order_type,
        OrderType::TakeProfit,
        "the stated rise, not the base price"
    );
    let at_base = spot_sell.replace(r#""basePrice":"0.07102""#, r#""basePrice":"0.06889""#);
    assert_eq!(one(at_base).order_type, OrderType::Limit, "a trigger at its base price");
    let no_base = spot_sell.replace(r#""basePrice":"0.07102","#, "");
    assert_eq!(one(no_base).order_type, OrderType::Limit, "no base price");
    let no_trigger = spot_sell.replace(r#""triggerPrice":"0.06889""#, r#""triggerPrice":"0""#);
    assert_eq!(one(no_trigger).order_type, OrderType::Limit, "no trigger price");
}

/// Everything not mapped above reads exactly as it did before `stopOrderType` was looked at: the empty
/// word of a plain order, the spot-only and margin-rate words, a word the docs do not list, and a row
/// whose own `orderType` is V5's `UNKNOWN`.
#[test]
fn unknown_stop_order_types_stay_as_they_were() {
    for word in [
        "",
        "UNKNOWN",
        "tpslOrder",
        "OcoOrder",
        "BidirectionalTpslOrder",
        "MmRateClose",
        "SomethingBybitAddsLater",
        "stop",
        "STOP",
    ] {
        assert_eq!(kind_of("Market", word, "Sell", Some(2)), OrderType::Market, "{word:?}");
        assert_eq!(kind_of("Limit", word, "Buy", Some(1)), OrderType::Limit, "{word:?}");
    }
    assert_eq!(
        kind_of("UNKNOWN", "Stop", "Sell", Some(2)),
        OrderType::Unrecognized("unknown".into()),
        "an orderType outside Market/Limit is never retyped"
    );
}

#[test]
fn parses_execution_list_fill_report_every_field() {
    let body = r#"{"list":[
        {"execType":"Trade","symbol":"BTCUSDT","orderId":"e5d2b9af-f7e0-4e8e-8880-679b850f4654","orderLinkId":"c9","execId":"c9f2b7e1-62b4-4164-aaff-aaff82e87d10","side":"Buy","execQty":"0.500","execPrice":"30000.0","execFee":"0.01","feeCurrency":"USDT","isMaker":true,"execTime":"1700000005000"}
    ]}"#;
    let reports = parse_fills(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.trade_id.as_str(), "c9f2b7e1-62b4-4164-aaff-aaff82e87d10");
    assert_eq!(r.venue_order_id.as_str(), "e5d2b9af-f7e0-4e8e-8880-679b850f4654");
    assert_eq!(r.order_client_id.as_deref(), Some("c9"));
    assert_eq!(r.side, 1, "Buy -> +1");
    assert_eq!(r.last_qty, 0.5);
    assert_eq!(r.last_price, 30000.0);
    assert_eq!(r.commission, 0.01);
    assert_eq!(r.commission_asset, "USDT");
    assert_eq!(r.liquidity_side, LiquiditySide::Maker, "isMaker:true -> Maker");
    assert_eq!(r.timestamp, 1700000005000);
}

#[test]
fn execution_list_sell_and_taker_flag() {
    let body = r#"{"list":[
        {"execType":"Trade","symbol":"BTCUSDT","orderId":"9f8cffee-e6e0-4bac-849e-3f225659a1be","orderLinkId":"c10","execId":"f4ec95f5-c206-40bc-a5c8-7185330ad47f","side":"Sell","execQty":"0.250","execPrice":"29990.0","execFee":"0.005","feeCurrency":"USDT","isMaker":false,"execTime":"1700000006000"}
    ]}"#;
    let reports = parse_fills(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.side, -1, "Sell -> -1");
    assert_eq!(r.liquidity_side, LiquiditySide::Taker, "isMaker:false -> Taker");
}

/// A `Funding` row shaped like the one the CI box's bybit demo account listed at the 00:00 UTC funding instant (2026-11-09):
/// the position's size (0.01 ETHUSDT) at the funding mark (2126.59), no `orderLinkId`; the ids are made up. Reported as a
/// fill it was a `MissingFill` with no order client id, which `hybrid` auto-applies as a phantom trade every eight hours.
const FUNDING_ROW: &str = r#"{"execType":"Funding","symbol":"ETHUSDT","orderId":"1762646400-ETHUSDT-592334-Sell","orderLinkId":"","execId":"0a4c5e2b-funding-0000","side":"Sell","execQty":"0.01","execPrice":"2126.59","execFee":"0.0003","feeCurrency":"USDT","isMaker":false,"execTime":"1762646400000"}"#;

/// A `Trade` row of the same account.
const TRADE_ROW: &str = r#"{"execType":"Trade","symbol":"ETHUSDT","orderId":"5b3c0f0a-1d1e-4f6a-9c1b-2a3b4c5d6e7f","orderLinkId":"c11","execId":"7d2e9a41-trade-0001","side":"Buy","execQty":"0.01","execPrice":"2100.5","execFee":"0.011","feeCurrency":"USDT","isMaker":false,"execTime":"1762640000000"}"#;

#[test]
fn a_funding_row_is_not_a_fill() {
    let body = format!(r#"{{"list":[{FUNDING_ROW}]}}"#);
    assert!(parse_fills(&body).unwrap().is_empty(), "a funding instant is no fill");
}

/// **A row with no `execType` is not a fill either** (okx drops a row with no `subType` alike): V5 documents the field on
/// every row, so such a row is malformed, and the rule is said once per process.
#[test]
fn a_row_with_no_exec_type_is_not_a_fill() {
    let absent = TRADE_ROW.replace(r#""execType":"Trade","#, "");
    assert!(!absent.contains("execType"), "{absent}");
    let body = format!(r#"{{"list":[{absent},{TRADE_ROW}]}}"#);
    let ids: Vec<String> =
        parse_fills(&body).unwrap().iter().map(|r| r.trade_id.as_str().to_string()).collect();
    assert_eq!(ids, ["7d2e9a41-trade-0001"], "only the typed Trade row is a fill");
}

#[test]
fn a_trade_row_is_still_a_fill() {
    let body = format!(r#"{{"list":[{TRADE_ROW}]}}"#);
    let reports = parse_fills(&body).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].trade_id.as_str(), "7d2e9a41-trade-0001");
    assert_eq!(reports[0].order_client_id.as_deref(), Some("c11"));
}

/// **A mixed list keeps its trades and drops the rest**: the liquidation kinds the event path books as
/// `PositionLiquidated` (never a fill), a settlement and a row with no `execType` at all are none of them a fill.
#[test]
fn a_mixed_body_keeps_only_its_trade_rows() {
    let other = |kind: &str| TRADE_ROW.replace(r#""execType":"Trade""#, kind);
    let body = format!(
        r#"{{"list":[{FUNDING_ROW},{TRADE_ROW},{},{},{},{}]}}"#,
        other(r#""execType":"BustTrade""#).replace("trade-0001", "bust-0002"),
        other(r#""execType":"AdlTrade""#).replace("trade-0001", "adl-0003"),
        other(r#""execType":"Settle""#).replace("trade-0001", "settle-0004"),
        other(r#""execType":"""#).replace("trade-0001", "blank-0005"),
    );
    let reports = parse_fills(&body).unwrap();
    let ids: Vec<&str> = reports.iter().map(|r| r.trade_id.as_str()).collect();
    assert_eq!(ids, ["7d2e9a41-trade-0001"], "only the Trade row is a fill");
}

/// `/v5/position/list`: `size` is UNSIGNED — the sign comes from `side` Buy/Sell (never re-signed
/// by `positionIdx`). One-way (`positionIdx:0`) -> `PositionSide::Both`.
#[test]
fn position_list_signs_qty_from_buy_sell_side() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","markPrice":"29500.0","positionIdx":0,"tradeMode":0,"updatedTime":"1700000010000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert_eq!(r.venue, "bybit");
    assert_eq!(r.symbol, "BTCUSDT");
    assert_eq!(r.qty, 0.5, "Buy -> positive signed qty");
    assert_eq!(r.position_side, PositionSide::Both);
    assert_eq!(r.average_price, 29000.0);
    assert_eq!(r.timestamp, 1700000010000);
    assert_eq!(r.margin_mode, MarginMode::Cross, "tradeMode 0 -> Cross");
    assert_eq!(r.isolated_margin, None, "bybit has no verified isolated-wallet field");
}

/// Margin-mode step-2: `tradeMode` 1 -> Isolated (number OR string spelling); an absent
/// `tradeMode` is the fail-safe Cross default (pre-field behavior, UTA accounts).
#[test]
fn position_list_trade_mode_maps_to_margin_mode() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","positionIdx":0,"tradeMode":1,"updatedTime":"1700000010000"},
        {"symbol":"ETHUSDT","size":"1.000","side":"Buy","avgPrice":"1800.0","positionIdx":0,"tradeMode":"1","updatedTime":"1700000011000"},
        {"symbol":"XRPUSDT","size":"10.0","side":"Sell","avgPrice":"0.5","positionIdx":0,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 3);
    assert_eq!(reports[0].margin_mode, MarginMode::Isolated, "tradeMode 1 (number) -> Isolated");
    assert_eq!(
        reports[1].margin_mode,
        MarginMode::Isolated,
        "tradeMode \"1\" (string) -> Isolated"
    );
    assert_eq!(reports[2].margin_mode, MarginMode::Cross, "absent tradeMode -> fail-safe Cross");
    assert!(reports.iter().all(|r| r.isolated_margin.is_none()));
}

#[test]
fn position_list_sell_side_is_negative_qty() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.250","side":"Sell","avgPrice":"29200.0","markPrice":"29500.0","positionIdx":0,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(
        reports[0].qty, -0.25,
        "Sell -> negative signed qty, size stays unsigned on the wire"
    );
}

/// Hedge mode: `positionIdx` 1 -> LONG, 2 -> SHORT — never the one-way BOTH default. Both hedge
/// legs are signed by their OWN `side`, not inferred from the leg label.
#[test]
fn position_list_hedge_mode_position_idx_maps_to_long_and_short() {
    let body = r#"{"list":[
        {"symbol":"BTCUSDT","size":"0.500","side":"Buy","avgPrice":"29000.0","markPrice":"29500.0","positionIdx":1,"updatedTime":"1700000010000"},
        {"symbol":"BTCUSDT","size":"0.250","side":"Sell","avgPrice":"29200.0","markPrice":"29500.0","positionIdx":2,"updatedTime":"1700000012000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].position_side, PositionSide::Long, "positionIdx:1 -> LONG");
    assert_eq!(reports[0].qty, 0.5);
    assert_eq!(reports[1].position_side, PositionSide::Short, "positionIdx:2 -> SHORT");
    assert_eq!(
        reports[1].qty, -0.25,
        "the SHORT leg is still signed negative even though it's Sell-labeled Buy/Sell side"
    );
}

/// A flat (`size:"0"`) row is DELIBERATELY KEPT, not filtered: `recon::diff::diff` needs the report
/// row PRESENT to detect "local still shows a position the venue has since closed" — matching the
/// binance `ReconcileClient`'s documented choice (Task 7).
#[test]
fn position_list_keeps_flat_rows() {
    let body = r#"{"list":[
        {"symbol":"ETHUSDT","size":"0.000","side":"Buy","avgPrice":"0","markPrice":"1800.0","positionIdx":0,"updatedTime":"1700000011000"}
    ]}"#;
    let reports = parse_positions(body).unwrap();
    assert_eq!(reports.len(), 1, "the flat row must survive parsing");
    assert_eq!(reports[0].qty, 0.0);
}

#[test]
fn malformed_body_is_an_error_not_a_panic() {
    assert!(parse_orders("not json").is_err());
    assert!(parse_orders("{}").is_err(), "no `list` key is an error");
    assert!(parse_fills("null").is_err());
    assert!(parse_fills("42").is_err());
    assert!(parse_positions("\"oops\"").is_err());
    assert!(parse_positions("{}").is_err());
}

/// `GET /v5/account/wallet-balance` {accountType:UNIFIED} -> the UNIFIED account's USDT
/// `walletBalance` (total wallet cash, NOT `availableToWithdraw`).
#[test]
fn parses_unified_wallet_balance_usdt() {
    let body = r#"{"list":[{"accountType":"UNIFIED","totalEquity":"1520.5","coin":[
        {"coin":"BTC","walletBalance":"0.01","availableToWithdraw":"0.01"},
        {"coin":"USDT","walletBalance":"1500.25","availableToWithdraw":"1400.0"}
    ]}]}"#;
    assert_eq!(parse_wallet_balance(body).unwrap(), Some(1500.25));
}

#[test]
fn wallet_balance_missing_usdt_coin_is_none() {
    let body =
        r#"{"list":[{"accountType":"UNIFIED","coin":[{"coin":"BTC","walletBalance":"0.01"}]}]}"#;
    assert_eq!(parse_wallet_balance(body).unwrap(), None);
}

#[test]
fn wallet_balance_malformed_body_is_an_error_not_a_panic() {
    assert!(parse_wallet_balance("not json").is_err());
    assert!(parse_wallet_balance("{}").is_err(), "no `list` key is an error");
    assert!(parse_wallet_balance("null").is_err());
}

// --- the `recon_client` factory (ReconFactory seam, wave-2 task 6) — no network, just proves the
// wiring is infallible ------------------------------------------------------------------------

#[test]
fn reconcile_client_factory_builds_for_any_credentialed_symbol() {
    let creds = vike_secrets::credentials::Credentials {
        api_key: "test-key".to_string().into(),
        api_secret: "test-secret".to_string().into(),
        passphrase: None,
    };
    assert!(
        vike_bybit::recon_client(&creds, "BTCUSDT", "https://unused.invalid").is_some(),
        "construction is pure/infallible (no network) — always Some"
    );
}
