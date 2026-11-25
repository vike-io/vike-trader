//! Wire-shape tests: a level is still a two-element array, and `L2BookUpdate` serde defaults.

use super::*;

/// The claim [`L2BookLevel`]'s doc makes — a level is still a two-element ARRAY on the wire, so a
/// journal written while `L2BookLevel` was a `(f64, f64)` alias still replays — checked against the
/// TEXT rather than through a round-trip. A round-trip proves only that this code agrees with
/// itself, which it would do just as happily after the shape changed to an object.
#[test]
fn a_level_is_still_a_two_element_array_on_the_wire() {
    let u = L2BookUpdate {
        timestamp: 1,
        local_timestamp: 0,
        sequence: 1,
        kind: L2BookUpdateKind::Snapshot,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(0.45, 100.0)],
        asks: vec![],
        symbol: String::new(),
    };
    let s = serde_json::to_string(&u).unwrap();
    assert!(
        s.contains(r#""bids":[[0.45,100.0]]"#),
        "a level must serialise as [price, qty]; got {s}"
    );

    // ...and the other direction, from a payload this code never wrote: the exact array shape
    // a pre-`L2BookLevel`-struct journal holds.
    let legacy: L2BookUpdate = serde_json::from_str(
        r#"{"timestamp":1,"sequence":1,"kind":"Snapshot","tick_size":0.01,"bids":[[0.45,100.0]],"asks":[]}"#,
    )
    .expect("a legacy array-shaped level must still deserialise");
    assert_eq!(legacy.bids[0].price.to_bits(), 0.45f64.to_bits());
    assert_eq!(legacy.bids[0].qty.to_bits(), 100.0f64.to_bits());
}

#[test]
fn book_update_serde_roundtrip_and_defaults() {
    let u = L2BookUpdate {
        timestamp: 1_000,
        local_timestamp: 1_002,
        sequence: 7,
        kind: L2BookUpdateKind::Delta,
        tick_size: 0.01,
        bids: vec![L2BookLevel::new(0.45, 100.0)],
        asks: vec![L2BookLevel::new(0.46, 50.0)],
        symbol: "TOK".to_string(),
    };
    let s = serde_json::to_string(&u).unwrap();
    let back: L2BookUpdate = serde_json::from_str(&s).unwrap();
    assert_eq!(back.sequence, 7);
    assert_eq!(back.kind, L2BookUpdateKind::Delta);
    assert_eq!(back.bids[0].price.to_bits(), 0.45f64.to_bits());
    // additive-serde contract: local_timestamp and symbol absent in old payloads → defaults
    let old: L2BookUpdate = serde_json::from_str(
        r#"{"timestamp":1,"sequence":1,"kind":"Snapshot","tick_size":0.01,"bids":[],"asks":[]}"#,
    )
    .unwrap();
    assert_eq!(old.local_timestamp, 0);
    assert!(old.symbol.is_empty());
}
