//! The STUDY ENTRY CONTRACT: what a user's `<name>.rs` is handed, and what it hands back.
//!
//! ```ignore
//! pub fn run(
//!     ctx: &vike_user_research::StudyContext,
//!     params: &toml::Value,
//! ) -> Result<vike_user_research::StudyOutcome, vike_user_research::StudyError>
//! ```
//!
//! [`StudyFn`] is that signature as a type, and the generated registry coerces every entry to it
//! (`user_<name>::run as StudyFn`) — so a user file whose signature drifts fails the BUILD naming
//! the mismatch, rather than failing at the call site of a study somebody is waiting on.
//!
//! # Why a FUNCTION and not a `Study` trait
//!
//! `docs/superpowers/specs/2026-08-24-research-engine-user-split-design.md` refuses a `Study`
//! trait in as many words — *"Wrong tool for one implementation; extract it from two"* — after
//! measuring twelve places where a cohort concept leaks into what an earlier draft called the
//! engine. This file honours that: there is no trait to implement, no object to construct, no
//! lifecycle to learn. A study is called ONCE and returns a value.
//!
//! The strategy tier needs a trait for a reason a study does not share: a strategy is DRIVEN —
//! `on_bar` is called thousands of times and the object between calls is where its state lives.
//! A study has no between-calls. Reading that difference as "so a study needs its own trait too"
//! is exactly the symmetry the spec measured and refused.
//!
//! # Why the study RETURNS its result instead of writing it
//!
//! `crates/vike-backtest/src/runs.rs` already owns run persistence for EVERY kind of run —
//! `create_run_dir` (whose atomic `create_dir` IS the id mint), `RunManifest` with its common
//! `kind`/`produced_by`/`config` head and its kind-specific `detail` subtree, and `write_run`,
//! which writes the report FIRST and the manifest LAST so the manifest doubles as the completion
//! marker `crates/vike-studio-core/src/listing.rs`'s `list_runs` keys on. That crate sits at layer
//! 50, ABOVE this one, so a study cannot reach it — and should not want to. A study that minted
//! its own run id and wrote its own manifest would be a second copy of a schema whose whole point
//! (R6: one results surface) is that there is only one.
//!
//! So: the study produces [`StudyOutcome`]; the caller mints the run, writes the artifacts and
//! folds the metrics into the manifest's `detail`. This is the same split
//! `crates/vike-research/src/cli.rs`'s `run_cohort` used for its own reason — *"Returned
//! rather than printed so `--dry-run` and a real run share one body"*. ⚠ That driver is GONE: the
//! research crate dissolved and its CLI had no successor, so the citation is the evidence for the
//! precedent rather than a file to go and read (it is filed in
//! `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`); the study it drove is now a
//! user study on this very contract.
//!
//! # Why the context is HANDED IN
//!
//! `docs/decisions/0029-a-study-reads-the-store-never-a-vendor-api.md` is accepted: a study reads
//! the hist store and nothing else, and the exploratory fetch is an ingest command. This file is
//! where that becomes structural rather than advisory. The study never resolves a path, never
//! reads an environment variable, never opens a socket and never constructs a store — it is
//! handed [`StudyContext`], which forwards the store's READ verbs and nothing else. The append
//! half of `vike_data::HistStore` is not reachable from here at all: this crate takes `vike-data`
//! feature-free, so the only concrete implementation (`DataFusionHist`, behind `hist-datafusion`)
//! is not compiled, and a study has no value of that type to call an append on.
//!
//! ⚠ **That is a property of the SANCTIONED surface, not a guarantee.** ADR 0029 says so itself:
//! *"The lever is NOT enforcement."* A user file is ordinary Rust compiled into the operator's own
//! binary; what this contract buys is that the cheapest path is the correct one and that an
//! unsanctioned one has to be added to a manifest, in a diff.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vike_data::{CohortRow, DataError, HistStore, PerpMetricRow, TsRange};
use vike_ml::{Capture, FitImportance, GridPoint, Learner, ProbaModel, TrainData};
use vike_model::{Bar, BookUpdate, QuoteTick, TradeTick};

/// The entry function every user study exposes, as a type.
///
/// NON-GENERIC on purpose, and that is the one place this contract deliberately DIVERGES from the
/// strategy tier's `build<B: HftBroker>`. R7 says a study may be written in Rhai or in Rust; a
/// Rhai script cannot carry a type parameter, and a generic entry would force the interpreted tier
/// to grow a second context type with a second set of methods to learn. There is no `B` here to
/// need one: a study places no orders, so nothing about it is generic over a broker.
pub type StudyFn = fn(&StudyContext, &toml::Value) -> Result<StudyOutcome, StudyError>;

