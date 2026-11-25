//! `impact_fit` — fits the backtest engine's market-impact slippage to the account's OWN fills.
//!
//! The engine prices a taker fill's extra adverse move with the Almgren et al. (2005) model, at the
//! published coefficients unless the strategy states its own: the `model = "market_impact"` slippage
//! table a strategy file states (`vike_model::Strategy::stated_slippage`; a run's slippage is a
//! strategy param since the owner's ruling of 2026-11-18, and no param names this model). This study
//! reads what the account really paid and fits that table's three numbers, `base`, `gamma` and `eta`
//! (`vike_fills::market_impact::calibrate_impact`), so the next backtest charges what the market
//! charged you.
//!
//! The recipe. Every key but the last two is REQUIRED, because each must equal the run the
//! coefficients will price (a default here would be a second copy of the engine's):
//!
//! ```toml
//! venue = "binance"
//! symbol = "BTCUSDT"
//! interval = "1h"           # the bars that run is on
//! window = 21               # the table's window: bars in the sigma and volume window
//! exec_time = 1.0           # the table's exec_time
//! horizon_ms = 60000        # optional: how long after a fill the mid shows the permanent move
//! arrival_within_ms = 5000  # optional: how old the last quote before a fill may be
//! ```
//!
//! What it reads, all through the context and so point in time:
//! - `execution_fills`: the journal's fills in the window, read as fills by
//!   `vike_data::ExecFillRow::fill_events`; only TAKER fills are fitted (the engine charges no
//!   impact to a bar-lane maker), and a fill whose horizon ends past the window is not;
//! - `quotes` around each fill: the mid just before it (the arrival price) and the last mid within
//!   the horizon after it;
//! - `bars`: the market context the engine would have measured at the fill
//!   (`vike_fills::market_impact::window_stats_before`, the engine's own call).
//!
//! What it returns: counts and the three numbers as metrics, `impact_fills.tsv` (one row per fitted
//! fill) and, when the table's own check would accept the fit (`base >= 0`, `gamma > 0`, `eta > 0`),
//! `stated_slippage.toml`, the table a strategy file states.

use vike_data::{ExecFillRow, TimestampRange};
use vike_fills::market_impact::{ImpactFill, calibrate_impact, window_stats_before};
use vike_marketdata::TickQuote;
use vike_model::events::LiquiditySide;
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

fn missing(key: &str, what: &str) -> StudyError {
    StudyError::Study(format!("the recipe needs `{key}`: {what}"))
}

fn text<'a>(params: &'a toml::Value, key: &str, what: &str) -> Result<&'a str, StudyError> {
    params.get(key).and_then(|v| v.as_str()).ok_or_else(|| missing(key, what))
}

