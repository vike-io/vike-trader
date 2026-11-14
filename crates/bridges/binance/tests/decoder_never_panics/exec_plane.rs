//! The exec-plane decoders (user-data mappers, history, reconcile parsers), fed arbitrary input.

use super::*;
use vike_binance::account::identity::parse_api_restrictions;
use vike_binance::execution::event_mapper::perp::map_binance_perp_opts;
use vike_binance::execution::rest::algo::parse_perp_open_algo_orders;
use vike_binance::execution::rest::perp::parse_binance_perp_instruments;
use vike_binance::execution::rest::spot::{parse_account_identity, parse_commission_rates};
use vike_binance::family::execution::event_mapper::perp::map_perp_opts;
use vike_binance::family::execution::event_mapper::spot::{map_execution_report, map_private};
use vike_binance::family::execution::order_map::{
    parse_symbol_properties, strip_broker_order_client_id_prefix,
};
use vike_binance::family::execution::resync::{map_history, map_perp_history};
use vike_binance::family::recon::{
    fill_is_maker, fill_side, parse_commission_rate_body, parse_margin_type, parse_perp_balance,
    parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk, parse_perp_user_trades,
    parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades, parse_spot_open_orders,
};
use vike_binance::transport::ws_auth::match_subscribe_ack;
use vike_bridge_core::json_arb::Field;

/// A small order id spelled as a number or a string, so grouped REST rows actually match.
fn small_id() -> BoxedStrategy<Value> {
    prop_oneof![(0i64..4).prop_map(|n| json!(n)), (0i64..4).prop_map(|n| json!(n.to_string()))]
        .boxed()
}

fn order_client_id() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => token(TOKENS),
        2 => "[ -~]{0,14}".prop_map(Value::String),
        1 => Just(Value::String("x-AB-12-".into())),
        1 => Just(Value::String("x--".into())),
    ]
    .boxed()
}

fn side_v() -> BoxedStrategy<Value> {
    token(&["BUY", "SELL", "buy", ""])
}

fn bool_v() -> BoxedStrategy<Value> {
    any::<bool>().prop_map(Value::Bool).boxed()
}

/// The `o` object of an `ORDER_TRADE_UPDATE`, or the body of a spot `executionReport`.
fn order_fields() -> Vec<Field> {
    vec![
        KIT.field("s", token(TOKENS)),
        KIT.field("c", order_client_id()),
        KIT.field("C", order_client_id()),
        KIT.field(
            "x",
            token(&["NEW", "CANCELED", "EXPIRED", "REJECTED", "TRADE", "CALCULATED", "AMENDMENT"]),
        ),
        KIT.field("X", token(&["NEW", "FILLED", "PARTIALLY_FILLED", "CANCELED"])),
        KIT.field("S", side_v()),
        KIT.field("r", token(TOKENS)),
        KIT.field("i", seq()),
        KIT.field("t", prop_oneof![seq(), numeric_string().prop_map(Value::String)].boxed()),
        KIT.field("l", KIT.number_str()),
        KIT.field("L", KIT.number_str()),
        KIT.field("n", KIT.number_str()),
        KIT.field("N", token(TOKENS)),
        KIT.field("m", bool_v()),
        KIT.field("ps", token(&["BOTH", "LONG", "SHORT", ""])),
    ]
}

fn with_e(e: &'static str, mut fields: Vec<Field>) -> BoxedStrategy<Value> {
    fields.push(KIT.field("e", Just(Value::String(e.into())).boxed()));
    fields.push(KIT.field("T", int()));
    fields.push(KIT.field("E", int()));
    object(fields)
}

fn wallet_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("a", token(TOKENS)),
        KIT.field("wb", prop_oneof![KIT.number_str(), KIT.arb_leaf()].boxed()),
        KIT.field("bc", KIT.number_str()),
        KIT.field("f", KIT.number_str()),
        KIT.field("l", KIT.number_str()),
    ])
}

