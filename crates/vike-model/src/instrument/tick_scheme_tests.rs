use super::*;

fn tier(above_price: f64, tick_size: f64) -> TickTier {
    TickTier { above_price, tick_size }
}

/// The real Deribit BTC-option grid: base `0.0001`, one step to `0.0005` above `0.005`.
fn deribit_option_scheme() -> TickScheme {
    TickScheme::new(0.0001, &[tier(0.005, 0.0005)]).expect("valid deribit grid")
}

#[test]
fn resolves_below_at_and_above_the_boundary() {
    let s = deribit_option_scheme();
    assert!(s.is_tiered());
    assert_eq!(s.base_tick(), 0.0001);
    // BELOW the boundary -> the base tick
    assert_eq!(s.tick_at(0.0001), 0.0001);
    assert_eq!(s.tick_at(0.0049), 0.0001);
    // ABOVE the boundary -> the tier tick (this is the case the live smoke dodges)
    assert_eq!(s.tick_at(0.0051), 0.0005);
    assert_eq!(s.tick_at(0.05), 0.0005);
    assert_eq!(s.tick_at(1.0), 0.0005);
}

/// THE boundary pin: `above_price` is EXCLUSIVE, so the boundary itself belongs to the LOWER
/// tier. Flipping this must break a test, not slip through.
#[test]
fn the_exact_boundary_belongs_to_the_lower_tier() {
    let s = deribit_option_scheme();
    assert_eq!(s.tick_at(0.005), 0.0001, "exactly AT above_price -> the tick BELOW it");
    // ...and on the shape venues publish the choice is inert: the boundary is on both grids,
    // so the snapped price is the same under either reading.
    assert_eq!(s.round_price(0.005), round_to_step(0.005, 0.0001));
    assert_eq!(round_to_step(0.005, 0.0001), round_to_step(0.005, 0.0005));
}

/// Several tiers resolve by walking ascending boundaries, each with its own exclusive edge.
#[test]
fn multi_tier_resolution_walks_ascending_boundaries() {
    let tiers = [tier(10.0, 0.05), tier(100.0, 0.5), tier(1000.0, 5.0)];
    let s = TickScheme::new(0.01, &tiers).unwrap();
    assert_eq!(s.tiers().len(), 3);
    assert_eq!(s.tick_at(0.0), 0.01);
    assert_eq!(s.tick_at(9.99), 0.01);
    assert_eq!(s.tick_at(10.0), 0.01); // exact boundary -> lower tier
    assert_eq!(s.tick_at(10.5), 0.05);
    assert_eq!(s.tick_at(100.0), 0.05); // exact boundary -> lower tier
    assert_eq!(s.tick_at(100.5), 0.5);
    assert_eq!(s.tick_at(1000.0), 0.5); // exact boundary -> lower tier
    assert_eq!(s.tick_at(5000.0), 5.0);
}

/// A FLAT scheme (no tiers) is the scalar world: every price resolves to the base tick, and
/// `round_price` equals the pinned primitive on that one tick.
#[test]
fn flat_scheme_is_the_scalar_grid() {
    let s = TickScheme::new(0.5, &[]).unwrap();
    assert!(!s.is_tiered());
    assert!(s.tiers().is_empty());
    for p in [0.0, 0.4, 2.5, 1e9] {
        assert_eq!(s.tick_at(p), 0.5);
        assert_eq!(s.round_price(p), round_to_step(p, 0.5));
    }
}

/// Magnitude, not sign: a signed (credit-combo) net limit resolves on the same grid as its
/// debit mirror. A non-finite price stays total: `NaN` yields the BASE tick (no `>` holds),
/// `±inf` the TOP tier (every finite boundary is exceeded).
#[test]
fn resolution_uses_the_magnitude_and_stays_total() {
    let s = deribit_option_scheme();
    assert_eq!(s.tick_at(-0.05), 0.0005);
    assert_eq!(s.tick_at(-0.001), 0.0001);
    assert_eq!(s.tick_at(f64::NAN), 0.0001);
    assert_eq!(s.tick_at(f64::INFINITY), 0.0005); // inf > every finite boundary
    assert_eq!(s.tick_at(f64::NEG_INFINITY), 0.0005); // ...magnitude, so likewise
}

/// Rounding snaps onto the grid RESOLVED FROM THE INPUT price — delegating, unchanged, to the
/// parity-sacred primitive. This is the hole the lane exists to close: a price above the tier
/// boundary snapped on the BASE grid is what the venue rejects.
#[test]
fn rounding_uses_the_tier_in_force_at_the_input_price() {
    let s = deribit_option_scheme();
    // above the boundary: the COARSE grid (0.0005), not the base one
    assert_eq!(s.round_price(0.01234), round_to_step(0.01234, 0.0005));
    assert_ne!(s.round_price(0.01234), round_to_step(0.01234, 0.0001));
    // below it: the base grid
    assert_eq!(s.round_price(0.00123), round_to_step(0.00123, 0.0001));
    // and the tiered result really is a multiple of the coarse tick
    let snapped = s.round_price(0.01234);
    assert_eq!(snapped, round_to_step(snapped, 0.0005), "snapping is idempotent on the tier");
}

/// A base-tick rounding from just BELOW a boundary can at worst land ON it, never strictly
/// past it (the boundary is on the base grid) — the property `round_price`'s single pass
/// relies on.
#[test]
fn base_grid_rounding_never_jumps_past_the_boundary() {
    let s = deribit_option_scheme();
    for p in [0.00494, 0.004951, 0.004999] {
        let r = s.round_price(p);
        assert!(r <= 0.005, "{p} rounded to {r}, past the boundary");
    }
}

