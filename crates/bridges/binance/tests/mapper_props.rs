//! Property harness for the binance parser surface (testing-arch parser workstream): the REAL
//! `crates/bridges/binance/src/family/event_mapper.rs`'s `map_execution_report` / `map_private`
//! (re-exported as `map_binance_private`) and `crates/bridges/binance/src/family/depth.rs`'s
//! `route_frame`, fed (a) arbitrary JSON, (b) arbitrary text, and (c) every deterministic
//! `vike_bridge_core::capture::frame_mutations` of every committed captured fixture. The property
//! is TOTALITY: a hostile or truncated frame may decode to nothing, but it must never panic the
//! user-data pump thread, and it must never fabricate an event flood.
//!
//! A minimized counterexample is a REAL bug: commit the `proptest-regressions/` seed and report
//! it — the mapper is a shared home (binance + aster), so the fix is its own coordinated PR.

use std::path::PathBuf;

use proptest::prelude::*;
use serde_json::Value;
use vike_binance::event_mapper::{map_binance_private, map_execution_report};
use vike_binance::market_data::{MdEvent, route_frame};
use vike_bridge_core::capture::{frame_mutations, load_captured};
use vike_model::L2Book;

/// One executionReport legitimately emits a handful of events (the dual-publish fill contract);
/// more than this from ONE frame is a mapper bug regardless of input.
const MAX_EVENTS_PER_FRAME: usize = 16;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/captured")
}

/// Real binance dispatch keys mixed into the generator so it actually reaches the mapper's arms
/// instead of bouncing off the first `.get()` (the generator-never-reaches-the-branch trap):
/// `e`/`o` route the private frame, `c`/`C`/`s`/`x`/`X`/`t` steer the executionReport decode,
/// `stream`/`data` route the market-data frame.
fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("e".to_string()),
        Just("o".to_string()),
        Just("c".to_string()),
        Just("C".to_string()),
        Just("s".to_string()),
        Just("x".to_string()),
        Just("X".to_string()),
        Just("t".to_string()),
        Just("stream".to_string()),
        Just("data".to_string()),
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
    /// (a) The two exec mappers are TOTAL over arbitrary JSON: no panic, no event flood.
    #[test]
    fn exec_mappers_are_total_over_arbitrary_json(frame in arb_json()) {
        let a = map_execution_report(&frame, "binance", "BTCUSDT");
        prop_assert!(a.len() < MAX_EVENTS_PER_FRAME, "event flood: {} events", a.len());
        let b = map_binance_private(&frame, "binance", "BTCUSDT");
        prop_assert!(b.len() < MAX_EVENTS_PER_FRAME, "event flood: {} events", b.len());
    }

    /// (b) `route_frame` is total over arbitrary printable text, and NON-JSON input is always
    /// `Ignored` — never a book mutation, never a panic.
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
/// a panic — the near-miss lane: real wire shapes with one key removed / nulled / type-flipped.
#[test]
fn every_mutation_of_every_captured_frame_decodes_without_panic() {
    let kinds = ["ws_accepted", "ws_canceled", "ws_fill", "ws_account_state"];
    let mut mutations_run = 0usize;
    for kind in kinds {
        let fx = load_captured(&fixtures_dir(), kind)
            .unwrap_or_else(|| panic!("committed fixture {kind}.json is missing"));
        for (fi, frame) in fx.frames.iter().enumerate() {
            for (mi, m) in frame_mutations(frame).iter().enumerate() {
                let a = map_execution_report(m, "binance", "BTCUSDT");
                assert!(a.len() < MAX_EVENTS_PER_FRAME, "{kind}[{fi}] mutation {mi}: flood");
                let b = map_binance_private(m, "binance", "BTCUSDT");
                assert!(b.len() < MAX_EVENTS_PER_FRAME, "{kind}[{fi}] mutation {mi}: flood");
                let mut book = L2Book::new(0.01);
                let _ = route_frame(&m.to_string(), "BTCUSDT", &mut book);
                mutations_run += 1;
            }
        }
    }
    assert!(mutations_run > 100, "only {mutations_run} mutations ran — the enumerator is broken");
}
