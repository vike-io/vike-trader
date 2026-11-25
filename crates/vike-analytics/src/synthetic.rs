//! Synthetic market data for backtests and studies, each seeded and reproducible bit for bit:
//! OHLCV bar paths from four price models (geometric Brownian motion, Ornstein-Uhlenbeck, two-state
//! regime switching, Merton jump diffusion), a moving block bootstrap of real bars, a limit order
//! book from the Cont-Stoikov-Talreja queue model (the child `order_book`, [`order_book_stream`]),
//! and bar paths sampled from a LEARNED next-step model a study fitted (the child `learned`,
//! [`learned_bars`]). [`stylized_facts`] (the child `stylized_facts`) puts any of these paths beside
//! the market it imitates: the return distribution's shape, its autocorrelation and volatility
//! clustering.
//!
//! # The contract
//!
//! - **Seeded and portable.** Every draw comes from ONE `rand_chacha::ChaCha8Rng` seeded with the
//!   caller's `seed` (`seed_from_u64`): a named, versioned stream, not `rand`'s `StdRng`, which is free
//!   to change algorithm between releases (the root `Cargo.toml`'s `rand_chacha` pin argues it). Every
//!   transcendental is the `libm` crate (decision 0032), so one seed gives the same data on every box.
//! - **Per-bar units.** A model's `drift` and `volatility` are per BAR of the grid: under
//!   [`SyntheticModel::GeometricBrownianMotion`] one bar's log return has mean `drift - volatility²/2`
//!   and standard deviation `volatility`. Nothing here reads a calendar or annualises.
//! - **Generated data is values, never stored rows.** A study or a test hands bars and book streams to a
//!   backtest through `vike_data::HistoryStoreSynthetic`, an in-memory store under the venue
//!   `synthetic`, so generated prices never reach a user's store and never wear a real venue's id.
//! - **`first_price` is the close before the first bar.** A model bar opens at the close before it, so
//!   a model path has no gaps and its first bar opens at `first_price`; a bootstrap bar opens at that
//!   close times the gap it drew from the source.
//! - **A refusal is a sentence.** Every parameter is checked before the first draw, and a path whose
//!   price leaves the range of `f64` is refused at the bar it happened, never returned as `inf` or 0.
//!
//! # Intrabar high and low: one path per bar
//!
//! A model bar is the model itself run over `intrabar_steps` equal sub-steps of the bar, and its open,
//! high, low and close are that ONE path's first point, maximum, minimum and last point, so the four
//! are jointly consistent by construction (a bar's range has the law of its path's range, not of two
//! independent draws). Every sub-step is sampled exactly: the Brownian increment, the
//! Ornstein-Uhlenbeck transition over `1 / intrabar_steps` of a bar, and the Poisson jumps at
//! `jump_intensity / intrabar_steps`. What the grid costs: a path's maximum over `n` points sits below
//! the continuous maximum; for a Brownian bar its mean is, to first order (Siegmund),
//! `volatility * (sqrt(2 / pi) + zeta(1/2) / sqrt(2 pi n))`, about 9% low at `n = 64` and 2% low at
//! `n = 1024`, so a study chooses `n` for the precision it needs and pays `n` normal draws per bar.
//!
//! # The block bootstrap
//!
//! [`block_bootstrap_bars`] cuts the source into one shape per bar after its first: the gap
//! (`open / previous close`), `high / open`, `low / open`, `close / open` and the volume. It draws
//! contiguous blocks of `block_length` shapes (Kunsch's moving block bootstrap, as
//! `crate::stats::block_bootstrap_sharpe` draws pnl) and chains them from `first_price`, so the path
//! keeps the source's bar shapes and its short-range dependence, and needs no transcendental. The
//! source's `funding`, `bid`, `ask` and `symbol` are not carried.

use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::{Rng, SeedableRng};
use vike_marketdata::Bar;