// ---------------------------------------------------------------------------------------------
// the learner seam, erased
// ---------------------------------------------------------------------------------------------

/// What [`StudyLearner::fit_captured`] hands back — [`vike_ml::CapturedFit`] with the model boxed.
pub type CapturedStudyFit = (Box<dyn ProbaModel>, Option<FitImportance>, Option<String>);

/// [`vike_ml::Learner`] with its associated `Model` type erased, so it can cross a NON-GENERIC
/// boundary.
///
/// `vike_ml::Learner` carries `type Model: ProbaModel`, which makes `dyn Learner` unusable without
/// naming a concrete model — so the seam cannot be put on [`StudyContext`] as it stands. This
/// trait is that seam boxed, and NOTHING ELSE: the blanket impl below covers every real
/// `Learner`, so there is nothing to implement and no second seam to keep in step.
///
/// ⚠ **It erases the WHOLE trait, all three methods, deliberately.** A two-method version would
/// have been shorter and would have silently removed two shipped capabilities from every user
/// study: `fit_captured` is what lets a run EXPORT the model it selected (the `--model-out` half
/// of the design's Phase 5) and `fit_identity` is the key half of a cross-run fit cache. An
/// erasure that quietly downgrades the seam it erases is worse than no erasure.
///
/// `Send + Sync` because the workspace pins `rhai` with `features = ["sync"]` (root `Cargo.toml`),
/// so a type the Rhai tier registers must be both — and every real implementation already is
/// (`Learner` itself requires `Sync`). Adopting the bound now is free; adding it later would break
/// every caller that had built a context.
pub trait StudyLearner: Send + Sync {
    /// Fit a model at one hyperparameter point. See [`vike_ml::Learner::fit`] — including its rule
    /// that a REFUSED point is an ordinary `Err`, not a reason to stop a search.
    fn fit(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
    ) -> Result<Box<dyn ProbaModel>, String>;

    /// Fit, keeping whatever `want` asks for beside the model. See
    /// [`vike_ml::Learner::fit_captured`] — ONE fit however many artifacts are requested.
    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedStudyFit, String>;

    /// A content digest of everything a fit would consume, or `None` — see
    /// [`vike_ml::Learner::fit_identity`]. `None` means a cache must never serve or store for this
    /// learner, which is the safe direction.
    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]>;
}

/// Every real [`vike_ml::Learner`] is a [`StudyLearner`]. There is deliberately no other way to
/// obtain one: a second implementation would be a second learner seam, and this crate is not where
/// a learner is defined.
impl<L> StudyLearner for L
where
    L: Learner + Send + Sync,
    L::Model: 'static,
{
    fn fit(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
    ) -> Result<Box<dyn ProbaModel>, String> {
        Ok(Box::new(Learner::fit(self, d, p, seed)?))
    }

    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedStudyFit, String> {
        let (model, importance, text) = Learner::fit_captured(self, d, p, seed, want)?;
        Ok((Box::new(model), importance, text))
    }

    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        Learner::fit_identity(self, d, p, seed)
    }
}

// ---------------------------------------------------------------------------------------------
// the context
// ---------------------------------------------------------------------------------------------

/// Everything a study is HANDED. Built by the caller — a binary, which is the only layer allowed
/// to read the environment — and never by a study.
///
/// A struct rather than four parameters because it IS the seam: a field added here is the visible
/// act of letting one more fact reach user code, and it does not break a single user file (the
/// fields are private and the accessors are methods).
///
/// `Clone` is cheap (two `Arc`s, a `TsRange` and a `PathBuf`) and is required of any type the Rhai
/// tier registers.
#[derive(Clone)]
pub struct StudyContext {
    store: Arc<dyn HistStore + Send + Sync>,
    learner: Option<Arc<dyn StudyLearner>>,
    window: TsRange,
    scratch: PathBuf,
}

impl StudyContext {
    /// The minimum a study can be run with: a store to read, the window it was asked about, and a
    /// scratch directory it may use.
    ///
    /// `scratch` is a directory the caller owns and may delete afterwards — the design's
    /// `<project>/tmp/research/`, *"re-derivable, NOT user content"*. It is a parameter because a
    /// study inventing its own temp path would be a library reading global state, the exact shape
    /// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` ratchets down.
    pub fn new(store: Arc<dyn HistStore + Send + Sync>, window: TsRange, scratch: PathBuf) -> Self {
        Self { store, learner: None, window, scratch }
    }

