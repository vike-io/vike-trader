//! Pair (benchmark) indicators: indicators computed from two aligned series, a primary instrument
//! and a benchmark.
//!
//! These read a second instrument that a single `&[Bar]` or `&Bar` cannot supply, so they live
//! behind their own [`PairIndicator`] trait and [`pair_registry`], separate from the single-series
//! [`crate::Indicator`]. The two series must be aligned bar for bar: element `i` of each is the
//! same point in time. The indicators work from the close of each bar.
//!
//! As with [`crate::Indicator`], folding [`PairIndicator::on_pair`] over a pair of series equals
//! [`PairIndicator::vectorize_pair`] on those series bit for bit.
//!
//! The pair indicators are `ratio`, `spread`, `spread_zscore`, `beta`, `correl`, `correl_log`,
//! `half_life` and `kalman_beta`.
//!
//! ## Mean-reversion statistics
//!
//! [`HalfLife`] and [`KalmanBeta`] are statistics, not a strategy: each returns a plottable number
//! and decides nothing. [`HalfLife`] says how fast a spread reverts, in bars (useful as a regime
//! filter: a half-life long relative to the intended holding period says the spread will not
//! revert in time). [`KalmanBeta`] is a time-varying hedge ratio, the alternative to the fixed
//! `beta = 1` a naive spread assumes.
//!
//! [`SpreadZscore`] and [`HalfLife`] take the hedge ratio as their `beta` parameter: the spread is
//! `close - beta * benchmark`, so `beta = 1.0` (the default) is the plain difference.
//
// ---- Engineering notes: not rendered on docs.rs. ----
// Faithful f64 ports of the `inputs=["close","benchmark"]` indicators from vike-trader-app
// `core/indicators/pairs.py` (ratio/spread/spread_zscore) and `statistics.py`
// (beta/correl/correl_log). These read a SECOND instrument (`benchmark`) that a
// single `&[Bar]` / `&Bar` cannot supply, so they live behind their own
// [`PairIndicator`] trait + [`pair_registry`], isolated from the single-series
// [`crate::Indicator`] seam and the vike-core hot path.
//
// Streaming runs the same core (`crate::fast_r`) that `vectorize_pair` and `series::*` run, one
// pair of closes at a time, so `on_pair`-fold == `vectorize_pair` bit-for-bit — gated in
// `tests/pairs_parity.rs` and `tests/fast_r.rs`.
// Wiring a live benchmark feed is out of scope (no second-instrument live seam
// exists yet); the indicators + their parity gate ship regardless.
//
// Mean-reversion statistics (NOT ports — no Python twin)
//
// [`HalfLife`] and [`KalmanBeta`] are new here. They are STATISTICS, not a
// strategy: each returns a plottable number and decides nothing. The trading
// rule that consumes them (entry/exit bands, sizing, orders) belongs in a
// strategy, never in this crate.
//
// Both are deliberately multi-consumer, which is why they sit on this shared
// seam rather than inside one strategy:
//   * [`HalfLife`] — how fast a spread reverts, in bars. Its use is a REGIME
//     GATE: a half-life long relative to the intended holding period says the
//     spread will not revert before costs and stops bite, so stand down. That
//     applies to a maker's regime filter as much as to a pair trade.
//   * [`KalmanBeta`] — a time-varying hedge ratio, the principled alternative
//     to the fixed `beta = 1` a naive spread assumes, and the input any
//     two-leg hedge sizing needs.
//
// [`SpreadZscore`] and [`HalfLife`] take that hedge ratio as the `beta`
// parameter: the spread is `close − beta·benchmark`, so `beta = 1.0` (the
// default) reproduces the previous hardcoded arithmetic BIT-FOR-BIT
// (multiplying by 1.0 is exact in IEEE-754).
use crate::fast_r::{
    BetaCore, CorrelCore, CorrelLogCore, HalfLifeCore, KalmanCore, RatioCore, SpreadCore,
    ZscoreCore, run_correl_log, run_slices,
};
use crate::indicators::{pback, pbars};
use crate::registry::{OutSpec, ParamSpec, coerce};
use std::sync::OnceLock;
use vike_marketdata::Bar;

