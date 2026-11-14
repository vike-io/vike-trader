//! The account-wide figures of closed-trade accounting: drawdowns and run-ups, the monthly ratios, the
//! returns and buy and hold, over the ledger's own equity. Pure arithmetic on what the ledger recorded;
//! [`super::TradeClosedLedger::metrics`] is the one caller.

use serde::{Deserialize, Serialize};
use vike_model::py_sum;

/// The reference's yearly risk-free rate when a run states none, a fraction: 2%.
pub const DEFAULT_RISK_FREE_RATE: f64 = 0.02;

const DAY_MS: f64 = 86_400_000.0;
/// A year of calendar days, for the compound annual growth rate.
const YEAR_DAYS: f64 = 365.0;

/// How a yearly risk-free rate becomes a monthly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeClosedAccountMonthlyRate {
    /// `rate / 12`.
    Simple,
    /// `(1 + rate)^(1/12) - 1`.
    Compound,
}

/// What a deviation's sum of squares is divided by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeClosedAccountDivisor {
    /// The number of months, `N`.
    Population,
    /// `N - 1`.
    Sample,
}

/// How the monthly Sharpe and Sortino read the reference's formula where its documentation is silent:
/// `(mean monthly return - monthly risk-free rate) / deviation`, the Sortino's deviation over the months
/// below that rate (every month in the count, the others as zero). The documentation states neither
/// a yearly scaling, nor how the yearly rate becomes a monthly one, nor the divisor, so each is a
/// choice here and [`Self::DOCUMENTED`] is the formula as written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeClosedAccountRatio {
    /// Times `sqrt(12)`, a yearly figure.
    pub annualised: bool,
    pub monthly_rate: TradeClosedAccountMonthlyRate,
    pub divisor: TradeClosedAccountDivisor,
}

impl TradeClosedAccountRatio {
    /// The formula as the reference documents it: a monthly ratio, the rate divided by twelve, the
    /// sum of squares by the number of months (its formula names `N` and no `N - 1`).
    pub const DOCUMENTED: Self = Self {
        annualised: false,
        monthly_rate: TradeClosedAccountMonthlyRate::Simple,
        divisor: TradeClosedAccountDivisor::Population,
    };
}

impl Default for TradeClosedAccountRatio {
    fn default() -> Self {
        Self::DOCUMENTED
    }
}

/// What the account-wide figures are measured against.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TradeClosedAccountTerms {
    /// The run's starting cash: the base of the percent figures and of the first month's return.
    pub initial_capital: f64,
    /// A yearly fraction, `0.02` for 2%.
    pub risk_free_rate: f64,
    pub reading: TradeClosedAccountRatio,
    /// The margin a long must carry, a fraction of its value (`1.0` all of it); `0.0` uses none.
    pub margin_long: f64,
    /// [`Self::margin_long`] for a short.
    pub margin_short: f64,
}

impl TradeClosedAccountTerms {
    /// `initial_capital` at the default rate and the documented reading.
    pub fn new(initial_capital: f64) -> Self {
        Self {
            initial_capital,
            risk_free_rate: DEFAULT_RISK_FREE_RATE,
            reading: TradeClosedAccountRatio::DOCUMENTED,
            margin_long: 0.0,
            margin_short: 0.0,
        }
    }
}

