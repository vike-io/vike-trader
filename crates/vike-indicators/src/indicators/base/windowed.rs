//! The rolling-window incremental family: the nine indicators under that banner and their kernels.

use crate::Indicator;
use crate::indicators::state::{EmaState, RmaState, SmaAcc};
use crate::math::{Columns, atr, ema, sma, sma_nan, sq, wma};
use std::collections::VecDeque;
use vike_marketdata::Bar;

// ============================ batch kernels ================================
// One per indicator; these are the c_*(&Columns) functions from the oracle,
// re-headed to build Columns from &[Bar]. Do NOT "improve" the formulas.

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
pub(crate) struct Wma {
    n: usize,
    window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Wma {
    pub(crate) fn new() -> Self {
        Self { n: 20, window: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; the rolling window starts
    /// empty regardless of `n`, so the rest of `new()`'s state is reused.
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
pub(crate) struct Bollinger {
    n: usize,
    m: f64,
    window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Bollinger {
    pub(crate) fn new() -> Self {
        Self { n: 20, m: 2.0, window: VecDeque::new(), last: vec![f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period (integer, rounded), `p[1]` =
    /// band multiplier (`m`, displayed as `"mult"` in the registry spec — the
    /// field name is unchanged). No sub-state depends on either.
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
pub(crate) struct Donchian {
    n: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    last: Vec<f64>,
}
impl Donchian {
    pub(crate) fn new() -> Self {
        Self { n: 20, highs: VecDeque::new(), lows: VecDeque::new(), last: vec![f64::NAN; 3] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
pub(crate) struct Keltner {
    ema_n: usize,
    atr_n: usize,
    mult: f64,
    ema: EmaState,
    prev_close: Option<f64>,
    rma: RmaState,
    last: Vec<f64>,
}
impl Keltner {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
pub(crate) struct Vwap {
    pv: f64,
    vol: f64,
    last_day: i64,
    last: Vec<f64>,
}
impl Vwap {
    pub(crate) fn new() -> Self {
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
pub(crate) struct Stochastic {
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
    pub(crate) fn new() -> Self {
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
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
            if rng > 0.0 { 100.0 * (bar.close - ll) / rng } else { 0.0 }
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
pub(crate) struct Cci {
    n: usize,
    tp_window: VecDeque<f64>,
    last: Vec<f64>,
}
impl Cci {
    pub(crate) fn new() -> Self {
        Self { n: 20, tp_window: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
            if md > 0.0 { (tp - mean) / (0.015 * md) } else { 0.0 }
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
pub(crate) struct Williams {
    n: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    last: Vec<f64>,
}
impl Williams {
    pub(crate) fn new() -> Self {
        Self { n: 14, highs: VecDeque::new(), lows: VecDeque::new(), last: vec![f64::NAN] }
    }
    /// Parametric constructor — `p[0]` = period; no sub-state depends on it.
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
            if rng > 0.0 { -100.0 * (hh - bar.close) / rng } else { 0.0 }
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
/// parameter (`crates/vike-indicators/src/registry/base.rs`'s `AWESOME_PARAMS`, range 1..=1000), not
/// the defaults. This indicator is hand-written, so `trims_history()` is `false` and no
/// [`crate::WindowReach`] retention applies: it is the one site in the crate where the
/// O(1) -> O(n) trade has no counterweight.
#[derive(Clone)]
pub(crate) struct Awesome {
    fast_n: usize,
    slow_n: usize,
    fast: SmaAcc,
    slow: SmaAcc,
    last: Vec<f64>,
}
impl Awesome {
    pub(crate) fn new() -> Self {
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
    pub(crate) fn with_params(p: &[f64]) -> Self {
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
