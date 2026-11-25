//! `TradingResultsComparison` — the ONE comparison of two [`TradingResults`], A against B (the owner, 2026-11-17).
//! `vike-cli report --compare`, the Studio's "Compare all" and the end of a forward test each render it, so
//! they cannot disagree about which rows two results share or what their difference is.
//!
//! # What it compares
//!
//! Every [`METRICS`] row, in catalog order, which is render order. Each side's value is
//! [`TradingResults::metric_value`], the number that side's own table prints. The difference is `b − a`.
//! Each side keeps its own table's rules (`crate::results_table::TradingResultsTable::rendered_rows`):
//! - an INVALID side (its equity reached zero) keeps only its money and count values;
//! - a closed-trade row appears only when a side armed that block.
//!
//! # What it does NOT do
//!
//! It aligns nothing. Each side arrives composed over its own source and period, and choosing them is the
//! caller's: under `report --compare` an `account:` side takes the `run:` side's period, which is the forward
//! test held against its backtest. It computes no metric either: a value is read, never recomputed, and the
//! one subtraction is the only arithmetic here.

use std::fmt;

use crate::metric_catalog::{METRICS, MetricHome, MetricSpec};
use crate::results::TradingResults;
use crate::results_table::{INVALID_LABEL, NOT_RECORDED, shown_on_an_invalid_run};

/// Two results held side by side: `a` is the reference (the backtest, or the top-ranked row), `b` the one
/// measured against it.
#[derive(Debug, Clone)]
pub struct TradingResultsComparison {
    pub a: TradingResults,
    pub b: TradingResults,
}

/// One [`METRICS`] row of a [`TradingResultsComparison`].
#[derive(Debug, Clone, Copy)]
pub struct TradingResultsComparisonRow {
    pub spec: &'static MetricSpec,
    /// `a`'s value; `None` when `a` did not record it, or when `a` is INVALID and the row is no money or count.
    pub a: Option<f64>,
    /// `b`'s value, on the same terms.
    pub b: Option<f64>,
    /// `b − a`. `None` when either side is `None`, and when the subtraction has no finite answer: a side
    /// holding the house `inf` sentinel (`TradingResults::profit_factor` with nothing lost) has no
    /// meaningful difference, and `inf − inf` would print `NaN`.
    pub difference: Option<f64>,
}

impl TradingResultsComparison {
    /// Hold `a` and `b` for comparison.
    #[must_use]
    pub fn of(a: TradingResults, b: TradingResults) -> Self {
        Self { a, b }
    }

    /// Every row either side's own table shows, in [`METRICS`] order.
    #[must_use]
    pub fn rows(&self) -> Vec<TradingResultsComparisonRow> {
        // A side that measured nothing (a live window with no fills) has nothing to be held against.
        let measured = !self.a.measured_nothing() && !self.b.measured_nothing();
        METRICS
            .iter()
            .filter_map(|spec| {
                let (a, b) = (value(&self.a, spec), value(&self.b, spec));
                (shown(&self.a, spec, a) || shown(&self.b, spec, b)).then(|| {
                    let difference =
                        a.zip(b).filter(|_| measured).map(|(a, b)| b - a).filter(|d| d.is_finite());
                    TradingResultsComparisonRow { spec, a, b, difference }
                })
            })
            .collect()
    }

    /// Every row as `(id, [a, b, difference])` text: the ONE rendering of a comparison, which `Display` and
    /// `crate::html::render_comparison_html` print. A value is its unit's `MetricUnit::render`. A missing
    /// value is [`INVALID_LABEL`] on an INVALID side and [`NOT_RECORDED`] otherwise; a missing difference
    /// is empty.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<(&'static str, [String; 3])> {
        let cell = |side: &TradingResults, spec: &MetricSpec, v: Option<f64>| match v {
            Some(v) => spec.unit.render(v),
            None if side.invalid.is_some() && !shown_on_an_invalid_run(spec) => {
                INVALID_LABEL.to_string()
            }
            None => NOT_RECORDED.to_string(),
        };
        self.rows()
            .into_iter()
            .map(|r| {
                let difference = r.difference.map_or_else(String::new, |d| r.spec.unit.render(d));
                (r.spec.id, [cell(&self.a, r.spec, r.a), cell(&self.b, r.spec, r.b), difference])
            })
            .collect()
    }
}

/// The header a side is named by: its source and period in the one wording
/// ([`TradingResults::source_label`]), or the profile's `name` for a side whose source is unknown (a
/// report read with a bare parse), as that side's own table prints it.
pub(crate) fn side_label(side: &TradingResults) -> String {
    side.source_label().unwrap_or_else(|| side.name.as_deref().unwrap_or("(unnamed)").to_string())
}

/// `side`'s value of `spec` as its own table holds it: nothing but money and counts on an INVALID side.
fn value(side: &TradingResults, spec: &MetricSpec) -> Option<f64> {
    if side.invalid.is_some() && !shown_on_an_invalid_run(spec) {
        return None;
    }
    side.metric_value(spec.id)
}

/// Whether `side`'s own table shows the row (`TradingResultsTable`'s two rules).
fn shown(side: &TradingResults, spec: &MetricSpec, v: Option<f64>) -> bool {
    (spec.home != MetricHome::ClosedTrades || v.is_some())
        && (side.invalid.is_none() || shown_on_an_invalid_run(spec))
}

/// The two headers, an INVALID side's words, then one aligned row per metric: `metric  a  b  b - a`.
impl fmt::Display for TradingResultsComparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "a: {}", side_label(&self.a))?;
        writeln!(f, "b: {}", side_label(&self.b))?;
        for (side, results) in [("a", &self.a), ("b", &self.b)] {
            if let Some(why) = &results.invalid {
                writeln!(f, "{side} {INVALID_LABEL}: {why}")?;
            }
        }
        let rows = self.rendered_rows();
        let width = |header: &str, column: fn(&(&'static str, [String; 3])) -> usize| {
            rows.iter().map(column).chain([header.len()]).max().unwrap_or(0)
        };
        let id_width = width("metric", |(id, _)| id.len());
        let a_width = width("a", |(_, cells)| cells[0].len());
        let b_width = width("b", |(_, cells)| cells[1].len());
        let line = |id: &str, a: &str, b: &str, difference: &str| {
            format!("{id:id_width$}  {a:a_width$}  {b:b_width$}  {difference}")
                .trim_end()
                .to_string()
        };
        writeln!(f, "{}", line("metric", "a", "b", "b - a"))?;
        for (id, [a, b, difference]) in &rows {
            writeln!(f, "{}", line(id, a, b, difference))?;
        }
        Ok(())
    }
}

#[path = "results_comparison_tests.rs"]
#[cfg(test)]
mod results_comparison_tests;
