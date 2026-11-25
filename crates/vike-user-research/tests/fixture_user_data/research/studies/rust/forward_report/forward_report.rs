//! `forward_report` — a forward test held against its backtest (book chapter 22): the same strategy over
//! the same window, once as a backtest and once forward on a paper or demo account. What the market did
//! differently shows as trades one side took and the other did not, as slippage, as a gap in return and
//! Sharpe, and as fills drifting through the test. The second question is how long the test must run:
//! how many days, and how many trades, before a Sharpe can be told apart from zero, and the forward's
//! from the backtest's, at the confidence and power the recipe states.
//!
//! The recipe:
//!
//! ```toml
//! forward_run = "1767225600-4242-0"  # optional: the forward run the trade daemon recorded (below)
//! backtest_run = "1767225600-ab12cd34ef56ab78-0"  # the saved backtest of the strategy over this window
//! venue = "binance"
//! symbol = "BTCUSDT"
//! capital = 10000.0        # the equity both records start the window with
//! account = "paper-1"      # optional: only the fills booked on this account label
//! match_within_ms = 60000  # optional: how far apart two entries may be and still be one signal
//! confidence = 0.95        # optional
//! power = 0.8              # optional
//! periods_per_year = 365   # optional: the days a year the market trades (252 for a session market)
//! ```
//!
//! Or name the forward run the trade daemon recorded (`flags.record_forward_runs`, on by default): with
//! `forward_run = "<its run id>"` the window is the forward run's own span (one still open ends where `--to`
//! does), the backtest is the run it names unless `backtest_run` names one, and `venue`, `symbol` and `account`
//! default to its mount's (the unlabelled account's fills count with or without a label).
//!
//! ⚠ **Keep the reference backtest: MARK it** (`vike-cli backtest tag <run> --as NAME`). A box that prunes its
//! saved runs (`config.saved_runs_per_strategy`) removes old backtests, and the recipe's `backtest_run` is the
//! only record of which one a forward test is held against; a marked run is never pruned.
//!
//! Three more keys, each off unless the recipe names it (without them the report is what it always was):
//!
//! ```toml
//! mount_id = "binance__BTCUSDT__1m"  # only this mount's fills (a forward run names its own); see below
//! compare_forecasts = true           # the live forecasts against the backtest run's `forecasts.json`
//! [drift]                            # the sequential drift alarms, every key optional (the defaults shown)
//! slippage_tolerance_bps = 1.0       # per matched trade
//! slippage_threshold_bps = 25.0
//! return_tolerance = 0.001           # per day, a fraction of equity
//! return_threshold = 0.02
//! ```
//!
//! - **The mount.** Every fill row names the mount whose order it filled (the journal materializer's
//!   `mount_id`), so two strategies on one account and symbol are told apart: a row of another mount is left
//!   out, and a row naming none (a hand-placed order, an order opened before the materializer's checkpoint)
//!   is kept, as every row was before, and counted: `fills_of_other_mounts`, `fills_without_mount`.
//! - **The forecasts** (`compare_forecasts`): the mount's live `kind=exec_forecast` rows in the window, read
//!   point in time at its end (`StudyContext::forecasts_as_of`), against the backtest run's forecasts:
//!   `live_forecasts`, `backtest_forecasts`, `paired_forecasts` (made at one same decision time),
//!   `forecast_difference` and `forecast_absolute_difference` (live minus backtest over the pairs), and the
//!   two distributions' `forecast_population_stability_index` (ten bins of the backtest's) and
//!   `forecast_kolmogorov_smirnov_statistic` / `_p_value`.
//! - **The drift alarms** (`[drift]`): F1's Page–Hinkley test and CUSUM (`vike_math::change_detection`) over two
//!   series where a rise is the forward doing WORSE, the slippage of each matched trade (in exit order, at its
//!   forward EXIT, where its exit slippage became known) and the day's return shortfall, the backtest's daily
//!   return minus the forward's (at the day's close).
//!   Page–Hinkley alarms when a series ROSE within the test; CUSUM when it sits above the backtest's own
//!   level, zero on both. One tolerance and one threshold a series serve as both the Page–Hinkley tolerance
//!   and threshold and the CUSUM allowance and threshold. Metrics, per series (`slippage`,
//!   `return_shortfall`) and per test (`page_hinkley`, `cusum`): `<series>_<test>_alarms` and
//!   `<series>_<test>_first_alarm_ms` (NaN when it never alarmed).
//!
//! The window (`--from`, `--to`) is the forward test's span, and the backtest run must cover it. A single
//! backtest keeps its trade ledger, whether the daemon saved it (`vike-cli backtest run`) or the engine did
//! (`--local`); the host refuses a run that kept none.
//!
//! What it reads, all through the context and so point in time:
//! - the run's closed trades (`StudyContext::runs`, `StudyRuns::trades`), of this symbol (a
//!   single-symbol run names none);
//! - the account's own fills up to the window's end (`execution_fills`), folded into closed trades by
//!   `vike_analytics::reconstruct_trades`, the live tearsheet's fold: the fills before the window are
//!   read so a position opened before it is not taken for one opened inside it.
//! - the symbol's kept instrument row (`StudyContext::kept_instrument`) and what the HOST values one unit of it at
//!   (`StudyContext::kept_contract_value`, the catalog's answer): every return is taken on that
//!   `vike_model::ContractValue`, and both records are folded linearly at one unit, so a symbol the report cannot
//!   value that way is REFUSED in words rather than reported wrong: no kept row, a row that is not linear (an
//!   inverse perpetual or future, a quanto), and a unit only the venue states (an ibkr future's multiplier, an
//!   fxcm CFD's, an ig epic's contract size). One unit is never read as 1.0 by default.
//!
//! On each side a position closed in pieces is one round trip (`vike_analytics::forward_test::round_trips`),
//! and a round trip counts when it opened and closed inside the window. Returns are REALIZED, net of
//! fees, booked on the day a round trip closes, so both records are measured the one same way.
//!
//! Metrics: the trade counts and `match_rate`; the mean `entry_delay_ms`; slippage in basis points
//! (positive = the forward paid more) at entry, at exit and in all, its Newey–West t statistic and
//! p-value, and its mean over each half of the pairs (the drift); each record's return and annualized
//! Sharpe and their differences, read off ONE `vike_analytics::results_comparison::TradingResultsComparison` of the
//! two records (the backtest its `a`; a record whose equity reached zero is INVALID there, so its return and Sharpe
//! and both differences read NaN); the probability the forward's true Sharpe is above zero and above the
//! backtest's; the days and trades needed to tell the backtest's Sharpe from zero and the forward's from
//! the backtest's. Artifacts: `matched_trades.tsv`, `power.tsv` (the days a given annual Sharpe needs),
//! and three charts, `chart_equity.tsv`, `chart_slippage.tsv` and `chart_confidence.tsv`.

