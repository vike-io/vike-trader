use super::*;

fn panel(periods: usize, symbols: usize, values: &[f64]) -> Matrix {
    Matrix::from_row_major(periods, symbols, values.to_vec()).unwrap()
}

fn close(got: &[f64], want: &[f64], tolerance: f64) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= tolerance, "{got:?} vs {want:?}");
    }
}

/// `Σₛ f · r` per period and strategy; a missing forecast (NaN) adds nothing, and so does a missing
/// return.
#[test]
fn an_alpha_return_is_the_forecast_weighted_realized_return() {
    let realized = panel(2, 2, &[0.01, -0.02, 0.03, f64::NAN]);
    let a = panel(2, 2, &[1.0, 2.0, -1.0, 5.0]);
    let b = panel(2, 2, &[f64::NAN, -1.0, 2.0, 0.5]);
    let alphas = alpha_returns(&[a, b], &realized).unwrap();
    assert_eq!((alphas.rows(), alphas.columns()), (2, 2));
    // period 0: a = 0.01 − 0.04, b = 0.02; period 1: a = −0.03, b = 0.06
    close(alphas.values(), &[0.01 - 0.04, 0.02, -0.03, 0.06], 1e-15);
    let other = panel(1, 2, &[1.0, 1.0]);
    assert!(alpha_returns(&[other], &realized).is_none(), "a panel of another shape");
    assert!(alpha_returns(&[], &realized).is_none());
}

/// Two uncorrelated alphas: the tangency weights are `μᵢ / σᵢ²` normalised. Alpha A is
/// `[3, 1, 3, 1]` (mean 2, variance 4/3), B `[2, 2, 0, 0]` (mean 1, variance 4/3), their centred
/// products sum to zero, so the weights are `2 : 1`.
#[test]
fn mean_variance_combining_by_hand() {
    let alphas = panel(4, 2, &[3.0, 2.0, 1.0, 2.0, 3.0, 0.0, 1.0, 0.0]);
    close(&mean_variance_weights(&alphas, false).unwrap(), &[2.0 / 3.0, 1.0 / 3.0], 1e-15);
    close(&mean_variance_weights(&alphas, true).unwrap(), &[2.0 / 3.0, 1.0 / 3.0], 1e-9);
    // The weights follow their strategies when the order changes.
    let swapped = panel(4, 2, &[2.0, 3.0, 2.0, 1.0, 0.0, 3.0, 0.0, 1.0]);
    close(&mean_variance_weights(&swapped, false).unwrap(), &[1.0 / 3.0, 2.0 / 3.0], 1e-15);
    // One strategy twice is no covariance to invert.
    let twice = panel(4, 2, &[3.0, 3.0, 1.0, 1.0, 3.0, 3.0, 1.0, 1.0]);
    assert!(mean_variance_weights(&twice, false).is_none());
}

/// Four alphas in two blocks: within a block they share one Walsh pattern (correlation 0.6), across
/// blocks every centred product sums to zero. The silhouette cuts the blocks, and Nested Clustered
/// Optimisation on a block-diagonal covariance with the blocks as clusters is the unnested tangency.
#[test]
fn hierarchical_combining_is_the_tangency_on_block_diagonal_alphas() {
    let walsh = [
        [1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0],
        [1.0, 1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0],
        [1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0],
        [1.0, 1.0, -1.0, -1.0, -1.0, -1.0, 1.0, 1.0],
    ];
    let means = [0.5, 0.25, 0.375, 0.125];
    let column = |k: usize, t: usize| {
        let (shared, own) = if k < 2 { (0, 1) } else { (2, 3) };
        let half = if k.is_multiple_of(2) { 0.5 } else { -0.5 };
        means[k] + walsh[shared][t] + half * walsh[own][t]
    };
    let values: Vec<f64> = (0..8).flat_map(|t| (0..4).map(move |k| column(k, t))).collect();
    let alphas = panel(8, 4, &values);
    let correlation = correlation_matrix(&alphas).unwrap();
    assert!((correlation.get(0, 1) - 0.6).abs() < 1e-12 && correlation.get(0, 2) == 0.0);
    assert_eq!(clusters_by_silhouette(&correlation, 3).unwrap(), vec![vec![0, 1], vec![2, 3]]);
    let nested = hierarchical_weights(&alphas, 3).unwrap();
    close(&nested, &mean_variance_weights(&alphas, false).unwrap(), 1e-12);
    close(&[nested.iter().fold(0.0, |total, w| total + w)], &[1.0], 1e-12);
    let pair: Vec<f64> = (0..8).flat_map(|t| (0..2).map(move |k| column(k, t))).collect();
    assert!(hierarchical_weights(&panel(8, 2, &pair), 3).is_none(), "two admit no cut");
}

