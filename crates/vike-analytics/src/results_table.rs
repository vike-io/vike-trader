//! `TradingResultsTable` — the assembled live performance summary, **keyed on the METRIC CATALOG**.
//!
//! Composes reconstructed [`Trade`]s + an equity curve into a `vike_analytics::TradingRecord`,
//! folds that through `vike_analytics::results::TradingResults` — the one assembly site — and then carries
//! every `vike_analytics::metric_catalog::METRICS` id it answers. This module does NO metric math
//! of its own, no metric ASSEMBLY of its own, and — since the taxonomy work — **no metric ROSTER of
//! its own either**.
//!
//! # ⚠ It lived in `vike-report` until 2026-09-28, as that crate's `report.rs`
//!
//! It moved here with the renderer ([`crate::html`]) and the pure folds ([`crate::trades`],
//! [`crate::equity`], [`crate::mtm`]) because none of it reads a journal or a store: it names
//! `vike_model` and this crate and nothing else, so it belongs with the numbers it carries. What
//! stayed behind was exactly the half that reads — the journal door, formerly an inherent
//! `TradingResultsTable::from_journal`, then a free function in `vike-report`. `vike-report` was
//! dissolved on 2026-10-12: the journal READ is `vike_journal::fills_from_journal`, and the fold
//! from fills to a document is [`trading_results_table_from_fills`] below, the ONE spelling of it (the daemon,
//! the CLI and the desktop each hand it the fills they hold). So a consumer that only renders a
//! document it already holds (`crates/vike-cli/src/cmd/backtest/read/show.rs`'s `html_document`) names this
//! crate and links no journal reader at all.
//!
//! # ⚠ Why the fields went away
//!
//! This type declared TWENTY-FIVE `pub` numeric fields, and that list was a SECOND TRUTH about
//! which performance numbers this workspace has. It was measurably a different truth — MEASURED
//! 2026-09-16, the day the fields came out, and a historical count rather than a standing one
//! (`METRICS.len()` is the only authority for today's, and `the_catalog_is_the_roster` below is
//! what holds this type equal to it): the catalog held 38 ids, `TradingResults::from_record`
//! computed every one of them, and this type copied 25 —
//! **it threw thirteen already-computed numbers away** (`funding_paid` off the compact scalars,
//! and `long_ratio`, `mar_ratio`, `recovery_factor`, `ulcer_index`, `ulcer_performance_index`,
//! `k_ratio`, `risk_return_ratio`, `returns_volatility`, `returns_skewness`, `returns_kurtosis`,
//! `tail_ratio` and `omega` out of `TradingResultsExtendedMetrics`; `mar_ratio`, the same formula as `calmar`,
//! has since left the catalog). Not because anybody judged them
//! uninteresting — because a hand-typed field list cannot grow when the catalog does.
//! `vike_analytics::metric_catalog`'s own module doc names three hand copies of that kind and this
//! file's was one of them; [`TradingResultsTableMetricValues`] is the answer, and its declaration order IS the render
//! order because it IS `METRICS`.
//!
//! The owner's ruling that shaped this: **the numbers become one roster and the two SHAPES stay.**
//! So there is still a text table ([`TradingResultsTable`]'s `Display`) and still an HTML document
//! ([`crate::html`]) — two presentations, one roster, and each cell rendered by
//! `vike_analytics::metric_catalog::MetricUnit::render` rather than by a local `format!`.
//!
//! # ⚠ This is a WIRE DOCUMENT, so the break is NAMED
//!
//! `vike-cli report account: --json` and the MCP tool `report` answer this type as JSON (the tradehub's
//! `Response::Tearsheet` carried it as TEXT until that verb was retired on 2026-11-24), so the FLAT shape is a
//! contract: one top-level key per metric, exactly as before. What changed is which keys exist (13
//! more) and the addition of [`TradingResultsTable::schema`].
//!
//! `crates/vike-cli/src/cmd/report/schema.rs`'s module doc declared the hole this closes — "it does
//! **not** cover the LIVE tearsheet a `vike-tradehub` node answers with … That is a declared hole
//! rather than an oversight — closing it means versioning the document in `vike-report`, beside its
//! producer." (`vike-report` is gone; the document and its producer are both in this module.) This
//! is that. The field is spelled `schema` and not `schema_version` for the reason
//! that file argues at length: `vike_runs::RunSeries` and `RunTrades` spell it `schema`, and
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
use vike_model::events::FillEvent;
use vike_model::{ContractValue, Trade};

