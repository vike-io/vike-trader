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
//! Covered (all through the PUBLIC API): `tick::{route_frame, parse_books_frame, decode_bbo,
//! decode_trade}`, `market_feed::{klines::route_frame, mark::route_mark_frame, trades::parse_trades}`, the depth fold
//! (`market_feed::depth::okx_depth_decode` is `pub(crate)`, so `fold_deep` (in `decoder_never_panics/kit.rs`) restates it over its public
//! parts: `parse_books_frame` + `infer_tick_size` + `L2Book`), `history::parse_okx_klines`,
//! `catalog::{parse_instruments, parse_underlyings}`; under the default-on `execution` feature also
//! `event_mapper`, `recon_client::parse_*`, `funding::decode_okx_funding_bills`,
//! `history::map_okx_history`, `account_config::parse_account_identity`, `ws_auth::match_event_ack`,
//! `transport::unwrap_okx`, `error_codes`, and `perp::{parse_okx_perp_instruments,
//! VenueRestOkx::last_price}` behind a canned transport.
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use proptest::prelude::*;
use vike_marketdata::L2Book;
use vike_okx::{feed::tick, history};

#[path = "decoder_never_panics/kit.rs"]
mod kit;

#[cfg(feature = "execution")]
#[path = "decoder_never_panics/execution_kit.rs"]
mod execution_kit;

use kit::{
    BOOK_CHANNELS, INSTRUMENT, INSTRUMENTS, KIT, PUBLIC_CHANNELS, PUBLIC_KEYS, candle_row,
    drive_public_text, drive_public_value, envelope, fold_deep, frame_text, instrument_row,
    market_row, rest_envelope,
};
use vike_bridge_core::json_arb::{array_of, truncated};

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
    fn public_decoders_are_total_over_arbitrary_json(v in KIT.arb_json(PUBLIC_KEYS)) {
        drive_public_value(&v);
        drive_public_text(&v.to_string());
    }

    /// (b') well-shaped public envelopes with hostile leaves, whole and cut off mid-flight.
    #[test]
    fn public_decoders_are_total_over_envelopes(
        v in prop_oneof![
            envelope(PUBLIC_CHANNELS, market_row()),
            envelope(PUBLIC_CHANNELS, candle_row()),
            envelope(PUBLIC_CHANNELS, instrument_row()),
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
        instruments in rest_envelope(array_of(instrument_row(), 5)),
        families in rest_envelope(array_of(array_of(KIT.pick(INSTRUMENTS), 4), 3)),
    ) {
        for instrument in ["BTC-USDT", "BTC-USDT-SWAP"] {
            let _ = history::parse_okx_klines(&klines.to_string(), instrument);
        }
        for payload in [&instruments, &families] {
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
            let _ = tick::route_frame(f, INSTRUMENT, &mut shallow);
            fold_deep(f, &mut deep);
            prop_assert!(shallow.tick_size > 0.0);
            if let Some(b) = &deep {
                prop_assert!(b.tick_size > 0.0);
            }
        }
    }
}

// --- the exec plane (default-on `execution` feature) ---------------------------------------------

