//! The generator kit the decoder harness draws from: value pools, combinators and Deribit frames.

use proptest::prelude::*;
use serde_json::{Map, Value};

pub(crate) const INST: &str = "BTC-PERPETUAL";

/// Strings that matter to a numeric / id / enum read on the wire (including non-ASCII, for the
/// non-char-boundary class of slicing bugs).
const STR_POOL: &[&str] = &[
    "",
    " ",
    "0",
    "1",
    "-1",
    "-0",
    "0.0",
    "1.5",
    "42",
    "NaN",
    "nan",
    "inf",
    "-inf",
    "Infinity",
    "1e999",
    "-1e999",
    "1e-999",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "99999999999999999999999999999999",
    "1700000000000",
    "50000.5",
    "true",
    "null",
    "\u{0}",
    "é",
    "日本",
];

/// Deribit vocabulary: methods, channels, order / trade states, sides, kinds.
const WORDS: &[&str] = &[
    "subscription",
    "public/subscribe",
    "snapshot",
    "change",
    "new",
    "delete",
    "buy",
    "sell",
    "M",
    "T",
    "open",
    "filled",
    "cancelled",
    "rejected",
    "untriggered",
    "zero",
    "market_price",
    "limit",
    "main",
    "subaccount",
    "option",
    "future",
    "spot",
    "future_combo",
    "option_combo",
    "perpetual",
    "linear",
    "reversed",
    "ok",
    "no_data",
    "BTC",
    "BTC_USDC",
    "btc_usd",
    "BTC-PERPETUAL",
    "BTC-27JUN26-100000-C",
    "book.BTC-PERPETUAL.100ms",
    "trades.BTC-PERPETUAL.100ms",
    "quote.BTC-PERPETUAL",
    "chart.trades.BTC-PERPETUAL.1",
    "user.trades.any.any.raw",
    "markprice.options.btc_usd",
    "ticker.BTC-27JUN26-100000-C.100ms",
    "deribit_volatility_index.btc_usd",
];

pub(crate) const INSTS: &[&str] = &[
    "BTC-PERPETUAL",
    "BTC-27JUN26-100000-C",
    "ETH-25DEC26-3000.5-P",
    "SOL_USDC-29FEB24-90-P",
    "BTC_USDC-PERPETUAL",
    "",
];
const BOOK_CH: &[&str] = &["book.BTC-PERPETUAL.100ms"];
const TRADES_CH: &[&str] = &["trades.BTC-PERPETUAL.100ms", "trades.option.BTC.100ms"];
const QUOTE_CH: &[&str] = &["quote.BTC-PERPETUAL"];
const CHART_CH: &[&str] = &["chart.trades.BTC-PERPETUAL.1"];
pub const USER_TRADES_CH: &[&str] = &["user.trades.any.any.raw", "user.trades.option.BTC.raw"];
pub(crate) const MARK_CH: &[&str] = &["markprice.options.btc_usd"];
pub(crate) const TICKER_CH: &[&str] = &["ticker.BTC-27JUN26-100000-C.100ms"];
pub(crate) const DVOL_CH: &[&str] = &["deribit_volatility_index.btc_usd"];
const ENVELOPE_KEYS: &[&str] =
    &["jsonrpc", "method", "params", "channel", "data", "id", "result", "error", "code", "message"];
/// Every real field name the keyless decoders read, plus the envelope keys.
pub(crate) const PUBLIC_KEYS: &[&str] = &[
    "jsonrpc",
    "method",
    "params",
    "channel",
    "data",
    "id",
    "result",
    "error",
    "code",
    "message",
    "type",
    "change_id",
    "prev_change_id",
    "timestamp",
    "bids",
    "asks",
    "instrument_name",
    "best_bid_price",
    "best_ask_price",
    "best_bid_amount",
    "best_ask_amount",
    "price",
    "amount",
    "direction",
    "tick",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "mark_price",
    "iv",
    "mark_iv",
    "open_interest",
    "underlying_price",
    "stats",
    "volatility",
    "index_name",
    "status",
    "ticks",
    "kind",
    "settlement_period",
    "is_active",
    "base_currency",
    "quote_currency",
    "instrument_type",
    "settlement_currency",
    "bid_price",
    "ask_price",
    "last_price",
    "bid_iv",
    "ask_iv",
    "index_price",
    "greeks",
    "delta",
    "gamma",
    "vega",
    "theta",
    "rho",
];

// --- the generator kit ---------------------------------------------------------------------

fn arb_str() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(STR_POOL).prop_map(String::from),
        4 => prop::sample::select(WORDS).prop_map(String::from),
        1 => any::<i64>().prop_map(|n| n.to_string()),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        2 => "[ -~]{0,12}",
        1 => any::<String>(),
    ]
    .boxed()
}

