//! Totality harness for the Hyperliquid wire decoders: every function that turns raw SOCKET / REST
//! input (frame text, a `serde_json::Value`, a response body) into events, market data, catalog
//! rows or reconcile reports is fed (a) arbitrary text and lossy-decoded byte noise, (b) random JSON
//! whose object keys are drawn from the decoders' REAL field names (so the generator reaches the
//! branches instead of bouncing off the first `.get()`), (c) well-shaped WS frames and REST bodies
//! whose fields are mostly well-typed with hostile leaves (`NaN`, `inf`, `1e999`, `i64::MIN`, empty
//! strings, wrong types), and (d) short frame SEQUENCES into ONE stateful instance (the user-data
//! pump's registry / symbology / snapshot flag, the funding poller, an L2 book).
//!
//! The property is TOTALITY: a hostile or truncated frame may decode to nothing (or to `Err`), but
//! it must never panic the pump thread that reads the socket. Outputs are asserted only where a
//! cheap bound must always hold (no event flood).
//!
//! Covered (all through the PUBLIC API): `market_data::{candle_to_bar, bbo_to_quote,
//! trades_from_frame, l2book_to_snapshot}`, `market_feed::mark_from_frame`, `funding::{
//! parse_user_funding, parse_funding_history, HlFundingPoller}`, `symbology::{Symbology,
//! parse_perp_dexs}`, `catalog::instruments_from_symbology`, `user_role::parse_user_role`,
//! `history::identity_coin_for`; under the default-on `exec` feature also `event_mapper`,
//! `user_data::map_frame_to_events`, `recon_client::parse_*` and `outcome_settlement`. NOT reachable
//! from here: `instruments::properties_for` (private; covered by the sibling unit test
//! `crates/bridges/hyperliquid/src/feeds/instruments_props.rs`).
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::depth::infer_tick_size;
use vike_hyperliquid::base::symbology::{Symbology, parse_perp_dexs};
use vike_hyperliquid::feeds::{catalog, funding, history, market_data, user_role};
use vike_marketdata::{BookLevel, L2Book};

#[path = "decoder_never_panics/frames.rs"]
mod frames;
#[path = "decoder_never_panics/kit.rs"]
mod kit;

use frames::*;
use kit::*;

/// Strings that matter to a numeric / id / enum read on the wire (including non-ASCII, for the
/// non-char-boundary class of slicing bugs).
const STR_POOL: &[&str] = &[
    "",
    " ",
    "0",
    "1",
    "-1",
    "-0",
    "0.0",
    "1.5",
    "42",
    "NaN",
    "nan",
    "inf",
    "-inf",
    "Infinity",
    "1e999",
    "-1e999",
    "1e-999",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775809",
    "18446744073709551616",
    "99999999999999999999999999999999",
    "1700000000000",
    "50000.5",
    "true",
    "null",
    "\u{0}",
    "é",
    "日本",
];

/// Hyperliquid vocabulary: channels, sides, statuses, coins, roles, outcome tokens.
const WORDS: &[&str] = &[
    "candle",
    "bbo",
    "trades",
    "l2Book",
    "activeAssetCtx",
    "orderUpdates",
    "userFills",
    "subscriptionResponse",
    "pong",
    "A",
    "B",
    "open",
    "resting",
    "triggered",
    "filled",
    "canceled",
    "marginCanceled",
    "scheduledCancel",
    "rejected",
    "tickRejected",
    "waitingForTrigger",
    "waitingForFill",
    "funding",
    "BTC",
    "ETH",
    "@107",
    "PURR/USDC",
    "HYPE/USDC",
    "test:BTC",
    "USDC",
    "isolated",
    "cross",
    "user",
    "agent",
    "subAccount",
    "vault",
    "missing",
    "ok",
    "err",
    "+10",
    "+11",
    "#10",
    "1m",
];

