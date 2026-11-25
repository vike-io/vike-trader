//! `funding_carry` — the book's chapter-38 case: THE PERPETUAL'S BASIS AND THE FUNDING IT PAYS.
//!
//! Hold the spot and short the same notional of the perpetual and the position has no price risk to first order.
//! It is paid the funding while the perpetual trades rich and it earns or loses the change of the basis (the
//! perpetual over the spot). The study measures both halves on the venue's own history and then asks whether the
//! funding helps predict where the basis goes next.
//!
//! **The carry.** The rule holds the position while the funding of the last settlement stays above `entry` and
//! leaves once it falls below `exit` (`vike_arbitrage::basis_and_funding::funding_carry`). The decision at a bar's
//! open knows only the settlements before it; what a bar is paid is the settlements INSIDE it. Each change of the
//! position is traded as a backtest trades it: the spot long and the perpetual short, each the whole equity,
//! filled at that bar's open by the vectorized kernel (`vike_simulator::VectorBacktestEngine::run_over_bars`) from
//! `cash` (10,000), charged the recipe's costs, the backtest's own keys (`commission` and `slippage` with their
//! units, `vike_user_research::costs_of_recipe`, none when absent). The profit is split into funding income (the
//! rule's, per unit of notional), basis change (the two legs' run with no cost) and cost (that run's equity less
//! the charged one's), each on `cash`, so the chapter can say which one carried the result.
//!
//! **The model.** The basis is a spread of two log prices with a hedge ratio of one, so it gets the model of
//! `pairs_reversion`: a ridge regression of its move over the next `horizon` bars (in units of its own recent
//! deviation) on its z-score, its momentum and its half-life, fitted on the earlier `train_fraction` of the bars
//! and judged on the later rest, with the training rows whose label reaches into the test window dropped. It is
//! fitted twice, with and without the last settled funding rate as one more feature (in basis points, so the ridge
//! penalty does not shrink it to nothing), so `funding_helps` says what the funding adds: the difference of the two
//! predictions' correlations with what happened.
//!
//! Recipe. Required: `venue`, `spot_symbol`, `perpetual_symbol` and `interval` (the bars of both; the funding
//! series is `ctx.bars(venue, perpetual_symbol, "funding", …)`, which `vike-cli data history fetch
//! VENUE:SYMBOL:funding` fills). Optional: `entry` (0.0001: a funding rate per settlement), `exit` (0.0), the
//! costs (`commission`, `slippage` and their units, none by default; a retired `cost` key is refused), `cash`
//! (10,000), `window` (48), `momentum` (6), `horizon` (3), `penalty` (1.0), `train_fraction` (0.6).
//!
//! Metrics: `bars`, `settlements`, `mean_funding`, `annualised_funding` (the mean rate times the settlements in a
//! year, from the median spacing of the settlements), `mean_basis`, the carry (`carry_share_in_market`,
//! `carry_funding`, `carry_basis`, `carry_cost`, `carry_net` = funding + basis − cost, and `carry_sharpe`, the
//! Sharpe ratio of the charged run's equity plus the funding paid so far, `vike_analytics::metrics::sharpe`),
//! and the model (`train_rows`, `test_rows`, `zscore_coefficient`, `funding_coefficient`,
//! `correlation_with_funding`, `correlation_without_funding`, `funding_helps`). Artifacts: `chart_carry.tsv`
//! (cumulative funding, basis and net profit on `cash`) and `model.txt` (the model that used the funding, which a
//! strategy file loads with `vike_ml_prediction::any_model::AnyModel::from_text` and feeds the same feature row).

use vike_analytics::metrics::sharpe;
use vike_arbitrage::basis_and_funding::{
    CarryRule, funding_carry, funding_in_bars, last_settled_funding, log_basis,
};
use vike_arbitrage::spread::{self, ReversionConfig, SpreadSignals, log_prices, reversion_dataset};
use vike_marketdata::Bar;
use vike_marketdata::interval::interval_ms;
use vike_math::linear_algebra::{Matrix, correlation_matrix};
use vike_ml_prediction::Predictor;
use vike_ml_prediction::linear::LinearModel;
use vike_ml_training::TrainData;
use vike_ml_training::linear::{LinearSpec, fit_linear};
use vike_model::{Commission, Costs};
use vike_options::MS_PER_YEAR;
use vike_simulator::VectorBacktestEngine;
use vike_user_research::{StudyContext, StudyError, StudyOutcome, costs_of_recipe};

