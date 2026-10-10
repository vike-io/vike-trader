//! "Arbitrary input never panics" harness for every PUBLIC OANDA wire decoder:
//!
//! - `vike_oanda::market_data::{decode_pricing_frame, parse_forming_candle}` — one
//!   `/pricing/stream` line (`PRICE` / `HEARTBEAT`) and the forming tail of a `candles` response;
//! - `vike_oanda::{decode_transaction_events, map_transactions_since, max_transaction_id}` — one
//!   `/transactions/stream` line and the A3 `sinceid` backfill body;
//! - `vike_oanda::{map_order_response, note_last_transaction_id}` — the order-POST response;
//! - `vike_oanda::parse_candles` / `parse_instruments` — the candles and catalog bodies;
//! - `vike_oanda::recon_client::{parse_order_reports, parse_position_reports, parse_fill_reports,
//!   parse_balance, normalize_order_state}` — the reconcile report parsers.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing, but
//! it must never panic the stream / pump thread (a dead thread is a venue that silently goes
//! quiet), and it must never fabricate an event flood. Each decoder is fed (a) arbitrary text (as
//! the stream loops do: `serde_json::from_str` first), (b) lossy-decoded byte noise and (c)
//! structured JSON whose object keys are the decoder's REAL field names, so the generator reaches
//! the branches instead of bouncing off the first `.get()`. Outputs are asserted only against the
//! cheap bounds that must always hold.
//!
//! The private decoders (`fold_pricing_line`, `pump_lines`, the history `decode_page`) are covered
//! by the sibling unit-test files `src/market_feed_props.rs` and `src/klines_props.rs`.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_oanda::market_data::{decode_pricing_frame, parse_forming_candle};
use vike_oanda::recon_client::{
    normalize_order_state, parse_balance, parse_fill_reports, parse_order_reports,
    parse_position_reports,
};
use vike_oanda::{
    decode_transaction_events, map_order_response, map_transactions_since, max_transaction_id,
    note_last_transaction_id, parse_candles, parse_instruments, to_oanda_instrument,
};

/// Real OANDA v20 field names across every decoder above.
const KEYS: &[&str] = &[
    "type",
    "time",
    "instrument",
    "bids",
    "asks",
    "price",
    "liquidity",
    "closeoutBid",
    "closeoutAsk",
    "status",
    "tradeable",
    "units",
    "id",
    "orderID",
    "clientOrderID",
    "clientExtensions",
    "reason",
    "commission",
    "tradeID",
    "candles",
    "complete",
    "mid",
    "o",
    "h",
    "l",
    "c",
    "volume",
    "transactions",
    "lastTransactionID",
    "orderRejectTransaction",
    "rejectReason",
    "orderCreateTransaction",
    "orderFillTransaction",
    "orders",
    "state",
    "createTime",
    "positions",
    "long",
    "short",
    "averagePrice",
    "account",
    "balance",
    "instruments",
    "name",
    "displayName",
    "errorMessage",
];

/// Real dispatch values and instruments.
const WORDS: &[&str] = &[
    "PRICE",
    "HEARTBEAT",
    "ORDER_FILL",
    "ORDER_CANCEL",
    "ORDER_CREATE",
    "FILLED",
    "CANCELLED",
    "PENDING",
    "TRIGGERED",
    "LIMIT",
    "STOP",
    "MARKET",
    "CURRENCY",
    "CFD",
    "EUR_USD",
    "XAU_USD",
    "eurusd",
    "_",
    "",
];

/// The (instrument, symbol) pairs the report parsers are mounted with.
const INSTRUMENTS: &[(&str, &str)] = &[("EUR_USD", "EURUSD"), ("XAU_USD", "XAUUSD"), ("", "")];

const TXN_KINDS: &[&str] = &["ORDER_FILL", "ORDER_CANCEL", "HEARTBEAT", "ORDER_CREATE"];
const TXN_KEYS: &[&str] = &[
    "id",
    "time",
    "orderID",
    "units",
    "price",
    "commission",
    "reason",
    "clientOrderID",
    "tradeID",
    "rejectReason",
];
const LEVEL_KEYS: &[&str] = &["price", "liquidity"];
const LEG_KEYS: &[&str] = &["units", "averagePrice"];
const MID_KEYS: &[&str] = &["o", "h", "l", "c"];
const CANDLE_KEYS: &[&str] = &["time", "volume"];
const ORDER_KEYS: &[&str] = &["id", "createTime", "type", "units", "state", "price"];