use crate::equity::{daily_equity_curve, equity_curve_from_trades};
use crate::metric_catalog::{METRICS, MetricHome, MetricSpec, MetricUnit};
use crate::record::TradingRecord;
use crate::results::TradingResults;
use crate::trades::reconstruct_trades;

// ⚠ `pub use vike_analytics::DAILY_PERIODS_PER_YEAR;` stood here while this module was
// vike-report's `report.rs` — a SECOND name for `crate::results::DAILY_PERIODS_PER_YEAR`, argued as
// "not a second copy of `252.0`". A copy it was not; a second NAME it was, and the owner's
// 2026-09-18 ruling (a symbol has ONE name) retired it with the move. Callers spell the constant
// `vike_analytics::results::DAILY_PERIODS_PER_YEAR`. The one sentence of the alias's doc that was
// about THIS type — a fill stream has no fixed period — now sits on
// `TradingResultsTable::from_result_parts`.

/// The version [`TradingResultsTable`] declares in its `schema` key. `1` is the first shape that carries
/// one at all; `2` (2026-11-09) has no `mar_ratio` key, which was `calmar` under a second name.
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
pub const TRADING_RESULTS_TABLE_SCHEMA: u32 = 2;

/// What an ABSENT `schema` key deserializes to — a document written before the version existed.
///
/// It is not an error: every tearsheet a node emitted before this field shipped is a valid
/// document, and refusing to parse one would make the version's arrival a breaking change for
/// readers instead of a legible one. A reader comparing against [`TRADING_RESULTS_TABLE_SCHEMA`] sees `0` and
/// knows exactly what it is holding.
pub const TRADING_RESULTS_TABLE_SCHEMA_UNVERSIONED: u32 = 0;

/// The text a renderer prints for a metric this document does not carry.
///
/// ⚠ **Not `0.0`, and that is the whole point of the distinction.** A `None` means THIS DOCUMENT
/// DID NOT RECORD IT, which is a different answer from a zero — rendering an unrecorded Sortino as
/// `0.0000` publishes "this strategy had no downside". `vike_analytics::metric_catalog::MetricHome`
/// argues the same case from the report side, and `TradingResults::render_metrics` prints its own
/// sentence for it; this constant is that convention on the tearsheet's two renderers.
pub const NOT_RECORDED: &str = "not recorded";

/// The label every renderer gives a run whose equity reached zero ([`TradingResultsTable::invalid`]): the first row
/// of its metrics, then only its money and counts, and the Studio's Validation, Distribution, sweep and
/// compare cells.
pub const INVALID_LABEL: &str = "INVALID";

/// Whether an INVALID run ([`INVALID_LABEL`]) still shows `spec`: only its `Money` and `Count` rows, because a
/// return, ratio or probability over a curve that crossed zero means nothing. The ONE spelling of that rule, for
/// [`TradingResultsTable::rendered_rows`] and `crate::results_comparison`.
pub(crate) fn shown_on_an_invalid_run(spec: &MetricSpec) -> bool {
    use crate::metric_catalog::MetricUnit;
    matches!(spec.unit, MetricUnit::Money | MetricUnit::Count)
}

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
/// `TradingResults` with no `extended` block (`TradingResults::metric_value` answers `None` for
/// every extended id there, and this type carries that answer through rather than substituting a
/// number), and the one deliberate case in [`TradingResultsTable::from_result_parts`].
#[derive(Debug, Clone, PartialEq)]
pub struct TradingResultsTableMetricValues {
    /// One slot per `METRICS` row, in declaration order. ⚠ INVARIANT: `values.len() ==
    /// METRICS.len()`. Every constructor here holds it; the accessors are written to survive a
    /// short vector anyway (reading past the end answers `None` instead of panicking), because
    /// this type is deserialized from bytes another process wrote.
    values: Vec<Option<f64>>,
}

