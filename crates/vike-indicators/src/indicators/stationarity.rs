//! Stationarity and memory measures over a rolling window, each written from its published
//! definition: the KPSS level-stationarity statistic (`kpss`), the fixed-width-window fractional
//! difference of the log close (`fractional_difference`), the Shannon entropy of the window's
//! discretised log returns (`shannon_entropy`) and the Lempel–Ziv complexity of its up/down
//! sequence (`lempel_ziv`). Cores and types live here; the rows are in `registry/stationarity.rs`.
//!
//! Sources:
//!
//! - Denis Kwiatkowski, Peter C. B. Phillips, Peter Schmidt and Yongcheol Shin, "Testing the null
//!   hypothesis of stationarity against the alternative of a unit root", Journal of Econometrics
//!   54 (1992) 159–178: the statistic `η_μ = T⁻² Σ S_t² / s²(l)` (their (13)), the long-run variance
//!   `s²(l)` with Bartlett weights `w(s, l) = 1 - s / (l + 1)` (their (10)), and the 5% critical
//!   value 0.463 of their Table 1.
//! - Marcos López de Prado, "Advances in Financial Machine Learning", Wiley, 2018: chapter 5, the
//!   weights `w_0 = 1`, `w_k = -w_(k-1) (d - k + 1) / k` (5.4.2) and the fixed-width window
//!   fractional difference (5.5, "FFD"), applied to the log price as its examples are; chapter 18,
//!   entropy features of discretised returns.
//! - Claude E. Shannon, "A Mathematical Theory of Communication", Bell System Technical Journal 27
//!   (1948): `H = -Σ p_i log p_i`.
//! - Abraham Lempel and Jacob Ziv, "On the Complexity of Finite Sequences", IEEE Transactions on
//!   Information Theory IT-22(1) (1976) 75–81: the complexity `c(n)` (the number of components of
//!   the exhaustive history) and its bound `n / log2 n` for a binary alphabet; F. Kaspar and H. G.
//!   Schuster, "Easily calculable measure for the complexity of spatiotemporal patterns", Physical
//!   Review A 36(2) (1987) 842–848: the scan that counts `c(n)`, and its worked example
//!   `0·001·10·100·1000·101`, `c = 6`.
//!
//! What is NOT here, and where it is: the augmented Dickey–Fuller statistic has ONE home,
//! `crates/vike-math/src/stationarity_tests.rs`'s `augmented_dickey_fuller`, and the rolling `adf` row in
//! `crates/vike-indicators/src/indicators/models.rs` (`AdfCore`) calls it on the window of log closes.
//! The long-memory measure next door is `hurst` (rescaled range, `crate::fast_l::HurstCore`): it
//! ESTIMATES memory, `fractional_difference` REMOVES a chosen amount of it, so neither replaces the
//! other.
//!
//! Every value is a function of the last `period` (or `window`) bars alone, so a stream and a batch
//! agree bit for bit and none is path dependent. `kpss` and `fractional_difference` read the LOG close
//! (`libm::log`; a close that is not finite and above zero is `NaN`), `shannon_entropy` its log
//! returns through the realized-volatility family's one return step (`crate::fast_j`'s
//! `next_log_return`: one bad close removes two returns), `lempel_ziv` only whether each close is above
//! the one before. A `NaN` inside the window makes the value `NaN` until it has left.

use super::pbars;
use crate::fast_e::scalar_indicator;
use crate::fast_j::next_log_return;

// ---- the shared window ---------------------------------------------------------

/// The last `n` values, oldest first, as one contiguous slice: each value is written twice (at slot
/// `i` and `i + n`), the `HurstCore` ring's trick, so a push costs two stores and no shift.
#[derive(Clone)]
struct Window {
    ring: Vec<f64>,
    n: usize,
    /// The slot the next value goes to, which is also where the oldest value sits.
    next: usize,
    /// Values pushed since the last clear, up to `n`.
    filled: usize,
}

impl Window {
    fn new(n: usize) -> Self {
        Self { ring: vec![0.0; 2 * n], n, next: 0, filled: 0 }
    }

    fn clear(&mut self) {
        self.next = 0;
        self.filled = 0;
    }

    /// Push `x`; the last `n` values, oldest first, once `n` have come since the last clear.
    fn push(&mut self, x: f64) -> Option<&[f64]> {
        if self.n == 0 {
            return None;
        }
        self.ring[self.next] = x;
        self.ring[self.next + self.n] = x;
        self.next += 1;
        if self.next == self.n {
            self.next = 0;
        }
        self.filled = (self.filled + 1).min(self.n);
        (self.filled == self.n).then(|| &self.ring[self.next..self.next + self.n])
    }
}

