//! `impl TradingResults`: composing a report from a run, and reading or rendering its metrics.

// `writeln!` into a `String` (every `writeln!` in this file is in the `render_metrics` builder, and
// each writes into its `out` string) needs the trait in scope; anonymous so it brings `Write` in
// for `String` without a nameable `Write` that could be mistaken for `fmt::Display`'s formatter.
use std::fmt::Write as _;

use super::{
    TradingRecord, TradingResults, TradingResultsExtendedMetrics, TradingResultsHonesty, metrics,
};

impl TradingResults {
    /// Compose a report from a raw [`TradingRecord`]. `periods_per_year` is the annualization
    /// factor `metrics::sharpe` needs — the caller (the `backtesthub` bin) picks it from the
    /// profile's data kind/interval via `run::report::periods_per_year` (252 for daily bars; a
    /// documented default otherwise), since the result itself doesn't carry that information.
    ///
    /// A record whose curve is kept by TIME (`TradingRecord::sampling`, a tick run since 2026-11-20) does carry it:
    /// it is annualized at its own step (`TradingRecord::periods_per_year`) and `periods_per_year` is not read, and
    /// its maximum drawdown is the whole path's (`TradingRecord::max_drawdown`), not the closes'.
    pub fn from_record(name: Option<String>, r: &TradingRecord, periods_per_year: f64) -> Self {
        let periods_per_year = r.periods_per_year(periods_per_year);
        TradingResults {
            name,
            final_equity: r.final_equity,
            total_return: metrics::total_return(&r.equity_curve),
            n_trades: r.n_trades,
            win_rate: metrics::win_rate(&r.trades),
            sharpe: metrics::sharpe(&r.equity_curve, periods_per_year),
            max_drawdown: r.max_drawdown(),
            profit_factor: metrics::profit_factor(&r.trades),
            funding_paid: r.funding_paid,
            per_symbol_pnl: r.per_symbol_pnl.clone(),
            // `None` for any run that traded or moved equity -> the report is byte-identical to
            // before this field existed; `Some` only diagnoses the all-zero / flat case.
            zero_trade: crate::zero_trade::ZeroTradeReport::analyze(r),
            invalid: r.equity_reached_zero(),
            // ⚠ ALWAYS `Some` from this door, unlike `zero_trade`. A conditional long-form block
            // would make "this run was clean" and "this run was recorded by an older binary"
            // the same bytes, and the second is the one a reader must refuse rather than render.
            extended: Some(TradingResultsExtendedMetrics::from_result(r, periods_per_year)),
            // A simulator's counters only: `None` for a live account or a walk-forward step.
            honesty: r.simulator().map(TradingResultsHonesty::from_simulator),
            // The stamp needs the profile, which this crate cannot name — see the field's doc.
            realism: None,
            data_warnings: r.data_warnings.clone(),
            in_dollars: r.settle_price.map(|price| super::TradingResultsInDollars {
                price,
                final_equity: r.final_equity * price,
            }),
            closed_trades: r.closed_trades.clone(),
            // The strategy file needs the profile, as the stamp does: `with_strategy`.
            strategy_file: String::new(),
            strategy_sha256: String::new(),
            strategy_sdk: None,
            returns_interval_ms: r.sampling.map(|s| s.interval_ms),
            mounts: r.mounts.clone(),
            strategy_failures: r.strategy_failures.clone(),
            source: Some(r.source.clone()),
            period: r.period(),
        }
    }

    /// **The ONE typed read of a stored report** (`report.json`, the backtesthub's `Response::Report`
    /// text): the reader states what it knows of the source and the period, because neither is in the
    /// bytes ([`Self::source`]).
    ///
    /// # Errors
    /// The parse error, in words, when `text` is not a report.
    pub fn read_stored(
        text: &str,
        source: crate::record::TradingRecordSource,
        period: Option<crate::record::TradingPeriod>,
    ) -> Result<Self, String> {
        let mut results: Self =
            serde_json::from_str(text).map_err(|e| format!("not a stored report: {e}"))?;
        results.source = Some(source);
        results.period = period;
        Ok(results)
    }

    /// **The label of these results**: their source and period in the one wording
    /// ([`crate::record::TradingRecordSource::label`]), a backtest named by its strategy file (or by
    /// [`Self::name`] where none was recorded). `None` when the source is unknown ([`Self::source`]).
    #[must_use]
    pub fn source_label(&self) -> Option<String> {
        let file =
            Some(self.strategy_file.as_str()).filter(|f| !f.is_empty()).or(self.name.as_deref());
        Some(self.source.as_ref()?.label(file, self.period))
    }

    /// Whether these results are one live account's ([`crate::record::TradingRecordSource::Live`]).
    #[must_use]
    pub fn is_live(&self) -> bool {
        matches!(self.source, Some(crate::record::TradingRecordSource::Live { .. }))
    }

    /// Whether these results are a LIVE record that measured nothing: it closed no trade and its curve never
    /// moved ([`Self::zero_trade`]'s condition), as a window with no fills in it. Its money and counts are
    /// facts; every return, ratio and probability over it is not recorded, and a comparison holds no
    /// difference against it.
    #[must_use]
    pub fn measured_nothing(&self) -> bool {
        self.is_live() && self.zero_trade.is_some()
    }

