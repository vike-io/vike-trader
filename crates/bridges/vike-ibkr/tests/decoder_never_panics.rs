//! Totality harness for the IBKR WIRE decoders reachable through the public API: whatever a CP
//! Gateway frame, a reconcile REST body or a market-data tick carries, they may decode to nothing
//! (or return `Err`), but they must NEVER panic the exec/pump/reconcile thread, and one input must
//! never fabricate an event flood. The property is TOTALITY, not correctness.
//!
//! * `ibkr-cpapi`: `transport::cpapi_decode::{decode_websocket_frame, decode_executions,
//!   decode_open_orders, decode_conid}` (the `sor` frame and the REST snapshots that become
//!   `IbkrInbound`s), the reconcile parsers `recon::cpapi::parse_{order,fill,position}_reports` /
//!   `parse_balance` / `normalize_order_status` (text bodies), `OrderStatusKind::from_ibkr`, the
//!   canonical-symbol grammar `contract::parse_simplified` with the `SecType` word table, and the
//!   market-data decoders `cpapi_feed::decode::{SmdAccumulator, smd_conid, snapshot_row,
//!   decode_history, parse_market_data_price, parse_market_data_size}` with the history request words.
//! * `ibkr-socket`: the pure market-data mappers `market_feed::map::{QuoteAccumulator,
//!   trade_from, bar_from_realtime}` over hostile prices, sizes and timestamps.
//!
//! The socket backend's TWS wire framing is the published `ibapi` crate's (not this crate's), so
//! there is no in-crate byte decoder to drive. The STATEFUL fold the inbounds land in
//! (`EventMapper`, `IdRegistry`) is crate-private; its sequence harness is the sibling unit-test
//! file `src/execution/event_mapper_property_tests.rs`.
//!
//! Generators put the decoders' REAL field names into the object keys and real status/side words
//! into the leaves so they reach the branches instead of bouncing off the first `.get()`.
//!
//! Features: `--features ibkr` runs everything; `ibkr-cpapi` alone runs the cpapi module,
//! `ibkr-socket` alone the socket module. A minimized counterexample is a REAL bug — commit the
//! generated `.proptest-regressions` seed beside this file.
#![cfg(any(feature = "ibkr-cpapi", feature = "ibkr-socket"))]

use proptest::prelude::*;
use vike_bridge_core::json_arb::{arb_f64, arb_i64};

#[cfg(feature = "ibkr-cpapi")]
#[path = "decoder_never_panics/cpapi_kit.rs"]
mod cpapi_kit;

#[cfg(feature = "ibkr-cpapi")]
mod cpapi {
    use super::*;
    use crate::cpapi_kit::{
        RECONCILE_KEYS, WEBSOCKET_KEYS, arb_array_body, arb_conid, arb_conid_param,
        arb_domain_word, arb_json, arb_ledger_body, arb_orders_body, arb_position_row,
        arb_trade_row, arb_websocket_frame, arb_websocket_row,
    };
    use vike_bridge_core::json_arb::json_text;

    use serde_json::{Value, json};
    use vike_ibkr::execution::order::OrderStatusKind;
    use vike_ibkr::instrument::contract::{
        SecType, contract_details_to_properties, parse_simplified, resolve_multiplier,
    };
    use vike_ibkr::reconcile::cpapi::{
        normalize_order_status, parse_balance, parse_fill_reports, parse_order_reports,
        parse_position_reports,
    };
    use vike_ibkr::transport::cpapi_decode::{
        decode_conid, decode_executions, decode_open_orders, decode_websocket_frame,
    };