/// A JSON leaf: null, bool, i64 extremes / zero / negative, u64, f64, hostile strings, and the
/// empty array / object.
fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => prop::sample::select(vec![0i64, 1, -1, i64::MAX, i64::MIN]).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        5 => arb_str().prop_map(Value::String),
        1 => Just(Value::Array(Vec::new())),
        1 => Just(Value::Object(Map::new())),
    ]
    .boxed()
}

fn arb_key(keys: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(keys).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Random JSON, depth <= 4, object keys drawn mostly from `keys`.
pub(crate) fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(keys), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
pub(crate) fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(ENVELOPE_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
pub(crate) fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop_oneof![
        8 => prop::sample::select(set).prop_map(|s| Value::String(s.to_string())),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// Decimal text a numeric read would parse (mostly), else hostile text.
fn num_text() -> BoxedStrategy<String> {
    prop_oneof![
        4 => (-1_000_000i64..1_000_000).prop_map(|n| n.to_string()),
        4 => (-1.0e6f64..1.0e6).prop_map(|f| format!("{f:.6}")),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        1 => arb_str(),
    ]
    .boxed()
}

fn json_f64(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A numeric wire field. Deribit sends JSON NUMBERS (so usually a number), but a string-encoded
/// numeric is read too; sometimes any leaf.
pub(crate) fn jnum() -> BoxedStrategy<Value> {
    prop_oneof![
        5 => (-1.0e6f64..1.0e6).prop_map(json_f64),
        2 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(json_f64),
        2 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// An integer wire field (epoch-ms stamps, ids): usually a number in the epoch-ms range.
pub(crate) fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A short id-shaped field (`trade_id`, `label`, `order_id`): usually non-empty.
pub(crate) fn idv() -> BoxedStrategy<Value> {
    prop_oneof![5 => "[a-z0-9_]{1,8}".prop_map(Value::String), 1 => jint()].boxed()
}

/// A sequence-number field: small chains (so `prev_change_id` can match), else anything.
fn seqv() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0u64..8).prop_map(|n| Value::Number(n.into())),
        2 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// An object over exactly the named fields; each is present with p = 0.85 and drawn from its own
/// (mostly well-typed) strategy.
pub(crate) fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
    fields
        .into_iter()
        .map(|(k, s)| prop::option::weighted(0.85, s).prop_map(move |v| (k, v)))
        .collect::<Vec<_>>()
        .prop_map(|kvs| {
            let mut m = Map::new();
            for (k, v) in kvs {
                if let Some(v) = v {
                    m.insert(k.to_string(), v);
                }
            }
            Value::Object(m)
        })
        .boxed()
}

pub(crate) fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
pub(crate) fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// --- Deribit-shaped rows -------------------------------------------------------------------

/// A realistic option name with a VALID calendar date, paired with its ISO expiry.
const EXPIRIES: &[(&str, &str)] = &[
    ("27JUN26", "2026-06-27"),
    ("25DEC26", "2026-12-25"),
    ("29FEB24", "2024-02-29"),
    ("1JAN27", "2027-01-01"),
];
pub(crate) const ISO_EXPIRIES: &[&str] = &["2026-06-27", "2026-12-25", "2024-02-29", "2027-01-01"];

fn strike_text() -> BoxedStrategy<String> {
    prop_oneof![
        (1u32..500_000).prop_map(|n| n.to_string()),
        (1u32..5_000, 0u32..1_000).prop_map(|(a, b)| format!("{a}.{b}")),
    ]
    .boxed()
}

const BASES: &[&str] = &["BTC", "ETH", "SOL", "BTC_USDC", "SOL_USDT", "XRP_USDC"];
const CALL_PUT: &[&str] = &["C", "P"];

fn valid_name() -> BoxedStrategy<String> {
    (
        prop::sample::select(BASES),
        prop::sample::select(EXPIRIES),
        strike_text(),
        prop::sample::select(CALL_PUT),
    )
        .prop_map(|(base, (exp, _), strike, cp)| format!("{base}-{exp}-{strike}-{cp}"))
        .boxed()
}

/// Dash-joined name parts from a pool seeded with the shapes that break naive slicing: non-ASCII
/// in the expiry slot, impossible dates, empty / dotted strikes.
const NAME_PARTS: &[&str] = &[
    "BTC", "ETH", "SOL_USDC", "BTC_USDT", "27JUN26", "1JAN27", "123é4", "é1234", "日12", "27ÉUN26",
    "99ZZZ99", "31FEB26", "00JAN26", "29FEB25", "100000", "1.5", "1.", ".5", "C", "P", "é", "",
];

pub(crate) fn hostile_name() -> BoxedStrategy<String> {
    let part = prop_oneof![
        6 => prop::sample::select(NAME_PARTS).prop_map(String::from),
        1 => "[ -~]{0,8}",
        1 => "\\PC{0,8}",
    ];
    prop::collection::vec(part, 1..6).prop_map(|p| p.join("-")).boxed()
}

fn name_value() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => valid_name().prop_map(Value::String),
        2 => hostile_name().prop_map(Value::String),
        2 => pick(INSTS),
    ]
    .boxed()
}

/// `[[action, price, amount], ...]` book sides (JSON numbers).
fn book_side() -> BoxedStrategy<Value> {
    let triple = (pick(&["new", "change", "delete", ""]), jnum(), jnum())
        .prop_map(|(a, p, q)| Value::Array(vec![a, p, q]));
    let junk = prop::collection::vec(arb_leaf(), 0..4).prop_map(Value::Array);
    let level = prop_oneof![9 => triple, 1 => junk];
    prop_oneof![
        8 => prop::collection::vec(level, 0..6).prop_map(Value::Array),
        2 => arb_leaf(),
    ]
    .boxed()
}

fn book_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("type", pick(&["snapshot", "change", "change", ""])),
        ("change_id", seqv()),
        ("prev_change_id", seqv()),
        ("timestamp", jint()),
        ("bids", book_side()),
        ("asks", book_side()),
        ("instrument_name", pick(INSTS)),
    ])
}

