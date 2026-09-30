use super::*;

#[test]
fn min_hold_locks_a_new_entry_for_n_bars() {
    // Enter long at 0, signal drops to 0 at 1, but a 2-bar hold keeps it.
    let out = apply_min_hold(&[1, 0, 0, 0], 2);
    assert_eq!(out, vec![1, 1, 0, 0]);
}

#[test]
fn min_hold_of_one_bar_changes_nothing() {
    let src = [1, 0, -1, -1];
    assert_eq!(apply_min_hold(&src, 1), src.to_vec());
}

#[test]
fn a_flip_inside_the_lock_window_is_suppressed() {
    // Bar 0 enters long and locks bars 0..1, so bar 1's -1 never takes effect.
    // A min-hold any opposite signal could break would not be a min-hold.
    assert_eq!(apply_min_hold(&[1, -1, 0, 0], 2), vec![1, 1, 0, 0]);
}

#[test]
fn min_hold_locks_a_flip_too() {
    // Bar 0 locks 0..1; bar 2 flips once that lock has expired and locks 2..3.
    assert_eq!(apply_min_hold(&[1, 0, -1, 0], 2), vec![1, 1, -1, -1]);
}

#[test]
fn the_entry_check_reads_the_held_position_not_the_original_signal() {
    // Bar 0's lock forces bar 1 to +1. Bar 2 is therefore a flip against the position
    // ACTUALLY HELD — which reading the original input (-1 at bar 1) would hide, since
    // -1 -> -1 looks like no change. A mutant comparing positions[i-1] returns
    // [1, 1, -1, 0] here instead.
    assert_eq!(apply_min_hold(&[1, -1, -1, 0], 2), vec![1, 1, -1, -1]);
}

#[test]
fn pnl_uses_the_previous_bars_position_never_the_current_one() {
    // No lookahead: bar 1's return is earned by bar 0's position.
    let pnl = bar_pnl(&[1, 1], &[0.10, 0.20], 0.0);
    assert_eq!(pnl[0], 0.0, "bar 0 has no prior position");
    assert_eq!(pnl[1], 0.20);
}

#[test]
fn slippage_is_charged_on_every_position_change() {
    // 15 bps = 0.0015. Entering at bar 0 is a change from the implicit flat start.
    let pnl = bar_pnl(&[1, 1], &[0.0, 0.0], 15.0);
    assert!((pnl[0] - -0.0015).abs() < 1e-15);
    assert_eq!(pnl[1], 0.0, "no change, no charge");
}

#[test]
fn slippage_is_charged_on_an_exit_to_flat_too() {
    // Exiting to flat is a position change and is charged, same as an entry.
    let pnl = bar_pnl(&[1, 0], &[0.0, 0.0], 15.0);
    assert!((pnl[1] - -0.0015).abs() < 1e-15);
}

#[test]
fn a_short_earns_the_negated_return() {
    let pnl = bar_pnl(&[-1, -1], &[0.0, 0.20], 0.0);
    assert_eq!(pnl[1], -0.20);
}

#[test]
fn equity_is_the_exponentiated_cumulative_log_pnl() {
    let eq = equity_from_pnl(&[0.0, 0.0]);
    assert_eq!(eq, vec![1.0, 1.0]);

    // A flat-PnL series can't distinguish cumulation from a dozen other implementations
    // (never accumulating, subtracting, a constant vec![1.0; n], exponentiating the mean —
    // all pass the case above too). This one only passes if the sum is genuinely running.
    let eq = equity_from_pnl(&[0.1, 0.2]);
    assert!((eq[0] - 0.1f64.exp()).abs() < 1e-15);
    assert!((eq[1] - 0.3f64.exp()).abs() < 1e-15); // cumulative, not per-bar
}

#[test]
fn trade_stats_counts_entries_and_flips_but_not_exits() {
    // flat -> long (1 trade) -> flat -> short (2nd trade)
    let stats = trade_stats(&[1, 1, 0, -1], &[0.1, 0.1, 0.0, -0.1]);
    assert_eq!(stats.n_trades, 2);
}

