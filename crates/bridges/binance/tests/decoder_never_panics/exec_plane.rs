//! The exec-plane decoders (user-data mappers, history, reconcile parsers), fed arbitrary input.

use super::*;
use vike_binance::family::event_mapper::{map_execution_report, map_private};
use vike_binance::family::history::{map_history, map_perp_history};
use vike_binance::family::order_map::{
    map_perp_open_order, parse_symbol_properties, strip_broker_coid_prefix,
};
use vike_binance::family::perp_mapper::map_perp_opts;
use vike_binance::family::recon::{
    fill_is_maker, fill_side, parse_commission_rate_body, parse_margin_type, parse_perp_balance,
    parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk, parse_perp_user_trades,
    parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades, parse_spot_open_orders,
};
use vike_binance::key_permissions::parse_api_restrictions;
use vike_binance::perp::parse_binance_perp_instruments;
use vike_binance::spot::{parse_account_identity, parse_commission_rates};
use vike_binance::ws_auth::match_subscribe_ack;

/// A small order id spelled as a number or a string, so grouped REST rows actually match.
fn small_id() -> BoxedStrategy<Value> {
    prop_oneof![(0i64..4).prop_map(|n| json!(n)), (0i64..4).prop_map(|n| json!(n.to_string()))]
        .boxed()
}

fn coid() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => tok(TOKENS),
        2 => "[ -~]{0,14}".prop_map(Value::String),
        1 => Just(Value::String("x-AB-12-".into())),
        1 => Just(Value::String("x--".into())),
    ]
    .boxed()
}

fn side_v() -> BoxedStrategy<Value> {
    tok(&["BUY", "SELL", "buy", ""])
}

fn bool_v() -> BoxedStrategy<Value> {
    any::<bool>().prop_map(Value::Bool).boxed()
}

/// The `o` object of an `ORDER_TRADE_UPDATE`, or the body of a spot `executionReport`.
fn order_fields() -> Vec<Field> {
    vec![
        field("s", tok(TOKENS)),
        field("c", coid()),
        field("C", coid()),
        field(
            "x",
            tok(&["NEW", "CANCELED", "EXPIRED", "REJECTED", "TRADE", "CALCULATED", "AMENDMENT"]),
        ),
        field("X", tok(&["NEW", "FILLED", "PARTIALLY_FILLED", "CANCELED"])),
        field("S", side_v()),
        field("r", tok(TOKENS)),
        field("i", seq()),
        field("t", prop_oneof![seq(), numeric_string().prop_map(Value::String)].boxed()),
        field("l", num_str()),
        field("L", num_str()),
        field("n", num_str()),
        field("N", tok(TOKENS)),
        field("m", bool_v()),
        field("ps", tok(&["BOTH", "LONG", "SHORT", ""])),
    ]
}

fn with_e(e: &'static str, mut fields: Vec<Field>) -> BoxedStrategy<Value> {
    fields.push(field("e", Just(Value::String(e.into())).boxed()));
    fields.push(field("T", int()));
    fields.push(field("E", int()));
    object(fields)
}

fn wallet_row() -> BoxedStrategy<Value> {
    object(vec![
        field("a", tok(TOKENS)),
        field("wb", prop_oneof![num_str(), arb_leaf()].boxed()),
        field("bc", num_str()),
        field("f", num_str()),
        field("l", num_str()),
    ])
}

/// A USDⓈ-M user-data frame: ORDER_TRADE_UPDATE, ACCOUNT_UPDATE, flat TRADE_LITE, or noise.
fn perp_frame() -> BoxedStrategy<Value> {
    let otu = (int(), object(order_fields()))
        .prop_map(|(t, o)| json!({ "e": "ORDER_TRADE_UPDATE", "T": t, "o": o }));
    let account = (
        int(),
        tok(&["FUNDING_FEE", "ORDER", "DEPOSIT", ""]),
        prop::collection::vec(wallet_row(), 0..6),
    )
        .prop_map(
            |(t, m, rows)| json!({ "e": "ACCOUNT_UPDATE", "T": t, "a": { "m": m, "B": rows } }),
        );
    prop_oneof![
        4 => otu,
        2 => account,
        3 => with_e("TRADE_LITE", order_fields()),
        1 => arb_json(),
    ]
    .boxed()
}