/// The account-wide figures. Money is the account's; a `_pct` field is a fraction; a `_days` field is
/// calendar days.
///
/// Two equities, as the reference keeps them. The EQUITY at a bar is the initial capital, the closed
/// rows' net, and the open lots' P&L less the entry commission they carry, at the bar's close or at
/// its extreme. The CLOSED-TRADE EQUITY is the initial capital and the closed rows' net after each
/// closed row, from the first one: the reference's close-to-close series (a closing price is a
/// trade's close, not a bar's), measured on its own reports (`closed_trades.rs`'s "What the reference
/// was measured to do").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeClosedAccountStatistics {
    /// The closed rows' net plus the open lots' P&L at the last price, each less its entry commission.
    pub total_pnl: f64,
    /// `total_pnl` over the initial capital.
    pub total_return: f64,
    /// The closed rows' net over the initial capital.
    pub net_profit_percent: f64,
    /// Max drawdown (intrabar): the deepest fall from the highest closed-trade equity before a bar
    /// (the initial capital at first) to the equity at that bar's adverse extreme.
    pub drawdown: f64,
    /// [`Self::drawdown`] over the peak it fell from.
    pub drawdown_percent: f64,
    /// [`Self::drawdown`] over the initial capital.
    pub drawdown_init_percent: f64,
    /// Max run-up (intrabar), the mirror: the highest rise from the lowest closed-trade equity before
    /// a bar to the equity at that bar's favourable extreme.
    pub runup: f64,
    /// [`Self::runup`] over the trough it rose from.
    pub runup_percent: f64,
    /// [`Self::runup`] over the initial capital.
    pub runup_init_percent: f64,
    /// Max drawdown (close-to-close): the deepest fall of the closed-trade equity below its running peak.
    pub drawdown_close: f64,
    /// [`Self::drawdown_close`] over that peak.
    pub drawdown_close_percent: f64,
    /// Max run-up (close-to-close): the highest of its run-ups ([`phases`]), from a drawdown's lowest
    /// point (or the first row) to the next peak, or to the last row.
    pub runup_close: f64,
    /// [`Self::runup_close`] over that trough.
    pub runup_close_percent: f64,
    /// The mean depth of the closed-trade equity's drawdowns, each over its peak (see [`phases`]).
    pub average_drawdown_percent: f64,
    /// Their mean length, from the peak to the lowest point.
    pub average_drawdown_days: f64,
    /// The mean height of its run-ups, each over its trough.
    pub average_runup_percent: f64,
    /// Their mean length, from the trough to the peak.
    pub average_runup_days: f64,
    /// The run-up in progress at the last closed row, over its trough.
    pub runup_now_percent: f64,
    /// Monthly Sharpe ratio of the equity at bar closes, under the ledger's [`TradeClosedAccountRatio`].
    pub sharpe: f64,
    /// Monthly Sortino ratio, likewise.
    pub sortino: f64,
    /// Compound annual growth of [`Self::net_profit_percent`] over the calendar days from the first bar
    /// to the last, in years of 365 days.
    pub cagr: f64,
    /// The initial capital put into the first entry's symbol at that entry's price and held to the
    /// last price, no commission.
    pub buy_hold: f64,
    /// [`Self::buy_hold`] over the initial capital.
    pub buy_hold_percent: f64,
    /// Buy and hold where the equity's line ends: at the last closed row's exit price when nothing is
    /// open at the end (the closed-trade equity's last point), else [`Self::buy_hold`].
    pub buy_hold_chart: f64,
    /// `total_return - buy_hold_percent`, a fraction (percentage points).
    pub outperformance: f64,
    /// The engine's margin calls: the cuts a short margin forced (`vike_simulator::ShortfallCover`).
    pub margin_calls: usize,
    /// The margin the open lots used at each bar's close (their value at the close times
    /// [`TradeClosedAccountTerms::margin_long`] or `margin_short`), averaged over the bars from the first
    /// entry's to the last.
    pub average_margin: f64,
    /// The closed rows' net over [`Self::average_margin`]: the reference's margin efficiency.
    pub margin_eff: f64,
}

