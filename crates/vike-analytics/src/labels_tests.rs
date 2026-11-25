//! Hand-computed cases for `labels`: every expected number below is worked from the published
//! formula in the comment beside it, never captured from a run.

use super::*;

fn close_enough(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * b.abs().max(1.0)
}

const CLOSE: [f64; 8] = [100.0, 101.0, 103.0, 99.0, 97.0, 104.0, 100.0, 100.5];

fn barriers(max_holding_bars: Option<usize>) -> TripleBarrierLabelling {
    TripleBarrierLabelling {
        profit_taking_multiple: 1.0,
        stop_loss_multiple: 1.0,
        max_holding_bars,
        minimum_target: 0.0,
    }
}

#[test]
fn triple_barrier_takes_the_first_touch_of_each_barrier() {
    let target = [0.025; 8];
    let labels = triple_barrier_labels(&CLOSE, &target, &[0, 1, 3, 5, 6], barriers(Some(4)), None);
    let summary: Vec<_> = labels.iter().map(|l| (l.start, l.end, l.label, l.touched)).collect();
    assert_eq!(
        summary,
        vec![
            // 0: +1% then +3% > 2.5% at bar 2.
            (0, 2, 1, BarrierTouched::Upper),
            // 1: +1.98%, -1.98%, then 97/101 - 1 = -3.96% < -2.5% at bar 4.
            (1, 4, -1, BarrierTouched::Lower),
            // 3: 97/99 - 1 = -2.02% stays inside, 104/99 - 1 = +5.05% at bar 5.
            (3, 5, 1, BarrierTouched::Upper),
            // 5: vertical at 9 is past the data; 100/104 - 1 = -3.85% at bar 6 still touches.
            (5, 6, -1, BarrierTouched::Lower),
            // 6: +0.5% at the last bar, no barrier: censored, read off the last bar.
            (6, 7, 1, BarrierTouched::EndOfData),
        ]
    );
    assert!(close_enough(labels[1].realized_return, 97.0 / 101.0 - 1.0));
}

#[test]
fn triple_barrier_vertical_barrier_closes_a_quiet_path() {
    let close = [100.0, 100.5, 100.2, 99.9, 100.1];
    let labels = triple_barrier_labels(&close, &[0.02; 5], &[0], barriers(Some(3)), None);
    assert_eq!(labels.len(), 1);
    assert_eq!(
        (labels[0].end, labels[0].touched, labels[0].label),
        (3, BarrierTouched::Vertical, -1)
    );
    assert!(close_enough(labels[0].realized_return, 99.9 / 100.0 - 1.0));
}

#[test]
fn a_disabled_barrier_is_never_touched_and_a_horizontal_touch_on_the_vertical_bar_wins() {
    // Profit-taking off: event 0 never takes the +3% at bar 2; -3% at bar 4 IS the vertical bar.
    let mut no_profit_taking = barriers(Some(4));
    no_profit_taking.profit_taking_multiple = 0.0;
    let labels = triple_barrier_labels(&CLOSE, &[0.025; 8], &[0], no_profit_taking, None);
    assert_eq!((labels[0].end, labels[0].touched, labels[0].label), (4, BarrierTouched::Lower, -1));
}

#[test]
fn meta_labels_score_the_side_and_skip_a_zero_side() {
    let target = [0.025; 8];
    let labels = triple_barrier_labels(
        &CLOSE,
        &target,
        &[0, 1, 3, 2],
        barriers(Some(4)),
        Some(&[-1, 1, 1, 0]),
    );
    let summary: Vec<_> = labels.iter().map(|l| (l.start, l.end, l.label, l.touched)).collect();
    assert_eq!(
        summary,
        vec![
            // short at 0: side return -3% at bar 2 is the STOP -> the bet lost, label 0.
            (0, 2, 0, BarrierTouched::Lower),
            // long at 1: stopped at bar 4 -> 0.
            (1, 4, 0, BarrierTouched::Lower),
            // long at 3: profit taken at bar 5 -> 1.
            (3, 5, 1, BarrierTouched::Upper),
        ]
    );
    assert!(close_enough(labels[0].realized_return, -(103.0 / 100.0 - 1.0)));
}

#[test]
fn a_target_at_or_below_the_minimum_or_nan_is_skipped() {
    let mut target = [0.025; 8];
    target[1] = 0.01;
    target[3] = f64::NAN;
    let mut with_minimum = barriers(Some(4));
    with_minimum.minimum_target = 0.01;
    let starts: Vec<usize> =
        triple_barrier_labels(&CLOSE, &target, &[0, 1, 3, 9], with_minimum, None)
            .iter()
            .map(|l| l.start)
            .collect();
    assert_eq!(starts, vec![0], "1 is at the minimum, 3 is NaN, 9 is past the series");
}