/// Wire scalars: OANDA sends every decimal as a STRING, so numeric strings (including epoch-seconds
/// with a fraction and absurd magnitudes) are first-class leaves, next to the usual hostile set.
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
            "1e300",
            "-1e300",
            "1e-999",
            "",
            " ",
            ".",
            "-",
            "0x10",
            "9999999999999999999999999999999999999999",
            "-9999999999999999999999999999999999999999",
            "1478012400.500000000",
            "18446744073709551615",
            "18446744073709551616",
        ])
        .prop_map(|s| Value::String(s.to_string())),
        "-?[0-9]{1,40}(\\.[0-9]{1,40})?".prop_map(Value::String),
        "[0-9]{1,3}\\.[0-9]{1,5}".prop_map(Value::String),
        prop::sample::select(WORDS).prop_map(|s| Value::String(s.to_string())),
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

fn set(mut v: Value, key: &str, child: Value) -> Value {
    if let Value::Object(m) = &mut v {
        m.insert(key.to_string(), child);
    }
    v
}

/// An object over exactly `keys`, each present with high probability and holding a hostile leaf.
fn shaped(keys: &'static [&'static str]) -> impl Strategy<Value = Value> {
    prop::collection::vec(prop::option::weighted(0.85, arb_leaf()), keys.len()..=keys.len())
        .prop_map(move |vals| {
            Value::Object(
                keys.iter()
                    .zip(vals)
                    .filter_map(|(k, v)| v.map(|v| ((*k).to_string(), v)))
                    .collect(),
            )
        })
}

/// `shaped(keys)` plus a `type` drawn from the decoder's real dispatch kinds (a hostile leaf one
/// time in nine, so the unknown-kind arm still runs).
fn typed(
    kinds: &'static [&'static str],
    keys: &'static [&'static str],
) -> impl Strategy<Value = Value> {
    let kind = prop_oneof![
        8 => prop::sample::select(kinds).prop_map(|k| Value::String(k.to_string())),
        1 => arb_leaf(),
    ];
    (shaped(keys), kind).prop_map(|(obj, kind)| set(obj, "type", kind))
}

/// The row's `instrument`: the mounted one most of the time, so the report filters let rows through.
fn on_instrument(row: impl Strategy<Value = Value>) -> impl Strategy<Value = Value> {
    let instrument = prop_oneof![
        4 => prop::sample::select(vec!["EUR_USD", "XAU_USD"])
            .prop_map(|s| Value::String(s.to_string())),
        1 => arb_leaf(),
    ];
    (row, instrument).prop_map(|(row, i)| set(row, "instrument", i))
}

fn arb_levels() -> impl Strategy<Value = Value> {
    prop_oneof![
        8 => prop::collection::vec(shaped(LEVEL_KEYS), 0..3).prop_map(Value::Array),
        1 => arb_leaf(),
    ]
}

/// A `PRICE` / `HEARTBEAT` stream line: ladders of `{price, liquidity}`.
fn arb_pricing_line() -> impl Strategy<Value = Value> {
    (on_instrument(typed(&["PRICE", "HEARTBEAT"], &["time", "status"])), arb_levels(), arb_levels())
        .prop_map(|(frame, bids, asks)| set(set(frame, "bids", bids), "asks", asks))
}

/// One candle: `{complete, time, volume, mid:{o,h,l,c}}`, `complete` mostly a real bool.
fn arb_candle() -> impl Strategy<Value = Value> {
    let complete = prop_oneof![
        6 => any::<bool>().prop_map(Value::Bool),
        1 => arb_leaf(),
    ];
    (shaped(CANDLE_KEYS), shaped(MID_KEYS), complete)
        .prop_map(|(candle, mid, complete)| set(set(candle, "mid", mid), "complete", complete))
}

fn arb_candles_response() -> impl Strategy<Value = Value> {
    prop::collection::vec(arb_candle(), 0..6)
        .prop_map(|candles| set(Value::Object(Map::new()), "candles", Value::Array(candles)))
}