impl Default for TradingResultsTableMetricValues {
    /// Every catalog metric declared UNRECORDED.
    ///
    /// ⚠ Hand-written rather than derived: `#[derive(Default)]` would produce an EMPTY vector,
    /// which silently breaks this type's one invariant and makes every metric read `None` for the
    /// wrong reason.
    fn default() -> Self {
        Self { values: vec![None; METRICS.len()] }
    }
}

impl TradingResultsTableMetricValues {
    /// Every catalog metric declared UNRECORDED — the starting point a filler writes into.
    #[must_use]
    pub fn unrecorded() -> Self {
        Self::default()
    }

    /// Fill every slot by asking `f` for each catalog id, in declaration order.
    ///
    /// This is the ONE filler, and `f` returning `Option` is what keeps "not recorded" expressible
    /// all the way from the source document — see [`TradingResultsTable::from_report`], whose `f` is
    /// `TradingResults::metric_value` verbatim.
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

    /// Every catalog row paired with its value, in declaration (== render) order — except a
    /// [`MetricHome::ClosedTrades`] row with no value, which appears only where a run armed that
    /// block. So a document that carries none (every live one) keeps its key set, its table and its
    /// HTML exactly as before those rows existed, and [`TRADING_RESULTS_TABLE_SCHEMA`] does not move.
    pub fn rows(&self) -> impl Iterator<Item = (&'static MetricSpec, Option<f64>)> + '_ {
        METRICS
            .iter()
            .enumerate()
            .map(|(i, m)| (m, self.values.get(i).copied().flatten()))
            .filter(|(m, v)| m.home != MetricHome::ClosedTrades || v.is_some())
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
/// `usize` fields, so every existing consumer of this document reads them as integers, and
/// `serde_json::Value`'s `PartialEq` does NOT consider `2.0` equal to `2`. Widening them to `f64`
/// internally is what lets the roster be one type; the wire must not pay for that.
impl Serialize for TradingResultsTableMetricValues {
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
impl<'de> Deserialize<'de> for TradingResultsTableMetricValues {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = TradingResultsTableMetricValues;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a map of vike_analytics::metric_catalog ids to numbers")
            }

            fn visit_map<M: MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<TradingResultsTableMetricValues, M::Error> {
                let mut out = TradingResultsTableMetricValues::unrecorded();
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
/// `vike_analytics::results::TradingResults` — see the module doc for why this type holds no numeric fields
/// of its own any more.
///
/// NB: some metrics answer `f64::INFINITY` for degenerate inputs (e.g. `profit_factor` with no
/// losing trades) — the house 0.0/inf sentinel convention of `vike_analytics::metrics`. Those are
/// preserved verbatim. `--json` stays VALID on such a run (serde_json writes a non-finite float as
/// `null`, it does not fail), but a consumer must expect `null` where it wanted a number; a real
/// trade history with both wins and losses yields finite metrics throughout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradingResultsTable {
    /// The document's shape version — [`TRADING_RESULTS_TABLE_SCHEMA`] from every constructor here.
    ///
    /// `#[serde(default)]` so a document written before the field existed parses as
    /// [`TRADING_RESULTS_TABLE_SCHEMA_UNVERSIONED`] rather than failing; that constant's doc argues why a
    /// refusal would be the wrong answer.
    #[serde(default)]
    pub schema: u32,
    /// Free-form label for this tearsheet (e.g. a session or account id).
    ///
    /// ⚠ Stays a public MUTABLE field: `crates/vike-cli/src/cmd/report.rs`'s `live` assigns `--name` after the sheet
    /// is built, because the label is not a property of the fills it folded.
    #[serde(default)]
    pub name: Option<String>,
    /// The run is INVALID: its equity reached zero (`crate::results::TradingResults::invalid`'s words, naming the
    /// day). `Some` only then and skipped when `None`, so every other document is byte-identical.
    /// [`Self::rendered_rows`] opens with it and keeps only the money and count rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid: Option<String>,
    /// The numbers, keyed on `vike_analytics::metric_catalog::METRICS` and serialized FLAT — see
    /// [`TradingResultsTableMetricValues`] and its `Serialize` impl.
    #[serde(flatten)]
    pub metrics: TradingResultsTableMetricValues,
}

impl TradingResultsTable {
    /// Assemble a tearsheet from reconstructed trades + an aligned equity curve.
    ///
    /// Builds a `TradingRecord` (the same shape the backtest produces) and fills every catalog
    /// metric through [`Self::from_result`]. `periods_per_year` is the Sharpe/Sortino/Calmar/CAGR
    /// annualization factor. A pure fill stream has no fixed period, so the caller passes whatever
    /// cadence its equity samples represent: [`trading_results_table_from_fills`] hands a curve of one sample per
    /// calendar day, annualised by [`crate::results::periods_per_year_for_days`].
    ///
    /// # ⚠ `funding_paid` is declared NOT RECORDED on this door, not carried as `0.0`
    ///
    /// The `TradingRecord` built here is SYNTHESIZED from a fill stream, and its `funding_paid`
    /// is therefore `Default`'s `0.0` — a field nobody filled, not a measurement. Publishing that
    /// as a number tells an operator of a live PERP account that funding cost them nothing, which
    /// is a claim this door has no evidence for; `None` says what is true, that the reconstruction
    /// folds no funding cashflow at all. [`Self::from_result`] over a REAL `TradingRecord` keeps
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
        let r = TradingRecord {
            trades,
            equity_curve,
            final_equity,
            n_trades,
            equity_timestamp: equity_ts,
            ..Default::default()
        };
        let mut sheet = Self::from_result(name, &r, periods_per_year);
        sheet.metrics.set("funding_paid", None);
        sheet
    }

    /// Compute the tearsheet from a `TradingRecord` — the reuse seam.
    ///
    /// ⚠ It is now a one-line function, and that is the point: it composes
    /// `vike_analytics::results::TradingResults` (which itself composes `TradingResultsExtendedMetrics`) and reads the
    /// result through [`Self::from_report`]. There is no second assembly, no second
    /// `periods_per_year` argument and no second tail confidence anywhere in this crate, so a live
    /// tearsheet and a stored `report.json` cannot print different Sortinos for one set of fills —
    /// the property the old F31 invariant asserted field-by-field and can now be read off the
    /// call graph.
    pub fn from_result(name: Option<String>, r: &TradingRecord, periods_per_year: f64) -> Self {
        Self::from_report(&TradingResults::from_record(name, r, periods_per_year))
    }

    /// Read a tearsheet off an already-composed `TradingResults` — **the door a STORED run comes
    /// through.**
    ///
    /// `crates/vike-cli/src/cmd/backtest/read/show.rs` holds a `report.json` and wants the HTML tearsheet;
    /// this is the whole conversion, and it needs no journal, no store and no equity curve. That is
    /// why it lives in this crate: it compiled without vike-report's old `journal` feature, and a
    /// caller of this one links no journal reader at all.
    ///
    /// Every value is `TradingResults::metric_value(id)` — including its `None`s. A report with no
    /// `extended` block (one written before that block existed) yields a tearsheet that says
    /// [`NOT_RECORDED`] for every extended metric rather than a tearsheet of zeros, which is the
    /// distinction `vike_analytics::metric_catalog::MetricSelection::needs_extended` exists to make
    /// askable one layer up.
    pub fn from_report(report: &TradingResults) -> Self {
        TradingResultsTable {
            schema: TRADING_RESULTS_TABLE_SCHEMA,
            name: report.name.clone(),
            invalid: report.invalid.clone(),
            metrics: TradingResultsTableMetricValues::from_fn(|id| report.metric_value(id)),
        }
    }

    /// The value of one catalog metric — `None` when this document does not carry it (or when the
    /// catalog holds no such id; see [`TradingResultsTableMetricValues::get`] for why the two collapse).
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
    ///
    /// An INVALID run ([`Self::invalid`]) opens with an [`INVALID_LABEL`] row of its words and keeps only the
    /// `Money` and `Count` rows: a return, ratio or probability over a curve that crossed zero means nothing.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<(&'static str, String)> {
        let rows = self
            .rows()
            .filter(|(spec, _)| self.invalid.is_none() || shown_on_an_invalid_run(spec))
            .map(|(spec, v)| {
                let text = match v {
                    Some(v) => spec.unit.render(v),
                    None => NOT_RECORDED.to_string(),
                };
                (spec.id, text)
            });
        self.invalid.iter().map(|why| (INVALID_LABEL, why.clone())).chain(rows).collect()
    }
}

/// How [`trading_results_table_from_fills`] orders the fills it is handed before it folds them.
///
/// Two callers, two answers, and both are kept exactly as they were when the fold was written
/// three times (`vike-report`'s two functions and the `tearsheet` CLI's inline copy): a journal
/// reader hands over the fills in JOURNAL order and gets them folded untouched, a reader that
/// holds rows of its own order (the desktop holds them newest-first) asks for time order.
/// ⚠ They differ on any stream whose `ts` is not ascending in the order it is held — an
/// out-of-order fill is a different position path, so a different trade list — which is why the
/// order is an explicit argument and not a default either door picks for the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TradingResultsTableFillOrder {
    /// Fold the fills exactly as handed over. The journal doors: the journal's record order is the
    /// order `vike_execution::Account::fold` saw them in, and the fold must not second-guess it.
    AsHeld,
    /// Stable-sort by `FillEvent::ts` first. The desktop's door, over rows read from the datahub.
    ByTime,
}

/// THE fold from fills to a tearsheet: `reconstruct_trades` → `equity_curve_from_trades` → one
/// sample per UTC calendar day (`crate::equity`'s `daily_equity_curve`) →
/// [`TradingResultsTable::from_result_parts`], with realized-only equity from `seed`.
///
/// ⚠ **Every curve row is read off the DAILY curve** (Sharpe, Sortino, CAGR, Calmar, max drawdown,
/// the moments, VaR), so `periods_per_year` counts DAYS: a door with no operator value passes
/// [`crate::results::periods_per_year_for_days`] (365 for a 24/7 book, 252 for a session market).
/// Until 2026-11-09 the curve had one point per closed trade and was annualised at 252, so every
/// trade read as a trading day; the ledger rows never read the curve and did not move.
///
/// Returns the document PLUS the `(equity, ts)` curve it was assembled from, because the flat
/// document does not retain the curve and `render_html` plots it (the `tearsheet` CLI's `--html`);
/// a caller that wants only the document takes `.0`. A free function, not a `TradingResultsTable`
/// method, only because this is the crate that owns the type and the callers' vocabulary is the
/// crate root: `vike_analytics::trading_results_table_from_fills`.
///
/// Where the fills come from is the caller's: `vike_journal::fills_from_journal` for a command
/// journal (the tradehub daemon, the CLI; `order` is [`TradingResultsTableFillOrder::AsHeld`]), or the desktop's
/// stored `kind=exec_fill` rows converted by `vike_data::ExecFillRow::fill_events`
/// ([`TradingResultsTableFillOrder::ByTime`]). This crate reads neither.
pub fn trading_results_table_from_fills(
    mut fills: Vec<FillEvent>,
    order: TradingResultsTableFillOrder,
    seed: f64,
    periods_per_year: f64,
) -> (TradingResultsTable, Vec<f64>, Vec<i64>) {
    if order == TradingResultsTableFillOrder::ByTime {
        fills.sort_by_key(|f| f.timestamp);
    }
    let trades = reconstruct_trades(&fills);
    let (equity, ts) = equity_curve_from_trades(seed, &trades);
    trading_results_table_from_curve(trades, &equity, &ts, periods_per_year)
}

/// The sheet of `trades` over an `(equity, ts)` curve the caller already holds, read ONE CLOSE PER
/// UTC CALENDAR DAY (`crate::equity`'s `daily_equity_curve`) like every live sheet, so
/// `periods_per_year` counts days ([`crate::results::periods_per_year_for_days`]).
///
/// [`trading_results_table_from_fills`] hands it its realized curve; the `tearsheet` CLI's `--datahub` path
/// hands it the curve it read (the sampler's stored series, the mark-to-market reconstruction or
/// the realized fallback), which until 2026-11-09 it annualised at 252 sample by sample. Returns
/// the daily curve beside the document, because `render_html` plots it.
pub fn trading_results_table_from_curve(
    trades: Vec<Trade>,
    equity: &[f64],
    ts: &[i64],
    periods_per_year: f64,
) -> (TradingResultsTable, Vec<f64>, Vec<i64>) {
    let (equity, ts) = daily_equity_curve(equity, ts);
    let sheet = TradingResultsTable::from_result_parts(
        None,
        trades,
        equity.clone(),
        ts.clone(),
        periods_per_year,
    );
    (sheet, equity, ts)
}

/// **The sentence a tearsheet door refuses with** when the fills it was handed include symbols quoted
/// in a currency other than the dollar (decision 0183, item 13). `list` names the offenders with their
/// quotes, as `vike_catalog::foreign_fiat_quoted_list` renders them (`"USD_JPY (JPY)"`).
///
/// The fold ([`trading_results_table_from_fills`]) reads every fill as an amount of ONE currency: a fill carries
/// no quote currency and the sheet holds no conversion rate, so 100,000 USDJPY closed 0.10 higher
/// would print as +10,000 where the truth, at 150.10, is 66.62 USD. Every money metric and every curve
/// metric is wrong by the rate, in the dangerous direction for a yen or a pound. This crate sits
/// below the symbol vocabulary and cannot tell a yen symbol from a dollar one, so each door asks
/// `vike_catalog` and, when it is told, refuses with these words instead of folding.
#[must_use]
pub fn trading_results_table_foreign_quote_refusal(list: &str) -> String {
    format!(
        "tearsheet refused: the fills include {list}, quoted in a currency other than USD. The sheet \
         reads every fill as dollars and holds no conversion rate, so it would report that \
         currency's PnL, fees and drawdown as dollars (100,000 USDJPY closed 0.10 higher is +10,000 \
         JPY, which is 66.62 USD at 150.10, not +10,000 USD). The engine's own equity curve is in \
         the account's currency; read that instead"
    )
}

/// One venue whose engine books in a currency that is not the dollar, and how many of its fills a sheet left
/// out ([`without_non_dollar_ledgers`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradingResultsTableLeftOutLedger {
    /// The venue the journal's snapshots record an engine of, in a non-dollar ledger.
    pub venue: String,
    /// That ledger's currency, upper-cased (`"EUR"`).
    pub currency: String,
    /// How many of the journal's fills are on that venue (and so were left out).
    pub fills: usize,
}

