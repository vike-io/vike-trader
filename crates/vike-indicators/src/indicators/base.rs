//! The 17 indicators of the base set — each a struct implementing [`crate::Indicator`].
//! Faithful f64 extraction of vike-trader-app `core/indicators/base.py` (the
//! `c_*(&Columns) -> Vec<Vec<f64>>` batch functions live here as the `batch_*`
//! kernels; `vectorize` is a thin wrapper over them, so it stays the parity
//! reference).
//!
//! TWO PATHS, ONE TRUTH: `vectorize` is the batch reference; `on_bar` streams with
//! bounded per-bar work — no full-series recompute. Nine indicators run a true
//! O(1) recurrence (EMA, RSI, ATR, ROC, OBV, MACD, PSAR, Keltner, VWAP); the eight
//! window statistics (WMA, Bollinger, Donchian, Stochastic, CCI, Williams, and now
//! SMA and Awesome) keep an O(window) fold and reproduce the batch's exact
//! per-window arithmetic.
//!
//! ⚠ SMA and Awesome MOVED into that second group. They were O(1) sliding sums until
//! `math::sma` was de-accumulated: a sliding `sum += c[i] - c[i-n]` carries rounding
//! error from bar 0, which made every consumer's value change when
//! `hist_indicator!` truncated history. Bit-parity with the batch is the contract, so
//! the streaming mirrors had to follow the batch rather than the other way round. Bit-parity forbids the usual O(1) shortcuts on the window
//! stats — a running (Welford) variance would round differently than a two-pass
//! window std, so Bollinger/CCI re-read the window with the computed mean. Every
//! indicator is gated by `tests/parity.rs`: on_bar folded == vectorize, bitwise.

// index loops (not iterators) are deliberate in the `batch_*` kernels: they mirror
// the Python oracle's `for i in range(len)` windows line-for-line, keeping the
// parity mapping obvious (same convention as vike-backtest's engine kernels).
#![allow(clippy::needless_range_loop)]

use super::state::{EmaState, RmaState, SmaAcc};
use crate::math::{atr, ema, sma, sma_nan, sq, wma, Columns};
use crate::Indicator;
use std::collections::VecDeque;
use vike_model::Bar;

// ============================ batch kernels ================================
// One per indicator; these are the c_*(&Columns) functions from the oracle,
// re-headed to build Columns from &[Bar]. Do NOT "improve" the formulas.

fn batch_sma(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![sma(&x.c, n)]
}

fn batch_ema(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![ema(&x.c, n)]
}

fn batch_wma(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![wma(&x.c, n)]
}

fn batch_bollinger(bars: &[Bar], n: usize, m: f64) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.c.len();
    let (mut mid, mut up, mut lo) = (vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]);
    if len >= n {
        for i in (n - 1)..len {
            let w = &x.c[i + 1 - n..=i];
            let mean = w.iter().sum::<f64>() / n as f64;
            let var = w.iter().map(|v| sq(v - mean)).sum::<f64>() / n as f64;
            let sd = var.sqrt();
            mid[i] = mean;
            up[i] = mean + m * sd;
            lo[i] = mean - m * sd;
        }
    }
    vec![up, mid, lo]
}

fn batch_donchian(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.h.len();
    let (mut up, mut mid, mut lo) = (vec![f64::NAN; len], vec![f64::NAN; len], vec![f64::NAN; len]);
    if len >= n {
        for i in (n - 1)..len {
            let hi = x.h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
            up[i] = hi;
            lo[i] = ll;
            mid[i] = (hi + ll) / 2.0;
        }
    }
    vec![up, mid, lo]
}

fn batch_keltner(bars: &[Bar], ema_n: usize, atr_n: usize, mult: f64) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let mid = ema(&x.c, ema_n);
    let a = atr(&x.h, &x.l, &x.c, atr_n);
    let len = x.c.len();
    let (mut up, mut lo) = (vec![f64::NAN; len], vec![f64::NAN; len]);
    for i in 0..len {
        if !mid[i].is_nan() && !a[i].is_nan() {
            up[i] = mid[i] + mult * a[i];
            lo[i] = mid[i] - mult * a[i];
        }
    }
    vec![up, mid, lo]
}

fn batch_vwap(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.c.len();
    let mut out = vec![f64::NAN; len];
    let (mut pv, mut vol) = (0.0, 0.0);
    for i in 0..len {
        if x.new_session[i] {
            pv = 0.0;
            vol = 0.0;
        }
        let tp = (x.h[i] + x.l[i] + x.c[i]) / 3.0;
        pv += tp * x.v[i];
        vol += x.v[i];
        out[i] = if vol > 0.0 { pv / vol } else { f64::NAN };
    }
    vec![out]
}

