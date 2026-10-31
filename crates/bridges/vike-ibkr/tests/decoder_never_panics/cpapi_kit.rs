//! The cpapi strategy kit of the decoder harness: field-name pools and the `arb_*` generators.

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_ibkr::cpapi_feed::decode::SMD_FIELDS;

use crate::arb_i64;

// ---- field names the decoders actually read ---------------------------------------------------

pub const WS_KEYS: &[&str] = &[
    "topic",
    "args",
    "orders",
    "cOID",
    "execId",
    "symbol",
    "side",
    "cumFill",
    "price",
    "ts",
    "commission",
    "currency",
    "status",
    "filledQuantity",
    "avgPrice",
    "orderId",
    "conid",
];
pub const RECON_KEYS: &[&str] = &[
    "orders",
    "conid",
    "orderId",
    "order_ref",
    "cOID",
    "side",
    "orderType",
    "totalSize",
    "filledQuantity",
    "avgPrice",
    "avgCost",
    "status",
    "lastExecutionTime_r",
    "execution_id",
    "size",
    "price",
    "commission",
    "commission_currency",
    "trade_time_r",
    "position",
    "cashbalance",
    "USD",
    "BASE",
];

// ---- generators -------------------------------------------------------------------------------

/// The numeric strings IBKR (and the CP Gateway) hand back for prices and sizes.
fn arb_numeric_string() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "0",
        "-0",
        "0.5",
        "1",
        "190.25",
        "1e3",
        "NaN",
        "inf",
        "-inf",
        "1e999",
        "",
        " ",
        "9223372036854775807",
        "-9223372036854775808",
        "99999999999999999999999999999999999999",
        "0x10",
    ])
    .prop_map(|s| s.to_string())
}

/// The words the decoders dispatch on.
pub fn arb_domain_word() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "sor",
        "str",
        "Submitted",
        "PreSubmitted",
        "PendingSubmit",
        "PendingCancel",
        "ApiPending",
        "Filled",
        "Cancelled",
        "ApiCancelled",
        "Inactive",
        "BUY",
        "SELL",
        "B",
        "S",
        "BOT",
        "SLD",
        "USD",
        "STK",
        "FUT",
        "OPT",
        "CASH",
        "CRYPTO",
        "true",
        "false",
    ])
    .prop_map(|s| s.to_string())
}

fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        arb_i64().prop_map(|n| Value::Number(n.into())),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        arb_numeric_string().prop_map(Value::String),
        "[ -~]{0,12}".prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        arb_domain_word().prop_map(Value::String),
        prop::sample::select(vec![json!([]), json!({})]),
    ]
    .boxed()
}

fn arb_key(names: &'static [&'static str]) -> impl Strategy<Value = String> {
    prop_oneof![
        9 => prop::sample::select(names.to_vec()).prop_map(|s| s.to_string()),
        1 => "[a-zA-Z_]{1,8}",
    ]
}

/// Arbitrary JSON, depth <= 4, objects keyed from `names`.
pub fn arb_json(names: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(names), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// The first half of `s` by CHARACTERS (a byte slice could split a code point in the harness
/// itself).
fn truncate_half(s: String) -> String {
    let keep = s.chars().count() / 2;
    s.chars().take(keep).collect()
}

/// A raw body: arbitrary unicode, lossy-decoded byte noise, valid JSON, truncated JSON.
pub fn arb_text(names: &'static [&'static str]) -> impl Strategy<Value = String> {
    prop_oneof![
        any::<String>(),
        prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
        arb_json(names).prop_map(|v| v.to_string()),
        arb_json(names).prop_map(|v| truncate_half(v.to_string())),
    ]
}

/// A number as the gateway spells it: a JSON number, a hostile numeric string, or any leaf.
fn arb_num() -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        2 => arb_i64().prop_map(|n| json!(n)),
        2 => arb_numeric_string().prop_map(Value::String),
        1 => arb_leaf(),
    ]
}

