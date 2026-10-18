//! The generator kit and the keyless drivers of the OKX decoder harness: pools, combinators, frames.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::depth::infer_tick_size;
use vike_marketdata::L2Book;
use vike_okx::market_data::BooksFrame;
use vike_okx::{catalog, data, market_data, market_feed};

pub const INST: &str = "BTC-USDT-SWAP";

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
    "0.00000000",
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

/// OKX vocabulary: channels, `event`s, `state`s, sides, `action`s, instruments.
const WORDS: &[&str] = &[
    "orders",
    "account",
    "positions",
    "bbo-tbt",
    "trades",
    "books5",
    "books",
    "mark-price",
    "candle1m",
    "candle1H",
    "tickers",
    "event",
    "error",
    "subscribe",
    "unsubscribe",
    "login",
    "pong",
    "snapshot",
    "update",
    "live",
    "canceled",
    "mmp_canceled",
    "filled",
    "partially_filled",
    "buy",
    "sell",
    "long",
    "short",
    "net",
    "M",
    "T",
    "8",
    "USDT",
    "BTC",
    "isolated",
    "cross",
    "full_liquidation",
    "adl",
    "BTC-USDT-SWAP",
    "BTC-USDT",
    "BTC-USD-241227-50000-C",
];

pub const INSTS: &[&str] =
    &["BTC-USDT-SWAP", "BTC-USDT", "ETH-USD-SWAP", "BTC-USD-241227-50000-C", ""];
pub const PUBLIC_CHANNELS: &[&str] =
    &["bbo-tbt", "trades", "books5", "books", "mark-price", "candle1m", "candle1H", "tickers"];
pub const BOOK_CHANNELS: &[&str] = &["books", "books", "books5", "bbo-tbt"];
const ENVELOPE_KEYS: &[&str] =
    &["event", "code", "msg", "action", "arg", "data", "channel", "instId", "op"];
/// Every real field name the public decoders read, plus the envelope keys.
pub const PUBLIC_KEYS: &[&str] = &[
    "event",
    "code",
    "msg",
    "action",
    "arg",
    "data",
    "channel",
    "instId",
    "bids",
    "asks",
    "ts",
    "px",
    "sz",
    "side",
    "seqId",
    "prevSeqId",
    "checksum",
    "markPx",
    "tradeId",
    "instType",
    "state",
    "baseCcy",
    "quoteCcy",
    "tickSz",
    "lotSz",
    "minSz",
    "maxMktSz",
    "ctVal",
    "ctMult",
    "ctValCcy",
    "lever",
    "ctType",
    "settleCcy",
    "expTime",
    "alias",
    "optType",
    "stk",
    "uly",
    "instFamily",
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
pub fn arb_leaf() -> BoxedStrategy<Value> {
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
pub fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
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
pub fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(ENVELOPE_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
pub fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
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

/// A numeric wire field: usually a decimal STRING (the OKX convention), sometimes a JSON number or
/// any leaf.
pub fn numv() -> BoxedStrategy<Value> {
    prop_oneof![
        7 => num_text().prop_map(Value::String),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// A short id-shaped field (`tradeId`, `ordId`, `clOrdId`): usually non-empty.
pub fn idv() -> BoxedStrategy<Value> {
    prop_oneof![5 => "[a-z0-9]{1,8}".prop_map(Value::String), 1 => arb_leaf()].boxed()
}

/// A sequence-number field: small chains (so `prevSeqId` can match), else anything.
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
pub fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
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

pub fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// `[[px, sz, ...], ...]` book sides.
fn levels() -> BoxedStrategy<Value> {
    let item = prop_oneof![9 => num_text().prop_map(Value::String), 1 => arb_leaf()];
    prop_oneof![
        8 => prop::collection::vec(
            prop::collection::vec(item, 0..5).prop_map(Value::Array),
            0..6,
        )
        .prop_map(Value::Array),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// One row of the public channels: `bbo-tbt` / `trades` / `books5` / `books` / `mark-price`.
pub fn market_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("bids", levels()),
        ("asks", levels()),
        ("ts", numv()),
        ("px", numv()),
        ("sz", numv()),
        ("side", pick(&["buy", "sell", ""])),
        ("seqId", seqv()),
        ("prevSeqId", seqv()),
        ("checksum", numv()),
        ("markPx", numv()),
        ("tradeId", idv()),
        ("instId", pick(INSTS)),
    ])
}

/// One `candle*` row: `[ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]`, decimal strings.
pub fn candle_row() -> BoxedStrategy<Value> {
    let item = prop_oneof![9 => num_text().prop_map(Value::String), 1 => arb_leaf()];
    (prop::collection::vec(item, 0..9), pick(&["0", "1"]))
        .prop_map(|(mut cells, confirm)| {
            cells.push(confirm);
            Value::Array(cells)
        })
        .boxed()
}

/// One `public/instruments` row.
pub fn inst_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instId", pick(INSTS)),
        ("state", pick(&["live", "suspend", ""])),
        ("instType", pick(&["SWAP", "FUTURES", "SPOT", "OPTION", ""])),
        ("baseCcy", pick(&["BTC", ""])),
        ("quoteCcy", pick(&["USDT", ""])),
        ("ctValCcy", pick(&["BTC", ""])),
        ("tickSz", numv()),
        ("lotSz", numv()),
        ("minSz", numv()),
        ("maxMktSz", numv()),
        ("ctVal", numv()),
        ("ctMult", numv()),
        ("lever", numv()),
        ("ctType", pick(&["linear", "inverse", ""])),
        ("settleCcy", pick(&["USDT", "BTC", ""])),
        ("expTime", pick(&["1791792000000", "-1", "0", "99999999999999999999", ""])),
        ("alias", pick(&["this_week", "quarter", "this_five_years", ""])),
        ("optType", pick(&["C", "P", "X", ""])),
        ("stk", numv()),
        ("uly", pick(&["BTC-USD", "BTC", "-", ""])),
        ("instFamily", pick(&["BTC-USDT", ""])),
    ])
}

/// `{arg:{channel, instId}, data:[rows], action?, + a few stray envelope keys}`, each part
/// occasionally missing or the wrong type.
pub fn envelope(
    channels: &'static [&'static str],
    row: BoxedStrategy<Value>,
) -> BoxedStrategy<Value> {
    let arg = junk_or(obj_of(vec![("channel", pick(channels)), ("instId", pick(INSTS))]));
    let data = junk_or(array_of(row, 4));
    let action = pick(&["snapshot", "update", ""]);
    let extras = prop::collection::vec((arb_key(ENVELOPE_KEYS), arb_leaf()), 0..3);
    (
        prop::option::weighted(0.92, arg),
        prop::option::weighted(0.92, data),
        prop::option::weighted(0.5, action),
        extras,
    )
        .prop_map(|(arg, data, action, extras)| {
            let mut m: Map<String, Value> = extras.into_iter().collect();
            if let Some(v) = arg {
                m.insert("arg".to_string(), v);
            }
            if let Some(v) = data {
                m.insert("data".to_string(), v);
            }
            if let Some(v) = action {
                m.insert("action".to_string(), v);
            }
            Value::Object(m)
        })
        .boxed()
}

