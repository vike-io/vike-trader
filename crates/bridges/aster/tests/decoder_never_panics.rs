//! "Arbitrary input never panics" harness for the aster wire decoders. Aster is a Binance fork:
//! almost every decoder it serves is `vike_binance::family`'s, re-exported under aster's names
//! (`market_data`, `event_mapper`, `perp_mapper`, `history`) or bound to the `"aster"` venue
//! string by a thin wrapper (`catalog`, `recon_client`'s 1-arg parsers). The shared
//! implementations are fuzzed in `crates/bridges/binance/tests/decoder_never_panics.rs`; this file
//! drives the SAME decoders through ASTER's public paths (so a wrapper that grows its own logic is
//! covered) plus the one decoder aster owns outright, `perp::parse_aster_perp_instruments`:
//!
//! - FEEDS plane (always compiled): `market_data::{decode_book_ticker, decode_trade,
//!   apply_depth_event, route_frame}` (the last as a SEQUENCE into one `L2Book`), `catalog`;
//! - EXEC plane (`cfg(feature = "exec")`, default-on): `perp_mapper::map_aster_perp`,
//!   `event_mapper::{map_aster_private, map_execution_report}`,
//!   `history::{map_aster_history, map_aster_perp_history}`, the nine 1-arg `recon_client`
//!   parsers, `perp::{parse_aster_perp_instruments, map_perp_open_order}`.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing or an
//! `Err`, but it must never panic a pump thread, and one frame must never fabricate an event flood.
//! Inputs are (a) arbitrary text, (b) lossy-decoded byte noise and (c) structured JSON whose keys
//! are the decoders' REAL field names and whose leaves are the dispatch tokens and hostile numerics
//! (`"NaN"`, `"1e999"`, `i64::MIN`, ...). Where a decoder folds into state (`L2Book`) a short
//! random sequence goes into ONE instance.
//!
//! The shared mappers are a coordinated-PR home (binance + aster): a minimized counterexample is a
//! REAL bug in `vike_binance::family`; commit the `.proptest-regressions` seed beside this file
//! and report it.
//!
//! Features: none beyond the crate's defaults. With `--no-default-features` only the feeds-plane
//! half compiles.

use proptest::prelude::*;
use serde_json::{Value, json};
use vike_aster::catalog::{parse_perp, parse_spot};
use vike_aster::market_data::{
    DepthOutcome, MdEvent, apply_depth_event, decode_book_ticker, decode_trade, route_frame,
};
use vike_model::L2Book;

/// One frame legitimately emits a handful of events (the dual-publish fill contract; one funding
/// event per wallet row, rows <= 6 in every generator); more than this from ONE frame is a mapper
/// bug regardless of input.
#[cfg(feature = "exec")]
const MAX_EVENTS_PER_FRAME: usize = 16;

/// Real field names across the decoders in this file (ws frames, REST rows, exchangeInfo, account
/// / balance bodies).
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
    "symbol",
    "symbols",
    "baseAsset",
    "quoteAsset",
    "contractType",
    "filters",
    "filterType",
    "tickSize",
    "stepSize",
    "minQty",
    "maxQty",
    "notional",
    "makerCommissionRate",
    "takerCommissionRate",
    "commissionRates",
    "maker",
    "taker",
];

/// Dispatch tokens: the string values the decoders `match` on.
const TOKENS: &[&str] = &[
    "executionReport",
    "outboundAccountPosition",
    "ORDER_TRADE_UPDATE",
    "ACCOUNT_UPDATE",
    "TRADE_LITE",
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
    "USDT",
    "BTCUSDT",
    "isolated",
    "LIMIT",
    "autoclose-1",
    "adl_autoclose",
    "settlement_autoclose-2",
];

/// Hostile numeric / textual leaves.
const SPECIAL: &[&str] = &[
    "NaN",
    "nan",
    "inf",
    "-inf",
    "Infinity",
    "-0",
    "0",
    "1e999",
    "-1e999",
    "1e-999",
    "",
    "  ",
    "0x10",
    ".",
    "+5",
    "1_000",
    "9223372036854775808",
    "18446744073709551616",
    "e5",
];

/// Venue-shaped stream names for the combined-stream router.
const STREAMS: &[&str] = &[
    "btcusdt@bookTicker",
    "btcusdt@trade",
    "btcusdt@depth@100ms",
    "btcusdt@aggTrade",
    "",
    "@depth",
];

fn tok(list: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop::sample::select(list.to_vec()).prop_map(|s| Value::String(s.to_string())).boxed()
}

