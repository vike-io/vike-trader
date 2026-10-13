//! "Arbitrary input never panics" harness for every PUBLIC Alpaca wire decoder:
//!
//! - `vike_alpaca::data::decode_ws_message` — one market-data WS frame (a JSON array of
//!   `T`/`S`-tagged bar/quote/trade/control objects);
//! - `vike_alpaca::decode_trade_event` — one `/v2/events/trades` SSE `data:` JSON object (the
//!   exec event lane), including its RFC3339 `timestamp` parse;
//! - `vike_alpaca::map_order_response` — the order-POST response body;
//! - `vike_alpaca::recon_client::{parse_orders, parse_fills, parse_positions, parse_balance}` —
//!   the reconcile report parsers over a raw REST body;
//! - `vike_alpaca::parse_assets` — the `/v1/assets` catalog payload.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing (or an
//! `Err`), but it must never panic the pump / SSE thread (a dead thread is a venue that silently
//! goes quiet), and it must never fabricate an event flood. Each decoder is fed (a) arbitrary
//! text, (b) lossy-decoded byte noise, and (c) structured JSON whose object keys are the decoder's
//! REAL field names, so the generator reaches the branches instead of bouncing off the first
//! `.get()`. Outputs are not asserted beyond the cheap bounds that must always hold.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_alpaca::data::decode_ws_message;
use vike_alpaca::recon_client::{parse_balance, parse_fills, parse_orders, parse_positions};
use vike_alpaca::{decode_trade_event, map_order_response, parse_assets};

/// Real Alpaca field names across every decoder above (WS `T`/`S`/OHLC/quote/trade keys, the SSE
/// event + nested `order`, the REST report rows, the asset catalog row).
const KEYS: &[&str] = &[
    "T",
    "S",
    "t",
    "o",
    "h",
    "l",
    "c",
    "v",
    "bp",
    "ap",
    "bs",
    "as",
    "p",
    "s",
    "msg",
    "code",
    "event",
    "event_id",
    "order",
    "execution_id",
    "timestamp",
    "position_qty",
    "id",
    "client_order_id",
    "updated_at",
    "symbol",
    "qty",
    "filled_qty",
    "filled_avg_price",
    "order_type",
    "type",
    "side",
    "status",
    "activity_type",
    "order_id",
    "transaction_time",
    "price",
    "avg_entry_price",
    "cash",
    "class",
    "tradable",
    "name",
    "message",
];

/// Real dispatch values: the WS `T` verbs, the SSE `event` verbs, order sides/statuses and the
/// symbols the parsers filter on.
const WORDS: &[&str] = &[
    "b",
    "q",
    "t",
    "success",
    "error",
    "subscription",
    "fill",
    "partial_fill",
    "canceled",
    "expired",
    "rejected",
    "new",
    "accepted",
    "pending_new",
    "filled",
    "partially_filled",
    "buy",
    "sell",
    "long",
    "short",
    "FILL",
    "us_equity",
    "crypto",
    "AAPL",
    "BTC/USD",
    "BTCUSD",
    "/",
    "",
];

/// The WS `T` message types: bar, quote, trade and the control acks.
const TAGS: &[&str] = &["b", "q", "t", "success", "error", "subscription"];

/// The (wire, vike) symbol pairs the report parsers are mounted with.
const SYMBOLS: &[(&str, &str)] = &[("AAPL", "AAPL"), ("BTC/USD", "BTCUSD"), ("", ""), ("/", "/")];

/// An RFC3339-ish timestamp with every field free: years of up to 12 digits, out-of-range
/// month/day/hour, 12-digit fractions, assorted suffixes.
fn arb_ts() -> impl Strategy<Value = String> {
    "[0-9]{1,12}-[0-9]{1,2}-[0-9]{1,2}[T ][0-9]{1,2}:[0-9]{1,2}:[0-9]{1,2}(\\.[0-9]{1,12})?(Z|\\+[0-9]{2}:[0-9]{2})?"
}

