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
use vike_bridge_core::json_arb::{self, TokenKit, int, numeric_string, object, tick_size, tok};
use vike_marketdata::{BookLevel, L2Book};

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

/// A u64 id / sequence number: small (so sequence rules hit equal/adjacent values), the u64
/// extremes, or anything.
fn seq() -> BoxedStrategy<Value> {
    json_arb::seq(24)
}

/// Binance's vocabulary over the shared generator shapes (`vike_bridge_core::json_arb`).
const KIT: TokenKit = TokenKit { keys: KEYS, tokens: TOKENS, special: SPECIAL };

// ---------------------------------------------------------------------------------------------
// Feeds-plane generators
// ---------------------------------------------------------------------------------------------

/// A `depthUpdate` payload under either sync grammar (`pu` present = futures, absent = spot).
fn depth_data() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("e", tok(TOKENS)),
        KIT.field("E", int()),
        KIT.field("U", seq()),
        KIT.field("u", seq()),
        KIT.field("pu", seq()),
        KIT.field("b", KIT.book_side()),
        KIT.field("a", KIT.book_side()),
    ])
}

fn quote_data() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("b", KIT.num_str()),
        KIT.field("a", KIT.num_str()),
        KIT.field("B", KIT.num_str()),
        KIT.field("A", KIT.num_str()),
    ])
}

fn trade_data() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("T", int()),
        KIT.field("p", KIT.num_str()),
        KIT.field("q", KIT.num_str()),
        KIT.field("m", any::<bool>().prop_map(Value::Bool).boxed()),
        KIT.field("t", seq()),
    ])
}

/// One combined-stream frame: `{"stream":…,"data":…}`, or arbitrary JSON.
fn stream_frame() -> BoxedStrategy<Value> {
    let data = prop_oneof![depth_data(), depth_data(), quote_data(), trade_data(), KIT.arb_json()];
    prop_oneof![
        9 => (prop::sample::select(STREAMS.to_vec()), data)
            .prop_map(|(s, d)| json!({ "stream": s, "data": d })),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

/// A REST depth snapshot body.
fn snapshot_body() -> BoxedStrategy<String> {
    let obj = object(vec![
        KIT.field("lastUpdateId", seq()),
        KIT.field("bids", KIT.book_side()),
        KIT.field("asks", KIT.book_side()),
    ]);
    prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => KIT.arb_text()].boxed()
}

/// A raw kline row (the REST 12-element array), first cell an i64 time, the rest decimal strings.
fn kline_row() -> BoxedStrategy<Value> {
    let cell = prop_oneof![8 => numeric_string().prop_map(Value::String), 2 => KIT.arb_leaf()];
    let row = (prop_oneof![8 => int(), 2 => KIT.arb_leaf()], prop::collection::vec(cell, 0..12))
        .prop_map(|(t, cells)| {
            let mut row = vec![t];
            row.extend(cells);
            Value::Array(row)
        });
    prop_oneof![9 => row, 1 => KIT.arb_json()].boxed()
}

fn ws_kline_frame() -> BoxedStrategy<Value> {
    let k = object(vec![
        KIT.field("t", int()),
        KIT.field("o", KIT.num_str()),
        KIT.field("h", KIT.num_str()),
        KIT.field("l", KIT.num_str()),
        KIT.field("c", KIT.num_str()),
        KIT.field("v", KIT.num_str()),
        KIT.field("x", any::<bool>().prop_map(Value::Bool).boxed()),
    ]);
    prop_oneof![
        9 => (tok(TOKENS), int(), k).prop_map(|(e, ts, k)| json!({ "e": e, "E": ts, "k": k })),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

fn ws_mark_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => object(vec![
            KIT.field("e", Just(Value::String("markPriceUpdate".into())).boxed()),
            KIT.field("E", int()),
            KIT.field("p", KIT.num_str()),
        ]),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

/// A trade record in any of the three wire spellings (ws `@aggTrade`, ws `@trade`, REST raw).
fn trade_record() -> BoxedStrategy<Value> {
    let bool_v = any::<bool>().prop_map(Value::Bool).boxed();
    prop_oneof![
        object(vec![
            KIT.field("e", tok(TOKENS)),
            KIT.field("a", seq()),
            KIT.field("p", KIT.num_str()),
            KIT.field("q", KIT.num_str()),
            KIT.field("T", int()),
            KIT.field("m", bool_v.clone()),
        ]),
        object(vec![
            KIT.field("e", tok(TOKENS)),
            KIT.field("t", seq()),
            KIT.field("p", KIT.num_str()),
            KIT.field("q", KIT.num_str()),
            KIT.field("T", int()),
            KIT.field("m", bool_v.clone()),
        ]),
        object(vec![
            KIT.field("id", seq()),
            KIT.field("price", KIT.num_str()),
            KIT.field("qty", KIT.num_str()),
            KIT.field("time", int()),
            KIT.field("isBuyerMaker", bool_v),
        ]),
        KIT.arb_json(),
    ]
    .boxed()
}

/// An `exchangeInfo` payload (spot, fapi or dapi flavour).
fn exchange_info() -> BoxedStrategy<Value> {
    let filter = object(vec![
        KIT.field(
            "filterType",
            tok(&["PRICE_FILTER", "LOT_SIZE", "MARKET_LOT_SIZE", "MIN_NOTIONAL", "NOTIONAL"]),
        ),
        KIT.field("tickSize", KIT.num_str()),
        KIT.field("stepSize", KIT.num_str()),
        KIT.field("minQty", KIT.num_str()),
        KIT.field("maxQty", KIT.num_str()),
        KIT.field("minNotional", KIT.num_str()),
        KIT.field("notional", KIT.num_str()),
    ]);
    let entry = object(vec![
        KIT.field("symbol", tok(TOKENS)),
        KIT.field("baseAsset", tok(TOKENS)),
        KIT.field("quoteAsset", tok(TOKENS)),
        KIT.field("status", tok(TOKENS)),
        KIT.field("contractStatus", tok(TOKENS)),
        KIT.field("contractType", tok(TOKENS)),
        KIT.field("filters", prop::collection::vec(filter, 0..6).prop_map(Value::Array).boxed()),
    ]);
    prop_oneof![
        8 => prop::collection::vec(entry, 0..5).prop_map(|e| json!({ "symbols": e })),
        2 => KIT.arb_json(),
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
    fn feeds_text_decoders_are_total(text in KIT.arb_text()) {
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
        data in prop_oneof![depth_data(), quote_data(), trade_data(), KIT.arb_json()],
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
    fn kline_bodies_are_total(body in KIT.arb_body(kline_row())) {
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
        body in KIT.arb_body(
            object(vec![
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("fundingTime", int()),
                KIT.field("fundingRate", KIT.num_str()),
                KIT.field("markPrice", KIT.num_str()),
            ])
        )
    ) {
        let _ = parse_funding_rates(&body);
    }
}

#[cfg(feature = "exec")]
#[path = "decoder_never_panics/exec_plane.rs"]
mod exec_plane;