fn numeric_string() -> BoxedStrategy<String> {
    prop_oneof!["[0-9]{1,6}", "-?[0-9]{1,8}\\.[0-9]{1,8}", "[0-9]{1,3}e-?[0-9]{1,3}"].boxed()
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

/// A u64 sequence number: small (so the sync rules hit equal / adjacent values), the extremes,
/// or anything.
fn seq() -> BoxedStrategy<Value> {
    prop_oneof![0u64..24, Just(u64::MAX), Just(u64::MAX - 1), any::<u64>()]
        .prop_map(|n| json!(n))
        .boxed()
}

/// Any leaf the venue could (mis)send.
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

/// Arbitrary text: unicode, lossy-decoded byte noise, or valid JSON text.
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

/// A `depthUpdate` payload under either sync grammar (aster speaks the futures grammar, `pu`
/// present, on both planes; spot-grammar frames, `pu` absent, must not panic either).
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

/// An `exchangeInfo` payload (spot or fapi flavour).
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
        field("notional", num_str()),
    ]);
    let entry = object(vec![
        field("symbol", tok(TOKENS)),
        field("baseAsset", tok(TOKENS)),
        field("quoteAsset", tok(TOKENS)),
        field("status", tok(TOKENS)),
        field("contractType", tok(TOKENS)),
        field("filters", prop::collection::vec(filter, 0..6).prop_map(Value::Array).boxed()),
    ]);
    prop_oneof![
        8 => prop::collection::vec(entry, 0..5).prop_map(|e| json!({ "symbols": e })),
        2 => arb_json(),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `route_frame` over arbitrary text (JSON or not) into a fresh book: non-JSON is always
    /// `Ignored`, never a book mutation.
    #[test]
    fn route_frame_is_total_over_text(text in arb_text(), tick in tick_size()) {
        let mut book = L2Book::new(tick);
        let out = route_frame(&text, "BTCUSDT", &mut book);
        if serde_json::from_str::<Value>(&text).is_err() {
            prop_assert!(matches!(out, MdEvent::Ignored), "non-JSON must be Ignored");
        }
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

    /// (c) `route_frame` folds a SEQUENCE of 1..8 frames into ONE book: the sequence number only
    /// moves forward, and a `BookUpdated` always moved it.
    #[test]
    fn route_frame_sequences_into_one_book(
        frames in prop::collection::vec(stream_frame(), 1..8),
        tick in tick_size(),
        seed in prop_oneof![0u64..24, Just(u64::MAX)],
    ) {
        let mut book = L2Book::new(tick);
        book.last_seq = seed;
        for frame in &frames {
            let before = book.last_seq;
            let out = route_frame(&frame.to_string(), "BTCUSDT", &mut book);
            prop_assert!(book.last_seq >= before, "last_seq regressed {} -> {}", before, book.last_seq);
            if matches!(out, MdEvent::BookUpdated) {
                prop_assert!(book.last_seq > before, "BookUpdated without a sequence advance");
            }
            let _ = (book.best_bid(), book.best_ask(), book.mid(), book.spread(), book.top_n(5));
        }
    }

    /// `exchangeInfo` payloads through aster's catalog faces; the perp catalog only mints `.P`.
    #[test]
    fn catalog_payloads_are_total(payload in exchange_info()) {
        for inst in parse_spot(&payload) {
            prop_assert_eq!(inst.venue.as_str(), "aster");
        }
        for inst in parse_perp(&payload) {
            prop_assert!(inst.raw_symbol.ends_with(vike_catalog::PERP_SUFFIX));
        }
    }
}

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use vike_aster::event_mapper::{map_aster_private, map_execution_report};
    use vike_aster::history::{map_aster_history, map_aster_perp_history};
    use vike_aster::perp::{map_perp_open_order, parse_aster_perp_instruments};
    use vike_aster::perp_mapper::map_aster_perp;
    use vike_aster::recon_client::{
        parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
        parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
        parse_spot_open_orders,
    };

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
        ]
        .boxed()
    }

    fn bool_v() -> BoxedStrategy<Value> {
        any::<bool>().prop_map(Value::Bool).boxed()
    }

    fn side_v() -> BoxedStrategy<Value> {
        tok(&["BUY", "SELL", "buy", ""])
    }

    /// The `o` object of an `ORDER_TRADE_UPDATE`, or the body of a spot `executionReport`.
    fn order_fields() -> Vec<Field> {
        vec![
            field("s", tok(TOKENS)),
            field("c", coid()),
            field("C", coid()),
            field("x", tok(&["NEW", "CANCELED", "EXPIRED", "REJECTED", "TRADE", "CALCULATED"])),
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
            tok(&["FUNDING_FEE", "ORDER", ""]),
            prop::collection::vec(wallet_row(), 0..6),
        )
            .prop_map(
                |(t, m, rows)| json!({ "e": "ACCOUNT_UPDATE", "T": t, "a": { "m": m, "B": rows } }),
            );
        prop_oneof![
            4 => otu,
            2 => account,
            2 => with_e("TRADE_LITE", order_fields()),
            1 => arb_json(),
        ]
        .boxed()
    }

    /// A spot user-data frame, bare or inside the `{"event":…}` envelope.
    fn spot_frame() -> BoxedStrategy<Value> {
        let report = with_e("executionReport", order_fields());
        let account = (int(), prop::collection::vec(wallet_row(), 0..6))
            .prop_map(|(t, rows)| json!({ "e": "outboundAccountPosition", "E": t, "B": rows }));
        let inner = prop_oneof![4 => report, 2 => account, 1 => arb_json()].boxed();
        prop_oneof![
            3 => inner.clone(),
            3 => (small_id(), inner)
                .prop_map(|(id, ev)| json!({ "subscriptionId": id, "event": ev })),
        ]
        .boxed()
    }

    fn order_row() -> BoxedStrategy<Value> {
        object(vec![
            field("symbol", tok(TOKENS)),
            field("orderId", small_id()),
            field("clientOrderId", coid()),
            field("side", side_v()),
            field("type", tok(&["LIMIT", "MARKET", ""])),
            field("origQty", num_str()),
            field("executedQty", num_str()),
            field("cummulativeQuoteQty", num_str()),
            field("avgPrice", num_str()),
            field(
                "status",
                tok(&["NEW", "FILLED", "CANCELED", "PENDING_CANCEL", "EXPIRED", "REJECTED"]),
            ),
            field("updateTime", int()),
            field("time", int()),
        ])
    }

    /// Aster's SPOT fill rows are futures-shaped (`side` + `maker`), perp's the same, binance-spot
    /// shaped ones (`isBuyer` + `isMaker`) are tolerated: all keys, any mix.
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
            field("marginType", tok(&["isolated", "cross", ""])),
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

    /// A REST body as a venue endpoint would send it: junk, JSON noise, an array of `rows`, or a
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
        ]);
        prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => arb_text()].boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// USDⓈ-M user-data frames through aster's perp mapper (it never takes the TRADE_LITE
        /// hint: a `TRADE_LITE` frame must map to nothing).
        #[test]
        fn perp_mapper_is_total(frame in perp_frame()) {
            let events = map_aster_perp(&frame, "aster", "BTCUSDT.P");
            prop_assert!(events.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", events.len());
            if frame.get("e").and_then(Value::as_str) == Some("TRADE_LITE") {
                prop_assert!(events.is_empty(), "aster never maps the TRADE_LITE hint");
            }
        }

        /// Spot user-data frames with the REAL dispatch tokens.
        #[test]
        fn spot_mapper_reaches_its_arms(frame in spot_frame()) {
            let a = map_aster_private(&frame, "aster", "BTCUSDT");
            prop_assert!(a.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", a.len());
            let b = map_execution_report(&frame, "aster", "BTCUSDT");
            prop_assert!(b.len() < MAX_EVENTS_PER_FRAME, "event flood: {}", b.len());
        }

        /// The audit-A3 resync replays: `allOrders` + `userTrades` arrays.
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
            let spot = map_aster_history(&o, &t, "aster", "BTCUSDT");
            prop_assert!(bodies_are_arrays || spot.is_empty());
            prop_assert!(spot.len() <= bound, "history flood: {} > {}", spot.len(), bound);
            let perp = map_aster_perp_history(&o, &t, "aster", "BTCUSDT.P");
            prop_assert!(bodies_are_arrays || perp.is_empty());
            prop_assert!(perp.len() <= bound, "perp history flood: {} > {}", perp.len(), bound);
        }

        /// Every 1-arg reconcile body parser over junk text and over arrays of well-shaped rows.
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
                let _ = parse_spot_open_orders(body);
                let _ = parse_perp_open_orders(body);
            }
            for body in [&fills, &junk] {
                let _ = parse_spot_my_trades(body);
                let _ = parse_perp_user_trades(body);
            }
            for body in [&positions, &junk] {
                let _ = parse_perp_position_risk(body);
            }
            for body in [&balances, &rates, &junk] {
                let _ = parse_perp_balance(body);
                let _ = parse_spot_balance(body);
                let _ = parse_spot_fee_rates(body);
                let _ = parse_perp_fee_rates(body);
            }
        }

        /// Aster's OWN perp `exchangeInfo` parser (a twin of binance's, not the family's).
        #[test]
        fn perp_instrument_parser_is_total(payload in exchange_info()) {
            let _ = parse_aster_perp_instruments(&payload);
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
            let _ = map_perp_open_order(&row);
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
            let _ = map_perp_open_order(&row);
        }
    }
}
