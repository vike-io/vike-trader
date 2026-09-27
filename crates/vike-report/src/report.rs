//! `LiveTearsheet` — the assembled live performance summary, **keyed on the METRIC CATALOG**.
//!
//! Composes reconstructed [`Trade`]s + an equity curve into a `vike_analytics::BacktestResult`,
//! folds that through `vike_analytics::BacktestReport` — the one assembly site — and then carries
//! every `vike_analytics::metric_catalog::METRICS` id it answers. This module does NO metric math
//! of its own, no metric ASSEMBLY of its own, and — since the taxonomy work — **no metric ROSTER of
//! its own either**.
//!
//! # ⚠ Why the fields went away
//!
//! This type declared TWENTY-FIVE `pub` numeric fields, and that list was a SECOND TRUTH about
//! which performance numbers this workspace has. It was measurably a different truth — MEASURED
//! 2026-09-16, the day the fields came out, and a historical count rather than a standing one
//! (`METRICS.len()` is the only authority for today's, and `the_catalog_is_the_roster` below is
//! what holds this type equal to it): the catalog held 38 ids, `BacktestReport::from_result`
//! computed every one of them, and this type copied 25 —
//! **it threw thirteen already-computed numbers away** (`funding_paid` off the compact scalars,
//! and `long_ratio`, `mar_ratio`, `recovery_factor`, `ulcer_index`, `ulcer_performance_index`,
//! `k_ratio`, `risk_return_ratio`, `returns_volatility`, `returns_skewness`, `returns_kurtosis`,
//! `tail_ratio` and `omega` out of `ExtendedMetrics`). Not because anybody judged them
//! uninteresting — because a hand-typed field list cannot grow when the catalog does.
//! `vike_analytics::metric_catalog`'s own module doc names three hand copies of that kind and this
//! file's was one of them; [`MetricValues`] is the answer, and its declaration order IS the render
//! order because it IS `METRICS`.
//!
//! The owner's ruling that shaped this: **the numbers become one roster and the two SHAPES stay.**
//! So there is still a text table ([`LiveTearsheet`]'s `Display`) and still an HTML document
//! ([`crate::html`]) — two presentations, one roster, and each cell rendered by
//! `vike_analytics::metric_catalog::MetricUnit::render` rather than by a local `format!`.
//!
//! # ⚠ This is a WIRE DOCUMENT, so the break is NAMED
//!
//! `vike_tradehub_client::proto`'s `Response::Tearsheet` carries this type as JSON TEXT and
//! `crates/vike-cli/src/cmd/report.rs` passes those bytes through VERBATIM, so the FLAT shape is a
//! contract: one top-level key per metric, exactly as before. What changed is which keys exist (13
//! more) and the addition of [`LiveTearsheet::schema`].
//!
//! `crates/vike-cli/src/cmd/report_schema.rs`'s module doc declared the hole this closes — "it does
//! **not** cover the LIVE tearsheet a `vike-tradehub` node answers with … That is a declared hole
//! rather than an oversight — closing it means versioning the document in `vike-report`, beside its
//! producer." This is that. The field is spelled `schema` and not `schema_version` for the reason
//! that file argues at length: `vike_model::runs::RunSeries` and `RunTrades` spell it `schema`, and
//! a second spelling for one idea inside one family means a consumer needs two readers to answer
//! one question.
//!
//! [`Deserialize`] ships beside [`Serialize`] for the same reason: a document nothing can parse is
//! a document whose version nobody can branch on, and the reader half is what makes the version
//! worth declaring.

use std::fmt;

use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use vike_analytics::metric_catalog::{METRICS, MetricSpec, MetricUnit};
use vike_analytics::{BacktestReport, BacktestResult};
use vike_model::Trade;

/// Annualization factor for daily periods — the LEAN/tearsheet convention. Re-exported from
/// vike-analytics (NOT a second copy of `252.0`) so the live and backtest tearsheets annualize on
/// one constant. A pure fill stream has no fixed period, so the caller passes whatever cadence its
/// equity samples represent; 252 is the default.
pub use vike_analytics::DAILY_PERIODS_PER_YEAR;

