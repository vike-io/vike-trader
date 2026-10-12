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
//! `crates/bridges/hyperliquid/src/instruments_props.rs`).
//!
//! A minimized counterexample is a REAL bug: commit the `decoder_never_panics.proptest-regressions`
//! seed beside this file and fix the decoder.

use proptest::prelude::*;
use serde_json::{Map, Value};
use vike_bridge_core::depth::infer_tick_size;
use vike_hyperliquid::symbology::{Symbology, parse_perp_dexs};
use vike_hyperliquid::{catalog, funding, history, market_data, market_feed, user_role};
use vike_model::{BookLevel, L2Book};

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

// --- the generator kit ---------------------------------------------------------------------

fn arb_str() -> BoxedStrategy<String> {
    prop_oneof![
        4 => prop::sample::select(STR_POOL).prop_map(String::from),
        4 => prop::sample::select(WORDS).prop_map(String::from),
        1 => any::<i64>().prop_map(|n| n.to_string()),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        2 => "[ -~]{0,12}",
        1 => any::<String>(),
    ]
    .boxed()
}

/// A JSON leaf: null, bool, i64 extremes / zero / negative, u64, f64, hostile strings, and the
/// empty array / object.
fn arb_leaf() -> BoxedStrategy<Value> {
    prop_oneof![
        1 => Just(Value::Null),
        1 => any::<bool>().prop_map(Value::Bool),
        2 => prop::sample::select(vec![0i64, 1, -1, i64::MAX, i64::MIN]).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        5 => arb_str().prop_map(Value::String),
        1 => Just(Value::Array(Vec::new())),
        1 => Just(Value::Object(Map::new())),
    ]
    .boxed()
}

fn arb_key(keys: &'static [&'static str]) -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(keys).prop_map(String::from),
        1 => "[a-zA-Z_]{1,8}",
    ]
    .boxed()
}

/// Random JSON, depth <= 4, object keys drawn mostly from `keys`.
fn arb_json(keys: &'static [&'static str]) -> BoxedStrategy<Value> {
    arb_leaf()
        .prop_recursive(4, 64, 6, move |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((arb_key(keys), inner), 0..6)
                    .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
            ]
        })
        .boxed()
}

/// Mostly the well-typed `good` value, sometimes any leaf / random JSON (the wrong-type lane).
fn junk_or(good: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    prop_oneof![9 => good, 1 => arb_json(ENVELOPE_KEYS)].boxed()
}