#[cfg(feature = "execution")]
mod exec_plane {
    use super::execution_kit::{
        ALL_KEYS, PRIVATE_CHANNELS, PRIVATE_KEYS, account_row, any_row, bill_row, check_private,
        ct_val, drive_execution_value, drive_reconcile_text, fill_row, misc_row, order_row,
        rest_over, rows,
    };
    use super::kit::{INSTRUMENT, KIT, envelope, junk_or, rest_envelope};
    use proptest::prelude::*;
    use vike_bridge_core::json_arb::{array_of, truncated};
    use vike_okx::execution::rest::parse_okx_perp_instruments;
    use vike_okx::transport::client::unwrap_okx;
    use vike_okx::{
        account::identity,
        execution::{event_mapper, funding, resync},
        reconcile,
        transport::{error_codes, ws_auth},
    };

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a) arbitrary text / byte noise through every reconcile-body parser.
        #[test]
        fn reconcile_parsers_are_total_over_text(text in any::<String>(), contract in ct_val()) {
            drive_reconcile_text(&text, contract);
        }

        #[test]
        fn reconcile_parsers_survive_byte_noise(
            bytes in prop::collection::vec(any::<u8>(), 0..256),
            contract in ct_val(),
        ) {
            drive_reconcile_text(&String::from_utf8_lossy(&bytes), contract);
        }

        /// (b) the reconcile bodies are JSON ARRAYS of rows: serialized arrays of every row shape,
        /// whole and cut off; a valid array must parse to exactly one report per row.
        #[test]
        fn reconcile_parsers_are_total_over_row_arrays(
            arr in rows(6),
            contract in ct_val(),
            cut in 0usize..600,
        ) {
            let n = arr.as_array().map_or(0, Vec::len);
            let body = arr.to_string();
            drive_reconcile_text(&body, contract);
            drive_reconcile_text(&truncated(&body, cut), contract);
            let pending = reconcile::parse_orders_pending(&body, contract);
            prop_assert!(matches!(&pending, Ok(r) if r.len() == n), "one report per row");
            let positions = reconcile::parse_positions(&body, INSTRUMENT, contract);
            prop_assert!(matches!(&positions, Ok(r) if !r.is_empty()), "never an empty snapshot");
            let fills = reconcile::parse_fills(&body, contract);
            prop_assert!(matches!(&fills, Ok(r) if r.len() <= n), "a fill report per row at most");
        }

        /// (b') arbitrary JSON over the real field names through every reconcile parser and every
        /// exec-plane value decoder.
        #[test]
        fn execution_decoders_are_total_over_arbitrary_json(v in KIT.arb_json(ALL_KEYS), contract in ct_val()) {
            drive_reconcile_text(&v.to_string(), contract);
            drive_execution_value(&v, contract);
        }

        /// (b'') the private WS dispatchers over arbitrary JSON.
        #[test]
        fn private_mappers_are_total_over_arbitrary_json(
            frame in KIT.arb_json(PRIVATE_KEYS),
            contract in ct_val(),
        ) {
            check_private(&frame, contract)?;
        }

        /// (b''') ...and over well-shaped `orders` / `account` envelopes whose rows reach the fill,
        /// liquidation and lifecycle arms.
        #[test]
        fn private_mappers_are_total_over_envelopes(
            frame in envelope(
                PRIVATE_CHANNELS,
                prop_oneof![order_row(), account_row(), KIT.arb_json(PRIVATE_KEYS)].boxed(),
            ),
            contract in ct_val(),
        ) {
            check_private(&frame, contract)?;
            drive_execution_value(&frame, contract);
        }

        /// ...and one `orders` row at a time, straight into the row mapper.
        #[test]
        fn order_row_mapper_is_total(row in order_row(), contract in ct_val()) {
            let one = event_mapper::map_okx_order(&row, "okx", INSTRUMENT);
            prop_assert!(one.len() <= 2, "one row flooded: {} events", one.len());
            let frame = serde_json::json!({
                "arg": {"channel": "orders", "instId": INSTRUMENT},
                "data": [row],
            });
            check_private(&frame, contract)?;
        }

        /// (c) a short sequence of private frames: the mappers hold no state, so a sequence is
        /// just N independent frames — kept to prove it stays that way.
        #[test]
        fn private_frame_sequences_never_panic(
            frames in prop::collection::vec(
                envelope(PRIVATE_CHANNELS, prop_oneof![order_row(), account_row()].boxed()),
                1..8,
            ),
            contract in ct_val(),
        ) {
            for f in &frames {
                check_private(f, contract)?;
            }
        }

        /// The history replay and the funding-bill decoder over arrays of fill / order / bill rows.
        #[test]
        fn history_and_funding_are_total(
            orders in junk_or(array_of(order_row(), 5)),
            fills in junk_or(array_of(fill_row(), 5)),
            bills in prop::collection::vec(prop_oneof![bill_row(), KIT.arb_json(ALL_KEYS)], 0..6),
            contract in ct_val(),
        ) {
            let events = resync::map_okx_history(&orders, &fills, "okx", INSTRUMENT, contract);
            let n_orders = orders.as_array().map_or(0, Vec::len);
            let n_fills = fills.as_array().map_or(0, Vec::len);
            // each order replays at most its own fills (a fill row joins to every order sharing its
            // ordId), two events per fill, plus one cancel
            prop_assert!(events.len() <= n_orders * (2 * n_fills + 1), "history flood");
            let out = funding::decode_okx_funding_bills(&bills, "okx", INSTRUMENT);
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
                KIT.arb_json(ALL_KEYS),
            ],
        ) {
            let _ = error_codes::by_msg(&msg);
            let _ = error_codes::by_code(code);
            let _ = ws_auth::match_event_ack(&v, &event);
            let _ = unwrap_okx(v.clone());
            let _ = identity::parse_account_identity(&v);
        }

        /// The decoder inside `VenueRestOkx`, behind a canned transport: the ticker. (Positions,
        /// balance and resting orders are read by the report path's parsers, fuzzed above.)
        #[test]
        fn perp_rest_decoders_are_total(
            body in rest_envelope(array_of(any_row(), 5)),
            contract in ct_val(),
        ) {
            let rest = rest_over(body.clone(), contract);
            let _ = rest.last_price();
            let _ = parse_okx_perp_instruments(&body);
        }

        /// `reconcile::parse_orders_pending` (the report path production runs) on one
        /// `orders-pending` row whose `sz * ct_val` may be non-finite (`sz` = "1e999" / "inf" /
        /// "NaN"; the legacy snapshot's row map was suspected of an `.expect("static shape")` panic
        /// there).
        #[test]
        fn open_order_mapper_is_total(row in order_row(), contract in ct_val()) {
            let _ = reconcile::parse_orders_pending(&serde_json::json!([row]).to_string(), contract);
        }
    }
}
