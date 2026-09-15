//! The 2-series (pairs) indicator seam — faithful f64 ports of the
//! `inputs=["close","benchmark"]` indicators from vike-trader-app
//! `core/indicators/pairs.py` (ratio/spread/spread_zscore) and `statistics.py`
//! (beta/correl/correl_log). These read a SECOND instrument (`benchmark`) that a
//! single `&[Bar]` / `&Bar` cannot supply, so they live behind their own
//! [`PairIndicator`] trait + [`pair_registry`], isolated from the single-series
//! [`crate::Indicator`] seam and the vike-core hot path.
//!
//! Streaming is history-recompute (both series pushed together, aligned), so
//! `on_pair`-fold == `vectorize_pair` bit-for-bit — gated in `tests/pairs_parity.rs`.
//! Wiring a live benchmark feed is out of scope (no second-instrument live seam
//! exists yet); the indicators + their parity gate ship regardless.
//!
//! ## Mean-reversion statistics (NOT ports — no Python twin)
//!
//! [`HalfLife`] and [`KalmanBeta`] are new here. They are STATISTICS, not a
//! strategy: each returns a plottable number and decides nothing. The trading
//! rule that consumes them (entry/exit bands, sizing, orders) belongs in a
//! strategy, never in this crate.
//!
//! Both are deliberately multi-consumer, which is why they sit on this shared
//! seam rather than inside one strategy:
//!   * [`HalfLife`] — how fast a spread reverts, in bars. Its use is a REGIME
//!     GATE: a half-life long relative to the intended holding period says the
//!     spread will not revert before costs and stops bite, so stand down. That
//!     applies to a maker's regime filter as much as to a pair trade.
//!   * [`KalmanBeta`] — a time-varying hedge ratio, the principled alternative
//!     to the fixed `beta = 1` a naive spread assumes, and the input any
//!     two-leg hedge sizing needs.
//!
//! [`SpreadZscore`] and [`HalfLife`] take that hedge ratio as the `beta`
//! parameter: the spread is `close − beta·benchmark`, so `beta = 1.0` (the
//! default) reproduces the previous hardcoded arithmetic BIT-FOR-BIT
//! (multiplying by 1.0 is exact in IEEE-754).
#![allow(clippy::needless_range_loop)]

use crate::indicators::{pback, pbars};
use crate::math::sq;
use crate::registry::{OutSpec, ParamSpec, coerce};
use std::sync::OnceLock;
use vike_model::Bar;

/// Object-safe clone for boxed pair indicators (mirror of [`crate::BoxedClone`]).
pub trait BoxedClonePair {
    fn clone_box(&self) -> Box<dyn PairIndicator>;
}
impl<T: PairIndicator + Clone + 'static> BoxedClonePair for T {
    fn clone_box(&self) -> Box<dyn PairIndicator> {
        Box::new(self.clone())
    }
}

/// A 2-series indicator over a primary instrument + an aligned `benchmark`.
/// The streaming/batch pair is one source of truth (`tests/pairs_parity.rs`).
pub trait PairIndicator: BoxedClonePair {
    /// Streaming: feed the next aligned (primary, benchmark) bar pair, advance,
    /// and return this bar's output line(s). Warm-up returns `f64::NAN`.
    fn on_pair(&mut self, primary: &Bar, benchmark: &Bar) -> Vec<f64>;
    /// Batch over two aligned series — the parity reference.
    fn vectorize_pair(&self, primary: &[Bar], benchmark: &[Bar]) -> Vec<Vec<f64>>;
    /// Most recent `on_pair` output without advancing.
    fn value(&self) -> Vec<f64>;
    /// Clear streaming state back to construction.
    fn reset(&mut self);
    /// Registry key (e.g. "ratio").
    fn name(&self) -> &str;

    /// Warm-up depth — the pair twin of [`crate::Indicator::lookback`]: the index
    /// of the first bar at which `vectorize_pair` yields a non-NaN value on any
    /// output line. Exact when [`PairIndicator::lookback_exact`], else the
    /// conservative `0` default. Gated in `tests/lookback.rs`.
    fn lookback(&self) -> usize {
        0
    }

    /// FULL warm-up depth — the pair twin of [`crate::Indicator::lookback_full`]:
    /// the first index at which EVERY output line is non-NaN. Defaults to
    /// [`PairIndicator::lookback`] (correct for every single-line pair indicator);
    /// the gate in `tests/lookback.rs` fails any staggered one that forgets to
    /// override it.
    fn lookback_full(&self) -> usize {
        self.lookback()
    }

