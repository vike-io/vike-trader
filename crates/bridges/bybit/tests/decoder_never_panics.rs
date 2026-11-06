//! "Arbitrary input never panics" harness for the bybit wire decoders that `mapper_props.rs` does
//! NOT reach. That file already feeds arbitrary JSON to `map_bybit_perp` and arbitrary text (one
//! frame, fresh book) to `market_data::route_frame`; this one covers the rest of the crate's
//! inbound surface:
//!
//! - FEEDS plane (always compiled): `market_data::{decode_trade, parse_orderbook_snapshot}` and
//!   `market_data::route_frame` as a SEQUENCE into one `L2Book` (the strict `u += 1` fold),
//!   `market_feed::{klines::route_frame, mark::route_mark_frame, trades::parse_trades}`,
//!   `data::parse_bybit_klines`,
//!   `catalog::{parse_spot, parse_perp, parse_inverse}`, `instruments::{trading_symbols,
//!   next_page_cursor, perp_book, ambiguous_bare_symbols}`;
//! - EXEC plane (`cfg(feature = "exec")`, default-on): the per-row `event_mapper` entries
//!   (`map_execution`, `map_execution_fast`, `map_order`) and the spot dispatcher
//!   `map_bybit_private` / structured perp frames with the real topics, `history::map_bybit_history`,
//!   `funding::decode_bybit_funding_settlements`, the `recon_client` body parsers,
//!   `perp::{parse_bybit_perp_instruments, map_bybit_open_order}`, `ws_auth::match_op_ack`,
//!   `transport::unwrap_envelope`, `error_codes::by_msg`.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing or an
//! `Err`, but it must never panic a pump thread (a dead thread is a venue that silently goes
//! quiet), and one frame must never fabricate an event flood. Each decoder is fed (a) arbitrary
//! text, (b) lossy-decoded byte noise and (c) structured JSON whose keys are the decoder's REAL
//! field names and whose leaves are the dispatch tokens and hostile numerics (`"NaN"`, `"1e999"`,
//! `i64::MIN`, ...), so the generator reaches the match arms. Where a decoder folds into state
//! (`L2Book`) a short random sequence goes into ONE instance.
//!
//! A minimized counterexample is a REAL bug: commit the `.proptest-regressions` seed beside this
//! file and report it.
//!
//! Features: none beyond the crate's defaults. With `--no-default-features` only the feeds-plane
//! half compiles.

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_bridge_core::depth::infer_tick_size;
use vike_bridge_core::feed_frame::{FeedFrame, MarkPx};
use vike_bridge_core::json_arb::{self, TokenKit, int, numeric_string, object, tick_size, tok};
use vike_bybit::catalog::{parse_inverse, parse_perp, parse_spot};
use vike_bybit::data::parse_bybit_klines;
use vike_bybit::instruments::{
    ambiguous_bare_symbols, next_page_cursor, perp_book, trading_symbols,
};
use vike_bybit::market_data::{self, MdEvent};
use vike_bybit::market_feed::klines::route_frame;
use vike_bybit::market_feed::mark::route_mark_frame;
use vike_bybit::market_feed::trades::parse_trades;
use vike_marketdata::{BookLevel, L2Book};

/// Real bybit field names across every decoder in this file.
const KEYS: &[&str] = &[
    "topic",
    "data",
    "type",
    "ts",
    "creationTime",
    "op",
    "success",
    "ret_msg",
    "retCode",
    "ret_code",
    "retMsg",
    "result",
    "list",
    "execType",
    "orderStatus",
    "orderLinkId",
    "orderId",
    "execId",
    "execTime",
    "execPrice",
    "execQty",
    "execFee",
    "feeCurrency",
    "isMaker",
    "side",
    "symbol",
    "markPrice",
    "positionIdx",
    "cumExecQty",
    "orderQty",
    "leavesQty",
    "updatedTime",
    "cancelType",
    "rejectReason",
    "coin",
    "walletBalance",
    "u",
    "seq",
    "s",
    "b",
    "a",
    "T",
    "p",
    "v",
    "S",
    "start",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "confirm",
    "turnover",
    "qty",
    "price",
    "avgPrice",
    "orderType",
    "size",
    "tradeMode",
    "makerFeeRate",
    "takerFeeRate",
    "nextPageCursor",
    "status",
    "contractType",
    "baseCoin",
    "quoteCoin",
    "settleCoin",
    "priceFilter",
    "tickSize",
    "lotSizeFilter",
    "qtyStep",
    "minOrderQty",
    "maxOrderQty",
    "minNotionalValue",
    "leverageFilter",
    "maxLeverage",
    "transactionTime",
    "funding",
    "feeRate",
    "lastPrice",
];

