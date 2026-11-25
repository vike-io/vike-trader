//! Research LABELS for supervised learning on bars: the triple barrier with meta-labelling, and trend
//! scanning. The symmetric CUSUM event filter that samples the events they label is
//! `vike_math::change_detection::cusum_events_of_series` (decision 0294).
//!
//! López de Prado, *Advances in Financial Machine Learning* (AFML) ch. 2 (snippet 2.4) and ch. 3
//! (snippets 3.2 to 3.7); trend scanning is his *Machine Learning for Asset Managers* (MLAM)
//! section 5.4 (snippet 5.2). vike is the reference implementation (decision 0021): there is no
//! Python twin, and the tests are hand-computed from the published formulas.
//!
//! # The contract
//!
//! - TIME IS A BAR INDEX. Every input series is one value per bar, oldest first; an event, a label
//!   start and a label end are positions in that series. A caller holding timestamps maps them.
//! - A label's span is `[start, end]`, both bar indices, `start <= end`, inclusive: the label is a
//!   function of the prices in `[start, end]` and of none after `end` (a trend-scanning label's `end`
//!   is the longest window's, every window it compared). That is the span
//!   [`crate::sample_weights`] and [`crate::validation::purged_kfold_label_spans`] take.
//! - Pure functions: no clock, no file, no randomness. Inputs must be finite where a barrier is
//!   read; a NaN price never crosses a barrier (every comparison with it is false).

use vike_math::regression::slope_t_value;

/// The barriers of [`triple_barrier_labels`] (AFML snippet 3.6's `ptSl`, `minRet` and `t1`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TripleBarrierLabelling {
    /// The profit-taking barrier sits `profit_taking_multiple × target` above the entry (in the
    /// bet's direction). `<= 0` disables it.
    pub profit_taking_multiple: f64,
    /// The stop-loss barrier sits `stop_loss_multiple × target` below the entry (against the bet).
    /// `<= 0` disables it.
    pub stop_loss_multiple: f64,
    /// The vertical barrier: the label closes at `start + max_holding_bars` at the latest. `None`
    /// is AFML's `t1=False`: no vertical barrier.
    pub max_holding_bars: Option<usize>,
    /// An event whose target is not strictly above this is skipped (AFML's `minRet`).
    pub minimum_target: f64,
}

/// Which barrier closed a [`TripleBarrierLabel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarrierTouched {
    /// The profit-taking barrier (above the entry for a long or side-less bet).
    Upper,
    /// The stop-loss barrier.
    Lower,
    /// The vertical barrier, with neither horizontal one crossed before or on it.
    Vertical,
    /// The series ended before any barrier: the label is read off the LAST bar, as AFML does
    /// (`t1.fillna(close.index[-1])`). A censored label; a caller who wants none drops these.
    EndOfData,
}

/// One labelled event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TripleBarrierLabel {
    /// The event bar: the entry price is `close[start]`.
    pub start: usize,
    /// The bar of the first touch: the exit price is `close[end]`.
    pub end: usize,
    /// `close[end] / close[start] - 1`, multiplied by the side when one was given.
    pub realized_return: f64,
    /// Side-less: the sign of `realized_return` (`-1`, `0`, `+1`; AFML snippet 3.5). With a side
    /// (meta-labelling, snippet 3.7): `1` when `realized_return > 0`, else `0`.
    pub label: i8,
    pub touched: BarrierTouched,
}