/// **Leave the fills of a non-dollar ledger out of a sheet, and say so** (decision 0183, item 13). The fold
/// ([`trading_results_table_from_fills`]) reads every fill as an amount of ONE currency (the dollar) and a fill carries no
/// currency, so a euro-ledger account's EURUSD fills would be read as dollars: the price PnL in USD, the fees in
/// EUR, one sheet of two currencies. `ledgers` is what the live snapshot states
/// (`vike_tradehub_client::fold_guards::off_dollar_ledgers`: `(venue, currency)` per engine whose ledger is not the
/// dollar); a fill of such a venue is not folded, and the returned [`TradingResultsTableLeftOutLedger`]s count what was left out so
/// the door can print [`trading_results_table_ledger_label`]. ⚠ It works on the VENUE, because a fill names no account: a
/// venue that runs a dollar account beside a euro one loses both accounts' fills (fail closed). No ledgers, which
/// is every journal of one currency, returns the fills untouched and an empty list.
#[must_use]
pub fn without_non_dollar_ledgers(
    fills: Vec<FillEvent>,
    ledgers: &[(String, String)],
) -> (Vec<FillEvent>, Vec<TradingResultsTableLeftOutLedger>) {
    if ledgers.is_empty() {
        return (fills, Vec::new());
    }
    let mut left: Vec<TradingResultsTableLeftOutLedger> = Vec::new();
    let kept = fills
        .into_iter()
        .filter(|f| {
            let Some((venue, currency_value)) =
                ledgers.iter().find(|(v, _)| v.as_str() == f.venue.as_str())
            else {
                return true;
            };
            match left.iter_mut().find(|l| l.venue == *venue) {
                Some(l) => l.fills += 1,
                None => left.push(TradingResultsTableLeftOutLedger {
                    venue: venue.clone(),
                    currency: currency_value.clone(),
                    fills: 1,
                }),
            }
            false
        })
        .collect();
    (kept, left)
}