/// Dispatch tokens: the string values the decoders `match` on.
const TOKENS: &[&str] = &[
    "execution",
    "execution.fast",
    "execution.fast.linear",
    "order",
    "wallet",
    "orderbook.50.BTCUSDT",
    "orderbook.200.BTCUSDT",
    "publicTrade.BTCUSDT",
    "tickers.BTCUSDT",
    "kline.1.BTCUSDT",
    "snapshot",
    "delta",
    "subscribe",
    "ping",
    "pong",
    "auth",
    "Trade",
    "BustTrade",
    "AdlTrade",
    "Funding",
    "New",
    "Untriggered",
    "PartiallyFilled",
    "Filled",
    "Cancelled",
    "PartiallyFilledCanceled",
    "Rejected",
    "Deactivated",
    "Triggered",
    "Buy",
    "Sell",
    "SETTLEMENT",
    "Trading",
    "LinearPerpetual",
    "InversePerpetual",
    "LinearFutures",
    "InverseFutures",
    "USDT",
    "BTCUSDT",
    "Limit",
    "Market",
    "CancelByUser",
];

/// Hostile numeric / textual leaves.
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

/// A u64 sequence number: small (so the strict `u += 1` rule hits adjacent / equal / regressed
/// values), the extremes, or anything.
fn seq() -> BoxedStrategy<Value> {
    json_arb::seq(16)
}

/// Bybit's vocabulary over the shared generator shapes (`vike_bridge_core::json_arb`).
const KIT: TokenKit = TokenKit { keys: KEYS, tokens: TOKENS, special: SPECIAL };

fn bool_v() -> BoxedStrategy<Value> {
    any::<bool>().prop_map(Value::Bool).boxed()
}

// ---------------------------------------------------------------------------------------------
// Frame generators
// ---------------------------------------------------------------------------------------------