/// The version [`LiveTearsheet`] declares in its `schema` key. `1` is the first shape that carries
/// one at all.
///
/// Bump it for any change a consumer could not read: a REMOVED or RETYPED key, or — because the
/// key set is the contract and not the key order — a key set that GROWS. That is the same rule
/// `crates/vike-cli/src/cmd/report_schema.rs`'s `REPORT_SCHEMA` states for the stored-run
/// document, and it is deliberately the same rule: these two documents are read by the same
/// operators through the same verb.
///
/// ⚠ The key set here grows when `vike_analytics::metric_catalog::METRICS` grows, which is a bump
/// in a file this crate does not own. That is deliberate rather than a hazard: the catalog is the
/// roster for the whole workspace, so one addition there is one version bump here, and the
/// alternative (a hand-kept field list that silently does NOT grow) is the defect this type was
/// restructured to remove.
pub const TEARSHEET_SCHEMA: u32 = 1;

/// What an ABSENT `schema` key deserializes to — a document written before the version existed.
///
/// It is not an error: every tearsheet a node emitted before this field shipped is a valid
/// document, and refusing to parse one would make the version's arrival a breaking change for
/// readers instead of a legible one. A reader comparing against [`TEARSHEET_SCHEMA`] sees `0` and
/// knows exactly what it is holding.
pub const TEARSHEET_SCHEMA_UNVERSIONED: u32 = 0;

/// The text a renderer prints for a metric this document does not carry.
///
/// ⚠ **Not `0.0`, and that is the whole point of the distinction.** A `None` means THIS DOCUMENT
/// DID NOT RECORD IT, which is a different answer from a zero — rendering an unrecorded Sortino as
/// `0.0000` publishes "this strategy had no downside". `vike_analytics::metric_catalog::MetricHome`
/// argues the same case from the report side, and `BacktestReport::render_metrics` prints its own
/// sentence for it; this constant is that convention on the tearsheet's two renderers.
pub const NOT_RECORDED: &str = "not recorded";

/// The catalog metrics one tearsheet carries: one slot per `metric_catalog::METRICS` row,
/// POSITIONAL — `values[i]` is `METRICS[i]`.
///
/// # Why positional rather than a map
///
/// A `HashMap<String, f64>` would make "which metrics exist" a question about whatever keys a
/// particular value happens to hold, which is the hand-copied-roster problem in a different
/// container. Indexing off `METRICS` makes the roster STRUCTURAL: the length is the catalog's
/// length by construction, the iteration order is the catalog's declaration order (which is the
/// render order), and an id the catalog does not hold is unrepresentable.
///
/// # The `Option` is load-bearing
///
/// `None` is "this document does not carry that metric", not "zero" — see [`NOT_RECORDED`]. Three
/// things produce one: a metric added to the catalog after a document was written, a
/// `BacktestReport` with no `extended` block (`BacktestReport::metric_value` answers `None` for
/// every extended id there, and this type carries that answer through rather than substituting a
/// number), and the one deliberate case in [`LiveTearsheet::from_result_parts`].
#[derive(Debug, Clone, PartialEq)]
pub struct MetricValues {
    /// One slot per `METRICS` row, in declaration order. ⚠ INVARIANT: `values.len() ==
    /// METRICS.len()`. Every constructor here holds it; the accessors are written to survive a
    /// short vector anyway (reading past the end answers `None` instead of panicking), because
    /// this type is deserialized from bytes another process wrote.
    values: Vec<Option<f64>>,
}

impl Default for MetricValues {
    /// Every catalog metric declared UNRECORDED.
    ///
    /// ⚠ Hand-written rather than derived: `#[derive(Default)]` would produce an EMPTY vector,
    /// which silently breaks this type's one invariant and makes every metric read `None` for the
    /// wrong reason.
    fn default() -> Self {
        Self { values: vec![None; METRICS.len()] }
    }
}

impl MetricValues {
    /// Every catalog metric declared UNRECORDED — the starting point a filler writes into.
    #[must_use]
    pub fn unrecorded() -> Self {
        Self::default()
    }

    /// Fill every slot by asking `f` for each catalog id, in declaration order.
    ///
    /// This is the ONE filler, and `f` returning `Option` is what keeps "not recorded" expressible
    /// all the way from the source document — see [`LiveTearsheet::from_report`], whose `f` is
    /// `BacktestReport::metric_value` verbatim.
    #[must_use]
    pub fn from_fn(mut f: impl FnMut(&'static str) -> Option<f64>) -> Self {
        Self { values: METRICS.iter().map(|m| f(m.id)).collect() }
    }

    /// The value of `id`.
    ///
    /// `None` answers TWO different questions — the catalog holds no such id, and this document
    /// does not carry it — and a caller that needs them apart asks
    /// `vike_analytics::metric_catalog::spec_for` which of the two it is. That collapse is
    /// deliberate: every caller in this crate renders the two the same way, and a three-valued
    /// return would be a type nobody wanted for a distinction they do not make.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<f64> {
        let i = METRICS.iter().position(|m| m.id == id)?;
        self.values.get(i).copied().flatten()
    }

