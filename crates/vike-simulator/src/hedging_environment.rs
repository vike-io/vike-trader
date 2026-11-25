//! A HEDGING environment over the RL environment ([`crate::RlEnvironment`]), research side: an option
//! position is held to its expiry while the agent trades the HEDGE (the one symbol of the held bars: a
//! future, a perpetual) at every decision. Deep hedging (Buehler, Gonon, Teichmann and Wood, *Deep
//! Hedging*, 2019) trains a policy for it; the classical delta hedge is the baseline it is judged against.
//!
//! # The contract
//!
//! - **The option** ([`HedgedOption`]): its right, its strike as a multiple of the episode's first close,
//!   the signed number of underlying units it is written on (`-1.0` is short one option on one unit) and
//!   its premium as a fraction of the first close. An episode of `window` bars opens it at the first
//!   decision (the first bar's close, `S_0`) and it expires at the LAST bar's close (`S_T`), settled in
//!   cash at its intrinsic value by the simulator's own expiry law (`vike_options::intrinsic`).
//! - **The hedge.** Each [`HedgingEnvironment::step`] holds a number of units of the hedge, sent as
//!   [`crate::RlAction::TargetPosition`]: it fills by the simulator's law (a market order on the NEXT
//!   bar's open) and pays the fees and slippage of the caller's config. The decision at the last bar
//!   trades nothing that fills: no bar follows it.
//! - **Reward**: the RL environment's (the change of the account's equity, so the hedge's gains net of
//!   every cost) plus the option leg's cash, `-quantity × premium` on the first step and
//!   `quantity × intrinsic(S_T)` on the last. An episode's rewards sum to the hedged book's terminal P&L.
//! - **Features** ([`HedgeObservation::features`]): `[S_t / S_0, ln(S_t / K), years to expiry]` (a
//!   365-day year, `vike_options`' basis), one function ([`hedge_features`]) for both the observation
//!   and [`hedging_training_rows`], so a policy is trained on exactly what it later observes.
//!
//! # Training rows ([`hedging_training_rows`])
//!
//! A held history becomes paths for the network trainer's `loss = "hedging"`
//! (`vike-ml-neural-training`'s `trainer` module): every `window` consecutive bars starting each `stride`
//! bars is one path, one row per bar in time order, the features above with the price ratio at
//! [`HEDGE_PRICE_COLUMN`], and on every row of a path the same label, the option leg's P&L in units of
//! `S_0`: `quantity × (intrinsic(S_T) / S_0 − premium ratio)`. The trainer then minimises the mean square
//! of `label + Σ δ_t (P_{t+1} − P_t) − fee_rate Σ |δ_t − δ_{t−1}| P_t` over the paths: the hedge's gains
//! on the closes, where the engine fills each change on the next bar's open (the same price on a tape
//! with no gap between a close and the next open).
//!
//! shortcut: the option is RELATIVE (strike and premium as multiples of `S_0`, expiry at the window's end),
//! not a listed contract read from a chain; hedging a listed option's own life is the next step.

use vike_marketdata::{Bar, OptionKind};
use vike_math::time_series::log_ratio;

use crate::{RlAction, RlEnvironment, RlObservation, StrategyEngineConfig};

/// The features' count, the row width [`hedging_training_rows`] lays and [`HedgeObservation::features`] answers.
pub const HEDGE_FEATURES: usize = 3;

/// The price ratio's column in a feature row: the trainer's `price_column` for this layout.
pub const HEDGE_PRICE_COLUMN: usize = 0;

/// The option an episode holds, relative to the episode's first close.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HedgedOption {
    /// Call or put.
    pub right: OptionKind,
    /// The strike over the first close (`1.0` at the money, `1.1` a call 10% out of the money).
    pub strike_ratio: f64,
    /// Units of the underlying the option is written on, signed: `-1.0` is short one option on one unit.
    pub quantity: f64,
    /// The premium of one unit over the first close.
    pub premium_ratio: f64,
}

impl HedgedOption {
    /// Refuses a strike or a premium that is not a positive (or, for the premium, non-negative) finite
    /// number, and a zero or non-finite quantity.
    fn validate(&self) -> Result<(), String> {
        let positive = self.strike_ratio.is_finite() && self.strike_ratio > 0.0;
        let premium = self.premium_ratio.is_finite() && self.premium_ratio >= 0.0;
        if !(positive && premium && self.quantity.is_finite() && self.quantity != 0.0) {
            return Err(format!("{self:?}: strike ratio > 0, premium ratio >= 0, quantity != 0"));
        }
        Ok(())
    }