/// The sentence a sheet that left fills out carries (its `name`, and stderr at the CLI):
/// `leaves out 12 EUR-ledger fills of fxcm: ...`. Empty `left_out` has no label (the caller checks).
#[must_use]
pub fn trading_results_table_ledger_label(left_out: &[TradingResultsTableLeftOutLedger]) -> String {
    let parts: Vec<String> = left_out
        .iter()
        .map(|l| {
            format!(
                "{} {}-ledger fill{} of {}",
                l.fills,
                l.currency,
                if l.fills == 1 { "" } else { "s" },
                l.venue
            )
        })
        .collect();
    format!(
        "leaves out {}: a sheet adds one currency and those books are not in dollars",
        parts.join(", ")
    )
}

/// One contract whose value a sheet cannot read, and how many of its fills the sheet left out
/// ([`without_non_default_contracts`]).
#[derive(Debug, Clone, PartialEq)]
pub struct TradingResultsTableLeftOutContract {
    /// The venue the journal's snapshots record an engine of, valuing the contract so.
    pub venue: String,
    /// The contract's symbol.
    pub symbol: String,
    /// Its kind and per-unit factor.
    pub value: ContractValue,
    /// How many of the journal's fills are on it (and so were left out).
    pub fills: usize,
}

/// **Leave the fills of a contract the fold cannot value out of a sheet, and say so** (decision 0243, item 6). The fold
/// ([`trading_results_table_from_fills`]) reads every fill as a linear unit worth its price (`crate::trades::MULTIPLIER`) and a
/// fill carries no contract value, so 1,000 USD of inverse `BTC-PERPETUAL` from 60,000 to 66,000 reads +6,000,000 where
/// the account booked +100, a coin fee enters as dollars, and a linear multiplier of 50 reads 50 times too small.
/// `contracts` is what the live snapshot states (`vike_tradehub_client::fold_guards::off_default_contracts`:
/// `(venue, symbol, value)` per value that is not the default). ⚠ By `(venue, symbol)`, because a fill names no
/// account: an account of the venue that values the symbol so takes the others' fills of it with it (fail closed). No
/// contract, which is every journal of default contracts, returns the fills untouched and an empty list.
#[must_use]
pub fn without_non_default_contracts(
    fills: Vec<FillEvent>,
    contracts: &[(String, String, ContractValue)],
) -> (Vec<FillEvent>, Vec<TradingResultsTableLeftOutContract>) {
    if contracts.is_empty() {
        return (fills, Vec::new());
    }
    let by_key: std::collections::HashMap<(&str, &str), ContractValue> =
        contracts.iter().map(|(v, s, value)| ((v.as_str(), s.as_str()), *value)).collect();
    let mut left: Vec<TradingResultsTableLeftOutContract> = Vec::new();
    let kept = fills
        .into_iter()
        .filter(|f| {
            let (venue, symbol) = (f.venue.as_str(), f.symbol.as_str());
            let Some(&value) = by_key.get(&(venue, symbol)) else {
                return true;
            };
            match left.iter_mut().find(|l| l.venue == venue && l.symbol == symbol) {
                Some(l) => l.fills += 1,
                None => left.push(TradingResultsTableLeftOutContract {
                    venue: venue.to_string(),
                    symbol: symbol.to_string(),
                    value,
                    fills: 1,
                }),
            }
            false
        })
        .collect();
    (kept, left)
}