    /// How many `args` rows a frame carries (0 for a non-array `args`).
    fn args_len(frame: &Value) -> usize {
        frame.get("args").and_then(Value::as_array).map_or(0, Vec::len)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a)+(b) The `sor` frame decoder is total over arbitrary and shaped frames, and one frame
        /// yields at most two inbounds per row (an execution row is ExecutionDetails + Commission).
        #[test]
        fn decode_websocket_frame_is_total_over_json(
            shaped in arb_websocket_frame(),
            generic in arb_json(WEBSOCKET_KEYS),
        ) {
            for frame in [&shaped, &generic] {
                let out = decode_websocket_frame(frame);
                prop_assert!(
                    out.len() <= 2 * args_len(frame),
                    "event flood: {} inbounds from {} rows", out.len(), args_len(frame)
                );
            }
        }

        /// (a) ...and over arbitrary text: whatever parses is decoded, whatever does not is skipped
        /// (exactly what the WS pump does with a text frame).
        #[test]
        fn decode_websocket_frame_is_total_over_arbitrary_text(text in json_text(arb_json(WEBSOCKET_KEYS))) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let _ = decode_websocket_frame(&v);
            }
        }

        /// (a)+(b) The REST snapshot decoders: `/trades` rows, the `/orders` snapshot, and the
        /// `secdef/search` conid. Bounded: executions are at most two inbounds per row, open
        /// orders exactly one per row.
        #[test]
        fn rest_snapshot_decoders_are_total_over_json(
            rows in prop::collection::vec(arb_websocket_row(), 0..5),
            generic in arb_json(WEBSOCKET_KEYS),
            conid in prop_oneof![
                prop::collection::vec(arb_conid().prop_map(|c| json!({ "conid": c })), 0..3)
                    .prop_map(Value::Array),
                arb_json(WEBSOCKET_KEYS),
            ],
        ) {
            let n = rows.len();
            let executions = decode_executions(&Value::Array(rows.clone()));
            prop_assert!(executions.len() <= 2 * n, "event flood: {} inbounds", executions.len());
            let open = decode_open_orders(&json!({ "orders": rows }));
            prop_assert!(open.len() <= n, "event flood: {} inbounds", open.len());

            let _ = decode_executions(&generic);
            let _ = decode_open_orders(&generic);
            let _ = decode_conid(&generic);
            let _ = decode_conid(&conid);
        }

        /// (a)+(b) The reconcile parsers over shaped bodies (with a matching and a non-matching
        /// conId) and over arbitrary text; `Err` is fine, a panic is not.
        #[test]
        fn reconcile_parsers_are_total_over_bodies(
            orders in arb_orders_body(),
            trades in arb_array_body(arb_trade_row()),
            positions in arb_array_body(arb_position_row()),
            ledger in arb_ledger_body(),
            raw in json_text(arb_json(RECONCILE_KEYS)),
            conid in arb_conid_param(),
            symbol in "[ -~]{0,16}",
            currency in prop::sample::select(vec!["USD", "EUR", "BASE", ""]),
        ) {
            for body in [&orders, &trades, &positions, &ledger, &raw] {
                let _ = parse_order_reports(body, &symbol, conid, &symbol);
                let _ = parse_fill_reports(body, &symbol, conid, &symbol);
                let _ = parse_position_reports(body, conid, &symbol);
                let _ = parse_balance(body, currency);
            }
        }

        /// (a) The status vocabulary: every string maps to a kind, and the pure normalizer takes
        /// any status and any progress numbers.
        #[test]
        fn order_status_vocabulary_is_total(
            raw in prop_oneof![arb_domain_word(), any::<String>()],
            filled in arb_f64(),
            // kept so the strategy, and so every committed seed, draws as it did
            _total in arb_f64(),
        ) {
            let kind = OrderStatusKind::from_ibkr(&raw);
            let _ = (kind.is_terminal(), kind.is_active());
            let _ = normalize_order_status(&raw, filled);
        }

        /// (a) The canonical-symbol grammar over arbitrary strings and over grammar-shaped ones
        /// (dot-joined fields drawn from the words it dispatches on), and the `SecType` word table
        /// IBKR's own words land in.
        #[test]
        fn canonical_symbol_grammar_is_total(
            raw in any::<String>(),
            fields in prop::collection::vec(
                prop_oneof![
                    prop::sample::select(vec![
                        "AAPL", "EUR", "USD", "SMART", "NASDAQ", "IDEALPRO", "GLOBEX", "STK",
                        "FUT", "OPT", "CASH", "IND", "CRYPTO", "20251219", "202512", "C", "P",
                        "CALL", "PUT", "100", "150.5", "", "é",
                    ])
                    .prop_map(|s| s.to_string()),
                    "[ -~]{0,6}",
                ],
                0..9,
            ),
            code in prop_oneof![arb_domain_word(), any::<String>()],
            tick in arb_f64(),
            step in arb_f64(),
        ) {
            for canonical in [raw, fields.join(".")] {
                if let Ok(contract) = parse_simplified(&canonical) {
                    let _ = contract.conid_search_is_ambiguous();
                    let _ = contract.sec_type.asset_class();
                    let _ = contract.to_contract();
                }
            }
            let sec_type = SecType::from_ibkr_code(&code);
            let _ = (sec_type.as_ibkr_code().len(), sec_type.asset_class());
            let multiplier = resolve_multiplier(Some(code.as_str()), [code.as_str(), "50"]);
            let _ = contract_details_to_properties(&sec_type, tick, step, tick, multiplier);
        }
    }

    // ---- the cpapi MARKET-DATA decoders (decision 0140) -------------------------------------------

    use crate::cpapi_kit::{arb_history_body, arb_market_data_value, arb_smd_row};
    use vike_ibkr::feed::cpapi::decode::{
        SmdAccumulator, decode_history, history_bar_param, history_period, parse_market_data_price,
        parse_market_data_size, smd_conid, snapshot_row,
    };

    /// The keys the market-data decoders read: the `smd` field codes, the frame envelope, and the
    /// history body.
    const MARKET_DATA_KEYS: &[&str] = &[
        "topic",
        "conid",
        "_updated",
        "31",
        "84",
        "85",
        "86",
        "88",
        "7059",
        "6509",
        "data",
        "t",
        "o",
        "h",
        "l",
        "c",
        "v",
        "mdAvailability",
        "mktDataDelay",
    ];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a)+(b) The `smd` fold is total over shaped and arbitrary rows, a SEQUENCE of them
        /// included (the accumulator is stateful), and yields at most one quote and one print per
        /// row — by type — whose numbers are finite.
        #[test]
        fn smd_fold_is_total_over_rows(
            rows in prop::collection::vec(prop_oneof![arb_smd_row(), arb_json(MARKET_DATA_KEYS)], 0..8),
            local_ts in arb_i64(),
        ) {
            let mut accumulator = SmdAccumulator::default();
            for row in &rows {
                let up = accumulator.apply(row, local_ts);
                if let Some(q) = &up.quote {
                    prop_assert!(q.quote.bid.is_finite() && q.quote.ask.is_finite());
                    prop_assert!(q.quote.bid_size >= 0.0 && q.quote.ask_size >= 0.0);
                }
                if let Some(t) = &up.trade {
                    prop_assert!(t.trade.price.is_finite() && t.trade.size >= 0.0);
                }
                let _ = smd_conid(row);
                let _ = accumulator.current_quote(local_ts);
            }
        }

        /// (a) ...and over arbitrary text, exactly as the pump reads a text frame.
        #[test]
        fn smd_fold_is_total_over_arbitrary_text(text in json_text(arb_json(MARKET_DATA_KEYS))) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let _ = SmdAccumulator::default().apply(&v, 0);
                let _ = smd_conid(&v);
            }
        }

        /// (a)+(b) The snapshot row picker and the history decoder are total, and a history body
        /// never yields more bars than it has rows, each with finite prices, ascending and unique.
        #[test]
        fn rest_market_data_decoders_are_total(
            history in prop_oneof![arb_history_body(), arb_json(MARKET_DATA_KEYS)],
            snapshot in prop_oneof![
                prop::collection::vec(arb_smd_row(), 0..4).prop_map(Value::Array),
                arb_json(MARKET_DATA_KEYS),
            ],
            conid in arb_conid_param(),
            interval_ms in arb_i64(),
            leaf in arb_market_data_value(),
        ) {
            let rows = history.get("data").and_then(Value::as_array).map_or(0, Vec::len);
            let h = decode_history(&history);
            prop_assert!(h.bars.len() <= rows, "{} bars from {rows} rows", h.bars.len());
            for b in &h.bars {
                prop_assert!(b.open.is_finite() && b.high.is_finite() && b.low.is_finite());
                prop_assert!(b.close.is_finite() && b.volume >= 0.0);
            }
            prop_assert!(h.bars.windows(2).all(|w| w[0].timestamp < w[1].timestamp), "ascending, unique");
            let _ = snapshot_row(&snapshot, conid);
            let _ = snapshot_row(&snapshot, 265598);
            let _ = (history_bar_param(interval_ms), history_period(interval_ms));
            let _ = (parse_market_data_price(&leaf), parse_market_data_size(&leaf));
        }
    }
}

