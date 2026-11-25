//! `deep_hedging` — a network that HEDGES an option, against the delta hedge (the book's options part,
//! "Option market making and hedging"; Buehler, Gonon, Teichmann and Wood, *Deep Hedging*, 2019). An
//! option is held to its expiry and hedged with a future of its underlying, in the event simulator with
//! the recipe's commission on every hedge trade (`vike_simulator::HedgingEnvironment`): the backtest's own
//! key, read into the one cost type (`vike_user_research::costs_of_recipe`) and charged as a backtest charges
//! it (`vike_model::Commission::engine_fee`).
//!
//! 1. **The option** of every episode: `right`, a strike of `strike_ratio` times the episode's first
//!    close, `quantity` units of the underlying (`-1` is short one), sold at its Black–Scholes premium at
//!    `volatility` for the episode's length (`vike_options::black_scholes_price`). It expires at the
//!    episode's last bar.
//! 2. **Train** on the first `train_fraction` of the hedge's bars: every `path_steps` consecutive bars
//!    starting each `stride` bars is one path (`vike_simulator::hedging_training_rows`), handed to the
//!    study's learner, whose recipe `[learner]` is a network with `loss = "hedging"`. Its `path_steps` is
//!    also the episode's length, and its `fee_rate` (the loss's fraction of each trade's notional) must be
//!    the recipe's `commission` (in `percent`, the only unit that loss charges), so the policy is trained
//!    on the costs it is judged by. A recipe stating a slippage is refused: the loss charges none.
//! 3. **Judge** on the rest: each whole window of `path_steps` bars, one after another, is one episode,
//!    played three ways with the same fills and fees: no hedge, the Black–Scholes delta at `volatility`
//!    (`vike_options::black_scholes_greeks`), and the network. An episode's summed reward is the hedged
//!    book's P&L; it is reported over the episode's first close.
//!
//! Recipe (no default for anything that names a market):
//!
//! ```toml
//! venue = "binance"             # the hedge's bars: a future (a perpetual) of the underlying
//! symbol = "BTCUSDT.P"
//! interval = "1h"
//! volatility = 0.55             # the annual volatility of the premium and of the delta hedge
//! right = "call"                # optional: "call" or "put"
//! strike_ratio = 1.0            # optional
//! quantity = -1.0               # optional
//! train_fraction = 0.7          # optional
//! stride = 1                    # optional: bars between two training paths
//! learning_rate = 0.01          # optional
//! cash = 1000000.0              # optional: the engine's starting cash
//! seed = 0                      # optional
//! commission = 0.05             # optional: 0.05% of each hedge trade's value, the backtest's key
//!
//! [learner]
//! kind = "neural"
//! network = "mlp"
//! hidden = [16, 16]
//! loss = "hedging"
//! path_steps = 168              # bars per episode: a week of hours
//! price_column = 0              # vike_simulator::HEDGE_PRICE_COLUMN
//! fee_rate = 0.0005             # the commission above as a fraction
//! epochs = 60
//! batch_size = 32               # paths per step
//! ```
//!
//! Metrics: `paths_train`, `episodes`, and for each of `none`, `delta` and `network`: `<policy>_pnl_mean`,
//! `<policy>_pnl_std` and `<policy>_pnl_q05` (the 5% quantile), P&L over the first close. Artifacts:
//! `chart_hedged_pnl.tsv` (each test episode's P&L per policy at its first bar) and `model.onnx` when the
//! learner hands its model's text back (a network's).

use vike_marketdata::OptionKind;
use vike_ml_prediction::Predictor;
use vike_ml_training::{Capture, DEFAULT_POINT, GridPoint, TrainData};
use vike_model::FeeSchedule;
use vike_simulator::{
    HEDGE_FEATURES, HEDGE_PRICE_COLUMN, HedgeObservation, HedgedOption, HedgingEnvironment,
    StrategyEngineConfig, hedging_training_rows,
};
use vike_user_research::{StudyContext, StudyError, StudyOutcome, costs_of_recipe};