// The log of a usable close (finite and above zero, the rule every return-reading row and `adf`
// share) is `vike_math::time_series::log_of_positive` (decision 0294).

// ---- kpss ----------------------------------------------------------------------

#[derive(Clone)]
struct KpssCore {
    window: Window,
    lags: usize,
    residuals: Vec<f64>,
}

impl KpssCore {
    fn new(period: usize, lags: usize) -> Self {
        Self { window: Window::new(period), lags, residuals: Vec::with_capacity(period) }
    }

    fn clear(&mut self) {
        self.window.clear();
    }

    fn push(&mut self, close: f64) -> f64 {
        match self.window.push(vike_math::time_series::log_of_positive(close)) {
            Some(levels) => vike_math::stationarity_tests::kpss_statistic(
                levels,
                self.lags,
                &mut self.residuals,
            ),
            None => f64::NAN,
        }
    }
}

// The KPSS statistic of a window is `vike_math::stationarity_tests::kpss_statistic` (decision 0294).

scalar_indicator! {
    /// The KPSS statistic (`kpss`): the null is that the log close is STATIONARY around a constant
    /// level, so a LARGE value rejects stationarity (the 5% critical value is 0.463, drawn as the
    /// row's band). Parameters `period` (closes in the window, default 100) and `lags` (the
    /// autocovariances in the long-run variance, default 4, Schwert's `4 (T/100)^(1/4)` at 100);
    /// one output line, `NaN` for the first `period - 1` bars.
    ///
    /// Formula (Kwiatkowski, Phillips, Schmidt and Shin 1992, in this module's doc): with `y` the
    /// window's log closes, `e_t = y_t - mean(y)` and `S_t = e_1 + … + e_t`,
    /// `η = Σ S_t² / (T² s²(l))`, where `s²(l) = (1/T) Σ e_t² + (2/T) Σ_(s=1..l) (1 - s/(l+1))
    /// Σ_(t>s) e_t e_(t-s)`. A lag of `T` or more adds nothing. It is the complement of the
    /// augmented Dickey–Fuller test, whose null is the unit root
    /// (`crates/vike-math/src/stationarity_tests.rs`'s `augmented_dickey_fuller`).
    ///
    /// `NaN` for a window of identical closes, one holding a close not finite and above zero, or a
    /// long-run variance that is not positive.
    Kpss, "kpss", KpssCore, input = value,
    [period = 100.0, lags = 4.0],
    build = KpssCore::new(pbars(period), pbars(lags)),
    lookback = pbars(period).saturating_sub(1),
    path_dependent = false
}

// ---- fractional difference -----------------------------------------------------

#[derive(Clone)]
struct FractionalDifferenceCore {
    window: Window,
    /// `w_(width-1) … w_1, w_0`: oldest first, the order the window's slice comes in.
    weights: Vec<f64>,
}

impl FractionalDifferenceCore {
    fn new(d: f64, width: usize) -> Self {
        let mut weights = vike_math::time_series::fractional_difference_weights(d, width);
        weights.reverse();
        Self { window: Window::new(width), weights }
    }

    fn clear(&mut self) {
        self.window.clear();
    }

    fn push(&mut self, close: f64) -> f64 {
        let Some(levels) = self.window.push(vike_math::time_series::log_of_positive(close)) else {
            return f64::NAN;
        };
        let mut sum = 0.0;
        for (w, x) in self.weights.iter().zip(levels) {
            sum += w * x;
        }
        sum
    }
}

scalar_indicator! {
    /// Fixed-width-window fractional difference of the log close (`fractional_difference`, López de
    /// Prado's "FFD"): a series made stationary while keeping as much of the price's memory as the
    /// order `d` allows. Parameters `d` (the order, default 0.4: 0 is the log close itself, 1 its log
    /// return) and `window` (the number of weights, default 100); one output line, `NaN` for the
    /// first `window - 1` bars.
    ///
    /// Formula (López de Prado 2018, chapter 5, in this module's doc): `x̃_t = Σ_(k=0..window-1)
    /// w_k log(close_(t-k))` with `w_0 = 1` and `w_k = -w_(k-1) (d - k + 1) / k`, folded oldest
    /// weight first. The book picks the width as the first `k` whose `|w_k|` falls under a
    /// threshold; here the width is the parameter, the same window for every bar.
    ///
    /// `NaN` while the window holds a close not finite and above zero.
    FractionalDifference, "fractional_difference", FractionalDifferenceCore, input = value,
    [d = 0.4, window = 100.0],
    build = FractionalDifferenceCore::new(d, pbars(window)),
    lookback = pbars(window).saturating_sub(1),
    path_dependent = false
}

