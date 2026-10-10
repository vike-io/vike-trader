//! Totality harness for the OKX wire decoders: every function that turns raw SOCKET / REST input
//! (frame text, a `serde_json::Value`, a response body) into events, market data, catalog rows or
//! reconcile reports is fed (a) arbitrary text and lossy-decoded byte noise, (b) random JSON whose
//! object keys are drawn from the decoders' REAL field names (so the generator reaches the branches
//! instead of bouncing off the first `.get()`), (c) well-shaped envelopes whose fields are mostly
//! well-typed with hostile leaves (`NaN`, `inf`, `1e999`, `i64::MIN`, empty strings, wrong types),
//! and (d) short frame SEQUENCES into ONE book instance.
//!
//! The property is TOTALITY: a hostile or truncated frame may decode to nothing (or to `Err`), but
//! it must never panic the pump thread that reads the socket. Outputs are asserted only where a
//! cheap bound must always hold (no event flood).
//!
//! Covered (all through the PUBLIC API): `market_data::{route_frame, parse_books_frame, decode_bbo,
//! decode_trade}`, `market_feed::{route_frame, route_mark_frame, parse_trades}`, the depth fold
//! (`market_feed::okx_depth_decode` is `pub(crate)`, so `fold_deep` below restates it over its public
//! parts: `parse_books_frame` + `infer_tick_size` + `L2Book`), `data::parse_okx_klines`,
//! `catalog::{parse_insts, parse_underlyings}`; under the default-on `exec` feature also
//! `event_mapper`, `recon_client::parse_*`, `funding::decode_okx_funding_bills`,
//! `history::map_okx_history`, `account_config::parse_account_identity`, `ws_auth::match_event_ack`,
//! `transport::unwrap_okx`, `error_codes`, and `perp::{parse_okx_perp_instruments,
//! map_okx_open_order, OkxPerpRest::reconcile_positions / last_price}` behind a canned transport.
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::depth::infer_tick_size;
use vike_model::{AssetClass, L2Book};
use vike_okx::market_data::BooksFrame;
use vike_okx::{catalog, data, market_data, market_feed};

const INST: &str = "BTC-USDT-SWAP";

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

const INSTS: &[&str] = &["BTC-USDT-SWAP", "BTC-USDT", "ETH-USD-SWAP", "BTC-USD-241227-50000-C", ""];
const PUBLIC_CHANNELS: &[&str] =
    &["bbo-tbt", "trades", "books5", "books", "mark-price", "candle1m", "candle1H", "tickers"];
const BOOK_CHANNELS: &[&str] = &["books", "books", "books5", "bbo-tbt"];
const ENVELOPE_KEYS: &[&str] =
    &["event", "code", "msg", "action", "arg", "data", "channel", "instId", "op"];
