//! `LiveTearsheet` — the assembled live performance summary, **keyed on the METRIC CATALOG**.
//!
//! Composes reconstructed [`Trade`]s + an equity curve into a `vike_analytics::BacktestResult`,
//! folds that through `vike_analytics::BacktestReport` — the one assembly site — and then carries
//! every `vike_analytics::metric_catalog::METRICS` id it answers. This module does NO metric math
//! of its own, no metric ASSEMBLY of its own, and — since the taxonomy work — **no metric ROSTER of
//! its own either**.
//!
//! # ⚠ It lived in `vike-report` until 2026-09-28, as that crate's `report.rs`
//!
//! It moved here with the renderer ([`crate::html`]) and the pure folds ([`crate::trades`],
//! [`crate::equity`], [`crate::mtm`]) because none of it reads a journal or a store: it names
//! `vike_model` and this crate and nothing else, so it belongs with the numbers it carries. What
//! stayed behind is exactly the half that reads — the journal door, formerly an inherent
//! `LiveTearsheet::from_journal` and now the free function `vike_report::tearsheet_from_journal`,
//! because an inherent `impl` may not be written on a type another crate owns. So a consumer that
//! only renders a document it already holds (`crates/vike-cli/src/cmd/runs/show.rs`'s
//! `html_document`) names this crate and links no journal reader at all.
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
//! `crates/vike-cli/src/cmd/report/schema.rs`'s module doc declared the hole this closes — "it does
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
use vike_model::Trade;

use crate::metric_catalog::{METRICS, MetricSpec, MetricUnit};
use crate::report::BacktestReport;
use crate::result::BacktestResult;

// ⚠ `pub use vike_analytics::DAILY_PERIODS_PER_YEAR;` stood here while this module was
// vike-report's `report.rs` — a SECOND name for `crate::report::DAILY_PERIODS_PER_YEAR`, argued as
// "not a second copy of `252.0`". A copy it was not; a second NAME it was, and the owner's
// 2026-09-18 ruling (a symbol has ONE name) retired it with the move. Callers spell the constant
// `vike_analytics::DAILY_PERIODS_PER_YEAR`. The one sentence of the alias's doc that was about THIS
// type — a fill stream has no fixed period — now sits on `LiveTearsheet::from_result_parts`.

/// The version [`LiveTearsheet`] declares in its `schema` key. `1` is the first shape that carries
/// one at all.
///
/// Bump it for any change a consumer could not read: a REMOVED or RETYPED key, or — because the
/// key set is the contract and not the key order — a key set that GROWS. That is the same rule
/// `crates/vike-cli/src/cmd/report/schema.rs`'s `REPORT_SCHEMA` states for the stored-run
/// document, and it is deliberately the same rule: these two documents are read by the same
/// operators through the same verb.
///
/// ⚠ The key set here grows when `vike_analytics::metric_catalog::METRICS` grows, which is a bump
/// owed HERE for an edit made in another module. (This read "in a file this crate does not own"
/// while the module lived in vike-report; the catalog is a sibling module since the 2026-09-28
/// move, and the bump is still owed by whoever edits the catalog.) That is deliberate rather than
/// a hazard: the catalog is the roster for the whole workspace, so one addition there is one
/// version bump here, and the alternative (a hand-kept field list that silently does NOT grow) is
/// the defect this type was restructured to remove.
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
/// `0`): `crates/vike-cli/src/cmd/report/schema.rs`'s `NULL_CONVENTION` is the published rule and
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
    /// ⚠ Stays a public MUTABLE field: `vike_report::tearsheet_cli`'s `run` assigns `--name` after
    /// the sheet is built, because the label is an operator's word and not a property of the fills.
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
    /// annualization factor. A pure fill stream has no fixed period, so the caller passes whatever
    /// cadence its equity samples represent; [`crate::DAILY_PERIODS_PER_YEAR`] (252) is the default
    /// every door onto this type uses.
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
    /// this is the whole conversion, and it needs no journal, no store and no equity curve. That is
    /// why it lives in this crate: it compiled without vike-report's old `journal` feature, and
    /// now that the journal door is the only thing vike-report keeps, a caller of this one links no
    /// journal reader at all.
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
    /// numbers**, shared by the `Display` table and [`crate::html`].
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

#[path = "tearsheet_tests.rs"]
#[cfg(test)]
mod tearsheet_tests;
