//! Summation: the compensated sum and its single step, and the fixed-order folds the streaming
//! indicators keep.
//!
//! Two summation rules exist in this workspace ON PURPOSE (this crate's module doc, rule 3): the
//! compensated sum [`compensated_sum`] and the plain left fold (`iter().sum()` or `acc += x`). They
//! answer in different last bits, and every pinned number was recorded under one of them, so a site
//! never switches between them in passing.
//!
//! [`compensated_sum`] is Neumaier's compensated summation as CPython ≥ 3.12 computes `sum()` over
//! floats (gh-100425, `bltinmodule.c`'s float fast path): the parity fixtures (`fixtures/r*/`) were
//! recorded against it, and the ported call sites that Python wrote as builtin `sum(...)` over floats
//! use it. [`neumaier_add`] is its one step, shared with the order language's running sum
//! (`vike_tactics::rolling::RollingSum`), which keeps its own sum and carry.

/// One compensated step: add `x` to the running `sum`, carrying the rounding error in `carry`
/// (Neumaier: the compensation is taken against the larger magnitude). The caller folds `carry` back
/// when it reads the total. [`compensated_sum`] and `vike_tactics::rolling::RollingSum` run exactly
/// this expression.
#[inline]
pub fn neumaier_add(sum: &mut f64, carry: &mut f64, x: f64) {
    let t = *sum + x;
    if sum.abs() >= x.abs() {
        *carry += (*sum - t) + x;
    } else {
        *carry += (x - t) + *sum;
    }
    *sum = t;
}

/// Neumaier-compensated sum matching CPython's `builtin_sum` float fast path
/// (start = 0, then per item: [`neumaier_add`]; final
/// `if c != 0.0 && c.is_finite() { r += c }` — the sign/NaN guard is CPython's).
pub fn compensated_sum<I: IntoIterator<Item = f64>>(items: I) -> f64 {
    let mut sum = CompensatedSum::default();
    for x in items {
        sum.add(x);
    }
    sum.total()
}

/// [`compensated_sum`] fed one item at a time, so one walk can keep several sums: after the same items
/// in the same order, [`CompensatedSum::total`] is [`compensated_sum`]'s answer bit for bit (it is that
/// function's own fold).
#[derive(Debug, Clone, Copy, Default)]
pub struct CompensatedSum {
    sum: f64,
    carry: f64,
}

impl CompensatedSum {
    /// Adds `x` ([`neumaier_add`]).
    #[inline]
    pub fn add(&mut self, x: f64) {
        neumaier_add(&mut self.sum, &mut self.carry, x);
    }

    /// The sum of every item added, `0.0` for none.
    #[inline]
    pub fn total(&self) -> f64 {
        // "Avoid losing the sign on a negative result, and don't let adding the
        //  compensation convert an infinite or overflowed sum to a NaN." — CPython
        if self.carry != 0.0 && self.carry.is_finite() { self.sum + self.carry } else { self.sum }
    }
}

/// The sum of a ring buffer in its storage (slot) order, a plain fold from `0.0`: a streaming core's
/// rebuild of a running sum, where the order does not matter to the core but must stay fixed to the bit.
#[inline(always)]
pub fn ring_sum_in_slot_order(buffer: &[f64]) -> f64 {
    buffer.iter().fold(0.0, |a, &v| a + v)
}

/// The sum of a ring buffer read oldest to newest (the oldest value at `head`), a plain fold from
/// `0.0`: a streaming core's cold-path rebuild of a running sum. Cold and never inlined, as the
/// cores that call it on a rare branch want.
#[cold]
#[inline(never)]
pub fn ring_sum_oldest_first(buffer: &[f64], head: usize) -> f64 {
    buffer[head..].iter().chain(&buffer[..head]).fold(0.0, |a, &v| a + v)
}

/// `Σ |v - m|` over `w` in a fixed order on EIGHT lanes: four accumulators of two lanes each (each
/// lane a stride of eight values), then the remainder one by one. Not
/// [`absolute_deviation_sum_four_lanes`]: the two association orders give different last bits, and
/// each is pinned by the indicator that runs it (`cci`'s and `mad`'s cores).
#[inline(always)]
pub fn absolute_deviation_sum_eight_lanes(w: &[f64], m: f64) -> f64 {
    let mut accumulator = [[0.0_f64; 2]; 4];
    let (chunks, remainder) = w.as_chunks::<8>();
    for c in chunks {
        for (a, p) in accumulator.iter_mut().zip(c.as_chunks::<2>().0) {
            a[0] += (p[0] - m).abs();
            a[1] += (p[1] - m).abs();
        }
    }
    let mut tail = 0.0_f64;
    for &v in remainder {
        tail += (v - m).abs();
    }
    (((accumulator[0][0] + accumulator[0][1]) + (accumulator[1][0] + accumulator[1][1]))
        + ((accumulator[2][0] + accumulator[2][1]) + (accumulator[3][0] + accumulator[3][1])))
        + tail
}