/// One of `set` as a JSON string (80%), else any leaf.
fn pick(set: &'static [&'static str]) -> BoxedStrategy<Value> {
    prop_oneof![
        8 => prop::sample::select(set).prop_map(|s| Value::String(s.to_string())),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// Decimal text a numeric read would parse (mostly), else hostile text.
fn num_text() -> BoxedStrategy<String> {
    prop_oneof![
        4 => (-1_000_000i64..1_000_000).prop_map(|n| n.to_string()),
        4 => (-1.0e6f64..1.0e6).prop_map(|f| format!("{f:.6}")),
        1 => any::<f64>().prop_map(|f| f.to_string()),
        1 => arb_str(),
    ]
    .boxed()
}

fn json_f64(f: f64) -> Value {
    serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
}

/// A numeric wire field: Hyperliquid sends decimal STRINGS (so usually a string), sometimes a JSON
/// number or any leaf.
fn numv() -> BoxedStrategy<Value> {
    prop_oneof![
        7 => num_text().prop_map(Value::String),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<f64>().prop_map(json_f64),
        2 => arb_leaf(),
    ]
    .boxed()
}

/// An integer wire field (epoch-ms stamps, `oid` / `tid` ids): usually a JSON number.
fn jint() -> BoxedStrategy<Value> {
    prop_oneof![
        6 => (0i64..4_102_444_800_000).prop_map(|n| Value::Number(n.into())),
        1 => any::<i64>().prop_map(|n| Value::Number(n.into())),
        1 => any::<u64>().prop_map(|n| Value::Number(n.into())),
        1 => num_text().prop_map(Value::String),
        1 => arb_leaf(),
    ]
    .boxed()
}

/// A small id (`oid`, `tid`): small numbers so a registry / retire set can match, else anything.
fn small_id() -> BoxedStrategy<Value> {
    prop_oneof![6 => (0u64..4).prop_map(|n| Value::Number(n.into())), 2 => jint()].boxed()
}

/// An object over exactly the named fields; each is present with p = 0.85 and drawn from its own
/// (mostly well-typed) strategy.
fn obj_of(fields: Vec<(&'static str, BoxedStrategy<Value>)>) -> BoxedStrategy<Value> {
    fields
        .into_iter()
        .map(|(k, s)| prop::option::weighted(0.85, s).prop_map(move |v| (k, v)))
        .collect::<Vec<_>>()
        .prop_map(|kvs| {
            let mut m = Map::new();
            for (k, v) in kvs {
                if let Some(v) = v {
                    m.insert(k.to_string(), v);
                }
            }
            Value::Object(m)
        })
        .boxed()
}

fn array_of(row: BoxedStrategy<Value>, max: usize) -> BoxedStrategy<Value> {
    prop::collection::vec(row, 0..max).prop_map(Value::Array).boxed()
}

/// The first `n` chars of `text` (a frame cut off mid-flight, never mid-char).
fn truncated(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

// --- Hyperliquid-shaped frames -------------------------------------------------------------

/// `{"channel": <chan>, "data": <data>}`, each part occasionally missing or the wrong type.
fn chan_frame(chan: &'static str, data: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    obj_of(vec![
        (
            "channel",
            prop_oneof![9 => Just(Value::String(chan.to_string())), 1 => arb_leaf()].boxed(),
        ),
        ("data", junk_or(data)),
    ])
}

/// One `{px, sz, n}` book level.
fn level() -> BoxedStrategy<Value> {
    obj_of(vec![("px", numv()), ("sz", numv()), ("n", jint())])
}

fn candle_frame() -> BoxedStrategy<Value> {
    chan_frame(
        "candle",
        obj_of(vec![
            ("t", jint()),
            ("T", jint()),
            ("s", pick(COINS)),
            ("i", pick(&["1m", "1h", ""])),
            ("o", numv()),
            ("h", numv()),
            ("l", numv()),
            ("c", numv()),
            ("v", numv()),
            ("n", jint()),
        ]),
    )
}

fn bbo_frame() -> BoxedStrategy<Value> {
    let side = prop_oneof![8 => level(), 2 => arb_leaf()];
    let bbo = prop::collection::vec(side, 0..4).prop_map(Value::Array).boxed();
    chan_frame("bbo", obj_of(vec![("coin", pick(COINS)), ("time", jint()), ("bbo", junk_or(bbo))]))
}

fn trades_frame() -> BoxedStrategy<Value> {
    let row = obj_of(vec![
        ("coin", pick(COINS)),
        ("side", pick(&["A", "B", ""])),
        ("px", numv()),
        ("sz", numv()),
        ("time", jint()),
        ("hash", arb_leaf()),
        ("tid", small_id()),
    ]);
    chan_frame("trades", array_of(row, 5))
}

fn l2_frame() -> BoxedStrategy<Value> {
    let side = junk_or(array_of(level(), 8));
    let levels =
        (side.clone(), side).prop_map(|(bids, asks)| Value::Array(vec![bids, asks])).boxed();
    chan_frame(
        "l2Book",
        obj_of(vec![("coin", pick(COINS)), ("time", jint()), ("levels", junk_or(levels))]),
    )
}

fn mark_frame() -> BoxedStrategy<Value> {
    let ctx = obj_of(vec![("markPx", numv())]);
    chan_frame("activeAssetCtx", obj_of(vec![("coin", pick(COINS)), ("ctx", junk_or(ctx))]))
}

/// Any keyless public market-data frame, one lane at a time (plus random JSON).
fn market_frame() -> BoxedStrategy<Value> {
    prop_oneof![
        candle_frame(),
        bbo_frame(),
        trades_frame(),
        l2_frame(),
        mark_frame(),
        arb_json(ALL_KEYS),
    ]
    .boxed()
}

fn frame_text(frame: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![9 => frame.prop_map(|v| v.to_string()), 1 => any::<String>()].boxed()
}

/// A `meta` body: `{universe: [{name, szDecimals, maxLeverage, onlyIsolated}]}`.
fn meta_body() -> BoxedStrategy<Value> {
    let row = obj_of(vec![
        ("name", pick(COINS)),
        (
            "szDecimals",
            prop_oneof![(0u64..10).prop_map(|n| Value::Number(n.into())), arb_leaf()].boxed(),
        ),
        ("maxLeverage", jint()),
        ("onlyIsolated", prop_oneof![any::<bool>().prop_map(Value::Bool), arb_leaf()].boxed()),
    ]);
    obj_of(vec![("universe", junk_or(array_of(row, 6)))])
}

/// A `spotMeta` body whose pair indices are SMALL — the hostile-index class has its own test.
fn spot_meta_body() -> BoxedStrategy<Value> {
    spot_meta_with(
        prop_oneof![8 => (0u64..8).prop_map(|n| Value::Number(n.into())), 2 => arb_small()].boxed(),
    )
}

/// A leaf that cannot carry a huge index (the hostile-index test supplies those).
fn arb_small() -> BoxedStrategy<Value> {
    prop_oneof![Just(Value::Null), Just(Value::String("7".to_string())), Just(Value::Bool(true))]
        .boxed()
}

fn spot_meta_with(pair_index: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
    let token = obj_of(vec![
        (
            "index",
            prop_oneof![(0u64..4).prop_map(|n| Value::Number(n.into())), arb_small()].boxed(),
        ),
        ("name", pick(&["USDC", "HYPE", "PURR", ""])),
        (
            "szDecimals",
            prop_oneof![(0u64..10).prop_map(|n| Value::Number(n.into())), arb_small()].boxed(),
        ),
    ]);
    let pair = obj_of(vec![
        ("name", pick(&["@107", "PURR/USDC", "@1", ""])),
        ("index", pair_index),
        (
            "tokens",
            prop_oneof![
                8 => (0u64..4, 0u64..4)
                    .prop_map(|(a, b)| Value::Array(vec![a.into(), b.into()]))
                    .boxed(),
                2 => array_of(arb_leaf(), 4),
            ]
            .boxed(),
        ),
    ]);
    obj_of(vec![("tokens", junk_or(array_of(token, 5))), ("universe", junk_or(array_of(pair, 5)))])
}

/// A `perpDexs` body: `[null, {name, fullName, deployer}, ...]`.
fn perp_dexs_body() -> BoxedStrategy<Value> {
    let dex = obj_of(vec![
        ("name", pick(&["test", "xyz", "", "dex"])),
        ("fullName", arb_leaf()),
        ("deployer", arb_leaf()),
    ]);
    let entry = prop_oneof![8 => dex, 2 => Just(Value::Null).boxed()];
    prop::collection::vec(entry, 0..5).prop_map(Value::Array).boxed()
}

fn user_role_body() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("role", pick(&["user", "agent", "subAccount", "vault", "missing", "other", ""])),
        (
            "data",
            junk_or(obj_of(vec![("user", pick(&["0xabc", ""])), ("master", pick(&["0xdef", ""]))])),
        ),
    ])
}

