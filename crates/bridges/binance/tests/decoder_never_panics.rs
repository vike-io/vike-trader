//! "Arbitrary input never panics" harness for the binance wire decoders that `mapper_props.rs`
//! does NOT reach. That file already feeds arbitrary JSON to `map_execution_report` /
//! `map_binance_private` and arbitrary text to `route_frame`; this one covers the rest of the
//! crate's inbound surface, entered through the public `family` rungs (the shared Binance-grammar
//! core that `vike-aster` also calls, so the same properties protect aster's wrappers):
//!
//! - FEEDS plane (always compiled): `family::depth` (`decode_book_ticker`, `decode_trade`,
//!   `apply_depth_event`, `parse_depth_snapshot`, and `route_frame` as a SEQUENCE into one
//!   `L2Book`), `family::market_feed` (`decode_kline_frame`, `decode_mark_frame`), `family::klines`
//!   (`parse_klines`, `parse_klines_with`), `family::trades` (the five aggTrade/raw-trade
//!   decoders), `family::catalog`, `instruments::parse_coin_m_contracts`,
//!   `data::parse_funding_rates`;
//! - EXEC plane (`cfg(feature = "exec")`, default-on): `family::perp_mapper::map_perp_opts`, the
//!   structured spot frames through `family::event_mapper`, `family::history`, every
//!   `family::recon` body parser, `family::order_map`, `perp::parse_binance_perp_instruments`,
//!   `spot::{parse_commission_rates, parse_account_identity}`, `key_permissions`,
//!   `ws_auth::match_subscribe_ack`.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing or an
//! `Err`, but it must never panic a pump thread (a dead thread is a venue that silently goes
//! quiet), and one frame must never fabricate an event flood. Each decoder is fed (a) arbitrary
//! text, (b) lossy-decoded byte noise and (c) structured JSON whose object keys are the decoder's
//! REAL field names and whose leaves are the dispatch tokens and the hostile numerics (`"NaN"`,
//! `"1e999"`, `i64::MIN`, ...), so the generator reaches the match arms instead of bouncing off the
//! first `.get()`. Where a decoder folds into state (`L2Book`) a short random sequence goes into
//! ONE instance. Outputs are asserted only where a cheap invariant must always hold.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it; the mappers are a shared home (binance + aster), so the fix is its own
//! coordinated PR.
//!
//! Features: none beyond the crate's defaults. With `--no-default-features` only the feeds-plane
//! half compiles.

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_binance::data::parse_funding_rates;
use vike_binance::family::catalog as family_catalog;
use vike_binance::family::depth::{
    DepthOutcome, MdEvent, apply_depth_event, decode_book_ticker, decode_trade,
    parse_depth_snapshot, route_frame,
};
use vike_binance::family::klines::{VolumeColumn, parse_klines, parse_klines_with};
use vike_binance::family::market_feed::{decode_kline_frame, decode_mark_frame};
use vike_binance::family::trades::{
    parse_agg_trades_page, rest_agg_trades, rest_raw_trades, ws_agg_trade, ws_raw_trade,
};
use vike_binance::instruments::parse_coin_m_contracts;
use vike_bridge_core::depth::infer_tick_size;
use vike_model::{BookLevel, L2Book};

/// One frame legitimately emits a handful of events (the dual-publish fill contract; one funding
/// event per wallet row, rows <= 6 in every generator); more than this from ONE frame is a mapper
/// bug regardless of input.
#[cfg(feature = "exec")]
const MAX_EVENTS_PER_FRAME: usize = 16;

