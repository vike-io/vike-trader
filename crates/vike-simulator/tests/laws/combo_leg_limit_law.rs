//! The per-leg law, backtest side: a combo priced by its LEGS ([`vike_model::OrderComboLeg::limit`]) fills
//! through [`vike_model::combo_legs_cross`], the one law the paper book shares. Pinned:
//!
//! 1. it fills only on an event at which EVERY leg crosses its own limit (a bought leg's ask at or
//!    below it, a sold leg's bid at or above it), all or nothing, each leg at the price the law
//!    returns, booked as a maker;
//! 2. on ticks a leg that crossed at an earlier ts is no price at a later one under the strict
//!    bound: the legs must cross on one fresh event;
//! 3. a combo that mixes a net limit with leg limits, or states a limit on some legs only, is
//!    refused in the model's words, with nothing resting.

use vike_model::{ComboError, OrderComboLeg};

use crate::combo_law::{
    A, B, at, combo, fills_of, leg, q, qbar, refused, run_two, run_two_ticks, ts,
};
use crate::stop_limit_law::{assert_fill, params};

/// Buy one unit of (+1 A at or below 10.5, -1 B at or above 5.5).
fn legs() -> Vec<OrderComboLeg> {
    vec![leg(A, 1, Some(10.5)), leg(B, -1, Some(5.5))]
}

#[test]
fn it_fills_only_when_every_leg_crosses_on_one_event_each_at_the_laws_price_as_a_maker() {
    // step 1: A crosses (ask 10.4), B does not (bid 5.4); step 2: B crosses, A does not; step 3: both
    let quotes_a = [(9.9, 10.1), (10.3, 10.4), (10.5, 10.6), (10.1, 10.2)];
    let quotes_b = [(4.9, 5.1), (5.4, 5.5), (5.6, 5.7), (5.7, 5.8)];
    let a = (0..4).map(|i| qbar(i, quotes_a[i].0, Some(quotes_a[i]))).collect();
    let b = (0..4).map(|i| qbar(i, quotes_b[i].0, Some(quotes_b[i]))).collect();
    let spec = combo(1, 1.0, legs(), None);
    let engine = run_two(params(), a, b, vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    let law = vike_model::combo_legs_cross(1, &legs(), |s, ask| {
        let (bid, offer) = if s == A { quotes_a[3] } else { quotes_b[3] };
        if ask { offer } else { bid }
    })
    .expect("both legs cross on step 3");
    assert_eq!(law, vec![10.2, 5.7], "a bought leg at its ask, a sold leg at its bid");
    assert_eq!(
        fills_of(&engine),
        vec![(A.to_string(), ts(3), 1, 1.0, law[0]), (B.to_string(), ts(3), -1, 1.0, law[1])],
        "nothing on steps 1 and 2, where one leg alone crossed"
    );
    for f in &engine.strategy.fills {
        assert_fill("a leg-limit combo is a maker", f, ts(3), f.price, true);
    }
}

#[test]
fn on_ticks_the_legs_must_cross_on_one_fresh_event() {
    // A crosses at 1000 and B at 2000: under the strict bound A's mark is stale by then. Both
    // cross again at 3000.
    let a = vec![q(A, 500, 10.0, 11.0), q(A, 1_000, 10.0, 10.4), q(A, 3_000, 10.0, 10.3)];
    let b = vec![q(B, 2_000, 5.6, 6.0), q(B, 3_000, 5.5, 6.0)];
    let spec = combo(1, 1.0, legs(), None);
    let engine = run_two_ticks(0, a, b, vec![at(0, move |b| assert!(b.submit_combo(&spec)))]);
    assert_eq!(
        fills_of(&engine),
        vec![(A.to_string(), 3_000, 1, 1.0, 10.3), (B.to_string(), 3_000, -1, 1.0, 5.5)]
    );
}

#[test]
fn a_net_limit_beside_leg_limits_or_a_leg_without_one_is_refused_in_the_models_words() {
    let one_priced = vec![leg(A, 1, Some(10.5)), leg(B, -1, None)];
    let cases = [
        (combo(1, 1.0, legs(), Some(4.0)), ComboError::NetAndLegLimits),
        (combo(1, 1.0, one_priced, None), ComboError::MissingLegLimit(1)),
    ];
    for (spec, err) in cases {
        let words = err.to_string();
        let a = (0..2).map(|i| qbar(i, 10.0, None)).collect();
        let b = (0..2).map(|i| qbar(i, 5.0, None)).collect();
        let engine = run_two(params(), a, b, vec![at(0, move |b| assert!(!b.submit_combo(&spec)))]);
        assert!(refused(&engine.core, &words), "{words}: {:?}", engine.core.dropped);
        assert!(engine.strategy.fills.is_empty());
    }
}