/// The sentence a sheet that left a contract out carries (its `name`, and stderr at the CLI):
/// `leaves out 2 fills of deribit BTC-PERPETUAL (inverse, 1 a unit): ...`. Empty `left_out` has no label (the caller
/// checks).
#[must_use]
pub fn trading_results_table_contract_label(
    left_out: &[TradingResultsTableLeftOutContract],
) -> String {
    let parts: Vec<String> = left_out
        .iter()
        .map(|l| {
            let kind = l.value.kind.word();
            format!(
                "{} fill{} of {} {} ({kind}, {} a unit)",
                l.fills,
                if l.fills == 1 { "" } else { "s" },
                l.venue,
                l.symbol,
                l.value.per_unit
            )
        })
        .collect();
    format!(
        "leaves out {}: a sheet reads every fill as a linear unit worth its price and cannot value those contracts",
        parts.join(", ")
    )
}

/// The fees a sheet's fills paid in ONE asset that is not their own money ([`without_fees_in_other_assets`]).
#[derive(Debug, Clone, PartialEq)]
pub struct TradingResultsTableOtherAssetFee {
    /// The fee's asset, upper-cased (`"BNB"`).
    pub asset: String,
    /// The fees in it, as the venue stated them: NOT converted (a fill row carries no price for it).
    pub amount: f64,
    /// How many fills paid it.
    pub fills: usize,
}