    /// Supply the learner this host can train with.
    ///
    /// Deliberately OPTIONAL, and the absence is not hypothetical:
    /// `scripts/fetch_release_tools.sh` ships no LightGBM binary on a non-Linux host, so a Windows
    /// desktop's ML story is inference and importance rather than training. A study that must fit
    /// says so with [`StudyError::NoLearner`] and stops; it does not degrade into a number.
    #[must_use]
    pub fn with_learner(mut self, learner: Arc<dyn StudyLearner>) -> Self {
        self.learner = Some(learner);
        self
    }

    /// The window this run was asked about. Pass it to the read verbs unless the study genuinely
    /// needs history BEFORE it — a rolling feature's warm-up is the ordinary reason to widen.
    ///
    /// It lives here rather than in `params` so the caller can record it in the run manifest
    /// without parsing a study's own configuration. That is what keeps R6's single run listing
    /// possible: `crates/vike-studio-core/src/listing.rs`'s `list_runs` must not need a parser per
    /// kind of run merely to list one.
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
    /// `kind=cohort` series `crates/vike-backfill/src/vikedata/ingest.rs` fills.
    ///
    /// This is the verb that makes a COHORT study expressible under ADR 0029 at all. Every other
    /// input a cohort study needs was already reachable — prices are bars, and the five verbs above
    /// cover the market plane — while its defining input, the graded panel, had no read at all, so
    /// the sanctioned surface could not serve the one study the store kind was added for. The
    /// alternative to this method is not "a study uses a different verb"; it is a study reaching
    /// past this contract for an HTTP client, which is the thing the contract exists to make the
    /// expensive path.
    ///
    /// # The name and the parameter are DERIVED, not chosen
    ///
    /// Every verb above is its store method minus the `scan_`/`load_` prefix
    /// (`scan_quotes` → `quotes`, `scan_book_updates` → `book_updates`, `load_bars` → `bars`), so
    /// `scan_cohort` → `cohort` — singular, because the store kind and its read verb are, and a
    /// tidier plural here would be the second spelling of a name that already has one.
    ///
    /// The second parameter is `asset` rather than the siblings' `symbol` for the same reason:
    /// [`vike_data::HistStore::scan_cohort`] calls it that and [`vike_data::CohortRow::asset`] is
    /// the column it comes back in. `crates/vike-data/src/store_kind.rs`' `cohort` row records
    /// that the store path spells it `symbol=<asset>` — one dimension, two names, and this crate
    /// follows the one the row carries so a caller reading a returned row never has to translate.
    ///
    /// # ⚠ ONE scan is EVERY axis, label, grading and label basis
    ///
    /// Unlike the market verbs, `(venue, asset)` does not identify a series here — the panel is
    /// LONG, and four more dimensions live in columns
    /// ([`vike_data::CohortRow::axis`], `cohort`, [`vike_data::CohortRow::grading`],
    /// `label_basis`). A study filters ROWS, not series, and it must: the three gradings return
    /// shape-identical rows over the same hours for the same asset, so a caller that sums whatever
    /// comes back reports one number over two tapes. `CohortRow::grading`'s own doc is the
    /// authority on that hazard, and this verb passes the rows through undiminished precisely so
    /// the study — not this crate — decides which of them its question is about.
    ///
    /// # ⚠ Read-only, like every verb here
    ///
    /// There is no `append_cohort` twin and there is no way to build one from this crate: the
    /// module doc's structural argument covers this verb unchanged — `vike-data` is taken
    /// feature-free, `DataFusionHist` is not compiled, and a study holds no value it could append
    /// through. A study that has found a gap says so with [`StudyError::Data`] and stops; filling
    /// it is `vikedata_backfill`'s job, which is ADR 0029's whole shape.
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
    /// Today that is one number: the funding PREMIUM, which is what the cohort study's
    /// `premium_z_24h` feature is computed over. It is a SEPARATE series from the funding rate on
    /// purpose — the rate rides [`vike_model::Bar::funding`], reachable through
    /// [`StudyContext::bars`] with `interval = "funding"` — and
    /// [`vike_data::PerpMetricRow`]'s doc is the authority on why one venue response fills two
    /// series rather than one row carrying both.
    ///
    /// # The name and the parameter are DERIVED, not chosen
    ///
    /// `scan_perp_metrics` minus the `scan_` prefix, exactly as `scan_cohort` → `cohort` and
    /// `scan_quotes` → `quotes`. The second parameter is `symbol` rather than `cohort`'s `asset`
    /// because [`vike_data::HistStore::scan_perp_metrics`] calls it that, and it is the SAME
    /// symbol the matching funding-rate bars are stored under — so a study joins the two series by
    /// `(venue, symbol, ts)` with no translation step.
    ///
    /// # ⚠ Absence here means the venue never served it, not that the study may fill it in
    ///
    /// A venue that publishes no premium writes no rows (Binance is one), and an interval whose
    /// premium was absent or unparseable writes no row for that interval — never a zero, because
    /// zero is a real premium. A study that finds a gap says so with [`StudyError::Data`] and
    /// stops, the same way every verb here behaves; filling it is `funding_rate_backfill`'s job.
    ///
    /// ⚠ **There is no open-interest twin of this verb and there cannot be one from a Hyperliquid
    /// backfill.** That venue publishes open interest only as a CURRENT snapshot — in the
    /// `POST /info {"type":"metaAndAssetCtxs"}` body and on the `activeAssetCtx` websocket
    /// channel — with no historical verb on its `/info` surface at all, so the study's `oi_z_24`,
    /// `oi_z_72` and `d_oi_{1,4,24}` features have no backfillable source and continue to refuse
    /// rather than NaN-fill.
    ///
    /// # ⚠ Read-only, like every verb here
    ///
    /// There is no `append_perp_metrics` twin and no way to build one from this crate — the module
    /// doc's structural argument covers this verb unchanged.
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
    /// Hand-written because neither trait object is `Debug`. Prints what an operator reading a log
    /// needs — the window, the scratch root and WHETHER a learner is present, which is the fact
    /// that decides whether a fitting study can run at all.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StudyContext")
            .field("window", &self.window)
            .field("scratch", &self.scratch)
            .field("learner", &self.learner.is_some())
            .finish()
    }
}