// ---- shannon entropy -----------------------------------------------------------

#[derive(Clone)]
struct ShannonEntropyCore {
    window: Window,
    counts: Vec<usize>,
    /// `ln(bins)`, the entropy of a uniform spread, which the value is divided by.
    log_bins: f64,
    /// The previous bar's close when it was usable, else `NaN` (`next_log_return` keeps it).
    previous: f64,
    /// Whether a close came since the last clear: the first has no return and pushes nothing.
    started: bool,
}

impl ShannonEntropyCore {
    fn new(period: usize, bins: usize) -> Self {
        Self {
            window: Window::new(period),
            counts: vec![0; bins],
            log_bins: libm::log(bins as f64),
            previous: f64::NAN,
            started: false,
        }
    }

    fn clear(&mut self) {
        self.window.clear();
        self.previous = f64::NAN;
        self.started = false;
    }

    fn push(&mut self, close: f64) -> f64 {
        // The realized-volatility family's one return step; a missing return enters the window as `NaN`.
        let r = next_log_return(&mut self.previous, close);
        if !std::mem::replace(&mut self.started, true) {
            return f64::NAN;
        }
        match self.window.push(r.unwrap_or(f64::NAN)) {
            Some(returns) => vike_math::time_series::shannon_entropy_of_bins(
                returns,
                &mut self.counts,
                self.log_bins,
            ),
            None => f64::NAN,
        }
    }
}

// The binned Shannon entropy of a window is `vike_math::time_series::shannon_entropy_of_bins` (decision 0294).

scalar_indicator! {
    /// Shannon entropy of the discretised log returns (`shannon_entropy`): how evenly the last
    /// `period` returns spread over `bins` equal-width bins between their own lowest and highest,
    /// from 0 (all in one bin) to 1 (as many in every bin). Parameters `period` (returns in the window,
    /// default 50) and `bins` (default 10); one output line, `NaN` for the first `period` bars (a
    /// return needs the close before it).
    ///
    /// Formula (Shannon 1948, in this module's doc): with `p_i` the share of the window's returns in
    /// bin `i`, `H = -Σ p_i ln p_i / ln(bins)`, the plug-in estimate López de Prado's chapter 18 calls
    /// it. A return equal to the window's highest belongs to the last bin; a window of identical
    /// returns is 0.
    ///
    /// `NaN` while the window holds a return that is not finite (a close not finite and above zero).
    ShannonEntropy, "shannon_entropy", ShannonEntropyCore, input = value,
    [period = 50.0, bins = 10.0],
    build = ShannonEntropyCore::new(pbars(period), pbars(bins)),
    lookback = pbars(period),
    path_dependent = false
}

// ---- lempel-ziv complexity -----------------------------------------------------

#[derive(Clone)]
struct LempelZivCore {
    window: Window,
    /// `log2(n) / n`, Lempel and Ziv's bound `n / log2 n` inverted.
    scale: f64,
    /// The last close, `None` before the first.
    previous: Option<f64>,
}

impl LempelZivCore {
    fn new(period: usize) -> Self {
        let n = period as f64;
        Self { window: Window::new(period), scale: libm::log2(n) / n, previous: None }
    }

    fn clear(&mut self) {
        self.window.clear();
        self.previous = None;
    }

    fn push(&mut self, close: f64) -> f64 {
        let Some(previous) = self.previous.replace(close) else {
            return f64::NAN;
        };
        let symbol = if close.is_nan() || previous.is_nan() {
            f64::NAN
        } else if close > previous {
            1.0
        } else {
            0.0
        };
        match self.window.push(symbol) {
            Some(symbols) if !symbols.iter().any(|s| s.is_nan()) => {
                vike_math::time_series::lempel_ziv_complexity(symbols) as f64 * self.scale
            }
            _ => f64::NAN,
        }
    }
}

// Lempel and Ziv's `c(n)` is `vike_math::time_series::lempel_ziv_complexity` (decision 0294).

