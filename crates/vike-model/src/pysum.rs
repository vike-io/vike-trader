//! CPython builtin `sum()` twin for floats.
//!
//! CPython ≥ 3.12 computes `sum()` over floats with **Neumaier compensated summation**
//! (gh-100425, `bltinmodule.c` float fast path) — NOT a naive left fold. Every ported
//! call site that Python writes as builtin `sum(...)` over floats must use this function;
//! explicit `acc += x` loops in Python stay naive folds. Getting this wrong is a
//! guaranteed last-ulp divergence (caught by the R2 bit-gates on 2026-07-03).

/// Neumaier-compensated sum matching CPython's `builtin_sum` float fast path
/// (start = 0, then per item: t = r + x; compensation by magnitude; final
/// `if c != 0.0 && c.is_finite() { r += c }` — the sign/NaN guard is CPython's).
pub fn py_sum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let mut f_result = 0.0_f64;
    let mut c = 0.0_f64;
    for x in items {
        let t = f_result + x;
        if f_result.abs() >= x.abs() {
            c += (f_result - t) + x;
        } else {
            c += (x - t) + f_result;
        }
        f_result = t;
    }
    // "Avoid losing the sign on a negative result, and don't let adding the
    //  compensation convert an infinite or overflowed sum to a NaN." — CPython
    if c != 0.0 && c.is_finite() {
        f_result += c;
    }
    f_result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every golden below is preceded by an `assert_ne!` against the naive fold it must beat, so a
    /// case that has stopped discriminating fails LOUDLY instead of passing for the wrong reason —
    /// the shape `crates/vike-core/tests/drawdown_measures_own_pnl.rs` already uses. Assertions are
    /// on `to_bits()` rather than on `==`, because this function's whole subject is the last bit
    /// and because `-0.0 == 0.0` compares equal while the bits do not.
    ///
    /// ⚠ The inputs are exact powers of two and small integers ON PURPOSE. A vector built from a
    /// transcendental would pin whatever `libm` answered on the box that recorded it, which is the
    /// failure `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`
    /// exists to prevent; every literal here is derivable by hand from IEEE 754 alone.
    ///
    /// ⚠ **`c != 0.0` is an EQUIVALENT MUTANT in this port and has deliberately no test.** CPython
    /// reaches a `-0.0` compensation through a caller-supplied `start`; this signature has none, so
    /// `f_result` is seeded `+0.0` and round-to-nearest yields `-0.0` only from `(-0.0) + (-0.0)`.
    /// Deleting that half of the guard changes no output reachable from here. Do not go looking for
    /// the case that covers it — there isn't one, and that is the finding rather than a gap.
    const NAIVE_LOSES_THE_SMALL_TERM: [f64; 3] = [1e100, 1.0, -1e100];
    const NAIVE_LOSES_IT_ON_THE_OTHER_BRANCH: [f64; 3] = [1.0, 1e100, -1e100];

    fn naive(xs: &[f64]) -> f64 {
        xs.iter().copied().sum()
    }

    #[test]
    fn the_compensation_recovers_a_term_a_naive_fold_drops() {
        // 1e100 swallows 1.0 whole — the ulp up there is ~1e84 — and the closing -1e100 then
        // cancels the big term, so a naive fold returns 0.0 as if the 1.0 had never been added.
        // Neumaier carries the lost 1.0 in `c` and folds it back at the end.
        let xs = NAIVE_LOSES_THE_SMALL_TERM;
        assert_ne!(
            naive(&xs).to_bits(),
            1.0_f64.to_bits(),
            "the naive fold must LOSE the term here, or this case is not testing compensation"
        );
        assert_eq!(naive(&xs).to_bits(), 0.0_f64.to_bits());
        assert_eq!(py_sum(xs).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn both_magnitude_branches_carry_the_compensation() {
        // The `if f_result.abs() >= x.abs()` split is not cosmetic: the two arms subtract in
        // opposite orders, and only the one matching the magnitudes keeps the small term. The
        // vector above enters the big value FIRST (so the surviving compensation is taken on the
        // `>=` arm); this one enters it SECOND, which routes the same recovery through `else`.
        // Inverting the comparison makes BOTH return 0.0, which is what these two cases pin.
        let xs = NAIVE_LOSES_IT_ON_THE_OTHER_BRANCH;
        assert_ne!(naive(&xs).to_bits(), 1.0_f64.to_bits());
        assert_eq!(py_sum(xs).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn an_infinite_sum_is_not_turned_into_a_nan_by_its_own_compensation() {
        // `c` becomes NaN the moment a term is infinite — `(inf - inf)` — and adding that back
        // would report NaN for a sum that is honestly infinite. `c.is_finite()` is what stops it,
        // and this is the case that fails if that half of the guard is deleted.
        let got = py_sum([f64::INFINITY, 1.0]);
        assert!(got.is_infinite() && got.is_sign_positive(), "got {got}");
        assert_eq!(py_sum([f64::NEG_INFINITY, 1.0]), f64::NEG_INFINITY);
    }

    #[test]
    fn a_nan_term_still_produces_a_nan() {
        // The guard must not launder a NaN INPUT into a finite answer — it only refuses to let a
        // NaN COMPENSATION spoil a good running total.
        assert!(py_sum([f64::NAN, 1.0]).is_nan());
        assert!(py_sum([1.0, f64::NAN]).is_nan());
    }

    #[test]
    fn an_empty_sum_is_positive_zero_and_a_single_term_is_itself() {
        // CPython's `sum(())` is `0`, positive. The bit check is the point: `-0.0` would compare
        // equal under `==` and is a different value to every consumer that divides by it.
        assert_eq!(py_sum([]).to_bits(), 0.0_f64.to_bits());
        // ⚠ `-0.0` is NOT in this list — see the test below. Everything else passes through.
        for x in [1.0, -1.0, 0.5, f64::MIN_POSITIVE, f64::MAX] {
            assert_eq!(py_sum([x]).to_bits(), x.to_bits(), "a single term must pass through: {x}");
        }
    }

    #[test]
    fn a_lone_negative_zero_comes_back_positive_which_is_cpythons_answer_too() {
        // ⚠ MEASURED, and it contradicted the obvious guess — this case was written asserting
        // pass-through and went red on its first run, which is why it is its own test now.
        //
        // The accumulator is seeded `+0.0`, and IEEE round-to-nearest makes `(+0.0) + (-0.0)`
        // POSITIVE zero (only `(-0.0) + (-0.0)` is negative). The compensation is then exactly
        // zero, so the closing `c != 0.0` guard does not fire and nothing restores the sign.
        //
        // This is FAITHFUL rather than a defect: CPython's `sum()` seeds its float fast path the
        // same way, so `sum([-0.0])` is `0.0` there too. A caller that needs the sign of a lone
        // `-0.0` must not route it through a summation — in either language.
        assert_eq!(py_sum([-0.0]).to_bits(), 0.0_f64.to_bits());
        assert_ne!(py_sum([-0.0]).to_bits(), (-0.0_f64).to_bits());
        // ⚠ And NO number of them changes that, which is the stronger statement: `(-0.0) + (-0.0)`
        // is the one case IEEE keeps negative, but the accumulator is `+0.0` before the first term
        // and `+0.0` after it, so that pairing is never reached from this signature. This is the
        // PROOF of the equivalent mutant named at the top of this module — `f_result` cannot be
        // `-0.0`, so the `c != 0.0` half of the closing guard has no reachable effect.
        assert_eq!(py_sum([-0.0, -0.0]).to_bits(), 0.0_f64.to_bits());
        assert_eq!(py_sum([-0.0, -0.0, -0.0]).to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn ordinary_sums_are_unchanged_by_the_compensation() {
        // Compensation must be INVISIBLE where a naive fold is already exact, or every ported site
        // would shift the moment it started using this function. Powers of two below 2^53 add
        // exactly, so both folds must agree bit for bit.
        for xs in [vec![1.0, 2.0, 4.0], vec![0.25, 0.5, 0.25], vec![-3.0, 1.0, 2.0]] {
            assert_eq!(
                py_sum(xs.iter().copied()).to_bits(),
                naive(&xs).to_bits(),
                "exact inputs must fold identically: {xs:?}"
            );
        }
    }
}
