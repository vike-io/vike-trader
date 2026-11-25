//! Whole-number arithmetic: the greatest common divisor, which reduces a ratio to its lowest terms (a
//! stock split's `3:2`, a combo's leg ratio).

/// The greatest common divisor of `a` and `b`, by Euclid's remainders: `gcd(12, 18) = 6`. A zero side
/// gives the other (`gcd(a, 0) = a`), so `gcd(0, 0) = 0`: a caller that divides by it checks its own
/// inputs are not both zero.
pub fn greatest_common_divisor(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
#[path = "number_theory_tests.rs"]
mod tests;