// ---------------------------------------------------------------------------------------------
// the outcome
// ---------------------------------------------------------------------------------------------

/// What a study hands back: named numbers, and named text artifacts.
///
/// **Two shapes, because a run listing and a run directory want different things.** The metrics
/// are what a listing row shows without opening anything — the design's *"a listed row with its
/// Sharpe, trades, CI and report"* — and the artifacts are the files that land beside the
/// manifest: `per_rule.tsv`, `folds.tsv`, `importance.tsv`, `model.txt`, `report.html`.
///
/// Metrics keep EMISSION order rather than sorting: a study puts its headline first, and a
/// `BTreeMap` would file `sharpe` after `n_trades` for no reason anyone chose.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudyOutcome {
    metrics: Vec<(String, f64)>,
    artifacts: Vec<(String, String)>,
}

impl StudyOutcome {
    /// An empty outcome. A study that legitimately found nothing returns one of these with a
    /// metric saying so — never an `Err`, which means the study could not run.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one headline number.
    ///
    /// A NON-FINITE value is ACCEPTED: `NaN` is the honest answer for a Sharpe over a fold that
    /// never traded, and `crates/vike-analytics/src/signal_backtest.rs`'s
    /// `neg_sharpe_mini_backtest` deliberately scores a degenerate slice `INFINITY`. Rounding
    /// either to zero here would be the `unwrap_or(0.0)` that turns a gap into an observation.
    /// A DUPLICATE name is refused — two rows called `sharpe` make a listing ambiguous and there
    /// is no correct way for a reader to pick.
    pub fn metric(&mut self, name: &str, value: f64) -> Result<(), StudyError> {
        if self.metrics.iter().any(|(n, _)| n == name) {
            return Err(StudyError::Study(format!("duplicate metric {name:?}")));
        }
        self.metrics.push((name.to_string(), value));
        Ok(())
    }

