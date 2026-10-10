//! Totality harness for the Deribit wire decoders: every function that turns raw SOCKET / REST
//! input (frame text, a `serde_json::Value`, a response body) into events, market data, chain rows
//! or reconcile reports is fed (a) arbitrary text and lossy-decoded byte noise, (b) random JSON
//! whose object keys are drawn from the decoders' REAL field names (so the generator reaches the
//! branches instead of bouncing off the first `.get()`), (c) well-shaped JSON-RPC subscription
//! frames and REST bodies whose fields are mostly well-typed with hostile leaves (`NaN`, `inf`,
//! `1e999`, `i64::MIN`, empty strings, wrong types), and (d) short frame SEQUENCES into ONE book /
//! bar folder.
//!
//! The property is TOTALITY: a hostile or truncated frame may decode to nothing (or to `Err`), but
//! it must never panic the pump thread that reads the socket. Outputs are asserted only where a
//! cheap bound must always hold (no event flood).
//!
//! Covered (all through the PUBLIC API): `market_data::{route_frame, classify_rpc_reply,
//! parse_quote, parse_trades, parse_chart_bar, parse_book_snapshot}`, the book / depth pump step
//! (`market_feed::fold_depth_frame` is private, so `pump_step` below restates it over its public
//! parts), `market_feed::BarFolder`, `options_feed::{parse_markprice_options, parse_ticker}`,
//! `dvol::{parse_dvol_frame, on_dvol_frame}`, `data::parse_deribit_klines`,
//! `catalog::{parse_instruments, asset_class_of}`, `chain::{parse_instrument_name,
//! list_expiries_from_summary, build_chain_from_summary, chain_snapshot_rows}`, `rpc::parse_response`;
//! under the default-on `exec` feature also `event_mapper`, `history::map_deribit_history`,
//! `reconcile::build_reconcile_snapshot`, `recon_client::parse_*`, `client::
//! parse_deribit_option_instruments`, `exec::parse_get_instrument` and `combo::{parse_combo,
//! parse_combo_grid, map_combo}`.
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use std::sync::Arc;

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::depth::infer_tick_size;
use vike_data::{MemHistStore, RecordingSink};
use vike_deribit::dvol::DvolRecorder;
use vike_deribit::market_data::MdEvent;
use vike_deribit::{catalog, chain, data, dvol, market_data, market_feed, options_feed, rpc};
use vike_model::L2Book;

const INST: &str = "BTC-PERPETUAL";

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