fn batch_psar(bars: &[Bar], step: f64, max_af: f64) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (h, l) = (&x.h, &x.l);
    let len = h.len();
    let mut out = vec![f64::NAN; len];
    if len < 2 {
        return vec![out];
    }
    let mut up = true;
    let mut sar = l[0];
    let mut ep = h[0];
    let mut af = step;
    out[0] = sar;
    for i in 1..len {
        let mut s = sar + af * (ep - sar);
        if up {
            s = s.min(l[i - 1]).min(if i >= 2 { l[i - 2] } else { l[i - 1] });
            if l[i] < s {
                up = false;
                s = ep;
                ep = l[i];
                af = step;
            } else if h[i] > ep {
                ep = h[i];
                af = (af + step).min(max_af);
            }
        } else {
            s = s.max(h[i - 1]).max(if i >= 2 { h[i - 2] } else { h[i - 1] });
            if h[i] > s {
                up = true;
                s = ep;
                ep = h[i];
                af = step;
            } else if l[i] < ep {
                ep = l[i];
                af = (af + step).min(max_af);
            }
        }
        sar = s;
        out[i] = sar;
    }
    vec![out]
}

fn batch_rsi(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let c = &x.c;
    let len = c.len();
    let mut out = vec![f64::NAN; len];
    if len < n + 1 {
        return vec![out];
    }
    let (mut gains, mut losses) = (vec![0.0; len], vec![0.0; len]);
    for i in 1..len {
        let d = c[i] - c[i - 1];
        gains[i] = if d > 0.0 { d } else { 0.0 };
        losses[i] = if d < 0.0 { -d } else { 0.0 };
    }
    let mut ag = gains[1..=n].iter().sum::<f64>() / n as f64;
    let mut al = losses[1..=n].iter().sum::<f64>() / n as f64;
    out[n] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    for i in (n + 1)..len {
        ag = (ag * (n as f64 - 1.0) + gains[i]) / n as f64;
        al = (al * (n as f64 - 1.0) + losses[i]) / n as f64;
        out[i] = if al == 0.0 { 100.0 } else { 100.0 - 100.0 / (1.0 + ag / al) };
    }
    vec![out]
}

fn batch_macd(bars: &[Bar], fast_n: usize, slow_n: usize, sig_n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let ef = ema(&x.c, fast_n);
    let es = ema(&x.c, slow_n);
    let len = x.c.len();
    let mut macd_line = vec![f64::NAN; len];
    for i in 0..len {
        if !ef[i].is_nan() && !es[i].is_nan() {
            macd_line[i] = ef[i] - es[i];
        }
    }
    let start = macd_line.iter().position(|v| !v.is_nan()).unwrap_or(len);
    let mut signal = vec![f64::NAN; len];
    if start + sig_n <= len {
        let es_sig = ema(&macd_line[start..], sig_n);
        for (k, val) in es_sig.iter().enumerate() {
            signal[start + k] = *val;
        }
    }
    let mut hist = vec![f64::NAN; len];
    for i in 0..len {
        if !macd_line[i].is_nan() && !signal[i].is_nan() {
            hist[i] = macd_line[i] - signal[i];
        }
    }
    vec![macd_line, signal, hist]
}

fn batch_stochastic(bars: &[Bar], n: usize, sk: usize, sd: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.c.len();
    let mut raw = vec![f64::NAN; len];
    if len >= n {
        for i in (n - 1)..len {
            let hh = x.h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            raw[i] = if rng > 0.0 { 100.0 * (x.c[i] - ll) / rng } else { 0.0 };
        }
    }
    let k = sma_nan(&raw, sk);
    let d = sma_nan(&k, sd);
    vec![k, d]
}

fn batch_atr(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    vec![atr(&x.h, &x.l, &x.c, n)]
}

fn batch_cci(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.c.len();
    let mut out = vec![f64::NAN; len];
    if len >= n {
        let tp: Vec<f64> = (0..len).map(|i| (x.h[i] + x.l[i] + x.c[i]) / 3.0).collect();
        for i in (n - 1)..len {
            let w = &tp[i + 1 - n..=i];
            let mean = w.iter().sum::<f64>() / n as f64;
            let md = w.iter().map(|v| (v - mean).abs()).sum::<f64>() / n as f64;
            out[i] = if md > 0.0 { (tp[i] - mean) / (0.015 * md) } else { 0.0 };
        }
    }
    vec![out]
}

fn batch_roc(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let c = &x.c;
    let len = c.len();
    let mut out = vec![f64::NAN; len];
    for i in n..len {
        out[i] = if c[i - n] != 0.0 { 100.0 * (c[i] - c[i - n]) / c[i - n] } else { f64::NAN };
    }
    vec![out]
}

fn batch_williams(bars: &[Bar], n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.c.len();
    let mut out = vec![f64::NAN; len];
    if len >= n {
        for i in (n - 1)..len {
            let hh = x.h[i + 1 - n..=i].iter().cloned().fold(f64::MIN, f64::max);
            let ll = x.l[i + 1 - n..=i].iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            out[i] = if rng > 0.0 { -100.0 * (hh - x.c[i]) / rng } else { 0.0 };
        }
    }
    vec![out]
}

fn batch_obv(bars: &[Bar]) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let (c, v) = (&x.c, &x.v);
    let len = c.len();
    let mut out = vec![f64::NAN; len];
    if len == 0 {
        return vec![out];
    }
    out[0] = 0.0;
    for i in 1..len {
        out[i] = out[i - 1]
            + if c[i] > c[i - 1] {
                v[i]
            } else if c[i] < c[i - 1] {
                -v[i]
            } else {
                0.0
            };
    }
    vec![out]
}