    /// Record one named text artifact. `name` becomes a FILE NAME inside the run directory, so it
    /// is validated as one: non-empty, at most [`MAX_ARTIFACT_NAME`] bytes, `[A-Za-z0-9._-]` only,
    /// not starting with `.`, and not a duplicate. Anything with a separator or a `..` in it is
    /// refused by that charset — a study naming its artifact `../../secrets.env` must not be a
    /// path the writer has to defend against.
    ///
    /// ⚠ **TEXT, not bytes**, and that is a narrowing rather than an oversight: every artifact the
    /// design names is text (`.tsv`, `.json`, `.html`, and a LightGBM `save_model` dump — see
    /// `user_data/research/studies/rust/cohort/run.rs`'s `WinnerModel`, whose `text` field is a
    /// `String`), and a `String` is the one shape the Rhai tier can build. A study that genuinely
    /// needs bytes is the condition that reopens this.
    ///
    /// ⚠ **RESERVED names are the WRITER's business, not this type's.**
    /// `crates/vike-backtest/src/runs.rs` owns `MANIFEST_FILE` and `REPORT_FILE` and sits at a
    /// layer this crate may not depend on; restating the two literals here would be a second
    /// authority for a fact that already has one. The caller refuses the collision.
    pub fn artifact(&mut self, name: &str, body: impl Into<String>) -> Result<(), StudyError> {
        if let Err(why) = valid_artifact_name(name) {
            return Err(StudyError::Study(format!("artifact name {name:?}: {why}")));
        }
        if self.artifacts.iter().any(|(n, _)| n == name) {
            return Err(StudyError::Study(format!("duplicate artifact {name:?}")));
        }
        self.artifacts.push((name.to_string(), body.into()));
        Ok(())
    }

    /// The headline numbers, in emission order.
    pub fn metrics(&self) -> &[(String, f64)] {
        &self.metrics
    }

    /// The named artifacts, in emission order.
    pub fn artifacts(&self) -> &[(String, String)] {
        &self.artifacts
    }

    /// One metric by name, or `None`.
    pub fn metric_value(&self, name: &str) -> Option<f64> {
        self.metrics.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
    }
}

/// The longest artifact name accepted. Long enough for any file the design names and short enough
/// that no filesystem in the deployment set can refuse the path the writer builds from it.
pub const MAX_ARTIFACT_NAME: usize = 64;

/// The artifact-name rule, as a function so it is testable without building an outcome. `Ok(())`
/// or a sentence naming what is wrong.
pub fn valid_artifact_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("must not be empty");
    }
    if name.len() > MAX_ARTIFACT_NAME {
        return Err("longer than MAX_ARTIFACT_NAME bytes");
    }
    if name.starts_with('.') {
        return Err("must not start with a dot (that is a hidden file, and `..` is a traversal)");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err("must match [A-Za-z0-9._-] — it becomes a file name in the run directory");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// the error
// ---------------------------------------------------------------------------------------------

/// Why a study could not produce a result.
///
/// Three variants, and the split is by WHO an operator has to go and talk to. A `Study` is the
/// author's; a `Data` is the ingest side's — ADR 0029's item 3 says a study asking for a window
/// the store does not hold must stop naming the range and the command that fills it, and this is
/// the variant that carries such a refusal up; a `NoLearner` is the HOST's, and is the one an
/// operator can fix by running somewhere else.
#[derive(Debug)]
pub enum StudyError {
    /// The study's own refusal: a parameter it will not accept, an assumption violated, a fit it
    /// decided was fatal. `vike_ml::Learner::fit` returns `Err(String)` for a REJECTED
    /// hyperparameter point, which a search treats as an ordinary event — a fit error arriving
    /// here means the study decided otherwise.
    Study(String),
    /// A store read failed. Carries `vike-data`'s own error rather than flattening it to a string:
    /// the caller can tell a query failure from an I/O one without parsing a sentence.
    Data(DataError),
    /// This study must TRAIN and the host supplied no learner. A TYPED variant rather than a
    /// sentence, because it is the one failure a caller can act on mechanically — it is the
    /// documented ceiling of a host with no LightGBM binary, not a bug, and a UI should say so
    /// rather than showing a stack of prose. The payload names what the study wanted to fit.
    NoLearner(String),
}

impl std::fmt::Display for StudyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StudyError::Study(m) => write!(f, "study: {m}"),
            StudyError::Data(e) => write!(f, "store read: {e}"),
            StudyError::NoLearner(what) => write!(
                f,
                "study needs a learner to fit {what}, and this host supplied none \
                 (no LightGBM binary on this platform — inference and importance still work)"
            ),
        }
    }
}

impl std::error::Error for StudyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StudyError::Data(e) => Some(e),
            _ => None,
        }
    }
}

/// So a study body can `?` a store read straight through.
impl From<DataError> for StudyError {
    fn from(e: DataError) -> Self {
        StudyError::Data(e)
    }
}
