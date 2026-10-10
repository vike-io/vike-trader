//! `impl BacktestReport`: composing a report from a run, and reading or rendering its metrics.

// `writeln!` into a `String` (every `writeln!` in this file is in the `render_metrics` builder, and
// each writes into its `out` string) needs the trait in scope; anonymous so it brings `Write` in
// for `String` without a nameable `Write` that could be mistaken for `fmt::Display`'s formatter.
use std::fmt::Write as _;

use super::{BacktestReport, BacktestResult, ExtendedMetrics, HonestyCounters, metrics};

impl BacktestReport {
    /// Compose a report from a raw [`BacktestResult`]. `periods_per_year` is the annualization
    /// factor `metrics::sharpe` needs — the caller (the `backtest` bin) picks it from the
    /// profile's data kind/interval via `harness::report::periods_per_year` (252 for daily bars; a
    /// documented default otherwise), since the result itself doesn't carry that information.
    pub fn from_result(name: Option<String>, r: &BacktestResult, periods_per_year: f64) -> Self {
        BacktestReport {
            name,
            final_equity: r.final_equity,
            total_return: metrics::total_return(&r.equity_curve),
            n_trades: r.n_trades,
            win_rate: metrics::win_rate(&r.trades),
            sharpe: metrics::sharpe(&r.equity_curve, periods_per_year),
            max_drawdown: metrics::max_drawdown(&r.equity_curve),
            profit_factor: metrics::profit_factor(&r.trades),
            funding_paid: r.funding_paid,
            per_symbol_pnl: r.per_symbol_pnl.clone(),
            // `None` for any run that traded or moved equity -> the report is byte-identical to
            // before this field existed; `Some` only diagnoses the all-zero / flat case.
            zero_trade: crate::zero_trade::ZeroTradeReport::analyze(r),
            // ⚠ ALWAYS `Some` from this door, unlike `zero_trade`. A conditional long-form block
            // would make "this run was clean" and "this run was recorded by an older binary"
            // the same bytes, and the second is the one a reader must refuse rather than render.
            extended: Some(ExtendedMetrics::from_result(r, periods_per_year)),
            honesty: Some(HonestyCounters::from_result(r)),
            // The stamp needs the profile, which this crate cannot name — see the field's doc.
            realism: None,
        }
    }

    /// Attach the cost model this run ran under. The producer's door, called once beside
    /// [`Self::from_result`] — see [`Self::realism`] for why it is a second step rather than a
    /// parameter.
    #[must_use]
    pub fn with_realism(mut self, stamp: crate::realism::RealismStamp) -> Self {
        self.realism = Some(stamp);
        self
    }

    /// The value of one [`crate::metric_catalog::METRICS`] id, wherever it is stored.
    ///
    /// Three outcomes, never two, and the middle one is the point: `Some(v)` is the number,
    /// `None` with [`Self::extended`] absent means the RUN never recorded it, and a `None` for an
    /// id the catalog does not hold cannot happen because
    /// [`crate::metric_catalog::parse_metric_selection`] already refused it. A caller that collapses
    /// the first two renders `0.0` for a run that has no answer.
    pub fn metric_value(&self, id: &str) -> Option<f64> {
        match id {
            "final_equity" => Some(self.final_equity),
            "total_return" => Some(self.total_return),
            "n_trades" => Some(self.n_trades as f64),
            "win_rate" => Some(self.win_rate),
            "sharpe" => Some(self.sharpe),
            "max_drawdown" => Some(self.max_drawdown),
            "profit_factor" => Some(self.profit_factor),
            "funding_paid" => Some(self.funding_paid),
            other => self.extended.as_ref().and_then(|e| e.value_of(other)),
        }
    }