/// A spot user-data frame, bare or inside the WS-API `{"event":…}` envelope.
fn spot_frame() -> BoxedStrategy<Value> {
    let report = with_e("executionReport", order_fields());
    let account = (int(), prop::collection::vec(wallet_row(), 0..6))
        .prop_map(|(t, rows)| json!({ "e": "outboundAccountPosition", "E": t, "B": rows }));
    let inner = prop_oneof![4 => report, 2 => account, 1 => arb_json()].boxed();
    prop_oneof![
        3 => inner.clone(),
        3 => (small_id(), inner).prop_map(|(id, ev)| json!({ "subscriptionId": id, "event": ev })),
    ]
    .boxed()
}

fn order_row() -> BoxedStrategy<Value> {
    object(vec![
        field("symbol", tok(TOKENS)),
        field("orderId", small_id()),
        field("clientOrderId", coid()),
        field("side", side_v()),
        field("type", tok(&["LIMIT", "MARKET", "STOP_MARKET", ""])),
        field("origQty", num_str()),
        field("executedQty", num_str()),
        field("cummulativeQuoteQty", num_str()),
        field("avgPrice", num_str()),
        field("price", num_str()),
        field(
            "status",
            tok(&[
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
        field("updateTime", int()),
        field("time", int()),
    ])
}

fn fill_row() -> BoxedStrategy<Value> {
    object(vec![
        field("id", prop_oneof![small_id(), seq()].boxed()),
        field("orderId", small_id()),
        field("symbol", tok(TOKENS)),
        field("qty", num_str()),
        field("price", num_str()),
        field("commission", num_str()),
        field("commissionAsset", tok(TOKENS)),
        field("time", int()),
        field("isBuyer", bool_v()),
        field("isMaker", bool_v()),
        field("side", side_v()),
        field("maker", bool_v()),
        field("positionSide", tok(&["BOTH", "LONG", "SHORT", ""])),
    ])
}

fn position_row() -> BoxedStrategy<Value> {
    object(vec![
        field("symbol", tok(TOKENS)),
        field("positionSide", tok(&["BOTH", "LONG", "SHORT", ""])),
        field("positionAmt", num_str()),
        field("entryPrice", num_str()),
        field("updateTime", int()),
        field("marginType", tok(&["isolated", "cross", "ISOLATED", ""])),
        field("isolatedWallet", num_str()),
        field("isolatedMargin", num_str()),
    ])
}

fn balance_row() -> BoxedStrategy<Value> {
    object(vec![field("asset", tok(TOKENS)), field("balance", num_str()), field("free", num_str())])
}

fn rate_body() -> BoxedStrategy<String> {
    let obj = object(vec![
        field("makerCommissionRate", num_str()),
        field("takerCommissionRate", num_str()),
        field(
            "commissionRates",
            object(vec![field("maker", num_str()), field("taker", num_str())]),
        ),
        field(
            "balances",
            prop::collection::vec(balance_row(), 0..4).prop_map(Value::Array).boxed(),
        ),
        field("uid", prop_oneof![seq(), arb_leaf()].boxed()),
    ]);
    prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => arb_text()].boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// USDⓈ-M user-data frames, both with the TRADE_LITE early-fill hint off and on.
    #[test]
    fn perp_mapper_is_total(frame in perp_frame(), early in any::<bool>()) {
        let events = map_perp_opts(&frame, "binance", "BTCUSDT.P", early);
        prop_assert!(events.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", events.len());
    }

    /// Spot user-data frames with the REAL dispatch tokens (`mapper_props.rs` feeds the same
    /// mappers random-string leaves that almost never spell `executionReport` / `TRADE`).
    #[test]
    fn spot_mapper_reaches_its_arms(frame in spot_frame()) {
        let a = map_private(&frame, "aster", "BTCUSDT");
        prop_assert!(a.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", a.len());
        let b = map_execution_report(&frame, "aster", "BTCUSDT");
        prop_assert!(b.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", b.len());
    }

    /// The audit-A3 resync replays: `allOrders` + `myTrades`/`userTrades` arrays.
    #[test]
    fn history_replay_is_total(
        orders in prop::collection::vec(prop_oneof![9 => order_row(), 1 => arb_json()], 0..6),
        trades in prop::collection::vec(prop_oneof![9 => fill_row(), 1 => arb_json()], 0..6),
        bodies_are_arrays in any::<bool>(),
    ) {
        let bound = 2 * orders.len() * trades.len() + orders.len();
        let (o, t) = if bodies_are_arrays {
            (Value::Array(orders), Value::Array(trades))
        } else {
            (json!({ "orders": orders }), json!({ "trades": trades }))
        };
        let spot = map_history(&o, &t, "binance", "BTCUSDT");
        prop_assert!(bodies_are_arrays || spot.is_empty());
        prop_assert!(spot.len() <= bound, "history flood: {} > {}", spot.len(), bound);
        let perp = map_perp_history(&o, &t, "binance", "BTCUSDT.P");
        prop_assert!(bodies_are_arrays || perp.is_empty());
        prop_assert!(perp.len() <= bound, "perp history flood: {} > {}", perp.len(), bound);
    }

    /// Every reconcile body parser over junk text and over arrays of well-shaped rows.
    #[test]
    fn recon_parsers_are_total(
        orders in arb_body(order_row()),
        fills in arb_body(fill_row()),
        positions in arb_body(position_row()),
        balances in arb_body(balance_row()),
        rates in rate_body(),
        junk in arb_text(),
    ) {
        for body in [&orders, &junk] {
            let _ = parse_spot_open_orders(body, "binance");
            let _ = parse_perp_open_orders(body, "binance");
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
    fn row_readers_are_total(row in prop_oneof![fill_row(), position_row(), arb_json()]) {
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
        body in arb_body(object(vec![
            field("enableWithdrawals", bool_v()),
            field("enableSpotAndMarginTrading", bool_v()),
            field("ipRestrict", bool_v()),
        ]))
    ) {
        let _ = parse_api_restrictions(&body);
    }

    /// The WS-API subscribe ack matcher.
    #[test]
    fn subscribe_ack_is_total(
        frame in prop_oneof![
            object(vec![
                field("id", Just(Value::String("vtr1".into())).boxed()),
                field("status", int()),
                field("error", object(vec![field("code", int()), field("msg", tok(TOKENS))])),
            ]),
            arb_json(),
        ],
        req_id in prop_oneof![Just("vtr1".to_string()), any::<String>()],
    ) {
        let _ = match_subscribe_ack(&frame, &req_id);
    }

    /// The broker-prefix strip only ever returns a suffix of its input (so it can never
    /// panic on a char boundary or invent text).
    #[test]
    fn strip_broker_prefix_returns_a_suffix(raw in prop_oneof![any::<String>(), "x-[ -~]{0,8}-[ -~]{0,8}"]) {
        let out = strip_broker_coid_prefix(&raw);
        prop_assert!(raw.ends_with(out));
    }

    /// `map_perp_open_order` over rows whose quantity may be any number the wire can spell
    /// (`"NaN"` / `"inf"` included - see the named cases below).
    #[test]
    fn perp_open_order_is_total_over_rows(
        row in object(vec![
            field("symbol", tok(TOKENS)),
            field("orderId", small_id()),
            field("clientOrderId", coid()),
            field("side", side_v()),
            field("type", tok(&["LIMIT", "MARKET", ""])),
            field("origQty", numeric_string().prop_map(Value::String).boxed()),
            field("price", num_str()),
        ])
    ) {
        let _ = map_perp_open_order(&row, "binance");
    }

    /// REGRESSION: a non-finite quantity (`"NaN"`, `"inf"`, `"1e999"`) is read through `json_num`,
    /// and `serde_json` serializes a non-finite `f64` as JSON `null`, which the non-optional
    /// `qty: f64` of the `OrderRequest` round trip refused - an `.expect("static shape")` panic.
    /// The mapper now reads such a quantity as `0.0`, like an unparseable one.
    #[test]
    fn perp_open_order_non_finite_quantity_does_not_panic(
        qty in tok(&["NaN", "inf", "-inf", "1e999", "-1e999"])
    ) {
        let row = json!({ "symbol": "BTCUSDT", "side": "BUY", "type": "LIMIT", "origQty": qty });
        let _ = map_perp_open_order(&row, "binance");
    }
}