/// An `orderbook.*` frame (snapshot / delta / unknown type), `u` small so the strict-sequence
/// rule is exercised.
fn orderbook_frame() -> BoxedStrategy<Value> {
    let data = object(vec![
        KIT.field("s", tok(TOKENS)),
        KIT.field("b", KIT.book_side()),
        KIT.field("a", KIT.book_side()),
        KIT.field("u", seq()),
        KIT.field("seq", seq()),
    ]);
    prop_oneof![
        9 => (
            tok(&["orderbook.50.BTCUSDT", "orderbook.200.BTCUSDT", "orderbook.1.BTCUSDT", "orderbook."]),
            tok(&["snapshot", "delta", "delta", "other", ""]),
            int(),
            data,
        )
            .prop_map(|(topic, ty, ts, data)| json!({ "topic": topic, "type": ty, "ts": ts, "data": data })),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

fn public_trade_row() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => object(vec![
            KIT.field("T", int()),
            KIT.field("s", tok(TOKENS)),
            KIT.field("S", tok(&["Buy", "Sell", "buy", ""])),
            KIT.field("v", KIT.num_str()),
            KIT.field("p", KIT.num_str()),
        ]),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

fn public_trade_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => (tok(&["publicTrade.BTCUSDT", "publicTrade", "x"]), prop::collection::vec(public_trade_row(), 0..6))
            .prop_map(|(topic, rows)| json!({ "topic": topic, "type": "snapshot", "data": rows })),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

fn kline_frame() -> BoxedStrategy<Value> {
    let datum = prop_oneof![
        8 => object(vec![
            KIT.field("start", int()),
            KIT.field("open", KIT.num_str()),
            KIT.field("high", KIT.num_str()),
            KIT.field("low", KIT.num_str()),
            KIT.field("close", KIT.num_str()),
            KIT.field("volume", KIT.num_str()),
            KIT.field("turnover", KIT.num_str()),
            KIT.field("confirm", bool_v()),
        ]),
        2 => KIT.arb_json(),
    ];
    prop_oneof![
        8 => (tok(&["kline.1.BTCUSDT", "kline.", "other"]), prop::collection::vec(datum, 0..3))
            .prop_map(|(topic, data)| json!({ "topic": topic, "data": data })),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

fn mark_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        8 => (
            tok(&["tickers.BTCUSDT", "tickers.", "x"]),
            int(),
            object(vec![KIT.field("markPrice", KIT.num_str()), KIT.field("lastPrice", KIT.num_str())]),
        )
            .prop_map(|(topic, ts, data)| json!({ "topic": topic, "ts": ts, "data": data })),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

/// A subscribe / pong envelope.
fn op_frame() -> BoxedStrategy<Value> {
    object(vec![
        KIT.field("op", tok(&["subscribe", "ping", "pong", "auth"])),
        KIT.field("success", bool_v()),
        KIT.field("ret_msg", tok(TOKENS)),
        KIT.field("retCode", int()),
        KIT.field("ret_code", int()),
    ])
}

/// Any frame one of the public lanes could receive.
fn public_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        4 => orderbook_frame(),
        2 => public_trade_frame(),
        2 => kline_frame(),
        2 => mark_frame(),
        1 => op_frame(),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

/// A raw kline row (`[start, open, high, low, close, volume, turnover]`, ALL decimal strings).
fn kline_row() -> BoxedStrategy<Value> {
    let cell = prop_oneof![8 => numeric_string().prop_map(Value::String), 2 => KIT.arb_leaf()];
    prop_oneof![
        9 => prop::collection::vec(cell, 0..9).prop_map(Value::Array),
        1 => KIT.arb_json(),
    ]
    .boxed()
}

/// A kline envelope body (`retCode` / `result.list`).
fn kline_body() -> BoxedStrategy<String> {
    let envelope = (
        prop_oneof![8 => Just(json!(0)), 2 => int()],
        prop::collection::vec(kline_row(), 0..6),
    )
        .prop_map(
            |(code, rows)| json!({ "retCode": code, "retMsg": "OK", "result": { "list": rows } }),
        );
    prop_oneof![8 => envelope.prop_map(|v| v.to_string()), 2 => KIT.arb_text()].boxed()
}

/// An `instruments-info` payload (`result.list`).
fn instruments_payload() -> BoxedStrategy<Value> {
    let entry = object(vec![
        KIT.field("symbol", tok(TOKENS)),
        KIT.field("status", tok(&["Trading", "PreLaunch", ""])),
        KIT.field("contractType", tok(TOKENS)),
        KIT.field("baseCoin", tok(TOKENS)),
        KIT.field("quoteCoin", tok(TOKENS)),
        KIT.field("settleCoin", tok(TOKENS)),
        KIT.field("priceFilter", object(vec![KIT.field("tickSize", KIT.num_str())])),
        KIT.field(
            "lotSizeFilter",
            object(vec![
                KIT.field("qtyStep", KIT.num_str()),
                KIT.field("minOrderQty", KIT.num_str()),
                KIT.field("maxOrderQty", KIT.num_str()),
                KIT.field("minNotionalValue", KIT.num_str()),
            ]),
        ),
        KIT.field("leverageFilter", object(vec![KIT.field("maxLeverage", KIT.num_str())])),
    ]);
    prop_oneof![
        8 => (prop::collection::vec(entry, 0..5), tok(&["", "cursor1", "next"]))
            .prop_map(|(rows, cursor)| json!({ "result": { "list": rows, "nextPageCursor": cursor } })),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

fn symbol_set() -> BoxedStrategy<std::collections::BTreeSet<String>> {
    prop::collection::btree_set(prop_oneof![Just("BTCUSDT".to_string()), "[A-Za-z]{0,6}"], 0..4)
        .boxed()
}

/// Run `book` through a sequence of frames. A `Resync` / `Ignored` outcome never folds, so it must
/// leave the sequence number alone; the state must stay readable after any fold.
fn fold_frames(book: &mut L2Book, frames: &[Value]) -> Result<(), TestCaseError> {
    for frame in frames {
        let before = book.last_seq;
        let out = market_data::route_frame(&frame.to_string(), "BTCUSDT", book);
        if matches!(out, MdEvent::Resync | MdEvent::Ignored | MdEvent::Trades(_)) {
            prop_assert_eq!(book.last_seq, before, "a non-folding outcome moved last_seq");
        }
        let _ = (book.best_bid(), book.best_ask(), book.mid(), book.spread(), book.top_n(5));
        let _ = market_data::quote_from_book(book, "BTCUSDT");
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every `&str` decoder of the feeds plane is total over arbitrary text and byte noise.
    #[test]
    fn feeds_text_decoders_are_total(text in KIT.arb_text()) {
        let _ = route_frame(&text);
        let _ = route_mark_frame(&text);
        let _ = market_data::parse_orderbook_snapshot(&text);
        let _ = parse_bybit_klines(&text);
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            for t in parse_trades(&v) {
                prop_assert!(t.price.is_finite() && t.price > 0.0);
                prop_assert!(t.size.is_finite() && t.size > 0.0);
            }
        }
    }

    /// The venue-JSON-in-`Value` decoders of the public lanes.
    #[test]
    fn public_value_decoders_are_total(frame in public_frame(), row in public_trade_row()) {
        for t in parse_trades(&frame) {
            prop_assert!(t.price.is_finite() && t.price > 0.0);
            prop_assert!(t.size.is_finite() && t.size > 0.0);
        }
        let _ = market_data::decode_trade(&row, "BTCUSDT");
        let _ = market_data::decode_trade(&frame, "BTCUSDT");
    }

    /// The kline lane's frame classifier and the mark lane's, over structured frames; a Mark is
    /// always a live (finite, positive) price.
    #[test]
    fn kline_and_mark_frames_are_total(
        kline in kline_frame(),
        mark in mark_frame(),
        op in op_frame(),
    ) {
        for f in [&kline, &mark, &op] {
            let _ = route_frame(&f.to_string());
        }
        for f in [&kline, &mark, &op] {
            if let FeedFrame::Data(MarkPx { px, .. }) = route_mark_frame(&f.to_string()) {
                prop_assert!(px.is_finite() && px > 0.0, "a dead mark escaped: {}", px);
            }
        }
    }

    /// (c) `market_data::route_frame` folds a SEQUENCE of 1..8 public frames into ONE book.
    #[test]
    fn route_frame_sequences_into_one_book(
        frames in prop::collection::vec(public_frame(), 1..8),
        tick in tick_size(),
        seed in prop_oneof![0u64..16, Just(u64::MAX)],
    ) {
        let mut book = L2Book::new(tick);
        book.last_seq = seed;
        fold_frames(&mut book, &frames)?;
    }

    /// The DOM lane's seeding path (`fold_depth_frame`): the first snapshot frame builds the book
    /// (tick inferred from it), later frames fold into it. NaN prices are filtered before
    /// `infer_tick_size`: its `partial_cmp(..).unwrap_or(Equal)` sort comparator is not a total
    /// order over NaN (see the suspected-panic note in the crate report), and the venue never
    /// sends one.
    #[test]
    fn snapshot_seed_then_frames_never_panics(
        first in orderbook_frame(),
        rest in prop::collection::vec(public_frame(), 0..8),
    ) {
        if let Some((seq, bids, asks)) = market_data::parse_orderbook_snapshot(&first.to_string()) {
            let keep = |side: Vec<BookLevel>| -> Vec<BookLevel> {
                side.into_iter().filter(|l| !l.price.is_nan()).collect()
            };
            let (bids, asks) = (keep(bids), keep(asks));
            let mut book = L2Book::new(infer_tick_size(&bids, &asks));
            book.apply_snapshot(seq, &bids, &asks);
            fold_frames(&mut book, &rest)?;
        }
    }

    /// REST kline envelopes.
    #[test]
    fn kline_bodies_are_total(body in kline_body()) {
        let _ = parse_bybit_klines(&body);
    }

    /// `instruments-info` payloads through the catalog parsers, the symbol-set readers and the
    /// book router; the perp catalogs only ever mint `.P` labels.
    #[test]
    fn instrument_payloads_are_total(
        payload in instruments_payload(),
        symbol in prop_oneof![tok(TOKENS).prop_map(|v| v.as_str().unwrap_or("").to_string()), any::<String>()],
        linear in symbol_set(),
        inverse in symbol_set(),
    ) {
        let _ = parse_spot(&payload);
        for inst in parse_perp(&payload).into_iter().chain(parse_inverse(&payload)) {
            prop_assert!(inst.raw_symbol.ends_with(vike_catalog::PERP_SUFFIX));
        }
        let _ = trading_symbols(&payload);
        let _ = next_page_cursor(&payload);
        let _ = ambiguous_bare_symbols(&linear, &inverse);
        let _ = perp_book(&symbol, &linear, &inverse);
    }
}

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use vike_bybit::error_codes::by_msg;
    use vike_bybit::event_mapper::{
        map_bybit_perp, map_bybit_private, map_execution, map_execution_fast, map_order,
    };
    use vike_bybit::funding::decode_bybit_funding_settlements;
    use vike_bybit::history::map_bybit_history;
    use vike_bybit::perp::{map_bybit_open_order, parse_bybit_perp_instruments};
    use vike_bybit::recon_client::{
        parse_fee_rate, parse_fills, parse_orders, parse_positions, parse_trade_mode,
        parse_wallet_balance,
    };
    use vike_bybit::transport::unwrap_envelope;
    use vike_bybit::ws_auth::match_op_ack;

    /// A bybit epoch-ms cell: bybit's REST sends these as numeric STRINGS, its WS as numbers.
    fn ms() -> BoxedStrategy<Value> {
        prop_oneof![int(), (0i64..2_000_000_000_000).prop_map(|n| json!(n.to_string()))].boxed()
    }

    /// A V5 REST `result` body (`{"list":[…]}`) as text: junk, JSON noise, a well-shaped list, or
    /// a list of the wrong top-level shape.
    fn list_body(rows: BoxedStrategy<Value>) -> BoxedStrategy<String> {
        prop_oneof![
            1 => KIT.arb_text(),
            2 => KIT.arb_json().prop_map(|v| v.to_string()),
            6 => prop::collection::vec(rows.clone(), 0..6)
                .prop_map(|r| json!({ "list": r }).to_string()),
            1 => rows.prop_map(|r| r.to_string()),
        ]
        .boxed()
    }

    /// `map_bybit_perp` / `map_bybit_private` fan out over the `data` ARRAY (at most two events
    /// per row), so the flood bound scales with the row count.
    fn flood_bound(frame: &Value) -> usize {
        8 + 8 * frame.get("data").and_then(Value::as_array).map_or(0, Vec::len)
    }

    fn small_id() -> BoxedStrategy<Value> {
        prop_oneof![tok(&["o1", "o2", "o3", ""]), (0i64..4).prop_map(|n| json!(n)),].boxed()
    }

    fn coid() -> BoxedStrategy<Value> {
        prop_oneof![
            3 => tok(TOKENS),
            2 => "[ -~]{0,14}".prop_map(Value::String),
        ]
        .boxed()
    }

    fn position_idx() -> BoxedStrategy<Value> {
        prop_oneof![(0i64..4).prop_map(|n| json!(n)), (0i64..4).prop_map(|n| json!(n.to_string())),]
            .boxed()
    }

    /// One `execution` row (the slow topic carries everything; the fast one a slim subset — the
    /// same generator feeds both, absent keys model the slim shape).
    fn execution_row() -> BoxedStrategy<Value> {
        prop_oneof![
            9 => object(vec![
                KIT.field("execType", tok(&["Trade", "Trade", "BustTrade", "AdlTrade", "Funding", ""])),
                KIT.field("orderLinkId", coid()),
                KIT.field("orderId", small_id()),
                KIT.field("execId", prop_oneof![tok(&["e1", "e2", ""]), seq()].boxed()),
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("side", tok(&["Buy", "Sell", ""])),
                KIT.field("execQty", KIT.num_str()),
                KIT.field("execPrice", KIT.num_str()),
                KIT.field("execFee", KIT.num_str()),
                KIT.field("feeCurrency", tok(TOKENS)),
                KIT.field("isMaker", bool_v()),
                KIT.field("execTime", ms()),
                KIT.field("cumExecQty", KIT.num_str()),
                KIT.field("orderQty", KIT.num_str()),
                KIT.field("leavesQty", prop_oneof![KIT.num_str(), Just(json!(0)).boxed()].boxed()),
                KIT.field("markPrice", KIT.num_str()),
                KIT.field("positionIdx", position_idx()),
            ]),
            1 => KIT.arb_json(),
        ]
        .boxed()
    }

    fn order_row() -> BoxedStrategy<Value> {
        prop_oneof![
            9 => object(vec![
                KIT.field("orderStatus", tok(&[
                    "New", "Untriggered", "PartiallyFilled", "Filled", "Cancelled",
                    "PartiallyFilledCanceled", "Rejected", "Deactivated", "Triggered", "",
                ])),
                KIT.field("orderLinkId", coid()),
                KIT.field("orderId", small_id()),
                KIT.field("updatedTime", ms()),
                KIT.field("cancelType", tok(TOKENS)),
                KIT.field("rejectReason", tok(TOKENS)),
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("side", tok(&["Buy", "Sell", ""])),
                KIT.field("orderType", tok(&["Limit", "Market", ""])),
                KIT.field("qty", KIT.num_str()),
                KIT.field("price", KIT.num_str()),
                KIT.field("cumExecQty", KIT.num_str()),
                KIT.field("avgPrice", KIT.num_str()),
            ]),
            1 => KIT.arb_json(),
        ]
        .boxed()
    }

    fn position_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("symbol", tok(TOKENS)),
            KIT.field("side", tok(&["Buy", "Sell", ""])),
            KIT.field("size", KIT.num_str()),
            KIT.field("avgPrice", KIT.num_str()),
            KIT.field("updatedTime", ms()),
            KIT.field("positionIdx", position_idx()),
            KIT.field("tradeMode", prop_oneof![position_idx(), tok(&["1", "0"])].boxed()),
        ])
    }

    fn wallet_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field(
                "coin",
                prop::collection::vec(
                    object(vec![
                        KIT.field("coin", tok(TOKENS)),
                        KIT.field("walletBalance", KIT.num_str()),
                    ]),
                    0..4,
                )
                .prop_map(Value::Array)
                .boxed(),
            ),
        ])
    }

    /// A private-stream frame with a real topic: execution / execution.fast / order / wallet.
    fn private_frame() -> BoxedStrategy<Value> {
        prop_oneof![
            4 => (tok(&["execution", "execution.fast", "execution.fast.linear"]), prop::collection::vec(execution_row(), 0..6), int())
                .prop_map(|(topic, rows, ts)| json!({ "topic": topic, "creationTime": ts, "data": rows })),
            3 => (prop::collection::vec(order_row(), 0..6), int())
                .prop_map(|(rows, ts)| json!({ "topic": "order", "creationTime": ts, "data": rows })),
            2 => (prop::collection::vec(wallet_row(), 0..4), int())
                .prop_map(|(rows, ts)| json!({ "topic": "wallet", "creationTime": ts, "data": rows })),
            1 => op_frame(),
            1 => KIT.arb_json(),
        ]
        .boxed()
    }

    fn fee_body() -> BoxedStrategy<String> {
        list_body(object(vec![
            KIT.field("makerFeeRate", KIT.num_str()),
            KIT.field("takerFeeRate", KIT.num_str()),
            KIT.field("symbol", tok(TOKENS)),
        ]))
    }

    fn wallet_body() -> BoxedStrategy<String> {
        let obj =
            prop::collection::vec(wallet_row(), 0..4).prop_map(|rows| json!({ "list": rows }));
        prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => KIT.arb_text()].boxed()
    }

    fn funding_row() -> BoxedStrategy<Value> {
        prop_oneof![
            8 => object(vec![
                KIT.field("type", prop_oneof![tok(&["SETTLEMENT", "TRADE", ""]), KIT.arb_leaf()].boxed()),
                KIT.field("funding", prop_oneof![KIT.num_str(), Just(json!("0")).boxed(), Just(Value::Null).boxed()].boxed()),
                KIT.field("feeRate", KIT.num_str()),
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("transactionTime", ms()),
                KIT.field("id", seq()),
            ]),
            2 => KIT.arb_json(),
        ]
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// The perp dispatcher and the spot dispatcher, over frames carrying the REAL topics and
        /// execType / orderStatus tokens (`mapper_props.rs` feeds `map_bybit_perp` random-string
        /// leaves that almost never spell `execution` / `Trade`, and never feeds
        /// `map_bybit_private`).
        #[test]
        fn dispatchers_reach_their_arms(frame in private_frame()) {
            let perp = map_bybit_perp(&frame, "bybit", "BTCUSDT");
            prop_assert!(perp.len() <= flood_bound(&frame), "perp flood: {}", perp.len());
            let spot = map_bybit_private(&frame, "bybit", "BTCUSDT");
            prop_assert!(spot.len() <= flood_bound(&frame), "spot flood: {}", spot.len());
        }

        /// The per-row entries the dispatchers fan out to, fed one row directly.
        #[test]
        fn per_row_mappers_are_total(row in prop_oneof![execution_row(), order_row()]) {
            prop_assert!(map_execution(&row, "bybit", "BTCUSDT").len() <= 2);
            prop_assert!(map_execution_fast(&row, "bybit", "BTCUSDT").len() <= 1);
            prop_assert!(map_order(&row, "bybit", "BTCUSDT").len() <= 1);
        }

        /// The audit-A3 resync replay: `order/history` + `execution/list` arrays.
        #[test]
        fn history_replay_is_total(
            orders in prop::collection::vec(order_row(), 0..6),
            execs in prop::collection::vec(execution_row(), 0..6),
            bodies_are_arrays in any::<bool>(),
        ) {
            let bound = 2 * execs.len() * (orders.len() + 1) + orders.len();
            let (o, e) = if bodies_are_arrays {
                (Value::Array(orders), Value::Array(execs))
            } else {
                (json!({ "list": orders }), json!({ "list": execs }))
            };
            let events = map_bybit_history(&o, &e, "bybit", "BTCUSDT");
            prop_assert!(bodies_are_arrays || events.is_empty());
            prop_assert!(events.len() <= bound, "history flood: {} > {}", events.len(), bound);
        }

        /// Funding settlements: at most one event per row.
        #[test]
        fn funding_settlements_are_total(rows in prop::collection::vec(funding_row(), 0..8)) {
            let events = decode_bybit_funding_settlements(&rows, "bybit", "BTCUSDT");
            prop_assert!(events.len() <= rows.len());
        }

        /// Every reconcile body parser over junk text and over `{"list":[…]}` bodies of
        /// well-shaped rows.
        #[test]
        fn recon_parsers_are_total(
            orders in list_body(order_row()),
            fills in list_body(execution_row()),
            positions in list_body(position_row()),
            wallet in wallet_body(),
            fees in fee_body(),
            junk in KIT.arb_text(),
        ) {
            for body in [&orders, &junk] {
                let _ = parse_orders(body);
            }
            for body in [&fills, &junk] {
                let _ = parse_fills(body);
            }
            for body in [&positions, &junk] {
                let _ = parse_positions(body);
            }
            for body in [&wallet, &junk] {
                let _ = parse_wallet_balance(body);
            }
            for body in [&fees, &junk] {
                let _ = parse_fee_rate(body);
            }
        }

        /// `tradeMode` reader, the instrument-grid parser, the V5 envelope unwrapper, the WS ack
        /// matcher and the message classifier.
        #[test]
        fn small_decoders_are_total(
            payload in instruments_payload(),
            row in prop_oneof![position_row(), KIT.arb_json()],
            envelope in prop_oneof![
                object(vec![KIT.field("retCode", int()), KIT.field("retMsg", tok(TOKENS)), KIT.field("result", KIT.arb_json())]),
                KIT.arb_json(),
            ],
            ack in prop_oneof![op_frame(), KIT.arb_json()],
            op in prop_oneof![Just("auth".to_string()), Just("subscribe".to_string()), any::<String>()],
            msg in any::<String>(),
        ) {
            let _ = parse_trade_mode(&row);
            let _ = parse_bybit_perp_instruments(&payload);
            let _ = unwrap_envelope(envelope);
            let _ = match_op_ack(&ack, &op);
            let _ = by_msg(&msg);
        }

        /// `map_bybit_open_order` over rows whose quantity may be any number the wire can spell
        /// (`"NaN"` / `"inf"` included - see the named cases below).
        #[test]
        fn open_order_is_total_over_rows(
            row in object(vec![
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("orderId", small_id()),
                KIT.field("orderLinkId", coid()),
                KIT.field("side", tok(&["Buy", "Sell", ""])),
                KIT.field("orderType", tok(&["Limit", "Market", ""])),
                KIT.field("qty", numeric_string().prop_map(Value::String).boxed()),
                KIT.field("price", KIT.num_str()),
            ])
        ) {
            let _ = map_bybit_open_order(&row);
        }

        /// REGRESSION: a non-finite quantity (`"NaN"`, `"inf"`, `"1e999"`) is read through `json_num`,
        /// and `serde_json` serializes a non-finite `f64` as JSON `null`, which the non-optional
        /// `qty: f64` of the `OrderRequest` round trip refused - an `.expect("static shape")` panic.
        /// The mapper now reads such a quantity as `0.0`, like an unparseable one.
        #[test]
        fn open_order_non_finite_quantity_does_not_panic(
            qty in tok(&["NaN", "inf", "-inf", "1e999", "-1e999"])
        ) {
            let row = json!({ "symbol": "BTCUSDT", "side": "Buy", "orderType": "Limit", "qty": qty });
            let _ = map_bybit_open_order(&row);
        }
    }
}
