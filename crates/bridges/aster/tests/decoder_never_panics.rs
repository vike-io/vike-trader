//! "Arbitrary input never panics" harness for the aster wire decoders. Aster is a Binance fork:
//! almost every decoder it serves is `vike_binance::family`'s, re-exported under aster's names
//! (`event_mapper`, `perp_mapper`, `history`: the `map_aster_*` aliases) or bound to the `"aster"`
//! venue string by a thin wrapper (`catalog`, `recon_client`'s 1-arg parsers). The shared
//! implementations are fuzzed in `crates/bridges/binance/tests/decoder_never_panics.rs`; this file
//! drives the SAME decoders through ASTER's public paths (so a wrapper that grows its own logic is
//! covered) plus the one decoder aster owns outright, `perp::parse_aster_perp_instruments`:
//!
//! - FEEDS plane (always compiled): `vike_binance::family::depth::route_frame` over arbitrary text,
//!   `catalog`. The structured depth payloads and frame sequences have no aster path, so they are
//!   fuzzed once, in binance's harness, over a vocabulary that contains every token of this file's;
//! - EXEC plane (`cfg(feature = "exec")`, default-on): `perp_mapper::map_aster_perp`,
//!   `event_mapper::map_aster_private` and the family's `map_execution_report`,
//!   `history::{map_aster_history, map_aster_perp_history}`, the nine 1-arg `recon_client`
//!   parsers, `perp::{parse_aster_perp_instruments, map_perp_open_order}`.
//!
//! The property is TOTALITY: a hostile, truncated or wrong-typed frame may decode to nothing or an
//! `Err`, but it must never panic a pump thread, and one frame must never fabricate an event flood.
//! Inputs are (a) arbitrary text, (b) lossy-decoded byte noise and (c) structured JSON whose keys
//! are the decoders' REAL field names and whose leaves are the dispatch tokens and hostile numerics
//! (`"NaN"`, `"1e999"`, `i64::MIN`, ...).
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
use vike_binance::family::depth::{MdEvent, route_frame};
use vike_bridge_core::json_arb::{TokenKit, object, tick_size, tok};
use vike_marketdata::L2Book;

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

/// Aster's vocabulary over the shared generator shapes (`vike_bridge_core::json_arb`).
const KIT: TokenKit = TokenKit { keys: KEYS, tokens: TOKENS, special: SPECIAL };

