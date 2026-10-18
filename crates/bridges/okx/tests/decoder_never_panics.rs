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
//! decode_trade}`, `market_feed::{klines::route_frame, mark::route_mark_frame, trades::parse_trades}`, the depth fold
//! (`market_feed::depth::okx_depth_decode` is `pub(crate)`, so `fold_deep` (in `decoder_never_panics/kit.rs`) restates it over its public
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
use vike_marketdata::L2Book;
use vike_okx::{data, market_data};

#[path = "decoder_never_panics/kit.rs"]
mod kit;

#[cfg(feature = "exec")]
#[path = "decoder_never_panics/exec_kit.rs"]
mod exec_kit;

use kit::{
    BOOK_CHANNELS, INST, INSTS, PUBLIC_CHANNELS, PUBLIC_KEYS, arb_json, array_of, candle_row,
    drive_public_text, drive_public_value, envelope, fold_deep, frame_text, inst_row, market_row,
    pick, rest_envelope, truncated,
};

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
    use crate::exec_kit::{
        ALL_KEYS, PRIVATE_CHANNELS, PRIVATE_KEYS, account_row, any_row, bill_row, check_private,
        ct_val, drive_exec_value, drive_recon_text, fill_row, misc_row, order_row, rest_over, rows,
    };
    use crate::kit::{INST, arb_json, array_of, envelope, junk_or, rest_envelope, truncated};
    use proptest::prelude::*;
    use vike_okx::perp::{map_okx_open_order, parse_okx_perp_instruments};
    use vike_okx::transport::unwrap_okx;
    use vike_okx::{
        account_config, error_codes, event_mapper, funding, history, recon_client, ws_auth,
    };

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