fn arb_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        prop_oneof![Just(0i64), Just(-1), Just(i64::MAX), Just(i64::MIN), any::<i64>()]
            .prop_map(Value::from),
        Just(Value::from(u64::MAX)),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        prop::sample::select(vec![
            "NaN",
            "inf",
            "-inf",
            "-0",
            "0",
            "1e999",
            "-1e999",
            "1e-999",
            "",
            " ",
            ".",
            "-",
            "0x10",
            "9999999999999999999999999999999999999999",
            "-9999999999999999999999999999999999999999",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        "-?[0-9]{1,40}(\\.[0-9]{1,40})?".prop_map(Value::String),
        prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
        arb_ts().prop_map(Value::String),
        any::<String>().prop_map(Value::String),
        Just(Value::Array(Vec::new())),
        Just(Value::Object(Map::new())),
    ]
}

/// Arbitrary JSON, depth <= 4, object keys drawn mostly from the real field names.
fn arb_json() -> impl Strategy<Value = Value> {
    let key = prop_oneof![
        8 => prop::sample::select(KEYS).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ];
    arb_leaf().prop_recursive(4, 64, 6, move |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((key.clone(), inner), 0..8)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

/// Raw bytes -> text the way a lossy socket read would produce it.
fn arb_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<u8>(), 0..512)
        .prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// A plausible SSE trade-update object: the fields `decode_trade_event` reads at the top level,
/// with `order` a free object, every value drawn from the hostile leaf set.
fn arb_trade_event() -> impl Strategy<Value = Value> {
    (
        prop_oneof![
            3 => prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
            1 => arb_leaf(),
        ],
        arb_json(),
        arb_leaf(),
        arb_leaf(),
        arb_leaf(),
        arb_leaf(),
    )
        .prop_map(|(event, order, execution_id, timestamp, qty, price)| {
            let mut m = Map::new();
            m.insert("event".into(), event);
            m.insert("order".into(), order);
            m.insert("execution_id".into(), execution_id);
            m.insert("timestamp".into(), timestamp);
            m.insert("qty".into(), qty);
            m.insert("price".into(), price);
            Value::Object(m)
        })
}

/// A plausible WS element: a `T`/`S`-tagged object with the OHLC / quote / trade keys hostile.
fn arb_ws_element() -> impl Strategy<Value = Value> {
    (
        prop::sample::select(TAGS),
        prop_oneof![
            3 => prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
            1 => arb_leaf(),
        ],
        prop::collection::vec(
            (prop::sample::select(KEYS).prop_map(String::from), arb_leaf()),
            0..10,
        ),
    )
        .prop_map(|(tag, symbol, fields)| {
            let mut m = Map::new();
            for (k, v) in fields {
                m.insert(k, v);
            }
            m.insert("T".into(), Value::String(tag.to_string()));
            m.insert("S".into(), symbol);
            Value::Object(m)
        })
}

/// A report body: a JSON array of free rows (the shape every `parse_*` expects), so the row
/// closures run rather than the "expected a JSON array" early return.
fn arb_rows() -> impl Strategy<Value = Value> {
    prop::collection::vec(arb_json(), 0..6).prop_map(Value::Array)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // ---- market-data WS frame ---------------------------------------------------------------

    /// (a) `decode_ws_message` is total over arbitrary text.
    #[test]
    fn ws_message_survives_arbitrary_text(text in any::<String>(), prefix in any::<char>()) {
        let _ = decode_ws_message(prefix, &text);
    }

    /// (a') ...and over byte noise.
    #[test]
    fn ws_message_survives_byte_noise(text in arb_noise()) {
        let _ = decode_ws_message('c', &text);
    }

    /// (b) ...and over arbitrary JSON; one frame never yields more messages than it has elements.
    #[test]
    fn ws_message_survives_structured_json(v in arb_json()) {
        let out = decode_ws_message('s', &v.to_string());
        if let Value::Array(items) = &v {
            prop_assert!(out.len() <= items.len(), "{} msgs from {} elements", out.len(), items.len());
        }
    }

    /// (b') ...and over arrays of T/S-tagged objects (reaches the bar/quote/trade arms).
    #[test]
    fn ws_message_survives_tagged_elements(items in prop::collection::vec(arb_ws_element(), 0..8)) {
        let n = items.len();
        let out = decode_ws_message('c', &Value::Array(items).to_string());
        prop_assert!(out.len() <= n, "{} msgs from {n} elements", out.len());
    }

    /// (c) A short run of frames through the decoder never panics (it is stateless; this pins
    /// that a poisoned frame leaves no residue behind that breaks the next one).
    #[test]
    fn ws_message_survives_a_frame_sequence(
        frames in prop::collection::vec(prop_oneof![
            arb_ws_element().prop_map(|e| Value::Array(vec![e]).to_string()),
            arb_json().prop_map(|v| v.to_string()),
            any::<String>(),
        ], 1..8),
    ) {
        for f in &frames {
            let _ = decode_ws_message('c', f);
        }
    }

    // ---- SSE trade-update object ------------------------------------------------------------

    /// `decode_trade_event` is total over arbitrary JSON; one frame emits at most the dual-publish
    /// pair (Fill + OrderFilled).
    #[test]
    fn trade_event_survives_structured_json(v in arb_json()) {
        let out = decode_trade_event(&v);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
    }

    /// ...and over objects shaped like a real trade update, so the fill / cancel / accept arms and
    /// the RFC3339 timestamp parse run on hostile fields.
    #[test]
    fn trade_event_survives_shaped_objects(v in arb_trade_event()) {
        let out = decode_trade_event(&v);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
    }

    /// ...and over whatever text parses as JSON, the way the SSE loop feeds it.
    #[test]
    fn trade_event_survives_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let out = decode_trade_event(&v);
            prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        }
    }

    /// (c) A run of SSE frames, one after another, never panics.
    #[test]
    fn trade_event_survives_a_frame_sequence(
        frames in prop::collection::vec(prop_oneof![arb_trade_event(), arb_json()], 1..8),
    ) {
        for f in &frames {
            let out = decode_trade_event(f);
            prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        }
    }

    // ---- order-POST response ----------------------------------------------------------------

    /// `map_order_response` always answers with exactly one event (accepted or rejected).
    #[test]
    fn order_response_survives_structured_json(
        coid in any::<String>(),
        ts in any::<i64>(),
        v in arb_json(),
    ) {
        let out = map_order_response(&coid, ts, &v);
        prop_assert_eq!(out.len(), 1);
    }

    // ---- reconcile report parsers (raw REST body) -------------------------------------------

    /// All four report parsers are total over arbitrary text and byte noise.
    #[test]
    fn recon_parsers_survive_arbitrary_text(
        text in prop_oneof![any::<String>(), arb_noise()],
        sym in prop::sample::select(SYMBOLS),
    ) {
        let _ = parse_orders(&text, sym.0, sym.1);
        let _ = parse_fills(&text, sym.0, sym.1);
        let _ = parse_positions(&text, sym.0, sym.1);
        let _ = parse_balance(&text);
    }

    /// ...and over arrays of free rows / arbitrary JSON; the position parser always answers at
    /// least one row (it synthesizes a flat one) and the others never invent rows.
    #[test]
    fn recon_parsers_survive_structured_json(
        rows in arb_rows(),
        any_json in arb_json(),
        sym in prop::sample::select(SYMBOLS),
    ) {
        let n = rows.as_array().map_or(0, Vec::len);
        let body = rows.to_string();
        if let Ok(o) = parse_orders(&body, sym.0, sym.1) {
            prop_assert!(o.len() <= n);
        }
        if let Ok(f) = parse_fills(&body, sym.0, sym.1) {
            prop_assert!(f.len() <= n);
        }
        if let Ok(p) = parse_positions(&body, sym.0, sym.1) {
            prop_assert!(!p.is_empty());
        }
        let free = any_json.to_string();
        let _ = parse_balance(&free);
        let _ = parse_orders(&free, sym.0, sym.1);
        let _ = parse_fills(&free, sym.0, sym.1);
        let _ = parse_positions(&free, sym.0, sym.1);
    }

    // ---- /v1/assets catalog -----------------------------------------------------------------

    /// `parse_assets` is total over arbitrary JSON and arrays of free rows, and emits no more
    /// instruments than the payload has rows.
    #[test]
    fn assets_survive_structured_json(rows in arb_rows(), v in arb_json()) {
        let n = rows.as_array().map_or(0, Vec::len);
        prop_assert!(parse_assets(&rows).len() <= n);
        let _ = parse_assets(&v);
    }
}
