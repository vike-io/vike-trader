//! Totality harness for the Deribit wire decoders: every function that turns raw SOCKET / REST
//! input (frame text, a `serde_json::Value`, a response body) into events, market data, chain rows
//! or reconcile reports is fed (a) arbitrary text and lossy-decoded byte noise, (b) random JSON
//! whose object keys are drawn from the decoders' REAL field names (so the generator reaches the
//! branches instead of bouncing off the first `.get()`), (c) well-shaped JSON-RPC subscription
//! frames and REST bodies whose fields are mostly well-typed with hostile leaves (`NaN`, `inf`,
//! `1e999`, `i64::MIN`, empty strings, wrong types), and (d) short frame SEQUENCES into ONE book /
//! bar folder.
//!
//! The property is TOTALITY: a hostile or truncated frame may decode to nothing (or to `Err`), but
//! it must never panic the pump thread that reads the socket. Outputs are asserted only where a
//! cheap bound must always hold (no event flood).
//!
//! Covered (all through the PUBLIC API): `market_data::{route_frame, classify_rpc_reply,
//! parse_quote, parse_trades, parse_chart_bar, parse_book_snapshot}`, the book / depth pump step
//! (`market_feed::fold_depth_frame` is private, so `pump_step` below restates it over its public
//! parts), `market_feed::BarFolder`, `options_feed::{parse_markprice_options, parse_ticker}`,
//! `dvol::{parse_dvol_frame, on_dvol_frame}`, `data::parse_deribit_klines`,
//! `catalog::{parse_instruments, asset_class_of, contract_of, properties_of}`, `chain::{parse_instrument_name,
//! list_expiries_from_summary, build_chain_from_summary, chain_snapshot_rows}`, `rpc::parse_response`;
//! under the default-on `exec` feature also `event_mapper`, `history::map_deribit_history`,
//! `reconcile::build_reconcile_snapshot`, `recon_client::parse_*`, `client::
//! parse_deribit_option_instruments`, `exec::parse_get_instrument` and `combo::{parse_combo,
//! parse_combo_grid, map_combo}`.
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use std::sync::Arc;

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::depth::infer_tick_size;
use vike_data::{MemHistStore, RecordingSink};
use vike_deribit::dvol::DvolRecorder;
use vike_deribit::market_data::MdEvent;
use vike_deribit::{catalog, chain, data, dvol, market_data, market_feed, options_feed, rpc};
use vike_marketdata::L2Book;

#[path = "decoder_never_panics/strategies.rs"]
mod strategies;

use strategies::*;

// --- the drivers ---------------------------------------------------------------------------

/// Restates `market_feed::fold_depth_frame` (private, unreachable from here) over its public
/// parts: the first snapshot anchors the book on its inferred grid, every later frame folds
/// through `route_frame`'s `prev_change_id` chain.
fn pump_step(text: &str, book: &mut Option<L2Book>) {
    match book.as_mut() {
        None => {
            let Ok(v) = serde_json::from_str::<Value>(text) else { return };
            let Some((seq, bids, asks)) = market_data::parse_book_snapshot(&v) else { return };
            let mut bk = L2Book::new(infer_tick_size(&bids, &asks));
            bk.apply_snapshot(seq, &bids, &asks);
            *book = Some(bk);
        }
        Some(bk) => {
            let _ = market_data::route_frame(text, INST, bk);
        }
    }
}

/// Every keyless `Value` decoder on one parsed payload.
fn drive_public_value(v: &Value) {
    let _ = market_data::classify_rpc_reply(v);
    let _ = market_data::parse_quote(v);
    let _ = market_data::parse_trades(v);
    let _ = market_data::parse_chart_bar(v);
    let _ = market_data::parse_book_snapshot(v);
    let _ = options_feed::parse_markprice_options(v, 0);
    let _ = options_feed::parse_ticker(v, 0);
    let _ = dvol::parse_dvol_frame(v);
    let _ = rpc::parse_response(v);
    let _ = catalog::parse_instruments(v);
    let _ = catalog::asset_class_of(v);
    let _ = catalog::contract_of(v);
    let _ = catalog::properties_of(v);
}