mod learned;
mod order_book;
mod stylized_facts;
pub use learned::{NextStepFrame, NextStepRows, learned_bars, next_step_rows};
pub use order_book::{
    SyntheticInitialQueues, SyntheticOrderBook, SyntheticOrderBookModel, order_book_stream,
};
pub use stylized_facts::{STYLIZED_QUANTILES, StylizedFacts, stylized_facts};

/// A jump intensity above this many jumps per bar is refused: the Poisson count walks one draw per
/// jump, and a rate this high is a diffusion, not jumps.
const MAX_JUMP_INTENSITY: f64 = 100.0;

/// The time grid of a synthetic path: `bars` bars, the first at `start_ms`, each `step_ms` after the
/// one before (epoch milliseconds, UTC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticGrid {
    /// The first bar's timestamp.
    pub start_ms: i64,
    /// Milliseconds between two bars; must be positive.
    pub step_ms: i64,
    /// How many bars the path holds.
    pub bars: usize,
}

/// One regime of [`SyntheticModel::RegimeSwitching`]: a geometric Brownian motion's per-bar drift and
/// volatility.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntheticRegime {
    /// Per-bar drift.
    pub drift: f64,
    /// Per-bar volatility, `>= 0`.
    pub volatility: f64,
}

/// The price model of [`model_bars`]. Every parameter is per bar of the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyntheticModel {
    /// Each bar's log return is `drift - volatility²/2 + volatility * Z`, `Z` standard normal.
    GeometricBrownianMotion {
        /// Per-bar drift.
        drift: f64,
        /// Per-bar volatility, `>= 0`.
        volatility: f64,
    },
    /// Ornstein-Uhlenbeck on the LOG price, pulled toward `ln(mean_price)` at `reversion` per bar and
    /// sampled exactly: over a span `h` of a bar, `x' = m + (x - m) e^-kh + volatility *
    /// sqrt((1 - e^-2kh) / 2k) * Z`, which is `x + volatility * sqrt(h) * Z` at `k = 0`.
    OrnsteinUhlenbeck {
        /// The price the log price reverts to, `> 0`.
        mean_price: f64,
        /// Per-bar mean-reversion speed `k`, `>= 0`.
        reversion: f64,
        /// Per-bar volatility, `>= 0`.
        volatility: f64,
    },
    /// Two geometric Brownian regimes and a Markov chain between them: the path starts in
    /// `regimes[0]`, and before every bar after the first it leaves regime `r` with probability
    /// `switch_probability[r]`. [`regime_switching_path`] also returns the regime of every bar.
    RegimeSwitching {
        /// The two regimes.
        regimes: [SyntheticRegime; 2],
        /// Per-bar probability of leaving each regime, each in `[0, 1]`.
        switch_probability: [f64; 2],
    },
    /// Merton jump diffusion: the geometric Brownian bar plus `N ~ Poisson(jump_intensity)` jumps per
    /// bar, each adding a log jump `~ Normal(jump_mean, jump_volatility²)`. The drift is NOT compensated
    /// for the jumps' mean, so `drift` is the diffusion's alone.
    JumpDiffusion {
        /// Per-bar drift of the diffusion.
        drift: f64,
        /// Per-bar volatility of the diffusion, `>= 0`.
        volatility: f64,
        /// Expected jumps per bar, in `[0, 100]`.
        jump_intensity: f64,
        /// Mean of one log jump.
        jump_mean: f64,
        /// Standard deviation of one log jump, `>= 0`.
        jump_volatility: f64,
    },
}

/// A regime-switching path with its ground truth: the bars, and the index into the model's `regimes`
/// (0 or 1) each bar was drawn in.
#[derive(Debug, Clone, PartialEq)]
pub struct SyntheticRegimePath {
    /// The bars, exactly [`model_bars`]' for the same arguments.
    pub bars: Vec<Bar>,
    /// `regimes[i]` is the regime bar `i` was drawn in.
    pub regimes: Vec<usize>,
}

