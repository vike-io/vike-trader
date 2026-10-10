//! The context: [`StudyContext`], everything a study is handed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_data::{CohortRow, DataError, HistStore, PerpMetricRow, TsRange};
use vike_model::{Bar, BookUpdate, QuoteTick, TradeTick};

#[cfg(doc)]
use super::StudyError;
use super::{StudyLearner, StudySim};

/// Everything a study is HANDED. Built by the caller — a binary, which is the only layer allowed
/// to read the environment — and never by a study.
///
/// A struct because it IS the seam: a field added here is the visible act of letting one more fact
/// reach user code. `Clone` is cheap and is required of any type the Rhai tier registers.
///
/// Every read verb is its `HistStore` method minus the `scan_`/`load_` prefix, and every one is
/// READ-ONLY (no append twin, and no way to build one). A study that finds a gap says so with
/// [`StudyError::Data`] and stops; filling it is an ingest command's job (ADR 0029).
#[derive(Clone)]
pub struct StudyContext {
    store: Arc<dyn HistStore + Send + Sync>,
    learner: Option<Arc<dyn StudyLearner>>,
    sim: Option<Arc<dyn StudySim>>,
    window: TsRange,
    scratch: PathBuf,
}

impl StudyContext {
    /// The minimum a study can be run with: a store to read, the window it was asked about, and a
    /// scratch directory it may use.
    ///
    /// `scratch` is a directory the caller owns and may delete afterwards — the design's
    /// `<project>/tmp/research/`. A parameter, because a study inventing its own temp path would be
    /// a library reading global state.
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, window: TsRange, scratch: PathBuf) -> Self {
        Self { store, learner: None, sim: None, window, scratch }
    }

    /// Supply the learner this host can train with.
    ///
    /// OPTIONAL: `scripts/fetch_release_tools.sh` ships no LightGBM binary on a non-Linux host. A
    /// study that must fit says so with [`StudyError::NoLearner`] and stops; it does not degrade
    /// into a number.
    #[must_use]
    pub fn with_learner(mut self, learner: Arc<dyn StudyLearner>) -> Self {
        self.learner = Some(learner);
        self
    }

    /// Supply the event-driven backtest this host can run.
    ///
    /// OPTIONAL, and absent is the ORDINARY case: only a host that links the simulator (the study
    /// runner in `crates/vike-studio-core`) can build one. A study that needs a run says so with
    /// [`StudyError::NoSim`] and stops.
    #[must_use]
    pub fn with_sim(mut self, sim: Arc<dyn StudySim>) -> Self {
        self.sim = Some(sim);
        self
    }

    /// The window this run was asked about. Pass it to the read verbs unless the study genuinely
    /// needs history BEFORE it — a rolling feature's warm-up is the ordinary reason to widen.
    ///
    /// It lives here rather than in `params` so the caller can record it in the run manifest
    /// without a parser per study: `crates/vike-studio-core/src/listing.rs`'s `list_runs` (R6).
    pub fn window(&self) -> TsRange {
        self.window
    }

    /// A directory the study may write scratch into. Nothing here survives the run.
    pub fn scratch(&self) -> &Path {
        &self.scratch
    }

    /// The learner, or `None` when this host cannot train. See [`StudyContext::with_learner`].
    pub fn learner(&self) -> Option<&dyn StudyLearner> {
        self.learner.as_deref()
    }

    /// The backtest runner, or `None` when this host cannot run one. See
    /// [`StudyContext::with_sim`].
    pub fn sim(&self) -> Option<&dyn StudySim> {
        self.sim.as_deref()
    }

    /// Derived OHLCV bars — [`vike_data::HistStore::load_bars`].
    pub fn bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TsRange,
    ) -> Result<Vec<Bar>, DataError> {
        self.store.load_bars(venue, symbol, interval, range)
    }

    /// L1 quotes — [`vike_data::HistStore::scan_quotes`].
    pub fn quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<QuoteTick>, DataError> {
        self.store.scan_quotes(venue, symbol, range)
    }

    /// Executed trades — [`vike_data::HistStore::scan_trades`].
    pub fn trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<TradeTick>, DataError> {
        self.store.scan_trades(venue, symbol, range)
    }

    /// Recorded L2 book events, the LOSSLESS lane — [`vike_data::HistStore::scan_book_updates`].
    pub fn book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.store.scan_book_updates(venue, symbol, range)
    }

    /// Recorded L2 depth snapshots, the CONFLATING lane — [`vike_data::HistStore::scan_depth`].
    /// A store with no depth lane REFUSES rather than answering an empty vector; that distinction
    /// is the whole reason the two verbs are separate and it is passed through unchanged.
    pub fn depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<BookUpdate>, DataError> {
        self.store.scan_depth(venue, symbol, range)
    }

    /// Graded cohort positioning marginals — [`vike_data::HistStore::scan_cohort`], the
    /// `kind=cohort` series `crates/vike-backfill/src/vikedata/ingest.rs` fills. The parameter is
    /// `asset`, not `symbol`, because [`vike_data::CohortRow::asset`] is the column it returns in.
    ///
    /// ⚠ **ONE scan is EVERY axis, label, grading and label basis**: the panel is LONG, so a study
    /// filters ROWS ([`vike_data::CohortRow::axis`], `cohort`, [`vike_data::CohortRow::grading`],
    /// `label_basis`). The three gradings return shape-identical rows over the same hours, so a
    /// caller that sums whatever comes back reports one number over two tapes.
    pub fn cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TsRange,
    ) -> Result<Vec<CohortRow>, DataError> {
        self.store.scan_cohort(venue, asset, range)
    }

    /// The venue's own per-interval market context for a perp —
    /// [`vike_data::HistStore::scan_perp_metrics`], the `kind=perp_metrics` series
    /// `crates/vike-backfill/src/funding_rate.rs` fills beside the funding-rate bars.
    ///
    /// Today that is the funding PREMIUM; the funding RATE rides [`vike_model::Bar::funding`]
    /// through [`StudyContext::bars`] with `interval = "funding"`, under the SAME symbol, so the two
    /// join on `(venue, symbol, ts)`.
    ///
    /// ⚠ **Absence means the venue never served it**: no premium writes no row, never a zero
    /// (zero is a real premium). Filling a gap is `vike-cli data hist fetch VENUE:SYMBOL:funding`.
    /// There is no open-interest twin: Hyperliquid publishes open interest only as a CURRENT
    /// snapshot, so the `oi_*` features have no backfillable source and refuse rather than NaN-fill.
    pub fn perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TsRange,
    ) -> Result<Vec<PerpMetricRow>, DataError> {
        self.store.scan_perp_metrics(venue, symbol, range)
    }
}

impl std::fmt::Debug for StudyContext {
    /// Hand-written because neither trait object is `Debug`. Prints the window, the scratch root
    /// and WHETHER a learner is present — the fact that decides whether a fitting study can run.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StudyContext")
            .field("window", &self.window)
            .field("scratch", &self.scratch)
            .field("learner", &self.learner.is_some())
            .finish()
    }
}
