//! Hyperliquid-shaped WS frames and REST bodies, and the drivers that feed every keyless decoder.

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::depth::infer_tick_size;
use vike_hyperliquid::base::symbology::{Symbology, parse_perp_dexs};
use vike_hyperliquid::feeds::{catalog, funding, history, market_data, market_feed, user_role};
use vike_marketdata::L2Book;

use super::kit::{arb_json, arb_leaf, array_of, jint, junk_or, numv, obj_of, pick, small_id};
use super::{ALL_KEYS, COINS};

// --- Hyperliquid-shaped frames -------------------------------------------------------------

/// `{"channel": <chan>, "data": <data>}`, each part occasionally missing or the wrong type.
pub(super) fn chan_frame(chan: &'static str, data: BoxedStrategy<Value>) -> BoxedStrategy<Value> {
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

pub(super) fn l2_frame() -> BoxedStrategy<Value> {
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
pub(super) fn market_frame() -> BoxedStrategy<Value> {
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

pub(super) fn frame_text(frame: BoxedStrategy<Value>) -> BoxedStrategy<String> {
    prop_oneof![9 => frame.prop_map(|v| v.to_string()), 1 => any::<String>()].boxed()
}

/// A `meta` body: `{universe: [{name, szDecimals, maxLeverage, onlyIsolated}]}`.
pub(super) fn meta_body() -> BoxedStrategy<Value> {
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
pub(super) fn spot_meta_body() -> BoxedStrategy<Value> {
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
pub(super) fn perp_dexs_body() -> BoxedStrategy<Value> {
    let dex = obj_of(vec![
        ("name", pick(&["test", "xyz", "", "dex"])),
        ("fullName", arb_leaf()),
        ("deployer", arb_leaf()),
    ]);
    let entry = prop_oneof![8 => dex, 2 => Just(Value::Null).boxed()];
    prop::collection::vec(entry, 0..5).prop_map(Value::Array).boxed()
}

pub(super) fn user_role_body() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("role", pick(&["user", "agent", "subAccount", "vault", "missing", "other", ""])),
        (
            "data",
            junk_or(obj_of(vec![("user", pick(&["0xabc", ""])), ("master", pick(&["0xdef", ""]))])),
        ),
    ])
}

/// One `userFunding` row.
pub(super) fn funding_row() -> BoxedStrategy<Value> {
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
pub(super) fn funding_history_row() -> BoxedStrategy<Value> {
    obj_of(vec![
        ("coin", pick(COINS)),
        ("fundingRate", numv()),
        ("premium", numv()),
        ("time", jint()),
    ])
}

// --- the drivers ---------------------------------------------------------------------------

/// Every keyless `Value` decoder on one parsed payload.
pub(super) fn drive_public_value(v: &Value) {
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
pub(super) fn drive_public_text(text: &str) {
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