#[test]
fn n_trades_counts_a_direct_flip_not_just_entries_from_flat() {
    // The fixture above is two entries FROM FLAT; a mutant counting only those still returns
    // 2 there. This fixture has one entry and one DIRECT flip (no flat bar in between).
    assert_eq!(trade_stats(&[1, -1, -1, 0], &[0.1; 4]).n_trades, 2); // entry + direct flip
}

#[test]
fn avg_holding_bars_and_avg_pnl_are_computed_correctly() {
    let s = trade_stats(&[1, 1, 0, -1], &[0.1, 0.1, 0.0, -0.1]);
    assert_eq!(s.avg_holding_bars, 1.5); // runs of 2 and 1
    assert!((s.avg_pnl - 0.1 / 3.0).abs() < 1e-15); // three non-flat bars
}

#[test]
fn win_rate_ignores_flat_bars() {
    // Only the three non-flat bars count; two are positive.
    let stats = trade_stats(&[1, 1, 0, 1], &[0.1, -0.1, 99.0, 0.1]);
    assert!((stats.win_rate - 2.0 / 3.0).abs() < 1e-15);
}

#[test]
fn sharpe_uses_the_sample_sd_not_the_population_one() {
    // [1,2,3,4]: mean 2.5, sample var 5/3, population var 5/4. The survival gate is
    // `sharpe >= 0.5`, and the population form is larger by sqrt(n/(n-1)) — enough to flip a
    // borderline candidate on a short series.
    let s = sharpe_from_bar_pnl(&[1.0, 2.0, 3.0, 4.0], 1.0);
    let expected = 2.5 / (5.0f64 / 3.0).sqrt();
    assert!((s - expected).abs() < 1e-12, "got {s}, want {expected}");
}

#[test]
fn a_flat_series_scores_zero_rather_than_infinity() {
    assert_eq!(sharpe_from_bar_pnl(&[0.01; 50], 8760.0), 0.0);
}

#[test]
fn fewer_than_two_observations_score_zero() {
    // ⚠ A DECLARED DIVERGENCE at n == 1, not a parity claim. The oracle guards on
    // `len(s) == 0 or s.std() == 0`; with exactly one observation pandas' `std()` is NaN
    // (ddof = 1, zero degrees of freedom), `NaN == 0` is False, so the oracle falls through and
    // returns `mean / NaN` = NaN. This returns 0.0. Both fail the `sharpe >= 0.5` survival gate
    // and neither reaches a stored per-rule row (a one-bar fold produces no run), so the
    // difference is unobservable downstream — but it is a difference, and calling it parity
    // would be false.
    assert_eq!(sharpe_from_bar_pnl(&[], 8760.0), 0.0);
    assert_eq!(sharpe_from_bar_pnl(&[0.5], 8760.0), 0.0, "oracle: NaN here; see the comment");
}

#[test]
fn the_signed_drawdown_is_negative_where_the_positive_one_is_positive() {
    // The oracle returns `dd.min()`; `metrics::max_drawdown` returns the same magnitude with the
    // opposite sign. Both are correct in their own vocabulary — the parity gate needs the
    // oracle's.
    let eq = [1.0, 1.25, 1.0, 1.5];
    let signed = max_drawdown_signed(&eq);
    assert!((signed - (-0.2)).abs() < 1e-15, "got {signed}");
    assert!(
        (signed + crate::metrics::max_drawdown(&eq)).abs() < 1e-15,
        "the two must be exact negatives of each other"
    );
}

#[test]
fn a_monotonically_rising_curve_has_no_drawdown_and_an_empty_one_is_zero() {
    assert_eq!(max_drawdown_signed(&[1.0, 1.1, 1.2]), 0.0);
    assert_eq!(max_drawdown_signed(&[]), 0.0, "the oracle's `len(eq) == 0` guard");
}

#[test]
fn nans_are_dropped_not_propagated() {
    let a = sharpe_from_bar_pnl(&[0.1, -0.2, 0.3], 8760.0);
    let b = sharpe_from_bar_pnl(&[0.1, f64::NAN, -0.2, 0.3], 8760.0);
    assert_eq!(a, b);
}