/// One `userFunding` row.
fn funding_row() -> BoxedStrategy<Value> {
    let delta = obj_of(vec![
        ("type", pick(&["funding", "deposit", ""])),
        ("coin", pick(COINS)),
        ("usdc", numv()),
        ("szi", numv()),
        ("fundingRate", numv()),
    ]);
    obj_of(vec![
        ("time", jint()),
        ("hash", prop_oneof!["[a-f0-9]{1,6}".prop_map(Value::String), arb_leaf()].boxed()),
        ("delta", junk_or(delta)),
    ])
}

/// `fundingHistory` rows.
fn funding_history_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("coin", pick(COINS)),
        ("fundingRate", numv()),
        ("premium", numv()),
        ("time", jint()),
    ])
}

// --- the drivers ---------------------------------------------------------------------------

/// Every keyless `Value` decoder on one parsed payload.
fn drive_public_value(v: &Value) {
    let _ = market_data::candle_to_bar(v);
    let _ = market_data::candle_data_to_bar(v);
    let _ = market_data::bbo_to_quote(v);
    let _ = market_data::trades_from_frame(v);
    let _ = market_data::l2book_to_snapshot(v);
    let _ = market_feed::mark_from_frame(v);
    let _ = user_role::parse_user_role(v);
    let _ = parse_perp_dexs(v);
    let sym = Symbology::from_meta(v, v);
    let _ = catalog::instruments_from_symbology(&sym);
}