/// One `user.trades` / `trades.*` row (the two share their schema).
pub(crate) fn trade_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("trade_id", idv()),
        ("label", idv()),
        ("order_id", idv()),
        ("instrument_name", pick(INSTS)),
        ("direction", pick(&["buy", "sell", "zero", ""])),
        ("amount", jnum()),
        ("price", jnum()),
        ("fee", jnum()),
        ("fee_currency", pick(&["BTC", "USDC", ""])),
        ("liquidity", pick(&["M", "T", ""])),
        ("timestamp", jint()),
        ("state", pick(&["filled", "open", "cancelled", ""])),
    ])
}

fn quote_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", pick(INSTS)),
        ("timestamp", jint()),
        ("best_bid_price", jnum()),
        ("best_ask_price", jnum()),
        ("best_bid_amount", jnum()),
        ("best_ask_amount", jnum()),
    ])
}

fn chart_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("tick", jint()),
        ("open", jnum()),
        ("high", jnum()),
        ("low", jnum()),
        ("close", jnum()),
        ("volume", jnum()),
        ("cost", jnum()),
    ])
}

pub(crate) fn markprice_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", name_value()),
        ("mark_price", jnum()),
        ("iv", jnum()),
        ("timestamp", jint()),
    ])
}

pub(crate) fn ticker_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", name_value()),
        ("best_bid_price", jnum()),
        ("best_ask_price", jnum()),
        ("best_bid_amount", jnum()),
        ("best_ask_amount", jnum()),
        ("last_price", jnum()),
        ("mark_price", jnum()),
        ("mark_iv", jnum()),
        ("bid_iv", jnum()),
        ("ask_iv", jnum()),
        ("open_interest", jnum()),
        ("underlying_price", jnum()),
        ("index_price", jnum()),
        ("timestamp", jint()),
        ("stats", junk_or(obj_of(vec![("volume", jnum())]))),
        (
            "greeks",
            junk_or(obj_of(vec![
                ("delta", jnum()),
                ("gamma", jnum()),
                ("vega", jnum()),
                ("theta", jnum()),
                ("rho", jnum()),
            ])),
        ),
    ])
}

pub(crate) fn dvol_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("volatility", jnum()),
        ("index_name", pick(&["btc_usd", "eth_usd", "_usd", "btc", ""])),
        ("timestamp", jint()),
    ])
}

fn error_obj() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("code", jint()),
        ("message", arb_leaf()),
        ("data", junk_or(obj_of(vec![("reason", arb_leaf())]))),
    ])
}

/// A JSON-RPC reply: `{id, result | error}`.
fn rpc_reply() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("jsonrpc", pick(&["2.0"])),
        ("id", jint()),
        ("result", prop_oneof![array_of(pick(INSTS), 3), arb_json(PUBLIC_KEYS)].boxed()),
        ("error", error_obj()),
    ])
}

/// `{jsonrpc, method:"subscription", params:{channel, data}}`, each part occasionally missing or
/// the wrong type.
pub(crate) fn sub_frame(
    channels: &'static [&'static str],
    data: BoxedStrategy<Value>,
) -> BoxedStrategy<Value> {
    let params = obj_of(vec![("channel", pick(channels)), ("data", junk_or(data))]);
    obj_of(vec![
        ("jsonrpc", pick(&["2.0"])),
        ("method", pick(&["subscription", "subscription", ""])),
        ("params", junk_or(params)),
    ])
}

pub(crate) fn book_frame() -> BoxedStrategy<Value> {
    sub_frame(BOOK_CH, book_data())
}

pub(crate) fn chart_frame() -> BoxedStrategy<Value> {
    sub_frame(CHART_CH, chart_data())
}

