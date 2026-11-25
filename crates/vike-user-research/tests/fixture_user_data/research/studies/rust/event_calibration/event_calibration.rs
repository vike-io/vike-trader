//! `event_calibration` — are an event market's prices calibrated probabilities, and what repairs
//! them?
//!
//! Each market of the recipe is a BINARY outcome token of a Polymarket market, such as its YES
//! token, whose price is the market's probability that the event happens. For each one:
//! - the OUTCOME is the market's RECORDED resolution, never its final trade price: the
//!   `polymarket_rules` archive (`vike_data::text_log::TEXT_KIND_POLYMARKET_RULES`, series
//!   `vike_data::text_log::POLYMARKET_RULES_ARCHIVE_COLLECTOR`, stored by `vike-cli data history fetch
//!   polymarket:archive:rules`) holds, per closed market, its rules row received at its close and its
//!   outcome row (`actual` = the outcome that paid 1) received at its resolution, read here as of the
//!   window's end, so a market resolved after it is unknown. The token is YES when the outcome at its
//!   place in the row's `subject` (the row's `tags`, in the same order) is the one that paid. A token
//!   with no stored outcome, or whose market paid no side whole (a 50-50 split), is UNRESOLVED and
//!   skipped, and a store that holds no texts refuses the run in its own words;
//! - the FORECAST is the price of its last trade at least `horizon_ms` before the market stopped
//!   trading: its end (`end_ms`), or its stored close when that came first (a market resolved early
//!   closes months before its scheduled end, and a price read after the close knows the outcome).
//!
//! Point in time: the markets are ordered by when their outcome was RECEIVED. The first half fits a
//! Platt and an isotonic calibration (`vike_ml_training::calibration`); every later market whose
//! forecast was read AFTER the last fitted outcome was received is EVALUATED, raw and through both
//! calibrations. A market in neither half is counted and left out.
//!
//! The recipe (`[[market]]` is the user's own list of markets):
//!
//! ```toml
//! venue = "polymarket"
//! horizon_ms = 86400000     # the forecast is read this long before each market stopped trading
//! bin_count = 10            # optional: the reliability diagram's equal-width bins
//! platt_penalty = 0.0       # optional: the Platt slope's penalty (above 0 if the fit rows separate)
//!
//! [[market]]
//! token = "<the YES token>"
//! end_ms = 1790000000000
//! ```
//!
//! What it returns: the counts, then for `raw`, `platt` and `isotonic` on the evaluated markets the
//! Brier score, the log loss and the expected calibration error, the raw forecasts' Brier
//! decomposition, and the Platt slope and intercept. Artifacts: `chart_reliability.tsv` (each
//! non-empty bin's mean forecast `x` against its observed frequency, per series, beside the
//! diagonal), `calibration_platt.txt` and `calibration_isotonic.txt` (the `vike-ml calibration v1`
//! texts a strategy applies with `AnyModel::from_text(text)?.predict_one(&[price])`) and
//! `markets.tsv` (one row per market used).

#[cfg(test)]
pub mod synthetic;

use std::collections::HashMap;

use vike_data::TimestampRange;
use vike_data::text_log::{POLYMARKET_RULES_ARCHIVE_COLLECTOR, TEXT_KIND_POLYMARKET_RULES};
use vike_ml_prediction::calibration::Calibration;
use vike_ml_training::calibration::{
    brier_decomposition, brier_score, expected_calibration_error, fit_isotonic, fit_platt,
    log_loss, reliability_bins,
};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// One resolved market with a forecast.
struct Market {
    token: String,
    end_ms: i64,
    /// When the forecast was read: `horizon_ms` before the market stopped trading.
    forecast_ms: i64,
    /// When its outcome row was received.
    resolved_ms: i64,
    forecast: f64,
    outcome: bool,
}

/// What the archive holds of one outcome token, as of the window's end.
#[derive(Default)]
struct Stored {
    /// When its market's rules row was received: the close.
    closed_ms: Option<i64>,
    /// When its market's outcome row was received, and whether this token paid (`None`: no side
    /// paid whole).
    resolved: Option<(i64, Option<bool>)>,
}