/// Object-safe clone for boxed pair indicators (the pair twin of [`crate::BoxedClone`]).
pub trait BoxedClonePair {
    /// An independent copy of the full streaming state as a fresh boxed trait object.
    fn clone_box(&self) -> Box<dyn PairIndicator>;
}
impl<T: PairIndicator + Clone + 'static> BoxedClonePair for T {
    fn clone_box(&self) -> Box<dyn PairIndicator> {
        Box::new(self.clone())
    }
}

/// An indicator over a primary instrument and an aligned `benchmark` instrument.
///
/// The streaming path ([`PairIndicator::on_pair`]) and the batch path
/// ([`PairIndicator::vectorize_pair`]) give the same numbers bit for bit.
pub trait PairIndicator: BoxedClonePair {
    /// Streaming: feed the next aligned (primary, benchmark) bar pair, advance the state, and
    /// return this bar's output line(s). Warm-up returns `f64::NAN`.
    fn on_pair(&mut self, primary: &Bar, benchmark: &Bar) -> Vec<f64>;
    /// Batch over two aligned series: one `Vec<f64>` per output line. This is the reference that
    /// `on_pair` reproduces.
    fn vectorize_pair(&self, primary: &[Bar], benchmark: &[Bar]) -> Vec<Vec<f64>>;
    /// The most recent `on_pair` output, without advancing.
    fn value(&self) -> Vec<f64>;
    /// Clear all streaming state back to construction.
    fn reset(&mut self);
    /// The registry key (for example `"ratio"`).
    fn name(&self) -> &str;

    /// Warm-up depth, the pair twin of [`crate::Indicator::lookback`]: the index of the first bar
    /// at which `vectorize_pair` yields a non-NaN value on any output line. Exact when
    /// [`PairIndicator::lookback_exact`] is `true`, otherwise the conservative default `0`.
    //
    // Gated in `tests/lookback.rs`.
    fn lookback(&self) -> usize {
        0
    }

    /// Full warm-up depth, the pair twin of [`crate::Indicator::lookback_full`]: the first index
    /// at which every output line is non-NaN. Defaults to [`PairIndicator::lookback`], which is
    /// correct for every single-line pair indicator.
    //
    // The gate in `tests/lookback.rs` fails any staggered one that forgets to override it.
    fn lookback_full(&self) -> usize {
        self.lookback()
    }

    /// `true` when [`PairIndicator::lookback`] is a real, parameter-derived value rather than the
    /// default `0`.
    fn lookback_exact(&self) -> bool {
        false
    }
}

/// The closes of a bar slice.
fn closes(bars: &[Bar]) -> Vec<f64> {
    bars.iter().map(|x| x.close).collect()
}