#[cfg(test)]
pub mod fixture_store;

fn refuse(why: impl Into<String>) -> StudyError {
    StudyError::Study(why.into())
}

fn required<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| refuse(format!("the recipe must name `{key}` (a string)")))
}

fn count(params: &toml::Value, key: &str, default: i64, least: i64) -> Result<usize, StudyError> {
    let value = params.get(key).and_then(toml::Value::as_integer).unwrap_or(default);
    if value < least {
        return Err(refuse(format!("`{key}` = {value} is below {least}")));
    }
    Ok(value as usize)
}

/// A recipe number written as `0.5` or as `1`.
fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    let value = params.get(key);
    value
        .and_then(toml::Value::as_float)
        .or_else(|| value.and_then(toml::Value::as_integer).map(|i| i as f64))
        .unwrap_or(default)
}

/// A ridge model of `dataset`'s training rows, and its predictions with the labels of the test rows.
struct Fitted {
    model: LinearModel,
    train_rows: usize,
    /// Each test row's prediction and label.
    test: Vec<(f64, f64)>,
}

/// Fit on the rows whose label is known before bar `split` and predict the rows at or after it.
fn fit_and_predict(
    signals: &SpreadSignals,
    config: ReversionConfig,
    extra: &[&[f64]],
    split: usize,
    penalty: f64,
) -> Result<Fitted, StudyError> {
    let dataset = reversion_dataset(signals, config, extra)
        .ok_or_else(|| refuse("the basis gives no feature rows"))?;
    let columns = dataset.columns;
    let (mut train_x, mut train_y, mut test_rows) = (Vec::new(), Vec::new(), Vec::new());
    for (row, &bar) in dataset.bars.iter().enumerate() {
        let features = &dataset.features[row * columns..(row + 1) * columns];
        if bar + config.horizon < split {
            train_x.extend_from_slice(features);
            train_y.push(dataset.labels[row]);
        } else if bar >= split {
            test_rows.push((features, f64::from(dataset.labels[row])));
        }
    }
    if train_y.len() <= 2 * columns || test_rows.is_empty() {
        return Err(refuse(format!(
            "{} training rows and {} test rows are too few to fit and judge a model",
            train_y.len(),
            test_rows.len()
        )));
    }
    let data = TrainData::new(&train_x, &train_y, columns)
        .map_err(|e| refuse(format!("the training rows were rejected: {e}")))?;
    let model = fit_linear(&data, LinearSpec::Ridge { penalty })
        .map_err(|e| refuse(format!("the fit was refused: {e}")))?;
    let mut test = Vec::with_capacity(test_rows.len());
    for (features, label) in &test_rows {
        test.push((model.predict_one(features).map_err(refuse)?, *label));
    }
    Ok(Fitted { model, train_rows: train_y.len(), test })
}