/// A USDⓈ-M user-data frame: ORDER_TRADE_UPDATE, ACCOUNT_UPDATE, flat TRADE_LITE, or noise.
fn perp_frame() -> BoxedStrategy<Value> {
    let otu = (int(), object(order_fields()))
        .prop_map(|(t, o)| json!({ "e": "ORDER_TRADE_UPDATE", "T": t, "o": o }));
    let account = (
        int(),
        token(&["FUNDING_FEE", "ORDER", "DEPOSIT", ""]),
        prop::collection::vec(wallet_row(), 0..6),
    )
        .prop_map(
            |(t, m, rows)| json!({ "e": "ACCOUNT_UPDATE", "T": t, "a": { "m": m, "B": rows } }),
        );
    prop_oneof![
        4 => otu,
        2 => account,
        3 => with_e("TRADE_LITE", order_fields()),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

/// The `o` object of an `ALGO_UPDATE` (the Algo Service's order event): the real keys, every status.
fn algo_fields() -> Vec<Field> {
    vec![
        KIT.field("caid", order_client_id()),
        KIT.field("aid", seq()),
        KIT.field("at", token(&["CONDITIONAL", ""])),
        KIT.field("o", token(&["STOP_MARKET", "TAKE_PROFIT", "TRAILING_STOP_MARKET", ""])),
        KIT.field("s", token(TOKENS)),
        KIT.field("S", side_v()),
        KIT.field("ps", token(&["BOTH", "LONG", "SHORT", ""])),
        KIT.field(
            "X",
            token(&[
                "NEW",
                "CANCELED",
                "TRIGGERING",
                "TRIGGERED",
                "FINISHED",
                "REJECTED",
                "EXPIRED",
                "",
            ]),
        ),
        // `ai`: "" before a child exists, a number or a numeric string after
        KIT.field(
            "ai",
            prop_oneof![
                seq(),
                numeric_string().prop_map(Value::String),
                Just(Value::String(String::new()))
            ]
            .boxed(),
        ),
        KIT.field("q", KIT.number_str()),
        KIT.field("tp", KIT.number_str()),
        KIT.field("p", KIT.number_str()),
        KIT.field("rm", token(TOKENS)),
    ]
}

/// An `ALGO_UPDATE` frame, the object under `o` (USDⓈ-M) or `ao` (the portfolio-margin spelling).
fn algo_update_frame() -> BoxedStrategy<Value> {
    (int(), object(algo_fields()), any::<bool>())
        .prop_map(|(t, o, under_ao)| {
            if under_ao {
                json!({ "e": "ALGO_UPDATE", "T": t, "ao": o })
            } else {
                json!({ "e": "ALGO_UPDATE", "T": t, "o": o })
            }
        })
        .boxed()
}

/// A `GET /fapi/v1/openAlgoOrders` row: the real keys, hostile numerics in the quantity.
fn algo_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("symbol", token(TOKENS)),
        KIT.field("algoId", small_id()),
        KIT.field("clientAlgoId", order_client_id()),
        KIT.field("side", side_v()),
        KIT.field(
            "orderType",
            token(&["STOP_MARKET", "TAKE_PROFIT", "STOP", "TRAILING_STOP_MARKET", ""]),
        ),
        KIT.field("quantity", numeric_string().prop_map(Value::String).boxed()),
        KIT.field("triggerPrice", KIT.number_str()),
        KIT.field("price", KIT.number_str()),
        KIT.field(
            "algoStatus",
            token(&[
                "NEW",
                "TRIGGERING",
                "TRIGGERED",
                "FINISHED",
                "CANCELED",
                "EXPIRED",
                "REJECTED",
                "",
            ]),
        ),
        KIT.field("reduceOnly", bool_v()),
        KIT.field("updateTime", int()),
    ])
}