macro_rules! pair_indicator {
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $Core:ty, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        core = $core:expr,
        |$a:ident, $b:ident| $body:expr
    ) => {
        pair_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $Core, [ $( $pf = $pdef ),* ],
            core = $core,
            |$a, $b| $body,
            lookback = 0, exact = false
        }
    };
    // Same, plus an EXACT warm-up expression over the param locals.
    (
        $(#[$m:meta])*
        $Name:ident, $key:literal, $Core:ty, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        core = $core:expr,
        |$a:ident, $b:ident| $body:expr,
        lookback = $lb:expr
    ) => {
        pair_indicator! {
            @impl
            $(#[$m])*
            $Name, $key, $Core, [ $( $pf = $pdef ),* ],
            core = $core,
            |$a, $b| $body,
            lookback = $lb, exact = true
        }
    };
    (
        @impl
        $(#[$m:meta])*
        $Name:ident, $key:literal, $Core:ty, [ $( $pf:ident = $pdef:expr ),* $(,)? ],
        core = $core:expr,
        |$a:ident, $b:ident| $body:expr,
        lookback = $lb:expr, exact = $exact:expr
    ) => {
        $(#[$m])*
        #[derive(Clone)]
        pub struct $Name {
            $( $pf: f64, )*
            core: $Core,
            last: f64,
        }
        impl $Name {
            pub fn new() -> Self {
                Self::with_params(&[])
            }
            // allow, not expect: inside macro_rules!: alive in some expansions, unfulfilled in others, so expect would redden a build (measured 2026-10-10).
            #[allow(unused_variables, unused_mut, unused_assignments, clippy::allow_attributes)]
            pub fn with_params(p: &[f64]) -> Self {
                let mut i = 0usize;
                $( let $pf = p.get(i).copied().unwrap_or($pdef); i += 1; )*
                Self { $( $pf, )* core: $core, last: f64::NAN }
            }

            /// Feeds the next aligned pair of closes (primary, benchmark) and returns this bar's
            /// value: the plain-number door beside [`PairIndicator::on_pair`], with no `Bar` and no
            /// `Vec` in between. `NaN` while warming up. Runs the same core as `on_pair`,
            /// `vectorize_pair` and the `series` call of the same name, so all four agree bit for bit.
            #[inline]
            pub fn on_pair_values(&mut self, primary: f64, benchmark: f64) -> f64 {
                let v = self.core.push(primary, benchmark);
                self.last = v;
                v
            }
        }
        impl Default for $Name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl PairIndicator for $Name {
            fn on_pair(&mut self, primary: &Bar, benchmark: &Bar) -> Vec<f64> {
                vec![self.on_pair_values(primary.close, benchmark.close)]
            }
            fn vectorize_pair(&self, primary: &[Bar], benchmark: &[Bar]) -> Vec<Vec<f64>> {
                $( let $pf = self.$pf; )*
                let $a = primary;
                let $b = benchmark;
                $body
            }
            fn value(&self) -> Vec<f64> {
                vec![self.last]
            }
            // allow, not expect: inside macro_rules!: alive in some expansions, unfulfilled in others, so expect would redden a build (measured 2026-10-10).
            #[allow(unused_variables, clippy::allow_attributes)]
            fn reset(&mut self) {
                $( let $pf = self.$pf; )*
                self.core = $core;
                self.last = f64::NAN;
            }
            fn name(&self) -> &str {
                $key
            }
            // allow, not expect: inside macro_rules!: alive in some expansions, unfulfilled in others, so expect would redden a build (measured 2026-10-10).
            #[allow(unused_variables, clippy::allow_attributes)]
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
    let mut core = RatioCore::new();
    vec![run_slices!(core, &ca, &cb)]
}

fn batch_spread(a: &[Bar], b: &[Bar], log: i64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = SpreadCore::new(log as f64);
    vec![run_slices!(core, &ca, &cb)]
}

/// Rolling z-score of the hedge-ratio spread `s = close − beta·benchmark`.
/// `beta = 1.0` is the plain `close − benchmark`. The window sums are taken on the values shifted
/// by the window's oldest, and a window of bit-equal values has a zero standard deviation exactly
/// (see `crate::fast_r`).
fn batch_spread_zscore(a: &[Bar], b: &[Bar], period: usize, beta: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = ZscoreCore::new(period, beta);
    vec![run_slices!(core, &ca, &cb)]
}

fn batch_beta(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = BetaCore::new(period);
    vec![run_slices!(core, &ca, &cb)]
}

fn batch_correl(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = CorrelCore::new(period);
    vec![run_slices!(core, &ca, &cb)]
}

fn batch_correl_log(a: &[Bar], b: &[Bar], period: usize) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    vec![run_correl_log(&ca, &cb, period)]
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
/// to trade on.
fn batch_half_life(a: &[Bar], b: &[Bar], period: usize, beta: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = HalfLifeCore::new(period, beta);
    vec![run_slices!(core, &ca, &cb)]
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
/// Emits `beta` only; `alpha` stays internal. This recursion is O(1) per bar with bounded state, and
/// the seam streams it the same way: one bar of the recursion per `on_pair`.
fn batch_kalman_beta(a: &[Bar], b: &[Bar], delta: f64, ve: f64) -> Vec<Vec<f64>> {
    let (ca, cb) = (closes(a), closes(b));
    let mut core = KalmanCore::new(delta, ve);
    vec![run_slices!(core, &ca, &cb)]
}

// ======================= streaming structs (macro) =========================

pair_indicator! {
    /// `ratio`: the price ratio `close / benchmark`. No parameters. `NaN` where the benchmark
    /// close is `0`.
    Ratio, "ratio", RatioCore, [],
    core = RatioCore::new(),
    |a, b| batch_ratio(a, b),
    lookback = 0
}
pair_indicator! {
    /// `spread`: the difference of the two closes. Parameter `log` (default `0`, rounded): `0`
    /// gives the arithmetic spread `close - benchmark`, any other value the log spread
    /// `ln(close) - ln(benchmark)` (`NaN` where either close is not positive).
    Spread, "spread", SpreadCore, [log = 0.0],
    core = SpreadCore::new(log),
    |a, b| batch_spread(a, b, log.round() as i64),
    lookback = 0
}
pair_indicator! {
    /// `spread_zscore`: rolling z-score of the hedge-ratio spread `close - beta * benchmark`
    /// (`beta = 1.0` is the plain difference). Parameters: `period` (default 20) and `beta`
    /// (default 1.0). `NaN` for the first `period - 1` bars and wherever the window has zero
    /// standard deviation.
    SpreadZscore, "spread_zscore", ZscoreCore, [period = 20.0, beta = 1.0],
    core = ZscoreCore::new(period.round() as usize, beta),
    |a, b| batch_spread_zscore(a, b, period.round() as usize, beta),
    lookback = pback(period, 1)
}
pair_indicator! {
    /// `beta`: rolling beta of simple returns, `cov(r_primary, r_benchmark) / var(r_benchmark)`
    /// over `period` returns (default 5). `NaN` while warming up and wherever a return is
    /// undefined or the benchmark's returns are constant.
    Beta, "beta", BetaCore, [period = 5.0],
    core = BetaCore::new(period.round() as usize),
    |a, b| batch_beta(a, b, period.round() as usize),
    lookback = pbars(period)
}
pair_indicator! {
    /// `correl`: rolling Pearson correlation of the two close series (price levels) over `period`
    /// bars (default 30), in `[-1, 1]`. `NaN` while warming up and where a window has no variance.
    Correl, "correl", CorrelCore, [period = 30.0],
    core = CorrelCore::new(period.round() as usize),
    |a, b| batch_correl(a, b, period.round() as usize),
    lookback = pback(period, 1)
}
pair_indicator! {
    /// `correl_log`: rolling Pearson correlation of log returns over `period` returns (default
    /// 30), in `[-1, 1]`. `NaN` while warming up and wherever a close is not positive.
    CorrelLog, "correl_log", CorrelLogCore, [period = 30.0],
    core = CorrelLogCore::new(period.round() as usize),
    |a, b| batch_correl_log(a, b, period.round() as usize),
    lookback = pbars(period)
}
pair_indicator! {
    /// `half_life`: the mean-reversion half-life, in bars, of the hedge-ratio spread
    /// `close - beta * benchmark`, estimated over a rolling window of `period` bars (default 20;
    /// `beta` defaults to 1.0). `NaN` whenever the window is not mean-reverting (a trending or
    /// random-walking spread has no finite half-life) or is degenerate.
    //
    // See `batch_half_life`.
    // Warm-up is left INEXACT (the conservative `0`) on purpose: the first
    // non-NaN bar depends on the DATA (whether a window reverts), not on the
    // parameters alone, so no exact lookback expression exists.
    HalfLife, "half_life", HalfLifeCore, [period = 20.0, beta = 1.0],
    core = HalfLifeCore::new(period.round() as usize, beta),
    |a, b| batch_half_life(a, b, period.round() as usize, beta)
}
pair_indicator! {
    /// `kalman_beta`: a time-varying hedge ratio from a two-state Kalman filter (the dynamic
    /// alternative to a fixed `beta`). The state is `[beta, alpha]`, the observation is
    /// `close = beta * benchmark + alpha + noise`. Parameters: `delta` (default 0.0001), which sets
    /// how fast the hedge ratio may drift (smaller is steadier), and `ve` (default 0.001), the
    /// assumed observation noise. The output is `beta`; it is defined from the first bar.
    KalmanBeta, "kalman_beta", KalmanCore, [delta = 0.0001, ve = 0.001],
    core = KalmanCore::new(delta, ve),
    |a, b| batch_kalman_beta(a, b, delta, ve)
}

// ============================ pair registry ================================

/// Static description of a pair indicator and its constructors (the pair twin of
/// [`crate::registry::IndicatorMeta`]).
pub struct PairMeta {
    /// The registry key (for example `"spread_zscore"`).
    pub name: &'static str,
    /// Human-readable label for a picker or legend.
    pub pretty: &'static str,
    /// The output lines, in the order `vectorize_pair` and `on_pair` return them.
    pub outputs: &'static [OutSpec],
    /// The indicator's parameters, in the order the slice given to `make_with` is read.
    pub params: &'static [ParamSpec],
    /// Builds a fresh instance at the default parameters.
    pub make: fn() -> Box<dyn PairIndicator>,
    /// Builds a fresh instance from a raw parameter slice, coerced like
    /// [`crate::registry::coerce`] (missing or `NaN` entries take the default, out-of-range
    /// values are clamped).
    pub make_with: fn(&[f64]) -> Box<dyn PairIndicator>,
}

impl PairMeta {
    /// Warm-up depth at `raw` parameters, see [`PairIndicator::lookback`].
    pub fn lookback(&self, raw: &[f64]) -> usize {
        (self.make_with)(raw).lookback()
    }

    /// Full warm-up depth at `raw` parameters, see [`PairIndicator::lookback_full`].
    pub fn lookback_full(&self, raw: &[f64]) -> usize {
        (self.make_with)(raw).lookback_full()
    }

    /// `true` when [`PairMeta::lookback`] is a real value, not the `0` default.
    pub fn lookback_exact(&self) -> bool {
        (self.make)().lookback_exact()
    }
}

/// Pair warm-up depth by registry name and raw parameters; `None` for an unknown name.
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

/// The full set of pair indicators, built once.
pub fn pair_registry() -> &'static [PairMeta] {
    static R: OnceLock<Vec<PairMeta>> = OnceLock::new();
    R.get_or_init(build_pair_registry)
}

/// Look up a pair indicator's metadata by name. `None` for an unknown name.
pub fn pair_get(name: &str) -> Option<&'static PairMeta> {
    pair_registry().iter().find(|m| m.name == name)
}

/// Construct a fresh pair indicator by name at its default parameters. `None` for an unknown name.
pub fn pair_make(name: &str) -> Option<Box<dyn PairIndicator>> {
    pair_get(name).map(|m| (m.make)())
}

/// Construct a fresh pair indicator by name with a raw parameter slice (coerced: missing or `NaN`
/// entries take the default, out-of-range values are clamped). `None` for an unknown name.
pub fn pair_make_with(name: &str, raw: &[f64]) -> Option<Box<dyn PairIndicator>> {
    pair_get(name).map(|m| (m.make_with)(raw))
}

#[path = "pairs_tests.rs"]
#[cfg(test)]
mod pairs_tests;