use std::collections::BTreeMap;

use vike_analytics::forward_test::{match_trades, realized_daily_equity, round_trips};
use vike_analytics::metrics;
use vike_analytics::overfit::{
    OverfitSharpeMoments, minimum_track_record_length, probabilistic_sharpe_ratio, sharpe_moments,
};
use vike_analytics::results::TradingResults;
use vike_analytics::results_comparison::TradingResultsComparison;
use vike_analytics::{TradingRecord, equity_curve_from_trades, reconstruct_trades};
use vike_data::{ExecFillRow, TimestampRange};
use vike_math::change_detection::{
    cusum_alarms, kolmogorov_smirnov_two_sample, page_hinkley_alarms, population_stability_index,
};
use vike_math::hypothesis_testing::two_sided_normal_p_value;
use vike_math::statistics::mean;
use vike_math::time_series::newey_west_t_statistic;
use vike_model::{ContractValue, NotionalKind, Trade};
use vike_user_research::{StudyContext, StudyError, StudyForwardRun, StudyOutcome};

#[cfg(test)]
pub mod fixture;

/// The annual Sharpes `power.tsv` answers for.
const ANNUAL_SHARPES: [f64; 6] = [0.5, 1.0, 1.5, 2.0, 2.5, 3.0];
const CHART_HEADER: &str = "time\tvalue\tseries\tkind\n";
/// The population stability index's bins: the customary ten (deciles of the backtest's forecasts).
const FORECAST_BINS: usize = 10;