#[test]
fn trend_scanning_picks_the_window_with_the_largest_absolute_t_value() {
    // start 0: L=3 on [0,1,3]: b = 1.5, SSE = 1/6, t = 1.5 / sqrt(1/12) = 3√3;
    //          L=4 on [0,1,3,2]: b = 0.8, SSE = 1.8, t = 0.8 / sqrt(0.18).  -> L=3, end 2.
    // start 1: L=3 on [1,3,2]: t = 0.5 / sqrt(0.75) = 1/√3;
    //          L=4 on [1,3,2,5]: b = 1.1, SSE = 2.7, t = 1.1 / sqrt(0.27). -> L=4, trend end 4.
    // start 2: 2 + 4 > 5, skipped.
    // Either way the label READ the longest window: its span ends at start + 4 - 1.
    let series = [0.0, 1.0, 3.0, 2.0, 5.0];
    let labels = trend_scanning_labels(&series, &[0, 1, 2], 3, 4);
    assert_eq!(labels.len(), 2);
    let ends = |l: &TrendScanLabel| (l.start, l.end, l.trend_end, l.label);
    assert_eq!(ends(&labels[0]), (0, 3, 2, 1));
    assert!(close_enough(labels[0].t_value, 3.0 * 3.0_f64.sqrt()));
    assert_eq!(ends(&labels[1]), (1, 4, 4, 1));
    assert!(close_enough(labels[1].t_value, 1.1 / 0.27_f64.sqrt()));

    // A falling series flips every t-value exactly (the residuals only change sign).
    let falling: Vec<f64> = series.iter().map(|v| -v).collect();
    let down = trend_scanning_labels(&falling, &[0, 1], 3, 4);
    for (up, down) in labels.iter().zip(&down) {
        assert_eq!(
            (down.end, down.trend_end, down.label, down.t_value),
            (up.end, up.trend_end, -1, -up.t_value)
        );
    }
}

/// The review's scenario: an event at 100 whose strongest trend is 5 bars long still READ the prices to
/// bar 119 (`max_horizon = 20`), so a purged split whose test fold starts at bar 110 drops it.
#[test]
fn a_trend_scanning_labels_span_is_every_bar_its_choice_read() {
    // A near-line rise on bars 100..=104 (L=5: t = 10.2 / sqrt(0.8 / 3 / 10) = 62.5, above L=3's 36.4
    // and L=4's 38.2), then the 0, 1, 2 saw again: every longer window's slope is weak.
    const RISE: [f64; 5] = [0.0, 10.0, 21.0, 30.0, 41.0];
    let series: Vec<f64> = (0..140usize)
        .map(|i| if (100..105).contains(&i) { RISE[i - 100] } else { (i % 3) as f64 })
        .collect();
    let labels = trend_scanning_labels(&series, &[100, 110], 3, 20);
    assert_eq!((labels[0].start, labels[0].trend_end, labels[0].end), (100, 104, 119));
    let spans: Vec<(usize, usize)> = labels.iter().map(|l| (l.start, l.end)).collect();
    let splits = crate::validation::purged_kfold_label_spans(&spans, 2, 0);
    let (train, test) = &splits[1];
    assert_eq!(test, &vec![1], "the fold of the event at 110");
    assert!(train.is_empty(), "the label at 100 read bars 105..=119: purged, {train:?}");
}

#[test]
fn trend_scanning_ignores_an_exact_line_and_refuses_short_horizons() {
    // [0,1,2] is an exact line (the shared regression reads t = 0); [0,1,2,4] is not and wins.
    let labels = trend_scanning_labels(&[0.0, 1.0, 2.0, 4.0], &[0], 3, 4);
    assert_eq!((labels[0].trend_end, labels[0].end), (3, 3));
    assert!(labels[0].t_value > 0.0);
    // Every window exact: t = 0 everywhere, the shorter window is kept, label 0; the span is still
    // every bar read.
    let flat = trend_scanning_labels(&[0.0, 1.0, 2.0, 3.0], &[0], 3, 4);
    let read = (flat[0].trend_end, flat[0].end, flat[0].t_value, flat[0].label);
    assert_eq!(read, (2, 3, 0.0, 0));
    assert!(trend_scanning_labels(&[0.0, 1.0, 3.0, 2.0], &[0], 2, 4).is_empty());
    assert!(trend_scanning_labels(&[0.0, 1.0, 3.0, 2.0], &[0], 4, 3).is_empty());
}
