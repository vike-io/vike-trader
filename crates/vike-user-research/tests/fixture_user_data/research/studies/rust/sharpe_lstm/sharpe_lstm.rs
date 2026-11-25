//! `sharpe_lstm` — a network trained DIRECTLY on the Sharpe ratio of the positions it takes (Deep
//! Momentum Networks: Lim, Zohren, Roberts, 2019), judged on bars it never saw, and its `model.onnx`
//! saved to the run (W24's consumer).
//!
//! The study builds the rows; the NETWORK is the recipe's `[learner]` table, which the host hands to
//! its network trainer (`sharpe_lstm.toml`: `kind = "neural"`, `network = "lstm"`, `loss = "sharpe"`).
//! The study names no network: the same file runs whatever learner the recipe asks for.
//!
//! * **Rows**, one per bar, in TIME order (the Sharpe objective trades them in sequence): the `steps`
//!   returns ending at the bar (`[learner] steps`, the network's own window; 16 when the table has
//!   none), oldest first, one feature per step (an LSTM's step-major row), each
//!   divided by the trailing standard deviation of returns at that bar (`vol_window` returns,
//!   `vike_indicators::series::stddev`). The label is the NEXT bar's return over the same deviation:
//!   a volatility-scaled return, as in the paper. No row sees its own label.
//! * **Fit, then judge.** The first `fit_share` of the rows fit the model; the later rows it never saw
//!   are where it is judged. A model answering a VALUE holds that value as its position (the Sharpe
//!   objective's `tanh`, in `[-1, 1]`); one answering a PROBABILITY `p` (a classifier) holds `2p - 1`.
//! * **Judged as a backtest judges it.** Each later position is a weight of the equity, decided at its
//!   row's bar's close and filled at the next bar's open by the vectorized kernel
//!   (`vike_simulator::VectorBacktestEngine::run_over_bars`) from `cash` (10,000), charged the recipe's costs: the
//!   backtest's own keys, `commission` and `slippage` with their units (`vike_user_research::costs_of_recipe`),
//!   none when absent. `[learner] turnover_cost` is the network's loss term, which the study never charges.
//! * **Metrics**: the row counts; from the first later decision on, the Sharpe ratio of that run's equity
//!   (`vike_analytics::metrics::sharpe`, the backtest report's), annualised by `periods_per_year`, against
//!   holding the whole equity long from the same bar under the same costs; the mean |position| and the mean
//!   turnover per bar.
//! * **Artifacts**: `model.onnx` when the learner hands back a network (its text is the base64 the
//!   host writes into the run as the bytes); `model.txt` for a tree model's text; `positions.tsv`
//!   (time, position, label) over the later rows.

use vike_analytics::metrics::sharpe;
use vike_indicators::series::stddev;
use vike_ml_prediction::{PredictionKind, parse_model_text};
use vike_ml_training::{Capture, DEFAULT_POINT, GridPoint, TrainData};
use vike_simulator::VectorBacktestEngine;
use vike_user_research::{StudyContext, StudyError, StudyOutcome, costs_of_recipe};

/// One text value of the recipe, or a refusal naming the missing key.
fn text<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| StudyError::Study(format!("the recipe names no `{key}`")))
}

