//! `alpha_pool` — the alpha pool over SAVED RUNS (book W21; Isichenko, *Quantitative Portfolio
//! Management*, ch. 3): the forecasts several strategies' backtests published (book W35, a run's
//! `forecasts.json`), measured against the bars that followed them, weighed and combined into one.
//!
//! The consumer of `vike_portfolio::alpha_pool` and of the study context's saved runs
//! (`StudyContext::runs`): a study file reads no file, so its host reads each run by id.
//!
//! Recipe (lenient, every key but `runs` defaulted):
//! ```toml
//! runs = ["1767225600-ab12cd34ef56ab78-0", "1767225601-ab12cd34ef56ab78-0", "..."]  # two or more
//! venue = "binance"
//! interval = "1m"      # the bars each forecast is measured against: one forecast a bar, one bar ahead
//! long_only = false    # the mean-variance combination may turn a strategy against itself
//! max_clusters = 2     # the hierarchical combination's largest cut (three runs or more)
//! max_lag = 3          # the pool's information decay, in bars
//! names = ["trend", "fade", "..."]  # the members' names in `model.txt`, one per run; the run ids otherwise
//! ```
//!
//! A forecast is stamped when the strategy decided: a bar step's at its bar's CLOSE (`vike_model::ForecastRow`,
//! decision 0271). It is held against the return from that close to the next bar's. Every symbol any run
//! forecast is read for the study's window; a period is a bar of any of them, and `pool.tsv` names it by its
//! close, the decision time.
//!
//! Metrics: `runs`, `symbols`, `periods`, `weight_<i>` (mean-variance, in the recipe's run order),
//! `hierarchical_weight_<i>` (NaN when no clustering exists) and `correlation_<i>_<j>` of the runs'
//! alpha returns: the MODEL, fit on the whole window. What the pool is WORTH is measured OUT OF SAMPLE:
//! weights fit on the window's first half alone, the pool they make scored on the second half
//! (`out_of_sample_periods`), as `out_of_sample_information_lag_<l>` and `out_of_sample_half_life` (NaN when
//! it never halves, or when the first half admits no combination).
//! Artifacts: `pool.tsv`, the whole-window model's forecast beside the realized return, per period and
//! symbol (IN SAMPLE: the weights saw those returns, so it is no measure of the pool; NaN where any run
//! published no forecast: `vike_portfolio::alpha_pool`'s "No partial pool"); and
//! `model.txt`, the mean-variance weights as a pool text (`vike_portfolio::alpha_pool::PoolWeights`): a LIVE
//! strategy file names this study's run by a `type = "model"` key, is handed the text, and combines its
//! own members' forecasts with `PoolWeights::combine` (book W21, the combined forecast in a live strategy).

use vike_marketdata::Bar;
use vike_math::linear_algebra::{Matrix, correlation_matrix};
use vike_model::ForecastRow;
use vike_portfolio::alpha_pool::{
    PoolWeights, alpha_returns, combined_forecast, decay_half_life, hierarchical_weights,
    information_decay, mean_variance_weights,
};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

#[cfg(test)]
pub mod fixture;

fn str_param<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

fn int_param(params: &toml::Value, key: &str, default: i64) -> i64 {
    params.get(key).and_then(|v| v.as_integer()).unwrap_or(default)
}

