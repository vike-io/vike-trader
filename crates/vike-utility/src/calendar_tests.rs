use super::*;

/// The one rule against the primitives it guards: a day is real exactly when the civil round trip
/// hands it back, over two full 400-year leap cycles and past both ends of every field.
#[test]
fn a_real_day_is_one_the_civil_round_trip_hands_back() {
    for y in -400..=2800_i64 {
        for m in 1..=12_u32 {
            for d in 1..=32_u32 {
                let round_trip = civil_from_days(days_from_civil(y, m, d)) == (y, m, d);
                assert_eq!(is_real_day(y, m, d), round_trip, "{y}-{m}-{d}");
            }
        }
    }
    for (y, m, d) in [(2026, 0, 1), (2026, 13, 1), (2026, 1, 0), (2026, 1, u32::MAX)] {
        assert!(!is_real_day(y, m, d), "{y}-{m}-{d}");
    }
    assert!(is_real_day(2000, 2, 29) && !is_real_day(2100, 2, 29) && !is_real_day(2026, 2, 31));
    assert!(is_real_day(i64::MAX, 1, 1) && is_real_day(i64::MIN, 1, 1), "no overflow at the edges");
}

#[test]
fn civil_roundtrips_days() {
    // days_from_civil ∘ civil_from_days == identity across a wide range.
    for z in [-146_097_i64, -719_468, -1, 0, 1, 18_628, 100_000, 999_999] {
        let (y, m, d) = civil_from_days(z);
        assert_eq!(days_from_civil(y, m, d), z, "roundtrip failed at z={z}");
    }
}