    /// `true` when [`PairIndicator::lookback`] is a real param-derived override.
    fn lookback_exact(&self) -> bool {
        false
    }
}

/// History-recompute streaming tail for pair indicators (both histories carry the
/// just-pushed current bar; aligned). Correct-by-construction for causal pairs.
fn stream_pair_tail(
    a: &[Bar],
    b: &[Bar],
    batch: impl Fn(&[Bar], &[Bar]) -> Vec<Vec<f64>>,
    arity: usize,
) -> Vec<f64> {
    let cols = batch(a, b);
    if cols.is_empty() {
        return vec![f64::NAN; arity];
    }
    cols.iter().map(|line| line.last().copied().unwrap_or(f64::NAN)).collect()
}

fn closes(bars: &[Bar]) -> Vec<f64> {
    bars.iter().map(|x| x.close).collect()
}

macro_rules! pair_indicator {
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$a:ident, $b:ident| $body:expr
    ) => {
        pair_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $arity, [ $( $pf = $pdef ),* ],
            |$a, $b| $body,
            lookback = 0, exact = false
        }
    };
    // Same, plus an EXACT warm-up expression over the param locals.
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$a:ident, $b:ident| $body:expr,
        lookback = $lb:expr
    ) => {
        pair_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $arity, [ $( $pf = $pdef ),* ],
            |$a, $b| $body,
            lookback = $lb, exact = true
        }
    };
    (
        @impl
        $(#[$m:meta])*
        $Name:ident, $key:literal, $arity:expr, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        |$a:ident, $b:ident| $body:expr,
        lookback = $lb:expr, exact = $exact:expr
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            $( $pf: f64, )*
            hist_a: Vec<Bar>,
            hist_b: Vec<Bar>,
            last: Vec<f64>,
        }
        impl $Name {
            pub fn new() -> Self {
                Self { $( $pf: $pdef, )* hist_a: Vec::new(), hist_b: Vec::new(), last: vec![f64::NAN; $arity] }
            }
            #[allow(unused_variables, unused_mut, unused_assignments)]
            pub fn with_params(p: &[f64]) -> Self {
                let mut s = Self::new();
                let mut i = 0usize;
                $( s.$pf = p.get(i).copied().unwrap_or($pdef); i += 1; )*
                s
            }
        }
        impl Default for $Name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl PairIndicator for $Name {
            fn on_pair(&mut self, primary: &Bar, benchmark: &Bar) -> Vec<f64> {
                self.hist_a.push(primary.clone());
                self.hist_b.push(benchmark.clone());
                $( let $pf = self.$pf; )*
                let out = stream_pair_tail(
                    &self.hist_a,
                    &self.hist_b,
                    |$a: &[Bar], $b: &[Bar]| $body,
                    $arity,
                );
                self.last = out.clone();
                out
            }
            fn vectorize_pair(&self, primary: &[Bar], benchmark: &[Bar]) -> Vec<Vec<f64>> {
                $( let $pf = self.$pf; )*
                let $a = primary;
                let $b = benchmark;
                $body
            }
            fn value(&self) -> Vec<f64> {
                self.last.clone()
            }
            fn reset(&mut self) {
                self.hist_a.clear();
                self.hist_b.clear();
                self.last = vec![f64::NAN; $arity];
            }
            fn name(&self) -> &str {
                $key
            }
            #[allow(unused_variables)]
            fn lookback(&self) -> usize {
                $( let $pf = self.$pf; )*
                $lb
            }
            fn lookback_exact(&self) -> bool {
                $exact
            }
        }
    };
}

// ============================ batch kernels ================================

fn batch_ratio(a: &[Bar], b: &[Bar]) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    for i in 0..n {
        if cb[i] != 0.0 {
            out[i] = ca[i] / cb[i];
        }
    }
    vec![out]
}

fn batch_spread(a: &[Bar], b: &[Bar], log: i64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    if log == 0 {
        for i in 0..n {
            out[i] = ca[i] - cb[i];
        }
    } else {
        for i in 0..n {
            if ca[i] > 0.0 && cb[i] > 0.0 {
                out[i] = libm::log(ca[i]) - libm::log(cb[i]);
            }
        }
    }
    vec![out]
}