/// A short identifier from a small pool (so ids collide across rows), the empty string, or any
/// leaf.
fn arb_id() -> impl Strategy<Value = Value> {
    prop_oneof![
        4 => prop::sample::select(vec!["c-1", "c-2", "e1", "e2", "265598", "USD", ""])
            .prop_map(|s| json!(s)),
        1 => arb_leaf(),
    ]
}

fn arb_word() -> impl Strategy<Value = Value> {
    prop_oneof![
        4 => arb_domain_word().prop_map(Value::String),
        1 => arb_leaf(),
    ]
}

/// An execution row of the `sor` frame / `/iserver/account/trades` (carries `execId`).
fn arb_exec_row() -> impl Strategy<Value = Value> {
    (
        arb_id(),
        arb_id(),
        arb_word(),
        arb_num(),
        arb_num(),
        (arb_num(), arb_id(), prop::option::of(arb_num())),
    )
        .prop_map(|(coid, exec_id, side, fill, price, (commission, currency, ts))| {
            let mut o = json!({
                "cOID": coid, "execId": exec_id, "symbol": "AAPL", "side": side,
                "cumFill": fill, "price": price, "commission": commission,
                "currency": currency
            });
            if let Some(ts) = ts {
                o["ts"] = ts;
            }
            o
        })
}

/// A status row of the `sor` frame (carries `status`).
fn arb_status_row() -> impl Strategy<Value = Value> {
    (arb_id(), arb_word(), arb_num(), arb_num()).prop_map(|(coid, status, filled, avg)| {
        json!({
            "cOID": coid, "status": status, "filledQuantity": filled, "avgPrice": avg
        })
    })
}

pub fn arb_ws_row() -> impl Strategy<Value = Value> {
    prop_oneof![
        4 => arb_exec_row(),
        4 => arb_status_row(),
        2 => arb_json(WS_KEYS),
    ]
}

/// A WS frame: usually topic `sor` with an `args` array of rows, sometimes another topic or a
/// non-array `args`.
pub fn arb_ws_frame() -> impl Strategy<Value = Value> {
    prop_oneof![
        8 => (
            prop_oneof![
                6 => Just(json!("sor")),
                1 => Just(json!("str")),
                1 => arb_leaf(),
            ],
            prop::collection::vec(arb_ws_row(), 0..5),
        )
            .prop_map(|(topic, rows)| json!({ "topic": topic, "args": rows })),
        1 => (Just(json!("sor")), arb_leaf())
            .prop_map(|(topic, args)| json!({ "topic": topic, "args": args })),
        3 => arb_json(WS_KEYS),
    ]
}

// ---- reconcile bodies -------------------------------------------------------------------------

pub fn arb_conid() -> impl Strategy<Value = Value> {
    prop_oneof![
        5 => Just(json!(265_598)),
        2 => Just(json!("265598")),
        1 => arb_leaf(),
    ]
}

fn arb_order_row() -> impl Strategy<Value = Value> {
    (
        arb_conid(),
        arb_id(),
        arb_id(),
        arb_word(),
        arb_word(),
        (arb_num(), arb_num(), arb_num(), arb_word(), arb_num()),
    )
        .prop_map(
            |(conid, order_id, order_ref, side, kind, (total, filled, avg, status, ts))| {
                json!({
                    "conid": conid, "orderId": order_id, "order_ref": order_ref,
                    "side": side, "orderType": kind, "totalSize": total,
                    "filledQuantity": filled, "avgPrice": avg, "status": status,
                    "lastExecutionTime_r": ts
                })
            },
        )
}

pub fn arb_trade_row() -> impl Strategy<Value = Value> {
    (
        arb_conid(),
        arb_id(),
        arb_id(),
        arb_word(),
        (arb_num(), arb_num(), arb_num(), arb_id(), arb_num()),
    )
        .prop_map(|(conid, exec_id, order_id, side, (size, price, commission, cur, ts))| {
            json!({
                "conid": conid, "execution_id": exec_id, "orderId": order_id, "side": side,
                "size": size, "price": price, "commission": commission,
                "commission_currency": cur, "trade_time_r": ts
            })
        })
}