    /// Attach the strategy file this run ran, the sha256 of its text and the SDK its artifact was
    /// built against ([`Self::strategy_sdk`]): the producer's door, beside [`Self::with_realism`].
    #[must_use]
    pub fn with_strategy(mut self, file: &str, sha256: &str, sdk: Option<u32>) -> Self {
        self.strategy_file = file.to_string();
        self.strategy_sha256 = sha256.to_string();
        self.strategy_sdk = sdk;
        self
    }

    /// Attach the cost model this run ran under. The producer's door, called once beside
    /// [`Self::from_record`] — see [`Self::realism`] for why it is a second step rather than a
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
        let money_or_count = || {
            crate::metric_catalog::METRICS
                .iter()
                .find(|spec| spec.id == id)
                .is_some_and(crate::results_table::shown_on_an_invalid_run)
        };
        if self.measured_nothing() && !money_or_count() {
            return None;
        }
        match id {
            "final_equity" => Some(self.final_equity),
            "total_return" => Some(self.total_return),
            "n_trades" => Some(self.n_trades as f64),
            "win_rate" => Some(self.win_rate),
            "sharpe" => Some(self.sharpe),
            "max_drawdown" => Some(self.max_drawdown),
            "profit_factor" => Some(self.profit_factor),
            // A LIVE record is folded from the execution stream's fills, and the stream carries no funding
            // (`exec_funding` stays out of it), so its `0.0` would be a false number: not recorded.
            // shortcut: keyed on the source; the day funding joins the stream, record it and lift this.
            "funding_paid" if self.is_live() => None,
            "funding_paid" => Some(self.funding_paid),
            other => self
                .extended
                .as_ref()
                .and_then(|e| e.value_of(other))
                .or_else(|| self.closed_trades.as_ref()?.value_of(other)),
        }
    }

    /// Every catalog metric as `(id, text)`, in [`crate::metric_catalog::METRICS`] order — the
    /// twin of [`crate::results_table::TradingResultsTable::rendered_rows`] for a report a GUI holds, and the
    /// SAME rendering: it reads the report through [`crate::results_table::TradingResultsTable::from_report`],
    /// so a cell is that unit's `MetricUnit::render` and an unrecorded metric is
    /// [`crate::results_table::NOT_RECORDED`], never a number. The Studio's Performance, Validation and
    /// Saved-compare tables render it instead of choosing rows and precisions of their own.
    ///
    /// An INVALID run ([`Self::invalid`]) opens with an [`crate::INVALID_LABEL`] row of its words and keeps
    /// only the money and count rows ([`crate::results_table::TradingResultsTable::rendered_rows`]).
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<(&'static str, String)> {
        crate::results_table::TradingResultsTable::from_report(self).rendered_rows()
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
    /// `crates/vike-analytics/src/results/tests/` (`catalog.rs`, `honesty.rs`). The only production use of the selection type anywhere is
    /// `crates/vike-cli/src/cmd/backtest/read/show.rs`'s `report_key_order`, which asks
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
    /// [`TradingResultsHonesty::is_noteworthy`] and a one-line `costs:` row (what the run was charged, in
    /// words) on every stamped report — argued at those two sites. And
    /// `crates/vike-cli/src/cmd/backtest/read/show.rs`'s `show_text` prints every `report.json` key its
    /// `report_key_order` does not name, alphabetically, which is `extended`, `honesty` and
    /// `realism` as their raw JSON. So what has no door is this RENDERER and the selection plane
    /// around it: the aligned per-id table, the two keywords that render those blocks instead of
    /// their JSON, and the run-aware "not recorded" sentences below — which are the half that
    /// distinguishes "nothing happened" from "nobody looked", and the reason wiring the door is
    /// worth doing rather than merely tidy.
    pub fn render_metrics(&self, selection: &crate::metric_catalog::MetricSelection) -> String {
        use crate::metric_catalog::{MetricHome, MetricSelection, MetricUnit, spec_for};

        match selection {
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
                    None => "realism stamp: not recorded — nobody stamped this run, so what it \
                             was charged is unknown, which must not be read as \"no fees\". \
                             Re-run the profile.\n"
                        .to_string(),
                };
            }
            MetricSelection::Full | MetricSelection::Named(_) => {}
        }

        let closed = |id: &str| spec_for(id).is_some_and(|s| s.home == MetricHome::ClosedTrades);
        // A closed-trade row appears only where the run armed it; one named by id still says why it is absent.
        let unarmed = matches!(selection, MetricSelection::Full) && self.closed_trades.is_none();
        let ids: Vec<&str> =
            selection.ids().into_iter().filter(|id| !(unarmed && closed(id))).collect();
        let width = ids.iter().map(|i| i.len()).max().unwrap_or(0);
        let mut out = String::with_capacity(64 * ids.len() + 128);
        // The label column is `width` wide for every row INCLUDING this one, so a reader's eye and
        // a `cut -c` both find the values in one place.
        let _ = writeln!(out, "{:width$}  {}", "name", self.name.as_deref().unwrap_or("(unnamed)"));
        for id in ids {
            // `spec_for` cannot answer `None` for an id that came out of `selection.ids()` — that
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
                None if closed(id) => {
                    let _ = writeln!(
                        out,
                        "{id:width$}  not recorded (this run did not arm `engine.closed_trades`)"
                    );
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
