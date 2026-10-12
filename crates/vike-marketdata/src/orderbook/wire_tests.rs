//! Wire-shape tests: a level is still a two-element array, and `BookUpdate` serde defaults.

use super::*;

/// The claim [`BookLevel`]'s doc makes — a level is still a two-element ARRAY on the wire, so a
/// journal written while `BookLevel` was a `(f64, f64)` alias still replays — checked against the
/// TEXT rather than through a round-trip. A round-trip proves only that this code agrees with
/// itself, which it would do just as happily after the shape changed to an object.
#[test]
fn a_level_is_still_a_two_element_array_on_the_wire() {
    let u = BookUpdate {
        ts: 1,
        local_ts: 0,
        seq: 1,
        kind: BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.45, 100.0)],
        asks: vec![],
        symbol: String::new(),
    };
    let s = serde_json::to_string(&u).unwrap();
    assert!(
        s.contains(r#""bids":[[0.45,100.0]]"#),
        "a level must serialise as [price, qty]; got {s}"
    );

    // ...and the other direction, from a payload this code never wrote: the exact array shape
    // a pre-`BookLevel`-struct journal holds.
    let legacy: BookUpdate = serde_json::from_str(
        r#"{"ts":1,"seq":1,"kind":"Snapshot","tick_size":0.01,"bids":[[0.45,100.0]],"asks":[]}"#,
    )
    .expect("a legacy array-shaped level must still deserialise");
    assert_eq!(legacy.bids[0].price.to_bits(), 0.45f64.to_bits());
    assert_eq!(legacy.bids[0].qty.to_bits(), 100.0f64.to_bits());
}

#[test]
fn book_update_serde_roundtrip_and_defaults() {
    let u = BookUpdate {
        ts: 1_000,
        local_ts: 1_002,
        seq: 7,
        kind: BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: vec![BookLevel::new(0.45, 100.0)],
        asks: vec![BookLevel::new(0.46, 50.0)],
        symbol: "TOK".to_string(),
    };
    let s = serde_json::to_string(&u).unwrap();
    let back: BookUpdate = serde_json::from_str(&s).unwrap();
    assert_eq!(back.seq, 7);
    assert_eq!(back.kind, BookUpdateKind::Delta);
    assert_eq!(back.bids[0].price.to_bits(), 0.45f64.to_bits());
    // additive-serde contract: local_ts and symbol absent in old payloads → defaults
    let old: BookUpdate = serde_json::from_str(
        r#"{"ts":1,"seq":1,"kind":"Snapshot","tick_size":0.01,"bids":[],"asks":[]}"#,
    )
    .unwrap();
    assert_eq!(old.local_ts, 0);
    assert!(old.symbol.is_empty());
}