/// An `exchangeInfo` payload (spot or fapi flavour).
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
        KIT.field("notional", KIT.num_str()),
    ]);
    let entry = object(vec![
        KIT.field("symbol", tok(TOKENS)),
        KIT.field("baseAsset", tok(TOKENS)),
        KIT.field("quoteAsset", tok(TOKENS)),
        KIT.field("status", tok(TOKENS)),
        KIT.field("contractType", tok(TOKENS)),
        KIT.field("filters", prop::collection::vec(filter, 0..6).prop_map(Value::Array).boxed()),
    ]);
    prop_oneof![
        8 => prop::collection::vec(entry, 0..5).prop_map(|e| json!({ "symbols": e })),
        2 => KIT.arb_json(),
    ]
    .boxed()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `route_frame` over arbitrary text (JSON or not) into a fresh book: non-JSON is always
    /// `Ignored`, never a book mutation.
    #[test]
    fn route_frame_is_total_over_text(text in KIT.arb_text(), tick in tick_size()) {
        let mut book = L2Book::new(tick);
        let out = route_frame(&text, "BTCUSDT", &mut book);
        if serde_json::from_str::<Value>(&text).is_err() {
            prop_assert!(matches!(out, MdEvent::Ignored), "non-JSON must be Ignored");
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
    use vike_aster::event_mapper::map_aster_private;
    use vike_aster::history::{map_aster_history, map_aster_perp_history};
    use vike_aster::perp::{map_perp_open_order, parse_aster_perp_instruments};
    use vike_aster::perp_mapper::map_aster_perp;
    use vike_aster::recon_client::{
        parse_perp_balance, parse_perp_fee_rates, parse_perp_open_orders, parse_perp_position_risk,
        parse_perp_user_trades, parse_spot_balance, parse_spot_fee_rates, parse_spot_my_trades,
        parse_spot_open_orders,
    };
    use vike_binance::family::event_mapper::map_execution_report;
    use vike_bridge_core::json_arb::{Field, int, numeric_string};

    /// A u64 sequence number: small (so the sync rules hit equal / adjacent values), the extremes,
    /// or anything.
    fn seq() -> BoxedStrategy<Value> {
        vike_bridge_core::json_arb::seq(24)
    }

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
            KIT.field("s", tok(TOKENS)),
            KIT.field("c", coid()),
            KIT.field("C", coid()),
            KIT.field("x", tok(&["NEW", "CANCELED", "EXPIRED", "REJECTED", "TRADE", "CALCULATED"])),
            KIT.field("X", tok(&["NEW", "FILLED", "PARTIALLY_FILLED", "CANCELED"])),
            KIT.field("S", side_v()),
            KIT.field("r", tok(TOKENS)),
            KIT.field("i", seq()),
            KIT.field("t", prop_oneof![seq(), numeric_string().prop_map(Value::String)].boxed()),
            KIT.field("l", KIT.num_str()),
            KIT.field("L", KIT.num_str()),
            KIT.field("n", KIT.num_str()),
            KIT.field("N", tok(TOKENS)),
            KIT.field("m", bool_v()),
            KIT.field("ps", tok(&["BOTH", "LONG", "SHORT", ""])),
        ]
    }

    fn with_e(e: &'static str, mut fields: Vec<Field>) -> BoxedStrategy<Value> {
        fields.push(KIT.field("e", Just(Value::String(e.into())).boxed()));
        fields.push(KIT.field("T", int()));
        fields.push(KIT.field("E", int()));
        object(fields)
    }

    fn wallet_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("a", tok(TOKENS)),
            KIT.field("wb", prop_oneof![KIT.num_str(), KIT.arb_leaf()].boxed()),
            KIT.field("bc", KIT.num_str()),
            KIT.field("f", KIT.num_str()),
            KIT.field("l", KIT.num_str()),
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
            1 => KIT.arb_json(),
        ]
        .boxed()
    }

    /// A spot user-data frame, bare or inside the `{"event":…}` envelope.
    fn spot_frame() -> BoxedStrategy<Value> {
        let report = with_e("executionReport", order_fields());
        let account = (int(), prop::collection::vec(wallet_row(), 0..6))
            .prop_map(|(t, rows)| json!({ "e": "outboundAccountPosition", "E": t, "B": rows }));
        let inner = prop_oneof![4 => report, 2 => account, 1 => KIT.arb_json()].boxed();
        prop_oneof![
            3 => inner.clone(),
            3 => (small_id(), inner)
                .prop_map(|(id, ev)| json!({ "subscriptionId": id, "event": ev })),
        ]
        .boxed()
    }

    fn order_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("symbol", tok(TOKENS)),
            KIT.field("orderId", small_id()),
            KIT.field("clientOrderId", coid()),
            KIT.field("side", side_v()),
            KIT.field("type", tok(&["LIMIT", "MARKET", ""])),
            KIT.field("origQty", KIT.num_str()),
            KIT.field("executedQty", KIT.num_str()),
            KIT.field("cummulativeQuoteQty", KIT.num_str()),
            KIT.field("avgPrice", KIT.num_str()),
            KIT.field(
                "status",
                tok(&["NEW", "FILLED", "CANCELED", "PENDING_CANCEL", "EXPIRED", "REJECTED"]),
            ),
            KIT.field("updateTime", int()),
            KIT.field("time", int()),
        ])
    }

    /// Aster's SPOT fill rows are futures-shaped (`side` + `maker`), perp's the same, binance-spot
    /// shaped ones (`isBuyer` + `isMaker`) are tolerated: all keys, any mix.
    fn fill_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("id", prop_oneof![small_id(), seq()].boxed()),
            KIT.field("orderId", small_id()),
            KIT.field("symbol", tok(TOKENS)),
            KIT.field("qty", KIT.num_str()),
            KIT.field("price", KIT.num_str()),
            KIT.field("commission", KIT.num_str()),
            KIT.field("commissionAsset", tok(TOKENS)),
            KIT.field("time", int()),
            KIT.field("isBuyer", bool_v()),
            KIT.field("isMaker", bool_v()),
            KIT.field("side", side_v()),
            KIT.field("maker", bool_v()),
            KIT.field("positionSide", tok(&["BOTH", "LONG", "SHORT", ""])),
        ])
    }

    fn position_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("symbol", tok(TOKENS)),
            KIT.field("positionSide", tok(&["BOTH", "LONG", "SHORT", ""])),
            KIT.field("positionAmt", KIT.num_str()),
            KIT.field("entryPrice", KIT.num_str()),
            KIT.field("updateTime", int()),
            KIT.field("marginType", tok(&["isolated", "cross", ""])),
            KIT.field("isolatedWallet", KIT.num_str()),
            KIT.field("isolatedMargin", KIT.num_str()),
        ])
    }

    fn balance_row() -> BoxedStrategy<Value> {
        object(vec![
            KIT.field("asset", tok(TOKENS)),
            KIT.field("balance", KIT.num_str()),
            KIT.field("free", KIT.num_str()),
        ])
    }

    fn rate_body() -> BoxedStrategy<String> {
        let obj = object(vec![
            KIT.field("makerCommissionRate", KIT.num_str()),
            KIT.field("takerCommissionRate", KIT.num_str()),
            KIT.field(
                "commissionRates",
                object(vec![KIT.field("maker", KIT.num_str()), KIT.field("taker", KIT.num_str())]),
            ),
            KIT.field(
                "balances",
                prop::collection::vec(balance_row(), 0..4).prop_map(Value::Array).boxed(),
            ),
        ]);
        prop_oneof![8 => obj.prop_map(|v| v.to_string()), 2 => KIT.arb_text()].boxed()
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
            orders in prop::collection::vec(prop_oneof![9 => order_row(), 1 => KIT.arb_json()], 0..6),
            trades in prop::collection::vec(prop_oneof![9 => fill_row(), 1 => KIT.arb_json()], 0..6),
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
            orders in KIT.arb_body(order_row()),
            fills in KIT.arb_body(fill_row()),
            positions in KIT.arb_body(position_row()),
            balances in KIT.arb_body(balance_row()),
            rates in rate_body(),
            junk in KIT.arb_text(),
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
                KIT.field("symbol", tok(TOKENS)),
                KIT.field("orderId", small_id()),
                KIT.field("clientOrderId", coid()),
                KIT.field("side", side_v()),
                KIT.field("type", tok(&["LIMIT", "MARKET", ""])),
                KIT.field("origQty", numeric_string().prop_map(Value::String).boxed()),
                KIT.field("price", KIT.num_str()),
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