/// The V5 REST envelope `{code, msg, data}`.
pub fn rest_envelope(data: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    obj_of(vec![
        ("code", pick(&["0", "0", "0", "50011", "51000", ""])),
        ("msg", arb_leaf()),
        ("data", junk_or(data)),
    ])
}

/// A frame as the socket delivers it: usually the serialized envelope, sometimes raw noise.
pub fn frame_text(
    channels: &'static [&'static str],
    row: BoxedStrategy<Value>,
) -> BoxedStrategy<String> {
    prop_oneof![9 => envelope(channels, row).prop_map(|v| v.to_string()), 1 => any::<String>()]
        .boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
pub fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// --- the drivers ---------------------------------------------------------------------------

/// Restates `market_feed::depth::okx_depth_decode` (`pub(crate)`, unreachable from here) over its public
/// parts, so the DOM fold's real inputs (`parse_books_frame`, `infer_tick_size`, the `L2Book`
/// reducer, the `prevSeqId` chain check) all run on the hostile frame.
pub fn fold_deep(text: &str, book: &mut Option<L2Book>) {
    match market_data::parse_books_frame(text) {
        BooksFrame::Snapshot { seq, bids, asks, .. } => {
            let mut bk = L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
        }
        BooksFrame::Update { seq, prev_seq, bids, asks, .. } => {
            if let Some(bk) = book.as_mut()
                && (prev_seq == 0 || prev_seq == bk.last_seq)
            {
                let _ = bk.apply_delta(seq, &bids, &asks);
            }
        }
        BooksFrame::Other => {}
    }
}

/// Every keyless text decoder on one frame.
pub fn drive_public_text(text: &str) {
    let mut book = L2Book::new(0.01);
    let _ = market_data::route_frame(text, INST, &mut book);
    let _ = market_data::parse_books_frame(text);
    let _ = market_feed::klines::route_frame(text);
    let _ = market_feed::mark::route_mark_frame(text);
    let _ = data::parse_okx_klines(text);
    let mut deep = None;
    fold_deep(text, &mut deep);
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        drive_public_value(&v);
    }
}

/// Every keyless `Value` decoder on one parsed payload.
pub fn drive_public_value(v: &Value) {
    let _ = market_data::decode_bbo(v, INST);
    let _ = market_data::decode_trade(v, INST);
    let _ = market_feed::trades::parse_trades(v);
    let _ = catalog::parse_underlyings(v);
    let _ = catalog::parse_insts(v);
}
