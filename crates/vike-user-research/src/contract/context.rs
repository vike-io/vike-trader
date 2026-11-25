//! The context: [`StudyContext`], everything a study is handed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vike_data::{
    ChainRow, CohortRow, ExecFillRow, ExecForecastRow, ExecOrderRow, FinancialFactRow,
    HistoryStore, HistoryStoreError, InstitutionalHoldingRow, PerpMetricRow, TextRow, TextScoreRow,
    TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_model::{ContractValue, Instrument};

#[cfg(doc)]
use super::StudyError;
use super::{StudyLearner, StudyRuns, StudySim};

/// **Today's kept instrument lists, as the host read them**: [`StudyContext::kept_instruments`]'s answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeptInstruments {
    /// When the host read the lists, epoch ms UTC: the day these rows are TODAY's of.
    pub read_ms: i64,
    /// Every venue's kept rows, as the store serves them ([`vike_data::HistoryStore::kept_instruments`]).
    pub rows: Vec<Instrument>,
}

/// Where a study file's context gets [`KeptInstruments`]: asked of its host, which read them
/// (`crate::file::guest`). `None` in the host's own context, which reads its store.
pub(crate) type KeptInstrumentsSource =
    Arc<dyn Fn() -> Result<KeptInstruments, HistoryStoreError> + Send + Sync>;

/// **How the HOST values one unit of a kept contract**: `vike_catalog::kept_contract_value`'s signature, handed in
/// by the host that links the catalog (`crates/vike-study-runner/src/study_run.rs`'s `run_study_with`). This crate,
/// the user-study surface, links no catalog (decision 0089's fence), so the valuation runs on the host's side and a
/// study file asks for its answer.
pub type ContractValuation = fn(&str, &str, Option<&Instrument>) -> Option<ContractValue>;

/// Where a study file's context gets [`StudyContext::kept_contract_value`]: asked of its host
/// (`crate::file::guest`).
pub(crate) type ContractValueSource =
    Arc<dyn Fn(&str, &str) -> Result<Option<ContractValue>, HistoryStoreError> + Send + Sync>;

/// Where [`StudyContext::kept_contract_value`] is answered.
#[derive(Clone)]
enum ContractValues {
    /// The host's own context: its valuation over the kept row it reads.
    Valued(ContractValuation),
    /// A study file's context: asked of its host.
    Asked(ContractValueSource),
}

/// Everything a study is HANDED. Built by the caller — a binary, which is the only layer allowed
/// to read the environment — and never by a study.
///
/// A struct because it IS the seam: a field added here is the visible act of letting one more fact
/// reach user code. `Clone` is cheap and is required of any type the Rhai tier registers.
///
/// Every read verb is its `HistoryStore` method minus the `scan_`/`load_` prefix (and the `_head`
/// of the two execution reads, which ask for every row of their range), and every one is
/// READ-ONLY (no append twin, and no way to build one). A study that finds a gap says so with
/// [`StudyError::Data`] and stops; filling it is an ingest command's job (ADR 0029).
#[derive(Clone)]
pub struct StudyContext {
    store: Arc<dyn HistoryStore + Send + Sync>,
    learner: Option<Arc<dyn StudyLearner>>,
    sim: Option<Arc<dyn StudySim>>,
    runs: Option<Arc<dyn StudyRuns>>,
    window: TimestampRange,
    scratch: PathBuf,
    kept_instruments: Option<KeptInstrumentsSource>,
    contract_values: Option<ContractValues>,
}

impl StudyContext {
    /// The minimum a study can be run with: a store to read, the window it was asked about, and a
    /// scratch directory it may use.
    ///
    /// `scratch` is a directory the caller owns and may delete afterwards — the design's
    /// `<project>/tmp/research/`. A parameter, because a study inventing its own temp path would be
    /// a library reading global state.
    pub fn new(
        store: Arc<dyn HistoryStore + Send + Sync>,
        window: TimestampRange,
        scratch: PathBuf,
    ) -> Self {
        Self {
            store,
            learner: None,
            sim: None,
            runs: None,
            window,
            scratch,
            kept_instruments: None,
            contract_values: None,
        }
    }

    /// Answer [`StudyContext::kept_instruments`] from `source` instead of the store: a study file's
    /// context asks its host, so the answer carries the time the HOST read the lists.
    #[must_use]
    pub(crate) fn with_kept_instruments(mut self, source: KeptInstrumentsSource) -> Self {
        self.kept_instruments = Some(source);
        self
    }