    /// Declare `id`'s value (`None` = explicitly not recorded). Returns `false`, writing nothing,
    /// when the catalog holds no such id — an unknown id is a caller's bug and must not silently
    /// grow this document a key the catalog does not publish.
    pub fn set(&mut self, id: &str, v: Option<f64>) -> bool {
        match METRICS.iter().position(|m| m.id == id) {
            Some(i) if i < self.values.len() => {
                self.values[i] = v;
                true
            }
            _ => false,
        }
    }

    /// Every catalog row paired with its value, in declaration (== render) order.
    pub fn rows(&self) -> impl Iterator<Item = (&'static MetricSpec, Option<f64>)> + '_ {
        METRICS.iter().enumerate().map(|(i, m)| (m, self.values.get(i).copied().flatten()))
    }
}

/// ⚠ **Serialized FLAT — one top-level key per catalog id — because this is a wire document.**
///
/// The key is `MetricSpec::id`, which is deliberately also the string an operator types at
/// `--metrics` and the key a `jq .sortino` names. `None` is written as `null` (never omitted, never
/// `0`): `crates/vike-cli/src/cmd/report_schema.rs`'s `NULL_CONVENTION` is the published rule and
/// this is it applied here. A non-finite value also lands as `null`, because that is what
/// serde_json does with one — the house `inf` sentinel is preserved in the TYPE and flattened on
/// the wire, exactly as it was before this restructuring.
///
/// ⚠ A `MetricUnit::Count` metric is written as an INTEGER when its value is a whole number, not
/// as `2.0`. That is not cosmetic: `n_trades`, `consecutive_wins` and `consecutive_losses` were
/// `usize` fields, so every existing consumer of this document —
/// `crates/vike-report/tests/tearsheet_cli.rs` included — reads them as integers, and
/// `serde_json::Value`'s `PartialEq` does NOT consider `2.0` equal to `2`. Widening them to `f64`
/// internally is what lets the roster be one type; the wire must not pay for that.
impl Serialize for MetricValues {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(METRICS.len()))?;
        for (spec, v) in self.rows() {
            match (spec.unit, v) {
                (MetricUnit::Count, Some(n)) if n.is_finite() && n.trunc() == n => {
                    map.serialize_entry(spec.id, &(n as i64))?;
                }
                _ => map.serialize_entry(spec.id, &v)?,
            }
        }
        map.end()
    }
}

/// ⚠ **An id this build does not know is SKIPPED, and one that is absent stays `None`.**
///
/// Both directions are deliberate. A NEWER producer sends metrics a catalog this old does not
/// hold; refusing the whole document over one unknown key would make every catalog addition a
/// flag-day for readers. An OLDER producer omits metrics this catalog does hold; defaulting those
/// to `0.0` would publish a fabricated number, which is the failure [`NOT_RECORDED`] exists to
/// prevent. So a reader gets what was actually sent, and `schema` says which shape sent it.
impl<'de> Deserialize<'de> for MetricValues {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = MetricValues;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a map of vike_analytics::metric_catalog ids to numbers")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<MetricValues, M::Error> {
                let mut out = MetricValues::unrecorded();
                while let Some(key) = map.next_key::<String>()? {
                    match METRICS.iter().position(|m| m.id == key) {
                        Some(i) => {
                            let v: Option<f64> = map.next_value()?;
                            out.values[i] = v;
                        }
                        None => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(out)
            }
        }

        d.deserialize_map(V)
    }
}

/// A flat live performance summary over the whole metric catalog. Every number is composed by
/// `vike_analytics::BacktestReport` — see the module doc for why this type holds no numeric fields
/// of its own any more.
///
/// NB: some metrics answer `f64::INFINITY` for degenerate inputs (e.g. `profit_factor` with no
/// losing trades) — the house 0.0/inf sentinel convention of `vike_analytics::metrics`. Those are
/// preserved verbatim. `--json` stays VALID on such a run (serde_json writes a non-finite float as
/// `null`, it does not fail), but a consumer must expect `null` where it wanted a number; a real
/// trade history with both wins and losses yields finite metrics throughout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveTearsheet {
    /// The document's shape version — [`TEARSHEET_SCHEMA`] from every constructor here.
    ///
    /// `#[serde(default)]` so a document written before the field existed parses as
    /// [`TEARSHEET_SCHEMA_UNVERSIONED`] rather than failing; that constant's doc argues why a
    /// refusal would be the wrong answer.
    #[serde(default)]
    pub schema: u32,
    /// Free-form label for this tearsheet (e.g. a session or account id).
    ///
    /// ⚠ Stays a public MUTABLE field: `crate::tearsheet_cli`'s `run` assigns `--name` after the
    /// sheet is built, because the label is an operator's word and not a property of the fills.
    #[serde(default)]
    pub name: Option<String>,
    /// The numbers, keyed on `vike_analytics::metric_catalog::METRICS` and serialized FLAT — see
    /// [`MetricValues`] and its `Serialize` impl.
    #[serde(flatten)]
    pub metrics: MetricValues,
}