/// Every real field name the public decoders read, plus the envelope keys.
const PUBLIC_KEYS: &[&str] = &[
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

/// A numeric wire field: usually a decimal STRING (the OKX convention), sometimes a JSON number or
/// any leaf.
fn numv() -> BoxedStrategy<Value> {
    prop_oneof![
        7 => num_text().prop_map(Value::String),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// A short id-shaped field (`tradeId`, `ordId`, `clOrdId`): usually non-empty.
fn idv() -> BoxedStrategy<Value> {
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
fn market_row() -> BoxedStrategy<Value> {
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
fn candle_row() -> BoxedStrategy<Value> {
    let item = prop_oneof![9 => num_text().prop_map(Value::String), 1 => arb_leaf()];
    (prop::collection::vec(item, 0..9), pick(&["0", "1"]))
        .prop_map(|(mut cells, confirm)| {
            cells.push(confirm);
            Value::Array(cells)
        })
        .boxed()
}

/// One `public/instruments` row.
fn inst_row() -> BoxedStrategy<Value> {
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
    ])
}

/// `{arg:{channel, instId}, data:[rows], action?, + a few stray envelope keys}`, each part
/// occasionally missing or the wrong type.
fn envelope(channels: &'static [&'static str], row: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
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
fn rest_envelope(data: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    obj_of(vec![
        ("code", pick(&["0", "0", "0", "50011", "51000", ""])),
        ("msg", arb_leaf()),
        ("data", junk_or(data)),
    ])
}

/// A frame as the socket delivers it: usually the serialized envelope, sometimes raw noise.
fn frame_text(
    channels: &'static [&'static str],
    row: BoxedStrategy<Value>,
) -> BoxedStrategy<String> {
    prop_oneof![9 => envelope(channels, row).prop_map(|v| v.to_string()), 1 => any::<String>()]
        .boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// --- the drivers ---------------------------------------------------------------------------

/// Restates `market_feed::okx_depth_decode` (`pub(crate)`, unreachable from here) over its public
/// parts, so the DOM fold's real inputs (`parse_books_frame`, `infer_tick_size`, the `L2Book`
/// reducer, the `prevSeqId` chain check) all run on the hostile frame.
fn fold_deep(text: &str, book: &mut Option<L2Book>) {
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
fn drive_public_text(text: &str) {
    let mut book = L2Book::new(0.01);
    let _ = market_data::route_frame(text, INST, &mut book);
    let _ = market_data::parse_books_frame(text);
    let _ = market_feed::route_frame(text);
    let _ = market_feed::route_mark_frame(text);
    let _ = data::parse_okx_klines(text);
    let mut deep = None;
    fold_deep(text, &mut deep);
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        drive_public_value(&v);
    }
}

/// Every keyless `Value` decoder on one parsed payload.
fn drive_public_value(v: &Value) {
    let _ = market_data::decode_bbo(v, INST);
    let _ = market_data::decode_trade(v, INST);
    let _ = market_feed::parse_trades(v);
    let _ = catalog::parse_underlyings(v);
    for class in [
        AssetClass::CryptoSpot,
        AssetClass::CryptoPerp,
        AssetClass::CryptoFuture,
        AssetClass::Option,
    ] {
        let _ = catalog::parse_insts(v, class);
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

    /// (b') well-shaped public envelopes with hostile leaves, whole and cut off mid-flight.
    #[test]
    fn public_decoders_are_total_over_envelopes(
        v in prop_oneof![
            envelope(PUBLIC_CHANNELS, market_row()),
            envelope(PUBLIC_CHANNELS, candle_row()),
            envelope(PUBLIC_CHANNELS, inst_row()),
        ],
        cut in 0usize..400,
    ) {
        drive_public_value(&v);
        let text = v.to_string();
        drive_public_text(&text);
        drive_public_text(&truncated(&text, cut));
    }

    /// (b'') the REST kline / instruments / underlying bodies.
    #[test]
    fn rest_bodies_are_total(
        klines in rest_envelope(array_of(candle_row(), 5)),
        insts in rest_envelope(array_of(inst_row(), 5)),
        families in rest_envelope(array_of(array_of(pick(INSTS), 4), 3)),
    ) {
        let _ = data::parse_okx_klines(&klines.to_string());
        for payload in [&insts, &families] {
            drive_public_value(payload);
        }
    }

    /// (c) a short frame sequence into ONE shallow book and ONE deep book: snapshot / update /
    /// gap / stale / junk in any order never panics, and the tick stays a usable grid.
    #[test]
    fn book_folds_survive_frame_sequences(
        frames in prop::collection::vec(frame_text(BOOK_CHANNELS, market_row()), 1..8),
    ) {
        let mut shallow = L2Book::new(0.01);
        let mut deep: Option<L2Book> = None;
        for f in &frames {
            let _ = market_data::route_frame(f, INST, &mut shallow);
            fold_deep(f, &mut deep);
            prop_assert!(shallow.tick_size > 0.0);
            if let Some(b) = &deep {
                prop_assert!(b.tick_size > 0.0);
            }
        }
    }
}

// --- the exec plane (default-on `exec` feature) ---------------------------------------------

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use proptest::test_runner::TestCaseError;
    use vike_bridge_core::credentials::Credentials;
    use vike_bridge_core::signer::OkxV5Signer;
    use vike_bridge_core::transport::VenueApiError;
    use vike_model::SymbolProperties;
    use vike_okx::perp::{OkxPerpRest, map_okx_open_order, parse_okx_perp_instruments};
    use vike_okx::transport::{OkxTransport, unwrap_okx};
    use vike_okx::{
        account_config, error_codes, event_mapper, funding, history, recon_client, ws_auth,
    };

    const PRIVATE_CHANNELS: &[&str] = &["orders", "orders", "account", "positions", ""];
    const PRIVATE_KEYS: &[&str] = &[
        "event",
        "arg",
        "data",
        "channel",
        "code",
        "msg",
        "state",
        "clOrdId",
        "ordId",
        "fillTime",
        "uTime",
        "posSide",
        "fillSz",
        "tradeId",
        "instId",
        "side",
        "fillPx",
        "fillFee",
        "fillFeeCcy",
        "execType",
        "accFillSz",
        "sz",
        "category",
        "px",
        "fillMarkPx",
        "markPx",
        "cancelSource",
        "details",
        "ccy",
        "cashBal",
    ];
    const ALL_KEYS: &[&str] = &[
        "data",
        "code",
        "msg",
        "instId",
        "ordId",
        "clOrdId",
        "tradeId",
        "side",
        "ordType",
        "sz",
        "px",
        "accFillSz",
        "avgPx",
        "state",
        "uTime",
        "ts",
        "fillSz",
        "fillPx",
        "fee",
        "feeCcy",
        "execType",
        "pos",
        "posSide",
        "mgnMode",
        "margin",
        "markPx",
        "details",
        "ccy",
        "cashBal",
        "makerU",
        "takerU",
        "maker",
        "taker",
        "type",
        "pnl",
        "balChg",
        "billId",
        "uid",
        "mainUid",
        "sCode",
        "sMsg",
        "event",
        "last",
        "instType",
        "tickSz",
        "lotSz",
        "minSz",
        "maxMktSz",
        "ctVal",
        "ctMult",
        "ctValCcy",
        "lever",
    ];

    /// One `orders` channel / `orders-pending` row.
    fn order_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("code", pick(&["0", "0", "0", "51000", ""])),
            ("msg", arb_leaf()),
            (
                "state",
                pick(&["live", "canceled", "mmp_canceled", "filled", "partially_filled", ""]),
            ),
            ("clOrdId", idv()),
            ("ordId", idv()),
            ("ordType", pick(&["limit", "market", "post_only", "LIMIT", ""])),
            ("fillTime", numv()),
            ("uTime", numv()),
            ("posSide", pick(&["long", "short", "net", ""])),
            ("fillSz", numv()),
            ("tradeId", idv()),
            ("instId", pick(INSTS)),
            ("side", pick(&["buy", "sell", "BUY", ""])),
            ("fillPx", numv()),
            ("fillFee", numv()),
            ("fillFeeCcy", pick(&["USDT", "BTC", ""])),
            ("execType", pick(&["M", "T", ""])),
            ("accFillSz", numv()),
            ("sz", numv()),
            ("px", numv()),
            ("avgPx", numv()),
            ("category", pick(&["normal", "full_liquidation", "partial_liquidation", "adl"])),
            ("fillMarkPx", numv()),
            ("markPx", numv()),
            ("cancelSource", arb_leaf()),
        ])
    }

    /// One `fills` / `fills-history` row.
    fn fill_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("tradeId", idv()),
            ("ordId", idv()),
            ("clOrdId", idv()),
            ("instId", pick(INSTS)),
            ("side", pick(&["buy", "sell", ""])),
            ("fillSz", numv()),
            ("fillPx", numv()),
            ("fee", numv()),
            ("feeCcy", pick(&["USDT", ""])),
            ("execType", pick(&["M", "T", ""])),
            ("fillMarkPx", numv()),
            ("ts", numv()),
        ])
    }

    /// One `account/positions` row.
    fn position_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("instId", pick(INSTS)),
            ("posSide", pick(&["long", "short", "net", ""])),
            ("pos", numv()),
            ("avgPx", numv()),
            ("markPx", numv()),
            ("uTime", numv()),
            ("mgnMode", pick(&["isolated", "cross", "ISOLATED", ""])),
            ("margin", numv()),
        ])
    }

    /// One `account/balance` / `account` channel entry.
    fn account_row() -> BoxedStrategy<Value> {
        let detail = obj_of(vec![("ccy", pick(&["USDT", "BTC", ""])), ("cashBal", numv())]);
        obj_of(vec![("uTime", numv()), ("details", junk_or(array_of(detail, 4)))])
    }

    /// One account-bill row (funding is `type` "8").
    fn bill_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("type", pick(&["8", "2", ""])),
            ("pnl", numv()),
            ("balChg", numv()),
            ("ts", numv()),
            ("billId", idv()),
            ("instId", pick(INSTS)),
        ])
    }

    /// One `trade-fee` row, one `account/config` row, one per-order result row.
    fn misc_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("makerU", numv()),
            ("takerU", numv()),
            ("maker", numv()),
            ("taker", numv()),
            ("uid", pick(&["111", " ", ""])),
            ("mainUid", pick(&["111", " ", ""])),
            ("sCode", pick(&["0", "51000", ""])),
            ("sMsg", arb_leaf()),
            ("last", numv()),
        ])
    }

    fn any_row() -> BoxedStrategy<Value> {
        prop_oneof![
            order_row(),
            fill_row(),
            position_row(),
            account_row(),
            bill_row(),
            misc_row(),
            inst_row(),
            arb_json(ALL_KEYS),
        ]
        .boxed()
    }

    fn rows(max: usize) -> BoxedStrategy<Value> {
        array_of(any_row(), max)
    }

    fn ct_val() -> BoxedStrategy<f64> {
        prop_oneof![Just(0.01), Just(1.0), Just(0.0), Just(-1.0), any::<f64>()].boxed()
    }

    /// The three private-WS dispatchers on one frame; no event flood (a row yields <= 2 events,
    /// the account arm <= 1).
    fn check_private(frame: &Value, ct: f64) -> Result<(), TestCaseError> {
        let n_rows = frame.get("data").and_then(Value::as_array).map_or(0, Vec::len);
        let bound = 2 * n_rows + 1;
        let spot = event_mapper::map_okx_private(frame, "okx", INST);
        prop_assert!(spot.len() <= bound, "spot flood: {} events from {n_rows} rows", spot.len());
        let perp = event_mapper::map_okx_perp(frame, "okx", INST, ct);
        prop_assert!(perp.len() <= bound, "perp flood: {} events from {n_rows} rows", perp.len());
        let one = event_mapper::map_okx_order(frame, "okx", INST);
        prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
        Ok(())
    }

    /// Every REST-body text decoder of the reconcile client on one body.
    fn drive_recon_text(body: &str, ct: f64) {
        let _ = recon_client::parse_orders_pending(body, ct);
        let _ = recon_client::parse_fills(body, ct);
        let _ = recon_client::parse_positions(body, INST, ct);
        let _ = recon_client::parse_balance(body);
        let _ = recon_client::parse_fee_rate(body);
    }

    /// Every exec-plane `Value` decoder on one payload.
    fn drive_exec_value(v: &Value, ct: f64) {
        let _ = recon_client::parse_mgn_mode(v);
        let _ = account_config::parse_account_identity(v);
        let _ = parse_okx_perp_instruments(v);
        let _ = unwrap_okx(v.clone());
        for event in ["login", "subscribe", "error", ""] {
            let _ = ws_auth::match_event_ack(v, event);
        }
        let empty = Value::Null;
        let _ = history::map_okx_history(v, &empty, "okx", INST, ct);
        let _ = history::map_okx_history(&empty, v, "okx", INST, ct);
        let _ = history::map_okx_history(v, v, "okx", INST, ct);
        if let Some(bills) = v.as_array() {
            let _ = funding::decode_okx_funding_bills(bills, "okx", INST);
        }
    }

    /// Answers every request, signed or public, with the same canned body: drives the decoders
    /// INSIDE `OkxPerpRest` (`reconcile_positions`, `fetch_usdt_balance`, `fetch_open_orders`,
    /// `last_price`) that no free function exposes.
    struct Canned(Value);

    impl OkxTransport for Canned {
        fn signed(
            &self,
            _base_url: &str,
            _path: &str,
            _method: &str,
            _params: &[(&str, Value)],
            _signer: &OkxV5Signer,
        ) -> Result<Value, VenueApiError> {
            Ok(self.0.clone())
        }

        fn public(
            &self,
            _base_url: &str,
            _path: &str,
            _params: &[(&str, String)],
        ) -> Result<Value, VenueApiError> {
            Ok(self.0.clone())
        }
    }

    fn rest_over(body: Value, ct: f64) -> OkxPerpRest<Canned> {
        OkxPerpRest {
            signer: OkxV5Signer::new(
                &Credentials {
                    api_key: "k".into(),
                    api_secret: "s".into(),
                    passphrase: Some("p".into()),
                },
                || 0,
            ),
            transport: Canned(body),
            base_url: "https://unused.invalid".to_string(),
            symbol: INST.to_string(),
            properties: SymbolProperties {
                tick_size: 0.1,
                step_size: 0.01,
                min_qty: 0.01,
                ..Default::default()
            },
            ct_val: ct,
            leverage: 2.0,
            broker_code: None,
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a) arbitrary text / byte noise through every reconcile-body parser.
        #[test]
        fn recon_parsers_are_total_over_text(text in any::<String>(), ct in ct_val()) {
            drive_recon_text(&text, ct);
        }

        #[test]
        fn recon_parsers_survive_byte_noise(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            ct in ct_val(),
        ) {
            drive_recon_text(&String::from_utf8_lossy(&bytes), ct);
        }

        /// (b) the reconcile bodies are JSON ARRAYS of rows: serialized arrays of every row shape,
        /// whole and cut off; a valid array must parse to exactly one report per row.
        #[test]
        fn recon_parsers_are_total_over_row_arrays(
            arr in rows(6),
            ct in ct_val(),
            cut in 0usize..600,
        ) {
            let n = arr.as_array().map_or(0, Vec::len);
            let body = arr.to_string();
            drive_recon_text(&body, ct);
            drive_recon_text(&truncated(&body, cut), ct);
            let pending = recon_client::parse_orders_pending(&body, ct);
            prop_assert!(matches!(&pending, Ok(r) if r.len() == n), "one report per row");
            let positions = recon_client::parse_positions(&body, INST, ct);
            prop_assert!(matches!(&positions, Ok(r) if !r.is_empty()), "never an empty snapshot");
            let fills = recon_client::parse_fills(&body, ct);
            prop_assert!(matches!(&fills, Ok(r) if r.len() <= n), "a fill report per row at most");
        }

        /// (b') arbitrary JSON over the real field names through every reconcile parser and every
        /// exec-plane value decoder.
        #[test]
        fn exec_decoders_are_total_over_arbitrary_json(v in arb_json(ALL_KEYS), ct in ct_val()) {
            drive_recon_text(&v.to_string(), ct);
            drive_exec_value(&v, ct);
        }

        /// (b'') the private WS dispatchers over arbitrary JSON.
        #[test]
        fn private_mappers_are_total_over_arbitrary_json(
            frame in arb_json(PRIVATE_KEYS),
            ct in ct_val(),
        ) {
            check_private(&frame, ct)?;
        }

        /// (b''') ...and over well-shaped `orders` / `account` envelopes whose rows reach the fill,
        /// liquidation and lifecycle arms.
        #[test]
        fn private_mappers_are_total_over_envelopes(
            frame in envelope(
                PRIVATE_CHANNELS,
                prop_oneof![order_row(), account_row(), arb_json(PRIVATE_KEYS)].boxed(),
            ),
            ct in ct_val(),
        ) {
            check_private(&frame, ct)?;
            drive_exec_value(&frame, ct);
        }

        /// ...and one `orders` row at a time, straight into the row mapper.
        #[test]
        fn order_row_mapper_is_total(row in order_row(), ct in ct_val()) {
            let one = event_mapper::map_okx_order(&row, "okx", INST);
            prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
            let frame = serde_json::json!({
                "arg": {"channel": "orders", "instId": INST},
                "data": [row],
            });
            check_private(&frame, ct)?;
        }

        /// (c) a short sequence of private frames: the mappers hold no state, so a sequence is
        /// just N independent frames — kept to prove it stays that way.
        #[test]
        fn private_frame_sequences_never_panic(
            frames in prop::collection::vec(
                envelope(PRIVATE_CHANNELS, prop_oneof![order_row(), account_row()].boxed()),
                1..8,
            ),
            ct in ct_val(),
        ) {
            for f in &frames {
                check_private(f, ct)?;
            }
        }

        /// The history replay and the funding-bill decoder over arrays of fill / order / bill rows.
        #[test]
        fn history_and_funding_are_total(
            orders in junk_or(array_of(order_row(), 5)),
            fills in junk_or(array_of(fill_row(), 5)),
            bills in prop::collection::vec(prop_oneof![bill_row(), arb_json(ALL_KEYS)], 0..6),
            ct in ct_val(),
        ) {
            let evs = history::map_okx_history(&orders, &fills, "okx", INST, ct);
            let n_orders = orders.as_array().map_or(0, Vec::len);
            let n_fills = fills.as_array().map_or(0, Vec::len);
            // each order replays at most its own fills (a fill row joins to every order sharing its
            // ordId), two events per fill, plus one cancel
            prop_assert!(evs.len() <= n_orders * (2 * n_fills + 1), "history flood");
            let out = funding::decode_okx_funding_bills(&bills, "okx", INST);
            prop_assert!(out.len() <= bills.len(), "more funding events than bills");
        }

        /// Small string / number surfaces: error classification, ack matching, envelope unwrap,
        /// account identity.
        #[test]
        fn small_decoders_are_total(
            msg in any::<String>(),
            code in any::<i64>(),
            event in prop_oneof![Just("login".to_string()), Just("subscribe".to_string()), any::<String>()],
            v in prop_oneof![
                rest_envelope(array_of(misc_row(), 3)),
                envelope(PRIVATE_CHANNELS, misc_row()),
                arb_json(ALL_KEYS),
            ],
        ) {
            let _ = error_codes::by_msg(&msg);
            let _ = error_codes::by_code(code);
            let _ = ws_auth::match_event_ack(&v, &event);
            let _ = unwrap_okx(v.clone());
            let _ = account_config::parse_account_identity(&v);
        }

        /// The decoders inside `OkxPerpRest`, behind a canned transport: positions (`pos` x
        /// `ct_val`), the USDT balance, the resting-order seed, the ticker.
        #[test]
        fn perp_rest_decoders_are_total(
            body in rest_envelope(array_of(any_row(), 5)),
            ct in ct_val(),
        ) {
            let rest = rest_over(body.clone(), ct);
            if let Ok(snap) = rest.reconcile_positions() {
                prop_assert!(!snap.positions.is_empty(), "a snapshot always names the symbol");
            }
            let _ = rest.last_price();
            let _ = parse_okx_perp_instruments(&body);
        }

        /// ⚠ `perp::map_okx_open_order` on one `orders-pending` row. Suspected `.expect("static
        /// shape")` panic when `sz * ct_val` is not finite (`sz` = "1e999" / "inf" / "NaN"): the
        /// `json!` image of a non-finite `qty` is `null`, which `OrderRequest.qty: f64` refuses.
        #[test]
        fn open_order_mapper_is_total(row in order_row(), ct in ct_val()) {
            let _ = map_okx_open_order(&row, ct);
        }
    }
}
