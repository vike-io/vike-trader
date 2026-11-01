//! Property tests of `ContractValue::in_account`, the one function that turns a symbol's money
//! (quoted in ITS currency) into the ACCOUNT's (the multi-currency seam: every money product is
//! linear in `per_unit`, so scaling it converts the notional, the margin and both PnLs at once).
//!
//! The laws, none of which widens a tolerance of the frozen parity suites (`fill_parity.rs` and
//! `scalar_props.rs` run unedited and prove par is the old arithmetic):
//!
//! 1. PAR IS IDENTITY, to the bit, through the whole fill fold: `in_account(1.0)` is the value itself.
//! 2. RELABEL INVARIANCE (metamorphic): a symbol quoted in a foreign currency at a constant rate `r`
//!    books the same account-currency numbers as an account-quoted symbol whose every price is
//!    multiplied by `r`. The two sides associate the same product differently, so they agree to a
//!    few ULP of the PRODUCT'S SCALE (`SCALE_TOL`, derived below), not exactly.
//! 3. COMPOSITION and ROUND TRIP: converting by `a` then `b` is converting by `a·b`, and by `r` then
//!    `1/r` is the value back (the orientation law the node's rate builder is tested against).
//! 4. FAIL CLOSED: a rate that is not finite and positive is `None` for every value, never par.

use proptest::prelude::*;
use vike_model::{ContractValue, NotionalKind, compute_fill_value};

/// Two evaluations of one product that differ only in association agree within a few ULP of the
/// product's scale (`|p|·|q|·u·r`): `f64::EPSILON` is 2.2e-16, so 1e-12 is ~4,500 ULP, orders of
/// magnitude above any honest rounding and orders below a real conversion error (a 0.01% one is 1e-4).
const SCALE_TOL: f64 = 1e-12;

fn value() -> impl Strategy<Value = ContractValue> {
    prop_oneof![
        (1e-3f64..1e3).prop_map(ContractValue::linear),
        (1e-3f64..1e3).prop_map(ContractValue::inverse),
    ]
}

fn linear_unit() -> impl Strategy<Value = ContractValue> {
    (1e-3f64..1e3).prop_map(ContractValue::linear)
}