impl TradeClosedAccountStatistics {
    /// The value of one field by its name (a catalog id without its `closed_` prefix).
    pub(super) fn value_of(&self, field: &str) -> Option<f64> {
        Some(match field {
            "total_pnl" => self.total_pnl,
            "total_return" => self.total_return,
            "net_profit_percent" => self.net_profit_percent,
            "drawdown" => self.drawdown,
            "drawdown_percent" => self.drawdown_percent,
            "drawdown_init_percent" => self.drawdown_init_percent,
            "runup" => self.runup,
            "runup_percent" => self.runup_percent,
            "runup_init_percent" => self.runup_init_percent,
            "drawdown_close" => self.drawdown_close,
            "drawdown_close_percent" => self.drawdown_close_percent,
            "runup_close" => self.runup_close,
            "runup_close_percent" => self.runup_close_percent,
            "average_drawdown_percent" => self.average_drawdown_percent,
            "average_drawdown_days" => self.average_drawdown_days,
            "average_runup_percent" => self.average_runup_percent,
            "average_runup_days" => self.average_runup_days,
            "runup_now_percent" => self.runup_now_percent,
            "sharpe" => self.sharpe,
            "sortino" => self.sortino,
            "cagr" => self.cagr,
            "buy_hold" => self.buy_hold,
            "buy_hold_percent" => self.buy_hold_percent,
            "buy_hold_chart" => self.buy_hold_chart,
            "outperformance" => self.outperformance,
            "margin_calls" => self.margin_calls as f64,
            "average_margin" => self.average_margin,
            "margin_eff" => self.margin_eff,
            _ => return None,
        })
    }
}

/// `part / base`, zero when the base is not positive.
pub(super) fn share(part: f64, base: f64) -> f64 {
    if base > 0.0 { part / base } else { 0.0 }
}

fn mean(xs: impl Iterator<Item = f64>) -> f64 {
    let xs: Vec<f64> = xs.collect();
    if xs.is_empty() { 0.0 } else { py_sum(xs.iter().copied()) / xs.len() as f64 }
}

/// The deepest fall of `values` below their running peak, and that peak; the first value starts it.
pub(super) fn deepest_fall(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let (mut peak, mut fall, mut from) = (f64::NEG_INFINITY, 0.0, 0.0);
    for v in values {
        peak = peak.max(v);
        if peak - v > fall {
            (fall, from) = (peak - v, peak);
        }
    }
    (fall, from)
}

/// One closing fill on the closed-trade equity: its time, the lowest value its rows passed through,
/// and its last.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Close {
    pub timestamp: i64,
    pub low: f64,
    pub end: f64,
}

/// One drawdown or run-up: its size over its peak (a drawdown) or its trough (a run-up), its days,
/// its size in money.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Phase {
    pub percent: f64,
    pub days: f64,
    pub money: f64,
}

/// The growth and decline of the closed-trade equity, as the reference measures it.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Phases {
    /// Each drawdown, its days from the peak to the lowest point.
    pub declines: Vec<Phase>,
    /// Each run-up, its days from the trough to the peak.
    pub rises: Vec<Phase>,
    /// The run-up in progress at the end, over its trough.
    pub now_rise: f64,
}

fn decline(peak: (i64, f64), low: (i64, f64)) -> Phase {
    Phase {
        percent: share(peak.1 - low.1, peak.1),
        days: days(peak.0, low.0),
        money: peak.1 - low.1,
    }
}

fn rise(trough: (i64, f64), top: (i64, f64)) -> Phase {
    Phase {
        percent: share(top.1 - trough.1, trough.1),
        days: days(trough.0, top.0),
        money: top.1 - trough.1,
    }
}

/// The phases of the closed-trade equity, from `start` (the first closed row) through each closing
/// fill. A drawdown opens on a fill that ends below the running peak and closes on the first fill
/// that ends back at it; its lowest point counts every row on the way, so a fill's rows that dip and
/// recover inside it open none. A run-up runs from a drawdown's lowest point (or the start) to the
/// peak the next drawdown falls from; at the end, the one in progress runs to the last value.
pub(super) fn phases(start: (i64, f64), closes: &[Close]) -> Phases {
    let mut out = Phases::default();
    let (mut peak, mut low, mut rise_from) = (start, start, start);
    let mut in_drawdown = false;
    for c in closes {
        if in_drawdown {
            if c.low < low.1 {
                low = (c.timestamp, c.low);
            }
            if c.end >= peak.1 {
                out.declines.push(decline(peak, low));
                (in_drawdown, rise_from, peak) = (false, low, (c.timestamp, c.end));
            }
        } else if c.end >= peak.1 {
            peak = (c.timestamp, c.end);
        } else {
            if peak.1 > rise_from.1 {
                out.rises.push(rise(rise_from, peak));
            }
            (in_drawdown, low) = (true, (c.timestamp, c.low.min(c.end)));
        }
    }
    let end = closes.last().map_or(start, |c| (c.timestamp, c.end));
    let from = if in_drawdown {
        out.declines.push(decline(peak, low));
        low
    } else {
        rise_from
    };
    let top = if in_drawdown { end } else { peak };
    if top.1 > from.1 {
        let now = rise(from, top);
        out.now_rise = now.percent;
        out.rises.push(now);
    }
    out
}