/// A path of `grid.bars` bars from `model`, starting from `first_price`, every bar carrying `volume`,
/// each bar one path of `intrabar_steps` sub-steps, drawn from `seed` (the module doc's contract).
///
/// # Errors
/// A sentence naming the parameter refused, or the bar whose price left the range of `f64`.
pub fn model_bars(
    model: &SyntheticModel,
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
    seed: u64,
) -> Result<Vec<Bar>, String> {
    generate(model, grid, first_price, volume, intrabar_steps, seed).map(|path| path.bars)
}

/// [`model_bars`] of a [`SyntheticModel::RegimeSwitching`] model, with the regime of every bar: the
/// ground truth a regime filter (an HMM) is scored against.
///
/// # Errors
/// [`model_bars`]' refusals, and any other model, which has no regime to return.
pub fn regime_switching_path(
    model: &SyntheticModel,
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
    seed: u64,
) -> Result<SyntheticRegimePath, String> {
    if !matches!(model, SyntheticModel::RegimeSwitching { .. }) {
        return Err(format!("a regime path needs a RegimeSwitching model, not {model:?}"));
    }
    generate(model, grid, first_price, volume, intrabar_steps, seed)
}

/// The one generator behind [`model_bars`] and [`regime_switching_path`]; every model reports regime 0.
fn generate(
    model: &SyntheticModel,
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
    seed: u64,
) -> Result<SyntheticRegimePath, String> {
    check_stepped(grid, first_price, volume, intrabar_steps)?;
    check_model(model)?;
    let laws = StepLaw::of(model, intrabar_steps as f64);
    let mut draws = Draws(ChaCha8Rng::seed_from_u64(seed));
    let switching = match *model {
        SyntheticModel::RegimeSwitching { switch_probability, .. } => Some(switch_probability),
        _ => None,
    };
    let mut regimes = Vec::with_capacity(grid.bars);
    let mut regime = 0;
    let hint = "lower the drift or the volatility, or shorten the path";
    let bars =
        stepped_bars(grid, first_price, volume, intrabar_steps, hint, |i, step, log_price| {
            // A regime is drawn once per bar, before its first sub-step.
            if step == 0 {
                if let Some(switch_probability) = switching
                    && i > 0
                    && draws.unit() <= switch_probability[regime]
                {
                    regime = 1 - regime;
                }
                regimes.push(regime);
            }
            Ok(laws[regime].next(log_price, &mut draws))
        })?;
    Ok(SyntheticRegimePath { bars, regimes })
}

/// The grid, the starting price, the volume and the sub-step count of a stepped path ([`model_bars`],
/// `learned::learned_bars`), checked in that order before the first draw.
fn check_stepped(
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
) -> Result<(), String> {
    check_path(grid, first_price)?;
    if !(volume.is_finite() && volume >= 0.0) {
        return Err(format!("the volume must be finite and >= 0, not {volume}"));
    }
    if intrabar_steps == 0 {
        return Err("a bar needs at least one intrabar step".to_string());
    }
    Ok(())
}

/// The ONE bar assembly of every stepped generator: bar `i` opens at the close before it
/// (`first_price` for the first), walks `intrabar_steps` sub-steps of the log price, each
/// `step(i, sub_step, log_price)`, and is that one path's first point, maximum, minimum and last point
/// (the module doc's "Intrabar high and low"). A bar whose price left the range of `f64` is refused,
/// the sentence ending with `hint`; a `step` refusal is returned as it is.
fn stepped_bars(
    grid: &SyntheticGrid,
    first_price: f64,
    volume: f64,
    intrabar_steps: usize,
    hint: &str,
    mut step: impl FnMut(usize, usize, f64) -> Result<f64, String>,
) -> Result<Vec<Bar>, String> {
    let mut bars = Vec::with_capacity(grid.bars);
    let mut open = first_price;
    let mut log_open = libm::log(first_price);
    for i in 0..grid.bars {
        // The bar's one intrabar path, in log price: its last point, maximum and minimum.
        let (mut log_price, mut highest, mut lowest) = (log_open, log_open, log_open);
        for sub_step in 0..intrabar_steps {
            log_price = step(i, sub_step, log_price)?;
            highest = highest.max(log_price);
            lowest = lowest.min(log_price);
        }
        let close = libm::exp(log_price);
        let high = libm::exp(highest).max(open).max(close);
        let low = libm::exp(lowest).min(open).min(close);
        if !(close.is_finite() && close > 0.0 && high.is_finite() && low > 0.0) {
            return Err(format!(
                "bar {i}: the path left the range of f64 (close {close}, high {high}, low {low}); \
                 {hint}"
            ));
        }
        bars.push(Bar::ohlcv(bar_time(grid, i), open, high, low, close, volume));
        open = close;
        log_open = log_price;
    }
    Ok(bars)
}