/// The pool's forecast is the weighted sum where every panel said something; one missing is none.
#[test]
fn the_combined_forecast_is_the_weighted_sum_where_every_panel_said_something() {
    let a = panel(1, 3, &[1.0, f64::NAN, f64::NAN]);
    let b = panel(1, 3, &[3.0, 2.0, f64::NAN]);
    let pool = combined_forecast(&[a.clone(), b], &[0.25, 0.75]).unwrap();
    assert_eq!(pool.values()[0], 0.25 + 2.25);
    assert!(pool.values()[1].is_nan(), "one strategy said nothing there: no partial pool");
    assert!(pool.values()[2].is_nan(), "no strategy said anything there");
    assert!(combined_forecast(&[a], &[0.5, 0.5]).is_none(), "one weight per panel");
}

/// REVIEW-LIVE-MONEY M4: two correlated alphas under tangency weights (2.5, −1.5). A silent member
/// silences the pool: never 2.5 × A's forecast (a 2.5× position) and never −1.5 × B's (a flipped
/// sign). Both speaking is the whole weighted sum, the panel's cell bit for bit.
#[test]
fn a_silent_member_silences_the_pool_and_never_scales_or_flips_it() {
    let pool = PoolWeights::new(vec![("a".to_string(), 2.5), ("b".to_string(), -1.5)]).unwrap();
    let (fa, fb) = (0.4, 0.3);
    let said =
        |a: Option<f64>, b: Option<f64>| pool.combine(|name| if name == "a" { a } else { b });
    assert_eq!(said(Some(fa), None), None, "B silent: not 2.5 × A");
    assert_eq!(said(None, Some(fb)), None, "A silent: not −1.5 × B, a flipped sign");
    assert_eq!(said(Some(fa), Some(f64::NAN)), None, "a non-finite answer is silence");
    let both = said(Some(fa), Some(fb)).expect("both spoke");
    assert_eq!(both.to_bits(), (0.0 + 2.5 * fa + -1.5 * fb).to_bits());
    let panels = [panel(1, 1, &[fa]), panel(1, 1, &[fb])];
    assert_eq!(
        combined_forecast(&panels, &[2.5, -1.5]).unwrap().get(0, 0).to_bits(),
        both.to_bits()
    );
}

/// A forecast that is the NEXT period's return has its information at lag 1, exactly, and none
/// that holds at lag 0; the half-life reads off a decaying curve.
#[test]
fn information_decay_finds_the_lag_a_forecast_was_about() {
    let r = [1.0, -2.0, 3.0, 0.0, 5.0, -1.0, 2.0, 4.0];
    let realized = panel(8, 1, &r);
    let mut f: Vec<f64> = r[1..].to_vec();
    f.push(f64::NAN);
    let forecast = panel(8, 1, &f);
    let curve = information_decay(&forecast, &realized, 3).unwrap();
    assert_eq!(curve.len(), 4);
    assert!((curve[1] - 1.0).abs() < 1e-12, "{curve:?}");
    assert!(curve[0] < 0.5, "{curve:?}");
    assert!(information_decay(&forecast, &panel(1, 1, &[1.0]), 1).is_none());

    assert_eq!(decay_half_life(&[0.1, 0.08, 0.05, 0.02]), Some(2));
    assert_eq!(decay_half_life(&[0.1, 0.09, 0.08]), None, "never halves");
    assert_eq!(decay_half_life(&[-0.1, -0.2]), None, "no information to decay");
    assert_eq!(decay_half_life(&[f64::NAN, 0.0]), None);
}