fn days(from_ms: i64, to_ms: i64) -> f64 {
    (to_ms - from_ms) as f64 / DAY_MS
}

/// The phases' means: depth and days of the drawdowns, height and days of the run-ups.
pub(super) fn phase_means(p: &Phases) -> [f64; 4] {
    [
        mean(p.declines.iter().map(|d| d.percent)),
        mean(p.declines.iter().map(|d| d.days)),
        mean(p.rises.iter().map(|r| r.percent)),
        mean(p.rises.iter().map(|r| r.days)),
    ]
}

/// The run-up of the largest percent, none when there is no run-up.
pub(super) fn highest_rise(p: &Phases) -> Option<Phase> {
    p.rises.iter().copied().reduce(|a, b| if b.percent > a.percent { b } else { a })
}

/// Each calendar month's return (UTC) of `points` (absolute equity at bar closes): the month's last
/// close over the one before it, the first month's over `start`. A month with no close is no month.
pub(super) fn monthly_returns(start: f64, points: &[(i64, f64)]) -> Vec<f64> {
    let mut ends: Vec<f64> = Vec::new();
    let mut month = String::new();
    for &(t, v) in points {
        let key = crate::periods::period_key(t, "monthly");
        if key == month {
            *ends.last_mut().expect("a month opened") = v;
        } else {
            month = key;
            ends.push(v);
        }
    }
    let mut before = start;
    ends.iter()
        .map(|&v| {
            let r = if before > 0.0 && v.is_finite() { v / before - 1.0 } else { 0.0 };
            before = v;
            r
        })
        .collect()
}

/// The monthly Sharpe and Sortino ratios of `returns` at a yearly risk-free `rate`, read by `reading`;
/// `0.0` for fewer months than the divisor needs or no deviation.
pub(super) fn sharpe_sortino(
    returns: &[f64],
    rate: f64,
    reading: TradeClosedAccountRatio,
) -> (f64, f64) {
    let n = returns.len();
    let divisor = match reading.divisor {
        TradeClosedAccountDivisor::Population => n as f64,
        TradeClosedAccountDivisor::Sample => n as f64 - 1.0,
    };
    if n == 0 || divisor <= 0.0 {
        return (0.0, 0.0);
    }
    let rf = match reading.monthly_rate {
        TradeClosedAccountMonthlyRate::Simple => rate / 12.0,
        TradeClosedAccountMonthlyRate::Compound => libm::pow(1.0 + rate, 1.0 / 12.0) - 1.0,
    };
    let m = mean(returns.iter().copied());
    let sd = (py_sum(returns.iter().map(|r| (r - m) * (r - m))) / divisor).sqrt();
    let short = |r: &f64| (r - rf).min(0.0);
    let down = (py_sum(returns.iter().map(|r| short(r) * short(r))) / divisor).sqrt();
    let scale = if reading.annualised { 12f64.sqrt() } else { 1.0 };
    let ratio = |dev: f64| if dev > 0.0 { (m - rf) / dev * scale } else { 0.0 };
    (ratio(sd), ratio(down))
}

/// Compound annual growth of a `total_return` over `span_days` calendar days; `-1` when the account
/// is gone, `0` over no time.
pub(super) fn cagr(total_return: f64, span_days: f64) -> f64 {
    if span_days <= 0.0 {
        return 0.0;
    }
    let growth = 1.0 + total_return;
    if growth <= 0.0 { -1.0 } else { libm::pow(growth, YEAR_DAYS / span_days) - 1.0 }
}

#[path = "account_tests.rs"]
#[cfg(test)]
mod account_tests;