/// The correlation of the predictions with the labels.
fn correlation(test: &[(f64, f64)]) -> f64 {
    let values: Vec<f64> = test.iter().flat_map(|(p, l)| [*p, *l]).collect();
    Matrix::from_row_major(test.len(), 2, values)
        .and_then(|m| correlation_matrix(&m))
        .map_or(f64::NAN, |m| m.get(0, 1))
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = required(params, "venue")?;
    let spot_symbol = required(params, "spot_symbol")?;
    let perpetual_symbol = required(params, "perpetual_symbol")?;
    let interval = required(params, "interval")?;
    let window = count(params, "window", 48, 2)?;
    let momentum = count(params, "momentum", 6, 1)?;
    let horizon = count(params, "horizon", 3, 1)?;
    let (entry, exit) = (number(params, "entry", 0.0001), number(params, "exit", 0.0));
    if params.get("cost").is_some() {
        return Err(refuse(
            "`cost` is retired: state the backtest's `commission` and `slippage` (with their units)",
        ));
    }
    let costs = costs_of_recipe(params)?;
    let cash = number(params, "cash", 10_000.0);
    let penalty = number(params, "penalty", 1.0);
    let train_fraction = number(params, "train_fraction", 0.6);
    if !(0.1..=0.9).contains(&train_fraction) {
        return Err(refuse(format!("`train_fraction` = {train_fraction} is outside 0.1..=0.9")));
    }
    let bar_ms = interval_ms(interval)
        .ok_or_else(|| refuse(format!("`interval` = {interval:?} is not a bar interval")))?;

    let spot = ctx.bars(venue, spot_symbol, interval, ctx.window())?;
    let perpetual = ctx.bars(venue, perpetual_symbol, interval, ctx.window())?;
    let funding = ctx.bars(venue, perpetual_symbol, "funding", ctx.window())?;
    for (what, found) in
        [("spot", spot.len()), ("perpetual", perpetual.len()), ("funding", funding.len())]
    {
        if found == 0 {
            return Err(refuse(format!("no {what} bars in the requested window")));
        }
    }
    // The bars both legs have.
    let (mut opens, mut spot_bars, mut perpetual_bars) = (Vec::new(), Vec::new(), Vec::new());
    let mut at = 0;
    for bar in &spot {
        while at < perpetual.len() && perpetual[at].timestamp < bar.timestamp {
            at += 1;
        }
        if at < perpetual.len() && perpetual[at].timestamp == bar.timestamp {
            opens.push(bar.timestamp);
            spot_bars.push(bar.clone());
            perpetual_bars.push(perpetual[at].clone());
        }
    }
    let closes = |legs: &[Bar]| -> Vec<f64> { legs.iter().map(|bar| bar.close).collect() };
    let (spot_closes, perpetual_closes) = (closes(&spot_bars), closes(&perpetual_bars));
    let bars = opens.len();
    let split = (bars as f64 * train_fraction) as usize;
    if split < 4 * window || bars - split < 4 * window {
        return Err(refuse(format!(
            "{bars} aligned bars split at {split} leave fewer than four windows of {window} on a side"
        )));
    }
    // Only a settlement with a rate counts; a venue that served none for a time wrote no row.
    let settlements: Vec<(i64, f64)> =
        funding.iter().filter_map(|b| b.funding.map(|rate| (b.timestamp, rate))).collect();
    if settlements.len() < 2 {
        return Err(refuse("the funding series holds fewer than two settlements"));
    }

    let basis = log_basis(&perpetual_closes, &spot_closes)
        .ok_or_else(|| refuse("the two legs differ in length"))?;
    let income = funding_in_bars(&opens, bar_ms, &settlements);
    let known = last_settled_funding(&opens, &settlements);
    let carry = funding_carry(&basis, &income, &known, CarryRule { entry, exit })
        .ok_or_else(|| refuse("the carry's series differ in length"))?;
    // Each change of the rule's position as the two legs' weights, decided at the close before the bar it holds
    // through and filled at that bar's open (the module doc), run with no cost and with the recipe's.
    let mut weights = vec![f64::NAN; 2 * bars];
    for t in 1..bars {
        let held = carry.position[t];
        if held != carry.position[t - 1] {
            weights[2 * (t - 1)] = held;
            weights[2 * (t - 1) + 1] = -held;
        }
    }
    let legs = [spot_bars.as_slice(), perpetual_bars.as_slice()];
    let backtest = |costs: &Costs| {
        VectorBacktestEngine::run_over_bars(&legs, &weights, costs, cash, 1.0)
            .map(|run| run.equity_curve)
            .map_err(refuse)
    };
    let free = Costs {
        commission: Commission::None,
        commission_set: false,
        slippage: None,
        slippage_set: false,
    };
    let (gross, charged) = (backtest(&free)?, backtest(&costs)?);
    // The charged run's equity plus the funding paid so far on `cash`: the carry's net, bar by bar.
    let mut paid = 0.0;
    let net: Vec<f64> = charged
        .iter()
        .zip(&carry.funding)
        .map(|(equity, funding)| {
            paid += funding;
            equity + cash * paid
        })
        .collect();
    let last = bars - 1;

    let mut gaps: Vec<i64> = settlements.windows(2).map(|w| w[1].0 - w[0].0).collect();
    gaps.sort_unstable();
    let median_gap = vike_math::statistics::high_median_of_sorted(&gaps)
        .ok_or_else(|| refuse("fewer than two funding settlements"))?;
    let mean_funding =
        settlements.iter().map(|(_, rate)| rate).sum::<f64>() / settlements.len() as f64;
    let finite_basis: Vec<f64> = basis.iter().copied().filter(|b| b.is_finite()).collect();
    // The platform's one 365-day year in milliseconds (a perpetual pays every calendar day).
    let bars_per_year = MS_PER_YEAR / bar_ms as f64;

    let mut out = StudyOutcome::new();
    out.metric("bars", bars as f64)?;
    out.metric("settlements", settlements.len() as f64)?;
    out.metric("mean_funding", mean_funding)?;
    out.metric("annualised_funding", mean_funding * MS_PER_YEAR / median_gap as f64)?;
    out.metric("mean_basis", finite_basis.iter().sum::<f64>() / finite_basis.len().max(1) as f64)?;
    out.metric("carry_share_in_market", carry.position.iter().sum::<f64>() / bars as f64)?;
    out.metric("carry_funding", carry.funding.iter().sum::<f64>())?;
    out.metric("carry_basis", gross[last] / cash - 1.0)?;
    out.metric("carry_cost", (gross[last] - charged[last]) / cash)?;
    out.metric("carry_net", net[last] / cash - 1.0)?;
    out.metric("carry_sharpe", sharpe(&net, bars_per_year))?;

    let mut chart = String::from("time\tvalue\tseries\tkind\n");
    let mut cumulative_funding = 0.0;
    for (t, time) in opens.iter().enumerate() {
        cumulative_funding += carry.funding[t];
        chart.push_str(&format!("{time}\t{cumulative_funding}\tfunding\tline\n"));
        chart.push_str(&format!("{time}\t{}\tbasis\tline\n", gross[t] / cash - 1.0));
        chart.push_str(&format!("{time}\t{}\tnet\tline\n", net[t] / cash - 1.0));
    }
    out.artifact("chart_carry.tsv", chart)?;

    // The model: the basis is a spread of two log prices at a hedge ratio of one.
    let signals =
        spread::signals(&log_prices(&perpetual_closes), &log_prices(&spot_closes), 1.0, window)
            .ok_or_else(|| refuse("the window is outside what the spread indicators accept"))?;
    let config = ReversionConfig { momentum, horizon };
    // The funding enters in basis points per settlement (hundredths of a percent): the ridge penalty acts on the
    // scale it is handed, and a rate of 0.0002 beside z-scores of order one would be shrunk to nothing.
    let known_in_basis_points: Vec<f64> = known.iter().map(|rate| rate * 10_000.0).collect();
    let with = fit_and_predict(&signals, config, &[&known_in_basis_points], split, penalty)?;
    let without = fit_and_predict(&signals, config, &[], split, penalty)?;
    let (with_correlation, without_correlation) =
        (correlation(&with.test), correlation(&without.test));
    out.metric("train_rows", with.train_rows as f64)?;
    out.metric("test_rows", with.test.len() as f64)?;
    out.metric("zscore_coefficient", with.model.coefficients[0])?;
    out.metric("funding_coefficient", with.model.coefficients[with.model.coefficients.len() - 1])?;
    out.metric("correlation_with_funding", with_correlation)?;
    out.metric("correlation_without_funding", without_correlation)?;
    out.metric("funding_helps", with_correlation - without_correlation)?;
    out.artifact("model.txt", with.model.to_text())?;
    Ok(out)
}
