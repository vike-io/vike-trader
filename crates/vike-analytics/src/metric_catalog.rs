//! The METRIC TAXONOMY as data: which performance numbers this workspace can answer, what each one
//! measures, where it is stored on a report, and the parser for an operator's selection over it.
//!
//! # Why a table rather than a `match`
//!
//! [`crate::metrics`] declares three dozen functions and [`crate::report::BacktestReport`] carried
//! eight of them. The eight were not a judgement about which numbers matter — they are the ones the
//! first version happened to need for a ranking objective — and the consequence was measurable:
//! the live tearsheet (`LiveTearsheet`, then in `vike-report`, now [`crate::LiveTearsheet`])
//! printed Sortino, Calmar, CAGR, SQN, VaR and expected shortfall over the SAME
//! [`crate::BacktestResult`] the backtest verb printed eight scalars for, so the same fills
//! answered a richer question through the live door than through the backtest door.
//!
//! Widening the report alone would not have fixed that, because the next consumer would have
//! hand-copied its own list of names — which is what happened four times already (the row vector
//! in what is now `crates/vike-analytics/src/html.rs`, vike-report's own finiteness test, the GUI
//! tearsheet panel's accessor, and an eleven-entry array of report keys typed out by hand in
//! `crates/vike-cli/src/cmd/runs/show.rs` — all four now DELETED, the last of them in favour of
//! that file's `report_key_order`, which asks [`MetricSelection::Compact`] for its middle eight).
//! So the roster is DATA here, [`METRICS`] is the only place it is
//! written, and the declaration order IS the render order — a consumer asks this module for the
//! ordered ids instead of keeping its own copy.
//!
//! # A metric is either computed or DECLARED ABSENT
//!
//! [`ABSENT`] is the other half and the reason this is a taxonomy rather than a list: a name an
//! operator can reasonably ask for and this tree cannot answer must say WHY, because the
//! alternative is an "unknown metric" refusal that reads as a typo. `exposure` is the worked
//! example — [`crate::metrics::exposure`] exists and is correct, and nothing in a run record can
//! feed it.

mod rows;
mod selection;

pub use rows::{ABSENT, METRICS};
pub use selection::{MetricSelection, metric_list_text, parse_metric_selection};

/// Where a metric's value is stored on a serialized report.
///
/// The split is not cosmetic: [`Self::Compact`] names a field that has been in `report.json` since
/// the document's first version, so every stored run can answer it; [`Self::Extended`] names a
/// field of [`crate::report::ExtendedMetrics`], which is `None` on a report written before that
/// block existed. A reader that ignores the distinction renders `0.0` for a run whose real answer
/// is "this was never recorded" — the failure mode [`crate::report::BacktestReport`]'s own doc
/// argues against for the required-versus-defaulted scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricHome {
    /// A scalar field of [`crate::report::BacktestReport`] itself.
    Compact,
    /// A field of [`crate::report::ExtendedMetrics`], the opt-in long-form block.
    Extended,
}

/// How a metric's number is to be read, which is what a renderer needs in order not to lie about
/// it: a drawdown of `0.031` printed as `0.0310` invites the reading "three basis points".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    /// Account currency — two decimals, no scaling.
    Money,
    /// A FRACTION on the wire, rendered times 100 with a `%` suffix. The scaling lives with the
    /// unit rather than at each call site because the report stores fractions, and every renderer
    /// that forgot the scaling published a number a hundred times too small.
    ///
    /// ⚠ That sentence was ASPIRATIONAL for as long as there was no [`MetricUnit::render`] to hold
    /// it — the scaling was spelled at four call sites instead, and two of them disagreed. It is
    /// that method now, and a renderer that reaches past it is the defect this variant's doc has
    /// always described.
    Percent,
    /// A dimensionless ratio — four decimals, no scaling. May carry the house `inf`/`0.0` sentinel
    /// (see [`crate::report::BacktestReport::profit_factor`]).
    Ratio,
    /// A whole count. Rendered with no decimals, because `3.0000` trades is not a thing.
    Count,
}

impl MetricUnit {
    /// Render `v` the way this unit is to be read — **the ONE home for the scaling and the
    /// precision**, which is what [`Self::Percent`]'s own doc has claimed since the enum was
    /// written and what was not true until this method existed.
    ///
    /// # What it refuses: a renderer deciding for itself
    ///
    /// The rule was spelled FOUR times. A private `match` inside
    /// `crate::report::BacktestReport::render_metrics`; `crates/vike-analytics/src/html.rs`'s
    /// `render_html_inner` (vike-report's then), which applied `* 100.0` by hand on four rows and
    /// got two MONEY rows' precision wrong besides;
    /// `crates/vike-app-core/src/ui/tool_views/tearsheet.rs`'s
    /// `tearsheet_rows`; and the CLI. Four spellings of one rule are four chances to forget it, and
    /// forgetting is not cosmetic: a renderer that drops the `* 100.0` publishes a max drawdown of
    /// three percent as `0.0310%`, which reads as three basis points — a number a hundred times too
    /// small, on the row an operator sizes risk from. Every renderer that forgot did exactly that.
    ///
    /// # What it deliberately does NOT do
    ///
    /// It does not rescue the house `inf`/`0.0` sentinel (see
    /// [`crate::report::BacktestReport::profit_factor`]). A non-finite value renders as Rust's own
    /// `inf`, byte-identical to what every door already prints, because substituting a placeholder
    /// here would move a published row in all of them at once — and "there is no meaningful ratio"
    /// is a statement about that METRIC, not about the unit it would have been measured in.
    ///
    /// It also does not pad, align or label. A caller owns its own column layout; this answers only
    /// "what does this number SAY", which is the half that was being answered four different ways.
    #[must_use]
    pub fn render(self, v: f64) -> String {
        match self {
            MetricUnit::Money => format!("{v:.2}"),
            // ⚠ The `* 100.0` every hand-rolled renderer forgot at least once. The report stores
            // FRACTIONS — see this variant's own doc.
            MetricUnit::Percent => format!("{:.4}%", v * 100.0),
            MetricUnit::Ratio => format!("{v:.4}"),
            // No decimals. A count reaches here as an `f64` (the `usize` fields widened by
            // [`crate::report::ExtendedMetrics::value_of`]), and `3.0000` would read as a measured
            // quantity rather than as three trades.
            MetricUnit::Count => format!("{v:.0}"),
        }
    }
}

/// One metric this tree can answer.
#[derive(Debug, Clone, Copy)]
pub struct MetricSpec {
    /// The id an operator types and the KEY the value is serialized under — deliberately the same
    /// string, so `--metrics sortino` and a `jq .extended.sortino` name one thing.
    pub id: &'static str,
    /// Which struct holds it — see [`MetricHome`].
    pub home: MetricHome,
    /// How to render it — see [`MetricUnit`].
    pub unit: MetricUnit,
    /// One line, for [`metric_list_text`]. What the number MEASURES, not how it is computed: the
    /// computation is on the `crate::metrics` function of the same name, and restating it here
    /// would be a second copy to rot.
    pub what: &'static str,
}

/// The [`MetricSpec`] for `id`, or `None` when nothing in the catalog is called that.
pub fn spec_for(id: &str) -> Option<&'static MetricSpec> {
    METRICS.iter().find(|m| m.id == id)
}

#[path = "metric_catalog_tests.rs"]
#[cfg(test)]
mod metric_catalog_tests;