#[cfg(feature = "ibkr-socket")]
mod socket {
    use super::*;

    use ibapi::contracts::tick_types::TickType;
    use ibapi::market_data::realtime::{
        Bar as RtBar, TickAttribute, TickPrice, TickSize, TickTypes, Trade, TradeAttribute,
    };
    use vike_ibkr::feed::socket::map::{QuoteAccumulator, bar_from_realtime, trade_from};

    /// The eight bid/ask price and size tick types the accumulator folds (realtime and delayed).
    fn tick_type_of(i: u8) -> TickType {
        match i % 8 {
            0 => TickType::Bid,
            1 => TickType::Ask,
            2 => TickType::BidSize,
            3 => TickType::AskSize,
            4 => TickType::DelayedBid,
            5 => TickType::DelayedAsk,
            6 => TickType::DelayedBidSize,
            _ => TickType::DelayedAskSize,
        }
    }

    fn attr() -> TickAttribute {
        TickAttribute { can_auto_execute: false, past_limit: false, pre_open: false }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (c) A random tick sequence (prices and sizes on all eight bid/ask tick types, hostile
        /// numbers) folds into ONE `QuoteAccumulator` without a panic.
        #[test]
        fn quote_accumulator_tick_sequences_never_panic(
            ticks in prop::collection::vec((any::<u8>(), any::<bool>(), arb_f64()), 1..16),
        ) {
            let mut accumulator = QuoteAccumulator::default();
            for (kind, is_price, value) in ticks {
                let tick = if is_price {
                    TickTypes::Price(TickPrice {
                        tick_type: tick_type_of(kind),
                        price: value,
                        attributes: attr(),
                    })
                } else {
                    TickTypes::Size(TickSize { tick_type: tick_type_of(kind), size: value })
                };
                let _ = accumulator.apply(&tick);
            }
        }

        /// (a) The trade and realtime-bar mappers over any representable timestamp and hostile
        /// numbers (`unix_timestamp() * 1000` must hold across the whole `OffsetDateTime` range).
        #[test]
        fn trade_and_bar_mappers_never_panic(
            secs in prop_oneof![arb_i64(), -62_135_596_800i64..=253_402_300_799i64],
            price in arb_f64(),
            size in arb_f64(),
        ) {
            if let Ok(time) = time::OffsetDateTime::from_unix_timestamp(secs) {
                let trade = Trade {
                    time,
                    price,
                    size,
                    trade_attribute: TradeAttribute { past_limit: false, unreported: false },
                    exchange: "SMART".into(),
                    special_conditions: String::new(),
                };
                let _ = trade_from(&trade);
                let bar = RtBar {
                    date: time,
                    open: price,
                    high: size,
                    low: price,
                    close: size,
                    volume: size,
                    wap: price,
                    count: 7,
                };
                let _ = bar_from_realtime(&bar);
            }
        }
    }
}