/// Rolling z-score of the hedge-ratio spread `s = close − beta·benchmark`.
/// `beta = 1.0` reproduces the original unhedged `close − benchmark` bit-for-bit.
fn batch_spread_zscore(a: &[Bar], b: &[Bar], period: usize, beta: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let s: Vec<f64> = (0..n).map(|i| ca[i] - beta * cb[i]).collect();
    let mut out = vec![f64::NAN; n];
    let (mut run_sum, mut run_sum2) = (0.0, 0.0);
    for i in 0..n {
        run_sum += s[i];
        run_sum2 += s[i] * s[i];
        if i >= period {
            run_sum -= s[i - period];
            run_sum2 -= s[i - period] * s[i - period];
        }
        if i + 1 >= period {
            let mean = run_sum / period as f64;
            let var = run_sum2 / period as f64 - mean * mean;
            let sd = var.max(0.0).sqrt();
            if sd != 0.0 {
                out[i] = (s[i] - mean) / sd;
            }
        }
    }
    vec![out]
}

fn batch_beta(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let (mut ret_a, mut ret_b) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 1..n {
        if ca[i - 1] != 0.0 {
            ret_a[i] = (ca[i] - ca[i - 1]) / ca[i - 1];
        }
        if cb[i - 1] != 0.0 {
            ret_b[i] = (cb[i] - cb[i - 1]) / cb[i - 1];
        }
    }
    let (mut buf_a, mut buf_b): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 0..n {
        if !ret_a[i].is_nan() && !ret_b[i].is_nan() {
            buf_a.push(ret_a[i]);
            buf_b.push(ret_b[i]);
        } else {
            buf_a.clear();
            buf_b.clear();
            continue;
        }
        if buf_a.len() > period {
            buf_a.remove(0);
            buf_b.remove(0);
        }
        if buf_a.len() == period {
            let mean_a = buf_a.iter().sum::<f64>() / period as f64;
            let mean_b = buf_b.iter().sum::<f64>() / period as f64;
            let cov = (0..period).map(|j| (buf_a[j] - mean_a) * (buf_b[j] - mean_b)).sum::<f64>()
                / period as f64;
            let vb = (0..period).map(|j| sq(buf_b[j] - mean_b)).sum::<f64>() / period as f64;
            if vb != 0.0 {
                out[i] = cov / vb;
            }
        }
    }
    vec![out]
}

fn batch_correl(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let (mut run_a, mut run_b, mut run_a2, mut run_b2, mut run_ab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        run_a += ca[i];
        run_b += cb[i];
        run_a2 += ca[i] * ca[i];
        run_b2 += cb[i] * cb[i];
        run_ab += ca[i] * cb[i];
        if i >= period {
            let (oa, ob) = (ca[i - period], cb[i - period]);
            run_a -= oa;
            run_b -= ob;
            run_a2 -= oa * oa;
            run_b2 -= ob * ob;
            run_ab -= oa * ob;
        }
        if i + 1 >= period {
            let p = period as f64;
            let num = p * run_ab - run_a * run_b;
            let denom =
                ((p * run_a2 - run_a * run_a) * (p * run_b2 - run_b * run_b)).max(0.0).sqrt();
            if denom != 0.0 {
                out[i] = (num / denom).clamp(-1.0, 1.0);
            }
        }
    }
    vec![out]
}

fn batch_correl_log(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let (mut lr_a, mut lr_b) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for i in 1..n {
        if ca[i] > 0.0 && ca[i - 1] > 0.0 {
            lr_a[i] = libm::log(ca[i] / ca[i - 1]);
        }
        if cb[i] > 0.0 && cb[i - 1] > 0.0 {
            lr_b[i] = libm::log(cb[i] / cb[i - 1]);
        }
    }
    let mut out = vec![f64::NAN; n];
    let (mut buf_a, mut buf_b): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 1..n {
        if !lr_a[i].is_nan() && !lr_b[i].is_nan() {
            buf_a.push(lr_a[i]);
            buf_b.push(lr_b[i]);
        } else {
            buf_a.clear();
            buf_b.clear();
            continue;
        }
        if buf_a.len() > period {
            buf_a.remove(0);
            buf_b.remove(0);
        }
        if buf_a.len() == period {
            let p = period as f64;
            let sa: f64 = buf_a.iter().sum();
            let sb: f64 = buf_b.iter().sum();
            let sa2: f64 = buf_a.iter().map(|x| x * x).sum();
            let sb2: f64 = buf_b.iter().map(|x| x * x).sum();
            let sab: f64 = (0..period).map(|j| buf_a[j] * buf_b[j]).sum();
            let num = p * sab - sa * sb;
            let denom = ((p * sa2 - sa * sa) * (p * sb2 - sb * sb)).max(0.0).sqrt();
            if denom != 0.0 {
                out[i] = (num / denom).clamp(-1.0, 1.0);
            }
        }
    }
    vec![out]
}