/// Every real field name across the decoders in this file (ws frames, REST rows, exchangeInfo,
/// account/balance bodies), so a random object reaches the keyed `.get()` arms.
const KEYS: &[&str] = &[
    "e",
    "E",
    "T",
    "s",
    "S",
    "c",
    "C",
    "o",
    "x",
    "X",
    "i",
    "t",
    "p",
    "q",
    "P",
    "l",
    "L",
    "n",
    "N",
    "m",
    "r",
    "ps",
    "a",
    "b",
    "B",
    "A",
    "f",
    "u",
    "U",
    "pu",
    "k",
    "v",
    "stream",
    "data",
    "event",
    "status",
    "result",
    "error",
    "id",
    "orderId",
    "clientOrderId",
    "side",
    "type",
    "origQty",
    "executedQty",
    "cummulativeQuoteQty",
    "avgPrice",
    "price",
    "qty",
    "time",
    "updateTime",
    "commission",
    "commissionAsset",
    "isBuyer",
    "isMaker",
    "maker",
    "positionSide",
    "positionAmt",
    "entryPrice",
    "marginType",
    "isolatedWallet",
    "isolatedMargin",
    "asset",
    "balance",
    "balances",
    "free",
    "locked",
    "symbol",
    "symbols",
    "baseAsset",
    "quoteAsset",
    "contractType",
    "contractStatus",
    "filters",
    "filterType",
    "tickSize",
    "stepSize",
    "minQty",
    "maxQty",
    "minNotional",
    "notional",
    "lastUpdateId",
    "bids",
    "asks",
    "makerCommissionRate",
    "takerCommissionRate",
    "commissionRates",
    "maker",
    "taker",
    "uid",
    "enableWithdrawals",
    "enableSpotAndMarginTrading",
    "ipRestrict",
    "fundingTime",
    "fundingRate",
    "markPrice",
    "msg",
    "code",
];

/// Dispatch tokens: the string values the decoders `match` on, so structured frames take real arms.
const TOKENS: &[&str] = &[
    "executionReport",
    "outboundAccountPosition",
    "balanceUpdate",
    "ORDER_TRADE_UPDATE",
    "ACCOUNT_UPDATE",
    "TRADE_LITE",
    "markPriceUpdate",
    "kline",
    "trade",
    "aggTrade",
    "depthUpdate",
    "NEW",
    "TRADE",
    "CANCELED",
    "EXPIRED",
    "REJECTED",
    "FILLED",
    "PARTIALLY_FILLED",
    "BUY",
    "SELL",
    "BOTH",
    "LONG",
    "SHORT",
    "FUNDING_FEE",
    "TRADING",
    "PERPETUAL",
    "CURRENT_QUARTER",
    "USDT",
    "BTCUSDT",
    "isolated",
    "cross",
    "LIMIT",
    "MARKET",
    "x-AB-12-deadbeef",
    "autoclose-1",
    "adl_autoclose",
    "settlement_autoclose-2",
];

/// Hostile numeric / textual leaves: everything `str::parse::<f64>` accepts that a venue never
/// sends (`NaN`, `inf`), everything it rejects that looks numeric, and the empty string.
const SPECIAL: &[&str] = &[
    "NaN",
    "nan",
    "inf",
    "-inf",
    "Infinity",
    "-Infinity",
    "-0",
    "0",
    "+0",
    "1e999",
    "-1e999",
    "1e-999",
    "",
    "  ",
    "0x10",
    ".",
    "-",
    "+5",
    "1_000",
    "\u{0661}\u{0662}\u{0663}",
    "9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "0.1e",
    "e5",
];

/// Venue-shaped stream names for the combined-stream router.
const STREAMS: &[&str] = &[
    "btcusdt@bookTicker",
    "btcusdt@trade",
    "btcusdt@depth@100ms",
    "btcusdt@aggTrade",
    "btcusdt@kline_1m",
    "",
    "@depth",
    "@",
];

fn tok(list: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop::sample::select(list.to_vec()).prop_map(|s| Value::String(s.to_string())).boxed()
}

/// A decimal-looking string (what venues send numbers as).
fn numeric_string() -> BoxedStrategy<String> {
    prop_oneof!["[0-9]{1,6}", "-?[0-9]{1,8}\\.[0-9]{1,8}", "[0-9]{1,3}e-?[0-9]{1,3}",].boxed()
}

/// A numeric string value, mostly well-formed, sometimes `NaN` / `1e999` / empty.
fn num_str() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => numeric_string().prop_map(Value::String),
        1 => tok(SPECIAL),
    ]
    .boxed()
}

/// An i64 JSON number: realistic epoch-ms, zero, negative, extremes.
fn int() -> BoxedStrategy<Value> {
    prop_oneof![
        Just(0i64),
        Just(-1i64),
        Just(i64::MIN),
        Just(i64::MAX),
        1_600_000_000_000i64..1_800_000_000_000i64,
        any::<i64>(),
    ]
    .prop_map(|n| json!(n))
    .boxed()
}

/// A u64 id / sequence number: small (so sequence rules hit equal/adjacent values), the u64
/// extremes, or anything.
fn seq() -> BoxedStrategy<Value> {
    prop_oneof![0u64..24, Just(u64::MAX), Just(u64::MAX - 1), any::<u64>()]
        .prop_map(|n| json!(n))
        .boxed()
}