    /// Render what an operator selected.
    ///
    /// [`crate::metric_catalog::MetricSelection::Compact`] delegates to this type's `Display`
    /// rather than re-rendering the same eight rows: the compact table is a published human format
    /// with a zero-trade branch and two conditional rows in it, and a second spelling of it would
    /// be two formats that drift. Every other selection renders one aligned row per id.
    ///
    /// A selection this report cannot answer produces a row saying so BY NAME rather than a zero or
    /// a silent omission — the run is what is missing, not the metric.
    ///
    /// ⚠ **This function owns the LAYOUT and nothing else.** How a number reads — the percent
    /// scaling, the decimal count — belongs to
    /// [`crate::metric_catalog::MetricUnit::render`], because it used to be spelled here AND in
    /// three other renderers, and the ones that spelled it differently published a fraction as a
    /// percent. Do not re-derive a cell here; widen the unit.
    ///
    /// # Nothing but tests calls it, so four of the five selections have no spelling
    ///
    /// ⚠ MEASURED (`git grep -n render_metrics`): no production code calls `render_metrics` — the
    /// other hits are doc links and prose in `metric_catalog`, `tearsheet` and `vike-app-core`'s
    /// tearsheet view — and every call site is a test under
    /// `crates/vike-analytics/src/report/tests/` (`catalog.rs`, `honesty.rs`). The only production use of the selection type anywhere is
    /// `crates/vike-cli/src/cmd/runs/show.rs`'s `report_key_order`, which asks
    /// [`crate::metric_catalog::MetricSelection::Compact`] for its ids in order to order JSON KEYS
    /// and never renders a cell. `--metrics` is still `value: Value::None` in
    /// `crates/vike-cli/src/surface.rs`'s `FLAGS`, so `Full`, `Named`, `Honesty` and `Realism` have
    /// no spelling an operator can type — the door
    /// [`crate::metric_catalog::parse_metric_selection`] was written for, whose own doc carries
    /// what wiring it owes.
    ///
    /// ⚠ **What that does NOT mean, and the stronger claim is the tempting one: it does NOT mean
    /// the honesty counters and the realism stamp go unprinted.** Two shipped surfaces print them
    /// today. This type's own `Display` prints the honesty block whenever
    /// [`HonestyCounters::is_noteworthy`] and a one-line `realism: FRICTIONLESS` row whenever the
    /// stamp carries that reason — both deliberately conditional, argued at those two sites. And
    /// `crates/vike-cli/src/cmd/runs/show.rs`'s `show_text` prints every `report.json` key its
    /// `report_key_order` does not name, alphabetically, which is `extended`, `honesty` and
    /// `realism` as their raw JSON. So what has no door is this RENDERER and the selection plane
    /// around it: the aligned per-id table, the two keywords that render those blocks instead of
    /// their JSON, and the run-aware "not recorded" sentences below — which are the half that
    /// distinguishes "nothing happened" from "nobody looked", and the reason wiring the door is
    /// worth doing rather than merely tidy.
    pub fn render_metrics(&self, sel: &crate::metric_catalog::MetricSelection) -> String {
        use crate::metric_catalog::{MetricSelection, MetricUnit, spec_for};

        match sel {
            MetricSelection::Compact => return self.to_string(),
            MetricSelection::Honesty => {
                return match &self.honesty {
                    Some(h) => h.to_string(),
                    None => "honesty counters: not recorded — this run predates the block, so \
                             \"nothing happened\" and \"nobody looked\" are not distinguishable \
                             for it. Re-run the profile.\n"
                        .to_string(),
                };
            }
            MetricSelection::Realism => {
                return match &self.realism {
                    Some(r) => r.to_string(),
                    None => "realism stamp: not recorded — nobody stamped this run, which is a \
                             different answer from a frictionless one and must not be read as \
                             one. Re-run the profile.\n"
                        .to_string(),
                };
            }
            MetricSelection::Full | MetricSelection::Named(_) => {}
        }

        let ids = sel.ids();
        let width = ids.iter().map(|i| i.len()).max().unwrap_or(0);
        let mut out = String::with_capacity(64 * ids.len() + 128);
        // The label column is `width` wide for every row INCLUDING this one, so a reader's eye and
        // a `cut -c` both find the values in one place.
        let _ = writeln!(out, "{:width$}  {}", "name", self.name.as_deref().unwrap_or("(unnamed)"));
        for id in ids {
            // `spec_for` cannot answer `None` for an id that came out of `sel.ids()` — that
            // function filters `METRICS` itself — so the fallback is unreachable rather than a
            // silent default.
            let unit = spec_for(id).map_or(MetricUnit::Ratio, |s| s.unit);
            match self.metric_value(id) {
                Some(v) => {
                    // ⚠ The scaling and the precision are the UNIT's, not this renderer's — see
                    // [`crate::metric_catalog::MetricUnit::render`]. This was a private `match`
                    // here, the second of four spellings of one rule, and the doors that spelled it
                    // differently published a percent row a hundred times too small.
                    let rendered = unit.render(v);
                    let _ = writeln!(out, "{id:width$}  {rendered}");
                }
                None => {
                    let _ = writeln!(
                        out,
                        "{id:width$}  not recorded (this run has no `extended` block)"
                    );
                }
            }
        }
        out
    }
}