#[test]
fn invalid_schemes_are_rejected() {
    assert_eq!(TickScheme::new(0.0, &[]), Err(TickSchemeError::BadBaseTick));
    assert_eq!(TickScheme::new(-0.1, &[]), Err(TickSchemeError::BadBaseTick));
    assert_eq!(TickScheme::new(f64::NAN, &[]), Err(TickSchemeError::BadBaseTick));
    assert_eq!(TickScheme::new(f64::INFINITY, &[]), Err(TickSchemeError::BadBaseTick));
    let bad_tick = [tier(1.0, 0.0)];
    assert_eq!(TickScheme::new(0.1, &bad_tick), Err(TickSchemeError::BadTierTick));
    let inf_tick = [tier(1.0, f64::INFINITY)];
    assert_eq!(TickScheme::new(0.1, &inf_tick), Err(TickSchemeError::BadTierTick));
    let neg_edge = [tier(-1.0, 0.5)];
    assert_eq!(TickScheme::new(0.1, &neg_edge), Err(TickSchemeError::BadBoundary));
    let nan_edge = [tier(f64::NAN, 0.5)];
    assert_eq!(TickScheme::new(0.1, &nan_edge), Err(TickSchemeError::BadBoundary));
    // duplicate / descending boundaries are BOTH unsorted
    let dup = [tier(5.0, 0.5), tier(5.0, 1.0)];
    assert_eq!(TickScheme::new(0.1, &dup), Err(TickSchemeError::UnsortedTiers));
    let descending = [tier(9.0, 0.5), tier(5.0, 1.0)];
    assert_eq!(TickScheme::new(0.1, &descending), Err(TickSchemeError::UnsortedTiers));
}

/// Over-long tier arrays are REPORTED, never silently truncated — a truncated grid would
/// put a wrong-tick price on the wire.
#[test]
fn more_tiers_than_capacity_is_an_error_not_a_truncation() {
    let many: Vec<TickTier> = (1..=MAX_TICK_TIERS + 1).map(|i| tier(i as f64, 0.5)).collect();
    assert_eq!(many.len(), MAX_TICK_TIERS + 1);
    assert_eq!(TickScheme::new(0.1, &many), Err(TickSchemeError::TooManyTiers));
    // exactly at capacity is fine
    assert!(TickScheme::new(0.1, &many[..MAX_TICK_TIERS]).is_ok());
}

/// THE off-path pin: with no scheme, the tier-aware rounder is the EXPRESSION IN USE TODAY.
#[test]
fn tierless_rounding_is_byte_identical_to_round_to() {
    for tick in [0.0, 0.0001, 0.01, 0.5, -0.5] {
        for v in [0.0, 1.23456, -1.23456, 2.5, 0.00499, 1e9] {
            assert_eq!(
                round_price_tiered(v, None, tick),
                round_to(v, nz_step(tick)),
                "v={v} tick={tick}"
            );
        }
    }
}

/// ...and WITH a scheme it is the scheme's own rounding (the tick_size argument is ignored).
#[test]
fn tiered_rounding_defers_to_the_scheme() {
    let s = deribit_option_scheme();
    assert_eq!(round_price_tiered(0.01234, Some(&s), 0.0001), s.round_price(0.01234));
    assert_eq!(round_price_tiered(0.01234, Some(&s), 999.0), s.round_price(0.01234));
}

#[test]
fn serde_round_trips_flat_and_tiered_schemes() {
    let flat = TickScheme::new(0.5, &[]).unwrap();
    let s = serde_json::to_string(&flat).unwrap();
    assert_eq!(s, r#"{"base_tick":0.5}"#, "a flat scheme omits `tiers` entirely");
    assert_eq!(serde_json::from_str::<TickScheme>(&s).unwrap(), flat);

    let tiered = deribit_option_scheme();
    let s = serde_json::to_string(&tiered).unwrap();
    assert!(s.contains("\"tiers\""), "a tiered scheme carries its tiers: {s}");
    let back: TickScheme = serde_json::from_str(&s).unwrap();
    assert_eq!(back, tiered);
    assert_eq!(back.tiers(), tiered.tiers());
    assert_eq!(back.tick_at(0.05), 0.0005);
}

/// The wire shape is the VENUE shape, so a hand-written (or venue-mirroring) payload decodes.
#[test]
fn deserializes_the_venue_shaped_payload() {
    let raw = r#"{"base_tick":0.0001,"tiers":[{"above_price":0.005,"tick_size":0.0005}]}"#;
    let s: TickScheme = serde_json::from_str(raw).unwrap();
    assert_eq!(s, deribit_option_scheme());
}

/// A persisted row can no more hold an invalid scheme than a caller can: deserialization
/// re-runs the constructor's validation and FAILS rather than yielding a zero-tick grid.
#[test]
fn deserializing_an_invalid_scheme_fails() {
    assert!(serde_json::from_str::<TickScheme>(r#"{"base_tick":0.0}"#).is_err());
    let descending = r#"{"base_tick":0.1,"tiers":[{"above_price":9,"tick_size":1},
                             {"above_price":5,"tick_size":2}]}"#;
    assert!(serde_json::from_str::<TickScheme>(descending).is_err());
}