/// Every keyless text decoder on one frame.
fn drive_public_text(text: &str) {
    let _ = funding::parse_user_funding(text);
    let _ = funding::parse_funding_history(text);
    let _ = history::identity_coin_for(text);
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        drive_public_value(&v);
        // the depth lane: snapshot -> inferred tick -> book
        if let Some(snap) = market_data::l2book_to_snapshot(&v) {
            let mut book = L2Book::new(infer_tick_size(&snap.bids, &snap.asks));
            book.apply_snapshot(snap.time.max(0) as u64, &snap.bids, &snap.asks);
        }
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
mod exec_plane {
    use std::sync::atomic::AtomicBool;

    use super::*;
    use proptest::test_runner::TestCaseError;
    use vike_hyperliquid::config::Product;
    use vike_hyperliquid::event_mapper::{
        self, SubmittedOrder, cloid_from_client_order_id, map_order_status,
    };
    use vike_hyperliquid::exec::CloidRegistry;
    use vike_hyperliquid::outcome_settlement;
    use vike_hyperliquid::{recon_client, user_data};

    /// The coids the fixture registry knows, so a frame's `cloid` can resolve.
    const KNOWN_COIDS: [&str; 3] = ["c1", "c2", "c3"];

    fn cloid() -> BoxedStrategy<Value> {
        prop_oneof![
            6 => prop::sample::select(KNOWN_COIDS.iter().map(|c| cloid_from_client_order_id(c)).collect::<Vec<_>>())
                .prop_map(Value::String),
            1 => Just(Value::String(String::new())),
            1 => arb_leaf(),
        ]
        .boxed()
    }

    /// The fixture a pump thread would hold: a symbology, a registry that knows [`KNOWN_COIDS`] and
    /// has retired a few oids, and the first-snapshot flag.
    fn fixture() -> (Symbology, CloidRegistry, AtomicBool) {
        let meta = serde_json::json!({"universe": [
            {"name": "BTC", "szDecimals": 5, "maxLeverage": 50},
            {"name": "ETH", "szDecimals": 4, "maxLeverage": 25},
        ]});
        let spot = serde_json::json!({
            "tokens": [
                {"index": 0, "name": "USDC", "szDecimals": 8},
                {"index": 150, "name": "HYPE", "szDecimals": 2},
            ],
            "universe": [{"name": "@107", "index": 107, "tokens": [150, 0]}],
        });
        let registry = CloidRegistry::new();
        for c in KNOWN_COIDS {
            let _ = registry.register(c);
        }
        registry.retire_oid(1);
        registry.retire_oid(2);
        (Symbology::from_meta(&meta, &spot), registry, AtomicBool::new(false))
    }

    fn order_row() -> BoxedStrategy<Value> {
        let order = obj_of(vec![
            ("coin", pick(COINS)),
            ("side", pick(&["A", "B", ""])),
            ("limitPx", numv()),
            ("sz", numv()),
            ("origSz", numv()),
            ("oid", small_id()),
            ("cloid", cloid()),
            ("timestamp", jint()),
        ]);
        obj_of(vec![
            ("order", junk_or(order)),
            (
                "status",
                pick(&[
                    "open",
                    "resting",
                    "triggered",
                    "filled",
                    "canceled",
                    "marginCanceled",
                    "scheduledCancel",
                    "rejected",
                    "tickRejected",
                    "somethingNew",
                    "",
                ]),
            ),
            ("statusTimestamp", jint()),
        ])
    }

    fn fill_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("coin", pick(COINS)),
            ("px", numv()),
            ("sz", numv()),
            ("side", pick(&["A", "B", ""])),
            ("time", jint()),
            ("oid", small_id()),
            ("tid", small_id()),
            ("cloid", cloid()),
            ("crossed", prop_oneof![any::<bool>().prop_map(Value::Bool), arb_leaf()].boxed()),
            ("fee", numv()),
            ("feeToken", pick(&["USDC", ""])),
        ])
    }

    fn order_updates_frame() -> BoxedStrategy<Value> {
        chan_frame("orderUpdates", array_of(order_row(), 5))
    }

    fn user_fills_frame() -> BoxedStrategy<Value> {
        let body = obj_of(vec![
            ("isSnapshot", prop_oneof![any::<bool>().prop_map(Value::Bool), arb_leaf()].boxed()),
            ("fills", junk_or(array_of(fill_row(), 5))),
        ]);
        chan_frame("userFills", body)
    }

    /// A private-WS frame of either lane (plus pongs and random JSON).
    fn private_frame() -> BoxedStrategy<Value> {
        prop_oneof![
            order_updates_frame(),
            user_fills_frame(),
            chan_frame("pong", arb_json(ALL_KEYS)),
            arb_json(ALL_KEYS),
        ]
        .boxed()
    }

    /// One `/exchange` `statuses[i]`: a string status or a `{resting|filled|error}` object.
    fn exchange_status() -> BoxedStrategy<Value> {
        prop_oneof![
            pick(&["waitingForTrigger", "waitingForFill", "weird", ""]),
            obj_of(vec![("resting", junk_or(obj_of(vec![("oid", small_id())])))]),
            obj_of(vec![(
                "filled",
                junk_or(obj_of(vec![("totalSz", numv()), ("avgPx", numv()), ("oid", small_id()),])),
            )]),
            obj_of(vec![("error", arb_leaf())]),
            arb_json(ALL_KEYS),
        ]
        .boxed()
    }

    fn exchange_response() -> BoxedStrategy<Value> {
        let data = obj_of(vec![("statuses", junk_or(array_of(exchange_status(), 5)))]);
        let response = obj_of(vec![("type", pick(&["order"])), ("data", junk_or(data))]);
        obj_of(vec![
            ("status", pick(&["ok", "err", ""])),
            (
                "response",
                junk_or(prop_oneof![response, arb_str().prop_map(Value::String).boxed()].boxed()),
            ),
        ])
    }

    fn submitted_orders() -> BoxedStrategy<Vec<SubmittedOrder>> {
        prop::collection::vec(
            (
                arb_str(),
                prop::sample::select(COINS),
                prop_oneof![Just(1), Just(-1), Just(0)],
                any::<f64>(),
                any::<i64>(),
            )
                .prop_map(|(client_order_id, coin, side, req_sz, ts)| SubmittedOrder {
                    client_order_id,
                    coin: coin.to_string(),
                    side,
                    req_sz,
                    ts,
                }),
            0..5,
        )
        .boxed()
    }

    /// `frontendOpenOrders` / `userFills` REST rows (the reconcile client's inputs).
    fn open_order_row() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("coin", pick(COINS)),
            ("oid", small_id()),
            ("cloid", cloid()),
            ("side", pick(&["A", "B", ""])),
            ("orderType", pick(&["Limit", "Stop Market", ""])),
            ("origSz", numv()),
            ("sz", numv()),
            ("status", pick(&["open", "canceled", "marginCanceled", "weird", ""])),
            ("timestamp", jint()),
        ])
    }

    fn position_row() -> BoxedStrategy<Value> {
        let leverage = obj_of(vec![("type", pick(&["cross", "isolated", ""])), ("value", jint())]);
        let position = obj_of(vec![
            ("coin", pick(COINS)),
            ("szi", numv()),
            ("entryPx", numv()),
            ("leverage", junk_or(leverage)),
        ]);
        obj_of(vec![("position", junk_or(position))])
    }

    fn positions_body() -> BoxedStrategy<Value> {
        obj_of(vec![("assetPositions", junk_or(array_of(position_row(), 5))), ("time", jint())])
    }

    fn balances_body() -> BoxedStrategy<Value> {
        let row = obj_of(vec![
            ("coin", pick(&["USDC", "HYPE", "+10", ""])),
            ("total", numv()),
            ("hold", numv()),
        ]);
        obj_of(vec![
            ("balances", junk_or(array_of(row, 5))),
            ("marginSummary", junk_or(obj_of(vec![("accountValue", numv())]))),
        ])
    }

    /// An outcome row: `{outcome, name, description, sideSpecs, quoteToken}`.
    fn outcome_row() -> BoxedStrategy<Value> {
        let side = obj_of(vec![("name", pick(&["Yes", "No", ""]))]);
        obj_of(vec![
            (
                "outcome",
                prop_oneof![(0u64..6).prop_map(|n| Value::Number(n.into())), jint()].boxed(),
            ),
            ("name", arb_leaf()),
            ("description", arb_leaf()),
            ("sideSpecs", junk_or(array_of(side, 4))),
            ("quoteToken", pick(&["USDH", ""])),
        ])
    }

    fn outcome_meta_body() -> BoxedStrategy<Value> {
        let question = obj_of(vec![
            ("question", jint()),
            ("name", arb_leaf()),
            ("description", arb_leaf()),
            ("fallbackOutcome", jint()),
            ("namedOutcomes", junk_or(array_of(jint(), 4))),
            ("settledNamedOutcomes", junk_or(array_of(jint(), 4))),
        ]);
        obj_of(vec![
            ("outcomes", junk_or(array_of(outcome_row(), 4))),
            ("questions", junk_or(array_of(question, 3))),
        ])
    }

    fn settled_body() -> BoxedStrategy<Value> {
        obj_of(vec![
            ("spec", junk_or(outcome_row())),
            ("settleFraction", numv()),
            ("details", arb_leaf()),
        ])
    }

    fn spot_balances_body() -> BoxedStrategy<Value> {
        let row = obj_of(vec![
            (
                "coin",
                prop_oneof![
                    (0u32..64).prop_map(|n| Value::String(format!("+{n}"))),
                    pick(&["USDC", "+", "+x", "+99999999999999999999"]),
                ]
                .boxed(),
            ),
            ("total", numv()),
            ("hold", numv()),
        ]);
        obj_of(vec![("balances", junk_or(array_of(row, 6)))])
    }

    /// One frame through the REAL pump decode against the shared fixture; no event flood (a row
    /// yields <= 1 event).
    fn check_frame(
        frame: &Value,
        fx: &(Symbology, CloidRegistry, AtomicBool),
        spawn_ms: i64,
    ) -> Result<(), TestCaseError> {
        let data = frame.get("data");
        let n_rows = data.and_then(Value::as_array).map_or(0, Vec::len)
            + data.and_then(|d| d.get("fills")).and_then(Value::as_array).map_or(0, Vec::len);
        let evs = user_data::map_frame_to_events(frame, &fx.0, &fx.1, &fx.2, spawn_ms);
        prop_assert!(evs.len() <= n_rows, "flood: {} events from {n_rows} rows", evs.len());
        let _ = event_mapper::map_order_updates(frame, "hyperliquid");
        let _ = event_mapper::map_user_fills(frame, "hyperliquid");
        let _ = event_mapper::map_order_update(frame, "hyperliquid");
        Ok(())
    }

    fn drive_recon_text(body: &str) {
        let _ = recon_client::parse_orders(body);
        let _ = recon_client::parse_fills(body);
        let _ = recon_client::parse_positions(body);
        let _ = recon_client::parse_balance(body, Product::Perp);
        let _ = recon_client::parse_balance(body, Product::Spot);
        let _ = outcome_settlement::parse_outcome_meta(body);
        let _ = outcome_settlement::parse_settled_outcome(body);
        let _ = outcome_settlement::parse_spot_balances(body);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// (a) arbitrary text / byte noise through every reconcile + outcome-settlement parser.
        #[test]
        fn recon_parsers_are_total_over_text(text in any::<String>()) {
            drive_recon_text(&text);
        }

        #[test]
        fn recon_parsers_survive_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
            drive_recon_text(&String::from_utf8_lossy(&bytes));
        }

        /// (b) the reconcile bodies: arrays of order / fill rows (whole and cut off), position and
        /// balance objects; a valid array must parse to exactly one order report per row.
        #[test]
        fn recon_parsers_are_total_over_bodies(
            orders in array_of(open_order_row(), 6),
            fills in array_of(fill_row(), 6),
            positions in positions_body(),
            balances in balances_body(),
            cut in 0usize..600,
        ) {
            let n = orders.as_array().map_or(0, Vec::len);
            let body = orders.to_string();
            drive_recon_text(&body);
            drive_recon_text(&truncated(&body, cut));
            let parsed = recon_client::parse_orders(&body);
            prop_assert!(matches!(&parsed, Ok(r) if r.len() == n), "one report per row");
            let n_fills = fills.as_array().map_or(0, Vec::len);
            let parsed = recon_client::parse_fills(&fills.to_string());
            prop_assert!(matches!(&parsed, Ok(r) if r.len() <= n_fills), "a fill report per row at most");
            let _ = recon_client::parse_positions(&positions.to_string());
            let _ = recon_client::parse_balance(&balances.to_string(), Product::Perp);
            let _ = recon_client::parse_balance(&balances.to_string(), Product::Spot);
        }

        /// (b') arbitrary JSON over the real field names through every exec-plane decoder.
        #[test]
        fn exec_decoders_are_total_over_arbitrary_json(
            v in arb_json(ALL_KEYS),
            spawn_ms in any::<i64>(),
        ) {
            drive_recon_text(&v.to_string());
            let fx = fixture();
            check_frame(&v, &fx, spawn_ms)?;
            let order = SubmittedOrder {
                client_order_id: "c1".to_string(),
                coin: "BTC".to_string(),
                side: 1,
                req_sz: 1.0,
                ts: 0,
            };
            let _ = map_order_status(&v, "hyperliquid", &order);
            let _ = event_mapper::map_order_response(&v, "hyperliquid", std::slice::from_ref(&order));
        }

        /// (b'') the private-WS pump decode over well-shaped `orderUpdates` / `userFills` frames,
        /// whole and cut off.
        #[test]
        fn pump_decode_is_total_over_private_frames(
            frame in private_frame(),
            spawn_ms in prop_oneof![Just(0i64), 0i64..4_102_444_800_000, any::<i64>()],
        ) {
            let fx = fixture();
            check_frame(&frame, &fx, spawn_ms)?;
        }

        /// (c) a short sequence of private frames into ONE registry / symbology / snapshot flag:
        /// the first-snapshot floor, the replay branch and the retired-oid cancel suppression all
        /// see state left behind by the previous frame.
        #[test]
        fn pump_decode_survives_frame_sequences(
            frames in prop::collection::vec(private_frame(), 1..8),
            spawn_ms in prop_oneof![Just(0i64), 0i64..4_102_444_800_000],
        ) {
            let fx = fixture();
            for f in &frames {
                check_frame(f, &fx, spawn_ms)?;
            }
        }

        /// The `/exchange` order response, positionally zipped with what was submitted: never more
        /// events than orders, whatever the envelope.
        #[test]
        fn order_response_is_total(
            resp in exchange_response(),
            orders in submitted_orders(),
            status in exchange_status(),
        ) {
            let evs = event_mapper::map_order_response(&resp, "hyperliquid", &orders);
            prop_assert!(evs.len() <= orders.len(), "{} events for {} orders", evs.len(), orders.len());
            if let Some(o) = orders.first() {
                let _ = map_order_status(&status, "hyperliquid", o);
            }
        }

        #[test]
        fn cloid_derivation_is_total(coid in any::<String>()) {
            let cloid = cloid_from_client_order_id(&coid);
            prop_assert_eq!(cloid.len(), 34, "0x + 32 hex chars");
        }

        /// HIP-4 outcome settlement: the parsers, then the pure derivation over whatever they
        /// produced, then the closing fill for an arbitrary local position.
        #[test]
        fn outcome_settlement_is_total(
            meta in outcome_meta_body(),
            settled in prop::collection::vec(settled_body(), 0..4),
            balances in spot_balances_body(),
            wallet in any::<String>(),
            signed_qty in any::<f64>(),
            ts in any::<i64>(),
            token in any::<String>(),
            fraction in any::<f64>(),
        ) {
            let meta = outcome_settlement::parse_outcome_meta(&meta.to_string()).expect("valid JSON");
            let settled: Vec<outcome_settlement::SettledOutcome> = settled
                .iter()
                .filter_map(|s| outcome_settlement::parse_settled_outcome(&s.to_string()).expect("valid JSON"))
                .collect();
            let balances = outcome_settlement::parse_spot_balances(&balances.to_string()).expect("valid JSON");
            let _ = outcome_settlement::settlement_candidates(&meta, &balances);
            let _ = meta.settled_by_question();
            let fills = outcome_settlement::derive_outcome_settlements(&settled, &balances, &wallet);
            prop_assert!(fills.len() <= balances.len(), "a settlement per held side token at most");
            for fill in &fills {
                let _ = outcome_settlement::settlement_fill_event(fill, signed_qty, ts);
                let _ = outcome_settlement::settlement_key(fill.outcome, fill.side, &wallet);
            }
            let _ = outcome_settlement::decode_token_name(&token);
            let _ = outcome_settlement::payout_for_side(0, fraction);
            let _ = outcome_settlement::payout_for_side(1, fraction);
            let _ = outcome_settlement::payout_for_side(7, fraction);
        }
    }
}