fn batch_awesome(bars: &[Bar], fast_n: usize, slow_n: usize) -> Vec<Vec<f64>> {
    let x = Columns::from_bars(bars);
    let len = x.h.len();
    let mp: Vec<f64> = (0..len).map(|i| (x.h[i] + x.l[i]) / 2.0).collect();
    let sf = sma(&mp, fast_n);
    let ss = sma(&mp, slow_n);
    let mut out = vec![f64::NAN; len];
    for i in 0..len {
        if !sf[i].is_nan() && !ss[i].is_nan() {
            out[i] = sf[i] - ss[i];
        }
    }
    vec![out]
}

// ===================== rolling-window incremental family ===================
// on_bar keeps only a fixed O(window) buffer and reproduces the batch kernel's
// EXACT per-window fold every bar — never a full-series recompute. Bit-parity
// forbids the usual O(1) shortcuts here: a running weighted sum (WMA), a Welford
// variance (Bollinger) or a running MAD (CCI) would round differently than
// vectorize's window folds, so the window is re-read in the batch's order.

/// `wma` — O(window) linear-weighted sum (weights 1..=n, oldest→newest) over the
/// last `n` closes, identical to `math::wma`.
#[derive(Clone)]
pub struct Wma {
    n: usize,
    window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Wma {
    pub fn new() -> Self {
        Self { n: 20, window: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; the rolling window starts
    /// empty regardless of `n`, so the rest of `new()`'s state is reused.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Wma {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Wma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        self.window.push_back(bar.close);
        if self.window.len() > n {
            self.window.pop_front();
        }
        let out = if self.window.len() == n {
            let denom = (n * (n + 1) / 2) as f64;
            let mut acc = 0.0;
            for k in 1..=n {
                acc += k as f64 * self.window[k - 1];
            }
            acc / denom
        } else {
            f64::NAN
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_wma(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.window.clear();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "wma"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `bollinger` — O(window) two-pass mean/std over the last `n` closes (same fold
/// order as `batch_bollinger`; a running variance would not bit-match). Outputs
/// `[up, mid, lo]`.
#[derive(Clone)]
pub struct Bollinger {
    n: usize,
    m: f64,
    window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Bollinger {
    pub fn new() -> Self {
        Self { n: 20, m: 2.0, window: VecDeque::new(), last: vec![f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period (integer, rounded), `p[1]` =
    /// band multiplier (`m`, displayed as `"mult"` in the registry spec — the
    /// field name is unchanged). No sub-state depends on either.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        let m = p.get(1).copied().unwrap_or(2.0);
        Self { n, m, ..Self::new() }
    }
}
impl Default for Bollinger {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Bollinger {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        let m = self.m;
        self.window.push_back(bar.close);
        if self.window.len() > n {
            self.window.pop_front();
        }
        let out = if self.window.len() == n {
            let mean = self.window.iter().sum::<f64>() / n as f64;
            let var = self.window.iter().map(|v| sq(v - mean)).sum::<f64>() / n as f64;
            let sd = var.sqrt();
            vec![mean + m * sd, mean, mean - m * sd]
        } else {
            vec![f64::NAN; 3]
        };
        self.last = out.clone();
        out
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_bollinger(bars, self.n, self.m)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.window.clear();
        self.last = vec![f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "bollinger"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `donchian` — O(window) rolling high/low extrema over the last `n` bars,
/// identical to `batch_donchian`. Outputs `[up, mid, lo]`.
#[derive(Clone)]
pub struct Donchian {
    n: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    last: Vec<f64>,
}
impl Donchian {
    pub fn new() -> Self {
        Self { n: 20, highs: VecDeque::new(), lows: VecDeque::new(), last: vec![f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Donchian {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Donchian {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        self.highs.push_back(bar.high);
        self.lows.push_back(bar.low);
        if self.highs.len() > n {
            self.highs.pop_front();
            self.lows.pop_front();
        }
        let out = if self.highs.len() == n {
            let hi = self.highs.iter().cloned().fold(f64::MIN, f64::max);
            let ll = self.lows.iter().cloned().fold(f64::MAX, f64::min);
            vec![hi, (hi + ll) / 2.0, ll]
        } else {
            vec![f64::NAN; 3]
        };
        self.last = out.clone();
        out
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_donchian(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.highs.clear();
        self.lows.clear();
        self.last = vec![f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "donchian"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `keltner` — TRUE O(1): EMA(20) mid + ATR(10) band, reusing the streaming
/// [`EmaState`]/[`RmaState`] recurrences, identical to `batch_keltner`. Outputs
/// `[up, mid, lo]`.
#[derive(Clone)]
pub struct Keltner {
    ema_n: usize,
    atr_n: usize,
    mult: f64,
    ema: EmaState,
    prev_close: Option<f64>,
    rma: RmaState,
    last: Vec<f64>,
}
impl Keltner {
    pub fn new() -> Self {
        let ema_n = 20;
        let atr_n = 10;
        Self {
            ema_n,
            atr_n,
            mult: 2.0,
            ema: EmaState::new(ema_n),
            prev_close: None,
            rma: RmaState::new(atr_n),
            last: vec![f64::NAN; 3],
        }
    }
    /// Parametric constructor — `p[0]` = EMA period, `p[1]` = ATR period,
    /// `p[2]` = band multiplier. Both periods are threaded into their
    /// [`EmaState`]/[`RmaState`] sub-state constructors, matching `new()`.
    pub fn with_params(p: &[f64]) -> Self {
        let ema_n = p.first().copied().unwrap_or(20.0).round() as usize;
        let atr_n = p.get(1).copied().unwrap_or(10.0).round() as usize;
        let mult = p.get(2).copied().unwrap_or(2.0);
        Self {
            ema_n,
            atr_n,
            mult,
            ema: EmaState::new(ema_n),
            prev_close: None,
            rma: RmaState::new(atr_n),
            last: vec![f64::NAN; 3],
        }
    }
}
impl Default for Keltner {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Keltner {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let (h, l, c) = (bar.high, bar.low, bar.close);
        // True range uses the *previous* close (matches `math::atr`), computed
        // before advancing the stored close.
        let tr = match self.prev_close {
            None => h - l,
            Some(pc) => (h - l).max((h - pc).abs()).max((l - pc).abs()),
        };
        self.prev_close = Some(c);
        let mid_opt = self.ema.push(c);
        let a_opt = self.rma.push(tr);
        let mid = mid_opt.unwrap_or(f64::NAN);
        let mult = self.mult;
        let out = match (mid_opt, a_opt) {
            (Some(midv), Some(a)) => vec![midv + mult * a, midv, midv - mult * a],
            _ => vec![f64::NAN, mid, f64::NAN],
        };
        self.last = out.clone();
        out
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_keltner(bars, self.ema_n, self.atr_n, self.mult)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.ema.reset();
        self.prev_close = None;
        self.rma.reset();
        self.last = vec![f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "keltner"
    }
    fn lookback(&self) -> usize {
        self.ema_n.saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // The mid line is the EMA; the bands additionally need the ATR, which lands
        // at `atr_n - 1` — either can dominate.
        self.ema_n.max(self.atr_n).saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `vwap` — TRUE O(1): running Σ(typical·vol) / Σ(vol), reset on each new UTC-day
/// session, identical to `batch_vwap`.
#[derive(Clone)]
pub struct Vwap {
    pv: f64,
    vol: f64,
    last_day: i64,
    last: Vec<f64>,
}
impl Vwap {
    pub fn new() -> Self {
        Self { pv: 0.0, vol: 0.0, last_day: i64::MIN, last: vec![f64::NAN] }
    }
}
impl Default for Vwap {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Vwap {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let day = bar.ts / 86_400_000; // UTC day (ts is epoch-ms); mirrors Columns
        if day != self.last_day {
            self.pv = 0.0;
            self.vol = 0.0;
            self.last_day = day;
        }
        let tp = (bar.high + bar.low + bar.close) / 3.0;
        self.pv += tp * bar.volume;
        self.vol += bar.volume;
        let out = if self.vol > 0.0 { self.pv / self.vol } else { f64::NAN };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_vwap(bars)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.pv = 0.0;
        self.vol = 0.0;
        self.last_day = i64::MIN;
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "vwap"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

/// `stochastic` — O(window) raw %K from rolling 14-bar extrema, then two 3-bar
/// `sma_nan` smoothings (%K, %D). Identical to `batch_stochastic`. A window of
/// raw/%K values is all-finite exactly when past the warm-up start, so the
/// streaming "mean iff last `n` all finite" reproduces `sma_nan` bit-for-bit.
/// Outputs `[k, d]`.
#[derive(Clone)]
pub struct Stochastic {
    n: usize,
    sk: usize,
    sd: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    raw_ring: VecDeque<f64>,
    k_ring: VecDeque<f64>,
    last: Vec<f64>,
}
impl Stochastic {
    pub fn new() -> Self {
        Self {
            n: 14,
            sk: 3,
            sd: 3,
            highs: VecDeque::new(),
            lows: VecDeque::new(),
            raw_ring: VecDeque::new(),
            k_ring: VecDeque::new(),
            last: vec![f64::NAN; 2],
        }
    }
    /// Parametric constructor — `p[0]` = %K period, `p[1]` = %K smoothing,
    /// `p[2]` = %D smoothing; all three rings start empty regardless of size.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(14.0).round() as usize;
        let sk = p.get(1).copied().unwrap_or(3.0).round() as usize;
        let sd = p.get(2).copied().unwrap_or(3.0).round() as usize;
        Self { n, sk, sd, ..Self::new() }
    }
    fn sma_nan_last(ring: &VecDeque<f64>, n: usize) -> f64 {
        if ring.len() == n && ring.iter().all(|v| !v.is_nan()) {
            ring.iter().sum::<f64>() / n as f64
        } else {
            f64::NAN
        }
    }
}
impl Default for Stochastic {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Stochastic {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let (n, sk, sd) = (self.n, self.sk, self.sd);
        self.highs.push_back(bar.high);
        self.lows.push_back(bar.low);
        if self.highs.len() > n {
            self.highs.pop_front();
            self.lows.pop_front();
        }
        let raw = if self.highs.len() == n {
            let hh = self.highs.iter().cloned().fold(f64::MIN, f64::max);
            let ll = self.lows.iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            if rng > 0.0 {
                100.0 * (bar.close - ll) / rng
            } else {
                0.0
            }
        } else {
            f64::NAN
        };
        self.raw_ring.push_back(raw);
        if self.raw_ring.len() > sk {
            self.raw_ring.pop_front();
        }
        let k = Self::sma_nan_last(&self.raw_ring, sk);
        self.k_ring.push_back(k);
        if self.k_ring.len() > sd {
            self.k_ring.pop_front();
        }
        let d = Self::sma_nan_last(&self.k_ring, sd);
        self.last = vec![k, d];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_stochastic(bars, self.n, self.sk, self.sd)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.highs.clear();
        self.lows.clear();
        self.raw_ring.clear();
        self.k_ring.clear();
        self.last = vec![f64::NAN; 2];
    }
    fn name(&self) -> &str {
        "stochastic"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1) + self.sk.saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // %D is an `sd`-bar SMA of %K, so it trails %K by `sd - 1`.
        self.n.saturating_sub(1) + self.sk.saturating_sub(1) + self.sd.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `cci` — O(window) two-pass mean + mean-absolute-deviation over the last `n`
/// typical prices, identical to `batch_cci`.
#[derive(Clone)]
pub struct Cci {
    n: usize,
    tp_window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Cci {
    pub fn new() -> Self {
        Self { n: 20, tp_window: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Cci {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Cci {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        let tp = (bar.high + bar.low + bar.close) / 3.0;
        self.tp_window.push_back(tp);
        if self.tp_window.len() > n {
            self.tp_window.pop_front();
        }
        let out = if self.tp_window.len() == n {
            let mean = self.tp_window.iter().sum::<f64>() / n as f64;
            let md = self.tp_window.iter().map(|v| (v - mean).abs()).sum::<f64>() / n as f64;
            if md > 0.0 {
                (tp - mean) / (0.015 * md)
            } else {
                0.0
            }
        } else {
            f64::NAN
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_cci(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.tp_window.clear();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "cci"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `williams` — O(window) Williams %R from rolling 14-bar extrema, identical to
/// `batch_williams`.
#[derive(Clone)]
pub struct Williams {
    n: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    last: Vec<f64>,
}
impl Williams {
    pub fn new() -> Self {
        Self { n: 14, highs: VecDeque::new(), lows: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(14.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Williams {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Williams {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        self.highs.push_back(bar.high);
        self.lows.push_back(bar.low);
        if self.highs.len() > n {
            self.highs.pop_front();
            self.lows.pop_front();
        }
        let out = if self.highs.len() == n {
            let hh = self.highs.iter().cloned().fold(f64::MIN, f64::max);
            let ll = self.lows.iter().cloned().fold(f64::MAX, f64::min);
            let rng = hh - ll;
            if rng > 0.0 {
                -100.0 * (hh - bar.close) / rng
            } else {
                0.0
            }
        } else {
            f64::NAN
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_williams(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.highs.clear();
        self.lows.clear();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "williams"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// `awesome` — SMA(5) − SMA(34) of the median price via two streaming [`SmaAcc`] window folds,
/// identical to `batch_awesome`.
///
/// ⚠ **NOT O(1) any more, and nothing bounds it.** `SmaAcc` carries no running sum since it was
/// de-accumulated to stay bit-identical to `math::sma`; each push is an O(n) fold. Measured
/// 6.79 ns/push at n = 34 against 412.70 ns at n = 1000 — and n is a user-settable registry
/// parameter (`crates/vike-indicators/src/registry.rs`'s `AWESOME_PARAMS`, range 1..=1000), not
/// the defaults. This indicator is hand-written, so `trims_history()` is `false` and no
/// [`crate::WindowReach`] retention applies: it is the one site in the crate where the
/// O(1) -> O(n) trade has no counterweight.
#[derive(Clone)]
pub struct Awesome {
    fast_n: usize,
    slow_n: usize,
    fast: SmaAcc,
    slow: SmaAcc,
    last: Vec<f64>,
}
impl Awesome {
    pub fn new() -> Self {
        let fast_n = 5;
        let slow_n = 34;
        Self {
            fast_n,
            slow_n,
            fast: SmaAcc::new(fast_n),
            slow: SmaAcc::new(slow_n),
            last: vec![f64::NAN],
        }
    }
    /// Parametric constructor — `p[0]` = fast period, `p[1]` = slow period,
    /// both threaded into their [`SmaAcc`] sliding-sum sub-states.
    pub fn with_params(p: &[f64]) -> Self {
        let fast_n = p.first().copied().unwrap_or(5.0).round() as usize;
        let slow_n = p.get(1).copied().unwrap_or(34.0).round() as usize;
        Self {
            fast_n,
            slow_n,
            fast: SmaAcc::new(fast_n),
            slow: SmaAcc::new(slow_n),
            last: vec![f64::NAN],
        }
    }
}
impl Default for Awesome {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Awesome {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let mp = (bar.high + bar.low) / 2.0;
        let sf = self.fast.push(mp);
        let ss = self.slow.push(mp);
        let out = match (sf, ss) {
            (Some(f), Some(s)) => f - s,
            _ => f64::NAN,
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_awesome(bars, self.fast_n, self.slow_n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.fast.reset();
        self.slow.reset();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "awesome"
    }
    fn lookback(&self) -> usize {
        self.fast_n.max(self.slow_n).saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

// ========================= true-incremental family =========================

/// Simple moving average — a naive left fold over the retained window, recomputed each bar,
/// identical to `math::sma`.
///
/// ⚠ **This was O(1)** (a seed fold over the first `n`, then `sum += c[i] - c[i-n]`) **and is now
/// O(n) per bar.** That is a genuine, deliberate regression on this one indicator, and it is the
/// price of the property `math::sma`'s doc explains: a carried sum is not a function of its
/// window, so the identical window returns different bits once history is truncated. `Sma` itself
/// never truncates (it is hand-written incremental, `trims_history() == false`), but it MUST match
/// `math::sma` bit-for-bit, and `math::sma` is the kernel the macro-generated indicators DO trim.
/// Measured 3.6 ns/bar at `n = 20` and 139 ns/bar at `n = 400`, i.e. a 300k-bar chart refold goes
/// ~0.3 ms -> 1.1 ms / 42 ms — dwarfed by what the same change saves elsewhere (`stddev` at
/// `period = 20` refolds 3.1 s -> 0.33 s), but real, and this docstring no longer advertises O(1).
///
/// The fold runs over `make_contiguous()` for the same reason `state.rs`'s `SmaAcc` does: it makes
/// this the same slice-fold code path as `math::sma` by construction.
#[derive(Clone)]
pub struct Sma {
    n: usize,
    window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Sma {
    pub fn new() -> Self {
        Self { n: 20, window: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor for [`crate::registry::IndicatorMeta::make_with`] —
    /// `p[0]` = period (assumed pre-coerced; a missing entry falls back to the
    /// default). No sub-state depends on `n` at construction, so the rest of the
    /// fields reuse `new()`'s empty state via struct-update.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Sma {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Sma {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let n = self.n;
        self.window.push_back(bar.close);
        if self.window.len() > n {
            self.window.pop_front();
        }
        // ⚠ `n == 0` used to reach `pop_front().unwrap()` on an empty deque and PANIC; `math::sma`
        // returns all-NaN for it, so NaN is the answer that AGREES with `vectorize`.
        let out = if n > 0 && self.window.len() == n {
            self.window.make_contiguous().iter().sum::<f64>() / n as f64
        } else {
            f64::NAN
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_sma(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.window.clear();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "sma"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Exponential moving average — the [`EmaState`] recurrence, identical to `math::ema`.
#[derive(Clone)]
pub struct Ema {
    n: usize,
    state: EmaState,
    last: Vec<f64>,
}
impl Ema {
    pub fn new() -> Self {
        let n = 20;
        Self { n, state: EmaState::new(n), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period, threaded into BOTH the `n`
    /// field and the [`EmaState`] recurrence (the sub-state must be built from
    /// the same coerced period, else `make_with` at non-default params would
    /// silently seed the alpha recurrence with the wrong `n`).
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(20.0).round() as usize;
        Self { n, state: EmaState::new(n), last: vec![f64::NAN] }
    }
}
impl Default for Ema {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Ema {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let out = self.state.push(bar.close).unwrap_or(f64::NAN);
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_ema(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.state.reset();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "ema"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// RSI — Wilder averages via the same seed-then-recurrence fold as `math`'s batch RSI.
#[derive(Clone)]
pub struct Rsi {
    n: usize,
    prev_close: Option<f64>,
    seen: usize,
    ag: f64,
    al: f64,
    last: Vec<f64>,
}
impl Rsi {
    pub fn new() -> Self {
        Self { n: 14, prev_close: None, seen: 0, ag: 0.0, al: 0.0, last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it
    /// (the Wilder averages are plain fields, seeded lazily in `on_bar`).
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(14.0).round() as usize;
        Self { n, ..Self::new() }
    }
    fn rsi_from(ag: f64, al: f64) -> f64 {
        if al == 0.0 {
            100.0
        } else {
            100.0 - 100.0 / (1.0 + ag / al)
        }
    }
}
impl Default for Rsi {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Rsi {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let c = bar.close;
        let n = self.n;
        let i = self.seen; // current bar index
        let out = match self.prev_close {
            None => f64::NAN,
            Some(pc) => {
                let d = c - pc;
                let g = if d > 0.0 { d } else { 0.0 };
                let l = if d < 0.0 { -d } else { 0.0 };
                if i <= n {
                    // seed window: accumulate gains[1..=n] / losses[1..=n]
                    self.ag += g;
                    self.al += l;
                    if i == n {
                        self.ag /= n as f64;
                        self.al /= n as f64;
                        Self::rsi_from(self.ag, self.al)
                    } else {
                        f64::NAN
                    }
                } else {
                    self.ag = (self.ag * (n as f64 - 1.0) + g) / n as f64;
                    self.al = (self.al * (n as f64 - 1.0) + l) / n as f64;
                    Self::rsi_from(self.ag, self.al)
                }
            }
        };
        self.prev_close = Some(c);
        self.seen += 1;
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_rsi(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.prev_close = None;
        self.seen = 0;
        self.ag = 0.0;
        self.al = 0.0;
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "rsi"
    }
    fn lookback(&self) -> usize {
        self.n
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// ATR — true range fed through [`RmaState`], identical to `math::atr`.
#[derive(Clone)]
pub struct Atr {
    n: usize,
    prev_close: Option<f64>,
    rma: RmaState,
    last: Vec<f64>,
}
impl Atr {
    pub fn new() -> Self {
        let n = 14;
        Self { n, prev_close: None, rma: RmaState::new(n), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period, threaded into both the `n`
    /// field and the [`RmaState`] sub-state.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(14.0).round() as usize;
        Self { n, prev_close: None, rma: RmaState::new(n), last: vec![f64::NAN] }
    }
}
impl Default for Atr {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Atr {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let (h, l) = (bar.high, bar.low);
        let tr = match self.prev_close {
            None => h - l,
            Some(pc) => (h - l).max((h - pc).abs()).max((l - pc).abs()),
        };
        self.prev_close = Some(bar.close);
        let out = self.rma.push(tr).unwrap_or(f64::NAN);
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_atr(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.prev_close = None;
        self.rma.reset();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "atr"
    }
    fn lookback(&self) -> usize {
        self.n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Rate of change — `100*(c[i]-c[i-n])/c[i-n]` from an `n`-deep close ring.
#[derive(Clone)]
pub struct Roc {
    n: usize,
    buf: VecDeque<f64>,
    last: Vec<f64>,
}
impl Roc {
    pub fn new() -> Self {
        Self { n: 10, buf: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub fn with_params(p: &[f64]) -> Self {
        let n = p.first().copied().unwrap_or(10.0).round() as usize;
        Self { n, ..Self::new() }
    }
}
impl Default for Roc {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Roc {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let c = bar.close;
        let n = self.n;
        let out = if self.buf.len() == n {
            let cn = *self.buf.front().unwrap(); // c[i-n]
            let r = if cn != 0.0 { 100.0 * (c - cn) / cn } else { f64::NAN };
            self.buf.pop_front();
            self.buf.push_back(c);
            r
        } else {
            self.buf.push_back(c);
            f64::NAN
        };
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_roc(bars, self.n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.buf.clear();
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "roc"
    }
    fn lookback(&self) -> usize {
        self.n
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// On-balance volume — running signed-volume accumulator, identical to `math`'s batch OBV.
#[derive(Clone)]
pub struct Obv {
    prev_close: Option<f64>,
    obv: f64,
    last: Vec<f64>,
}
impl Obv {
    pub fn new() -> Self {
        Self { prev_close: None, obv: 0.0, last: vec![f64::NAN] }
    }
}
impl Default for Obv {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Obv {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let (c, v) = (bar.close, bar.volume);
        let out = match self.prev_close {
            None => {
                self.obv = 0.0;
                0.0
            }
            Some(pc) => {
                self.obv += if c > pc {
                    v
                } else if c < pc {
                    -v
                } else {
                    0.0
                };
                self.obv
            }
        };
        self.prev_close = Some(c);
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_obv(bars)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.prev_close = None;
        self.obv = 0.0;
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "obv"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}

/// MACD — three [`EmaState`]s (fast/slow diff, then signal EMA of the diff),
/// reproducing `math`'s batch MACD fold exactly. Outputs `[macd, signal, hist]`.
#[derive(Clone)]
pub struct Macd {
    fast_n: usize,
    slow_n: usize,
    sig_n: usize,
    fast: EmaState,
    slow: EmaState,
    sig: EmaState,
    last: Vec<f64>,
}
impl Macd {
    pub fn new() -> Self {
        let fast_n = 12;
        let slow_n = 26;
        let sig_n = 9;
        Self {
            fast_n,
            slow_n,
            sig_n,
            fast: EmaState::new(fast_n),
            slow: EmaState::new(slow_n),
            sig: EmaState::new(sig_n),
            last: vec![f64::NAN; 3],
        }
    }
    /// Parametric constructor — `p[0]` = fast period, `p[1]` = slow period,
    /// `p[2]` = signal period, each threaded into its own [`EmaState`].
    pub fn with_params(p: &[f64]) -> Self {
        let fast_n = p.first().copied().unwrap_or(12.0).round() as usize;
        let slow_n = p.get(1).copied().unwrap_or(26.0).round() as usize;
        let sig_n = p.get(2).copied().unwrap_or(9.0).round() as usize;
        Self {
            fast_n,
            slow_n,
            sig_n,
            fast: EmaState::new(fast_n),
            slow: EmaState::new(slow_n),
            sig: EmaState::new(sig_n),
            last: vec![f64::NAN; 3],
        }
    }
}
impl Default for Macd {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Macd {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let c = bar.close;
        let ef = self.fast.push(c);
        let es = self.slow.push(c);
        let macd = match (ef, es) {
            (Some(f), Some(s)) => Some(f - s),
            _ => None,
        };
        // Signal EMA only advances on live MACD values (mirrors ema(macd_line[start..])).
        let signal = match macd {
            Some(m) => self.sig.push(m),
            None => None,
        };
        let hist = match (macd, signal) {
            (Some(m), Some(s)) => Some(m - s),
            _ => None,
        };
        let out =
            vec![macd.unwrap_or(f64::NAN), signal.unwrap_or(f64::NAN), hist.unwrap_or(f64::NAN)];
        self.last = out.clone();
        out
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_macd(bars, self.fast_n, self.slow_n, self.sig_n)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        self.fast.reset();
        self.slow.reset();
        self.sig.reset();
        self.last = vec![f64::NAN; 3];
    }
    fn name(&self) -> &str {
        "macd"
    }
    fn lookback(&self) -> usize {
        self.fast_n.max(self.slow_n).saturating_sub(1)
    }
    fn lookback_full(&self) -> usize {
        // The signal EMA (and hist) trail the macd line by `signal_n - 1`.
        self.fast_n.max(self.slow_n).saturating_sub(1) + self.sig_n.saturating_sub(1)
    }
    fn lookback_exact(&self) -> bool {
        true
    }
}

/// Parabolic SAR — the forward recurrence streamed directly (buffer-recompute
/// can't reproduce it: the batch's `len < 2` guard makes a 1-bar prefix NaN at
/// index 0, whereas the full series sets `out[0] = low[0]`). Keeps the last two
/// highs/lows so `on_bar` matches `batch_psar` bit-for-bit for series of >= 2 bars.
#[derive(Clone)]
pub struct Psar {
    step: f64,
    max_af: f64,
    seen: usize,
    up: bool,
    sar: f64,
    ep: f64,
    af: f64,
    pl1: f64, // low[i-1]
    pl2: f64, // low[i-2]
    ph1: f64, // high[i-1]
    ph2: f64, // high[i-2]
    last: Vec<f64>,
}
impl Psar {
    pub fn new() -> Self {
        Self {
            step: 0.02,
            max_af: 0.20,
            seen: 0,
            up: true,
            sar: 0.0,
            ep: 0.0,
            af: 0.0,
            pl1: 0.0,
            pl2: 0.0,
            ph1: 0.0,
            ph2: 0.0,
            last: vec![f64::NAN],
        }
    }
    /// Parametric constructor — `p[0]` = acceleration step, `p[1]` = max
    /// acceleration factor. Both are f64 multipliers (not periods): read
    /// directly, no rounding. No sub-state depends on either at construction
    /// (the forward recurrence reads `self.step`/`self.max_af` at fold time).
    pub fn with_params(p: &[f64]) -> Self {
        let step = p.first().copied().unwrap_or(0.02);
        let max_af = p.get(1).copied().unwrap_or(0.20);
        Self { step, max_af, ..Self::new() }
    }
}
impl Default for Psar {
    fn default() -> Self {
        Self::new()
    }
}
impl Indicator for Psar {
    fn on_bar(&mut self, bar: &Bar) -> Vec<f64> {
        let (h, l) = (bar.high, bar.low);
        let step = self.step;
        let max_af = self.max_af;
        let out = if self.seen == 0 {
            self.up = true;
            self.sar = l;
            self.ep = h;
            self.af = step;
            self.sar
        } else {
            let i = self.seen;
            let mut s = self.sar + self.af * (self.ep - self.sar);
            if self.up {
                let l2 = if i >= 2 { self.pl2 } else { self.pl1 };
                s = s.min(self.pl1).min(l2);
                if l < s {
                    self.up = false;
                    s = self.ep;
                    self.ep = l;
                    self.af = step;
                } else if h > self.ep {
                    self.ep = h;
                    self.af = (self.af + step).min(max_af);
                }
            } else {
                let h2 = if i >= 2 { self.ph2 } else { self.ph1 };
                s = s.max(self.ph1).max(h2);
                if h > s {
                    self.up = true;
                    s = self.ep;
                    self.ep = h;
                    self.af = step;
                } else if l < self.ep {
                    self.ep = l;
                    self.af = (self.af + step).min(max_af);
                }
            }
            self.sar = s;
            self.sar
        };
        // shift the two-deep high/low history (used for the SAR clamp).
        self.pl2 = self.pl1;
        self.pl1 = l;
        self.ph2 = self.ph1;
        self.ph1 = h;
        self.seen += 1;
        self.last = vec![out];
        self.last.clone()
    }
    fn vectorize(&self, bars: &[Bar]) -> Vec<Vec<f64>> {
        batch_psar(bars, self.step, self.max_af)
    }
    fn value(&self) -> Vec<f64> {
        self.last.clone()
    }
    fn reset(&mut self) {
        // State fields only — `step`/`max_af` are params and must survive reset.
        self.seen = 0;
        self.up = true;
        self.sar = 0.0;
        self.ep = 0.0;
        self.af = 0.0;
        self.pl1 = 0.0;
        self.pl2 = 0.0;
        self.ph1 = 0.0;
        self.ph2 = 0.0;
        self.last = vec![f64::NAN];
    }
    fn name(&self) -> &str {
        "psar"
    }
    fn lookback(&self) -> usize {
        0
    }
    fn lookback_exact(&self) -> bool {
        true
    }
    fn warmup_path_dependent(&self) -> bool {
        true
    }
}
