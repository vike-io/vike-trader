//! Small integer powers, spelled as one multiply chain each and never `powi`: `square`, `cube`,
//! `fourth_power`. The association order is part of each contract (this crate's module doc, rule 3).
//! And the powers of ten an `f64` holds exactly, `1e-22..=1e22` ([`power_of_ten`]), the scale of a tick,
//! a pip or a count of decimals.

/// `x * x`, and the ONE spelling of it the workspace uses — never `x.powi(2)`.
///
/// ⚠ **MEASURED 2026-08-25: `f64::powi` is not bit-stable across OPTIMISATION LEVELS on
/// Windows.** In a `dev` build MSVC lowers `powi` to the CRT's `pow()`, which IEEE 754 does not
/// require to be correctly rounded; in a `release` build LLVM expands the same call to a plain
/// multiply. Identical source therefore produced two different numbers on one box — and `dev` is
/// the profile `just t vike-indicators` and CI's roster lane actually run.
///
/// The measurement lives in `crates/vike-indicators/tests/libm_platform_probe.rs`'s module doc (the indicators
/// these powers moved from, decision 0294):
/// the four-cell table, plus the row that identifies the libcall by showing `powi(2)` hashing
/// EQUAL to `powf(2.0)` in exactly the one anomalous cell.
///
/// A plain multiply is what every other configuration already computes, so routing through here
/// moves NOTHING except that cell — and it is what lets `libm_cross_platform_pin` hold on the dev
/// box in the profile that box actually uses.
///
/// See [`cube`] / [`fourth_power`] for the higher powers, which close the residual this doc used to declare.
#[inline]
pub fn square(x: f64) -> f64 {
    x * x
}

/// `x³`, spelled as LLVM's own expansion spells it — never `x.powi(3)`.
///
/// ⚠ **The association order is the whole point, and a left-to-right chain is the WRONG one.**
/// `llvm.powi` with a constant exponent lowers by BINARY EXPONENTIATION, not by a running product:
/// it squares once and multiplies the square by the base. `x * x * x` parses as `(x * x) * x`,
/// which happens to agree here — but writing it as a chain invites the four-term case below to be
/// written the same way, where it does NOT agree. Both helpers are therefore spelled from the
/// square outward, so a reader sees one rule rather than two coincidences.
///
/// This is why converting these sites moves NOTHING on a platform whose LLVM already expanded
/// them, while fixing the one that emitted a libcall — the same argument [`square`] makes, extended to
/// the exponents [`square`] deliberately left behind. `libm_cross_platform_pin`'s committed table is
/// the evidence: it did not move when these landed.
#[inline]
pub fn cube(x: f64) -> f64 {
    let s = x * x;
    s * x
}

/// `x⁴` as `(x²)²` — never `x.powi(4)`, and never `x * x * x * x`.
///
/// ⚠ **This is the one where the naive chain genuinely disagrees.** LLVM squares the square;
/// a left-to-right product rounds after each of three multiplies instead of two, so
/// `((x * x) * x) * x` is a DIFFERENT f64 from `(x * x) * (x * x)` for inputs whose intermediate
/// products round. Converting to the chain would have moved values on every platform — which is
/// exactly what the residual note here used to predict, and why it read as a reason not to
/// convert at all. The prediction was right about the chain and wrong about the conversion: the
/// correct spelling is the square-of-the-square, and it moves nothing.
#[inline]
pub fn fourth_power(x: f64) -> f64 {
    let s = x * x;
    s * s
}

/// `10^exponent` as the `f64` NEAREST the decimal value, for `exponent` in `-22..=22`; `None` outside.
///
/// The bits are the literal `1e{exponent}`'s (what a decimal parse answers) and `10f64.powi(exponent)`'s,
/// on every platform and in every build profile, and that is a property rather than an observation: a
/// power of ten up to `1e22` is EXACT in `f64` (`5^22 < 2^53`), so the positive half is the value
/// itself, and a negative power is ONE division of `1.0` by an exact power, which IEEE 754 requires to
/// round to the nearest `f64`, the parse's own answer. `powi`'s lowering does not matter for an exact
/// input (MSVC's `dev` build calls the CRT's `pow`, a release build multiplies): measured on both
/// platforms in `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`'s
/// amendment of 2026-08-26, `0` mismatches of 23, and held here by `powers_tests.rs`'s
/// `a_power_of_ten_is_the_literal_and_powi_bit_for_bit`.
///
/// Past ±22 the positive powers stop being exact and the spellings part, so this answers nothing
/// rather than one of them: a caller with a wider range keeps its own rule at its own site.
pub fn power_of_ten(exponent: i32) -> Option<f64> {
    let magnitude = *POWERS_OF_TEN.get(usize::try_from(exponent.unsigned_abs()).ok()?)?;
    Some(if exponent < 0 { 1.0 / magnitude } else { magnitude })
}

/// `10^0..=10^22`, each exact in `f64` ([`power_of_ten`]).
const POWERS_OF_TEN: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

#[cfg(test)]
#[path = "powers_tests.rs"]
mod tests;