scalar_indicator! {
    /// Lempel–Ziv complexity of the up/down sequence (`lempel_ziv`): how many new patterns the last
    /// `period` moves need, against what a coin would need. A move is 1 when the close is above the
    /// one before and 0 otherwise. One parameter, `period` (moves in the window, default 100); one
    /// output line, `NaN` for the first `period` bars. Near 1 for a sequence as irregular as fair coin
    /// flips, lower for a regular one (a trend, an alternation); the band is 1.
    ///
    /// Formula (Lempel and Ziv 1976; Kaspar and Schuster 1987, in this module's doc): `c(n)` is the
    /// number of components when the sequence is split, left to right, into the shortest pieces that
    /// are not a copy of something starting earlier; the value is `c(n) · (log2(n) / n)`. The
    /// sequence `0001101001000101` splits as `0·001·10·100·1000·101`: `c = 6`, value `6 · 4 / 16`.
    ///
    /// `NaN` while the window holds a `NaN` close.
    LempelZiv, "lempel_ziv", LempelZivCore, input = value,
    [period = 100.0],
    build = LempelZivCore::new(pbars(period)),
    lookback = pbars(period),
    path_dependent = false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Indicator;
    use crate::series;
    use vike_marketdata::Bar;
    use vike_math::statistics::high_median_of_sorted;

    /// A fixed uniform sequence in `[-0.5, 0.5)` (a 64-bit LCG; this crate takes no `rand`).
    fn noise(seed: u64, n: usize) -> Vec<f64> {
        let mut state = seed;
        (0..n)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
            })
            .collect()
    }

    /// Closes whose log returns are `step * noise`: a geometric random walk from 100.
    fn walk(seed: u64, step: f64, n: usize) -> Vec<f64> {
        let mut close = 100.0;
        noise(seed, n)
            .into_iter()
            .map(|u| {
                close *= libm::exp(step * u);
                close
            })
            .collect()
    }

    fn bars(closes: &[f64]) -> Vec<Bar> {
        closes
            .iter()
            .enumerate()
            .map(|(i, &c)| Bar::ohlcv(i as i64 * 60_000, c, c + 0.5, c - 0.5, c, 1.0))
            .collect()
    }

    fn same(a: &[f64], b: &[f64]) -> bool {
        a.len() == b.len()
            && a.iter()
                .zip(b)
                .all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()))
    }

    fn median(values: &[f64]) -> f64 {
        let mut v: Vec<f64> = values.iter().copied().filter(|x| !x.is_nan()).collect();
        v.sort_by(f64::total_cmp);
        high_median_of_sorted(&v).expect("a KPSS series with a value")
    }

    /// Stationary noise sits under the 5% critical value and a random walk far above it.
    #[test]
    fn kpss_tells_noise_from_a_walk() {
        let level: Vec<f64> = noise(3, 2000).iter().map(|u| 100.0 * libm::exp(0.01 * u)).collect();
        let stationary = series::kpss(&level, 400.0, 4.0).unwrap();
        let wandering = series::kpss(&walk(3, 0.01, 2000), 400.0, 4.0).unwrap();
        assert!(median(&stationary) < 0.463, "{}", median(&stationary));
        assert!(median(&wandering) > 0.463, "{}", median(&wandering));
    }

    /// `(1 - B)^d`'s binomial weights, oldest first: for `d = 0.5`, `1, -1/2, -1/8, -1/16`.
    #[test]
    fn fractional_difference_weights_are_the_binomial_series() {
        assert_eq!(FractionalDifferenceCore::new(0.5, 4).weights, [-0.0625, -0.125, -0.5, 1.0]);
        assert_eq!(FractionalDifferenceCore::new(1.0, 4).weights, [0.0, 0.0, -1.0, 1.0]);
        assert_eq!(FractionalDifferenceCore::new(2.0, 4).weights, [0.0, 1.0, -2.0, 1.0]);
    }

    /// `d = 0` is the log close and `d = 1` its log return, bit for bit, from the `window`-th bar.
    #[test]
    fn fractional_difference_at_whole_orders() {
        let closes = walk(5, 0.02, 300);
        let logs: Vec<f64> = closes.iter().map(|&c| libm::log(c)).collect();
        let level = series::fractional_difference(&closes, 0.0, 20.0).unwrap();
        let first = series::fractional_difference(&closes, 1.0, 20.0).unwrap();
        for i in 0..closes.len() {
            if i < 19 {
                assert!(level[i].is_nan() && first[i].is_nan(), "{i}");
            } else {
                assert_eq!(level[i], logs[i], "{i}");
                assert_eq!(first[i], logs[i] - logs[i - 1], "{i}");
            }
        }
    }

    /// Evenly spread returns fill every bin; one spike among still returns puts them all in one.
    #[test]
    fn shannon_entropy_of_even_and_lumped_returns() {
        let even = series::shannon_entropy(&walk(7, 0.02, 1100), 1000.0, 10.0).unwrap();
        assert!(even[1000] > 0.99, "{}", even[1000]);
        let mut lumped = vec![100.0; 1001];
        lumped[600] = 150.0;
        for (i, c) in lumped.iter_mut().enumerate().skip(601) {
            *c = 150.0 + 1e-9 * (i % 2) as f64;
        }
        let value = series::shannon_entropy(&lumped, 1000.0, 10.0).unwrap()[1000];
        assert!(value < 0.01, "{value}");
    }

    /// The worked example as closes: up for 1, down for 0. Sixteen moves, `c = 6`, `6 · 4 / 16`.
    #[test]
    fn lempel_ziv_end_to_end() {
        let mut closes = vec![100.0];
        for b in "0001101001000101".bytes() {
            let last = closes[closes.len() - 1];
            closes.push(if b == b'1' { last + 1.0 } else { last - 1.0 });
        }
        let value = series::lempel_ziv(&closes, 16.0).unwrap();
        assert!(value[..16].iter().all(|v| v.is_nan()));
        assert_eq!(value[16], 1.5);
        // A coin is near 1; a steady climb is the least there is, two components.
        let coin = series::lempel_ziv(&walk(11, 0.02, 1001), 1000.0).unwrap()[1000];
        assert!((0.7..1.4).contains(&coin), "{coin}");
        let climb: Vec<f64> = (0..1001).map(|i| 100.0 + i as f64).collect();
        let least = series::lempel_ziv(&climb, 1000.0).unwrap()[1000];
        assert_eq!(least, 2.0 * (libm::log2(1000.0) / 1000.0));
    }

    /// The plain-number door is the stream: `series::*`, `next_value` and `next(&bar)` give the
    /// same bits, a close at or below zero (here a zero close) included.
    #[test]
    fn the_series_calls_are_the_stream() {
        let mut closes = walk(13, 0.03, 700);
        closes[350] = 0.0;
        let b = bars(&closes);
        macro_rules! check {
            ($ty:ident, $series:expr, $($p:expr),+) => {{
                let whole = $series;
                let mut a = $ty::new($($p),+).unwrap();
                let mut c = $ty::new($($p),+).unwrap();
                let streamed: Vec<f64> = closes.iter().map(|&x| a.next_value(x)).collect();
                let by_bar: Vec<f64> = b.iter().map(|x| c.next(x)).collect();
                assert!(same(&whole, &streamed), stringify!($ty));
                assert!(same(&whole, &by_bar), stringify!($ty));
                assert!(same(&whole, &$ty::new($($p),+).unwrap().vectorize(&b)[0]), stringify!($ty));
                assert!(whole[600].is_finite() && whole[360].is_nan(), stringify!($ty));
            }};
        }
        check!(Kpss, series::kpss(&closes, 100.0, 4.0).unwrap(), 100.0, 4.0);
        check!(
            FractionalDifference,
            series::fractional_difference(&closes, 0.4, 100.0).unwrap(),
            0.4,
            100.0
        );
        check!(ShannonEntropy, series::shannon_entropy(&closes, 50.0, 10.0).unwrap(), 50.0, 10.0);
        let mut lz = LempelZiv::new(100.0).unwrap();
        let whole = series::lempel_ziv(&closes, 100.0).unwrap();
        assert!(same(&whole, &closes.iter().map(|&x| lz.next_value(x)).collect::<Vec<f64>>()));
        assert!(same(&whole, &LempelZiv::new(100.0).unwrap().vectorize(&b)[0]));
    }

    /// A raw `with_params` can read an infinity, a `NaN`, a negative or a zero: no constructor may
    /// wrap a length, ask the allocator for the address space, or panic on the first bars.
    #[test]
    fn with_params_degenerate_never_panics() {
        let bars: Vec<Bar> = (0..40)
            .map(|i| {
                let x = 100.0 + f64::from(i % 7);
                Bar::ohlcv(i64::from(i) * 60_000, x, x + 1.0, x - 1.0, x, 10.0)
            })
            .collect();
        macro_rules! probe {
            ($($ty:ty),* $(,)?) => {$(
                for raw in [
                    &[0.0f64][..],
                    &[0.0, 0.0][..],
                    &[1.0, 1.0][..],
                    &[f64::NAN, f64::NAN][..],
                    &[-1e9, -1e9][..],
                    &[f64::INFINITY, f64::NEG_INFINITY][..],
                    &[f64::INFINITY, f64::INFINITY][..],
                ] {
                    let mut indicator = <$ty>::with_params(raw);
                    assert!(indicator.lookback() < 1_000_000, "{} lookback at {raw:?}", indicator.name());
                    for b in &bars {
                        let _ = indicator.on_bar(b);
                    }
                    let _ = indicator.vectorize(&bars);
                }
            )*};
        }
        probe!(Kpss, FractionalDifference, ShannonEntropy, LempelZiv);
    }
}