    /// The leg's P&L in units of the first close when it settles at `last` over `first`.
    fn leg_ratio(&self, first: f64, last: f64) -> f64 {
        let strike = self.strike_ratio * first;
        self.quantity
            * (vike_options::intrinsic(self.right, strike, last) / first - self.premium_ratio)
    }
}

/// `[S_t / S_0, ln(S_t / K), years to expiry]`: what a hedging policy sees at a decision.
pub fn hedge_features(
    price: f64,
    start_price: f64,
    strike: f64,
    timestamp: i64,
    expiry_ms: i64,
) -> [f64; HEDGE_FEATURES] {
    // `vike_options`' ONE year (its `black_scholes_greeks` takes years so counted), clamped at 0.
    let years = vike_options::years_to_expiry_ms(expiry_ms, timestamp);
    // `ln(S / K)`: the negative of `vike_options::log_moneyness`'s `ln(K / S)`, but not bit for bit (the
    // two divisions round differently), so keep this argument order (REVIEW-LAWS L3).
    [price / start_price, log_ratio(price, strike), years]
}

/// What the agent sees at a decision: the market as the RL environment observes it, and the option.
#[derive(Debug, Clone)]
pub struct HedgeObservation {
    /// The hedge's market and the account (`market.position` is the hedge held).
    pub market: RlObservation,
    /// The episode's first close.
    pub start_price: f64,
    /// The option's strike.
    pub strike: f64,
    /// The option's expiry, the last bar's timestamp.
    pub expiry_ms: i64,
}

impl HedgeObservation {
    /// [`hedge_features`] at this decision.
    pub fn features(&self) -> [f64; HEDGE_FEATURES] {
        hedge_features(
            self.market.price,
            self.start_price,
            self.strike,
            self.market.timestamp,
            self.expiry_ms,
        )
    }

    /// Years left to the option's expiry.
    pub fn years_to_expiry(&self) -> f64 {
        self.features()[2]
    }
}

/// What one [`HedgingEnvironment::step`] answers.
#[derive(Debug, Clone)]
pub struct HedgeStep {
    /// The next decision's observation, or the terminal one when `done`.
    pub observation: HedgeObservation,
    /// The hedged book's gain since the previous decision (the option leg's cash included).
    pub reward: f64,
    /// The option has expired: [`HedgingEnvironment::reset`] starts the next episode.
    pub done: bool,
}

/// The episode's option, fixed at its first decision.
#[derive(Debug, Clone, Copy)]
struct HedgeEpisode {
    start_price: f64,
    strike: f64,
    expiry_ms: i64,
    first_step: bool,
}

/// The environment: the hedge's held bars, the option each episode holds, and the engine config.
pub struct HedgingEnvironment {
    environment: RlEnvironment,
    timestamps: Vec<i64>,
    option: HedgedOption,
    episode: Option<HedgeEpisode>,
}

impl HedgingEnvironment {
    /// An environment hedging `option` with `symbol`, deciding once per bar of `bars`.
    pub fn over_bars(
        symbol: &str,
        bars: Vec<Bar>,
        option: HedgedOption,
        config: impl Fn() -> StrategyEngineConfig + Send + Sync + 'static,
    ) -> Result<Self, String> {
        option.validate()?;
        refuse_a_hedge_it_cannot_add_to(symbol, &config())?;
        let timestamps = bars.iter().map(|bar| bar.timestamp).collect();
        let environment = RlEnvironment::over_bars(symbol, bars, config);
        Ok(HedgingEnvironment { environment, timestamps, option, episode: None })
    }

    /// The episode the RL environment would run for `seed` ([`RlEnvironment::reset`]).
    pub fn reset(&mut self, seed: u64, window: usize) -> Result<HedgeObservation, String> {
        self.reset_at(self.environment.seeded_start(seed, window), window)
    }

    /// An episode of `window` bars from `start`: the option opens at the first decision and expires
    /// at the window's last bar.
    pub fn reset_at(&mut self, start: usize, window: usize) -> Result<HedgeObservation, String> {
        self.episode = None;
        let market = self.environment.reset_at(start, window)?;
        let last = start + window.min(self.timestamps.len()) - 1;
        let start_price = market.price;
        if !(start_price.is_finite() && start_price > 0.0) {
            return Err(format!("the first decision's price {start_price} cannot open an option"));
        }
        let episode = HedgeEpisode {
            start_price,
            strike: self.option.strike_ratio * start_price,
            expiry_ms: self.timestamps[last],
            first_step: true,
        };
        self.episode = Some(episode);
        Ok(observe(market, &episode))
    }