/// `Σ |d - m|` over `w` in storage order on FOUR lanes: four named accumulators over chunks of four,
/// the remainder one by one, then `((a0 + a1) + (a2 + a3)) + tail`. A fixed slot to lane map keeps
/// every caller bit-identical; the lanes are four named locals because an array of them indexed in a
/// loop was kept in memory (measured: 9.8 ns against 6.3 ns for a ring of 20). Not
/// [`absolute_deviation_sum_eight_lanes`] (different last bits, each pinned).
#[inline]
pub fn absolute_deviation_sum_four_lanes(w: &[f64], m: f64) -> f64 {
    let (mut a0, mut a1, mut a2, mut a3) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    let (chunks, remainder) = w.as_chunks::<4>();
    for c in chunks {
        a0 += (c[0] - m).abs();
        a1 += (c[1] - m).abs();
        a2 += (c[2] - m).abs();
        a3 += (c[3] - m).abs();
    }
    let mut tail = 0.0_f64;
    for &d in remainder {
        tail += (d - m).abs();
    }
    ((a0 + a1) + (a2 + a3)) + tail
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every golden below is preceded by an `assert_ne!` against the naive fold it must beat, so a
    /// case that has stopped discriminating fails LOUDLY instead of passing for the wrong reason —
    /// the shape `crates/vike-core/tests/integration/drawdown_measures_own_pnl.rs` already uses. Assertions are
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
        assert_eq!(compensated_sum(xs).to_bits(), 1.0_f64.to_bits());
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
        assert_eq!(compensated_sum(xs).to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn an_infinite_sum_is_not_turned_into_a_nan_by_its_own_compensation() {
        // `c` becomes NaN the moment a term is infinite — `(inf - inf)` — and adding that back
        // would report NaN for a sum that is honestly infinite. `c.is_finite()` is what stops it,
        // and this is the case that fails if that half of the guard is deleted.
        let got = compensated_sum([f64::INFINITY, 1.0]);
        assert!(got.is_infinite() && got.is_sign_positive(), "got {got}");
        assert_eq!(compensated_sum([f64::NEG_INFINITY, 1.0]), f64::NEG_INFINITY);
    }

    #[test]
    fn a_nan_term_still_produces_a_nan() {
        // The guard must not launder a NaN INPUT into a finite answer — it only refuses to let a
        // NaN COMPENSATION spoil a good running total.
        assert!(compensated_sum([f64::NAN, 1.0]).is_nan());
        assert!(compensated_sum([1.0, f64::NAN]).is_nan());
    }

    #[test]
    fn an_empty_sum_is_positive_zero_and_a_single_term_is_itself() {
        // CPython's `sum(())` is `0`, positive. The bit check is the point: `-0.0` would compare
        // equal under `==` and is a different value to every consumer that divides by it.
        assert_eq!(compensated_sum([]).to_bits(), 0.0_f64.to_bits());
        // ⚠ `-0.0` is NOT in this list — see the test below. Everything else passes through.
        for x in [1.0, -1.0, 0.5, f64::MIN_POSITIVE, f64::MAX] {
            assert_eq!(
                compensated_sum([x]).to_bits(),
                x.to_bits(),
                "a single term must pass through: {x}"
            );
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
        assert_eq!(compensated_sum([-0.0]).to_bits(), 0.0_f64.to_bits());
        assert_ne!(compensated_sum([-0.0]).to_bits(), (-0.0_f64).to_bits());
        // ⚠ And NO number of them changes that, which is the stronger statement: `(-0.0) + (-0.0)`
        // is the one case IEEE keeps negative, but the accumulator is `+0.0` before the first term
        // and `+0.0` after it, so that pairing is never reached from this signature. This is the
        // PROOF of the equivalent mutant named at the top of this module — `f_result` cannot be
        // `-0.0`, so the `c != 0.0` half of the closing guard has no reachable effect.
        assert_eq!(compensated_sum([-0.0, -0.0]).to_bits(), 0.0_f64.to_bits());
        assert_eq!(compensated_sum([-0.0, -0.0, -0.0]).to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn ordinary_sums_are_unchanged_by_the_compensation() {
        // Compensation must be INVISIBLE where a naive fold is already exact, or every ported site
        // would shift the moment it started using this function. Powers of two below 2^53 add
        // exactly, so both folds must agree bit for bit.
        for xs in [vec![1.0, 2.0, 4.0], vec![0.25, 0.5, 0.25], vec![-3.0, 1.0, 2.0]] {
            assert_eq!(
                compensated_sum(xs.iter().copied()).to_bits(),
                naive(&xs).to_bits(),
                "exact inputs must fold identically: {xs:?}"
            );
        }
    }
}