/// Any keyless public frame, one lane at a time (plus replies and random JSON).
pub(crate) fn lane_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        book_frame(),
        sub_frame(TRADES_CH, array_of(trade_row(), 4)),
        sub_frame(QUOTE_CH, quote_data()),
        chart_frame(),
        sub_frame(USER_TRADES_CH, array_of(trade_row(), 4)),
        sub_frame(MARK_CH, array_of(markprice_row(), 4)),
        sub_frame(TICKER_CH, ticker_data()),
        sub_frame(DVOL_CH, dvol_data()),
        rpc_reply(),
        arb_json(PUBLIC_KEYS),
    ]
    .boxed()
}

pub(crate) fn frame_text(frame: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![9 => frame.prop_map(|v| v.to_string()), 1 => any::<String>()].boxed()
}

/// A `public/get_tradingview_chart_data` result: usually aligned columns, sometimes ragged / junk.
fn chart_result() -> BoxedStrategy<Value> {
    let aligned = prop::collection::vec((jint(), jnum(), jnum(), jnum(), jnum(), jnum()), 0..6)
        .prop_map(|rows| {
            let (mut t, mut o, mut h, mut l, mut c, mut v) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for (a, b, cc, d, e, f) in rows {
                t.push(a);
                o.push(b);
                h.push(cc);
                l.push(d);
                c.push(e);
                v.push(f);
            }
            let mut m = Map::new();
            m.insert("status".to_string(), Value::String("ok".to_string()));
            for (k, col) in
                [("ticks", t), ("open", o), ("high", h), ("low", l), ("close", c), ("volume", v)]
            {
                m.insert(k.to_string(), Value::Array(col));
            }
            Value::Object(m)
        });
    let ragged = obj_of(vec![
        ("status", pick(&["ok", "no_data", "error", ""])),
        ("ticks", array_of(jint(), 4)),
        ("open", array_of(jnum(), 4)),
        ("high", array_of(jnum(), 4)),
        ("low", array_of(jnum(), 4)),
        ("close", array_of(jnum(), 4)),
        ("volume", array_of(jnum(), 4)),
    ]);
    prop_oneof![8 => aligned, 2 => ragged].boxed()
}

pub(crate) fn chart_body() -> BoxedStrategy<Value> {
    (prop::option::weighted(0.9, junk_or(chart_result())), prop::option::weighted(0.1, error_obj()))
        .prop_map(|(result, error)| {
            let mut m = Map::new();
            if let Some(r) = result {
                m.insert("result".to_string(), r);
            }
            if let Some(e) = error {
                m.insert("error".to_string(), e);
            }
            Value::Object(m)
        })
        .boxed()
}

/// One `get_instruments` / `get_instrument` row.
pub(crate) fn instrument_row() -> BoxedStrategy<Value> {
    let step = obj_of(vec![("above_price", jnum()), ("tick_size", jnum())]);
    obj_of(vec![
        ("kind", pick(&["option", "future", "spot", "future_combo", "option_combo", ""])),
        ("settlement_period", pick(&["perpetual", "month", "week", ""])),
        ("instrument_name", name_value()),
        ("is_active", prop_oneof![any::<bool>().prop_map(Value::Bool), arb_leaf()].boxed()),
        ("base_currency", pick(&["BTC", "ETH", ""])),
        ("quote_currency", pick(&["USD", "USDC", ""])),
        ("instrument_type", pick(&["linear", "reversed", ""])),
        ("settlement_currency", pick(&["BTC", "USDC", ""])),
        ("tick_size", jnum()),
        ("min_trade_amount", jnum()),
        ("contract_size", jnum()),
        ("tick_size_steps", junk_or(array_of(step, 4))),
    ])
}

pub(crate) fn instruments_payload() -> BoxedStrategy<Value> {
    obj_of(vec![("result", junk_or(array_of(instrument_row(), 5)))])
}

/// A book-summary `instrument_name`: a realistic option name with a VALID calendar date (the
/// impossible-date class has its own test), else any leaf.
fn summary_name() -> BoxedStrategy<Value> {
    prop_oneof![8 => valid_name().prop_map(Value::String), 2 => arb_leaf()].boxed()
}

/// One `get_book_summary_by_currency` row.
pub(crate) fn summary_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", summary_name()),
        ("underlying_price", jnum()),
        ("bid_price", jnum()),
        ("ask_price", jnum()),
        ("mark_price", jnum()),
        ("mark_iv", jnum()),
        ("open_interest", jnum()),
        ("volume", jnum()),
    ])
}

pub(crate) fn now_ms() -> BoxedStrategy<i64> {
    // 1970 .. 2100: `DateTime::from_timestamp_millis` is the CALLER's clock, never wire input.
    (0i64..4_102_444_800_000).boxed()
}