/// The archive's rows by outcome token, read as of the window's end: the first close and the first
/// outcome received of each (the rows come back received-ascending).
fn stored_by_token(ctx: &StudyContext) -> Result<HashMap<String, Stored>, StudyError> {
    let as_of = ctx.window().end.unwrap_or(i64::MAX);
    let rows = ctx.texts_as_of(
        TEXT_KIND_POLYMARKET_RULES,
        POLYMARKET_RULES_ARCHIVE_COLLECTOR,
        as_of,
        i64::MAX,
    )?;
    let mut stored: HashMap<String, Stored> = HashMap::new();
    for row in &rows {
        let tokens: Vec<&str> = row.subject.split_whitespace().collect();
        let outcomes: Vec<&str> = row.tag_list().collect();
        // A token's outcome is the one at its own place, so the two lists must line up.
        let paid = outcomes
            .iter()
            .position(|o| *o == row.actual)
            .filter(|_| tokens.len() == outcomes.len());
        for (place, token) in tokens.into_iter().enumerate() {
            let entry = stored.entry(token.to_string()).or_default();
            if row.actual.is_empty() {
                entry.closed_ms.get_or_insert(row.received_ms);
            } else {
                entry.resolved.get_or_insert((row.received_ms, paid.map(|p| p == place)));
            }
        }
    }
    Ok(stored)
}

fn missing(key: &str, what: &str) -> StudyError {
    StudyError::Study(format!("the recipe needs `{key}`: {what}"))
}