/// Rolling Ornstein-Uhlenbeck / AR(1) HALF-LIFE of mean reversion, in bars, for
/// the hedge-ratio spread `s = close − beta·benchmark`.
///
/// Over each trailing window of `period` observations, fits the discrete
/// mean-reversion regression `Δs_t = a + λ·s_{t−1}` by OLS —
/// `λ = cov(s_lag, Δs) / var(s_lag)` — and reports `−ln 2 / λ`, the time for a
/// deviation to decay by half.
///
/// NaN is a MEANINGFUL output here, not merely warm-up: only a mean-REVERTING
/// window has `λ < 0`. A window that is trending or random-walking (`λ >= 0`)
/// has no finite half-life, and a degenerate window (`var(s_lag) == 0`) has no
/// slope at all. Both yield NaN — the signal to stand down rather than a number
/// to trade on. Naive folds, matching [`batch_beta`].
fn batch_half_life(a: &[Bar], b: &[Bar], period: usize, beta: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    if period == 0 {
        return vec![out];
    }
    let s: Vec<f64> = (0..n).map(|i| ca[i] - beta * cb[i]).collect();
    let (mut lag, mut dif): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
    for i in 1..n {
        lag.push(s[i - 1]);
        dif.push(s[i] - s[i - 1]);
        if lag.len() > period {
            lag.remove(0);
            dif.remove(0);
        }
        if lag.len() == period {
            let p = period as f64;
            let mean_l = lag.iter().sum::<f64>() / p;
            let mean_d = dif.iter().sum::<f64>() / p;
            let cov = (0..period).map(|j| (lag[j] - mean_l) * (dif[j] - mean_d)).sum::<f64>() / p;
            let var_l = (0..period).map(|j| (lag[j] - mean_l) * (lag[j] - mean_l)).sum::<f64>() / p;
            if var_l != 0.0 {
                let lambda = cov / var_l;
                // Only a reverting window (λ < 0) has a finite, positive half-life.
                if lambda < 0.0 {
                    out[i] = -std::f64::consts::LN_2 / lambda;
                }
            }
        }
    }
    vec![out]
}

/// Time-varying hedge ratio from a 2-state Kalman filter — the dynamic
/// alternative to a fixed `beta`.
///
/// State `x = [beta, alpha]`, scalar observation `close_t = beta·benchmark_t +
/// alpha + v` with `v ~ N(0, ve)`. The state follows a random walk with
/// process covariance `Vw = delta/(1 − delta) · I`, so `delta` sets how fast the
/// hedge ratio is allowed to drift (smaller = steadier) and `ve` how much
/// observation noise is assumed. Per bar:
///
/// ```text
/// R = P + Vw                 (predicted state covariance)
/// F = [benchmark_t, 1]       (observation row)
/// Q = F·R·Fᵀ + ve            (innovation variance, scalar)
/// K = R·Fᵀ / Q               (gain)
/// x = x + K·(close_t − F·x)  (update)
/// P = R − K·(F·R)
/// ```
///
/// Emits `beta` only; `alpha` stays internal. Unlike the windowed statistics
/// above this recursion is O(1) per bar with bounded state, so it is the one
/// pair statistic here that is cheap enough for a live path. (The seam still
/// STREAMS it by history-recompute, which keeps `on_pair == vectorize_pair`
/// bit-exact; a live consumer would drive the recursion directly.)
fn batch_kalman_beta(a: &[Bar], b: &[Bar], delta: f64, ve: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let n = ca.len();
    let mut out = vec![f64::NAN; n];
    let denom = 1.0 - delta;
    let vw = if denom != 0.0 { delta / denom } else { 0.0 };
    let (mut x_beta, mut x_alpha) = (0.0f64, 0.0f64);
    let (mut p00, mut p01, mut p10, mut p11) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        // R = P + Vw·I
        let (r00, r01, r10, r11) = (p00 + vw, p01, p10, p11 + vw);
        // F = [benchmark, 1]
        let f0 = cb[i];
        let yhat = f0 * x_beta + x_alpha;
        // R·Fᵀ, then the scalar innovation variance Q.
        let rf0 = r00 * f0 + r01;
        let rf1 = r10 * f0 + r11;
        let q = f0 * rf0 + rf1 + ve;
        if !q.is_finite() || q == 0.0 {
            continue; // no usable gain — hold state, leave this bar NaN
        }
        let e = ca[i] - yhat;
        let (k0, k1) = (rf0 / q, rf1 / q);
        x_beta += k0 * e;
        x_alpha += k1 * e;
        // P = R − K·(F·R); R is symmetric so F·R == (R·Fᵀ)ᵀ == [rf0, rf1].
        p00 = r00 - k0 * rf0;
        p01 = r01 - k0 * rf1;
        p10 = r10 - k1 * rf0;
        p11 = r11 - k1 * rf1;
        out[i] = x_beta;
    }
    vec![out]
}