const COINS: &[&str] = &["BTC", "ETH", "@107", "PURR/USDC", "test:BTC", ""];
const ENVELOPE_KEYS: &[&str] = &["channel", "data", "status", "response", "type", "method"];
/// Every real field name the decoders read, plus the envelope keys.
const ALL_KEYS: &[&str] = &[
    "channel",
    "data",
    "status",
    "response",
    "type",
    "method",
    "coin",
    "time",
    "bbo",
    "px",
    "sz",
    "n",
    "side",
    "hash",
    "tid",
    "oid",
    "cloid",
    "levels",
    "ctx",
    "markPx",
    "order",
    "statusTimestamp",
    "limitPx",
    "origSz",
    "timestamp",
    "isSnapshot",
    "fills",
    "crossed",
    "fee",
    "feeToken",
    "statuses",
    "resting",
    "filled",
    "totalSz",
    "avgPx",
    "error",
    "t",
    "T",
    "o",
    "h",
    "l",
    "c",
    "v",
    "assetPositions",
    "position",
    "szi",
    "entryPx",
    "leverage",
    "value",
    "marginSummary",
    "accountValue",
    "balances",
    "total",
    "hold",
    "delta",
    "usdc",
    "fundingRate",
    "role",
    "user",
    "master",
    "universe",
    "name",
    "szDecimals",
    "maxLeverage",
    "onlyIsolated",
    "tokens",
    "index",
    "outcomes",
    "questions",
    "outcome",
    "question",
    "sideSpecs",
    "settleFraction",
    "spec",
    "origSz",
    "fullName",
    "deployer",
];

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
    fn public_decoders_are_total_over_arbitrary_json(v in arb_json(ALL_KEYS)) {
        drive_public_value(&v);
        drive_public_text(&v.to_string());
    }

    /// (b') well-shaped market-data frames of every lane with hostile leaves, whole and cut off
    /// mid-flight.
    #[test]
    fn public_decoders_are_total_over_lane_frames(v in market_frame(), cut in 0usize..400) {
        drive_public_value(&v);
        let text = v.to_string();
        drive_public_text(&text);
        drive_public_text(&truncated(&text, cut));
    }

    /// (b'') the REST bodies: funding payments / history and the account role.
    #[test]
    fn rest_bodies_are_total(
        rows in array_of(funding_row(), 6),
        history_rows in array_of(funding_history_row(), 6),
        role in user_role_body(),
        symbol in any::<String>(),
    ) {
        let n = rows.as_array().map_or(0, Vec::len);
        let parsed = funding::parse_user_funding(&rows.to_string());
        prop_assert!(matches!(&parsed, Ok(r) if r.len() <= n), "a payment per row at most");
        let _ = funding::parse_funding_history(&history_rows.to_string());
        let _ = user_role::parse_user_role(&role);
        let _ = history::identity_coin_for(&symbol);
    }

    /// The symbology builders (`meta`, `spotMeta`, HIP-3 per-dex `meta`) and the catalog mapping
    /// over them: one catalog row per resolved instrument, every lookup total.
    #[test]
    fn symbology_builders_are_total(
        meta in meta_body(),
        spot in spot_meta_body(),
        dexs in perp_dexs_body(),
        dex_meta in meta_body(),
        probe in any::<String>(),
    ) {
        let mut sym = Symbology::from_meta(&meta, &spot);
        for dex in parse_perp_dexs(&dexs) {
            sym.extend_with_perp_dex(&dex_meta, dex.index, &dex.name);
        }
        let rows = catalog::instruments_from_symbology(&sym);
        prop_assert_eq!(rows.len(), sym.len());
        for inst in sym.iter() {
            let _ = inst.effective_margin_mode();
            prop_assert!(sym.by_symbol(&inst.symbol).is_some());
        }
        let _ = sym.by_coin(&probe);
        let _ = sym.symbol_for_coin(&probe);
        let _ = sym.coin_for(&probe);
        let _ = sym.asset_id_for(&probe);
    }

    /// ⚠ `Symbology::load_spot` with HOSTILE pair indices. Suspected overflow panic:
    /// `asset_id: SPOT_ASSET_OFFSET + index as u32` where `index` is the wire `u64` truncated to
    /// `u32` — an `index` of `i64::MAX` / `u32::MAX` makes `10_000 + index as u32` overflow (a
    /// debug-build panic, a silent wrap in release). See the report.
    #[test]
    fn spot_meta_with_hostile_pair_indices_is_total(
        index in prop_oneof![
            Just(u64::from(u32::MAX)),
            Just(u64::from(u32::MAX) - 9_999),
            Just(i64::MAX as u64),
            Just(u64::MAX),
            any::<u64>(),
        ],
    ) {
        let spot = serde_json::json!({
            "tokens": [
                {"index": 0, "name": "USDC", "szDecimals": 8},
                {"index": 1, "name": "HYPE", "szDecimals": 2},
            ],
            "universe": [{"name": "@1", "index": index, "tokens": [1, 0]}],
        });
        let sym = Symbology::from_meta(&Value::Null, &spot);
        let _ = catalog::instruments_from_symbology(&sym);
    }

    /// (c) a short sequence of `l2Book` frames into ONE book, re-anchored on each snapshot's
    /// inferred tick grid (the depth lane's decode, restated over its public parts).
    #[test]
    fn l2_book_survives_snapshot_sequences(
        frames in prop::collection::vec(frame_text(l2_frame()), 1..8),
    ) {
        let mut book = L2Book::new(0.5);
        for f in &frames {
            let Ok(v) = serde_json::from_str::<Value>(f) else { continue };
            if let Some(snap) = market_data::l2book_to_snapshot(&v) {
                book = L2Book::new(infer_tick_size(&snap.bids, &snap.asks));
                book.apply_snapshot(snap.time.max(0) as u64, &snap.bids, &snap.asks);
            }
            prop_assert!(book.tick_size > 0.0);
        }
    }

    /// `infer_tick_size` (shared, `vike_bridge_core::depth`) on a WIDE book salted with non-finite
    /// prices: it sorts the prices with `partial_cmp(..).unwrap_or(Equal)`, a comparator that is not
    /// a total order once a `NaN` is in, which `slice::sort_by` is allowed to panic on.
    #[test]
    fn tick_inference_survives_non_finite_prices(
        levels in prop::collection::vec(
            (
                prop_oneof![
                    8 => -1.0e6f64..1.0e6,
                    1 => Just(f64::NAN),
                    1 => Just(f64::INFINITY),
                    1 => Just(f64::NEG_INFINITY),
                ],
                any::<f64>(),
            ),
            0..80,
        ),
        split in any::<prop::sample::Index>(),
    ) {
        let levels: Vec<BookLevel> =
            levels.into_iter().map(|(price, qty)| BookLevel { price, qty }).collect();
        let at = split.index(levels.len() + 1);
        let tick = infer_tick_size(&levels[..at], &levels[at..]);
        prop_assert!(tick > 0.0, "inferred tick {tick} is not a usable grid");
    }

    /// (c') the funding poller (hash seen-set, watermark, spawn-time floor) over a short sequence
    /// of `userFunding` bodies, each decoded by the real parser.
    #[test]
    fn funding_poller_survives_body_sequences(
        bodies in prop::collection::vec(array_of(funding_row(), 5), 1..6),
        floor in 0i64..4_000_000_000_000,
    ) {
        let mut queue = bodies.into_iter();
        let fetch = move |_start: i64, _end: i64| match queue.next() {
            Some(body) => funding::parse_user_funding(&body.to_string()),
            None => Err("no more bodies".to_string()),
        };
        let mut poller = funding::HlFundingPoller::new(fetch, "hyperliquid", floor);
        let mut now = floor;
        for _ in 0..6 {
            now += 60_000;
            let _ = poller.poll(now);
        }
    }
}

// --- the exec plane (default-on `exec` feature) ---------------------------------------------

#[cfg(feature = "exec")]
#[path = "decoder_never_panics/exec_plane.rs"]
mod exec_plane;
