//! Totality harness for the IBKR WIRE decoders reachable through the public API: whatever a CP
//! Gateway frame, a reconcile REST body or a market-data tick carries, they may decode to nothing
//! (or return `Err`), but they must NEVER panic the exec/pump/reconcile thread, and one input must
//! never fabricate an event flood. The property is TOTALITY, not correctness.
//!
//! * `ibkr-cpapi`: `transport::cpapi_decode::{decode_ws_frame, decode_executions,
//!   decode_open_orders, decode_conid}` (the `sor` frame and the REST snapshots that become
//!   `IbInbound`s), the reconcile parsers `recon_client::parse_{order,fill,position}_reports` /
//!   `parse_balance` / `normalize_order_status` (text bodies), `OrderStatusKind::from_ib`, and the
//!   canonical-symbol grammar `contract::parse_simplified` with the `SecType` word table.
//! * `ibkr-socket`: the pure market-data mappers `market_feed::map::{QuoteAccumulator,
//!   trade_from, bar_from_realtime}` over hostile prices, sizes and timestamps.
//!
//! The socket backend's TWS wire framing is the published `ibapi` crate's (not this crate's), so
//! there is no in-crate byte decoder to drive. The STATEFUL fold the inbounds land in
//! (`EventMapper`, `IdRegistry`) is crate-private; its sequence harness is the sibling unit-test
//! file `src/event_mapper_props.rs`.
//!
//! Generators put the decoders' REAL field names into the object keys and real status/side words
//! into the leaves so they reach the branches instead of bouncing off the first `.get()`.
//!
//! Features: `--features ibkr` runs everything; `ibkr-cpapi` alone runs the cpapi module,
//! `ibkr-socket` alone the socket module. A minimized counterexample is a REAL bug — commit the
//! generated `.proptest-regressions` seed beside this file.
#![cfg(any(feature = "ibkr-cpapi", feature = "ibkr-socket"))]

use proptest::prelude::*;

/// i64 with the values that break arithmetic on a wire timestamp or count.
fn arb_i64() -> impl Strategy<Value = i64> {
    prop_oneof![
        Just(0i64),
        Just(1i64),
        Just(-1i64),
        Just(i64::MIN),
        Just(i64::MAX),
        Just(1_700_000_000_000i64),
        Just(1_700_000_000i64),
        any::<i64>(),
    ]
}

/// f64 with every IEEE special.
fn arb_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        Just(0.0f64),
        Just(-0.0f64),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(f64::MIN_POSITIVE),
        Just(f64::MAX),
        Just(f64::MIN),
        any::<f64>(),
    ]
}

#[cfg(feature = "ibkr-cpapi")]
mod cpapi {
    use super::*;

    use serde_json::{Value, json};
    use vike_ibkr::contract::{SecType, contract_details_to_properties, parse_simplified};
    use vike_ibkr::order::OrderStatusKind;
    use vike_ibkr::recon_client::{
        normalize_order_status, parse_balance, parse_fill_reports, parse_order_reports,
        parse_position_reports,
    };
    use vike_ibkr::transport::cpapi_decode::{
        decode_conid, decode_executions, decode_open_orders, decode_ws_frame,
    };

    // ---- field names the decoders actually read ---------------------------------------------------

    const WS_KEYS: &[&str] = &[
        "topic",
        "args",
        "orders",
        "cOID",
        "execId",
        "symbol",
        "side",
        "cumFill",
        "price",
        "ts",
        "commission",
        "currency",
        "status",
        "filledQuantity",
        "avgPrice",
        "orderId",
        "conid",
    ];
    const RECON_KEYS: &[&str] = &[
        "orders",
        "conid",
        "orderId",
        "order_ref",
        "cOID",
        "side",
        "orderType",
        "totalSize",
        "filledQuantity",
        "avgPrice",
        "avgCost",
        "status",
        "lastExecutionTime_r",
        "execution_id",
        "size",
        "price",
        "commission",
        "commission_currency",
        "trade_time_r",
        "position",
        "cashbalance",
        "USD",
        "BASE",
    ];

    // ---- generators -------------------------------------------------------------------------------