const INSTS: &[&str] = &[
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
const USER_TRADES_CH: &[&str] = &["user.trades.any.any.raw", "user.trades.option.BTC.raw"];
const MARK_CH: &[&str] = &["markprice.options.btc_usd"];
const TICKER_CH: &[&str] = &["ticker.BTC-27JUN26-100000-C.100ms"];
const DVOL_CH: &[&str] = &["deribit_volatility_index.btc_usd"];
const ENVELOPE_KEYS: &[&str] =
    &["jsonrpc", "method", "params", "channel", "data", "id", "result", "error", "code", "message"];
/// Every real field name the keyless decoders read, plus the envelope keys.
const PUBLIC_KEYS: &[&str] = &[
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
fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
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
fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(ENVELOPE_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
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
fn jnum() -> BoxedStrategy<Value> {
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
fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A short id-shaped field (`trade_id`, `label`, `order_id`): usually non-empty.
fn idv() -> BoxedStrategy<Value> {
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
fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
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

fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
fn truncated(text: &str, n: usize) -> String {
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
const ISO_EXPIRIES: &[&str] = &["2026-06-27", "2026-12-25", "2024-02-29", "2027-01-01"];

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

fn hostile_name() -> BoxedStrategy<String> {
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
fn trade_row() -> BoxedStrategy<Value> {
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

fn markprice_row() -> BoxedStrategy<Value> {
    obj_of(vec![("instrument_name", name_value()), ("mark_price", jnum()), ("iv", jnum())])
}

fn ticker_data() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instrument_name", name_value()),
        ("best_bid_price", jnum()),
        ("best_ask_price", jnum()),
        ("mark_price", jnum()),
        ("mark_iv", jnum()),
        ("open_interest", jnum()),
        ("underlying_price", jnum()),
        ("stats", junk_or(obj_of(vec![("volume", jnum())]))),
    ])
}

fn dvol_data() -> BoxedStrategy<Value> {
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
fn sub_frame(
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

fn book_frame() -> BoxedStrategy<Value> {
    sub_frame(BOOK_CH, book_data())
}

fn chart_frame() -> BoxedStrategy<Value> {
    sub_frame(CHART_CH, chart_data())
}

/// Any keyless public frame, one lane at a time (plus replies and random JSON).
fn lane_frame() -> BoxedStrategy<Value> {
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

fn frame_text(frame: BoxedStrategy<Value>) -> BoxedStrategy<String> {
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

fn chart_body() -> BoxedStrategy<Value> {
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
fn instrument_row() -> BoxedStrategy<Value> {
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

fn instruments_payload() -> BoxedStrategy<Value> {
    obj_of(vec![("result", junk_or(array_of(instrument_row(), 5)))])
}

/// A book-summary `instrument_name`: a realistic option name with a VALID calendar date (the
/// impossible-date class has its own test), else any leaf.
fn summary_name() -> BoxedStrategy<Value> {
    prop_oneof![8 => valid_name().prop_map(Value::String), 2 => arb_leaf()].boxed()
}

/// One `get_book_summary_by_currency` row.
fn summary_row() -> BoxedStrategy<Value> {
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

fn now_ms() -> BoxedStrategy<i64> {
    // 1970 .. 2100: `DateTime::from_timestamp_millis` is the CALLER's clock, never wire input.
    (0i64..4_102_444_800_000).boxed()
}

// --- the drivers ---------------------------------------------------------------------------

/// Restates `market_feed::fold_depth_frame` (private, unreachable from here) over its public
/// parts: the first snapshot anchors the book on its inferred grid, every later frame folds
/// through `route_frame`'s `prev_change_id` chain.
fn pump_step(text: &str, book: &mut Option<L2Book>) {
    match book.as_mut() {
        None => {
            let Ok(v) = serde_json::from_str::<Value>(text) else { return };
            let Some((seq, bids, asks)) = market_data::parse_book_snapshot(&v) else { return };
            let mut bk = L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
        }
        Some(bk) => {
            let _ = market_data::route_frame(text, INST, bk);
        }
    }
}

/// Every keyless `Value` decoder on one parsed payload.
fn drive_public_value(v: &Value) {
    let _ = market_data::classify_rpc_reply(v);
    let _ = market_data::parse_quote(v);
    let _ = market_data::parse_trades(v);
    let _ = market_data::parse_chart_bar(v);
    let _ = market_data::parse_book_snapshot(v);
    let _ = options_feed::parse_markprice_options(v);
    let _ = options_feed::parse_ticker(v);
    let _ = dvol::parse_dvol_frame(v);
    let _ = rpc::parse_response(v);
    let _ = catalog::parse_instruments(v);
    let _ = catalog::asset_class_of(v);
}

/// Every keyless text decoder on one frame.
fn drive_public_text(text: &str) {
    let mut book = L2Book::new(0.5);
    let _ = market_data::route_frame(text, INST, &mut book);
    let mut pumped = None;
    pump_step(text, &mut pumped);
    let _ = data::parse_deribit_klines(text);
    let sink = RecordingSink::default();
    let _ = dvol::on_dvol_frame(text, &sink, None);
    let recorder = DvolRecorder::new(Arc::new(MemHistStore::default()), true);
    let _ = dvol::on_dvol_frame(text, &sink, Some(&recorder));
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        drive_public_value(&v);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// (a) arbitrary text never panics any keyless text decoder.
    #[test]
    fn public_text_decoders_are_total_over_arbitrary_text(text in any::<String>()) {
        drive_public_text(&text);
    }

    /// (a') ...nor lossy-decoded byte noise (controls, invalid UTF-8 replaced).
    #[test]
    fn public_text_decoders_survive_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        drive_public_text(&String::from_utf8_lossy(&bytes));
    }

    /// (b) random JSON over the real field names, as a value and as serialized text.
    #[test]
    fn public_decoders_are_total_over_arbitrary_json(v in arb_json(PUBLIC_KEYS)) {
        drive_public_value(&v);
        drive_public_text(&v.to_string());
    }

    /// (b') well-shaped subscription frames of every lane with hostile leaves, whole and cut off
    /// mid-flight.
    #[test]
    fn public_decoders_are_total_over_lane_frames(v in lane_frame(), cut in 0usize..400) {
        drive_public_value(&v);
        let text = v.to_string();
        drive_public_text(&text);
        drive_public_text(&truncated(&text, cut));
    }

    /// (b'') the REST bodies: chart data and the instrument list.
    #[test]
    fn rest_bodies_are_total(
        chart in chart_body(),
        insts in instruments_payload(),
        cut in 0usize..400,
    ) {
        let text = chart.to_string();
        let _ = data::parse_deribit_klines(&text);
        let _ = data::parse_deribit_klines(&truncated(&text, cut));
        drive_public_value(&insts);
    }

    /// (c) a short frame sequence into ONE book (the snapshot anchor + `change_id` chain, replays,
    /// gaps, junk in any order) never panics, and a Resync / Ignored leaves the book usable.
    #[test]
    fn book_fold_survives_frame_sequences(
        frames in prop::collection::vec(frame_text(prop_oneof![book_frame(), lane_frame()].boxed()), 1..8),
    ) {
        let mut book = L2Book::new(0.5);
        let mut pumped: Option<L2Book> = None;
        for f in &frames {
            let ev = market_data::route_frame(f, INST, &mut book);
            if let MdEvent::Trades(rows) = &ev {
                prop_assert!(!rows.is_empty(), "an empty Trades event");
            }
            pump_step(f, &mut pumped);
            prop_assert!(book.tick_size > 0.0);
            if let Some(b) = &pumped {
                prop_assert!(b.tick_size > 0.0);
            }
        }
    }

    /// (c') the closed-bar inference: any push sequence through `BarFolder` never panics, and it
    /// emits at most one close per push.
    #[test]
    fn bar_folder_survives_push_sequences(
        frames in prop::collection::vec(chart_frame(), 1..8),
    ) {
        let mut folder = market_feed::BarFolder::new();
        for f in &frames {
            if let Some(bar) = market_data::parse_chart_bar(f)
                && let Some(roll) = folder.fold(bar)
            {
                prop_assert!(roll.closed.as_ref().is_none_or(|c| c.ts < roll.forming.ts));
            }
        }
    }

    /// The `markprice.options` / `ticker` / DVOL frames, plus the instrument-name grammar the
    /// chain provider keys on.
    #[test]
    fn options_and_dvol_frames_are_total(
        mark in sub_frame(MARK_CH, array_of(markprice_row(), 5)),
        ticker in sub_frame(TICKER_CH, ticker_data()),
        dvol_frame in sub_frame(DVOL_CH, dvol_data()),
        name in hostile_name(),
        index in any::<String>(),
    ) {
        let _ = options_feed::parse_markprice_options(&mark);
        let _ = options_feed::parse_ticker(&ticker);
        let _ = dvol::parse_dvol_frame(&dvol_frame);
        let _ = chain::parse_instrument_name(&name);
        let _ = chain::is_usd_quoted(&name);
        let _ = dvol::dvol_symbol(&index);
        let _ = dvol::dvol_channel(&index);
        let _ = options_feed::markprice_options_channel(&index);
    }

    /// The chain builder over realistic book-summary rows (valid calendar dates; the ISO expiry
    /// is the CALLER's, from `list_expiries`) with hostile premiums / IVs / spots, and the
    /// recorder rows derived from the result.
    #[test]
    fn chain_builder_is_total(
        rows in prop::collection::vec(summary_row(), 0..8),
        now in now_ms(),
        usd_quoted in any::<bool>(),
        r in prop_oneof![Just(0.0), Just(0.05), any::<f64>()],
    ) {
        let _ = chain::list_expiries_from_summary(&rows, now, None);
        let _ = chain::list_expiries_from_summary(&rows, now, Some("BTC"));
        for iso in ISO_EXPIRIES {
            for currency in ["BTC", "SOL"] {
                let built = chain::build_chain_from_summary(currency, &rows, iso, now, usd_quoted, r);
                let flat = chain::chain_snapshot_rows(&built);
                prop_assert!(flat.len() <= 2 * built.rows.len(), "more than a call and a put per strike");
            }
        }
    }

    /// ⚠ `chain::list_expiries_from_summary` over names with ANY day 0..=99 and any month: a
    /// grammar-valid but impossible date (`31FEB26`, `00JAN26`, `29FEB25`) reaches
    /// `vike_options::make_expiry`, whose `parse_iso` panics ("invalid calendar date"). Suspected
    /// real bug — see the report; the parser admits `\d{1,2}` days without a calendar check.
    #[test]
    fn expiry_listing_is_total_over_any_day_and_month(
        days in prop::collection::vec(0u32..100, 1..6),
        months in prop::collection::vec(0usize..12, 1..6),
        years in prop::collection::vec(0u32..100, 1..6),
        now in now_ms(),
    ) {
        const MONTHS: [&str; 12] =
            ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
        let rows: Vec<Value> = days
            .iter()
            .zip(&months)
            .zip(&years)
            .map(|((d, m), y)| {
                serde_json::json!({ "instrument_name": format!("BTC-{d}{}{y:02}-100000-C", MONTHS[*m]) })
            })
            .collect();
        let _ = chain::list_expiries_from_summary(&rows, now, None);
    }
}

// --- the exec plane (default-on `exec` feature) ---------------------------------------------

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use proptest::test_runner::TestCaseError;
    use vike_deribit::{client, combo, event_mapper, exec, history, recon_client, reconcile};
    use vike_model::ComboLeg;

    const ALL_KEYS: &[&str] = &[
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
        "trade_id",
        "label",
        "order_id",
        "instrument_name",
        "direction",
        "amount",
        "price",
        "fee",
        "fee_currency",
        "liquidity",
        "timestamp",
        "state",
        "order_state",
        "order_type",
        "filled_amount",
        "average_price",
        "last_update_timestamp",
        "creation_timestamp",
        "size",
        "mark_price",
        "delta",
        "balance",
        "type",
        "maker_commission",
        "taker_commission",
        "legs",
        "tick_size",
        "min_trade_amount",
        "contract_size",
        "tick_size_steps",
        "above_price",
        "kind",
        "settlement_period",
        "is_active",
        "trades",
    ];

    /// One `get_open_orders_by_instrument` / `get_order_history` row.
    fn order_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("label", idv()),
            ("order_id", idv()),
            ("instrument_name", pick(INSTS)),
            ("direction", pick(&["buy", "sell", ""])),
            ("order_type", pick(&["limit", "market", "stop_limit", ""])),
            ("order_state", pick(&["open", "filled", "cancelled", "rejected", ""])),
            ("amount", jnum()),
            ("filled_amount", jnum()),
            ("average_price", jnum()),
            (
                "price",
                prop_oneof![jnum(), Just(Value::String("market_price".to_string())).boxed()]
                    .boxed(),
            ),
            ("last_update_timestamp", jint()),
            ("creation_timestamp", jint()),
        ])
    }

    /// One `get_positions` row.
    fn position_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("instrument_name", pick(INSTS)),
            ("direction", pick(&["buy", "sell", "zero", ""])),
            ("size", jnum()),
            ("average_price", jnum()),
            ("mark_price", jnum()),
            ("delta", jnum()),
        ])
    }

    fn any_row() -> BoxedStrategy<Value> {
        prop_oneof![trade_row(), order_row(), position_row(), instrument_row(), arb_json(ALL_KEYS),]
            .boxed()
    }

    fn rows(max: usize) -> BoxedStrategy<Value> {
        array_of(any_row(), max)
    }

    /// Account-summary / fee-rate shaped objects.
    fn account_obj() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("balance", jnum()),
            ("id", jint()),
            ("type", pick(&["main", "subaccount", "sub_account", ""])),
            ("maker_commission", jnum()),
            ("taker_commission", jnum()),
        ])
    }

    /// A combo leg list as `private/create_combo` / `get_combo_details` answers it.
    fn combo_result() -> BoxedStrategy<Value> {
        let leg = obj_of(vec![
            ("instrument_name", pick(INSTS)),
            ("amount", prop_oneof![jnum(), jint()].boxed()),
        ]);
        obj_of(vec![
            ("id", pick(&["BTC-FS-27JUN26_PERP", ""])),
            ("state", pick(&["active", "inactive", ""])),
            ("legs", junk_or(array_of(leg, 4))),
            ("tick_size", jnum()),
            ("min_trade_amount", jnum()),
        ])
    }

    fn legs() -> BoxedStrategy<Vec<ComboLeg>> {
        prop::collection::vec(
            (prop::sample::select(INSTS), any::<i32>())
                .prop_map(|(symbol, ratio)| ComboLeg { symbol: symbol.to_string(), ratio }),
            0..5,
        )
        .boxed()
    }

    /// Every REST-body text decoder of the reconcile client on one body.
    fn drive_recon_text(body: &str, now: i64) {
        let _ = recon_client::parse_open_orders(body);
        let _ = recon_client::parse_positions(body, INST, now);
        let _ = recon_client::parse_user_trades(body);
        let _ = recon_client::parse_account_summary_balance(body);
        let _ = recon_client::parse_account_identity(body);
        let _ = recon_client::parse_fee_rate(body);
    }

    /// Every exec-plane `Value` decoder on one payload.
    fn drive_exec_value(v: &Value) {
        let _ = client::parse_deribit_option_instruments(v);
        let _ = exec::parse_get_instrument(v);
        let _ = combo::parse_combo(v);
        let _ = combo::parse_combo_grid(v);
        let empty = Value::Null;
        let _ = history::map_deribit_history(v, &empty, "deribit", INST);
        let _ = history::map_deribit_history(&empty, v, "deribit", INST);
        let _ = history::map_deribit_history(v, v, "deribit", INST);
    }

    /// The private-WS dispatcher on one frame; no event flood (a row yields <= 2 events).
    fn check_private(frame: &Value) -> Result<(), TestCaseError> {
        let n_rows = frame
            .get("params")
            .and_then(|p| p.get("data"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let evs = event_mapper::map_deribit_private(frame, "deribit", INST);
        prop_assert!(evs.len() <= 2 * n_rows, "flood: {} events from {n_rows} rows", evs.len());
        let one = event_mapper::map_deribit_trade(frame, "deribit", INST);
        prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a) arbitrary text / byte noise through every reconcile-body parser.
        #[test]
        fn recon_parsers_are_total_over_text(text in any::<String>(), now in now_ms()) {
            drive_recon_text(&text, now);
        }

        #[test]
        fn recon_parsers_survive_byte_noise(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            now in now_ms(),
        ) {
            drive_recon_text(&String::from_utf8_lossy(&bytes), now);
        }

        /// (b) the reconcile bodies: serialized arrays of every row shape (whole and cut off) and
        /// account-shaped objects; a valid array must parse to exactly one report per row.
        #[test]
        fn recon_parsers_are_total_over_bodies(
            arr in rows(6),
            obj in account_obj(),
            now in now_ms(),
            cut in 0usize..600,
        ) {
            let n = arr.as_array().map_or(0, Vec::len);
            let body = arr.to_string();
            drive_recon_text(&body, now);
            drive_recon_text(&truncated(&body, cut), now);
            drive_recon_text(&obj.to_string(), now);
            let orders = recon_client::parse_open_orders(&body);
            prop_assert!(matches!(&orders, Ok(r) if r.len() == n), "one report per row");
            let fills = recon_client::parse_user_trades(&body);
            prop_assert!(matches!(&fills, Ok(r) if r.len() <= n), "a fill report per row at most");
            let positions = recon_client::parse_positions(&body, INST, now);
            prop_assert!(matches!(&positions, Ok(r) if r.len() <= n), "a position per row at most");
        }

        /// (b') arbitrary JSON over the real field names through every exec-plane decoder.
        #[test]
        fn exec_decoders_are_total_over_arbitrary_json(v in arb_json(ALL_KEYS), now in now_ms()) {
            drive_recon_text(&v.to_string(), now);
            drive_exec_value(&v);
            check_private(&v)?;
        }

        /// (b'') the private `user.trades` dispatcher over well-shaped subscription frames.
        #[test]
        fn private_mapper_is_total_over_user_trade_frames(
            frame in sub_frame(USER_TRADES_CH, array_of(trade_row(), 5)),
        ) {
            check_private(&frame)?;
            drive_exec_value(&frame);
        }

        /// ...and one `user.trades` row at a time, straight into the row mapper.
        #[test]
        fn trade_row_mapper_is_total(row in trade_row()) {
            let one = event_mapper::map_deribit_trade(&row, "deribit", INST);
            prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
        }

        /// (c) a short sequence of private frames: the mapper holds no state, so a sequence is
        /// just N independent frames — kept to prove it stays that way.
        #[test]
        fn private_frame_sequences_never_panic(
            frames in prop::collection::vec(
                sub_frame(USER_TRADES_CH, array_of(trade_row(), 4)),
                1..8,
            ),
        ) {
            for f in &frames {
                check_private(f)?;
            }
        }

        /// The history replay over arrays of order / trade rows.
        #[test]
        fn history_replay_is_total(
            orders in junk_or(array_of(order_row(), 5)),
            trades in junk_or(array_of(trade_row(), 5)),
        ) {
            let evs = history::map_deribit_history(&orders, &trades, "deribit", INST);
            let n_orders = orders.as_array().map_or(0, Vec::len);
            let n_trades = trades.as_array().map_or(0, Vec::len);
            // two events per trade, at most one terminal per order
            prop_assert!(evs.len() <= 2 * n_trades + n_orders, "history flood");
        }

        /// The instrument payloads: the plural list, the singular grid, the combo grid.
        #[test]
        fn instrument_payloads_are_total(
            list in instruments_payload(),
            single in obj_of(vec![("result", junk_or(instrument_row()))]),
            combo_grid in obj_of(vec![("result", junk_or(combo_result()))]),
        ) {
            let _ = client::parse_deribit_option_instruments(&list);
            let _ = exec::parse_get_instrument(&single);
            let _ = exec::parse_get_instrument(&list);
            let _ = combo::parse_combo_grid(&combo_grid);
            let _ = combo::parse_combo_grid(&single);
        }

        /// `client::option_base_asset` (private) is reached through the option-instrument list:
        /// hostile instrument names, non-ASCII expiries included, must not panic the slicing.
        #[test]
        fn option_instrument_names_are_total(names in prop::collection::vec(hostile_name(), 1..6)) {
            let rows: Vec<Value> = names
                .iter()
                .map(|n| serde_json::json!({ "kind": "option", "instrument_name": n }))
                .collect();
            let payload = serde_json::json!({ "result": rows });
            let _ = client::parse_deribit_option_instruments(&payload);
            for n in &names {
                let _ = chain::parse_instrument_name(n);
            }
        }

        /// The combo reply parser and the leg-mapping solver (arbitrary i32 ratios, duplicate and
        /// unmatched symbols, empty lists).
        #[test]
        fn combo_decoders_are_total(
            reply in combo_result(),
            spec in legs(),
            venue in legs(),
        ) {
            let parsed = combo::parse_combo(&reply);
            let _ = combo::map_combo(&spec, &venue);
            if let Some(c) = &parsed {
                let _ = combo::map_combo(&spec, &c.legs);
                let _ = combo::map_combo(&c.legs, &c.legs);
            }
        }

        /// ⚠ `reconcile::build_reconcile_snapshot` on `get_positions` / `get_open_orders` results.
        /// Suspected `.expect("static shape")` panic in `build_orders` when an open order's
        /// `amount` is a string that parses non-finite ("inf" / "NaN" / "1e999"): the `json!`
        /// image of a non-finite `qty` is `null`, which `OrderRequest.qty: f64` refuses.
        #[test]
        fn reconcile_snapshot_is_total(
            positions in junk_or(array_of(position_row(), 4)),
            orders in junk_or(array_of(order_row(), 4)),
        ) {
            let snap = reconcile::build_reconcile_snapshot(&positions, &orders, INST);
            prop_assert!(!snap.positions.is_empty(), "a snapshot always names the symbol");
        }
    }
}