#[test]
fn it_disagrees_with_the_equity_curve_sharpe_which_is_the_whole_point() {
    // This test exists so that substituting `metrics::sharpe` — which compiles, because the
    // shapes match — fails loudly instead of silently changing the headline number.
    let pnl = [0.01, -0.02, 0.03, 0.005];
    let eq = equity_from_pnl(&pnl);
    assert_ne!(sharpe_from_bar_pnl(&pnl, 8760.0), crate::metrics::sharpe(&eq, 8760.0));
}

// ---- the frictioned mini-backtest scorer --------------------------------------------------

/// A [`SharpeObjective`] at the cohort study's own defaults except the two knobs a test varies.
fn sharpe_obj(bar_returns: &[f64], slippage_bps: f64, min_hold_bars: usize) -> SharpeObjective<'_> {
    SharpeObjective {
        bar_returns,
        bands: HysteresisBands::default(),
        min_hold_bars,
        slippage_bps,
        periods_per_year: 8_760.0,
    }
}

/// ⚠ THE reason this scorer is a function rather than three composed calls, stated as the
/// ranking it protects: a candidate that never opened a position must rank BELOW one that
/// traded and lost money. The second half of the test is the bug — the shared kernel's own
/// answer for the degenerate slice, negated, sorting ABOVE the real loss.
#[test]
fn a_degenerate_candidate_never_outranks_one_that_traded_and_lost() {
    let lr = vec![-0.001; 40]; // a downward drift: a steady long really does lose
    let s = sharpe_obj(&lr, 1.5, 2);

    // Traded and lost: every probability above the long entry band, so it enters at bar 0.
    let loser = neg_sharpe_mini_backtest(&[0.9; 40], &s);
    assert!(loser.is_finite(), "the losing fixture scored degenerate — it tests nothing");
    assert!(loser > 0.0, "the losing fixture made money — it tests nothing");

    // No evidence: every probability inside the dead zone, so it never opens a position.
    let degenerate = neg_sharpe_mini_backtest(&[0.5; 40], &s);
    assert!(
        degenerate > loser,
        "a candidate that never traded ({degenerate}) outranked one that traded and lost \
             ({loser}) — in a ranked leaderboard that puts a strategy which never opened a \
             position above strategies that traded"
    );

    // ...and this is what it would score without the rule. `sharpe_from_bar_pnl` answers
    // `0.0` for a series with fewer than two cleaned points or zero dispersion — right for
    // its threshold-comparing callers, exactly wrong for an argmin.
    let flat = apply_min_hold(&hysteresis(&[0.5; 40], s.bands), s.min_hold_bars);
    assert!(flat.iter().all(|p| *p == 0), "the degenerate fixture traded — it tests nothing");
    let naive = -sharpe_from_bar_pnl(&bar_pnl(&flat, &lr, s.slippage_bps), s.periods_per_year);
    assert_eq!(naive, 0.0, "the kernel's own answer, which an argmin must never receive");
    assert!(naive < loser, "the naive composition already ranks correctly — nothing to guard");
}

/// The zero-trade rule, stated as the difference it exists to create: for an all-flat slice
/// the shared kernel answers `0.0` (its threshold-caller contract) while the scorer answers
/// `INFINITY` — explicitly, not through NaN luck.
#[test]
fn an_all_flat_slice_scores_worst_explicitly_not_zero() {
    // Every probability in the dead zone: no candidate bar ever enters.
    let proba = vec![0.5; 101];
    let lr = vec![0.001; 101];
    let s = sharpe_obj(&lr, 1.5, 2);
    let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
    assert!(held.iter().all(|p| *p == 0), "the fixture entered — it tests nothing");
    assert_eq!(
        sharpe_from_bar_pnl(&bar_pnl(&held, &lr, s.slippage_bps), s.periods_per_year),
        0.0,
        "the kernel's own answer, which an argmin must never receive"
    );
    assert_eq!(neg_sharpe_mini_backtest(&proba, &s), f64::INFINITY);

    // An all-NaN candidate is the same case through the same rule: hysteresis holds flat on
    // NaN (a live walk's own semantics), so it never trades and scores worst.
    assert_eq!(neg_sharpe_mini_backtest(&[f64::NAN; 101], &s), f64::INFINITY);
}

