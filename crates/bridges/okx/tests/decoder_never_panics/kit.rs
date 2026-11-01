//! The generator kit and the keyless drivers of the OKX decoder harness: pools, combinators, frames.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::depth::infer_tick_size;
use vike_bridge_core::json_arb::{PoolKit, array_of, obj_of, weighted_key};
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

/// OKX's vocabulary over the shared generator shapes (`vike_bridge_core::json_arb`).
pub const KIT: PoolKit = PoolKit { strs: STR_POOL, words: WORDS };

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
pub fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => KIT.arb_json(ENVELOPE_KEYS)].boxed()
}

/// A short id-shaped field (`tradeId`, `ordId`, `clOrdId`): usually non-empty.
pub fn idv() -> BoxedStrategy<Value> {
    prop_oneof![5 => "[a-z0-9]{1,8}".prop_map(Value::String), 1 => KIT.arb_leaf()].boxed()
}

/// `[[px, sz, ...], ...]` book sides.
fn levels() -> BoxedStrategy<Value> {
    let item = prop_oneof![9 => KIT.num_text().prop_map(Value::String), 1 => KIT.arb_leaf()];
    prop_oneof![
        8 => prop::collection::vec(
            prop::collection::vec(item, 0..5).prop_map(Value::Array),
            0..6,
        )
        .prop_map(Value::Array),
        2 => KIT.arb_leaf(),
    ]
    .boxed()
}

/// One row of the public channels: `bbo-tbt` / `trades` / `books5` / `books` / `mark-price`.
pub fn market_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("bids", levels()),
        ("asks", levels()),
        ("ts", KIT.numv()),
        ("px", KIT.numv()),
        ("sz", KIT.numv()),
        ("side", KIT.pick(&["buy", "sell", ""])),
        ("seqId", KIT.seqv()),
        ("prevSeqId", KIT.seqv()),
        ("checksum", KIT.numv()),
        ("markPx", KIT.numv()),
        ("tradeId", idv()),
        ("instId", KIT.pick(INSTS)),
    ])
}

/// One `candle*` row: `[ts, o, h, l, c, vol, volCcy, volCcyQuote, confirm]`, decimal strings.
pub fn candle_row() -> BoxedStrategy<Value> {
    let item = prop_oneof![9 => KIT.num_text().prop_map(Value::String), 1 => KIT.arb_leaf()];
    (prop::collection::vec(item, 0..9), KIT.pick(&["0", "1"]))
        .prop_map(|(mut cells, confirm)| {
            cells.push(confirm);
            Value::Array(cells)
        })
        .boxed()
}

/// One `public/instruments` row.
pub fn inst_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("instId", KIT.pick(INSTS)),
        ("state", KIT.pick(&["live", "suspend", ""])),
        ("instType", KIT.pick(&["SWAP", "FUTURES", "SPOT", "OPTION", ""])),
        ("baseCcy", KIT.pick(&["BTC", ""])),
        ("quoteCcy", KIT.pick(&["USDT", ""])),
        ("ctValCcy", KIT.pick(&["BTC", ""])),
        ("tickSz", KIT.numv()),
        ("lotSz", KIT.numv()),
        ("minSz", KIT.numv()),
        ("maxMktSz", KIT.numv()),
        ("ctVal", KIT.numv()),
        ("ctMult", KIT.numv()),
        ("lever", KIT.numv()),
        ("ctType", KIT.pick(&["linear", "inverse", ""])),
        ("settleCcy", KIT.pick(&["USDT", "BTC", ""])),
        ("expTime", KIT.pick(&["1791792000000", "-1", "0", "99999999999999999999", ""])),
        ("alias", KIT.pick(&["this_week", "quarter", "this_five_years", ""])),
        ("optType", KIT.pick(&["C", "P", "X", ""])),
        ("stk", KIT.numv()),
        ("uly", KIT.pick(&["BTC-USD", "BTC", "-", ""])),
        ("instFamily", KIT.pick(&["BTC-USDT", ""])),
    ])
}

/// `{arg:{channel, instId}, data:[rows], action?, + a few stray envelope keys}`, each part
/// occasionally missing or the wrong type.
pub fn envelope(
    channels: &'static [&'static str],
    row: BoxedStrategy<Value>,
) -> BoxedStrategy<Value> {
    let arg = junk_or(obj_of(vec![("channel", KIT.pick(channels)), ("instId", KIT.pick(INSTS))]));
    let data = junk_or(array_of(row, 4));
    let action = KIT.pick(&["snapshot", "update", ""]);
    let extras = prop::collection::vec((weighted_key(8, ENVELOPE_KEYS), KIT.arb_leaf()), 0..3);
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
        ("code", KIT.pick(&["0", "0", "0", "50011", "51000", ""])),
        ("msg", KIT.arb_leaf()),
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