// ======================= streaming structs (macro) =========================

pair_indicator! {
    /// `ratio` — price ratio `close / benchmark`.
    Ratio, "ratio", 1, [],
    |a, b| batch_ratio(a, b),
    lookback = 0
}
pair_indicator! {
    /// `spread` — arithmetic (`log=0`) or log spread of two series.
    Spread, "spread", 1, [log = 0.0],
    |a, b| batch_spread(a, b, log.round() as i64),
    lookback = 0
}
pair_indicator! {
    /// `spread_zscore` — rolling z-score of the hedge-ratio spread
    /// `close − beta·benchmark` (`beta = 1.0` = the plain arithmetic spread).
    SpreadZscore, "spread_zscore", 1, [period = 20.0, beta = 1.0],
    |a, b| batch_spread_zscore(a, b, period.round() as usize, beta),
    lookback = pback(period, 1)
}
pair_indicator! {
    /// `beta` — rolling beta of returns (`cov(rA,rB)/var(rB)`).
    Beta, "beta", 1, [period = 5.0],
    |a, b| batch_beta(a, b, period.round() as usize),
    lookback = pbars(period)
}
pair_indicator! {
    /// `correl` — rolling Pearson correlation of price levels.
    Correl, "correl", 1, [period = 30.0],
    |a, b| batch_correl(a, b, period.round() as usize),
    lookback = pback(period, 1)
}
pair_indicator! {
    /// `correl_log` — rolling Pearson correlation of log returns.
    CorrelLog, "correl_log", 1, [period = 30.0],
    |a, b| batch_correl_log(a, b, period.round() as usize),
    lookback = pbars(period)
}
pair_indicator! {
    /// `half_life` — OU/AR(1) half-life (in bars) of the hedge-ratio spread.
    /// NaN whenever the window is not mean-reverting; see [`batch_half_life`].
    /// Warm-up is left INEXACT (the conservative `0`) on purpose: the first
    /// non-NaN bar depends on the DATA (whether a window reverts), not on the
    /// parameters alone, so no exact lookback expression exists.
    HalfLife, "half_life", 1, [period = 20.0, beta = 1.0],
    |a, b| batch_half_life(a, b, period.round() as usize, beta)
}
pair_indicator! {
    /// `kalman_beta` — time-varying hedge ratio from a 2-state Kalman filter.
    KalmanBeta, "kalman_beta", 1, [delta = 0.0001, ve = 0.001],
    |a, b| batch_kalman_beta(a, b, delta, ve)
}

// ============================ pair registry ================================

/// Static description of a pair indicator + fresh constructors (the pair-seam
/// twin of [`crate::registry::IndicatorMeta`]).
pub struct PairMeta {
    pub name: &'static str,
    pub pretty: &'static str,
    pub outputs: &'static [OutSpec],
    pub params: &'static [ParamSpec],
    pub make: fn() -> Box<dyn PairIndicator>,
    pub make_with: fn(&[f64]) -> Box<dyn PairIndicator>,
}

impl PairMeta {
    /// Warm-up depth at `raw` params — see [`PairIndicator::lookback`].
    pub fn lookback(&self, raw: &[f64]) -> usize {
        (self.make_with)(raw).lookback()
    }

