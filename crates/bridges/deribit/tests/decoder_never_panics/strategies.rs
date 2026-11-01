//! The generator kit the decoder harness draws from: value pools, combinators and Deribit frames.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::json_arb::{PoolKit, array_of, obj_of};

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

/// Deribit's vocabulary over the shared generator shapes (`vike_bridge_core::json_arb`).
pub(crate) const KIT: PoolKit = PoolKit { strs: STR_POOL, words: WORDS };

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
pub(crate) fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => KIT.arb_json(ENVELOPE_KEYS)].boxed()
}

/// A short id-shaped field (`trade_id`, `label`, `order_id`): usually non-empty.
pub(crate) fn idv() -> BoxedStrategy<Value> {
    prop_oneof![5 => "[a-z0-9_]{1,8}".prop_map(Value::String), 1 => KIT.jint()].boxed()
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
        2 => KIT.pick(INSTS),
    ]
    .boxed()
}

/// `[[action, price, amount], ...]` book sides (JSON numbers).
fn book_side() -> BoxedStrategy<Value> {
    let triple = (KIT.pick(&["new", "change", "delete", ""]), KIT.jnum(), KIT.jnum())
        .prop_map(|(a, p, q)| Value::Array(vec![a, p, q]));
    let junk = prop::collection::vec(KIT.arb_leaf(), 0..4).prop_map(Value::Array);
    let level = prop_oneof![9 => triple, 1 => junk];
    prop_oneof![
        8 => prop::collection::vec(level, 0..6).prop_map(Value::Array),
        2 => KIT.arb_leaf(),
    ]
    .boxed()
}

fn book_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("type", KIT.pick(&["snapshot", "change", "change", ""])),
        ("change_id", KIT.seqv()),
        ("prev_change_id", KIT.seqv()),
        ("timestamp", KIT.jint()),
        ("bids", book_side()),
        ("asks", book_side()),
        ("instrument_name", KIT.pick(INSTS)),
    ])
}

/// One `user.trades` / `trades.*` row (the two share their schema).
pub(crate) fn trade_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("trade_id", idv()),
        ("label", idv()),
        ("order_id", idv()),
        ("instrument_name", KIT.pick(INSTS)),
        ("direction", KIT.pick(&["buy", "sell", "zero", ""])),
        ("amount", KIT.jnum()),
        ("price", KIT.jnum()),
        ("fee", KIT.jnum()),
        ("fee_currency", KIT.pick(&["BTC", "USDC", ""])),
        ("liquidity", KIT.pick(&["M", "T", ""])),
        ("timestamp", KIT.jint()),
        ("state", KIT.pick(&["filled", "open", "cancelled", ""])),
    ])
}

fn quote_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", KIT.pick(INSTS)),
        ("timestamp", KIT.jint()),
        ("best_bid_price", KIT.jnum()),
        ("best_ask_price", KIT.jnum()),
        ("best_bid_amount", KIT.jnum()),
        ("best_ask_amount", KIT.jnum()),
    ])
}

fn chart_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("tick", KIT.jint()),
        ("open", KIT.jnum()),
        ("high", KIT.jnum()),
        ("low", KIT.jnum()),
        ("close", KIT.jnum()),
        ("volume", KIT.jnum()),
        ("cost", KIT.jnum()),
    ])
}

pub(crate) fn markprice_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", name_value()),
        ("mark_price", KIT.jnum()),
        ("iv", KIT.jnum()),
        ("timestamp", KIT.jint()),
    ])
}

pub(crate) fn ticker_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", name_value()),
        ("best_bid_price", KIT.jnum()),
        ("best_ask_price", KIT.jnum()),
        ("best_bid_amount", KIT.jnum()),
        ("best_ask_amount", KIT.jnum()),
        ("last_price", KIT.jnum()),
        ("mark_price", KIT.jnum()),
        ("mark_iv", KIT.jnum()),
        ("bid_iv", KIT.jnum()),
        ("ask_iv", KIT.jnum()),
        ("open_interest", KIT.jnum()),
        ("underlying_price", KIT.jnum()),
        ("index_price", KIT.jnum()),
        ("timestamp", KIT.jint()),
        ("stats", junk_or(obj_of(vec![("volume", KIT.jnum())]))),
        (
            "greeks",
            junk_or(obj_of(vec![
                ("delta", KIT.jnum()),
                ("gamma", KIT.jnum()),
                ("vega", KIT.jnum()),
                ("theta", KIT.jnum()),
                ("rho", KIT.jnum()),
            ])),
        ),
    ])
}

