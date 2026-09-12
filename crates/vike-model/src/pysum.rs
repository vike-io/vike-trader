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