const POLICIES: [&str; 3] = ["none", "delta", "network"];

fn text<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params.get(key).and_then(toml::Value::as_str).ok_or_else(|| {
        StudyError::Study(format!("the recipe must name `{key}` (no market has a default)"))
    })
}

fn number(params: &toml::Value, key: &str) -> Option<f64> {
    params.get(key).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
}

/// The hedge each policy holds at a decision.
fn hedge(
    policy: &str,
    seen: &HedgeObservation,
    option: &HedgedOption,
    volatility: f64,
    network: &dyn Predictor,
) -> Result<f64, StudyError> {
    Ok(match policy {
        "none" => 0.0,
        "delta" => vike_options::black_scholes_greeks(
            seen.market.price,
            seen.strike,
            seen.years_to_expiry(),
            volatility,
            option.right,
            0.0,
        )
        .map_or(0.0, |(delta, ..)| -option.quantity * delta),
        _ => network.predict_one(&seen.features()).map_err(StudyError::Study)?,
    })
}

/// The mean, the population standard deviation and the 5% quantile of `values`.
fn summary(values: &[f64]) -> (f64, f64, f64) {
    let (mean, variance) = vike_math::statistics::mean_variance(values.iter().copied(), 0.0);
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    (mean, variance.sqrt(), vike_math::statistics::percentile(&sorted, 0.05))
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = text(params, "venue")?;
    let symbol = text(params, "symbol")?;
    let interval = text(params, "interval")?;
    let volatility = number(params, "volatility")
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| StudyError::Study("the recipe must name a positive `volatility`".into()))?;
    let right = match params.get("right").and_then(toml::Value::as_str).unwrap_or("call") {
        "call" => OptionKind::Call,
        "put" => OptionKind::Put,
        other => return Err(StudyError::Study(format!("right {other:?}: \"call\" or \"put\""))),
    };
    let strike_ratio = number(params, "strike_ratio").unwrap_or(1.0);
    let quantity = number(params, "quantity").unwrap_or(-1.0);
    let train_fraction = number(params, "train_fraction").unwrap_or(0.7);
    let stride = number(params, "stride").unwrap_or(1.0).max(1.0) as usize;
    let learning_rate = number(params, "learning_rate").unwrap_or(DEFAULT_POINT.learning_rate);
    let cash = number(params, "cash").unwrap_or(1_000_000.0);
    let seed = params.get("seed").and_then(toml::Value::as_integer).unwrap_or(0).max(0) as u64;
    // The learner's own keys the environment must agree with: one number each, read where it is set.
    let learner_table = params.get("learner");
    let window = learner_table
        .and_then(|t| t.get("path_steps"))
        .and_then(toml::Value::as_integer)
        .filter(|steps| *steps >= 2)
        .ok_or_else(|| StudyError::Study("[learner] must name `path_steps` (2 or more)".into()))?
        as usize;
    let price_column =
        learner_table.and_then(|t| t.get("price_column")).and_then(toml::Value::as_integer);
    if price_column != Some(HEDGE_PRICE_COLUMN as i64) {
        return Err(StudyError::Study(format!(
            "[learner] price_column must be {HEDGE_PRICE_COLUMN}, the hedging environment's"
        )));
    }
    // The engine charges the recipe's commission, the one cost type, as a backtest charges it; the hedging loss
    // charges a fraction of each trade's notional, so the learner's `fee_rate` must be that same fraction.
    let costs = costs_of_recipe(params)?;
    if costs.slippage.is_some() {
        return Err(StudyError::Study(
            "the hedging loss charges no slippage, so the recipe may state none".into(),
        ));
    }
    let (fee_rate, fee_schedule) = costs.commission.engine_fee();
    let charged = match fee_schedule {
        None => 0.0,
        Some(schedule @ FeeSchedule::PercentMakerTaker { .. }) => schedule.maker_taker_rates().1,
        Some(_) => {
            return Err(StudyError::Study(
                "the hedging loss charges a fraction of the notional: state `commission` in percent"
                    .into(),
            ));
        }
    };
    let trained = learner_table.and_then(|t| number(t, "fee_rate")).unwrap_or(0.0);
    // The loss trains in `f32`: the two must agree as the network sees them.
    if trained as f32 != charged as f32 {
        return Err(StudyError::Study(format!(
            "[learner] fee_rate = {trained} is not the recipe's commission, {charged} of the notional: \
             the policy is trained on the costs it is judged by"
        )));
    }
    let Some(interval_ms) = vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0)
    else {
        return Err(StudyError::Study(format!("interval {interval:?} is not a bar interval")));
    };
    if !(train_fraction > 0.0 && train_fraction < 1.0) {
        return Err(StudyError::Study(format!("train_fraction {train_fraction} is not in (0, 1)")));
    }
    let learner = ctx.learner().ok_or_else(|| {
        StudyError::NoLearner(format!("a hedging network for {venue}'s {symbol}"))
    })?;

    let years = (window - 1) as f64 * interval_ms as f64 / vike_options::MS_PER_YEAR;
    let premium_ratio =
        vike_options::black_scholes_price(1.0, strike_ratio, years, volatility, right, 0.0)
            .ok_or_else(|| StudyError::Study("the option has no Black–Scholes price".into()))?;
    let option = HedgedOption { right, strike_ratio, quantity, premium_ratio };

    let bars = ctx.bars(venue, symbol, interval, ctx.window())?;
    let cut = (bars.len() as f64 * train_fraction) as usize;
    let rows =
        hedging_training_rows(&bars[..cut], &option, window, stride).map_err(StudyError::Study)?;
    let episodes = (bars.len() - cut) / window;
    if rows.paths == 0 || episodes == 0 {
        return Err(StudyError::Study(format!(
            "{} bar(s) of {symbol}: too few for a path of {window} to train on and one to judge",
            bars.len()
        )));
    }
    let data = TrainData::new(&rows.features, &rows.labels, HEDGE_FEATURES)
        .map_err(|e| StudyError::Study(format!("the training matrix is refused: {e}")))?;
    let point = GridPoint { learning_rate, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (network, _, model_text) = learner
        .fit_captured(&data, &point, seed, want)
        .map_err(|e| StudyError::Study(format!("the fit refused: {e}")))?;

    let config =
        move || StrategyEngineConfig { fee_rate, fee_schedule, cash, ..Default::default() };
    let mut environment =
        HedgingEnvironment::over_bars(symbol, bars[cut..].to_vec(), option, config)
            .map_err(StudyError::Study)?;
    let mut pnl: [Vec<f64>; 3] = Default::default();
    let mut chart = String::from("time\tvalue\tseries\tkind\n");
    for episode in 0..episodes {
        for (policy, results) in POLICIES.iter().zip(pnl.iter_mut()) {
            let mut seen =
                environment.reset_at(episode * window, window).map_err(StudyError::Study)?;
            let (first_time, first_price) = (seen.market.timestamp, seen.start_price);
            let mut total = 0.0;
            loop {
                let held = hedge(policy, &seen, &option, volatility, network.as_ref())?;
                let step = environment.step(held).map_err(StudyError::Study)?;
                total += step.reward;
                if step.done {
                    break;
                }
                seen = step.observation;
            }
            results.push(total / first_price);
            chart.push_str(&format!("{first_time}\t{}\t{policy}\tpoints\n", total / first_price));
        }
    }

    let mut out = StudyOutcome::new();
    out.metric("paths_train", rows.paths as f64)?;
    out.metric("episodes", episodes as f64)?;
    for (policy, results) in POLICIES.iter().zip(&pnl) {
        let (mean, std, q05) = summary(results);
        out.metric(&format!("{policy}_pnl_mean"), mean)?;
        out.metric(&format!("{policy}_pnl_std"), std)?;
        out.metric(&format!("{policy}_pnl_q05"), q05)?;
    }
    out.artifact("chart_hedged_pnl.tsv", chart)?;
    if let Some(model_text) = model_text {
        out.artifact("model.onnx", model_text)?;
    }
    Ok(out)
}