impl LiveTearsheet {
    /// Assemble a tearsheet from reconstructed trades + an aligned equity curve.
    ///
    /// Builds a `BacktestResult` (the same shape the backtest produces) and fills every catalog
    /// metric through [`Self::from_result`]. `periods_per_year` is the Sharpe/Sortino/Calmar/CAGR
    /// annualization factor (see [`DAILY_PERIODS_PER_YEAR`]).
    ///
    /// # ⚠ `funding_paid` is declared NOT RECORDED on this door, not carried as `0.0`
    ///
    /// The `BacktestResult` built here is SYNTHESIZED from a fill stream, and its `funding_paid`
    /// is therefore `Default`'s `0.0` — a field nobody filled, not a measurement. Publishing that
    /// as a number tells an operator of a live PERP account that funding cost them nothing, which
    /// is a claim this door has no evidence for; `None` says what is true, that the reconstruction
    /// folds no funding cashflow at all. [`Self::from_result`] over a REAL `BacktestResult` keeps
    /// the venue's number, because there the field is measured.
    pub fn from_result_parts(
        name: Option<String>,
        trades: Vec<Trade>,
        equity_curve: Vec<f64>,
        equity_ts: Vec<i64>,
        periods_per_year: f64,
    ) -> Self {
        let n_trades = trades.len();
        let final_equity = equity_curve.last().copied().unwrap_or(0.0);
        let r = BacktestResult {
            trades,
            equity_curve,
            final_equity,
            n_trades,
            equity_ts,
            ..Default::default()
        };
        let mut sheet = Self::from_result(name, &r, periods_per_year);
        sheet.metrics.set("funding_paid", None);
        sheet
    }

    /// Compute the tearsheet from a `BacktestResult` — the reuse seam.
    ///
    /// ⚠ It is now a one-line function, and that is the point: it composes
    /// `vike_analytics::BacktestReport` (which itself composes `ExtendedMetrics`) and reads the
    /// result through [`Self::from_report`]. There is no second assembly, no second
    /// `periods_per_year` argument and no second tail confidence anywhere in this crate, so a live
    /// tearsheet and a stored `report.json` cannot print different Sortinos for one set of fills —
    /// the property the old F31 invariant asserted field-by-field and can now be read off the
    /// call graph.
    pub fn from_result(name: Option<String>, r: &BacktestResult, periods_per_year: f64) -> Self {
        Self::from_report(&BacktestReport::from_result(name, r, periods_per_year))
    }

    /// Read a tearsheet off an already-composed `BacktestReport` — **the door a STORED run comes
    /// through.**
    ///
    /// `crates/vike-cli/src/cmd/runs/show.rs` holds a `report.json` and wants the HTML tearsheet;
    /// this is the whole conversion, and it needs no journal, no store and no equity curve, which
    /// is why it compiles without the `journal` feature.
    ///
    /// Every value is `BacktestReport::metric_value(id)` — including its `None`s. A report with no
    /// `extended` block (one written before that block existed) yields a tearsheet that says
    /// [`NOT_RECORDED`] for every extended metric rather than a tearsheet of zeros, which is the
    /// distinction `vike_analytics::metric_catalog::MetricSelection::needs_extended` exists to make
    /// askable one layer up.
    pub fn from_report(report: &BacktestReport) -> Self {
        LiveTearsheet {
            schema: TEARSHEET_SCHEMA,
            name: report.name.clone(),
            metrics: MetricValues::from_fn(|id| report.metric_value(id)),
        }
    }

    /// The value of one catalog metric — `None` when this document does not carry it (or when the
    /// catalog holds no such id; see [`MetricValues::get`] for why the two collapse).
    #[must_use]
    pub fn metric(&self, id: &str) -> Option<f64> {
        self.metrics.get(id)
    }