pub fn arb_position_row() -> impl Strategy<Value = Value> {
    (arb_conid(), arb_num(), prop::option::of(arb_num()), arb_num()).prop_map(
        |(conid, position, avg_price, avg_cost)| {
            let mut o = json!({ "conid": conid, "position": position, "avgCost": avg_cost });
            if let Some(a) = avg_price {
                o["avgPrice"] = a;
            }
            o
        },
    )
}

/// The `/iserver/account/orders` body, the `/trades` and `/positions` arrays, the `/ledger`
/// object — as shaped JSON text, or as arbitrary body text.
pub fn arb_orders_body() -> impl Strategy<Value = String> {
    prop_oneof![
        5 => prop::collection::vec(arb_order_row(), 0..4)
            .prop_map(|rows| json!({ "orders": rows, "snapshot": true }).to_string()),
        2 => arb_text(RECON_KEYS),
    ]
}

pub fn arb_array_body(
    row: impl Strategy<Value = Value> + 'static,
) -> impl Strategy<Value = String> {
    prop_oneof![
        5 => prop::collection::vec(row, 0..4).prop_map(|rows| Value::Array(rows).to_string()),
        2 => arb_text(RECON_KEYS),
    ]
}

pub fn arb_ledger_body() -> impl Strategy<Value = String> {
    prop_oneof![
        5 => (arb_num(), arb_num(), arb_num()).prop_map(|(usd, base, other)| json!({
            "USD": { "cashbalance": usd, "netliquidationvalue": 1, "settledcash": 2 },
            "BASE": { "cashbalance": base },
            "EUR": other
        })
        .to_string()),
        2 => arb_text(RECON_KEYS),
    ]
}

pub fn arb_conid_param() -> impl Strategy<Value = i64> {
    prop_oneof![Just(265_598i64), Just(0i64), arb_i64()]
}

/// A field value as the gateway may spell it: a number, a prefixed/separated/suffixed numeric
/// string, an availability code, or any leaf.
pub fn arb_md_value() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => arb_num(),
        2 => prop::sample::select(vec![
            "C193.18", "H1", "C", "H", "1,100", "1.2K", "3M", "2B", "K", "-5", "DPB", "RpB",
            "Z", "Y", "N", "", "é", "1,,0", "C-0",
        ])
        .prop_map(|s| json!(s)),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// An `smd` row: the envelope plus a random subset of the field codes.
pub fn arb_smd_row() -> impl Strategy<Value = Value> {
    (
        prop_oneof![
            Just(json!("smd+265598")),
            Just(json!("smd+")),
            Just(json!("smd+x+y")),
            Just(json!("smd")),
            arb_leaf(),
        ],
        prop::collection::vec((prop::sample::select(SMD_FIELDS.to_vec()), arb_md_value()), 0..8),
        arb_i64(),
        arb_conid(),
    )
        .prop_map(|(topic, fields, updated, conid)| {
            let mut row = serde_json::Map::new();
            row.insert("topic".into(), topic);
            row.insert("_updated".into(), json!(updated));
            row.insert("conid".into(), conid);
            for (k, v) in fields {
                row.insert(k.to_string(), v);
            }
            Value::Object(row)
        })
}

/// A history body: shaped bar rows plus the two delay fields.
pub fn arb_history_body() -> impl Strategy<Value = Value> {
    (
        prop::collection::vec(
            prop::collection::vec(
                (prop::sample::select(vec!["t", "o", "h", "l", "c", "v"]), arb_md_value()),
                0..7,
            )
            .prop_map(|kvs| {
                Value::Object(kvs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
            }),
            0..6,
        ),
        arb_md_value(),
        arb_md_value(),
    )
        .prop_map(|(rows, avail, delay)| {
            json!({ "data": rows, "mdAvailability": avail, "mktDataDelay": delay })
        })
}