/// Any leaf the venue could (mis)send: null, bool, i64 extremes, u64::MAX, f64, numeric strings,
/// hostile numeric strings, dispatch tokens, unicode noise, and the empty array / object.
fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        2 => Just(Value::Null),
        2 => any::<bool>().prop_map(Value::Bool),
        3 => int(),
        1 => Just(Value::Number(u64::MAX.into())),
        2 => any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        4 => numeric_string().prop_map(Value::String),
        3 => tok(SPECIAL),
        4 => tok(TOKENS),
        2 => "\\PC{0,12}".prop_map(Value::String),
        1 => Just(json!([])),
        1 => Just(json!({})),
    ]
    .boxed()
}

fn arb_key() -> BoxedStrategy<String> {
    prop_oneof![
        5 => prop::sample::select(KEYS.to_vec()).prop_map(|s| s.to_string()),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Arbitrary JSON, depth <= 4, keys from the real vocabulary.
fn arb_json() -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Arbitrary text: printable-or-not unicode, lossy-decoded byte noise, or valid JSON text.
fn arb_text() -> BoxedStrategy<String> {
    prop_oneof![
        any::<String>(),
        prop::collection::vec(any::<u8>(), 0..256)
            .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
        arb_json().prop_map(|v| v.to_string()),
    ]
    .boxed()
}

/// One optional `"key": value` pair: usually present with a value of the right shape, sometimes
/// with a hostile leaf, sometimes absent.
type Field = BoxedStrategy<Option<(String, Value)>>;

fn field(key: &'static str, valid: BoxedStrategy<Value>) -> Field {
    let value = prop_oneof![8 => valid, 2 => arb_leaf()];
    prop_oneof![
        9 => value.prop_map(move |v| Some((key.to_string(), v))),
        1 => Just(None),
    ]
    .boxed()
}

fn object(fields: Vec<Field>) -> BoxedStrategy<Value> {
    fields.prop_map(|fs| Value::Object(fs.into_iter().flatten().collect())).boxed()
}

/// A body as a venue REST endpoint would send it: junk text, JSON noise, an array of `rows`, or a
/// single row (the wrong top-level shape).
fn arb_body(rows: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![
        1 => arb_text(),
        2 => arb_json().prop_map(|v| v.to_string()),
        6 => prop::collection::vec(rows.clone(), 0..6).prop_map(|r| Value::Array(r).to_string()),
        1 => rows.prop_map(|r| r.to_string()),
    ]
    .boxed()
}

// ---------------------------------------------------------------------------------------------
// Feeds-plane generators
// ---------------------------------------------------------------------------------------------

/// `[price, qty]` book levels, mostly well-formed.
fn book_side() -> BoxedStrategy<Value> {
    let level = prop_oneof![
        8 => (num_str(), num_str()).prop_map(|(p, q)| json!([p, q])),
        1 => arb_json(),
    ];
    prop_oneof![
        9 => prop::collection::vec(level, 0..8).prop_map(Value::Array),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A `depthUpdate` payload under either sync grammar (`pu` present = futures, absent = spot).
fn depth_data() -> BoxedStrategy<Value> {
    object(vec![
        field("e", tok(TOKENS)),
        field("E", int()),
        field("U", seq()),
        field("u", seq()),
        field("pu", seq()),
        field("b", book_side()),
        field("a", book_side()),
    ])
}

fn quote_data() -> BoxedStrategy<Value> {
    object(vec![
        field("b", num_str()),
        field("a", num_str()),
        field("B", num_str()),
        field("A", num_str()),
    ])
}

fn trade_data() -> BoxedStrategy<Value> {
    object(vec![
        field("T", int()),
        field("p", num_str()),
        field("q", num_str()),
        field("m", any::<bool>().prop_map(Value::Bool).boxed()),
        field("t", seq()),
    ])
}

/// One combined-stream frame: `{"stream":…,"data":…}`, or arbitrary JSON.
fn stream_frame() -> BoxedStrategy<Value> {
    let data = prop_oneof![depth_data(), depth_data(), quote_data(), trade_data(), arb_json()];
    prop_oneof![
        9 => (prop::sample::select(STREAMS.to_vec()), data)
            .prop_map(|(s, d)| json!({ "stream": s, "data": d })),
        1 => arb_json(),
    ]
    .boxed()
}

/// A REST depth snapshot body.
fn snapshot_body() -> BoxedStrategy<String> {
    let obj = object(vec![
        field("lastUpdateId", seq()),
        field("bids", book_side()),
        field("asks", book_side()),
    ]);
    prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => arb_text()].boxed()
}

fn tick_size() -> BoxedStrategy<f64> {
    prop_oneof![
        Just(0.01),
        Just(1.0),
        Just(0.0),
        Just(-1.0),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(1e-300),
        Just(1e300),
        any::<f64>(),
    ]
    .boxed()
}

/// A raw kline row (the REST 12-element array), first cell an i64 time, the rest decimal strings.
fn kline_row() -> BoxedStrategy<Value> {
    let cell = prop_oneof![8 => numeric_string().prop_map(Value::String), 2 => arb_leaf()];
    let row = (prop_oneof![8 => int(), 2 => arb_leaf()], prop::collection::vec(cell, 0..12))
        .prop_map(|(t, cells)| {
            let mut row = vec![t];
            row.extend(cells);
            Value::Array(row)
        });
    prop_oneof![9 => row, 1 => arb_json()].boxed()
}

fn ws_kline_frame() -> BoxedStrategy<Value> {
    let k = object(vec![
        field("t", int()),
        field("o", num_str()),
        field("h", num_str()),
        field("l", num_str()),
        field("c", num_str()),
        field("v", num_str()),
        field("x", any::<bool>().prop_map(Value::Bool).boxed()),
    ]);
    prop_oneof![
        9 => (tok(TOKENS), int(), k).prop_map(|(e, ts, k)| json!({ "e": e, "E": ts, "k": k })),
        1 => arb_json(),
    ]
    .boxed()
}

fn ws_mark_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => object(vec![
            field("e", Just(Value::String("markPriceUpdate".into())).boxed()),
            field("E", int()),
            field("p", num_str()),
        ]),
        2 => arb_json(),
    ]
    .boxed()
}

/// A trade record in any of the three wire spellings (ws `@aggTrade`, ws `@trade`, REST raw).
fn trade_record() -> BoxedStrategy<Value> {
    let bool_v = any::<bool>().prop_map(Value::Bool).boxed();
    prop_oneof![
        object(vec![
            field("e", tok(TOKENS)),
            field("a", seq()),
            field("p", num_str()),
            field("q", num_str()),
            field("T", int()),
            field("m", bool_v.clone()),
        ]),
        object(vec![
            field("e", tok(TOKENS)),
            field("t", seq()),
            field("p", num_str()),
            field("q", num_str()),
            field("T", int()),
            field("m", bool_v.clone()),
        ]),
        object(vec![
            field("id", seq()),
            field("price", num_str()),
            field("qty", num_str()),
            field("time", int()),
            field("isBuyerMaker", bool_v),
        ]),
        arb_json(),
    ]
    .boxed()
}

/// An `exchangeInfo` payload (spot, fapi or dapi flavour).
fn exchange_info() -> BoxedStrategy<Value> {
    let filter = object(vec![
        field(
            "filterType",
            tok(&["PRICE_FILTER", "LOT_SIZE", "MARKET_LOT_SIZE", "MIN_NOTIONAL", "NOTIONAL"]),
        ),
        field("tickSize", num_str()),
        field("stepSize", num_str()),
        field("minQty", num_str()),
        field("maxQty", num_str()),
        field("minNotional", num_str()),
        field("notional", num_str()),
    ]);
    let entry = object(vec![
        field("symbol", tok(TOKENS)),
        field("baseAsset", tok(TOKENS)),
        field("quoteAsset", tok(TOKENS)),
        field("status", tok(TOKENS)),
        field("contractStatus", tok(TOKENS)),
        field("contractType", tok(TOKENS)),
        field("filters", prop::collection::vec(filter, 0..6).prop_map(Value::Array).boxed()),
    ]);
    prop_oneof![
        8 => prop::collection::vec(entry, 0..5).prop_map(|e| json!({ "symbols": e })),
        2 => arb_json(),
    ]
    .boxed()
}

/// Run `book` through a sequence of frames, checking the two state invariants of the depth fold:
/// `last_seq` never goes backwards, and a `BookUpdated` always moved it forward.
fn fold_frames(book: &mut L2Book, frames: &[Value]) -> Result<(), TestCaseError> {
    for frame in frames {
        let before = book.last_seq;
        let out = route_frame(&frame.to_string(), "BTCUSDT", book);
        prop_assert!(book.last_seq >= before, "last_seq regressed {} -> {}", before, book.last_seq);
        if matches!(out, MdEvent::BookUpdated) {
            prop_assert!(book.last_seq > before, "BookUpdated without a sequence advance");
        }
        // The state must stay readable after any fold.
        let _ = (book.best_bid(), book.best_ask(), book.mid(), book.spread(), book.top_n(5));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every `&str` decoder of the feeds plane is total over arbitrary text; the ones with a
    /// validity guard keep it.
    #[test]
    fn feeds_text_decoders_are_total(text in arb_text()) {
        let _ = decode_kline_frame(&text);
        if let Ok(Some(m)) = decode_mark_frame(&text) {
            prop_assert!(m.px.is_finite() && m.px > 0.0, "a dead mark escaped: {}", m.px);
        }
        for t in ws_agg_trade("BTCUSDT", &text).into_iter().chain(ws_raw_trade("BTCUSDT", &text)) {
            prop_assert!(t.tick.price.is_finite() && t.tick.price > 0.0);
            prop_assert!(t.tick.size.is_finite() && t.tick.size > 0.0);
        }
        for t in rest_agg_trades("BTCUSDT", &text).into_iter().chain(rest_raw_trades("BTCUSDT", &text)) {
            prop_assert!(t.tick.price.is_finite() && t.tick.price > 0.0);
            prop_assert!(t.tick.size.is_finite() && t.tick.size > 0.0);
        }
        let _ = parse_agg_trades_page("binance", "BTCUSDT", &text);
        let _ = parse_klines(&text);
        let _ = parse_klines_with(&text, VolumeColumn::Index7);
        let _ = parse_depth_snapshot(&text);
        let _ = parse_funding_rates(&text);
    }

    /// The same decoders over the byte noise printable strategies never generate.
    #[test]
    fn feeds_text_decoders_survive_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let _ = decode_kline_frame(&text);
        let _ = decode_mark_frame(&text);
        let _ = ws_agg_trade("BTCUSDT", &text);
        let _ = ws_raw_trade("BTCUSDT", &text);
        let _ = rest_agg_trades("BTCUSDT", &text);
        let _ = rest_raw_trades("BTCUSDT", &text);
        let _ = parse_agg_trades_page("binance", "BTCUSDT", &text);
        let _ = parse_klines(&text);
        let _ = parse_depth_snapshot(&text);
        let _ = parse_funding_rates(&text);
    }

    /// `decode_book_ticker` / `decode_trade` / `apply_depth_event` over the structured payloads.
    #[test]
    fn depth_payload_decoders_are_total(
        data in prop_oneof![depth_data(), quote_data(), trade_data(), arb_json()],
        tick in tick_size(),
        last_seq in prop_oneof![0u64..24, Just(u64::MAX), any::<u64>()],
    ) {
        let _ = decode_book_ticker(&data, "BTCUSDT");
        let _ = decode_trade(&data, "BTCUSDT");
        let mut book = L2Book::new(tick);
        book.last_seq = last_seq;
        let before = book.last_seq;
        let out = apply_depth_event(&mut book, &data);
        if matches!(out, DepthOutcome::Stale | DepthOutcome::Gap | DepthOutcome::Ignored) {
            prop_assert_eq!(book.last_seq, before, "a non-applied diff moved last_seq");
        }
    }

    /// (c) `route_frame` folds a SEQUENCE of 1..8 frames into ONE book: it never panics and the
    /// sequence number only moves forward.
    #[test]
    fn route_frame_sequences_into_one_book(
        frames in prop::collection::vec(stream_frame(), 1..8),
        tick in tick_size(),
        seed in prop_oneof![0u64..24, Just(u64::MAX)],
    ) {
        let mut book = L2Book::new(tick);
        book.last_seq = seed;
        fold_frames(&mut book, &frames)?;
    }

    /// The seeding path the DOM lane and the tick pump share: REST snapshot body -> tick inference
    /// -> `apply_snapshot` -> diffs. NaN prices are filtered before `infer_tick_size`: its
    /// `partial_cmp(..).unwrap_or(Equal)` sort comparator is not a total order over NaN (see the
    /// suspected-panic note in the crate report), and the venue never sends one.
    #[test]
    fn snapshot_seed_then_diffs_never_panics(
        body in snapshot_body(),
        frames in prop::collection::vec(stream_frame(), 0..8),
    ) {
        if let Ok((seq, bids, asks)) = parse_depth_snapshot(&body) {
            let keep = |side: Vec<BookLevel>| -> Vec<BookLevel> {
                side.into_iter().filter(|l| !l.price.is_nan()).collect()
            };
            let (bids, asks) = (keep(bids), keep(asks));
            let mut book = L2Book::new(infer_tick_size(&bids, &asks));
            book.apply_snapshot(seq, &bids, &asks);
            fold_frames(&mut book, &frames)?;
        }
    }

    /// `@kline_<interval>` and `@markPriceUpdate` frames with the real field names.
    #[test]
    fn ws_kline_and_mark_frames_are_total(
        kline in ws_kline_frame(),
        mark in ws_mark_frame(),
    ) {
        let _ = decode_kline_frame(&kline.to_string());
        if let Ok(Some(m)) = decode_mark_frame(&mark.to_string()) {
            prop_assert!(m.px.is_finite() && m.px > 0.0);
        }
    }

    /// REST kline bodies: the two volume columns differ in `volume` ONLY.
    #[test]
    fn kline_bodies_are_total(body in arb_body(kline_row())) {
        let a = parse_klines(&body);
        let b = parse_klines_with(&body, VolumeColumn::Index7);
        if let (Ok(a), Ok(b)) = (a, b) {
            prop_assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(&b) {
                prop_assert_eq!(x.ts, y.ts);
                prop_assert_eq!(x.open.to_bits(), y.open.to_bits());
                prop_assert_eq!(x.close.to_bits(), y.close.to_bits());
            }
        }
    }

    /// Trade records (ws single object, REST array) through all five trade decoders.
    #[test]
    fn trade_decoders_are_total(records in prop::collection::vec(trade_record(), 0..6)) {
        let array = Value::Array(records.clone()).to_string();
        let _ = rest_agg_trades("BTCUSDT", &array);
        let _ = rest_raw_trades("BTCUSDT", &array);
        let _ = parse_agg_trades_page("binance", "BTCUSDT", &array);
        for r in &records {
            let one = r.to_string();
            let _ = ws_agg_trade("BTCUSDT", &one);
            let _ = ws_raw_trade("BTCUSDT", &one);
            let _ = rest_agg_trades("BTCUSDT", &one);
            let _ = parse_agg_trades_page("binance", "BTCUSDT", &one);
        }
    }

    /// REST depth snapshot bodies.
    #[test]
    fn depth_snapshot_bodies_are_total(body in snapshot_body()) {
        let _ = parse_depth_snapshot(&body);
    }

    /// `exchangeInfo` payloads through the catalog and COIN-M parsers; the perp catalog only ever
    /// mints `.P` labels.
    #[test]
    fn exchange_info_parsers_are_total(payload in exchange_info()) {
        let _ = family_catalog::parse_spot(&payload, "binance");
        for inst in family_catalog::parse_perp(&payload, "aster") {
            prop_assert!(inst.raw_symbol.ends_with(vike_catalog::PERP_SUFFIX));
        }
        let _ = parse_coin_m_contracts(&payload);
    }

    /// Funding-rate history bodies (same row shape on fapi and dapi).
    #[test]
    fn funding_bodies_are_total(
        body in arb_body(
            object(vec![
                field("symbol", tok(TOKENS)),
                field("fundingTime", int()),
                field("fundingRate", num_str()),
                field("markPrice", num_str()),
            ])
        )
    ) {
        let _ = parse_funding_rates(&body);
    }
}

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use vike_binance::family::event_mapper::{map_execution_report, map_private};
    use vike_binance::family::history::{map_history, map_perp_history};
    use vike_binance::family::order_map::{
        map_perp_open_order, parse_symbol_properties, strip_broker_coid_prefix,
    };
    use vike_binance::family::perp_mapper::map_perp_opts;
    use vike_binance::family::recon::{
        fill_is_maker, fill_side, parse_commission_rate_body, parse_margin_type,
        parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
        parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
        parse_spot_open_orders,
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
                tok(&[
                    "NEW",
                    "CANCELED",
                    "EXPIRED",
                    "REJECTED",
                    "TRADE",
                    "CALCULATED",
                    "AMENDMENT",
                ]),
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
        object(vec![
            field("asset", tok(TOKENS)),
            field("balance", num_str()),
            field("free", num_str()),
        ])
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
}