/// **Take the fees a fill paid in ANOTHER asset off the fill, and count them per asset.** The fold reads every
/// commission as an amount of the fill's quote money, so a fee of 0.01 BNB on a USDT fill would count as 0.01
/// USDT in `total_fees`, in every trade's net and in the realized curve. A fill whose fee `in_the_fills_money`
/// refuses keeps its quantity and price and loses its commission, which the returned rows carry, one per asset in
/// first-seen order, for [`trading_results_table_other_asset_fee_label`]. No conversion price is invented: the
/// fill rows carry none. The rule is the caller's (`vike_catalog::fee_in_the_fills_money`, the symbol vocabulary
/// this crate may not name).
#[must_use]
pub fn without_fees_in_other_assets(
    mut fills: Vec<FillEvent>,
    in_the_fills_money: impl Fn(&FillEvent) -> bool,
) -> (Vec<FillEvent>, Vec<TradingResultsTableOtherAssetFee>) {
    let mut other: Vec<TradingResultsTableOtherAssetFee> = Vec::new();
    for fill in fills.iter_mut().filter(|f| f.commission != 0.0 && !in_the_fills_money(f)) {
        let asset = fill.commission_asset.as_str().to_ascii_uppercase();
        match other.iter_mut().find(|o| o.asset == asset) {
            Some(o) => {
                o.amount += fill.commission;
                o.fills += 1;
            }
            None => other.push(TradingResultsTableOtherAssetFee {
                asset,
                amount: fill.commission,
                fills: 1,
            }),
        }
        fill.commission = 0.0;
    }
    (fills, other)
}