fn refused(why: impl Into<String>) -> StudyError {
    StudyError::Study(why.into())
}

fn text<'a>(params: &'a toml::Value, key: &str, what: &str) -> Result<&'a str, StudyError> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| refused(format!("the recipe needs `{key}`: {what}")))
}

/// A TOML number, integer or float.
fn number(params: &toml::Value, key: &str) -> Option<f64> {
    let v = params.get(key)?;
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// A probability key, `default` when absent; refused outside (0, 1).
fn probability(params: &toml::Value, key: &str, default: f64) -> Result<f64, StudyError> {
    let p = number(params, key).unwrap_or(default);
    if p > 0.0 && p < 1.0 {
        Ok(p)
    } else {
        Err(refused(format!("`{key}` is a probability between 0 and 1, not {p}")))
    }
}

/// The round trips of `trades` that opened and closed inside `[start, end]`, in closing order.
fn inside(trades: &[Trade], start: i64, end: i64) -> Vec<Trade> {
    let mut kept: Vec<Trade> = round_trips(trades)
        .into_iter()
        .filter(|t| t.entry_timestamp >= start && t.exit_timestamp <= end)
        .collect();
    kept.sort_by_key(|t| t.exit_timestamp);
    kept
}

/// One record's closed trades and realized daily equity (`realized_daily_equity`, sampled at `day_times`) as
/// `TradingResults`, so its totals are read as every table reads a record: INVALID where its equity reached zero.
fn daily_results(
    trades: &[Trade],
    days: &[f64],
    day_times: &[i64],
    periods_per_year: f64,
) -> TradingResults {
    let record = TradingRecord {
        trades: trades.to_vec(),
        equity_curve: days.to_vec(),
        final_equity: days.last().copied().unwrap_or(f64::NAN),
        n_trades: trades.len(),
        equity_timestamp: day_times.to_vec(),
        ..TradingRecord::default()
    };
    TradingResults::from_record(None, &record, periods_per_year)
}

/// The `vike_model::ContractValue` every money figure of the report is taken on
/// (`vike_analytics::forward_test::trade_net_return`, `match_trades`): the HOST's value of the symbol's KEPT row
/// (`StudyContext::kept_contract_value`, the catalog's `vike_catalog::kept_contract_value`), refused in words
/// wherever the report cannot book it. Both records are folded linearly at one unit
/// (`vike_analytics::reconstruct_trades` at its `MULTIPLIER` of 1.0, a return on `entry_price · size`), so 1,000
/// USD of an inverse perpetual from 60,000 to 66,000 would read +6,000,000 where the account booked +100, and an
/// ES future (50 dollars a point) a fiftieth of its money, and every return, Sharpe and drift alarm with them.
/// Refused: no kept row (nothing says what one unit is), a row that is not linear (an inverse perpetual or
/// future, a quanto `ContractValue::of` cannot value), a unit only the venue states (an ibkr future's multiplier,
/// an fxcm CFD's `ContractMultiplier`, an ig epic's `contractSize`: the catalog answers none), and any value but
/// one linear unit.
fn refuse_a_contract_it_cannot_value(
    ctx: &StudyContext,
    venue: &str,
    symbol: &str,
) -> Result<ContractValue, StudyError> {
    let Some(row) = ctx.kept_instrument(venue, symbol)? else {
        return Err(refused(format!(
            "{venue} {symbol} has no kept instrument row on this run's host, so nothing says what one unit of \
             it is worth: refresh {venue}'s instrument list (`vike-cli data catalog refresh`) rather than read \
             it as 1.0"
        )));
    };
    let kind = ContractValue::of(&row.contract, 1.0).map(|v| v.kind);
    if kind != Some(NotionalKind::Linear) {
        return Err(refused(format!(
            "{venue} {symbol} is not a linear contract (its kept row reads {kind:?}): this report folds \
             and returns both records linearly, so its returns, Sharpe and drift alarms would be wrong"
        )));
    }
    match ctx.kept_contract_value(venue, symbol)? {
        Some(value) if value == ContractValue::default() => Ok(value),
        Some(value) => Err(refused(format!(
            "{venue} {symbol} is worth {value:?} a unit by its kept row: this report folds both records at \
             one linear unit, so its returns, Sharpe and drift alarms would be off by that factor"
        ))),
        None => Err(refused(format!(
            "{venue} {symbol}: the catalog states no unit for its kept row (only the venue's own row does: an \
             ibkr future's multiplier, an fxcm CFD's ContractMultiplier, an ig epic's contractSize), so this \
             report refuses it rather than read one unit as 1.0"
        ))),
    }
}

fn chart_row(chart: &mut String, time: i64, value: f64, series: &str, kind: &str) {
    if value.is_finite() {
        chart.push_str(&format!("{time}\t{value}\t{series}\t{kind}\n"));
    }
}

/// The recipe's `[drift]` table: a `(tolerance, threshold)` pair per series, each a Page–Hinkley tolerance
/// and threshold and a CUSUM allowance and threshold at once (F1's `vike_math::change_detection`).
struct DriftSettings {
    /// Per matched trade, in basis points.
    slippage: (f64, f64),
    /// Per day, as a fraction of equity.
    return_shortfall: (f64, f64),
}

/// The `[drift]` table read: defaults where a key is absent; a negative tolerance or a threshold not above
/// zero refused in words (either would alarm on every step).
fn drift_settings(table: &toml::Value) -> Result<DriftSettings, StudyError> {
    let pair = |tolerance: &str, threshold: &str, defaults: (f64, f64)| {
        let pair = (
            number(table, tolerance).unwrap_or(defaults.0),
            number(table, threshold).unwrap_or(defaults.1),
        );
        if pair.0 >= 0.0 && pair.1 > 0.0 {
            Ok(pair)
        } else {
            Err(refused(format!(
                "[drift] `{tolerance}` is at least 0 and `{threshold}` above 0, not {} and {}",
                pair.0, pair.1
            )))
        }
    };
    Ok(DriftSettings {
        slippage: pair("slippage_tolerance_bps", "slippage_threshold_bps", (1.0, 25.0))?,
        return_shortfall: pair("return_tolerance", "return_threshold", (0.001, 0.02))?,
    })
}

/// The mount's LIVE forecasts in the window, read point in time (`StudyContext::forecasts_as_of` at the
/// window's end), held against the backtest run's own (`forecasts.json`): their two distributions (the
/// population stability index over ten bins of the backtest's, and the two-sample Kolmogorov–Smirnov test),
/// and the forecasts both made at one same decision time, as the mean difference live minus backtest.
fn compare_with_backtest_forecasts(
    ctx: &StudyContext,
    runs: &dyn vike_user_research::StudyRuns,
    run_id: &str,
    (venue, symbol): (&str, &str),
    mount: Option<&str>,
    (start, end): (i64, i64),
    out: &mut StudyOutcome,
) -> Result<(), StudyError> {
    let mut live = ctx.forecasts_as_of(venue, symbol, end, Some(start))?;
    if let Some(mount) = mount {
        live.retain(|f| f.mount_id == mount);
    }
    let saved: BTreeMap<i64, f64> = runs
        .forecasts(run_id)
        .map_err(StudyError::Study)?
        .into_iter()
        .filter(|f| f.symbol.is_empty() || f.symbol == symbol)
        .filter(|f| f.timestamp >= start && f.timestamp <= end)
        .map(|f| (f.timestamp, f.forecast.value()))
        .collect();
    let (live_values, saved_values): (Vec<f64>, Vec<f64>) =
        (live.iter().map(|f| f.value).collect(), saved.values().copied().collect());
    let gaps: Vec<f64> =
        live.iter().filter_map(|f| saved.get(&f.ts).map(|b| f.value - b)).collect();
    let absolute: Vec<f64> = gaps.iter().map(|g| g.abs()).collect();
    let tested = kolmogorov_smirnov_two_sample(&saved_values, &live_values);
    out.metric("live_forecasts", live_values.len() as f64)?;
    out.metric("backtest_forecasts", saved_values.len() as f64)?;
    out.metric("paired_forecasts", gaps.len() as f64)?;
    out.metric("forecast_difference", mean(gaps.iter().copied()))?;
    out.metric("forecast_absolute_difference", mean(absolute.iter().copied()))?;
    let stability = population_stability_index(&saved_values, &live_values, FORECAST_BINS);
    out.metric("forecast_population_stability_index", stability)?;
    out.metric("forecast_kolmogorov_smirnov_statistic", tested.statistic)?;
    out.metric("forecast_kolmogorov_smirnov_p_value", tested.p_value)
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let named = |key: &str| params.get(key).and_then(|v| v.as_str());
    // The forward run the trade daemon recorded, when the recipe names one: it supplies what the recipe leaves out.
    let forward = match named("forward_run") {
        Some(id) => {
            let runs = ctx.runs().ok_or_else(|| refused("this host offers no saved runs"))?;
            Some(runs.forward(id).map_err(StudyError::Study)?)
        }
        None => None,
    };
    let run_id = match (&forward, named("backtest_run")) {
        (_, Some(id)) => id.to_string(),
        (Some(f), None) => f.reference_run.clone().ok_or_else(|| {
            refused("the forward run names no backtest to hold it against: give the recipe `backtest_run`")
        })?,
        (None, None) => text(params, "backtest_run", "the saved backtest run of the strategy")?.to_string(),
    };
    let from_forward =
        |key: &str, of: fn(&StudyForwardRun) -> &str, what: &str| match (&forward, named(key)) {
            (Some(f), None) => Ok(of(f).to_string()),
            _ => text(params, key, what).map(str::to_string),
        };
    let venue =
        from_forward("venue", |f| f.venue.as_str(), "the venue the forward test traded on")?;
    let symbol =
        from_forward("symbol", |f| f.symbol.as_str(), "the instrument the strategy traded")?;
    let (venue, symbol) = (venue.as_str(), symbol.as_str());
    let value = refuse_a_contract_it_cannot_value(ctx, venue, symbol)?;
    let capital = number(params, "capital")
        .filter(|c| c.is_finite() && *c > 0.0)
        .ok_or_else(|| refused("the recipe needs `capital`: the equity at the window's start"))?;
    // The forward run's account is `venue/LABEL`; the unlabelled account's fills may carry no label at all.
    let forward_label = forward.as_ref().filter(|_| named("account").is_none()).map(|f| {
        f.account.rsplit_once('/').map_or(f.account.as_str(), |(_, label)| label).to_string()
    });
    let account = named("account").or(forward_label.as_deref());
    let unlabelled_too = forward_label.as_deref() == Some("DEFAULT");
    // The mount whose fills and forecasts count: the recipe's `mount_id`, else the forward run's own.
    let mount = named("mount_id")
        .map(str::to_string)
        .or_else(|| forward.as_ref().map(|f| f.mount_id.clone()));
    let compare_forecasts =
        params.get("compare_forecasts").and_then(|v| v.as_bool()).unwrap_or(false);
    let drift = params.get("drift").map(drift_settings).transpose()?;
    let tolerance = params.get("match_within_ms").and_then(|v| v.as_integer()).unwrap_or(60_000);
    let confidence = probability(params, "confidence", 0.95)?;
    let power = probability(params, "power", 0.8)?;
    let periods_per_year = number(params, "periods_per_year").filter(|p| *p > 0.0).unwrap_or(365.0);
    let span = match &forward {
        // The forward run's own span; one still open ends where the window does.
        Some(f) => (Some(f.started_ms), f.ended_ms.or(ctx.window().end)),
        None => (ctx.window().start, ctx.window().end),
    };
    let (Some(start), Some(end)) = span else {
        return Err(refused("the window is the forward test's span: give it a start and an end"));
    };
    let runs = ctx.runs().ok_or_else(|| refused("this host offers no saved runs"))?;

    let ledger: Vec<Trade> = runs
        .trades(&run_id)
        .map_err(StudyError::Study)?
        .into_iter()
        .filter(|t| t.symbol.is_empty() || t.symbol == symbol)
        .collect();
    let backtest = inside(&ledger, start, end);
    let mut rows =
        ctx.execution_fills(venue, symbol, TimestampRange { start: None, end: Some(end) })?;
    if let Some(label) = account {
        rows.retain(|r| {
            r.account.as_deref() == Some(label) || (unlabelled_too && r.account.is_none())
        });
    }
    // With a mount named, a row of ANOTHER mount is left out. A row whose mount nobody could tell (`None`: a
    // hand-placed order, one opened before the materializer's checkpoint, or a row from before the column) is
    // kept, as every row was before mounts were named, and counted.
    let (mut fills_of_other_mounts, mut fills_without_mount) = (0_usize, 0_usize);
    if let Some(mount) = mount.as_deref() {
        rows.retain(|r| match r.mount_id.as_deref() {
            Some(m) if m != mount => {
                fills_of_other_mounts += 1;
                false
            }
            Some(_) => true,
            None => {
                fills_without_mount += 1;
                true
            }
        });
    }
    let forward = inside(&reconstruct_trades(&ExecFillRow::fill_events(&rows)), start, end);
    let matching = match_trades(&backtest, &forward, tolerance, value);
    let pairs = &matching.pairs;

    // Slippage, pair by pair in the backtest's entry order. Neighbouring trades share a market, so the
    // t statistic allows one lag of autocorrelation.
    let slippage: Vec<f64> = pairs.iter().map(|p| p.slippage_bps()).collect();
    let t_statistic = newey_west_t_statistic(&slippage, 1);
    let half = slippage.len() / 2;

    // Both records on the window's day grid, and one sample a closed trade for the count in trades.
    let (backtest_days, day_times) = realized_daily_equity(capital, &backtest, start, end);
    let (forward_days, _) = realized_daily_equity(capital, &forward, start, end);
    let per_day = (sharpe_moments(&backtest_days), sharpe_moments(&forward_days));
    let per_trade = (
        sharpe_moments(&equity_curve_from_trades(capital, &backtest).0),
        sharpe_moments(&equity_curve_from_trades(capital, &forward).0),
    );
    let above = |forward: &OverfitSharpeMoments, star: f64| {
        probabilistic_sharpe_ratio(
            forward.sr_per_obs,
            forward.n_obs,
            star,
            forward.skew,
            forward.kurt,
        )
    };
    let needed = |sharpe: &OverfitSharpeMoments, star: f64| {
        minimum_track_record_length(
            sharpe.sr_per_obs,
            star,
            sharpe.skew,
            sharpe.kurt,
            confidence,
            power,
        )
    };
    // The headline is ONE comparison of the two records, the one `vike-cli report --compare` and the Studio's
    // "Compare all" render: the backtest is `a`, the forward `b`. A row it leaves out (an INVALID record keeps
    // only its money and counts) reads NaN.
    let comparison = TradingResultsComparison::of(
        daily_results(&backtest, &backtest_days, &day_times, periods_per_year),
        // Fills, not a simulator run, and the account's tier is unknown here: it claims no source.
        TradingResults {
            source: None,
            ..daily_results(&forward, &forward_days, &day_times, periods_per_year)
        },
    );
    let rows = comparison.rows();
    let headline = |id: &str| {
        let nan = |v: Option<f64>| v.unwrap_or(f64::NAN);
        rows.iter()
            .find(|r| r.spec.id == id)
            .map_or([f64::NAN; 3], |r| [nan(r.a), nan(r.b), nan(r.difference)])
    };
    let [backtest_return, forward_return, return_difference] = headline("total_return");
    let [backtest_sharpe, forward_sharpe, sharpe_difference] = headline("sharpe");

    let mut out = StudyOutcome::new();
    out.metric("backtest_trades", backtest.len() as f64)?;
    out.metric("forward_trades", forward.len() as f64)?;
    out.metric("matched_trades", pairs.len() as f64)?;
    out.metric("backtest_only_trades", matching.backtest_only.len() as f64)?;
    out.metric("forward_only_trades", matching.forward_only.len() as f64)?;
    out.metric("match_rate", pairs.len() as f64 / backtest.len() as f64)?;
    let delays: Vec<f64> = pairs.iter().map(|p| p.entry_delay_ms as f64).collect();
    out.metric("entry_delay_ms", mean(delays.iter().copied()))?;
    let entry: Vec<f64> = pairs.iter().map(|p| p.entry_slippage_bps).collect();
    let exit: Vec<f64> = pairs.iter().map(|p| p.exit_slippage_bps).collect();
    out.metric("entry_slippage_bps", mean(entry.iter().copied()))?;
    out.metric("exit_slippage_bps", mean(exit.iter().copied()))?;
    out.metric("slippage_bps", mean(slippage.iter().copied()))?;
    out.metric("slippage_t_statistic", t_statistic)?;
    out.metric("slippage_p_value", two_sided_normal_p_value(t_statistic))?;
    out.metric("slippage_bps_first_half", mean(slippage[..half].iter().copied()))?;
    out.metric("slippage_bps_second_half", mean(slippage[half..].iter().copied()))?;
    let return_gaps: Vec<f64> =
        pairs.iter().map(|p| p.forward_return - p.backtest_return).collect();
    out.metric("matched_return_difference", mean(return_gaps.iter().copied()))?;
    out.metric("backtest_return", backtest_return)?;
    out.metric("forward_return", forward_return)?;
    out.metric("return_difference", return_difference)?;
    out.metric("backtest_sharpe", backtest_sharpe)?;
    out.metric("forward_sharpe", forward_sharpe)?;
    out.metric("sharpe_difference", sharpe_difference)?;
    out.metric("days", per_day.1.n_obs as f64)?;
    out.metric("forward_probability_sharpe_above_zero", above(&per_day.1, 0.0))?;
    out.metric(
        "forward_probability_sharpe_above_backtest",
        above(&per_day.1, per_day.0.sr_per_obs),
    )?;
    out.metric("days_to_tell_backtest_from_zero", needed(&per_day.0, 0.0))?;
    out.metric("days_to_tell_forward_from_backtest", needed(&per_day.1, per_day.0.sr_per_obs))?;
    out.metric("trades_to_tell_backtest_from_zero", needed(&per_trade.0, 0.0))?;
    out.metric(
        "trades_to_tell_forward_from_backtest",
        needed(&per_trade.1, per_trade.0.sr_per_obs),
    )?;
    if mount.is_some() {
        out.metric("fills_of_other_mounts", fills_of_other_mounts as f64)?;
        out.metric("fills_without_mount", fills_without_mount as f64)?;
    }
    if compare_forecasts {
        compare_with_backtest_forecasts(
            ctx,
            runs,
            &run_id,
            (venue, symbol),
            mount.as_deref(),
            (start, end),
            &mut out,
        )?;
    }
    if let Some(drift) = &drift {
        // The forward falling behind, so an alarm on either series is the forward doing WORSE: the slippage it
        // paid per matched trade, in EXIT order and stamped at its forward exit (its exit slippage is known only
        // there), and the day's return the backtest made over the forward's, stamped at the day's close.
        let mut known: Vec<(i64, f64)> =
            pairs.iter().map(|p| (forward[p.forward].exit_timestamp, p.slippage_bps())).collect();
        known.sort_by_key(|(exit, _)| *exit);
        let (exits, slippage_by_exit): (Vec<i64>, Vec<f64>) = known.into_iter().unzip();
        let (backtest_returns, forward_returns) =
            (metrics::returns(&backtest_days), metrics::returns(&forward_days));
        let closes = &day_times[1..];
        // shortcut: `metrics::returns` skips a step from an equity of exactly 0 (a total loss), and the two
        // records then no longer line up with the day grid; such a window gets no return alarm at all.
        let shortfall: Vec<f64> =
            if backtest_returns.len() == closes.len() && forward_returns.len() == closes.len() {
                backtest_returns.iter().zip(&forward_returns).map(|(b, f)| b - f).collect()
            } else {
                Vec::new()
            };
        for (name, series, times, (tolerance, threshold)) in [
            ("slippage", &slippage_by_exit, &exits[..], drift.slippage),
            ("return_shortfall", &shortfall, closes, drift.return_shortfall),
        ] {
            let first = |alarms: &[usize]| alarms.first().map_or(f64::NAN, |&i| times[i] as f64);
            // Page–Hinkley: the series ROSE within the forward test. CUSUM: it sits above the backtest's own
            // level, which is zero on both (no slippage against itself, no shortfall).
            let page_hinkley = page_hinkley_alarms(series, tolerance, threshold);
            let cusum = cusum_alarms(series, 0.0, tolerance, threshold);
            out.metric(&format!("{name}_page_hinkley_alarms"), page_hinkley.len() as f64)?;
            out.metric(&format!("{name}_page_hinkley_first_alarm_ms"), first(&page_hinkley))?;
            out.metric(&format!("{name}_cusum_alarms"), cusum.len() as f64)?;
            out.metric(&format!("{name}_cusum_first_alarm_ms"), first(&cusum))?;
        }
    }

    let mut table = String::from(
        "status\tbacktest_entry_ms\tforward_entry_ms\tlong\tbacktest_entry_price\tforward_entry_price\t\
         backtest_exit_price\tforward_exit_price\tentry_slippage_bps\texit_slippage_bps\t\
         backtest_return\tforward_return\n",
    );
    for p in pairs {
        let (b, f) = (&backtest[p.backtest], &forward[p.forward]);
        table.push_str(&format!(
            "matched\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            b.entry_timestamp,
            f.entry_timestamp,
            b.is_long,
            b.entry_price,
            f.entry_price,
            b.exit_price,
            f.exit_price,
            p.entry_slippage_bps,
            p.exit_slippage_bps,
            p.backtest_return,
            p.forward_return
        ));
    }
    for &i in &matching.backtest_only {
        let b = &backtest[i];
        table.push_str(&format!(
            "backtest_only\t{}\t\t{}\t{}\t\t{}\t\t\t\t{}\t\n",
            b.entry_timestamp,
            b.is_long,
            b.entry_price,
            b.exit_price,
            vike_analytics::forward_test::trade_net_return(b, value)
        ));
    }
    for &j in &matching.forward_only {
        let f = &forward[j];
        table.push_str(&format!(
            "forward_only\t\t{}\t{}\t\t{}\t\t{}\t\t\t\t{}\n",
            f.entry_timestamp,
            f.is_long,
            f.entry_price,
            f.exit_price,
            vike_analytics::forward_test::trade_net_return(f, value)
        ));
    }
    out.artifact("matched_trades.tsv", table)?;

    let mut power_table =
        String::from("annual_sharpe\tdays_normal_returns\tdays_forward_moments\n");
    for annual in ANNUAL_SHARPES {
        let daily = annual / periods_per_year.sqrt();
        let normal = minimum_track_record_length(daily, 0.0, 0.0, 3.0, confidence, power);
        let observed = minimum_track_record_length(
            daily,
            0.0,
            per_day.1.skew,
            per_day.1.kurt,
            confidence,
            power,
        );
        power_table.push_str(&format!("{annual}\t{normal}\t{observed}\n"));
    }
    out.artifact("power.tsv", power_table)?;

    let mut equity = String::from(CHART_HEADER);
    for (series, curve) in [("backtest", &backtest_days), ("forward", &forward_days)] {
        for (value, time) in curve.iter().zip(&day_times) {
            chart_row(&mut equity, *time, *value, series, "line");
        }
    }
    out.artifact("chart_equity.tsv", equity)?;

    let mut drift = String::from(CHART_HEADER);
    let mut cumulative = 0.0;
    for p in pairs {
        let time = forward[p.forward].entry_timestamp;
        cumulative += p.slippage_bps();
        chart_row(&mut drift, time, p.slippage_bps(), "trade_slippage_bps", "points");
        chart_row(&mut drift, time, cumulative, "cumulative_slippage_bps", "line");
    }
    out.artifact("chart_slippage.tsv", drift)?;

    // The evidence as the forward record grows, day by day, against the backtest's whole-window Sharpe.
    let mut evidence = String::from(CHART_HEADER);
    for day in 2..forward_days.len() {
        let so_far = sharpe_moments(&forward_days[..=day]);
        chart_row(&mut evidence, day_times[day], above(&so_far, 0.0), "above_zero", "line");
        let versus = above(&so_far, per_day.0.sr_per_obs);
        chart_row(&mut evidence, day_times[day], versus, "above_backtest", "line");
    }
    out.artifact("chart_confidence.tsv", evidence)?;
    Ok(out)
}