/// A spot user-data frame, bare or inside the WS-API `{"event":…}` envelope.
fn spot_frame() -> BoxedStrategy<Value> {
    let report = with_e("executionReport", order_fields());
    let account = (int(), prop::collection::vec(wallet_row(), 0..6))
        .prop_map(|(t, rows)| json!({ "e": "outboundAccountPosition", "E": t, "B": rows }));
    let inner = prop_oneof![4 => report, 2 => account, 1 => KIT.arb_json()].boxed();
    prop_oneof![
        3 => inner.clone(),
        3 => (small_id(), inner).prop_map(|(id, event)| json!({ "subscriptionId": id, "event": event })),
    ]
    .boxed()
}

fn order_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("symbol", token(TOKENS)),
        KIT.field("orderId", small_id()),
        KIT.field("clientOrderId", order_client_id()),
        KIT.field("side", side_v()),
        KIT.field("type", token(&["LIMIT", "MARKET", "STOP_MARKET", ""])),
        KIT.field("origQty", KIT.number_str()),
        KIT.field("executedQty", KIT.number_str()),
        KIT.field("cummulativeQuoteQty", KIT.number_str()),
        KIT.field("avgPrice", KIT.number_str()),
        KIT.field("price", KIT.number_str()),
        KIT.field(
            "status",
            token(&[
                "NEW",
                "FILLED",
                "CANCELED",
                "PENDING_CANCEL",
                "EXPIRED",
                "EXPIRED_IN_MATCH",
                "REJECTED",
                "PARTIALLY_FILLED",
            ]),
        ),
        KIT.field("updateTime", int()),
        KIT.field("time", int()),
    ])
}

fn fill_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("id", prop_oneof![small_id(), seq()].boxed()),
        KIT.field("orderId", small_id()),
        KIT.field("symbol", token(TOKENS)),
        KIT.field("qty", KIT.number_str()),
        KIT.field("price", KIT.number_str()),
        KIT.field("commission", KIT.number_str()),
        KIT.field("commissionAsset", token(TOKENS)),
        KIT.field("time", int()),
        KIT.field("isBuyer", bool_v()),
        KIT.field("isMaker", bool_v()),
        KIT.field("side", side_v()),
        KIT.field("maker", bool_v()),
        KIT.field("positionSide", token(&["BOTH", "LONG", "SHORT", ""])),
    ])
}

fn position_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("symbol", token(TOKENS)),
        KIT.field("positionSide", token(&["BOTH", "LONG", "SHORT", ""])),
        KIT.field("positionAmt", KIT.number_str()),
        KIT.field("entryPrice", KIT.number_str()),
        KIT.field("updateTime", int()),
        KIT.field("marginType", token(&["isolated", "cross", "ISOLATED", ""])),
        KIT.field("isolatedWallet", KIT.number_str()),
        KIT.field("isolatedMargin", KIT.number_str()),
    ])
}

fn balance_row() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("asset", token(TOKENS)),
        KIT.field("balance", KIT.number_str()),
        KIT.field("free", KIT.number_str()),
    ])
}