    /// Value the kept contracts this run reads with `valuation` ([`StudyContext::kept_contract_value`]): the HOST's
    /// door, which links the catalog and passes `vike_catalog::kept_contract_value`. Without it a study is answered
    /// that this host values no contract, never a unit of 1.0.
    #[must_use]
    pub fn with_contract_valuation(mut self, valuation: ContractValuation) -> Self {
        self.contract_values = Some(ContractValues::Valued(valuation));
        self
    }

    /// Answer [`StudyContext::kept_contract_value`] from `source`: a study file's context asks its host.
    #[must_use]
    pub(crate) fn with_contract_values_asked(mut self, source: ContractValueSource) -> Self {
        self.contract_values = Some(ContractValues::Asked(source));
        self
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
    /// OPTIONAL, and absent is the ORDINARY case: only a host that links the simulator can build one
    /// (`crates/vike-backtesthub/src/harness_sim.rs`'s `HarnessSim`, which the backtest server's
    /// `RunStudy` and `vike-study` hand in; the desktop's Research pane links no engine and hands none). A study that needs a run says so with
    /// [`StudyError::NoSim`] and stops.
    #[must_use]
    pub fn with_sim(mut self, sim: Arc<dyn StudySim>) -> Self {
        self.sim = Some(sim);
        self
    }

    /// Supply the project's saved runs this host can read (book W21: a study that pools many
    /// strategies' forecasts reads them from their runs).
    ///
    /// OPTIONAL: `crates/vike-study-runner/src/study_run.rs`'s `run_study_with` hands the runs root
    /// it writes the study's own run into, so every host that persists a study offers it. A study
    /// that needs runs and is offered none says so and stops.
    #[must_use]
    pub fn with_runs(mut self, runs: Arc<dyn StudyRuns>) -> Self {
        self.runs = Some(runs);
        self
    }

    /// The window this run was asked about. Pass it to the read verbs unless the study genuinely
    /// needs history BEFORE it — a rolling feature's warm-up is the ordinary reason to widen.
    ///
    /// It lives here rather than in `params` so the caller can record it in the run manifest
    /// without a parser per study: `crates/vike-study-runner/src/listing.rs`'s `list_runs` (R6).
    pub fn window(&self) -> TimestampRange {
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

    /// The saved runs, or `None` when this host offers none. See [`StudyContext::with_runs`].
    pub fn runs(&self) -> Option<&dyn StudyRuns> {
        self.runs.as_deref()
    }

    /// Derived OHLCV bars — [`vike_data::HistoryStore::load_bars`].
    ///
    /// ⚠ Each bar is stamped at its OPEN (`vike_marketdata::Bar::timestamp`), so `range` selects by open time:
    /// a bar with `timestamp <= t` may still have been forming at `t`. A study deciding at `t` keeps the bars
    /// `vike_marketdata::Bar::closed_at(step, t)`.
    pub fn bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.store.load_bars(venue, symbol, interval, range)
    }

    /// The bars of ONE `source=` lane — [`vike_data::HistoryStore::load_bars_sourced_capped`], the only read
    /// that reaches a lane: [`StudyContext::bars`] reads the sourceless series, so a user's `csv-bars` import
    /// (`kind=bar/venue=…/source=user-<label>/…`) is read here with `lane` = `user-<label>`. A lane the store
    /// does not hold answers empty; one the store does not declare is refused. Stamped at the bar's OPEN,
    /// as [`StudyContext::bars`]' are.
    pub fn bars_in_lane(
        &self,
        lane: &str,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.bars_in_lane_capped(lane, venue, symbol, interval, range, None)
    }

    /// [`StudyContext::bars_in_lane`] under the store's row budget: the parent's answer to a study file's
    /// lane read, which carries the budget its store was asked with.
    pub(crate) fn bars_in_lane_capped(
        &self,
        lane: &str,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.store.load_bars_sourced_capped(venue, lane, symbol, interval, range, budget)
    }

    /// L1 quotes — [`vike_data::HistoryStore::scan_quotes`].
    pub fn quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.store.scan_quotes(venue, symbol, range)
    }