    /// Hold `hedge` units of the hedge from this decision, and run to the next one.
    pub fn step(&mut self, hedge: f64) -> Result<HedgeStep, String> {
        let Some(mut episode) = self.episode else {
            return Err("no episode is running: call reset first".to_string());
        };
        let market = self.environment.step(RlAction::TargetPosition(hedge))?;
        let mut reward = market.reward;
        if episode.first_step {
            reward -= self.option.quantity * self.option.premium_ratio * episode.start_price;
            episode.first_step = false;
        }
        if market.done {
            let settle = market.observation.price;
            reward += self.option.quantity
                * vike_options::intrinsic(self.option.right, episode.strike, settle);
            self.episode = None;
        } else {
            self.episode = Some(episode);
        }
        Ok(HedgeStep {
            observation: observe(market.observation, &episode),
            reward,
            done: market.done,
        })
    }

    /// The whole backtest result of the hedge's episode that just ended ([`RlEnvironment::episode_result`]).
    pub fn episode_result(&self) -> Option<&vike_analytics::TradingRecord> {
        self.environment.episode_result()
    }
}

/// Refuse a hedge whose contract is not linear at 1.0 a unit, resolved as the engine resolves it (a listed
/// [`StrategyEngineConfig::contract_values`] entry, else the linear value of its first
/// [`StrategyEngineConfig::multipliers`] match, else [`StrategyEngineConfig::multiplier`]). The option leg is
/// cash per UNDERLYING unit in the quote currency, added to the account's equity change by hand: a coin ledger
/// (an inverse hedge) would add dollars to coin, and a multiplier other than 1 would make one hedge unit stand
/// for several underlying units, which neither the delta-hedge baseline nor [`hedging_training_rows`]' loss
/// knows.
fn refuse_a_hedge_it_cannot_add_to(
    symbol: &str,
    config: &StrategyEngineConfig,
) -> Result<(), String> {
    let multiplier =
        config.multipliers.iter().find(|(s, _)| s == symbol).map_or(config.multiplier, |(_, m)| *m);
    let value = config
        .contract_values
        .iter()
        .find(|(s, _)| s == symbol)
        .map_or(vike_model::ContractValue::linear(multiplier), |(_, v)| *v);
    if value.is_default() {
        Ok(())
    } else {
        Err(format!(
            "the hedge {symbol} is valued {value:?}, not linear at 1.0 a unit: the option leg's cash is per \
             underlying unit in the quote currency and would not add to its equity"
        ))
    }
}

fn observe(market: RlObservation, episode: &HedgeEpisode) -> HedgeObservation {
    HedgeObservation {
        market,
        start_price: episode.start_price,
        strike: episode.strike,
        expiry_ms: episode.expiry_ms,
    }
}

/// Rows for the network trainer's `loss = "hedging"` (module doc): every `window` bars of `bars` starting
/// each `stride` bars is one path.
#[derive(Debug, Clone, PartialEq)]
pub struct HedgingRows {
    /// `paths × window` rows of [`HEDGE_FEATURES`] features, row-major, path after path.
    pub features: Vec<f64>,
    /// One label per row, the same on every row of a path: the option leg's P&L over `S_0`.
    pub labels: Vec<f32>,
    /// How many paths.
    pub paths: usize,
}

/// The paths of `bars` (module doc). Refuses a window under 2 bars, a zero stride, an invalid option
/// and a close that is not a positive finite number.
pub fn hedging_training_rows(
    bars: &[Bar],
    option: &HedgedOption,
    window: usize,
    stride: usize,
) -> Result<HedgingRows, String> {
    option.validate()?;
    if window < 2 || stride == 0 {
        return Err(format!(
            "a path needs a window of 2 bars or more and a stride of 1 or more, got {window} and {stride}"
        ));
    }
    if let Some(bad) = bars.iter().find(|bar| !(bar.close.is_finite() && bar.close > 0.0)) {
        return Err(format!("the bar at {} closes at {}, not a price", bad.timestamp, bad.close));
    }
    let mut rows = HedgingRows { features: Vec::new(), labels: Vec::new(), paths: 0 };
    for path in bars.windows(window).step_by(stride) {
        let (first, last) = (path[0].close, path[window - 1].close);
        let strike = option.strike_ratio * first;
        let label = option.leg_ratio(first, last) as f32;
        for bar in path {
            let features =
                hedge_features(bar.close, first, strike, bar.timestamp, path[window - 1].timestamp);
            rows.features.extend_from_slice(&features);
            rows.labels.push(label);
        }
        rows.paths += 1;
    }
    Ok(rows)
}

#[cfg(test)]
mod tests;
