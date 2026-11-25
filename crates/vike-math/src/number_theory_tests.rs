use super::*;

/// Euclid in both argument orders, a coprime pair, a zero side and two zeros, and the largest sides.
#[test]
fn the_greatest_common_divisor_reduces_a_ratio_to_its_lowest_terms() {
    for (a, b, divisor) in [
        (12, 18, 6),
        (18, 12, 6),
        (10_000, 9_356, 4),
        (1_067, 1_000, 1),
        (3, 2, 1),
        (7, 0, 7),
        (0, 7, 7),
        (0, 0, 0),
        (u64::MAX, u64::MAX, u64::MAX),
        (u64::MAX, 1, 1),
    ] {
        assert_eq!(greatest_common_divisor(a, b), divisor, "gcd({a}, {b})");
    }
}