/// One transaction (stream line / `sinceid` entry / order-POST sub-transaction), with a
/// `clientExtensions.id` most of the time.
fn arb_txn() -> impl Strategy<Value = Value> {
    (on_instrument(typed(TXN_KINDS, TXN_KEYS)), prop::option::weighted(0.8, shaped(&["id"])))
        .prop_map(|(txn, ext)| match ext {
            Some(ext) => set(txn, "clientExtensions", ext),
            None => txn,
        })
}

/// An order-POST response: any of the three transaction slots, a watermark, no particular order.
fn arb_order_response() -> impl Strategy<Value = Value> {
    (
        prop::option::of(arb_txn()),
        prop::option::of(arb_txn()),
        prop::option::of(arb_txn()),
        arb_leaf(),
    )
        .prop_map(|(rej, create, fill, last)| {
            let mut v = Value::Object(Map::new());
            for (k, t) in [
                ("orderRejectTransaction", rej),
                ("orderCreateTransaction", create),
                ("orderFillTransaction", fill),
            ] {
                if let Some(t) = t {
                    v = set(v, k, t);
                }
            }
            set(v, "lastTransactionID", last)
        })
}

fn arb_sinceid_body() -> impl Strategy<Value = Value> {
    (prop::collection::vec(arb_txn(), 0..6), arb_leaf()).prop_map(|(txns, last)| {
        set(
            set(Value::Object(Map::new()), "transactions", Value::Array(txns)),
            "lastTransactionID",
            last,
        )
    })
}

fn arb_orders_body() -> impl Strategy<Value = Value> {
    let row = (on_instrument(shaped(ORDER_KEYS)), prop::option::weighted(0.7, shaped(&["id"])))
        .prop_map(|(row, ext)| match ext {
            Some(ext) => set(row, "clientExtensions", ext),
            None => row,
        });
    prop::collection::vec(row, 0..6)
        .prop_map(|rows| set(Value::Object(Map::new()), "orders", Value::Array(rows)))
}