/// A TOML number, integer or float.
fn number(params: &toml::Value, key: &str) -> Option<f64> {
    let v = params.get(key)?;
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// The last valid mid among `quotes` (ascending) with a time `<= until`, and its time.
fn last_mid(quotes: &[TickQuote], until: i64) -> Option<(i64, f64)> {
    quotes
        .iter()
        .rev()
        .filter(|q| q.timestamp <= until)
        .find(|q| q.bid > 0.0 && q.ask > 0.0 && q.bid.is_finite() && q.ask.is_finite())
        .map(|q| (q.timestamp, q.mid()))
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = text(params, "venue", "the venue the account traded on")?;
    let symbol = text(params, "symbol", "the instrument to fit")?;
    let interval = text(params, "interval", "the bars the profile runs on, such as \"1h\"")?;
    let interval_ms = vike_marketdata::interval::interval_ms(interval).ok_or_else(|| {
        StudyError::Study(format!("`interval` {interval:?} is not a bar interval"))
    })?;
    let window = params
        .get("window")
        .and_then(|v| v.as_integer())
        .filter(|w| *w >= 3)
        .ok_or_else(|| missing("window", "the slippage table's window, 3 or more"))?;
    let exec_time = number(params, "exec_time")
        .filter(|t| t.is_finite() && *t > 0.0)
        .ok_or_else(|| missing("exec_time", "the slippage table's exec_time, above 0"))?;
    let horizon_ms = params.get("horizon_ms").and_then(|v| v.as_integer()).unwrap_or(60_000).max(1);
    let arrival_within_ms =
        params.get("arrival_within_ms").and_then(|v| v.as_integer()).unwrap_or(5_000).max(0);

    let range = ctx.window();
    let fills = ExecFillRow::fill_events(&ctx.execution_fills(venue, symbol, range)?);
    let takers: Vec<_> =
        fills.iter().filter(|f| matches!(f.liquidity_side, LiquiditySide::Taker)).collect();

    // One bar read for every fill, reaching back far enough for the first fill's window.
    let bars = match (takers.first(), takers.last()) {
        (Some(first), Some(last)) => {
            let back = (window + 1).saturating_mul(interval_ms);
            let from = first.timestamp.saturating_sub(back);
            ctx.bars(venue, symbol, interval, TimestampRange::of(from, last.timestamp))?
        }
        _ => Vec::new(),
    };

    let (mut without_mid, mut without_context) = (0usize, 0usize);
    let mut used: Vec<ImpactFill> = Vec::new();
    let mut tsv = String::from(
        "ts\tside\tquantity\tprice\tarrival_mid\tafter_mid\tsigma\taverage_volume\tcost\tpermanent\n",
    );
    for f in &takers {
        let ts = f.timestamp;
        let after_until = ts.saturating_add(horizon_ms);
        // The permanent move is read inside the window, never past it.
        if range.end.is_some_and(|end| after_until > end) {
            without_mid += 1;
            continue;
        }
        let around = TimestampRange::of(ts.saturating_sub(arrival_within_ms), after_until);
        let quotes = ctx.quotes(venue, symbol, around)?;
        let (Some((_, arrival_mid)), Some((after_ts, after_mid))) =
            (last_mid(&quotes, ts - 1), last_mid(&quotes, after_until))
        else {
            without_mid += 1;
            continue;
        };
        if after_ts <= ts {
            without_mid += 1; // no quote after the fill: nothing says what stayed of the move
            continue;
        }
        let Some(context) = window_stats_before(&bars, ts, window as usize) else {
            without_context += 1;
            continue;
        };
        let fill = ImpactFill {
            side: f.side,
            quantity: f.last_qty.abs(),
            price: f.last_price,
            arrival_mid,
            after_mid,
            context,
        };
        let s = f64::from(f.side);
        let cost = s * (fill.price - arrival_mid) / arrival_mid;
        let permanent = s * (after_mid - arrival_mid) / arrival_mid;
        tsv.push_str(&format!(
            "{ts}\t{}\t{}\t{}\t{arrival_mid}\t{after_mid}\t{}\t{}\t{cost}\t{permanent}\n",
            f.side, fill.quantity, fill.price, context.sigma, context.average_volume
        ));
        used.push(fill);
    }

    let fit = calibrate_impact(&used, exec_time);
    let (base, gamma, eta) =
        fit.map_or((f64::NAN, f64::NAN, f64::NAN), |c| (c.base, c.gamma, c.eta));
    let profile_ready = base >= 0.0 && gamma > 0.0 && eta > 0.0;

    let mut out = StudyOutcome::new();
    out.metric("gamma", gamma)?;
    out.metric("eta", eta)?;
    out.metric("base", base)?;
    out.metric("profile_ready", if profile_ready { 1.0 } else { 0.0 })?;
    out.metric("fills", fills.len() as f64)?;
    out.metric("fills_used", fit.map_or(0, |c| c.fills) as f64)?;
    out.metric("fills_not_taker", (fills.len() - takers.len()) as f64)?;
    out.metric("fills_without_mid", without_mid as f64)?;
    out.metric("fills_without_context", without_context as f64)?;
    out.artifact("impact_fills.tsv", tsv)?;
    if profile_ready {
        // `{:?}` writes every f64 as a TOML float (`1.0`, `3e-5`), which the profile's keys need.
        out.artifact(
            "stated_slippage.toml",
            format!(
                "model = \"market_impact\"\nbase = {base:?}\ngamma = {gamma:?}\n\
                 eta = {eta:?}\nexec_time = {exec_time:?}\nwindow = {window}\n"
            ),
        )?;
    }
    Ok(out)
}