    /// Executed trades — [`vike_data::HistoryStore::scan_trades`].
    pub fn trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.store.scan_trades(venue, symbol, range)
    }

    /// Recorded L2 book events, the LOSSLESS lane — [`vike_data::HistoryStore::scan_book_updates`].
    pub fn book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.store.scan_book_updates(venue, symbol, range)
    }

    /// Recorded L2 depth snapshots, the CONFLATING lane — [`vike_data::HistoryStore::scan_depth`].
    /// A store with no depth lane REFUSES rather than answering an empty vector; that distinction
    /// is the whole reason the two verbs are separate and it is passed through unchanged.
    pub fn depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.store.scan_depth(venue, symbol, range)
    }

    /// Graded cohort positioning marginals — [`vike_data::HistoryStore::scan_cohort`], the
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
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        self.store.scan_cohort(venue, asset, range)
    }

    /// The venue's own per-interval market context for a perp —
    /// [`vike_data::HistoryStore::scan_perp_metrics`], the `kind=perp_metrics` series
    /// `crates/vike-backfill/src/funding_rate.rs` fills beside the funding-rate bars.
    ///
    /// Today that is the funding PREMIUM; the funding RATE rides [`vike_marketdata::Bar::funding`]
    /// through [`StudyContext::bars`] with `interval = "funding"`, under the SAME symbol, so the two
    /// join on `(venue, symbol, ts)`.
    ///
    /// ⚠ **Absence means the venue never served it**: no premium writes no row, never a zero
    /// (zero is a real premium). Filling a gap is `vike-cli data history fetch VENUE:SYMBOL:funding`.
    /// There is no open-interest twin: Hyperliquid publishes open interest only as a CURRENT
    /// snapshot, so the `oi_*` features have no backfillable source and refuse rather than NaN-fill.
    pub fn perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        self.store.scan_perp_metrics(venue, symbol, range)
    }

    /// **The texts a decision at `decision_ms` could have read** — [`vike_data::HistoryStore::texts_as_of`],
    /// the `kind=text` series the datahub's text recorder fills: every row of `(text_kind, collector)`
    /// RECEIVED in `[decision_ms - lookback_ms, decision_ms]` (`news` from `rss`, `economic_calendar`
    /// from `forexfactory`, and a later kind's own words), received-ascending.
    ///
    /// ⚠ **The only text verb, on purpose: a study cannot ask for a text received after its decision.**
    /// The rows are selected on [`vike_data::TextRow::received_ms`], never on `published_ms`, which a
    /// source can back-date. Every revision of a calendar event comes back; the last row per
    /// [`vike_data::TextRow::text_id`] is the version known at `decision_ms`. The history begins the day
    /// the recorder was switched on: an empty answer before it is the truth of what was recorded.
    pub fn texts_as_of(
        &self,
        text_kind: &str,
        collector: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        self.store.texts_as_of(text_kind, collector, decision_ms, lookback_ms)
    }

    /// Option-chain snapshot rows — [`vike_data::HistoryStore::scan_chain`], the `kind=chain` series
    /// of `(venue, underlying)`: one row per option per snapshot, every row of a snapshot sharing its
    /// `ts`, ts-ascending. `underlying` is the series partition (`BTC` on deribit, `BTCUSDT` on
    /// binance), never an option's own name.
    ///
    /// ⚠ **`ts` is when the row was KNOWN**, so a study deciding at time `t` reads rows with
    /// `ts <= t` and nothing else. Each producer stamps it so: the datahub's recorder at its poll,
    /// Binance's hourly summary (`vike-cli data history import binance-eohsummary`) at the LAST
    /// millisecond of the hour the row summarises, never the hour's start.
    ///
    /// A store that holds no chains refuses rather than answering empty; a chain the store lacks is
    /// imported or recorded, never fetched by a study (ADR 0029).
    pub fn chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.store.scan_chain(venue, underlying, range)
    }

    /// The surface as it stood at `ts`: each option's LAST row observed in
    /// `[ts - lookback_ms, ts]` — [`vike_data::HistoryStore::chain_as_of_within`], sorted
    /// `(expiry_ms, strike, instrument)`. The point-in-time read a study takes at each decision time.
    ///
    /// ⚠ An option that expired or stopped being quoted still answers its last row inside the window:
    /// filter `expiry_ms > ts` for the live ones. `lookback_ms` bounds the read (and so its cost):
    /// a few snapshot periods, such as two hours over Binance's hourly summary.
    pub fn chain_as_of(
        &self,
        venue: &str,
        underlying: &str,
        ts: i64,
        lookback_ms: i64,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.store.chain_as_of_within(venue, underlying, ts, lookback_ms)
    }

    /// **A company's reported XBRL values a decision at `decision_ms` could have known** —
    /// [`vike_data::HistoryStore::financial_facts_as_of`], the `kind=financial_fact` series the SEC
    /// import fills (`venue` `sec`, `cik` ten digits): every value KNOWABLE in
    /// `[decision_ms - lookback_ms, decision_ms]`, knowable-ascending.
    ///
    /// ⚠ Selected on [`vike_data::FinancialFactRow::filed_ms`] (the end of its filing's EDGAR day),
    /// never on the period it describes: a restatement filed after the decision is never seen, and
    /// the last row per concept, unit and period is the value known at `decision_ms`.
    pub fn financial_facts_as_of(
        &self,
        venue: &str,
        cik: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        self.store.financial_facts_as_of(venue, cik, decision_ms, lookback_ms)
    }

    /// **A manager's Form 13F lines a decision at `decision_ms` could have known** —
    /// [`vike_data::HistoryStore::institutional_holdings_as_of`], the `kind=institutional_holding`
    /// series the SEC import fills (`venue` `sec`, `filer_cik` the manager's ten digits): every line
    /// KNOWABLE in `[decision_ms - lookback_ms, decision_ms]`. The latest `period` is the manager's
    /// latest report then; an amendment's `RESTATEMENT` replaces it and `NEW HOLDINGS` adds to it.
    pub fn institutional_holdings_as_of(
        &self,
        venue: &str,
        filer_cik: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        self.store.institutional_holdings_as_of(venue, filer_cik, decision_ms, lookback_ms)
    }

    /// **The model scores a decision at `decision_ms` could have read** —
    /// [`vike_data::HistoryStore::text_scores_as_of`], the `kind=text_score` series the `text-scores` import
    /// fills: every score of `(text_kind, model_name)` (`news`, `finbert` or `all-minilm-l6-v2`) whose
    /// TEXT was received in `[decision_ms - lookback_ms, decision_ms]`, the window [`Self::texts_as_of`]
    /// reads, so the two join on `(received_ms, text_id)`. The numbers were computed host-side by
    /// `vike-nlp` (a study cannot load a model); what a position of
    /// [`vike_data::TextScoreRow::value_list`] means is the model's (`finbert`: positive, negative, neutral).
    /// The point-in-time read only, like the texts: no study can ask for a score of a later text.
    ///
    /// ⚠ **The TEXT is point in time; the MODEL is not.** Each score is keyed to its text's received time, but
    /// it was computed by today's pinned model (`vike-nlp`'s pins), pretrained on corpora gathered after much
    /// of any backtest window: a 2020 decision scored by a model that read 2021 text knows what no 2020 reader
    /// could. A study over a window before the model's training cut-off measures that knowledge too, so an
    /// edge found there must be confirmed on decisions after the cut-off.
    pub fn text_scores_as_of(
        &self,
        text_kind: &str,
        model_name: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        self.store.text_scores_as_of(text_kind, model_name, decision_ms, lookback_ms)
    }

    /// **The live forecasts a decision at `decision_ms` could have known** —
    /// [`vike_data::HistoryStore::forecasts_as_of`], the `kind=exec_forecast` series the data daemon
    /// stores from the live strategies' published forecasts (the tradehub's execution stream,
    /// decision 0295): every mount's rows of `(venue, symbol)` DECIDED at or before
    /// `decision_ms` (`ts <= decision_ms`), from `start_ms` when given, ts-ascending. Tell two mounts
    /// apart by [`vike_data::ExecForecastRow::mount_id`].
    ///
    /// ⚠ **The only live-forecast verb, on purpose: a study cannot ask for a forecast decided after its
    /// decision.** The row carries no outcome: a forecast whose `horizon_ms` ends after `decision_ms`
    /// had not come true or false yet, so pricing it against later bars measures with hindsight. A
    /// backtest's forecasts are its saved run's ([`StudyContext::runs`]), never this series.
    pub fn forecasts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        start_ms: Option<i64>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        self.store.forecasts_as_of(venue, symbol, decision_ms, start_ms)
    }

    /// The account's OWN fills with a time in `range` —
    /// [`vike_data::HistoryStore::scan_execution_fills_head`] asked for every row, ts-ascending. The
    /// `kind=exec_fill` series is the trade record the data daemon stores from the tradehub's
    /// execution stream (decision 0295), so a study reads what was traded, point in time, through
    /// the window it was handed.
    ///
    /// ⚠ A fee correction is a ROW of the same series, not a fill: read the rows as fills with
    /// [`vike_data::ExecFillRow::fill_events`], which folds each correction into its fill.
    ///
    /// ⚠ **Point in time for the fees too: a correction not KNOWN by `range.end` is left out.** A
    /// correction row is stamped at its FILL's `ts` ([`vike_data::ExecFillRow::fee_correction_row`]),
    /// so the range alone would hand a study over `[.., 12:00]` a commission the venue revised at
    /// 18:00. The rows kept are every fill as before, and the corrections whose
    /// [`vike_data::ExecFillRow::received_ms`] is at or before `range.end`; one with no receipt time
    /// (written before the column) is left out too, since when it became known cannot be told. A
    /// range with no end keeps every correction. A study FILE asks its host this verb
    /// (`Call::ExecutionFills`), so the host's own context leaves them out before the rows cross.
    pub fn execution_fills(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let mut rows = self.store.scan_execution_fills_head(venue, symbol, range, usize::MAX)?;
        if let Some(end) = range.end {
            rows.retain(|r| {
                r.corrects_trade_id.is_none() || r.received_ms.is_some_and(|known| known <= end)
            });
        }
        Ok(rows)
    }

    /// The account's own ORDER lifecycle snapshots (`kind=execution_order`: submit, accept, …,
    /// terminal) with a time in `range` — [`vike_data::HistoryStore::scan_execution_orders_head`]
    /// asked for every row, ts-ascending. Join them to [`Self::execution_fills`] by
    /// `order_client_id`.
    pub fn execution_orders(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        self.store.scan_execution_orders_head(venue, symbol, range, usize::MAX)
    }

    /// **Every venue's kept instrument rows, TODAY's** — [`vike_data::HistoryStore::kept_instruments`]: the
    /// catalog the datahub keeps (`<project>/instruments/<venue>.json`, its one reader), with the time the
    /// host read it ([`KeptInstruments::read_ms`]). What `vike_analytics::instrument_graph` builds its graph
    /// from. The host reads the lists; the study gets the rows, never a file or a path.
    ///
    /// ⚠ **Today's list, not the list of the window's day.** An instrument the venue delisted is not in it,
    /// and one listed since is, so a study over a PAST window that picks its instruments from these rows
    /// carries survivorship bias. No list history is kept.
    ///
    /// # Errors
    /// The store serves no kept list (`Unsupported`, in words): only the datahub's store keeps them, so a
    /// run over a local store or a datahub too old to keep them has none. The store's own refusal otherwise.
    pub fn kept_instruments(&self) -> Result<KeptInstruments, HistoryStoreError> {
        if let Some(source) = &self.kept_instruments {
            return source();
        }
        let rows = self.store.kept_instruments()?.ok_or_else(|| {
            HistoryStoreError::Unsupported(
                "kept_instruments: this run's store serves no kept instrument list (only the datahub \
                 keeps them; `vike-cli data catalog refresh` writes them)"
                    .into(),
            )
        })?;
        Ok(KeptInstruments { read_ms: vike_model::time::clock::now_ms(), rows })
    }

    /// **`symbol`'s kept row on `venue`** ([`Self::kept_instruments`]): the row whose wire name or stored user name
    /// is `symbol`. `None` when the list holds no such row AND when this run's store serves no list at all, the two
    /// cases a study treats alike: it knows nothing of the contract, and an unlisted symbol reads
    /// `vike_model::ContractValue`'s default.
    ///
    /// # Errors
    /// The store's own refusal, other than serving no list.
    pub fn kept_instrument(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Option<Instrument>, HistoryStoreError> {
        let kept = match self.kept_instruments() {
            Ok(kept) => kept,
            Err(HistoryStoreError::Unsupported(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let user = Some(vike_model::InstrumentNaming::User(symbol.to_string()));
        Ok(kept
            .rows
            .into_iter()
            .find(|r| r.venue == venue && (r.raw_symbol == symbol || r.naming == user)))
    }

    /// **What one unit of `symbol` on `venue` is worth, as the HOST values its kept row**: the host's valuation
    /// ([`StudyContext::with_contract_valuation`], the catalog's `vike_catalog::kept_contract_value`) over
    /// [`Self::kept_instrument`]'s row. `None` when the host holds no kept row of it, and when the row's unit is one
    /// only the venue states (an ibkr future's multiplier, an fxcm CFD's `ContractMultiplier`, an ig epic's
    /// `contractSize`): a study that books money in the contract refuses a `None` in words and never reads 1.0 into it.
    ///
    /// # Errors
    /// `Unsupported` when this run's host values no contract (a context built without
    /// [`StudyContext::with_contract_valuation`]); a study file run by a host older than this verb is answered
    /// nothing, said as an `Io` refusal; the store's own refusal otherwise.
    pub fn kept_contract_value(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Option<ContractValue>, HistoryStoreError> {
        match &self.contract_values {
            Some(ContractValues::Valued(valuation)) => Ok(self
                .kept_instrument(venue, symbol)?
                .and_then(|row| valuation(venue, symbol, Some(&row)))),
            Some(ContractValues::Asked(ask)) => ask(venue, symbol),
            None => Err(HistoryStoreError::Unsupported(format!(
                "kept_contract_value: this run's host values no contract (asked for {venue} {symbol}), \
                 and one unit is never read as 1.0 without it"
            ))),
        }
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