pub(crate) fn dvol_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("volatility", KIT.jnum()),
        ("index_name", KIT.pick(&["btc_usd", "eth_usd", "_usd", "btc", ""])),
        ("timestamp", KIT.jint()),
    ])
}

fn error_obj() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("code", KIT.jint()),
        ("message", KIT.arb_leaf()),
        ("data", junk_or(obj_of(vec![("reason", KIT.arb_leaf())]))),
    ])
}

/// A JSON-RPC reply: `{id, result | error}`.
fn rpc_reply() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("jsonrpc", KIT.pick(&["2.0"])),
        ("id", KIT.jint()),
        ("result", prop_oneof![array_of(KIT.pick(INSTS), 3), KIT.arb_json(PUBLIC_KEYS)].boxed()),
        ("error", error_obj()),
    ])
}

/// `{jsonrpc, method:"subscription", params:{channel, data}}`, each part occasionally missing or
/// the wrong type.
pub(crate) fn sub_frame(
    channels: &'static [&'static str],
    data: BoxedStrategy<Value>,
) -> BoxedStrategy<Value> {
    let params = obj_of(vec![("channel", KIT.pick(channels)), ("data", junk_or(data))]);
    obj_of(vec![
        ("jsonrpc", KIT.pick(&["2.0"])),
        ("method", KIT.pick(&["subscription", "subscription", ""])),
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
        KIT.arb_json(PUBLIC_KEYS),
    ]
    .boxed()
}

pub(crate) fn frame_text(frame: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![9 => frame.prop_map(|v| v.to_string()), 1 => any::<String>()].boxed()
}

/// A `public/get_tradingview_chart_data` result: usually aligned columns, sometimes ragged / junk.
fn chart_result() -> BoxedStrategy<Value> {
    let aligned = prop::collection::vec(
        (KIT.jint(), KIT.jnum(), KIT.jnum(), KIT.jnum(), KIT.jnum(), KIT.jnum()),
        0..6,
    )
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
        ("status", KIT.pick(&["ok", "no_data", "error", ""])),
        ("ticks", array_of(KIT.jint(), 4)),
        ("open", array_of(KIT.jnum(), 4)),
        ("high", array_of(KIT.jnum(), 4)),
        ("low", array_of(KIT.jnum(), 4)),
        ("close", array_of(KIT.jnum(), 4)),
        ("volume", array_of(KIT.jnum(), 4)),
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
    let step = obj_of(vec![("above_price", KIT.jnum()), ("tick_size", KIT.jnum())]);
    obj_of(vec![
        ("kind", KIT.pick(&["option", "future", "spot", "future_combo", "option_combo", ""])),
        ("settlement_period", KIT.pick(&["perpetual", "month", "week", ""])),
        ("instrument_name", name_value()),
        ("is_active", prop_oneof![any::<bool>().prop_map(Value::Bool), KIT.arb_leaf()].boxed()),
        ("base_currency", KIT.pick(&["BTC", "ETH", ""])),
        ("quote_currency", KIT.pick(&["USD", "USDC", ""])),
        ("instrument_type", KIT.pick(&["linear", "reversed", ""])),
        ("settlement_currency", KIT.pick(&["BTC", "USDC", ""])),
        ("tick_size", KIT.jnum()),
        ("min_trade_amount", KIT.jnum()),
        ("contract_size", KIT.jnum()),
        ("tick_size_steps", junk_or(array_of(step, 4))),
    ])
}

pub(crate) fn instruments_payload() -> BoxedStrategy<Value> {
    obj_of(vec![("result", junk_or(array_of(instrument_row(), 5)))])
}

/// A book-summary `instrument_name`: a realistic option name with a VALID calendar date (the
/// impossible-date class has its own test), else any leaf.
fn summary_name() -> BoxedStrategy<Value> {
    prop_oneof![8 => valid_name().prop_map(Value::String), 2 => KIT.arb_leaf()].boxed()
}

/// One `get_book_summary_by_currency` row.
pub(crate) fn summary_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", summary_name()),
        ("underlying_price", KIT.jnum()),
        ("bid_price", KIT.jnum()),
        ("ask_price", KIT.jnum()),
        ("mark_price", KIT.jnum()),
        ("mark_iv", KIT.jnum()),
        ("open_interest", KIT.jnum()),
        ("volume", KIT.jnum()),
    ])
}

pub(crate) fn now_ms() -> BoxedStrategy<i64> {
    // 1970 .. 2100: `DateTime::from_timestamp_millis` is the CALLER's clock, never wire input.
    (0i64..4_102_444_800_000).boxed()
}