/// A path of `grid.bars` bars drawn from `source` by a moving block bootstrap of `block_length` bar
/// shapes, chained from `first_price` (the module doc's "The block bootstrap"), drawn from `seed`.
///
/// Pass `source[0].close` as `first_price` to start where the source started.
///
/// # Errors
/// A sentence naming the parameter or the source bar refused (a source needs `block_length + 1` bars,
/// each a consistent bar with finite prices above zero), or the bar whose price left the range of `f64`.
pub fn block_bootstrap_bars(
    source: &[Bar],
    block_length: usize,
    grid: &SyntheticGrid,
    first_price: f64,
    seed: u64,
) -> Result<Vec<Bar>, String> {
    check_path(grid, first_price)?;
    if block_length == 0 {
        return Err("the block length must be at least one bar".to_string());
    }
    if let Some((i, bar)) = source.iter().enumerate().find(|(_, b)| {
        ![b.open, b.high, b.low, b.close].iter().all(|p| p.is_finite() && *p > 0.0)
            || !b.volume.is_finite()
            || b.low > b.open.min(b.close)
            || b.high < b.open.max(b.close)
    }) {
        return Err(format!(
            "source bar {i} (at {}) is not a bar: every price must be finite and above zero, the \
             low at most the open and the close, the high at least both, and the volume finite",
            bar.timestamp
        ));
    }
    if source.len() <= block_length {
        return Err(format!(
            "a block of {block_length} bar shapes needs at least {} source bars, and the source \
             holds {}",
            block_length + 1,
            source.len()
        ));
    }
    // [gap, high / open, low / open, close / open, volume] of every source bar after the first.
    let shapes: Vec<[f64; 5]> = source
        .windows(2)
        .map(|w| {
            let b = &w[1];
            [b.open / w[0].close, b.high / b.open, b.low / b.open, b.close / b.open, b.volume]
        })
        .collect();
    let starts = shapes.len() - block_length + 1;
    let mut draws = Draws(ChaCha8Rng::seed_from_u64(seed));
    let mut bars = Vec::with_capacity(grid.bars);
    let mut close = first_price;
    while bars.len() < grid.bars {
        let start = draws.index(starts);
        for shape in shapes[start..start + block_length].iter().take(grid.bars - bars.len()) {
            let open = close * shape[0];
            let bar = Bar::ohlcv(
                bar_time(grid, bars.len()),
                open,
                open * shape[1],
                open * shape[2],
                open * shape[3],
                shape[4],
            );
            if !(bar.high.is_finite() && bar.low > 0.0) {
                return Err(format!(
                    "bar {}: the path left the range of f64 (high {}, low {}); shorten the path",
                    bars.len(),
                    bar.high,
                    bar.low
                ));
            }
            close = bar.close;
            bars.push(bar);
        }
    }
    Ok(bars)
}

/// The grid and the starting price, checked once for both generators.
fn check_path(grid: &SyntheticGrid, first_price: f64) -> Result<(), String> {
    if grid.step_ms <= 0 {
        return Err(format!("the grid's step must be positive, not {} ms", grid.step_ms));
    }
    let last = i64::try_from(grid.bars.saturating_sub(1))
        .ok()
        .and_then(|n| n.checked_mul(grid.step_ms))
        .and_then(|span| grid.start_ms.checked_add(span));
    if last.is_none() {
        return Err(format!(
            "{} bars of {} ms from {} run past the last representable timestamp",
            grid.bars, grid.step_ms, grid.start_ms
        ));
    }
    if !(first_price.is_finite() && first_price > 0.0) {
        return Err(format!("the first price must be finite and above zero, not {first_price}"));
    }
    Ok(())
}