    /// The numeric strings IBKR (and the CP Gateway) hand back for prices and sizes.
    fn arb_numeric_string() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "0",
            "-0",
            "0.5",
            "1",
            "190.25",
            "1e3",
            "NaN",
            "inf",
            "-inf",
            "1e999",
            "",
            " ",
            "9223372036854775807",
            "-9223372036854775808",
            "99999999999999999999999999999999999999",
            "0x10",
        ])
        .prop_map(|s| s.to_string())
    }

    /// The words the decoders dispatch on.
    fn arb_domain_word() -> impl Strategy<Value = String> {
        prop::sample::select(vec![
            "sor",
            "str",
            "Submitted",
            "PreSubmitted",
            "PendingSubmit",
            "PendingCancel",
            "ApiPending",
            "Filled",
            "Cancelled",
            "ApiCancelled",
            "Inactive",
            "BUY",
            "SELL",
            "B",
            "S",
            "BOT",
            "SLD",
            "USD",
            "STK",
            "FUT",
            "OPT",
            "CASH",
            "CRYPTO",
            "true",
            "false",
        ])
        .prop_map(|s| s.to_string())
    }

    fn arb_leaf() -> BoxedStrategy<Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            arb_i64().prop_map(|n| Value::Number(n.into())),
            any::<f64>()
                .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
            arb_numeric_string().prop_map(Value::String),
            "[ -~]{0,12}".prop_map(Value::String),
            any::<String>().prop_map(Value::String),
            arb_domain_word().prop_map(Value::String),
            prop::sample::select(vec![json!([]), json!({})]),
        ]
        .boxed()
    }

    fn arb_key(names: &'static [&'static str]) -> impl Strategy<Value = String> {
        prop_oneof![
            9 => prop::sample::select(names.to_vec()).prop_map(|s| s.to_string()),
            1 => "[a-zA-Z_]{1,8}",
        ]
    }

    /// Arbitrary JSON, depth <= 4, objects keyed from `names`.
    fn arb_json(names: &'static [&'static str]) -> BoxedStrategy<Value> {
        arb_leaf()
            .prop_recursive(4, 64, 6, move |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                    prop::collection::vec((arb_key(names), inner), 0..6)
                        .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
                ]
            })
            .boxed()
    }

    /// The first half of `s` by CHARACTERS (a byte slice could split a code point in the harness
    /// itself).
    fn truncate_half(s: String) -> String {
        let keep = s.chars().count() / 2;
        s.chars().take(keep).collect()
    }

    /// A raw body: arbitrary unicode, lossy-decoded byte noise, valid JSON, truncated JSON.
    fn arb_text(names: &'static [&'static str]) -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            prop::collection::vec(any::<u8>(), 0..256)
                .prop_map(|b| String::from_utf8_lossy(&b).into_owned()),
            arb_json(names).prop_map(|v| v.to_string()),
            arb_json(names).prop_map(|v| truncate_half(v.to_string())),
        ]
    }

    /// A number as the gateway spells it: a JSON number, a hostile numeric string, or any leaf.
    fn arb_num() -> impl Strategy<Value = Value> {
        prop_oneof![
            3 => any::<f64>()
                .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
            2 => arb_i64().prop_map(|n| json!(n)),
            2 => arb_numeric_string().prop_map(Value::String),
            1 => arb_leaf(),
        ]
    }

    /// A short identifier from a small pool (so ids collide across rows), the empty string, or any
    /// leaf.
    fn arb_id() -> impl Strategy<Value = Value> {
        prop_oneof![
            4 => prop::sample::select(vec!["c-1", "c-2", "e1", "e2", "265598", "USD", ""])
                .prop_map(|s| json!(s)),
            1 => arb_leaf(),
        ]
    }

    fn arb_word() -> impl Strategy<Value = Value> {
        prop_oneof![
            4 => arb_domain_word().prop_map(Value::String),
            1 => arb_leaf(),
        ]
    }

    /// An execution row of the `sor` frame / `/iserver/account/trades` (carries `execId`).
    fn arb_exec_row() -> impl Strategy<Value = Value> {
        (
            arb_id(),
            arb_id(),
            arb_word(),
            arb_num(),
            arb_num(),
            (arb_num(), arb_id(), prop::option::of(arb_num())),
        )
            .prop_map(|(coid, exec_id, side, fill, price, (commission, currency, ts))| {
                let mut o = json!({
                    "cOID": coid, "execId": exec_id, "symbol": "AAPL", "side": side,
                    "cumFill": fill, "price": price, "commission": commission,
                    "currency": currency
                });
                if let Some(ts) = ts {
                    o["ts"] = ts;
                }
                o
            })
    }

    /// A status row of the `sor` frame (carries `status`).
    fn arb_status_row() -> impl Strategy<Value = Value> {
        (arb_id(), arb_word(), arb_num(), arb_num()).prop_map(|(coid, status, filled, avg)| {
            json!({
                "cOID": coid, "status": status, "filledQuantity": filled, "avgPrice": avg
            })
        })
    }

    fn arb_ws_row() -> impl Strategy<Value = Value> {
        prop_oneof![
            4 => arb_exec_row(),
            4 => arb_status_row(),
            2 => arb_json(WS_KEYS),
        ]
    }

    /// A WS frame: usually topic `sor` with an `args` array of rows, sometimes another topic or a
    /// non-array `args`.
    fn arb_ws_frame() -> impl Strategy<Value = Value> {
        prop_oneof![
            8 => (
                prop_oneof![
                    6 => Just(json!("sor")),
                    1 => Just(json!("str")),
                    1 => arb_leaf(),
                ],
                prop::collection::vec(arb_ws_row(), 0..5),
            )
                .prop_map(|(topic, rows)| json!({ "topic": topic, "args": rows })),
            1 => (Just(json!("sor")), arb_leaf())
                .prop_map(|(topic, args)| json!({ "topic": topic, "args": args })),
            3 => arb_json(WS_KEYS),
        ]
    }

    // ---- reconcile bodies -------------------------------------------------------------------------

    fn arb_conid() -> impl Strategy<Value = Value> {
        prop_oneof![
            5 => Just(json!(265_598)),
            2 => Just(json!("265598")),
            1 => arb_leaf(),
        ]
    }

    fn arb_order_row() -> impl Strategy<Value = Value> {
        (
            arb_conid(),
            arb_id(),
            arb_id(),
            arb_word(),
            arb_word(),
            (arb_num(), arb_num(), arb_num(), arb_word(), arb_num()),
        )
            .prop_map(
                |(conid, order_id, order_ref, side, kind, (total, filled, avg, status, ts))| {
                    json!({
                        "conid": conid, "orderId": order_id, "order_ref": order_ref,
                        "side": side, "orderType": kind, "totalSize": total,
                        "filledQuantity": filled, "avgPrice": avg, "status": status,
                        "lastExecutionTime_r": ts
                    })
                },
            )
    }

    fn arb_trade_row() -> impl Strategy<Value = Value> {
        (
            arb_conid(),
            arb_id(),
            arb_id(),
            arb_word(),
            (arb_num(), arb_num(), arb_num(), arb_id(), arb_num()),
        )
            .prop_map(
                |(conid, exec_id, order_id, side, (size, price, commission, cur, ts))| {
                    json!({
                        "conid": conid, "execution_id": exec_id, "orderId": order_id, "side": side,
                        "size": size, "price": price, "commission": commission,
                        "commission_currency": cur, "trade_time_r": ts
                    })
                },
            )
    }

    fn arb_position_row() -> impl Strategy<Value = Value> {
        (arb_conid(), arb_num(), prop::option::of(arb_num()), arb_num()).prop_map(
            |(conid, position, avg_price, avg_cost)| {
                let mut o = json!({ "conid": conid, "position": position, "avgCost": avg_cost });
                if let Some(a) = avg_price {
                    o["avgPrice"] = a;
                }
                o
            },
        )
    }

    /// The `/iserver/account/orders` body, the `/trades` and `/positions` arrays, the `/ledger`
    /// object — as shaped JSON text, or as arbitrary body text.
    fn arb_orders_body() -> impl Strategy<Value = String> {
        prop_oneof![
            5 => prop::collection::vec(arb_order_row(), 0..4)
                .prop_map(|rows| json!({ "orders": rows, "snapshot": true }).to_string()),
            2 => arb_text(RECON_KEYS),
        ]
    }

    fn arb_array_body(
        row: impl Strategy<Value = Value> + 'static,
    ) -> impl Strategy<Value = String> {
        prop_oneof![
            5 => prop::collection::vec(row, 0..4).prop_map(|rows| Value::Array(rows).to_string()),
            2 => arb_text(RECON_KEYS),
        ]
    }

    fn arb_ledger_body() -> impl Strategy<Value = String> {
        prop_oneof![
            5 => (arb_num(), arb_num(), arb_num()).prop_map(|(usd, base, other)| json!({
                "USD": { "cashbalance": usd, "netliquidationvalue": 1, "settledcash": 2 },
                "BASE": { "cashbalance": base },
                "EUR": other
            })
            .to_string()),
            2 => arb_text(RECON_KEYS),
        ]
    }

    fn arb_conid_param() -> impl Strategy<Value = i64> {
        prop_oneof![Just(265_598i64), Just(0i64), arb_i64()]
    }

    /// How many `args` rows a frame carries (0 for a non-array `args`).
    fn args_len(frame: &Value) -> usize {
        frame.get("args").and_then(Value::as_array).map_or(0, Vec::len)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a)+(b) The `sor` frame decoder is total over arbitrary and shaped frames, and one frame
        /// yields at most two inbounds per row (an execution row is ExecDetails + Commission).
        #[test]
        fn decode_ws_frame_is_total_over_json(
            shaped in arb_ws_frame(),
            generic in arb_json(WS_KEYS),
        ) {
            for frame in [&shaped, &generic] {
                let out = decode_ws_frame(frame);
                prop_assert!(
                    out.len() <= 2 * args_len(frame),
                    "event flood: {} inbounds from {} rows", out.len(), args_len(frame)
                );
            }
        }

        /// (a) ...and over arbitrary text: whatever parses is decoded, whatever does not is skipped
        /// (exactly what the WS pump does with a text frame).
        #[test]
        fn decode_ws_frame_is_total_over_arbitrary_text(text in arb_text(WS_KEYS)) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                let _ = decode_ws_frame(&v);
            }
        }

        /// (a)+(b) The REST snapshot decoders: `/trades` rows, the `/orders` snapshot, and the
        /// `secdef/search` conid. Bounded: executions are at most two inbounds per row, open
        /// orders exactly one per row.
        #[test]
        fn rest_snapshot_decoders_are_total_over_json(
            rows in prop::collection::vec(arb_ws_row(), 0..5),
            generic in arb_json(WS_KEYS),
            conid in prop_oneof![
                prop::collection::vec(arb_conid().prop_map(|c| json!({ "conid": c })), 0..3)
                    .prop_map(Value::Array),
                arb_json(WS_KEYS),
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
        fn recon_parsers_are_total_over_bodies(
            orders in arb_orders_body(),
            trades in arb_array_body(arb_trade_row()),
            positions in arb_array_body(arb_position_row()),
            ledger in arb_ledger_body(),
            raw in arb_text(RECON_KEYS),
            conid in arb_conid_param(),
            symbol in "[ -~]{0,16}",
            currency in prop::sample::select(vec!["USD", "EUR", "BASE", ""]),
        ) {
            for body in [&orders, &trades, &positions, &ledger, &raw] {
                let _ = parse_order_reports(body, conid, &symbol);
                let _ = parse_fill_reports(body, conid, &symbol);
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
            total in arb_f64(),
        ) {
            let kind = OrderStatusKind::from_ib(&raw);
            let _ = (kind.is_terminal(), kind.is_active());
            let _ = normalize_order_status(&raw, filled, total);
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
                }
            }
            let sec_type = SecType::from_ib_code(&code);
            let _ = (sec_type.as_ib_code().len(), sec_type.asset_class());
            let _ = contract_details_to_properties(&sec_type, tick, step, tick);
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
    use vike_ibkr::market_feed::map::{QuoteAccumulator, bar_from_realtime, trade_from};

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
            let mut acc = QuoteAccumulator::default();
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
                let _ = acc.apply(&tick);
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
                    tick_type: "AllLast".into(),
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