/// "Too few bars for a defined Sharpe": a one-row slice trades but has no sample (`ddof = 1`)
/// deviation, and an empty slice (a caller's cut clamped to the end of its rows) is not a
/// panic.
#[test]
fn a_slice_too_short_for_a_sample_deviation_scores_worst() {
    assert_eq!(neg_sharpe_mini_backtest(&[0.9], &sharpe_obj(&[0.001], 1.5, 2)), f64::INFINITY);
    assert_eq!(neg_sharpe_mini_backtest(&[], &sharpe_obj(&[], 1.5, 2)), f64::INFINITY);
}

/// A candidate can TRADE and still have no Sharpe: a constant return exactly equal to minus
/// the entry slippage makes every PnL bar identical, so the sample deviation is exactly zero.
/// This is the one degenerate case the trade check cannot see, which is also why the variance
/// check is the guard that catches the all-flat case if the (implied) trade check is ever
/// simplified away — see [`neg_sharpe_mini_backtest`]'s doc for the implication.
#[test]
fn a_zero_variance_pnl_scores_worst_even_when_the_candidate_trades() {
    let slip = 10.0 / 10_000.0;
    let lr = vec![-slip; 40];
    let proba = vec![0.9; 40];
    let s = sharpe_obj(&lr, 10.0, 2);
    let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
    assert!(held.iter().any(|p| *p != 0), "the fixture never traded — it tests nothing");
    assert_eq!(neg_sharpe_mini_backtest(&proba, &s), f64::INFINITY);
}

/// A single-TRADE slice is NOT degenerate — one entry then holding gives `n - 1` market bars
/// and a defined variance — and its score IS the negated shared-kernel composition, which is
/// the "reuse the kernels above, never re-spell them" claim stated as an exact identity
/// (annualisation constant included).
#[test]
fn a_single_trade_slice_ranks_by_its_real_sharpe() {
    let lr: Vec<f64> = (0..40).map(|i| 0.001 * ((i as f64) * 0.37).sin()).collect();
    let proba = vec![0.9; 40];
    let s = sharpe_obj(&lr, 1.5, 2);
    let held = apply_min_hold(&hysteresis(&proba, s.bands), s.min_hold_bars);
    let want = -sharpe_from_bar_pnl(&bar_pnl(&held, &lr, 1.5), 8_760.0);
    let got = neg_sharpe_mini_backtest(&proba, &s);
    assert!(got.is_finite(), "a single-trade slice was scored degenerate");
    assert_eq!(got, want);
    assert_ne!(got, 0.0, "the fixture's Sharpe is zero — the identity proves nothing");
}

/// The SELECTION Sharpe and the REPORTED Sharpe must annualise identically, and
/// [`SharpeObjective::for_interval`] is what makes that structural instead of remembered.
///
/// ⚠ The assertion that matters is the FIRST one, and it is written against the authority
/// rather than a copied number on purpose: a literal here would pass while the authority moved,
/// which is the exact failure the constructor exists to prevent. The rest are anti-vacuity —
/// without them this test would pass against a constructor that ignored `interval` entirely and
/// returned a constant, which is the defect wearing the fix's clothes.
#[test]
fn the_objective_annualises_off_the_interval_through_the_reports_own_authority() {
    let ppy = |iv: &str| {
        let o = SharpeObjective::for_interval(&[], iv, HysteresisBands::default(), 0, 0.0);
        o.periods_per_year
    };
    // An UNPARSEABLE interval rides along deliberately: the fallback must be the AUTHORITY's,
    // not a second answer invented here to a question `report` already owns.
    for iv in ["1m", "1h", "4h", "1d", "not-an-interval"] {
        let want = crate::report::periods_per_year_for_interval(iv);
        assert_eq!(ppy(iv), want, "{iv}: the selection constant must BE the reported one");
    }
    // ...and it genuinely READS the interval: two bar sizes must not annualise the same, or
    // the loop above is comparing a constant with itself and would pass unchanged against a
    // constructor that ignored its argument.
    assert!(ppy("1h") > ppy("1d"), "more bars per year, not fewer");
}