/// `Ok` when `x` is finite, else the sentence naming `name`.
fn finite(name: &str, x: f64) -> Result<(), String> {
    if x.is_finite() { Ok(()) } else { Err(format!("{name} must be finite, not {x}")) }
}

/// `Ok` when `x` is finite and `>= 0`, else the sentence naming `name`.
fn at_least_zero(name: &str, x: f64) -> Result<(), String> {
    if x.is_finite() && x >= 0.0 {
        Ok(())
    } else {
        Err(format!("{name} must be finite and >= 0, not {x}"))
    }
}

/// Every parameter of `model` inside its documented range.
fn check_model(model: &SyntheticModel) -> Result<(), String> {
    let probability = |name: &str, x: f64| {
        if (0.0..=1.0).contains(&x) {
            Ok(())
        } else {
            Err(format!("{name} must be a probability in [0, 1], not {x}"))
        }
    };
    match *model {
        SyntheticModel::GeometricBrownianMotion { drift, volatility } => {
            finite("drift", drift)?;
            at_least_zero("volatility", volatility)
        }
        SyntheticModel::OrnsteinUhlenbeck { mean_price, reversion, volatility } => {
            if !(mean_price.is_finite() && mean_price > 0.0) {
                return Err(format!("mean_price must be finite and above zero, not {mean_price}"));
            }
            at_least_zero("reversion", reversion)?;
            at_least_zero("volatility", volatility)
        }
        SyntheticModel::RegimeSwitching { regimes, switch_probability } => {
            for (r, regime) in regimes.iter().enumerate() {
                finite(&format!("regimes[{r}].drift"), regime.drift)?;
                at_least_zero(&format!("regimes[{r}].volatility"), regime.volatility)?;
                probability(&format!("switch_probability[{r}]"), switch_probability[r])?;
            }
            Ok(())
        }
        SyntheticModel::JumpDiffusion {
            drift,
            volatility,
            jump_intensity,
            jump_mean,
            jump_volatility,
        } => {
            finite("drift", drift)?;
            at_least_zero("volatility", volatility)?;
            at_least_zero("jump_intensity", jump_intensity)?;
            if jump_intensity > MAX_JUMP_INTENSITY {
                return Err(format!(
                    "jump_intensity is expected jumps per bar and must be at most \
                     {MAX_JUMP_INTENSITY}, not {jump_intensity}"
                ));
            }
            finite("jump_mean", jump_mean)?;
            at_least_zero("jump_volatility", jump_volatility)
        }
    }
}

/// Bar `i`'s timestamp; [`check_path`] proved the grid's last one representable.
fn bar_time(grid: &SyntheticGrid, i: usize) -> i64 {
    grid.start_ms + i as i64 * grid.step_ms
}

/// One intrabar sub-step's exact law, computed once per path from the model and the sub-step count.
#[derive(Debug, Clone, Copy)]
enum StepLaw {
    /// A Brownian log increment.
    Brownian { mean: f64, deviation: f64 },
    /// The exact Ornstein-Uhlenbeck transition of the log price toward `mean_log`.
    MeanReverting { mean_log: f64, decay: f64, deviation: f64 },
    /// A Brownian log increment plus Poisson(`intensity`) normal log jumps.
    Jumping { mean: f64, deviation: f64, intensity: f64, jump_mean: f64, jump_volatility: f64 },
}