/// Every keyless text decoder on one frame.
fn drive_public_text(text: &str) {
    let mut book = L2Book::new(0.5);
    let _ = market_data::route_frame(text, INST, &mut book);
    let mut pumped = None;
    pump_step(text, &mut pumped);
    let _ = data::parse_deribit_klines(text);
    let sink = RecordingSink::default();
    let _ = dvol::on_dvol_frame(text, &sink, None);
    let recorder = DvolRecorder::new(Arc::new(MemHistStore::default()), true);
    let _ = dvol::on_dvol_frame(text, &sink, Some(&recorder));
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        drive_public_value(&v);
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

    /// (b') well-shaped subscription frames of every lane with hostile leaves, whole and cut off
    /// mid-flight.
    #[test]
    fn public_decoders_are_total_over_lane_frames(v in lane_frame(), cut in 0usize..400) {
        drive_public_value(&v);
        let text = v.to_string();
        drive_public_text(&text);
        drive_public_text(&truncated(&text, cut));
    }

    /// (b'') the REST bodies: chart data and the instrument list.
    #[test]
    fn rest_bodies_are_total(
        chart in chart_body(),
        insts in instruments_payload(),
        cut in 0usize..400,
    ) {
        let text = chart.to_string();
        let _ = data::parse_deribit_klines(&text);
        let _ = data::parse_deribit_klines(&truncated(&text, cut));
        drive_public_value(&insts);
    }

    /// (c) a short frame sequence into ONE book (the snapshot anchor + `change_id` chain, replays,
    /// gaps, junk in any order) never panics, and a Resync / Ignored leaves the book usable.
    #[test]
    fn book_fold_survives_frame_sequences(
        frames in prop::collection::vec(frame_text(prop_oneof![book_frame(), lane_frame()].boxed()), 1..8),
    ) {
        let mut book = L2Book::new(0.5);
        let mut pumped: Option<L2Book> = None;
        for f in &frames {
            let ev = market_data::route_frame(f, INST, &mut book);
            if let MdEvent::Trades(rows) = &ev {
                prop_assert!(!rows.is_empty(), "an empty Trades event");
            }
            pump_step(f, &mut pumped);
            prop_assert!(book.tick_size > 0.0);
            if let Some(b) = &pumped {
                prop_assert!(b.tick_size > 0.0);
            }
        }
    }

    /// (c') the closed-bar inference: any push sequence through `BarFolder` never panics, and it
    /// emits at most one close per push.
    #[test]
    fn bar_folder_survives_push_sequences(
        frames in prop::collection::vec(chart_frame(), 1..8),
    ) {
        let mut folder = market_feed::BarFolder::new();
        for f in &frames {
            if let Some(bar) = market_data::parse_chart_bar(f)
                && let Some(roll) = folder.fold(bar)
            {
                prop_assert!(roll.closed.as_ref().is_none_or(|c| c.ts < roll.forming.ts));
            }
        }
    }

    /// The `markprice.options` / `ticker` / DVOL frames, plus the instrument-name grammar the
    /// chain provider keys on.
    #[test]
    fn options_and_dvol_frames_are_total(
        mark in sub_frame(MARK_CH, array_of(markprice_row(), 5)),
        ticker in sub_frame(TICKER_CH, ticker_data()),
        dvol_frame in sub_frame(DVOL_CH, dvol_data()),
        name in hostile_name(),
        index in any::<String>(),
    ) {
        let _ = options_feed::parse_markprice_options(&mark, 0);
        let _ = options_feed::parse_ticker(&ticker, 0);
        let _ = dvol::parse_dvol_frame(&dvol_frame);
        let _ = chain::parse_instrument_name(&name);
        let _ = dvol::dvol_symbol(&index);
        let _ = dvol::dvol_channel(&index);
        let _ = options_feed::markprice_options_channel(&index);
    }

    /// The chain builder over realistic book-summary rows (valid calendar dates; the ISO expiry
    /// is the CALLER's, from `list_expiries`) with hostile premiums / IVs / spots, and the
    /// recorder rows derived from the result.
    #[test]
    fn chain_builder_is_total(
        rows in prop::collection::vec(summary_row(), 0..8),
        now in now_ms(),
        usd_quoted in any::<bool>(),
        r in prop_oneof![Just(0.0), Just(0.05), any::<f64>()],
    ) {
        let _ = chain::list_expiries_from_summary(&rows, now, None);
        let _ = chain::list_expiries_from_summary(&rows, now, Some("BTC"));
        for iso in ISO_EXPIRIES {
            for currency in ["BTC", "SOL"] {
                let built = chain::build_chain_from_summary(currency, &rows, iso, now, usd_quoted, r);
                let flat = chain::chain_snapshot_rows(&built);
                prop_assert!(flat.len() <= 2 * built.rows.len(), "more than a call and a put per strike");
            }
        }
    }

    /// ⚠ `chain::list_expiries_from_summary` over names with ANY day 0..=99 and any month: a
    /// grammar-valid but impossible date (`31FEB26`, `00JAN26`, `29FEB25`) reaches
    /// `vike_options::make_expiry`, whose `parse_iso` panics ("invalid calendar date"). Suspected
    /// real bug — see the report; the parser admits `\d{1,2}` days without a calendar check.
    #[test]
    fn expiry_listing_is_total_over_any_day_and_month(
        days in prop::collection::vec(0u32..100, 1..6),
        months in prop::collection::vec(0usize..12, 1..6),
        years in prop::collection::vec(0u32..100, 1..6),
        now in now_ms(),
    ) {
        const MONTHS: [&str; 12] =
            ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
        let rows: Vec<Value> = days
            .iter()
            .zip(&months)
            .zip(&years)
            .map(|((d, m), y)| {
                serde_json::json!({ "instrument_name": format!("BTC-{d}{}{y:02}-100000-C", MONTHS[*m]) })
            })
            .collect();
        let _ = chain::list_expiries_from_summary(&rows, now, None);
    }
}