    /// Every catalog row paired with its value, in declaration (== render) order — what a renderer
    /// iterates instead of keeping a row list of its own.
    pub fn rows(&self) -> impl Iterator<Item = (&'static MetricSpec, Option<f64>)> + '_ {
        self.metrics.rows()
    }

    /// Every metric as `(id, text)`, in catalog order — **the ONE rendering of this document's
    /// numbers**, shared by the `Display` table and `crate::html`.
    ///
    /// The text comes from `vike_analytics::metric_catalog::MetricUnit::render`, so the `× 100.0`
    /// and the per-unit precision are spelled once for the whole workspace rather than once per
    /// renderer. That method's own doc records what the four hand-rolled spellings cost: two of
    /// them disagreed, and this crate's HTML table printed two MONEY rows at four decimals.
    /// An unrecorded metric renders as [`NOT_RECORDED`], never as a number.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<(&'static str, String)> {
        self.rows()
            .map(|(spec, v)| {
                let text = match v {
                    Some(v) => spec.unit.render(v),
                    None => NOT_RECORDED.to_string(),
                };
                (spec.id, text)
            })
            .collect()
    }

    /// Read a vike-core command journal directory, reconstruct trades from its fill stream, build
    /// the realized-only fallback equity curve from `seed`, and assemble the tearsheet.
    ///
    /// This is the primary live-tearsheet entry point. The equity curve is realized-only (derived
    /// from trade PnLs); when a stored `kind=equity` series exists, prefer
    /// [`Self::from_result_parts`] with `crate::equity::equity_curve_from_store` for a true
    /// mark-to-market curve.
    ///
    /// Behind the `journal` feature — it is the one constructor that reads a journal, so it carries
    /// the vike-core/vike-exec closure the crate doc splits on.
    #[cfg(feature = "journal")]
    pub fn from_journal(
        dir: &std::path::Path,
        seed: f64,
        periods_per_year: f64,
    ) -> Result<Self, crate::journal_read::JournalReadError> {
        use crate::equity::equity_curve_from_trades;
        use crate::journal_read::fills_from_journal;
        use crate::trades::reconstruct_trades;

        let fills = fills_from_journal(dir)?;
        let trades = reconstruct_trades(&fills);
        let (equity, ts) = equity_curve_from_trades(seed, &trades);
        Ok(Self::from_result_parts(None, trades, equity, ts, periods_per_year))
    }
}