impl StepLaw {
    /// The law of one of `steps` sub-steps of a bar, per regime (both entries are one law for a model
    /// without regimes).
    fn of(model: &SyntheticModel, steps: f64) -> [StepLaw; 2] {
        let brownian = |drift: f64, volatility: f64| StepLaw::Brownian {
            mean: (drift - volatility * volatility / 2.0) / steps,
            deviation: volatility / steps.sqrt(),
        };
        match *model {
            SyntheticModel::GeometricBrownianMotion { drift, volatility } => {
                [brownian(drift, volatility); 2]
            }
            SyntheticModel::OrnsteinUhlenbeck { mean_price, reversion, volatility } => {
                let span = reversion / steps;
                // `-expm1(-2kh)` is `1 - e^-2kh` without the cancellation a tiny `kh` would suffer.
                let deviation = if reversion > 0.0 {
                    volatility * (-libm::expm1(-2.0 * span) / (2.0 * reversion)).sqrt()
                } else {
                    volatility / steps.sqrt()
                };
                let law = StepLaw::MeanReverting {
                    mean_log: libm::log(mean_price),
                    decay: libm::exp(-span),
                    deviation,
                };
                [law; 2]
            }
            SyntheticModel::RegimeSwitching { regimes, .. } => [
                brownian(regimes[0].drift, regimes[0].volatility),
                brownian(regimes[1].drift, regimes[1].volatility),
            ],
            SyntheticModel::JumpDiffusion {
                drift,
                volatility,
                jump_intensity,
                jump_mean,
                jump_volatility,
            } => {
                let law = StepLaw::Jumping {
                    mean: (drift - volatility * volatility / 2.0) / steps,
                    deviation: volatility / steps.sqrt(),
                    intensity: jump_intensity / steps,
                    jump_mean,
                    jump_volatility,
                };
                [law; 2]
            }
        }
    }

    /// The log price one sub-step after `log_price`.
    fn next(self, log_price: f64, draws: &mut Draws) -> f64 {
        match self {
            StepLaw::Brownian { mean, deviation } => log_price + mean + deviation * draws.normal(),
            StepLaw::MeanReverting { mean_log, decay, deviation } => {
                mean_log + (log_price - mean_log) * decay + deviation * draws.normal()
            }
            StepLaw::Jumping { mean, deviation, intensity, jump_mean, jump_volatility } => {
                let mut next = log_price + mean + deviation * draws.normal();
                // No draw at all for a zero intensity, so that path IS the Brownian path of its seed.
                if intensity > 0.0 {
                    for _ in 0..draws.poisson(intensity) {
                        next += jump_mean + jump_volatility * draws.normal();
                    }
                }
                next
            }
        }
    }
}

/// The one random stream of a generator, and the laws drawn from it.
struct Draws(ChaCha8Rng);

impl Draws {
    /// Uniform on `(0, 1]`: 53 random bits, never 0, so its log is finite.
    fn unit(&mut self) -> f64 {
        ((self.0.next_u64() >> 11) + 1) as f64 / 9_007_199_254_740_992.0
    }

    /// Standard normal by Box-Muller (`vike_math::random::box_muller`, the cosine half): the radius
    /// uniform is drawn first, and [`Self::unit`] is never 0, so its log is finite.
    fn normal(&mut self) -> f64 {
        let u1 = self.unit();
        vike_math::random::box_muller(u1, self.unit())
    }

    /// Exponential waiting time with rate `rate > 0`.
    fn exponential(&mut self, rate: f64) -> f64 {
        -libm::log(self.unit()) / rate
    }

    /// Uniform index in `0..n` by multiply-shift (bias below `n / 2^64`); `n > 0`.
    fn index(&mut self, n: usize) -> usize {
        ((u128::from(self.0.next_u64()) * n as u128) >> 64) as usize
    }

    /// Poisson count with mean `rate > 0`: exponential arrivals counted until the span's end.
    fn poisson(&mut self, rate: f64) -> u64 {
        let mut count = 0;
        let mut arrival = self.exponential(rate);
        while arrival <= 1.0 {
            count += 1;
            arrival += self.exponential(rate);
        }
        count
    }
}

#[path = "synthetic_tests.rs"]
#[cfg(test)]
mod synthetic_tests;