proptest! {
    /// LAW 1. Par through the real fold: every field of the outcome is bit-identical.
    #[test]
    fn par_is_the_identity_through_the_fill_fold(
        v in value(),
        prior_size in -1e4f64..1e4,
        prior_avg in 1e-2f64..1e5,
        side in prop_oneof![Just(1i32), Just(-1i32)],
        qty in 1e-3f64..1e4,
        px in 1e-2f64..1e5,
    ) {
        let par = v.in_account(1.0).expect("par is always representable");
        prop_assert_eq!(par.kind, v.kind);
        prop_assert_eq!(par.per_unit.to_bits(), v.per_unit.to_bits());
        let a = compute_fill_value(prior_size, prior_avg, side, qty, px, v);
        let b = compute_fill_value(prior_size, prior_avg, side, qty, px, par);
        prop_assert_eq!(a.new_size.to_bits(), b.new_size.to_bits());
        prop_assert_eq!(a.new_avg_px.to_bits(), b.new_avg_px.to_bits());
        prop_assert_eq!(a.realized_pnl.to_bits(), b.realized_pnl.to_bits());
        prop_assert_eq!(a.closing_qty.to_bits(), b.closing_qty.to_bits());
        // and through every other table row
        prop_assert_eq!(par.order_notional(qty, px).to_bits(), v.order_notional(qty, px).to_bits());
        prop_assert_eq!(
            par.unrealized_pnl(prior_avg, px, prior_size).to_bits(),
            v.unrealized_pnl(prior_avg, px, prior_size).to_bits()
        );
    }

    /// LAW 2a. A JPY-quoted linear symbol at rate `r` closes with the PnL of an account-quoted symbol
    /// whose prices are `r` times as large.
    #[test]
    fn a_foreign_quoted_fill_books_what_the_relabelled_account_quoted_fill_books(
        v in linear_unit(),
        r in 1e-4f64..1e3,
        size in 1e-2f64..1e4,
        entry in 1e-2f64..1e5,
        exit in 1e-2f64..1e5,
    ) {
        let converted = v.in_account(r).expect("a sane rate");
        // close the long entirely: side -1, the whole size
        let foreign = compute_fill_value(size, entry, -1, size, exit, converted);
        let relabelled = compute_fill_value(size, entry * r, -1, size, exit * r, v);
        let scale = (entry.abs() + exit.abs()) * size * v.per_unit * r;
        prop_assert!(
            (foreign.realized_pnl - relabelled.realized_pnl).abs() <= SCALE_TOL * scale,
            "{} vs {} (scale {})", foreign.realized_pnl, relabelled.realized_pnl, scale
        );
        // the unrealized twin at the mark, and the notional and the margin the gate reads
        let (mark, qty) = (exit, size);
        let u_f = converted.unrealized_pnl(entry, mark, qty);
        let u_r = v.unrealized_pnl(entry * r, mark * r, qty);
        prop_assert!((u_f - u_r).abs() <= SCALE_TOL * scale);
        let n_f = converted.order_notional(qty, mark);
        let n_r = v.order_notional(qty, mark * r);
        prop_assert!((n_f - n_r).abs() <= SCALE_TOL * n_f.abs().max(n_r.abs()));
        let m_f = converted.initial_margin(mark, qty, 1.0, 0.05);
        let m_r = v.initial_margin(mark * r, qty, 1.0, 0.05);
        prop_assert!((m_f - m_r).abs() <= SCALE_TOL * m_f.abs().max(m_r.abs()));
    }

    /// LAW 3a. Composition: `(v · a) · b == v · (a·b)`.
    #[test]
    fn conversions_compose(v in value(), a in 1e-3f64..1e3, b in 1e-3f64..1e3) {
        let two_steps = v.in_account(a).and_then(|x| x.in_account(b)).expect("sane rates");
        let one_step = v.in_account(a * b).expect("sane rate");
        prop_assert_eq!(two_steps.kind, one_step.kind);
        prop_assert!(
            (two_steps.per_unit - one_step.per_unit).abs() <= SCALE_TOL * one_step.per_unit
        );
    }

    /// LAW 3b. The orientation law: converting by `r` and then by `1/r` is the value back. A flipped
    /// rate is NOT inside this identity (`150` is not `1/150`), which is exactly why the engine
    /// keeps a jump cap on a rate's step (`vike-exec`'s quote-rate tests).
    #[test]
    fn a_rate_and_its_reciprocal_cancel(v in value(), r in 1e-3f64..1e3) {
        let back = v.in_account(r).and_then(|x| x.in_account(1.0 / r)).expect("sane rates");
        prop_assert_eq!(back.kind, v.kind);
        prop_assert!((back.per_unit - v.per_unit).abs() <= SCALE_TOL * v.per_unit);
        let flipped = v.in_account(r).and_then(|x| x.in_account(r)).expect("sane rates");
        if (r - 1.0).abs() > 1e-6 {
            prop_assert!(
                (flipped.per_unit - v.per_unit).abs() > 1e-9 * v.per_unit,
                "applying the rate twice must not look like the round trip"
            );
        }
    }

    /// LAW 4. Fail closed, for every value and every degenerate rate.
    #[test]
    fn a_degenerate_rate_is_refused_for_every_value(
        v in value(),
        bad in prop_oneof![
            Just(0.0f64), Just(-0.0f64), Just(f64::NAN), Just(f64::INFINITY),
            Just(f64::NEG_INFINITY), (-1e6f64..-1e-9),
        ],
    ) {
        prop_assert_eq!(v.in_account(bad), None);
    }

    /// A good rate on a good base is always a positive finite factor with the kind kept.
    #[test]
    fn a_sane_rate_always_gives_a_positive_finite_unit(v in value(), r in 1e-6f64..1e6) {
        let out = v.in_account(r).expect("representable");
        prop_assert!(out.per_unit.is_finite() && out.per_unit > 0.0);
        prop_assert_eq!(out.kind, v.kind);
        prop_assert!(matches!(out.kind, NotionalKind::Linear | NotionalKind::Inverse));
    }
}