/// A TOML number, integer or float.
fn number(params: &toml::Value, key: &str) -> Option<f64> {
    let v = params.get(key)?;
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = params.get("venue").and_then(|v| v.as_str()).ok_or_else(|| {
        missing("venue", "the venue the markets trade on, such as \"polymarket\"")
    })?;
    let horizon_ms =
        params.get("horizon_ms").and_then(|v| v.as_integer()).filter(|h| *h > 0).ok_or_else(
            || missing("horizon_ms", "how long before its end a market's price is read"),
        )?;
    let bin_count = params.get("bin_count").and_then(|v| v.as_integer()).unwrap_or(10).max(1);
    let penalty = number(params, "platt_penalty").unwrap_or(0.0);
    let listed = params
        .get("market")
        .and_then(|v| v.as_array())
        .ok_or_else(|| missing("market", "one [[market]] table per market: token and end_ms"))?;

    let stored = stored_by_token(ctx)?;
    let window = ctx.window();
    let (mut unresolved, mut without_forecast) = (0usize, 0usize);
    let mut markets = Vec::new();
    for entry in listed {
        let token = entry
            .get("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| missing("market.token", "the outcome token's symbol"))?;
        let end_ms = entry
            .get("end_ms")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| missing("market.end_ms", "the market's end, epoch milliseconds"))?;
        let found = stored.get(token);
        let Some((resolved_ms, Some(outcome))) = found.and_then(|s| s.resolved) else {
            unresolved += 1; // no stored outcome by the window's end, or no side paid whole
            continue;
        };
        let closed_ms = found.and_then(|s| s.closed_ms).unwrap_or(resolved_ms);
        let forecast_ms = end_ms.min(closed_ms) - horizon_ms;
        let range = TimestampRange { start: window.start, end: Some(forecast_ms) };
        let forecast = ctx
            .trades(venue, token, range)?
            .iter()
            .filter(|t| t.timestamp <= forecast_ms)
            .max_by_key(|t| t.timestamp)
            .map(|t| t.price)
            .filter(|p| (0.0..=1.0).contains(p));
        match forecast {
            Some(forecast) => markets.push(Market {
                token: token.to_string(),
                end_ms,
                forecast_ms,
                resolved_ms,
                forecast,
                outcome,
            }),
            None => without_forecast += 1,
        }
    }
    markets.sort_by_key(|m| m.resolved_ms);

    let fitted = markets.len() / 2;
    if fitted == 0 {
        return Err(StudyError::Study(format!(
            "{} resolved market(s) with a forecast: at least two are needed, one to fit and one to \
             evaluate. {unresolved} listed market(s) have no stored outcome by the window's end: an \
             outcome is read from the polymarket_rules archive, never from a final trade price, so \
             fetch it first (`vike-cli data history fetch polymarket:archive:rules --from <day> --to \
             <day>` over the markets' end days)",
            markets.len()
        )));
    }
    let fitted_until = markets[fitted - 1].resolved_ms;
    let (fit_rows, later) = markets.split_at(fitted);
    let evaluated: Vec<&Market> = later.iter().filter(|m| m.forecast_ms > fitted_until).collect();
    if evaluated.is_empty() {
        return Err(StudyError::Study(
            "no market's forecast was read after the last fitted market's outcome was received: \
             list more markets or shorten `horizon_ms`"
                .into(),
        ));
    }
    let study = StudyError::Study;
    let fit_forecasts: Vec<f64> = fit_rows.iter().map(|m| m.forecast).collect();
    let fit_outcomes: Vec<bool> = fit_rows.iter().map(|m| m.outcome).collect();
    let platt = fit_platt(&fit_forecasts, &fit_outcomes, penalty).map_err(study)?;
    let isotonic = fit_isotonic(&fit_forecasts, &fit_outcomes).map_err(study)?;

    let raw: Vec<f64> = evaluated.iter().map(|m| m.forecast).collect();
    let outcomes: Vec<bool> = evaluated.iter().map(|m| m.outcome).collect();
    let calibrated = |c: &Calibration| -> Result<Vec<f64>, StudyError> {
        raw.iter().map(|&p| c.apply(p).map_err(|e| study(e.to_string()))).collect()
    };
    let series = [
        ("raw", raw.clone()),
        ("platt", calibrated(&platt)?),
        ("isotonic", calibrated(&isotonic)?),
    ];

    let mut out = StudyOutcome::new();
    out.metric("markets", listed.len() as f64)?;
    out.metric("unresolved", unresolved as f64)?;
    out.metric("without_forecast", without_forecast as f64)?;
    out.metric("fitted", fitted as f64)?;
    out.metric("evaluated", evaluated.len() as f64)?;
    out.metric("left_out", (later.len() - evaluated.len()) as f64)?;
    let mut chart =
        String::from("x\tvalue\tseries\tkind\n0\t0\tperfect\tline\n1\t1\tperfect\tline\n");
    for (name, forecasts) in &series {
        out.metric(&format!("brier_{name}"), brier_score(forecasts, &outcomes).map_err(study)?)?;
        out.metric(&format!("log_loss_{name}"), log_loss(forecasts, &outcomes).map_err(study)?)?;
        let bins = reliability_bins(forecasts, &outcomes, bin_count as usize).map_err(study)?;
        out.metric(&format!("calibration_error_{name}"), expected_calibration_error(&bins))?;
        for bin in bins.iter().filter(|b| b.count > 0) {
            chart.push_str(&format!(
                "{}\t{}\t{name}\tpoints\n",
                bin.mean_forecast, bin.observed_frequency
            ));
        }
    }
    let parts = brier_decomposition(&raw, &outcomes, bin_count as usize).map_err(study)?;
    out.metric("reliability_raw", parts.reliability)?;
    out.metric("resolution_raw", parts.resolution)?;
    out.metric("uncertainty", parts.uncertainty)?;
    if let Calibration::Platt { slope, intercept } = platt {
        out.metric("platt_slope", slope)?;
        out.metric("platt_intercept", intercept)?;
    }

    let mut table = String::from("token\tend_ms\tresolved_ms\tforecast\toutcome\trole\n");
    for (i, m) in markets.iter().enumerate() {
        let role = if i < fitted {
            "fitted"
        } else if m.forecast_ms > fitted_until {
            "evaluated"
        } else {
            "left_out"
        };
        table.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{role}\n",
            m.token,
            m.end_ms,
            m.resolved_ms,
            m.forecast,
            u8::from(m.outcome)
        ));
    }
    out.artifact("chart_reliability.tsv", chart)?;
    out.artifact("calibration_platt.txt", platt.to_text())?;
    out.artifact("calibration_isotonic.txt", isotonic.to_text())?;
    out.artifact("markets.tsv", table)?;
    Ok(out)
}