/// One number of the recipe (an integer reads as itself), or `default`.
fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    match params.get(key) {
        Some(toml::Value::Float(value)) => *value,
        Some(toml::Value::Integer(value)) => *value as f64,
        _ => default,
    }
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let (venue, symbol, interval) =
        (text(params, "venue")?, text(params, "symbol")?, text(params, "interval")?);
    // The window is the NETWORK's (its `[learner]` table), so the rows the study builds are the rows
    // the trainer was told about; a learner table without one gets 16 steps.
    let steps =
        params.get("learner").map_or(16.0, |table| number(table, "steps", 16.0)).max(1.0) as usize;
    let costs = costs_of_recipe(params)?;
    let cash = number(params, "cash", 10_000.0);
    let vol_window = number(params, "vol_window", 24.0);
    let fit_share = number(params, "fit_share", 0.7).clamp(0.0, 1.0);
    let periods_per_year = number(params, "periods_per_year", 1.0);
    let seed = number(params, "seed", 0.0).max(0.0) as u64;
    let learning_rate = number(params, "learning_rate", DEFAULT_POINT.learning_rate);

    let learner = ctx.learner().ok_or_else(|| {
        StudyError::NoLearner(format!("a network over {venue}/{symbol}/{interval}"))
    })?;
    let bars = ctx.bars(venue, symbol, interval, ctx.window())?;
    let closes: Vec<f64> = bars.iter().map(|bar| bar.close).collect();
    let returns: Vec<f64> = closes.windows(2).map(|pair| pair[1] / pair[0] - 1.0).collect();
    let deviation = stddev(&returns, vol_window).map_err(|e| StudyError::Study(e.to_string()))?;

    // Row at return `i`: the window of returns ending at `i`, and the return at `i + 1`.
    let (mut x, mut y, mut times, mut decided) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for i in steps.saturating_sub(1)..returns.len().saturating_sub(1) {
        let scale = deviation[i];
        if !(scale.is_finite() && scale > 0.0) {
            continue;
        }
        x.extend(returns[i + 1 - steps..=i].iter().map(|r| r / scale));
        y.push((returns[i + 1] / scale) as f32);
        // The bar whose close the label's return ends at.
        times.push(bars[i + 2].timestamp);
        // The bar whose close the row's last return ends at: where its position is decided.
        decided.push(i + 1);
    }
    let rows = y.len();
    let fit = (rows as f64 * fit_share) as usize;
    if fit < 2 * steps || rows - fit < 2 {
        return Err(StudyError::Study(format!(
            "{venue}/{symbol}/{interval}: {rows} row(s) give {fit} to fit and {} to judge; too few",
            rows - fit
        )));
    }

    let data = TrainData::new(&x[..fit * steps], &y[..fit], steps)
        .map_err(|e| StudyError::Study(format!("the rows were refused: {e}")))?;
    let point = GridPoint { learning_rate, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (model, _, model_text) = learner
        .fit_captured(&data, &point, seed, want)
        .map_err(|e| StudyError::Study(format!("the fit was refused: {e}")))?;

    let kind = model.kind();
    let mut answer = vec![0.0; kind.output_count()];
    let mut positions = Vec::with_capacity(rows - fit);
    for row in x[fit * steps..].chunks(steps) {
        model.predict_into(row, &mut answer).map_err(StudyError::Study)?;
        positions.push(match kind {
            PredictionKind::Value => answer[0],
            PredictionKind::Probability => 2.0 * answer[0] - 1.0,
            PredictionKind::ClassProbabilities { .. } => {
                return Err(StudyError::Study(
                    "a model answering class probabilities holds no position".into(),
                ));
            }
        });
    }
    let later: Vec<f64> = y[fit..].iter().map(|&label| f64::from(label)).collect();
    let turnover: f64 = positions.windows(2).map(|pair| (pair[1] - pair[0]).abs()).sum::<f64>()
        / later.len() as f64;
    // The positions as the weights a backtest is handed, and the whole equity held long from the same bar,
    // both charged the recipe's costs.
    let first = decided[fit];
    let mut weights = vec![f64::NAN; bars.len()];
    for (&bar, &position) in decided[fit..].iter().zip(&positions) {
        weights[bar] = position;
    }
    let mut hold = vec![f64::NAN; bars.len()];
    hold[first] = 1.0;
    let backtest = |weights: &[f64]| {
        VectorBacktestEngine::run_over_bars(&[bars.as_slice()], weights, &costs, cash, 1.0)
            .map(|run| run.equity_curve)
            .map_err(StudyError::Study)
    };
    let (held, holding) = (backtest(&weights)?, backtest(&hold)?);

    let mut out = StudyOutcome::new();
    out.metric("rows", rows as f64)?;
    out.metric("fit_rows", fit as f64)?;
    out.metric("later_rows", later.len() as f64)?;
    out.metric("later_sharpe", sharpe(&held[first..], periods_per_year))?;
    out.metric("later_sharpe_hold", sharpe(&holding[first..], periods_per_year))?;
    out.metric(
        "later_mean_abs_position",
        positions.iter().map(|p| p.abs()).sum::<f64>() / positions.len() as f64,
    )?;
    out.metric("later_turnover_per_bar", turnover)?;
    if let Some(model_text) = model_text {
        // A tree model's text parses as one; anything else a learner hands back is a network's ONNX.
        let name = if parse_model_text(&model_text).is_ok() { "model.txt" } else { "model.onnx" };
        out.artifact(name, model_text)?;
    }
    let mut table = String::from("time\tposition\tlabel\n");
    for ((time, position), label) in times[fit..].iter().zip(&positions).zip(&later) {
        table.push_str(&format!("{time}\t{position}\t{label}\n"));
    }
    out.artifact("positions.tsv", table)?;
    Ok(out)
}