fn arb_positions_body() -> impl Strategy<Value = Value> {
    let row = (
        on_instrument(Just(Value::Object(Map::new()))),
        prop::option::weighted(0.8, shaped(LEG_KEYS)),
        prop::option::weighted(0.8, shaped(LEG_KEYS)),
    )
        .prop_map(|(mut row, long, short)| {
            if let Some(l) = long {
                row = set(row, "long", l);
            }
            if let Some(s) = short {
                row = set(row, "short", s);
            }
            row
        });
    prop::collection::vec(row, 0..6)
        .prop_map(|rows| set(Value::Object(Map::new()), "positions", Value::Array(rows)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    // ---- pricing stream ---------------------------------------------------------------------

    /// `decode_pricing_frame` / `parse_forming_candle` are total over arbitrary JSON.
    #[test]
    fn pricing_decoders_survive_structured_json(v in arb_json()) {
        let _ = decode_pricing_frame(&v);
        let _ = parse_forming_candle(&v);
    }

    /// ...and over text the way the pump feeds it (parse first; a non-JSON line never reaches them).
    #[test]
    fn pricing_decoders_survive_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let _ = decode_pricing_frame(&v);
            let _ = parse_forming_candle(&v);
        }
    }

    /// ...and over PRICE / HEARTBEAT lines with hostile ladders.
    #[test]
    fn pricing_decoders_survive_shaped_lines(v in arb_pricing_line()) {
        let _ = decode_pricing_frame(&v);
    }

    /// (c) A short run of pricing lines never panics (the decoder is stateless; the stateful fold
    /// over it is `src/market_feed_props.rs`).
    #[test]
    fn pricing_decoders_survive_a_line_sequence(
        lines in prop::collection::vec(prop_oneof![arb_pricing_line(), arb_json()], 1..8),
    ) {
        for l in &lines {
            let _ = decode_pricing_frame(l);
        }
    }

    // ---- candles ----------------------------------------------------------------------------

    /// `parse_candles` / `parse_forming_candle` over candle responses with hostile prices and
    /// times; one response never yields more bars than it has candles.
    #[test]
    fn candle_decoders_survive_shaped_responses(v in arb_candles_response()) {
        let n = v["candles"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_candles(&v).len() <= n);
        let _ = parse_forming_candle(&v);
    }

    #[test]
    fn candle_decoders_survive_structured_json(v in arb_json()) {
        let _ = parse_candles(&v);
        let _ = parse_forming_candle(&v);
    }

    // ---- transactions stream and A3 backfill -----------------------------------------------

    /// `decode_transaction_events` is total over arbitrary JSON; one line emits at most the
    /// dual-publish pair.
    #[test]
    fn transaction_events_survive_structured_json(v in arb_json()) {
        let out = decode_transaction_events(&v);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
    }

    #[test]
    fn transaction_events_survive_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let out = decode_transaction_events(&v);
            prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        }
    }

    #[test]
    fn transaction_events_survive_shaped_lines(v in arb_txn()) {
        let out = decode_transaction_events(&v);
        prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
    }

    /// (c) A run of stream lines, one after another, never panics.
    #[test]
    fn transaction_events_survive_a_line_sequence(
        lines in prop::collection::vec(prop_oneof![arb_txn(), arb_json()], 1..8),
    ) {
        for l in &lines {
            let out = decode_transaction_events(l);
            prop_assert!(out.len() <= 2, "event flood: {} events", out.len());
        }
    }

    /// The `sinceid` backfill: at most two events per transaction, and a total watermark read.
    #[test]
    fn sinceid_backfill_survives_shaped_and_free_bodies(shaped_body in arb_sinceid_body(), free in arb_json()) {
        let n = shaped_body["transactions"].as_array().map_or(0, Vec::len);
        prop_assert!(map_transactions_since(&shaped_body).len() <= 2 * n);
        let _ = max_transaction_id(&shaped_body);
        let _ = map_transactions_since(&free);
        let _ = max_transaction_id(&free);
    }

    // ---- order-POST response ----------------------------------------------------------------

    /// `map_order_response` emits at most Accepted + Fill + OrderFilled (a rejection short-circuits
    /// to one event); `note_last_transaction_id` never moves the A3 watermark backwards.
    #[test]
    fn order_response_survives_shaped_and_free_bodies(
        coid in any::<String>(),
        ts in any::<i64>(),
        shaped_resp in arb_order_response(),
        free in arb_json(),
        start in any::<u64>(),
    ) {
        for resp in [&shaped_resp, &free] {
            let out = map_order_response(&coid, ts, resp);
            prop_assert!(out.len() <= 3, "event flood: {} events", out.len());
            let last_seen = Arc::new(AtomicU64::new(start));
            note_last_transaction_id(&last_seen, resp);
            prop_assert!(last_seen.load(Ordering::Relaxed) >= start, "the watermark moved backwards");
        }
    }

    // ---- reconcile report parsers -----------------------------------------------------------

    /// The four report parsers over arbitrary parsed JSON (the `fetch_*` paths parse first).
    #[test]
    fn recon_parsers_survive_structured_json(
        v in arb_json(),
        inst in prop::sample::select(INSTRUMENTS),
        home in any::<String>(),
    ) {
        let _ = parse_order_reports(&v, inst.0, inst.1);
        let _ = parse_position_reports(&v, inst.0, inst.1);
        let _ = parse_fill_reports(&v, inst.0, inst.1, &home);
        let _ = parse_balance(&v);
    }

    /// ...and over bodies shaped like the real endpoints, so the row closures run on hostile
    /// fields; a parser never invents rows.
    #[test]
    fn recon_parsers_survive_shaped_bodies(
        orders in arb_orders_body(),
        positions in arb_positions_body(),
        sinceid in arb_sinceid_body(),
        summary in arb_json(),
        inst in prop::sample::select(INSTRUMENTS),
    ) {
        let n_orders = orders["orders"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_order_reports(&orders, inst.0, inst.1).len() <= n_orders);
        let n_pos = positions["positions"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_position_reports(&positions, inst.0, inst.1).len() <= n_pos);
        let n_txn = sinceid["transactions"].as_array().map_or(0, Vec::len);
        prop_assert!(parse_fill_reports(&sinceid, inst.0, inst.1, "USD").len() <= n_txn);
        let _ = parse_balance(&summary);
    }

    // ---- catalog, symbol and state helpers --------------------------------------------------

    #[test]
    fn catalog_survives_structured_json(v in arb_json()) {
        let _ = parse_instruments(&v);
    }

    #[test]
    fn symbol_and_state_helpers_survive_arbitrary_text(text in prop_oneof![any::<String>(), arb_noise()]) {
        let _ = to_oanda_instrument(&text);
        let _ = normalize_order_state(&text);
    }
}