    /// FULL warm-up depth at `raw` params — see [`PairIndicator::lookback_full`].
    pub fn lookback_full(&self, raw: &[f64]) -> usize {
        (self.make_with)(raw).lookback_full()
    }

    /// `true` when [`PairMeta::lookback`] is a real override, not the `0` default.
    pub fn lookback_exact(&self) -> bool {
        (self.make)().lookback_exact()
    }
}

/// Pair warm-up depth by registry name + raw params; `None` for an unknown name.
pub fn pair_lookback(name: &str, raw: &[f64]) -> Option<usize> {
    pair_registry().iter().find(|m| m.name == name).map(|m| m.lookback(raw))
}

macro_rules! pmk {
    ($ty:ty) => {
        || Box::new(<$ty>::default()) as Box<dyn PairIndicator>
    };
}

fn build_pair_registry() -> Vec<PairMeta> {
    use crate::registry::OutputStyle::Line;
    macro_rules! pout {
        ($($n:literal),*) => { &[$( OutSpec { name: $n, style: Line } ),*] };
    }
    macro_rules! pparams {
        ($(($n:literal, $d:expr, $mn:expr, $mx:expr, $st:expr)),* $(,)?) => {
            &[$( ParamSpec { name: $n, default: $d, min: $mn, max: $mx, step: $st } ),*]
        };
    }
    vec![
        PairMeta {
            name: "ratio",
            pretty: "Price Ratio",
            outputs: pout!("ratio"),
            params: &[],
            make: pmk!(Ratio),
            make_with: |raw| Box::new(Ratio::with_params(&coerce(&[], raw))),
        },
        PairMeta {
            name: "spread",
            pretty: "Spread",
            outputs: pout!("spread"),
            params: pparams!(("log", 0.0, 0.0, 1.0, 1.0)),
            make: pmk!(Spread),
            make_with: |raw| {
                Box::new(Spread::with_params(&coerce(pparams!(("log", 0.0, 0.0, 1.0, 1.0)), raw)))
            },
        },
        PairMeta {
            name: "spread_zscore",
            pretty: "Spread Z-Score",
            outputs: pout!("zscore"),
            params: pparams!(("period", 20.0, 2.0, 200.0, 1.0), ("beta", 1.0, -5.0, 5.0, 0.1)),
            make: pmk!(SpreadZscore),
            make_with: |raw| {
                Box::new(SpreadZscore::with_params(&coerce(
                    pparams!(("period", 20.0, 2.0, 200.0, 1.0), ("beta", 1.0, -5.0, 5.0, 0.1)),
                    raw,
                )))
            },
        },
        PairMeta {
            name: "beta",
            pretty: "Beta",
            outputs: pout!("beta"),
            params: pparams!(("period", 5.0, 2.0, 200.0, 1.0)),
            make: pmk!(Beta),
            make_with: |raw| {
                Box::new(Beta::with_params(&coerce(
                    pparams!(("period", 5.0, 2.0, 200.0, 1.0)),
                    raw,
                )))
            },
        },
        PairMeta {
            name: "correl",
            pretty: "Correlation",
            outputs: pout!("correl"),
            params: pparams!(("period", 30.0, 2.0, 200.0, 1.0)),
            make: pmk!(Correl),
            make_with: |raw| {
                Box::new(Correl::with_params(&coerce(
                    pparams!(("period", 30.0, 2.0, 200.0, 1.0)),
                    raw,
                )))
            },
        },
        PairMeta {
            name: "correl_log",
            pretty: "Log-Return Correlation",
            outputs: pout!("correl_log"),
            params: pparams!(("period", 30.0, 2.0, 200.0, 1.0)),
            make: pmk!(CorrelLog),
            make_with: |raw| {
                Box::new(CorrelLog::with_params(&coerce(
                    pparams!(("period", 30.0, 2.0, 200.0, 1.0)),
                    raw,
                )))
            },
        },
        PairMeta {
            name: "half_life",
            pretty: "Spread Half-Life",
            outputs: pout!("half_life"),
            params: pparams!(("period", 20.0, 2.0, 200.0, 1.0), ("beta", 1.0, -5.0, 5.0, 0.1)),
            make: pmk!(HalfLife),
            make_with: |raw| {
                Box::new(HalfLife::with_params(&coerce(
                    pparams!(("period", 20.0, 2.0, 200.0, 1.0), ("beta", 1.0, -5.0, 5.0, 0.1)),
                    raw,
                )))
            },
        },
        PairMeta {
            name: "kalman_beta",
            pretty: "Kalman Hedge Ratio",
            outputs: pout!("beta"),
            params: pparams!(
                ("delta", 0.0001, 0.000001, 0.1, 0.0001),
                ("ve", 0.001, 0.000001, 1.0, 0.001)
            ),
            make: pmk!(KalmanBeta),
            make_with: |raw| {
                Box::new(KalmanBeta::with_params(&coerce(
                    pparams!(
                        ("delta", 0.0001, 0.000001, 0.1, 0.0001),
                        ("ve", 0.001, 0.000001, 1.0, 0.001)
                    ),
                    raw,
                )))
            },
        },
    ]
}

