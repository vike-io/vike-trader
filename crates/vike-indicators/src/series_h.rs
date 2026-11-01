//! Whole-series calls on plain `f64` slices for group `h` of the third wave of indicators,
//! re-exported from [`crate::series`] (`series::<name>(...)`).
//!
//! `series::gmma`, `series::alligator`, `series::relative_vigor`, `series::high_low_52w`,
//! `series::fisher`, `series::chande_kroll_stop`, `series::connors_rsi`, `series::hvol`,
//! `series::relative_volatility`, `series::median_ma`, `series::linreg_channel`,
//! `series::mcginley` and `series::asi`: the same arithmetic as the indicator of that name in
//! [`crate::indicators`], over plain prices with no `Bar` in between. Each returns one value per
//! input (the SHORTEST of the input slices when there are several), `NaN` while the indicator is
//! warming up, and refuses a parameter the indicator's registry row would not accept with a
//! [`crate::ParamError`] (`gmma` and `alligator` have no parameter, so they cannot fail).

use crate::ParamError;
use crate::fast_h::{
    AlligatorCore, AsiCore, ChandeKrollCore, ConnorsCore, FisherCore, GmmaCore, HiLoCore, HvolCore,
    LinregCore, McginleyCore, MedianCore, RelVolCore, VigorCore,
};

/// The window length of an indicator built from a registry parameter: the parameter rounded, at most
/// a bound far above any registry range.
#[inline]
fn cap(p: f64) -> usize {
    p.round().min(crate::roll::RING_CAP as f64) as usize
}

/// Guppy multiple moving average of `close`: twelve EMAs, over 3, 5, 8, 10, 12, 15, 30, 35, 40, 45,
/// 50 and 60 values, each seeded with the mean of its first values; line `k` is `NaN` for the first
/// `period_k - 1` values. It has no parameter, so it cannot fail.
#[inline]
pub fn gmma(close: &[f64]) -> [Vec<f64>; 12] {
    let mut core = GmmaCore::new();
    let mut out: [Vec<f64>; 12] = std::array::from_fn(|_| vec![f64::NAN; close.len()]);
    for (i, &c) in close.iter().enumerate() {
        let v = core.push(c);
        for (col, x) in out.iter_mut().zip(v) {
            col[i] = x;
        }
    }
    out
}

/// Williams Alligator from parallel `high` and `low` slices: `[jaw, teeth, lips]`, each as long as
/// the SHORTER slice. Wilder smoothing of the median price over 13, 8 and 5 bars, plotted 8, 5 and 3
/// bars forward; `NaN` until a line's smoothing has seeded and its shift has elapsed. It has no
/// parameter, so it cannot fail.
#[inline]
pub fn alligator(high: &[f64], low: &[f64]) -> [Vec<f64>; 3] {
    let mut core = AlligatorCore::new();
    let len = high.len().min(low.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, c] = &mut out;
    for (((x, y), z), (&h, &l)) in
        a.iter_mut().zip(b.iter_mut()).zip(c.iter_mut()).zip(high.iter().zip(low))
    {
        let v = core.push(h, l);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    out
}

/// Relative Vigor Index from parallel `open`, `high`, `low` and `close` slices: `[rvgi, signal]`,
/// each as long as the SHORTEST slice. The sum over `period` bars (registry range `2..=200`) of the
/// 4-bar-smoothed close-minus-open over the sum of the smoothed high-minus-low, and a 4-bar smoothing
/// of that line; `NaN` while warming up and wherever the range sum is zero.
#[inline]
pub fn relative_vigor(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    period: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("relative_vigor", &[period])?;
    let mut core = VigorCore::new(cap(period));
    let len = open.len().min(high.len()).min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [r, s] = &mut out;
    for ((((a, b), &o), &h), (&l, &c)) in
        r.iter_mut().zip(s.iter_mut()).zip(open).zip(high).zip(low.iter().zip(close))
    {
        let v = core.push(o, h, l, c);
        *a = v[0];
        *b = v[1];
    }
    Ok(out)
}

/// Rolling highest high and lowest low from parallel `high` and `low` slices: `[high_n, low_n]`,
/// each as long as the SHORTER slice, over `period` bars (registry range `2..=1000`); `NaN` for the
/// first `period - 1` values. A `NaN` price is ignored.
#[inline]
pub fn high_low_52w(high: &[f64], low: &[f64], period: f64) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("high_low_52w", &[period])?;
    let mut core = HiLoCore::new(cap(period));
    let len = high.len().min(low.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [hi, lo] = &mut out;
    for (((a, b), &h), &l) in hi.iter_mut().zip(lo.iter_mut()).zip(high).zip(low) {
        let v = core.push(h, l);
        *a = v[0];
        *b = v[1];
    }
    Ok(out)
}

/// Fisher Transform from parallel `high` and `low` slices: `[fisher, trigger]`, each as long as the
/// SHORTER slice. The median price placed inside its `period`-bar range (registry range `2..=100`),
/// smoothed and run through an inverse hyperbolic tangent; the trigger is the previous Fisher value.
/// `NaN` for the first `period - 1` values (the trigger one more).
#[inline]
pub fn fisher(high: &[f64], low: &[f64], period: f64) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("fisher", &[period])?;
    let mut core = FisherCore::new(cap(period));
    let len = high.len().min(low.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [f, t] = &mut out;
    for (((a, b), &h), &l) in f.iter_mut().zip(t.iter_mut()).zip(high).zip(low) {
        let v = core.push(h, l);
        *a = v[0];
        *b = v[1];
    }
    Ok(out)
}

/// Chande Kroll Stop from parallel `high`, `low` and `close` slices: `[long_stop, short_stop]`, each
/// as long as the SHORTEST slice. A Wilder ATR over `p` bars (registry range `2..=200`) scaled by `x`
/// (`1..=10`, rounded), subtracted from the `p`-bar highest high and added to the `p`-bar lowest low,
/// then the highest of the first and the lowest of the second over the last `q` bars (`2..=200`).
/// `NaN` until the ATR has seeded (bar `p`) and `q` bars are in.
#[inline]
pub fn chande_kroll_stop(
    high: &[f64],
    low: &[f64],
    close: &[f64],
    p: f64,
    x: f64,
    q: f64,
) -> Result<[Vec<f64>; 2], ParamError> {
    crate::typed::checked("chande_kroll_stop", &[p, x, q])?;
    let mut core = ChandeKrollCore::new(cap(p), (x.round() as usize) as f64, cap(q));
    let len = high.len().min(low.len()).min(close.len());
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len]];
    let [ls, ss] = &mut out;
    for ((((a, b), &h), &l), &c) in ls.iter_mut().zip(ss.iter_mut()).zip(high).zip(low).zip(close) {
        let v = core.push(h, l, c);
        *a = v[0];
        *b = v[1];
    }
    Ok(out)
}