fn refused(why: impl Into<String>) -> StudyError {
    StudyError::Study(why.into())
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = str_param(params, "venue", "binance");
    let interval = str_param(params, "interval", "1m");
    let step = vike_marketdata::interval::interval_ms(interval)
        .filter(|ms| *ms > 0)
        .ok_or_else(|| refused(format!("`interval` = {interval:?} is not a bar length")))?;
    let long_only = params.get("long_only").and_then(|v| v.as_bool()).unwrap_or(false);
    let max_clusters = int_param(params, "max_clusters", 2).max(0) as usize;
    let max_lag = int_param(params, "max_lag", 3).max(0) as usize;
    let run_ids: Vec<&str> = params
        .get("runs")
        .and_then(|v| v.as_array())
        .map(|ids| ids.iter().filter_map(|id| id.as_str()).collect())
        .unwrap_or_default();
    if run_ids.len() < 2 {
        return Err(refused("the recipe names fewer than two runs in `runs`: a pool needs two"));
    }
    let names: Vec<&str> = match params.get("names").and_then(|v| v.as_array()) {
        Some(names) => names.iter().filter_map(|n| n.as_str()).collect(),
        None => run_ids.clone(),
    };
    if names.len() != run_ids.len() {
        return Err(refused("`names` gives a different number of names than `runs` has runs"));
    }
    let saved = ctx
        .runs()
        .ok_or_else(|| refused("this host offers no saved runs, so no pool can be built here"))?;
    let forecasts: Vec<Vec<ForecastRow>> =
        run_ids.iter().map(|id| saved.forecasts(id).map_err(refused)).collect::<Result<_, _>>()?;
    if let Some(at) = forecasts.iter().position(Vec::is_empty) {
        return Err(refused(format!("run {} published no forecast", run_ids[at])));
    }

    let mut symbols: Vec<&str> = forecasts.iter().flatten().map(|r| r.symbol.as_str()).collect();
    symbols.sort_unstable();
    symbols.dedup();
    let bars: Vec<Vec<Bar>> = symbols
        .iter()
        .map(|symbol| ctx.bars(venue, symbol, interval, ctx.window()))
        .collect::<Result<_, _>>()?;
    let mut times: Vec<i64> = bars.iter().flatten().map(|b| b.timestamp).collect();
    times.sort_unstable();
    times.dedup();
    if times.len() < 3 {
        return Err(refused(format!("fewer than three {interval} bars in the window: no pool")));
    }

    // The return from each bar's close to the next bar's, at the bar's time.
    let (periods, width) = (times.len(), symbols.len());
    let mut realized = vec![f64::NAN; periods * width];
    for (s, series) in bars.iter().enumerate() {
        for pair in series.windows(2) {
            if let Ok(t) = times.binary_search(&pair[0].timestamp) {
                realized[t * width + s] = pair[1].close / pair[0].close - 1.0;
            }
        }
    }
    let realized = Matrix::from_row_major(periods, width, realized).expect("the panel's shape");
    // Each run's forecast panel: the last forecast it published at a bar's CLOSE, per symbol.
    let panels: Vec<Matrix> = forecasts
        .iter()
        .map(|rows| {
            let mut values = vec![f64::NAN; periods * width];
            for row in rows {
                let s = symbols.binary_search(&row.symbol.as_str()).expect("a symbol of the union");
                if let Ok(t) = times.binary_search(&(row.timestamp - step)) {
                    values[t * width + s] = row.forecast.value();
                }
            }
            Matrix::from_row_major(periods, width, values).expect("the panel's shape")
        })
        .collect();

    let alphas = alpha_returns(&panels, &realized).expect("panels of one shape");
    let weights = mean_variance_weights(&alphas, long_only).ok_or_else(|| {
        refused(
            "the runs' alpha returns admit no mean-variance combination: two runs that are one, \
             a run that never varies, or no combination with a positive expected return",
        )
    })?;
    let hierarchical = hierarchical_weights(&alphas, max_clusters);
    let pool = combined_forecast(&panels, &weights).expect("one weight per panel");
    // OUT OF SAMPLE: weights fit on the window's first half alone, the pool they make scored on the second.
    // The whole-window weights above are the model (`model.txt`), fit on the very periods they would be
    // scored on. The last fitted return ends at the first scored decision's close, so nothing is seen early.
    let split = periods / 2;
    let rows = |m: &Matrix, from: usize, to: usize| {
        let values = m.values()[from * m.columns()..to * m.columns()].to_vec();
        Matrix::from_row_major(to - from, m.columns(), values).expect("a range of rows")
    };
    let decay = mean_variance_weights(&rows(&alphas, 0, split), long_only)
        .and_then(|fitted| {
            let scored: Vec<Matrix> = panels.iter().map(|p| rows(p, split, periods)).collect();
            let pool = combined_forecast(&scored, &fitted)?;
            information_decay(&pool, &rows(&realized, split, periods), max_lag)
        })
        .unwrap_or_else(|| vec![f64::NAN; max_lag + 1]);

    let mut out = StudyOutcome::new();
    out.metric("runs", run_ids.len() as f64)?;
    out.metric("symbols", width as f64)?;
    out.metric("periods", periods as f64)?;
    out.metric("out_of_sample_periods", (periods - split) as f64)?;
    for (i, w) in weights.iter().enumerate() {
        out.metric(&format!("weight_{i}"), *w)?;
        let h = hierarchical.as_ref().map_or(f64::NAN, |h| h[i]);
        out.metric(&format!("hierarchical_weight_{i}"), h)?;
    }
    let correlation = correlation_matrix(&alphas);
    for i in 0..run_ids.len() {
        for j in i + 1..run_ids.len() {
            let c = correlation.as_ref().map_or(f64::NAN, |c| c.get(i, j));
            out.metric(&format!("correlation_{i}_{j}"), c)?;
        }
    }
    for (lag, information) in decay.iter().enumerate() {
        out.metric(&format!("out_of_sample_information_lag_{lag}"), *information)?;
    }
    let half_life = decay_half_life(&decay).map_or(f64::NAN, |l| l as f64);
    out.metric("out_of_sample_half_life", half_life)?;

    let mut tsv = String::from("timestamp\tsymbol\tin_sample_pool_forecast\trealized_return\n");
    for (t, time) in times.iter().enumerate() {
        for (s, symbol) in symbols.iter().enumerate() {
            let (f, r) = (pool.get(t, s), realized.get(t, s));
            tsv.push_str(&format!("{}\t{symbol}\t{f}\t{r}\n", time + step));
        }
    }
    out.artifact("pool.tsv", tsv)?;
    // The weights a live strategy file combines its own members with (`type = "model"`).
    let members = names.iter().map(|n| n.to_string()).zip(weights.iter().copied()).collect();
    out.artifact("model.txt", PoolWeights::new(members).map_err(refused)?.to_text())?;
    Ok(out)
}