/// The full pair-indicator set, built once.
pub fn pair_registry() -> &'static [PairMeta] {
    static R: OnceLock<Vec<PairMeta>> = OnceLock::new();
    R.get_or_init(build_pair_registry)
}

/// Look up a pair indicator's metadata by name.
pub fn pair_get(name: &str) -> Option<&'static PairMeta> {
    pair_registry().iter().find(|m| m.name == name)
}

/// Construct a fresh pair indicator by name.
pub fn pair_make(name: &str) -> Option<Box<dyn PairIndicator>> {
    pair_get(name).map(|m| (m.make)())
}

/// Construct a fresh pair indicator by name with a raw parameter slice.
pub fn pair_make_with(name: &str, raw: &[f64]) -> Option<Box<dyn PairIndicator>> {
    pair_get(name).map(|m| (m.make_with)(raw))
}

#[cfg(test)]
mod tests {
    //! `tests/pairs_parity.rs` proves stream == batch; it does NOT prove the
    //! arithmetic is right. These pin the MATH against closed-form answers.
    use super::*;

    fn bar(ts: usize, close: f64) -> Bar {
        Bar {
            ts: ts as i64 * 3_600_000,
            open: close,
            high: close,
            low: close,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    fn series(v: &[f64]) -> Vec<Bar> {
        v.iter().enumerate().map(|(i, &c)| bar(i, c)).collect()
    }

    /// The backward-compatibility contract: `beta = 1.0` must reproduce the
    /// original hardcoded `close − benchmark` spread BIT-FOR-BIT (multiplying by
    /// 1.0 is exact in IEEE-754). Asserted against a verbatim copy of the
    /// pre-change kernel, not merely eyeballed.
    #[test]
    fn spread_zscore_beta_one_is_bit_identical_to_unhedged() {
        fn legacy(a: &[Bar], b: &[Bar], period: usize) -> Vec<f64> {
            let (ca, cb) = (closes(a), closes(b));
            let n = ca.len();
            let s: Vec<f64> = (0..n).map(|i| ca[i] - cb[i]).collect();
            let mut out = vec![f64::NAN; n];
            let (mut run_sum, mut run_sum2) = (0.0, 0.0);
            for i in 0..n {
                run_sum += s[i];
                run_sum2 += s[i] * s[i];
                if i >= period {
                    run_sum -= s[i - period];
                    run_sum2 -= s[i - period] * s[i - period];
                }
                if i + 1 >= period {
                    let mean = run_sum / period as f64;
                    let var = run_sum2 / period as f64 - mean * mean;
                    let sd = var.max(0.0).sqrt();
                    if sd != 0.0 {
                        out[i] = (s[i] - mean) / sd;
                    }
                }
            }
            out
        }

        let a: Vec<Bar> =
            series(&(0..120).map(|i| 100.0 + (i as f64 * 0.11).sin() * 7.0).collect::<Vec<_>>());
        let b: Vec<Bar> =
            series(&(0..120).map(|i| 50.0 + (i as f64 * 0.07).cos() * 3.0).collect::<Vec<_>>());

        let got = &batch_spread_zscore(&a, &b, 20, 1.0)[0];
        let want = legacy(&a, &b, 20);
        assert_eq!(got.len(), want.len());
        for (i, (&g, &w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(
                (g.is_nan() && w.is_nan()) || g.to_bits() == w.to_bits(),
                "idx {i}: hedged(beta=1) {g:?} != legacy {w:?}"
            );
        }
    }

    /// A non-unit beta must actually change the spread — proves the parameter is
    /// wired through and not silently ignored.
    #[test]
    fn spread_zscore_beta_changes_the_spread() {
        let a: Vec<Bar> =
            series(&(0..80).map(|i| 100.0 + (i as f64 * 0.2).sin() * 5.0).collect::<Vec<_>>());
        let b: Vec<Bar> =
            series(&(0..80).map(|i| 20.0 + (i as f64 * 0.2).sin() * 5.0).collect::<Vec<_>>());
        let one = batch_spread_zscore(&a, &b, 20, 1.0)[0].clone();
        let two = batch_spread_zscore(&a, &b, 20, 2.0)[0].clone();
        assert!(
            one.iter()
                .zip(two.iter())
                .any(|(x, y)| { !(x.is_nan() && y.is_nan()) && x.to_bits() != y.to_bits() }),
            "beta had no effect on the z-score"
        );
    }

    /// Closed form: on a noiseless geometric decay `s_t = 0.5·s_{t−1}` the
    /// regression slope is exactly `λ = −0.5`, so the half-life is `−ln2/λ =
    /// 2·ln2 ≈ 1.3863` bars. Benchmark held constant so `beta = 1` leaves the
    /// spread equal to the decaying series.
    #[test]
    fn half_life_recovers_known_decay_rate() {
        let mut s = 64.0f64;
        let mut a_close = Vec::new();
        for _ in 0..40 {
            a_close.push(100.0 + s);
            s *= 0.5;
        }
        let a = series(&a_close);
        let b = series(&vec![100.0; a_close.len()]);

        let hl = &batch_half_life(&a, &b, 10, 1.0)[0];
        let last = hl.iter().rev().find(|v| !v.is_nan()).copied().expect("a half-life");
        let expected = 2.0 * std::f64::consts::LN_2;
        assert!((last - expected).abs() < 1e-9, "half-life {last} != {expected}");
    }

    /// A trending spread has `λ >= 0` — no finite half-life. NaN is the correct
    /// answer and the stand-down signal, not a warm-up artifact.
    #[test]
    fn half_life_is_nan_when_not_mean_reverting() {
        let a = series(&(0..60).map(|i| 100.0 + i as f64).collect::<Vec<_>>());
        let b = series(&vec![100.0; 60]);
        let hl = &batch_half_life(&a, &b, 10, 1.0)[0];
        assert!(hl.iter().all(|v| v.is_nan()), "a pure linear trend must yield no half-life");
    }

    /// A degenerate (constant) spread has zero variance and therefore no slope.
    #[test]
    fn half_life_is_nan_on_constant_spread() {
        let a = series(&vec![100.0; 40]);
        let b = series(&vec![100.0; 40]);
        assert!(batch_half_life(&a, &b, 10, 1.0)[0].iter().all(|v| v.is_nan()));
    }

    /// With `close = 2·benchmark` exactly, the filter must converge on the true
    /// hedge ratio 2.0.
    #[test]
    fn kalman_beta_converges_to_true_hedge_ratio() {
        let b_close: Vec<f64> = (0..300).map(|i| 100.0 + (i as f64 * 0.05).sin() * 10.0).collect();
        let a_close: Vec<f64> = b_close.iter().map(|x| 2.0 * x).collect();
        let a = series(&a_close);
        let b = series(&b_close);

        let out = &batch_kalman_beta(&a, &b, 0.0001, 0.001)[0];
        let last = out.last().copied().unwrap();
        assert!((last - 2.0).abs() < 0.05, "kalman beta {last} did not converge to 2.0");
        assert!(out.iter().all(|v| v.is_finite()), "kalman beta produced a non-finite value");
    }

    /// Tracking check: when the true ratio SHIFTS mid-series, the filter must
    /// move toward the new value — the whole point of a time-varying beta.
    #[test]
    fn kalman_beta_tracks_a_regime_shift() {
        let b_close: Vec<f64> = (0..600).map(|i| 100.0 + (i as f64 * 0.05).sin() * 10.0).collect();
        let a_close: Vec<f64> = b_close
            .iter()
            .enumerate()
            .map(|(i, x)| if i < 300 { 2.0 * x } else { 3.0 * x })
            .collect();
        let a = series(&a_close);
        let b = series(&b_close);

        let out = &batch_kalman_beta(&a, &b, 0.01, 0.001)[0];
        let before = out[299];
        let after = out[599];
        assert!((before - 2.0).abs() < 0.1, "pre-shift beta {before} != ~2.0");
        assert!(after > before, "beta did not move toward the new ratio");
    }
}