/// Connors RSI of `close`, 0 to 100: the mean of the RSI of the close over `rsi_p` moves, the RSI over
/// `streak_p` moves of the up/down streak length (both registry range `2..=100`), and the percent
/// rank of the bar's percentage return among the last `rank_p` returns (`10..=500`). `NaN` until all
/// three parts are defined.
#[inline]
pub fn connors_rsi(
    close: &[f64],
    rsi_p: f64,
    streak_p: f64,
    rank_p: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("connors_rsi", &[rsi_p, streak_p, rank_p])?;
    let mut core = ConnorsCore::new(cap(rsi_p), cap(streak_p), cap(rank_p));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Annualized historical volatility of `close`, in percent: the population standard deviation of the
/// last `period` log returns (registry range `2..=200`) times the square root of `ann` (periods per
/// year, `1..=365`) and 100. A bar whose close or previous close is not positive has no log return and
/// is skipped, not counted; `NaN` until `period` log returns exist.
#[inline]
pub fn hvol(close: &[f64], period: f64, ann: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("hvol", &[period, ann])?;
    let mut core = HvolCore::new(cap(period), ann.round() as usize);
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Relative Volatility Index of `close`, 0 to 100: the rolling population standard deviation of the
/// close over `period` values (registry range `2..=200`), split by the direction of the close, each
/// side EMA-smoothed over `period`, and the up side as a percentage of the two (`50` when both are
/// zero). `NaN` while warming up.
#[inline]
pub fn relative_volatility(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("relative_volatility", &[period])?;
    let mut core = RelVolCore::new(cap(period));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Rolling median of `close` over `period` values (registry range `2..=400`): the middle value, or
/// the mean of the two middle ones when `period` is even. `NaN` for the first `period - 1` values and
/// while a `NaN` is inside the window.
#[inline]
pub fn median_ma(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("median_ma", &[period])?;
    let mut core = MedianCore::new(cap(period));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Linear-regression channel of `close`: `[upper, mid, lower]`, each as long as `close`. The
/// least-squares line through the last `period` closes (registry range `2..=400`), `mid` its value at
/// the newest bar and the bands `mult` (`0.5..=10`) population standard deviations of the window's
/// residuals either side. `NaN` for the first `period - 1` values.
#[inline]
pub fn linreg_channel(close: &[f64], period: f64, mult: f64) -> Result<[Vec<f64>; 3], ParamError> {
    crate::typed::checked("linreg_channel", &[period, mult])?;
    let mut core = LinregCore::new(cap(period), mult);
    let len = close.len();
    let mut out = [vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]];
    let [a, b, d] = &mut out;
    for (((x, y), z), &c) in a.iter_mut().zip(b.iter_mut()).zip(d.iter_mut()).zip(close) {
        let v = core.push(c);
        *x = v[0];
        *y = v[1];
        *z = v[2];
    }
    Ok(out)
}

/// McGinley Dynamic of `close` over `period` (registry range `2..=200`): a moving average that speeds
/// up and slows down with how far price has moved from it. It starts at the first close, so there is
/// no warm-up.
#[inline]
pub fn mcginley(close: &[f64], period: f64) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("mcginley", &[period])?;
    let mut core = McginleyCore::new(cap(period));
    let mut out = vec![f64::NAN; close.len()];
    for (o, &c) in out.iter_mut().zip(close) {
        *o = core.push(c);
    }
    Ok(out)
}

/// Accumulative swing index from parallel `open`, `high`, `low` and `close` slices (the result is as
/// long as the SHORTEST): a running sum of Wilder's swing index, which reads the previous bar's open
/// and close, with the limit move `limit` (registry range `0.1..=10`). `NaN` on the first bar.
#[inline]
pub fn asi(
    open: &[f64],
    high: &[f64],
    low: &[f64],
    close: &[f64],
    limit: f64,
) -> Result<Vec<f64>, ParamError> {
    crate::typed::checked("asi", &[limit])?;
    let mut core = AsiCore::new(limit);
    let mut out = vec![f64::NAN; open.len().min(high.len()).min(low.len()).min(close.len())];
    for ((((o, &a), &h), &l), &c) in out.iter_mut().zip(open).zip(high).zip(low).zip(close) {
        *o = core.push(a, h, l, c);
    }
    Ok(out)
}