/// The coefficient per stretch of time: a forecast right in the first stretch and inverted in the
/// second tracks +1 then −1.
#[test]
fn information_by_window_tracks_the_edge_through_time() {
    let r = [1.0, -1.0, 2.0, -3.0, 1.0, 2.0];
    let f = [1.0, -1.0, 2.0, 3.0, -1.0, -2.0];
    let track = information_by_window(&panel(6, 1, &f), &panel(6, 1, &r), 3).unwrap();
    close(&track, &[1.0, -1.0], 1e-12);
    assert!(information_by_window(&panel(6, 1, &f), &panel(6, 1, &r), 0).is_none());
}

/// The pool's weights as TEXT: written and read back to the same bits, in the members' order.
#[test]
fn pool_weights_round_trip_through_their_text() {
    let weights = PoolWeights::new(vec![
        ("trend".to_string(), 1.25),
        ("1767225600-ab12-0".to_string(), -0.375),
        ("hold".to_string(), 0.1 + 0.2),
    ])
    .unwrap();
    let text = weights.to_text();
    assert_eq!(
        text,
        "vike-alpha-pool v1\ntrend\t1.25\n1767225600-ab12-0\t-0.375\nhold\t0.30000000000000004\n"
    );
    assert_eq!(PoolWeights::parse(&text).unwrap(), weights, "the same bits back");
    assert_eq!(PoolWeights::parse(&text.replace('\n', "\r\n")).unwrap(), weights);
}

/// Every refusal is in words: another format line, a line with no tab, a weight that is no number, an
/// empty pool, a name twice, a non-finite weight.
#[test]
fn a_pool_text_that_breaks_the_rule_is_refused_in_words() {
    let refused = |text: &str| PoolWeights::parse(text).unwrap_err();
    assert!(refused("vike-alpha-pool v2\na\t1\n").contains("not a pool's weights"));
    assert!(refused("vike-alpha-pool v1\na 1\n").contains("line 2"));
    assert!(refused("vike-alpha-pool v1\na\tone\n").contains("weight"));
    assert!(refused("vike-alpha-pool v1\n").contains("at least one member"));
    assert!(refused("vike-alpha-pool v1\na\t1\na\t2\n").contains("twice"));
    assert!(refused("vike-alpha-pool v1\na\tNaN\n").contains("finite"));
    assert!(PoolWeights::new(vec![("a\tb".to_string(), 1.0)]).is_err());
}

/// A strategy file combining its members' forecasts with the pool's text gets the bits
/// [`combined_forecast`] gives the same cell; a member that said nothing silences both; none said is none.
#[test]
fn combining_from_the_text_is_the_panel_combination_of_one_cell() {
    let weights = [0.75, -0.125, 0.5];
    let pool = PoolWeights::new(
        ["a", "b", "c"].iter().zip(weights).map(|(n, w)| (n.to_string(), w)).collect(),
    )
    .unwrap();
    let cells = [0.3, 0.2, -0.7];
    let panels: Vec<Matrix> = cells.iter().map(|v| panel(1, 1, &[*v])).collect();
    let expected = combined_forecast(&panels, &weights).unwrap().get(0, 0);
    let got = pool.combine(|name| match name {
        "a" => Some(0.3),
        "b" => Some(0.2),
        _ => Some(-0.7),
    });
    assert_eq!(got.map(f64::to_bits), Some(expected.to_bits()));
    let silent_b: Vec<Matrix> = [0.3, f64::NAN, -0.7].iter().map(|v| panel(1, 1, &[*v])).collect();
    assert!(combined_forecast(&silent_b, &weights).unwrap().get(0, 0).is_nan());
    assert_eq!(pool.combine(|name| (name != "b").then_some(0.3)), None);
    assert_eq!(pool.combine(|_| None), None);
}