/// The TEXT shape — one of the two the owner's ruling preserved.
///
/// The label column is the widest catalog id, so a reader's eye and a `cut -c` both find the values
/// in one place, and the rows are `METRICS` in declaration order. The header carries the document
/// version: a human diffing two tearsheets needs to know whether the row SETS are comparable, and
/// the metric table is not the place to answer a question about the document.
impl fmt::Display for LiveTearsheet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = METRICS.iter().map(|m| m.id.len()).max().unwrap_or(0) + 1;
        writeln!(
            f,
            "=== Live Tearsheet: {} (schema {}) ===",
            self.name.as_deref().unwrap_or("(unnamed)"),
            self.schema
        )?;
        for (id, text) in self.rendered_rows() {
            let label = format!("{id}:");
            writeln!(f, "{label:width$} {text}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equity::equity_curve_from_trades;
    use vike_analytics::metric_catalog::{MetricHome, spec_for};
    // Imported HERE rather than at module scope: production code composes through
    // `BacktestReport`, and the tests want the direct call precisely because comparing a value
    // against it is what proves nothing was reimplemented.
    use vike_analytics::{ExtendedMetrics, metrics};

    fn trade(pnl: f64, fees: f64, is_long: bool, exit_ts: i64) -> Trade {
        Trade {
            entry_price: 100.0,
            exit_price: 100.0 + pnl,
            size: 1.0,
            pnl,
            fees,
            entry_ts: exit_ts - 1,
            exit_ts,
            symbol: "BTCUSDT".to_string(),
            mae: 0.0,
            mfe: 0.0,
            is_long,
        }
    }

    /// A small trades vec with BOTH wins and losses so every metric is finite.
    fn sample_trades() -> Vec<Trade> {
        vec![
            trade(10.0, 0.5, true, 2_000),
            trade(-4.0, 0.5, true, 3_000),
            trade(6.0, 0.3, false, 4_000),
            trade(-2.0, 0.2, true, 5_000),
        ]
    }

    fn sample_sheet() -> (LiveTearsheet, Vec<f64>, Vec<i64>) {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(
            Some("sess-1".into()),
            trades,
            eq.clone(),
            ts.clone(),
            DAILY_PERIODS_PER_YEAR,
        );
        (sheet, eq, ts)
    }

    fn result_of(trades: Vec<Trade>, eq: Vec<f64>, ts: Vec<i64>) -> BacktestResult {
        let n_trades = trades.len();
        let final_equity = eq.last().copied().unwrap_or(0.0);
        BacktestResult {
            trades,
            equity_curve: eq,
            final_equity,
            n_trades,
            equity_ts: ts,
            ..Default::default()
        }
    }

    /// **The roster property.** Every catalog id is carried, so this type cannot be a shorter list
    /// than the catalog again — which is exactly what it was.
    #[test]
    fn the_catalog_is_the_roster() {
        let (sheet, _, _) = sample_sheet();
        let rendered = sheet.rendered_rows();
        assert_eq!(rendered.len(), METRICS.len(), "one row per catalog metric, no more, no fewer");
        for (m, (id, _)) in METRICS.iter().zip(rendered.iter()) {
            assert_eq!(m.id, *id, "rows render in METRICS declaration order");
        }
    }

    /// ⚠ **The twelve numbers the old field list threw away and this door still computes.**
    /// `BacktestReport::from_result` computed every one of them and the old type copied 25 of 38;
    /// a test that only asserted those 25 would have stayed green through the whole defect, so
    /// these are named one by one.
    #[test]
    fn the_discarded_metrics_are_carried_now() {
        let (sheet, _, _) = sample_sheet();
        // `funding_paid` is the thirteenth id the old shape dropped and is deliberately NOT here —
        // `from_result_parts` declares it unrecorded, with the argument on that function.
        for id in [
            "long_ratio",
            "mar_ratio",
            "recovery_factor",
            "ulcer_index",
            "ulcer_performance_index",
            "k_ratio",
            "risk_return_ratio",
            "returns_volatility",
            "returns_skewness",
            "returns_kurtosis",
            "tail_ratio",
            "omega",
        ] {
            assert!(sheet.metric(id).is_some(), "{id} must be carried, not discarded");
        }
    }

    /// **The F31 invariant, in its strongest form**: every catalog value equals
    /// `BacktestReport::metric_value` over the same result. The old test asserted eight shared
    /// fields plus eighteen extended ones by hand; there is nothing left to enumerate, because the
    /// live door composes the report rather than reading beside it.
    #[test]
    fn every_value_equals_the_one_report_composition() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result(
            Some("sess-1".into()),
            &result_of(trades.clone(), eq.clone(), ts.clone()),
            DAILY_PERIODS_PER_YEAR,
        );
        let report = BacktestReport::from_result(
            Some("sess-1".into()),
            &result_of(trades, eq, ts),
            DAILY_PERIODS_PER_YEAR,
        );

        assert_eq!(sheet.name, report.name);
        for m in METRICS {
            assert_eq!(
                sheet.metric(m.id),
                report.metric_value(m.id),
                "{} diverges from the report composition",
                m.id
            );
        }
    }

    /// ...and the EXTENDED half of that composition is still `ExtendedMetrics`' own, with the tail
    /// confidence unmoved. The numbers must not have shifted when the roster did: a stored
    /// tearsheet's VaR changing silently would look exactly like a fix.
    #[test]
    fn the_extended_values_are_the_one_extended_composition() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let r = result_of(trades, eq, ts);
        let sheet = LiveTearsheet::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
        let ext = ExtendedMetrics::from_result(&r, DAILY_PERIODS_PER_YEAR);

        for m in METRICS.iter().filter(|m| m.home == MetricHome::Extended) {
            assert_eq!(sheet.metric(m.id), ext.value_of(m.id), "{} left the one composition", m.id);
        }
        assert_eq!(
            sheet.metric("value_at_risk_95"),
            Some(metrics::value_at_risk(&r.equity_curve, 0.95))
        );
        assert_eq!(
            sheet.metric("expected_shortfall_95"),
            Some(metrics::expected_shortfall(&r.equity_curve, 0.95))
        );
    }

    /// Every carried metric is finite over a trade set with both wins and losses, and the wiring
    /// matches the direct `metrics::` calls — the no-reimplementation check.
    #[test]
    fn every_stat_is_finite_and_matches_direct_metrics() {
        let (sheet, eq, _) = sample_sheet();
        for (spec, v) in sheet.rows() {
            if let Some(v) = v {
                assert!(v.is_finite(), "{} must be finite, got {v}", spec.id);
            }
        }
        let trades = sample_trades();
        assert_eq!(sheet.metric("n_trades"), Some(4.0));
        assert_eq!(sheet.metric("net_profit"), Some(metrics::net_profit(&trades)));
        assert_eq!(sheet.metric("win_rate"), Some(metrics::win_rate(&trades)));
        assert_eq!(sheet.metric("profit_factor"), Some(metrics::profit_factor(&trades)));
        assert_eq!(sheet.metric("sharpe"), Some(metrics::sharpe(&eq, DAILY_PERIODS_PER_YEAR)));
        assert_eq!(sheet.metric("max_drawdown"), Some(metrics::max_drawdown(&eq)));
        // net profit sanity: 10 - 4 + 6 - 2 = 10
        assert_eq!(sheet.metric("net_profit"), Some(10.0));
        // final equity = seed + Σ(pnl - fees) = 1000 + (10-.5)+(-4-.5)+(6-.3)+(-2-.2) = 1008.5
        assert!((sheet.metric("final_equity").unwrap() - 1_008.5).abs() < 1e-9);
    }

    /// ⚠ The journal door has no funding cashflow to fold, so it says so rather than publishing
    /// the `Default` `0.0` as a measurement. See `from_result_parts`' own doc.
    #[test]
    fn funding_paid_is_unrecorded_on_the_synthesized_door() {
        let (sheet, _, _) = sample_sheet();
        assert_eq!(sheet.metric("funding_paid"), None, "a fill stream measures no funding");
        let rendered = sheet.rendered_rows();
        let (_, text) = rendered.iter().find(|(id, _)| *id == "funding_paid").expect("the row");
        assert_eq!(text, NOT_RECORDED, "and it renders as words, never as 0.00");

        // ...while a REAL result's measured value survives.
        let mut r = result_of(sample_trades(), vec![1_000.0, 1_010.0], vec![1, 2]);
        r.funding_paid = -1.25;
        let measured = LiveTearsheet::from_result(None, &r, DAILY_PERIODS_PER_YEAR);
        assert_eq!(measured.metric("funding_paid"), Some(-1.25));
    }

    /// A report with NO `extended` block yields "not recorded" for every extended metric, never a
    /// tearsheet of zeros. This is the shape a `report.json` written before that block existed has,
    /// and the `crates/vike-cli` `--html` door will hold exactly such files.
    #[test]
    fn an_extendedless_report_renders_words_not_zeros() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let mut report =
            BacktestReport::from_result(None, &result_of(trades, eq, ts), DAILY_PERIODS_PER_YEAR);
        report.extended = None;
        let sheet = LiveTearsheet::from_report(&report);

        assert_eq!(sheet.metric("sortino"), None, "no extended block, no sortino");
        assert!(sheet.metric("sharpe").is_some(), "the compact scalars still answer");
        let rendered = sheet.rendered_rows();
        let (_, text) = rendered.iter().find(|(id, _)| *id == "sortino").expect("the row");
        assert_eq!(text, NOT_RECORDED);
    }

    #[test]
    fn display_is_non_empty_and_labeled() {
        let trades = sample_trades();
        let (eq, ts) = equity_curve_from_trades(1_000.0, &trades);
        let sheet = LiveTearsheet::from_result_parts(None, trades, eq, ts, DAILY_PERIODS_PER_YEAR);
        let s = sheet.to_string();
        assert!(!s.is_empty());
        assert!(s.contains("Live Tearsheet"));
        assert!(s.contains("(unnamed)"));
        assert!(s.contains(&format!("schema {TEARSHEET_SCHEMA}")));
        assert!(s.contains("sharpe:"));
        assert!(s.contains("win_rate:"));
        assert!(s.contains("max_drawdown:"));
        // The table renders through `MetricUnit::render`, so a PERCENT row carries the `%` the
        // four hand-rolled spellings kept forgetting.
        assert!(s.contains('%'), "percent rows must be scaled and suffixed:\n{s}");
    }

    /// ⚠ **The wire shape**: FLAT, one top-level key per catalog id, `schema` beside `name`.
    #[test]
    fn the_document_is_flat_and_carries_every_catalog_key() {
        let (sheet, _, _) = sample_sheet();
        let json = serde_json::to_string(&sheet).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = parsed.as_object().expect("a JSON object");

        assert_eq!(obj["schema"], TEARSHEET_SCHEMA);
        assert_eq!(obj["name"], "sess-1");
        for m in METRICS {
            assert!(obj.contains_key(m.id), "{} is missing from the document", m.id);
        }
        assert_eq!(
            obj.len(),
            METRICS.len() + 2,
            "no key beyond `schema`, `name` and the catalog: {json}"
        );
        // No nesting: the flat shape is what `vike_tradehub_client::proto`'s `Response::Tearsheet`
        // carries and what `vike-cli report --json` passes through verbatim.
        assert!(obj.values().all(|v| !v.is_object()), "no key may nest: {json}");
    }

    /// ⚠ A `Count` metric stays an INTEGER on the wire. `serde_json::Value`'s `PartialEq` does not
    /// consider `2.0` equal to `2`, so widening these to `f64` internally must not reach the bytes
    /// — `crates/vike-report/tests/tearsheet_cli.rs` asserts `json["n_trades"] == 2`.
    #[test]
    fn a_count_metric_serializes_as_an_integer() {
        let (sheet, _, _) = sample_sheet();
        let parsed: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&sheet).unwrap()).unwrap();
        for m in METRICS.iter().filter(|m| m.unit == MetricUnit::Count) {
            let v = &parsed[m.id];
            if v.is_null() {
                continue; // an unrecorded count is `null`, which is a different assertion
            }
            assert!(v.is_i64() || v.is_u64(), "{} must be an integer on the wire, got {v}", m.id);
        }
        assert_eq!(parsed["n_trades"], 4);
    }

    /// The document round-trips through the reader half — the property that makes `schema` worth
    /// declaring at all.
    #[test]
    fn serde_round_trips_through_the_reader() {
        let (sheet, _, _) = sample_sheet();
        let json = serde_json::to_string(&sheet).unwrap();
        let back: LiveTearsheet = serde_json::from_str(&json).expect("a writer's bytes parse");
        assert_eq!(back.schema, TEARSHEET_SCHEMA);
        assert_eq!(back.name.as_deref(), Some("sess-1"));
        for m in METRICS {
            // ⚠ NOT `assert_eq!(back, sheet)`: a non-finite value serializes as `null` and comes
            // back as `None`, so the `inf` house sentinel does NOT survive the wire. That is
            // pre-existing behaviour (serde_json has no `inf`), and stating it here is the point.
            match sheet.metric(m.id) {
                Some(v) if v.is_finite() => assert_eq!(back.metric(m.id), Some(v), "{}", m.id),
                _ => assert_eq!(back.metric(m.id), None, "{} was not finite", m.id),
            }
        }
    }

    /// Forward AND backward compatibility, both asserted: an id this build does not know is
    /// skipped, and one that is absent stays unrecorded rather than becoming `0.0`.
    #[test]
    fn an_unknown_key_is_skipped_and_a_missing_one_is_unrecorded() {
        let doc = r#"{"name":"old","sharpe":1.5,"a_metric_from_the_future":42.0}"#;
        let sheet: LiveTearsheet = serde_json::from_str(doc).expect("the document still parses");
        assert_eq!(sheet.schema, TEARSHEET_SCHEMA_UNVERSIONED, "no schema key -> unversioned");
        assert_eq!(sheet.metric("sharpe"), Some(1.5));
        assert_eq!(sheet.metric("sortino"), None, "absent is unrecorded, never 0.0");
        assert!(spec_for("a_metric_from_the_future").is_none(), "the unknown id really is unknown");
    }

    /// ⚠ **No catalog id may collide with this document's own two keys.** `#[serde(flatten)]`
    /// resolves the struct's OWN field names first, so a metric called `name` or `schema` would be
    /// shadowed on the way out and swallowed on the way back in — a metric that silently does not
    /// exist on the wire, which is the failure mode hardest to notice from either side. Nothing in
    /// the catalog is called either today; this is the test that says so rather than the reader who
    /// assumes it.
    #[test]
    fn no_catalog_id_collides_with_the_documents_own_keys() {
        for m in METRICS {
            assert!(
                m.id != "name" && m.id != "schema",
                "{} collides with a LiveTearsheet field and would be shadowed by flatten",
                m.id
            );
        }
    }

    /// An id the catalog does not hold cannot be written into the document — the property that
    /// keeps the key set equal to the published roster.
    #[test]
    fn setting_an_uncatalogued_id_writes_nothing() {
        let mut v = MetricValues::unrecorded();
        assert!(!v.set("shrapnel", Some(1.0)), "an unknown id is refused");
        assert!(v.set("sharpe", Some(1.0)), "a catalogued one is written");
        assert_eq!(v.get("sharpe"), Some(1.0));
        assert_eq!(v.get("shrapnel"), None);
    }
}
