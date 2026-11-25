use vike_arbitrage::basis_and_funding::{
    CarryRule, funding_carry, funding_in_bars, last_settled_funding, log_basis,
};

const NAN: f64 = f64::NAN;

fn near(got: f64, want: f64) -> bool {
    (got - want).abs() <= 1e-12
}

#[test]
fn the_basis_by_hand() {
    let basis = log_basis(&[101.0, 102.0, -1.0, 100.0], &[100.0, 100.0, 100.0, NAN]).unwrap();
    assert!(near(basis[0], libm::log(1.01)) && near(basis[1], libm::log(1.02)), "{basis:?}");
    assert!(basis[2].is_nan(), "a price that is not positive has no basis");
    assert!(basis[3].is_nan(), "a missing price has none either");
    assert!(log_basis(&[1.0, 2.0], &[1.0]).is_none());
}

/// Bars of 100 starting at 0, 100, 200 and 300. A settlement belongs to the bar that contains its time, so
/// the one stamped exactly 100 and the one at 105 both fall in bar 1 (summed), and the one at 300 in bar 3.
#[test]
fn funding_settles_in_the_bar_that_contains_it() {
    let opens = [0, 100, 200, 300];
    let settlements = [(100, 0.001), (105, 0.0005), (300, 0.002)];
    let paid = funding_in_bars(&opens, 100, &settlements);
    assert_eq!(paid.len(), 4);
    assert_eq!(paid[0], 0.0);
    assert!(near(paid[1], 0.0015));
    assert_eq!(paid[2], 0.0);
    assert!(near(paid[3], 0.002));
}

/// What a decision at a bar's open can know is the settlement BEFORE the open: the one stamped exactly at bar
/// 1's open (100) has not been seen yet there, and bar 3 sees the one at 250, not the one at 100.
#[test]
fn the_last_settled_funding_is_strictly_before_the_open() {
    let known = last_settled_funding(&[0, 100, 200, 300], &[(100, 0.001), (250, 0.002)]);
    assert!(known[0].is_nan() && known[1].is_nan(), "{known:?}");
    assert_eq!(known[2], 0.001);
    assert_eq!(known[3], 0.002);
    assert!(last_settled_funding(&[0, 100], &[]).iter().all(|v| v.is_nan()));
}

/// By hand, gross (the costs are a backtest's). Entry above `0.0002`, exit below `0`. Bar 1: last funding `0.0003`
/// enters; income `0.0001`, basis `−(0.001 − 0) = −0.001`. Bar 2: `0.0003` is not below the exit, so it stays;
/// income `0.0001`, basis `−(0.0005 − 0.001) = +0.0005`. Bar 3: `−0.0001` exits; nothing is earned.
#[test]
fn the_carry_by_hand() {
    let basis = [0.0, 0.001, 0.0005, 0.0005];
    let income = [0.0, 0.0001, 0.0001, 0.0001];
    let last = [NAN, 0.0003, 0.0003, -0.0001];
    let rule = CarryRule { entry: 0.0002, exit: 0.0 };
    let result = funding_carry(&basis, &income, &last, rule).unwrap();
    assert_eq!(result.position, vec![0.0, 1.0, 1.0, 0.0]);
    let close = |got: &[f64], want: [f64; 4]| {
        assert!(got.iter().zip(want).all(|(g, w)| near(*g, w)), "{got:?} vs {want:?}");
    };
    close(&result.funding, [0.0, 0.0001, 0.0001, 0.0]);
    close(&result.basis, [0.0, -0.001, 0.0005, 0.0]);
    close(&result.net(), [0.0, -0.0009, 0.0006, 0.0]);
}

/// A bar with no basis (or after one) keeps its position and earns nothing, even where the rule would have left:
/// nothing is traded on a price that is not there.
#[test]
fn a_missing_basis_holds_the_position_and_earns_nothing() {
    let basis = [0.0, 0.001, NAN, 0.0005];
    let income = [0.0, 0.0001, 0.0001, 0.0001];
    let last = [NAN, 0.0003, -0.0001, -0.0001];
    let rule = CarryRule { entry: 0.0002, exit: 0.0 };
    let result = funding_carry(&basis, &income, &last, rule).unwrap();
    assert_eq!(result.position, vec![0.0, 1.0, 1.0, 1.0]);
    assert_eq!((result.funding[2], result.funding[3]), (0.0, 0.0));
    assert_eq!((result.basis[2], result.basis[3]), (0.0, 0.0));
}

#[test]
fn the_carry_has_no_answer_for_series_of_different_lengths() {
    let rule = CarryRule { entry: 0.0, exit: 0.0 };
    assert!(funding_carry(&[0.0, 1.0], &[0.0], &[0.0, 0.0], rule).is_none());
    assert!(funding_carry(&[0.0, 1.0], &[0.0, 0.0], &[0.0], rule).is_none());
}