/// Triple-barrier labels (AFML ch. 3), and meta-labels when `side` is given.
///
/// For each event bar `s` with `target[s] > minimum_target`, the path return
/// `r_t = (close[t] / close[s] - 1) × side` for `t > s` is scanned until the first bar where
/// `r_t > profit_taking_multiple × target[s]` (upper) or `r_t < -stop_loss_multiple × target[s]`
/// (lower), both STRICT as AFML's `df0 > pt` / `df0 < sl`, or the vertical barrier, whichever comes
/// first. A horizontal barrier crossed ON the vertical bar counts as that horizontal barrier (the
/// label is the same either way). The target is the volatility scale: pass a per-bar volatility
/// estimate (for example a rolling standard deviation of returns), never a forward-looking one.
///
/// `side` (one `-1`/`+1` per event, the primary model's bet) turns this into META-labelling:
/// barriers apply to the side-adjusted return and the label says whether the bet paid (`1`) or not
/// (`0`). An event whose side is `0` is skipped (no bet to label). Without a side, the bet is long:
/// AFML forces the two multiples equal in that case; pass equal multiples for its exact form.
///
/// Skipped events (target not above `minimum_target`, NaN target, side `0`, or a start outside
/// `close`) produce no row, so the output carries `start` to align it with the input.
///
/// # Panics
///
/// When `side` is given with a length other than `events.len()`, or `target.len() != close.len()`.
pub fn triple_barrier_labels(
    close: &[f64],
    target: &[f64],
    events: &[usize],
    barriers: TripleBarrierLabelling,
    side: Option<&[i8]>,
) -> Vec<TripleBarrierLabel> {
    assert_eq!(target.len(), close.len(), "one target per bar");
    if let Some(side) = side {
        assert_eq!(side.len(), events.len(), "one side per event");
    }
    let Some(last_bar) = close.len().checked_sub(1) else { return Vec::new() };
    let mut out = Vec::with_capacity(events.len());
    for (k, &start) in events.iter().enumerate() {
        let bet = side.map_or(1, |side| side[k]);
        // A NaN target fails this test, so it is skipped too.
        let target_is_wide_enough = start <= last_bar && target[start] > barriers.minimum_target;
        if bet == 0 || !target_is_wide_enough {
            continue;
        }
        let direction = f64::from(bet);
        let upper = barriers.profit_taking_multiple * target[start];
        let lower = -barriers.stop_loss_multiple * target[start];
        let vertical = barriers.max_holding_bars.map(|h| start.saturating_add(h));
        let path_end = vertical.map_or(last_bar, |v| v.min(last_bar));
        let mut found = None;
        for (offset, &price) in close[start + 1..=path_end].iter().enumerate() {
            let t = start + 1 + offset;
            let path_return = (price / close[start] - 1.0) * direction;
            if barriers.profit_taking_multiple > 0.0 && path_return > upper {
                found = Some((t, BarrierTouched::Upper));
                break;
            }
            if barriers.stop_loss_multiple > 0.0 && path_return < lower {
                found = Some((t, BarrierTouched::Lower));
                break;
            }
        }
        let (end, touched) = found.unwrap_or(match vertical {
            Some(v) if v <= last_bar => (v, BarrierTouched::Vertical),
            _ => (last_bar, BarrierTouched::EndOfData),
        });
        let realized_return = (close[end] / close[start] - 1.0) * direction;
        let label = if side.is_some() {
            i8::from(realized_return > 0.0)
        } else if realized_return > 0.0 {
            1
        } else if realized_return < 0.0 {
            -1
        } else {
            0
        };
        out.push(TripleBarrierLabel { start, end, realized_return, label, touched });
    }
    out
}

/// One trend-scanning label.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrendScanLabel {
    pub start: usize,
    /// The last bar the label READ, `start + max_horizon - 1`: choosing the window compared every window up
    /// to the longest, so the label's span is `[start, end]` (AFML's `t1`), the span purging and sample
    /// weights take.
    pub end: usize,
    /// The last bar of the window whose trend was strongest (the identified trend's end, `<= end`).
    pub trend_end: usize,
    /// The slope t-value of that window's regression on time.
    pub t_value: f64,
    /// The sign of `t_value`: `-1`, `0` or `+1`.
    pub label: i8,
}

/// Trend-scanning labels (MLAM section 5.4, snippet 5.2).
///
/// For each event bar `s` and each window length `L` in `min_horizon..=max_horizon`, regress
/// `series[s..s + L]` on the time index `0..L` with an intercept (ordinary least squares) and take
/// the slope's t-value `b / sqrt(SSE / (L - 2) / Σ(i - ī)²)`, through the one regression
/// `crate::metrics::k_ratio` uses. The label is the sign of the t-value with the largest absolute
/// value. Its span ends at the LONGEST window's last bar (`end`, the snippet's record): the choice read
/// every window's prices, so a purge by a shorter span would keep a training label whose choice read
/// the test fold. The chosen window's last bar is `trend_end`.
///
/// A reading of the published snippet, stated: a window the line fits exactly reads `t = 0` (the
/// shared regression's convention for an undefined t-value), so it is never chosen over a sloped one,
/// as the snippet's `replace([-inf, inf, nan], 0)` arranges. Equal scores keep the shorter window. An
/// event is skipped when
/// `s + max_horizon > series.len()` (the snippet's `iloc0 + max(hrzns) > close.shape[0]`), and
/// every event is skipped when `min_horizon < 3` (a slope t-value needs `L - 2 >= 1`) or
/// `min_horizon > max_horizon`.
pub fn trend_scanning_labels(
    series: &[f64],
    events: &[usize],
    min_horizon: usize,
    max_horizon: usize,
) -> Vec<TrendScanLabel> {
    if min_horizon < 3 || min_horizon > max_horizon {
        return Vec::new();
    }
    let time: Vec<f64> = (0..max_horizon).map(|i| i as f64).collect();
    let mut out = Vec::new();
    for &start in events {
        if start.saturating_add(max_horizon) > series.len() {
            continue;
        }
        let mut best: Option<(usize, f64)> = None; // (trend_end, t_value)
        for length in min_horizon..=max_horizon {
            let t_value = slope_t_value(&time[..length], &series[start..start + length]);
            if best.is_none_or(|(_, best_t)| t_value.abs() > best_t.abs()) {
                best = Some((start + length - 1, t_value));
            }
        }
        if let Some((trend_end, t_value)) = best {
            let label = if t_value > 0.0 {
                1
            } else if t_value < 0.0 {
                -1
            } else {
                0
            };
            let end = start + max_horizon - 1;
            out.push(TrendScanLabel { start, end, trend_end, t_value, label });
        }
    }
    out
}

#[path = "labels_tests.rs"]
#[cfg(test)]
mod labels_tests;