/// The sentence a sheet that paid fees in other assets carries: `fees in other assets: 0.0123 BNB (not converted;
/// left out of total_fees and the realized curve)`. Each amount to eight decimals at most, its trailing zeros dropped.
/// Empty `fees` has no label (the caller checks).
#[must_use]
pub fn trading_results_table_other_asset_fee_label(
    fees: &[TradingResultsTableOtherAssetFee],
) -> String {
    let amount = |v: f64| {
        let text = format!("{v:.8}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    let parts: Vec<String> =
        fees.iter().map(|f| format!("{} {}", amount(f.amount), f.asset)).collect();
    format!(
        "fees in other assets: {} (not converted; left out of total_fees and the realized curve)",
        parts.join(", ")
    )
}

/// The whole of what a sheet left out, for its `name`: [`trading_results_table_ledger_label`] and [`trading_results_table_contract_label`],
/// `None` when it left out nothing.
#[must_use]
pub fn trading_results_table_left_out_label(
    ledgers: &[TradingResultsTableLeftOutLedger],
    contracts: &[TradingResultsTableLeftOutContract],
) -> Option<String> {
    let ledger = (!ledgers.is_empty()).then(|| trading_results_table_ledger_label(ledgers));
    let contract = (!contracts.is_empty()).then(|| trading_results_table_contract_label(contracts));
    match (ledger, contract) {
        (Some(ledger), Some(contract)) => Some(format!("{ledger}; {contract}")),
        (ledger, contract) => ledger.or(contract),
    }
}

/// The TEXT shape — one of the two the owner's ruling preserved.
///
/// The label column is the widest catalog id, so a reader's eye and a `cut -c` both find the values
/// in one place, and the rows are `METRICS` in declaration order. The header carries the document
/// version: a human diffing two tearsheets needs to know whether the row SETS are comparable, and
/// the metric table is not the place to answer a question about the document.
impl fmt::Display for TradingResultsTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = METRICS.iter().map(|m| m.id.len()).max().unwrap_or(0) + 1;
        writeln!(
            f,
            "=== Trading results: {} (schema {}) ===",
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

#[path = "results_table_tests.rs"]
#[cfg(test)]
mod results_table_tests;
