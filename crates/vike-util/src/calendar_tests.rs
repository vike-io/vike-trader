use super::*;

#[test]
fn civil_roundtrips_days() {
    // days_from_civil ∘ civil_from_days == identity across a wide range.
    for z in [-146_097_i64, -719_468, -1, 0, 1, 18_628, 100_000, 999_999] {
        let (y, m, d) = civil_from_days(z);
        assert_eq!(days_from_civil(y, m, d), z, "roundtrip failed at z={z}");
    }
}