// --- the exec plane (default-on `exec` feature) ---------------------------------------------

#[cfg(feature = "exec")]
mod exec_plane {
    use super::*;
    use proptest::test_runner::TestCaseError;
    use vike_deribit::{client, combo, event_mapper, exec, history, recon_client, reconcile};
    use vike_model::ComboLeg;

    const ALL_KEYS: &[&str] = &[
        "jsonrpc",
        "method",
        "params",
        "channel",
        "data",
        "id",
        "result",
        "error",
        "code",
        "message",
        "trade_id",
        "label",
        "order_id",
        "instrument_name",
        "direction",
        "amount",
        "price",
        "fee",
        "fee_currency",
        "liquidity",
        "timestamp",
        "state",
        "order_state",
        "order_type",
        "filled_amount",
        "average_price",
        "last_update_timestamp",
        "creation_timestamp",
        "size",
        "mark_price",
        "delta",
        "balance",
        "type",
        "maker_commission",
        "taker_commission",
        "legs",
        "tick_size",
        "min_trade_amount",
        "contract_size",
        "tick_size_steps",
        "above_price",
        "kind",
        "settlement_period",
        "is_active",
        "trades",
    ];

    /// One `get_open_orders_by_instrument` / `get_order_history` row.
    fn order_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("label", idv()),
            ("order_id", idv()),
            ("instrument_name", pick(INSTS)),
            ("direction", pick(&["buy", "sell", ""])),
            ("order_type", pick(&["limit", "market", "stop_limit", ""])),
            ("order_state", pick(&["open", "filled", "cancelled", "rejected", ""])),
            ("amount", jnum()),
            ("filled_amount", jnum()),
            ("average_price", jnum()),
            (
                "price",
                prop_oneof![jnum(), Just(Value::String("market_price".to_string())).boxed()]
                    .boxed(),
            ),
            ("last_update_timestamp", jint()),
            ("creation_timestamp", jint()),
        ])
    }

    /// One `get_positions` row.
    fn position_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("instrument_name", pick(INSTS)),
            ("direction", pick(&["buy", "sell", "zero", ""])),
            ("size", jnum()),
            ("average_price", jnum()),
            ("mark_price", jnum()),
            ("delta", jnum()),
        ])
    }

    fn any_row() -> BoxedStrategy<Value> {
        prop_oneof![trade_row(), order_row(), position_row(), instrument_row(), arb_json(ALL_KEYS),]
            .boxed()
    }

    fn rows(max: usize) -> BoxedStrategy<Value> {
        array_of(any_row(), max)
    }

    /// Account-summary / fee-rate shaped objects.
    fn account_obj() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("balance", jnum()),
            ("id", jint()),
            ("type", pick(&["main", "subaccount", "sub_account", ""])),
            ("maker_commission", jnum()),
            ("taker_commission", jnum()),
        ])
    }

    /// A combo leg list as `private/create_combo` / `get_combo_details` answers it.
    fn combo_result() -> BoxedStrategy<Value> {
        let leg = obj_of(vec![
            ("instrument_name", pick(INSTS)),
            ("amount", prop_oneof![jnum(), jint()].boxed()),
        ]);
        obj_of(vec![
            ("id", pick(&["BTC-FS-27JUN26_PERP", ""])),
            ("state", pick(&["active", "inactive", ""])),
            ("legs", junk_or(array_of(leg, 4))),
            ("tick_size", jnum()),
            ("min_trade_amount", jnum()),
        ])
    }

    fn legs() -> BoxedStrategy<Vec<ComboLeg>> {
        prop::collection::vec(
            (prop::sample::select(INSTS), any::<i32>())
                .prop_map(|(symbol, ratio)| ComboLeg { symbol: symbol.to_string(), ratio }),
            0..5,
        )
        .boxed()
    }

    /// Every REST-body text decoder of the reconcile client on one body.
    fn drive_recon_text(body: &str, now: i64) {
        let _ = recon_client::parse_open_orders(body);
        let _ = recon_client::parse_positions(body, INST, now);
        let _ = recon_client::parse_user_trades(body);
        let _ = recon_client::parse_account_summary_balance(body);
        let _ = recon_client::parse_account_identity(body);
        let _ = recon_client::parse_fee_rate(body);
    }

    /// Every exec-plane `Value` decoder on one payload.
    fn drive_exec_value(v: &Value) {
        let _ = client::parse_deribit_option_instruments(v);
        let _ = exec::parse_get_instrument(v);
        let _ = combo::parse_combo(v);
        let _ = combo::parse_combo_grid(v);
        let empty = Value::Null;
        let _ = history::map_deribit_history(v, &empty, "deribit", INST);
        let _ = history::map_deribit_history(&empty, v, "deribit", INST);
        let _ = history::map_deribit_history(v, v, "deribit", INST);
    }

    /// The private-WS dispatcher on one frame; no event flood (a row yields <= 2 events).
    fn check_private(frame: &Value) -> Result<(), TestCaseError> {
        let n_rows = frame
            .get("params")
            .and_then(|p| p.get("data"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let evs = event_mapper::map_deribit_private(frame, "deribit", INST);
        prop_assert!(evs.len() <= 2 * n_rows, "flood: {} events from {n_rows} rows", evs.len());
        let one = event_mapper::map_deribit_trade(frame, "deribit", INST);
        prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a) arbitrary text / byte noise through every reconcile-body parser.
        #[test]
        fn recon_parsers_are_total_over_text(text in any::<String>(), now in now_ms()) {
            drive_recon_text(&text, now);
        }

        #[test]
        fn recon_parsers_survive_byte_noise(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            now in now_ms(),
        ) {
            drive_recon_text(&String::from_utf8_lossy(&bytes), now);
        }

        /// (b) the reconcile bodies: serialized arrays of every row shape (whole and cut off) and
        /// account-shaped objects; a valid array must parse to exactly one report per row.
        #[test]
        fn recon_parsers_are_total_over_bodies(
            arr in rows(6),
            obj in account_obj(),
            now in now_ms(),
            cut in 0usize..600,
        ) {
            let n = arr.as_array().map_or(0, Vec::len);
            let body = arr.to_string();
            drive_recon_text(&body, now);
            drive_recon_text(&truncated(&body, cut), now);
            drive_recon_text(&obj.to_string(), now);
            let orders = recon_client::parse_open_orders(&body);
            prop_assert!(matches!(&orders, Ok(r) if r.len() == n), "one report per row");
            let fills = recon_client::parse_user_trades(&body);
            prop_assert!(matches!(&fills, Ok(r) if r.len() <= n), "a fill report per row at most");
            let positions = recon_client::parse_positions(&body, INST, now);
            prop_assert!(matches!(&positions, Ok(r) if r.len() <= n), "a position per row at most");
        }

        /// (b') arbitrary JSON over the real field names through every exec-plane decoder.
        #[test]
        fn exec_decoders_are_total_over_arbitrary_json(v in arb_json(ALL_KEYS), now in now_ms()) {
            drive_recon_text(&v.to_string(), now);
            drive_exec_value(&v);
            check_private(&v)?;
        }

        /// (b'') the private `user.trades` dispatcher over well-shaped subscription frames.
        #[test]
        fn private_mapper_is_total_over_user_trade_frames(
            frame in sub_frame(USER_TRADES_CH, array_of(trade_row(), 5)),
        ) {
            check_private(&frame)?;
            drive_exec_value(&frame);
        }

        /// ...and one `user.trades` row at a time, straight into the row mapper.
        #[test]
        fn trade_row_mapper_is_total(row in trade_row()) {
            let one = event_mapper::map_deribit_trade(&row, "deribit", INST);
            prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
        }

        /// (c) a short sequence of private frames: the mapper holds no state, so a sequence is
        /// just N independent frames — kept to prove it stays that way.
        #[test]
        fn private_frame_sequences_never_panic(
            frames in prop::collection::vec(
                sub_frame(USER_TRADES_CH, array_of(trade_row(), 4)),
                1..8,
            ),
        ) {
            for f in &frames {
                check_private(f)?;
            }
        }

        /// The history replay over arrays of order / trade rows.
        #[test]
        fn history_replay_is_total(
            orders in junk_or(array_of(order_row(), 5)),
            trades in junk_or(array_of(trade_row(), 5)),
        ) {
            let evs = history::map_deribit_history(&orders, &trades, "deribit", INST);
            let n_orders = orders.as_array().map_or(0, Vec::len);
            let n_trades = trades.as_array().map_or(0, Vec::len);
            // two events per trade, at most one terminal per order
            prop_assert!(evs.len() <= 2 * n_trades + n_orders, "history flood");
        }

        /// The instrument payloads: the plural list, the singular grid, the combo grid.
        #[test]
        fn instrument_payloads_are_total(
            list in instruments_payload(),
            single in obj_of(vec![("result", junk_or(instrument_row()))]),
            combo_grid in obj_of(vec![("result", junk_or(combo_result()))]),
        ) {
            let _ = client::parse_deribit_option_instruments(&list);
            let _ = exec::parse_get_instrument(&single);
            let _ = exec::parse_get_instrument(&list);
            let _ = combo::parse_combo_grid(&combo_grid);
            let _ = combo::parse_combo_grid(&single);
        }

        /// `client::option_base_asset` (private) is reached through the option-instrument list:
        /// hostile instrument names, non-ASCII expiries included, must not panic the slicing.
        #[test]
        fn option_instrument_names_are_total(names in prop::collection::vec(hostile_name(), 1..6)) {
            let rows: Vec<Value> = names
                .iter()
                .map(|n| serde_json::json!({ "kind": "option", "instrument_name": n }))
                .collect();
            let payload = serde_json::json!({ "result": rows });
            let _ = client::parse_deribit_option_instruments(&payload);
            for n in &names {
                let _ = chain::parse_instrument_name(n);
            }
        }

        /// The combo reply parser and the leg-mapping solver (arbitrary i32 ratios, duplicate and
        /// unmatched symbols, empty lists).
        #[test]
        fn combo_decoders_are_total(
            reply in combo_result(),
            spec in legs(),
            venue in legs(),
        ) {
            let parsed = combo::parse_combo(&reply);
            let _ = combo::map_combo(&spec, &venue);
            if let Some(c) = &parsed {
                let _ = combo::map_combo(&spec, &c.legs);
                let _ = combo::map_combo(&c.legs, &c.legs);
            }
        }

        /// ⚠ `reconcile::build_reconcile_snapshot` on `get_positions` / `get_open_orders` results.
        /// Suspected `.expect("static shape")` panic in `build_orders` when an open order's
        /// `amount` is a string that parses non-finite ("inf" / "NaN" / "1e999"): the `json!`
        /// image of a non-finite `qty` is `null`, which `OrderRequest.qty: f64` refuses.
        #[test]
        fn reconcile_snapshot_is_total(
            positions in junk_or(array_of(position_row(), 4)),
            orders in junk_or(array_of(order_row(), 4)),
        ) {
            let snap = reconcile::build_reconcile_snapshot(&positions, &orders, INST);
            prop_assert!(!snap.positions.is_empty(), "a snapshot always names the symbol");
        }
    }
}