fn rate_body() -> BoxedStrategy<String> {
    let obj = object(vec![
        KIT.field("makerCommissionRate", KIT.number_str()),
        KIT.field("takerCommissionRate", KIT.number_str()),
        KIT.field(
            "commissionRates",
            object(vec![
                KIT.field("maker", KIT.number_str()),
                KIT.field("taker", KIT.number_str()),
            ]),
        ),
        KIT.field(
            "balances",
            prop::collection::vec(balance_row(), 0..4).prop_map(Value::Array).boxed(),
        ),
        KIT.field("uid", prop_oneof![seq(), KIT.arb_leaf()].boxed()),
    ]);
    prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => KIT.arb_text()].boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// USDⓈ-M user-data frames, both with the TRADE_LITE early-fill hint off and on.
    #[test]
    fn perp_mapper_is_total(frame in perp_frame(), early in any::<bool>()) {
        let events = map_perp_opts(&frame, "binance", "BTCUSDT.P", early, None);
        prop_assert!(events.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", events.len());
    }

    /// The Binance perp PUMP's mapper (`ALGO_UPDATE` here, the family mapper for everything else),
    /// fed algo updates and the order frames beside them: never a panic or an event flood.
    #[test]
    fn the_binance_perp_mapper_is_total_over_algo_updates_and_order_frames(
        frame in prop_oneof![4 => algo_update_frame(), 4 => perp_frame()],
        early in any::<bool>(),
    ) {
        let events = map_binance_perp_opts(&frame, "binance", "BTCUSDT.P", early, None);
        prop_assert!(events.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", events.len());
    }

    /// The open-algo-order reader production runs (the report path), over junk text, arrays of
    /// well-shaped rows, and one row alone, whose quantity must read finite.
    #[test]
    fn open_algo_order_readers_are_total(
        body in KIT.arb_body(algo_row()),
        row in prop_oneof![9 => algo_row(), 1 => KIT.arb_json()],
    ) {
        let _ = parse_perp_open_algo_orders(&body, "binance", None);
        let one = parse_perp_open_algo_orders(&json!([row]).to_string(), "binance", None);
        prop_assert!(one.iter().flatten().all(|r| r.qty.is_finite()));
    }

    /// Spot user-data frames with the REAL dispatch tokens (`mapper_property_tests.rs` feeds the same
    /// mappers random-string leaves that almost never spell `executionReport` / `TRADE`).
    #[test]
    fn spot_mapper_reaches_its_arms(frame in spot_frame()) {
        let a = map_private(&frame, "aster", "BTCUSDT", None);
        prop_assert!(a.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", a.len());
        let b = map_execution_report(&frame, "aster", "BTCUSDT", None);
        prop_assert!(b.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", b.len());
    }

    /// The audit-A3 resync replays: `allOrders` + `myTrades`/`userTrades` arrays.
    #[test]
    fn history_replay_is_total(
        orders in prop::collection::vec(prop_oneof![9 => order_row(), 1 => KIT.arb_json()], 0..6),
        trades in prop::collection::vec(prop_oneof![9 => fill_row(), 1 => KIT.arb_json()], 0..6),
        bodies_are_arrays in any::<bool>(),
    ) {
        let bound = 2 * orders.len() * trades.len() + orders.len();
        let (o, t) = if bodies_are_arrays {
            (Value::Array(orders), Value::Array(trades))
        } else {
            (json!({ "orders": orders }), json!({ "trades": trades }))
        };
        let spot = map_history(&o, &t, "binance", "BTCUSDT", None);
        prop_assert!(bodies_are_arrays || spot.is_empty());
        prop_assert!(spot.len() <= bound, "history flood: {} > {}", spot.len(), bound);
        let perp = map_perp_history(&o, &t, "binance", "BTCUSDT.P", None);
        prop_assert!(bodies_are_arrays || perp.is_empty());
        prop_assert!(perp.len() <= bound, "perp history flood: {} > {}", perp.len(), bound);
    }

    /// Every reconcile body parser over junk text and over arrays of well-shaped rows.
    #[test]
    fn reconcile_parsers_are_total(
        orders in KIT.arb_body(order_row()),
        fills in KIT.arb_body(fill_row()),
        positions in KIT.arb_body(position_row()),
        balances in KIT.arb_body(balance_row()),
        rates in rate_body(),
        junk in KIT.arb_text(),
    ) {
        for body in [&orders, &junk] {
            let _ = parse_spot_open_orders(body, "binance", None);
            let _ = parse_perp_open_orders(body, "binance", None);
        }
        for body in [&fills, &junk] {
            let _ = parse_spot_my_trades(body, "binance");
            let _ = parse_perp_user_trades(body, "binance");
        }
        for body in [&positions, &junk] {
            let _ = parse_perp_position_risk(body, "binance");
        }
        for body in [&balances, &rates, &junk] {
            let _ = parse_perp_balance(body);
            let _ = parse_spot_balance(body);
            let _ = parse_spot_fee_rates(body);
            let _ = parse_perp_fee_rates(body);
            let _ = parse_commission_rate_body(body);
        }
    }

    /// The row-level family readers: side is always +1 / -1.
    #[test]
    fn row_readers_are_total(row in prop_oneof![fill_row(), position_row(), KIT.arb_json()]) {
        let side = fill_side(&row);
        prop_assert!(side == 1 || side == -1);
        let _ = fill_is_maker(&row);
        let _ = parse_margin_type(&row);
    }

    /// `exchangeInfo` -> symbol grids, the spot / perp instrument maps and the account bodies
    /// that carry commission rates and the account uid.
    #[test]
    fn instrument_and_account_parsers_are_total(
        payload in exchange_info(),
        account in rate_body(),
    ) {
        let _ = parse_symbol_properties(&payload);
        let _ = parse_binance_perp_instruments(&payload);
        if let Ok(v) = serde_json::from_str::<Value>(&account) {
            let _ = parse_commission_rates(&v);
            let _ = parse_account_identity(&v);
        }
        let _ = parse_api_restrictions(&account);
    }

    /// `apiRestrictions` bodies, junk and structured.
    #[test]
    fn api_restrictions_are_total(
        body in KIT.arb_body(object(vec![
            KIT.field("enableWithdrawals", bool_v()),
            KIT.field("enableSpotAndMarginTrading", bool_v()),
            KIT.field("ipRestrict", bool_v()),
        ]))
    ) {
        let _ = parse_api_restrictions(&body);
    }

    /// The WS-API subscribe ack matcher.
    #[test]
    fn subscribe_ack_is_total(
        frame in prop_oneof![
            object(vec![
                KIT.field("id", Just(Value::String("vtr1".into())).boxed()),
                KIT.field("status", int()),
                KIT.field("error", object(vec![KIT.field("code", int()), KIT.field("msg", token(TOKENS))])),
            ]),
            KIT.arb_json(),
        ],
        request_id in prop_oneof![Just("vtr1".to_string()), any::<String>()],
    ) {
        let _ = match_subscribe_ack(&frame, &request_id);
    }

    /// The broker-prefix strip only ever returns a suffix of its input (so it can never
    /// panic on a char boundary or invent text).
    #[test]
    fn strip_broker_prefix_returns_a_suffix(raw in prop_oneof![any::<String>(), "x-[ -~]{0,8}-[ -~]{0,8}"]) {
        let out = strip_broker_order_client_id_prefix(&raw, None);
        prop_assert!(raw.ends_with(out));
    }

    /// `parse_perp_open_orders` (the report path production runs) over one row whose quantity may
    /// be any number the wire can spell (`"NaN"` / `"inf"` included - see the named cases below).
    #[test]
    fn perp_open_order_is_total_over_rows(
        row in object(vec![
            KIT.field("symbol", token(TOKENS)),
            KIT.field("orderId", small_id()),
            KIT.field("clientOrderId", order_client_id()),
            KIT.field("side", side_v()),
            KIT.field("type", token(&["LIMIT", "MARKET", ""])),
            KIT.field("origQty", numeric_string().prop_map(Value::String).boxed()),
            KIT.field("price", KIT.number_str()),
        ])
    ) {
        let _ = parse_perp_open_orders(&json!([row]).to_string(), "binance", None);
    }

    /// A non-finite quantity (`"NaN"`, `"inf"`, `"1e999"`), read through `json_number`, never panics
    /// the report path. (It panicked the legacy snapshot's row map once, `.expect("static shape")`
    /// on a `null` `qty`; that map is deleted.)
    #[test]
    fn perp_open_order_non_finite_quantity_does_not_panic(
        qty in token(&["NaN", "inf", "-inf", "1e999", "-1e999"])
    ) {
        let row = json!({ "symbol": "BTCUSDT", "side": "BUY", "type": "LIMIT", "origQty": qty });
        let _ = parse_perp_open_orders(&json!([row]).to_string(), "binance", None);
    }
}
