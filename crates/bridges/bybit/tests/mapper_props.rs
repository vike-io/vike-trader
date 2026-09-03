//! Property harness for the bybit parser surface (testing-arch parser workstream): the REAL
//! `crates/bridges/bybit/src/event_mapper.rs`'s `map_bybit_perp` (the whole-frame entry the
//! captured-wire replay uses) and `crates/bridges/bybit/src/market_data.rs`'s `route_frame`, fed
//! (a) arbitrary JSON, (b) arbitrary text, and (c) every deterministic
//! `vike_bridge_core::capture::frame_mutations` of every committed captured fixture. The property
//! is TOTALITY: a hostile or truncated frame may decode to nothing, but it must never panic the
//! user-data pump thread, and it must never fabricate an event flood.
//!
//! A minimized counterexample is a REAL bug: commit the `proptest-regressions/` seed and report
//! it rather than patching the mapper in this PR.

use std::path::PathBuf;

use proptest::prelude::*;
use serde_json::Value;
use vike_bridge_core::capture::{frame_mutations, load_captured};
use vike_bybit::event_mapper::map_bybit_perp;
use vike_bybit::market_data::{route_frame, MdEvent};
use vike_model::L2Book;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

/// `map_bybit_perp` fans out over the `data` ARRAY (one venue row each), so the flood bound
/// scales with the row count instead of pretending a per-frame constant exists.
fn flood_bound(frame: &Value) -> usize {
    8 + 8 * frame.get("data").and_then(Value::as_array).map_or(0, Vec::len)
}

/// Real bybit dispatch keys mixed into the generator so it actually reaches the mapper's arms
/// (the generator-never-reaches-the-branch trap): `topic`/`data` route the frame,
/// `execType`/`orderStatus`/`orderLinkId`/`markPrice` steer the per-row decode,
/// `type`/`u`/`seq`/`s`/`b`/`a` steer `route_frame`'s orderbook lane.
fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("topic".to_string()),
        Just("data".to_string()),
        Just("type".to_string()),
        Just("execType".to_string()),
        Just("orderStatus".to_string()),
        Just("orderLinkId".to_string()),
        Just("markPrice".to_string()),
        Just("u".to_string()),
        Just("seq".to_string()),
        Just("s".to_string()),
        Just("b".to_string()),
        Just("a".to_string()),
        "[a-zA-Z_]{1,8}",
    ]
}

/// Arbitrary JSON, depth <= 4: objects/arrays over string/number/bool/null leaves.
fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        any::<f64>()
            .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
        "[ -~]{0,16}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec((arb_key(), inner), 0..6)
                .prop_map(|kvs| Value::Object(kvs.into_iter().collect())),
        ]
    })
}

proptest! {
    /// (a) `map_bybit_perp` is TOTAL over arbitrary JSON: no panic, no event flood.
    #[test]
    fn map_bybit_perp_is_total_over_arbitrary_json(frame in arb_json()) {
        let evs = map_bybit_perp(&frame, "bybit", "BTCUSDT");
        prop_assert!(evs.len() <= flood_bound(&frame), "event flood: {} events", evs.len());
    }

    /// (b) `route_frame` is total over arbitrary printable text; non-JSON is always `Ignored`.
    #[test]
    fn route_frame_is_total_and_ignores_non_json(text in "[ -~]{0,256}") {
        let mut book = L2Book::new(0.01);
        let out = route_frame(&text, "BTCUSDT", &mut book);
        if serde_json::from_str::<Value>(&text).is_err() {
            prop_assert!(matches!(out, MdEvent::Ignored), "non-JSON must be Ignored");
        }
    }

    /// (b') ...and over raw byte noise (controls, newlines, lossy-decoded invalid UTF-8) — the
    /// printable-ASCII strategy above never generates those, so they get their own lane.
    #[test]
    fn route_frame_survives_byte_noise(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut book = L2Book::new(0.01);
        let _ = route_frame(&text, "BTCUSDT", &mut book);
    }
}

/// (c) Every deterministic structural mutation of every committed captured frame decodes without
/// a panic — real wire shapes with one key removed / nulled / type-flipped / edge-cased.
#[test]
fn every_mutation_of_every_captured_frame_decodes_without_panic() {
    let kinds = ["ws_accepted", "ws_canceled", "ws_fill", "ws_account_state"];
    let mut mutations_run = 0usize;
    for kind in kinds {
        let fx = load_captured(&fixtures_dir(), kind)
            .unwrap_or_else(|| panic!("committed fixture {kind}.json is missing"));
        for (fi, frame) in fx.frames.iter().enumerate() {
            for (mi, m) in frame_mutations(frame).iter().enumerate() {
                let evs = map_bybit_perp(m, "bybit", "BTCUSDT");
                assert!(evs.len() <= flood_bound(m), "{kind}[{fi}] mutation {mi}: event flood");
                let mut book = L2Book::new(0.01);
                let _ = route_frame(&m.to_string(), "BTCUSDT", &mut book);
                mutations_run += 1;
            }
        }
    }
    assert!(mutations_run > 100, "only {mutations_run} mutations ran — the enumerator is broken");
}
