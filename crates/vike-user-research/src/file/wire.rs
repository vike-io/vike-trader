//! The messages a study file and its host exchange: ONE spelling, linked by both halves.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use vike_data::{HistoryStoreError, SeriesId, TimestampRange};
use vike_ml_prediction::{FitImportance, PredictionKind};
use vike_ml_training::{GridPoint, TrainData};

use crate::{StudyError, StudyOutcome};

/// `value` as msgpack with field names (so an optional or skipped field never shifts a position).
/// A value that cannot be written is written as nothing, which the reader answers as "no answer".
pub(crate) fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    rmp_serde::to_vec_named(value).unwrap_or_default()
}

/// `bytes` read back; empty bytes are the other side's panic, said as such.
pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.is_empty() {
        return Err("no answer came back (the other side failed: its words are on stderr)".into());
    }
    rmp_serde::from_slice(bytes).map_err(|e| format!("an answer that does not read: {e}"))
}

/// What the parent hands the study file to start it: the recipe (TOML text, as a strategy file's
/// params cross), the window, the scratch path, and whether a learner and a backtest are on offer.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Start {
    pub(crate) params: String,
    pub(crate) window: TimestampRange,
    pub(crate) scratch: String,
    pub(crate) learner: bool,
    pub(crate) sim: bool,
    /// Whether the saved runs are on offer (book W21).
    #[serde(default)]
    pub(crate) runs: bool,
}

/// A request the study makes of the PARENT: each `StudyContext` read verb, the learner seam method
/// for method, and the one backtest.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Call {
    Bars {
        venue: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
    },
    Quotes {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    Trades {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    BookUpdates {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    Depth {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    Cohort {
        venue: String,
        asset: String,
        range: TimestampRange,
    },
    PerpMetrics {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    /// `StudyContext::texts_as_of`: the point-in-time text read, asked as it was written (never a
    /// range), so the parent answers exactly what a decision at `decision_ms` could have read.
    TextsAsOf {
        text_kind: String,
        collector: String,
        decision_ms: i64,
        lookback_ms: i64,
    },
    /// `StudyContext::financial_facts_as_of`, asked as the point-in-time read it is.
    FinancialFactsAsOf {
        venue: String,
        symbol: String,
        decision_ms: i64,
        lookback_ms: i64,
    },
    /// `StudyContext::institutional_holdings_as_of`, asked as the point-in-time read it is.
    InstitutionalHoldingsAsOf {
        venue: String,
        symbol: String,
        decision_ms: i64,
        lookback_ms: i64,
    },
    /// `StudyContext::text_scores_as_of`: the point-in-time score read, asked the way [`Call::TextsAsOf`] is.
    TextScoresAsOf {
        text_kind: String,
        model_name: String,
        decision_ms: i64,
        lookback_ms: i64,
    },
    /// `StudyContext::kept_instruments`: today's kept rows, answered as the parent read them, the
    /// time it read them included (`crate::KeptInstruments`), so the study's process opens no file.
    KeptInstruments,
    /// `StudyContext::kept_contract_value`: what one unit of a kept contract is worth, valued on the parent's
    /// side (it links the catalog; the study file does not). A parent older than this call answers it with
    /// nothing, which the study file reads as a refusal in words, never as a unit of 1.0.
    KeptContractValue {
        venue: String,
        symbol: String,
    },
    Fit {
        data: Train,
        point: Point,
        seed: u64,
    },
    FitCaptured {
        data: Train,
        point: Point,
        seed: u64,
        importance: bool,
        text: bool,
    },
    FitIdentity {
        data: Train,
        point: Point,
        seed: u64,
    },
    /// `vike_ml_training::Learner::fit_chained`: a [`Call::FitCaptured`] as the next fold of the
    /// walk-forward chain `chain` at `step`, answered as `FitCaptured` is.
    FitChained {
        data: Train,
        point: Point,
        seed: u64,
        importance: bool,
        text: bool,
        chain: String,
        step: i64,
    },
    /// Score one row with a model the parent HOLDS ([`Model::Held`]): its kind's count of numbers.
    Predict {
        model: u64,
        row: Vec<f64>,
    },
    /// Score a row-major matrix of `n_features`-wide rows with a held model in ONE crossing
    /// (`vike_ml_prediction::Predictor::predict_batch`): each row's [`Call::Predict`] answer, in row
    /// order, or the first row's refusal.
    PredictBatch {
        model: u64,
        rows: Vec<f64>,
        n_features: usize,
    },
    /// The study dropped a held model.
    DropModel {
        model: u64,
    },
    /// The study's one backtest: the profile (TOML text) and the store it handed over, by the number
    /// the study file keeps it under (the parent reads it back through `vike_study_serve`).
    Sim {
        profile: String,
        store: u64,
    },
    /// The option-chain read verb. Only the range scan crosses: the point-in-time fold
    /// (`StudyContext::chain_as_of`) runs beside the study, over the rows this answers.
    Chain {
        venue: String,
        underlying: String,
        range: TimestampRange,
    },
    /// The forecasts of the saved run `run_id` (book W21), read by the parent's `StudyRuns`.
    RunForecasts {
        run_id: String,
    },
    /// `StudyContext::execution_fills`: every row of the account's own fills in `range`.
    ExecutionFills {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    /// `StudyContext::execution_orders`: every order lifecycle row in `range`.
    ExecutionOrders {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    /// The closed trades of the saved run `run_id` (book chapter 22), read by the parent's `StudyRuns`.
    RunTrades {
        run_id: String,
    },
    /// What the forward run `run_id` recorded (book chapter 22), read by the parent's `StudyRuns`.
    RunForward {
        run_id: String,
    },
    /// `StudyContext::forecasts_as_of`: the point-in-time read of the live forecasts, asked as it was
    /// written (the decision time, never a range), so the parent answers nothing decided after it.
    ForecastsAsOf {
        venue: String,
        symbol: String,
        decision_ms: i64,
        start_ms: Option<i64>,
    },
    /// `StudyContext::bars_in_lane`: the bars of ONE `source=` lane, with the row budget the study's
    /// store was asked under (none from the context's own verb). Last, so no earlier variant's index moves.
    BarsInLane {
        lane: String,
        venue: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
        budget: Option<usize>,
    },
}

/// `vike_user_research::SimOutcome`, as it crosses.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct SimAnswer {
    pub(crate) total_return: f64,
    pub(crate) n_trades: u64,
    pub(crate) win_rate: f64,
}

/// A `vike_ml_training::TrainData`, owned: every field, so the learner on the other side is handed the
/// matrix the study built, row count included.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Train {
    pub(crate) x: Vec<f64>,
    pub(crate) y: Vec<f32>,
    pub(crate) n_rows: usize,
    pub(crate) n_cols: usize,
    pub(crate) categorical: Vec<usize>,
}

impl Train {
    pub(crate) fn of(d: &TrainData<'_>) -> Self {
        Self {
            x: d.x.to_vec(),
            y: d.y.to_vec(),
            n_rows: d.n_rows,
            n_cols: d.n_cols,
            categorical: d.categorical.to_vec(),
        }
    }

    pub(crate) fn data(&self) -> TrainData<'_> {
        TrainData {
            x: &self.x,
            y: &self.y,
            n_rows: self.n_rows,
            n_cols: self.n_cols,
            categorical: &self.categorical,
        }
    }
}

/// A `vike_ml_training::GridPoint`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) struct Point {
    pub(crate) num_leaves: u32,
    pub(crate) max_depth: i32,
    pub(crate) min_data_in_leaf: u32,
    pub(crate) learning_rate: f64,
    pub(crate) feature_fraction: f64,
}

impl From<&GridPoint> for Point {
    fn from(p: &GridPoint) -> Self {
        Self {
            num_leaves: p.num_leaves,
            max_depth: p.max_depth,
            min_data_in_leaf: p.min_data_in_leaf,
            learning_rate: p.learning_rate,
            feature_fraction: p.feature_fraction,
        }
    }
}

impl From<Point> for GridPoint {
    fn from(p: Point) -> Self {
        Self {
            num_leaves: p.num_leaves,
            max_depth: p.max_depth,
            min_data_in_leaf: p.min_data_in_leaf,
            learning_rate: p.learning_rate,
            feature_fraction: p.feature_fraction,
        }
    }
}

/// A fitted model as it comes back: the learner's own TEXT, which the study parses beside itself
/// (`vike_ml_prediction::parse_model_text`, the parse the learner made), or a number the parent holds it
/// under with the KIND of answer the held model declares (a network's comes from its provenance; never
/// assumed). A held model's [`Call::Predict`] answers `kind`'s count of numbers.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Model {
    Text(String),
    Held { model: u64, kind: Kind },
}

/// A `vike_ml_prediction::PredictionKind`, variant for variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub(crate) enum Kind {
    Probability,
    ClassProbabilities { class_count: usize },
    Value,
}

impl From<PredictionKind> for Kind {
    fn from(kind: PredictionKind) -> Self {
        match kind {
            PredictionKind::Probability => Kind::Probability,
            PredictionKind::ClassProbabilities { class_count } => {
                Kind::ClassProbabilities { class_count }
            }
            PredictionKind::Value => Kind::Value,
        }
    }
}

impl From<Kind> for PredictionKind {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Probability => PredictionKind::Probability,
            Kind::ClassProbabilities { class_count } => {
                PredictionKind::ClassProbabilities { class_count }
            }
            Kind::Value => PredictionKind::Value,
        }
    }
}

/// A `vike_ml_prediction::FitImportance`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Importance {
    pub(crate) n_features: usize,
    pub(crate) gain: Vec<f64>,
    pub(crate) splits: Vec<u64>,
}

impl Importance {
    pub(crate) fn of(i: FitImportance) -> Self {
        Self { n_features: i.n_features, gain: i.gain, splits: i.splits }
    }

    pub(crate) fn importance(self) -> FitImportance {
        FitImportance { n_features: self.n_features, gain: self.gain, splits: self.splits }
    }
}

/// A store answer: the rows (or value), or the store's own refusal (`vike_data::HistoryStoreError`'s serde
/// form, variant for variant), so a study sees the store's words.
pub(crate) fn answered<T: Serialize>(answer: Result<T, HistoryStoreError>) -> Vec<u8> {
    encode(&answer)
}

/// A `crate::StudyOutcome`: metrics and artifacts in emission order.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub(crate) metrics: Vec<(String, f64)>,
    pub(crate) artifacts: Vec<(String, String)>,
}

impl Outcome {
    pub(crate) fn of(o: &StudyOutcome) -> Self {
        Self { metrics: o.metrics().to_vec(), artifacts: o.artifacts().to_vec() }
    }

    /// The outcome rebuilt through its own checks (which it passed once, beside the study).
    pub(crate) fn outcome(self) -> Result<StudyOutcome, StudyError> {
        let mut out = StudyOutcome::new();
        for (name, value) in self.metrics {
            out.metric(&name, value)?;
        }
        for (name, body) in self.artifacts {
            out.artifact(&name, body)?;
        }
        Ok(out)
    }
}

/// A `crate::StudyError`, variant for variant.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Failure {
    Study(String),
    Data(HistoryStoreError),
    NoLearner(String),
    NoSim(String),
}

impl From<StudyError> for Failure {
    fn from(e: StudyError) -> Self {
        match e {
            StudyError::Study(m) => Self::Study(m),
            StudyError::Data(e) => Self::Data(e),
            StudyError::NoLearner(m) => Self::NoLearner(m),
            StudyError::NoSim(m) => Self::NoSim(m),
        }
    }
}

impl From<Failure> for StudyError {
    fn from(f: Failure) -> Self {
        match f {
            Failure::Study(m) => Self::Study(m),
            Failure::Data(e) => Self::Data(e),
            Failure::NoLearner(m) => Self::NoLearner(m),
            Failure::NoSim(m) => Self::NoSim(m),
        }
    }
}

/// What the study file answers when it is done.
pub(crate) type Finished = Result<Outcome, Failure>;

/// A request the HOST makes of a store the study handed out (its one backtest's): which store, and
/// the verb. The verbs are the ones a backtest reads, the sandboxed backtest's own list
/// (`crates/vike-backtesthub/src/strategy_sandbox/pipe_store.rs`'s `PipeHistStore`), plus the
/// instrument rows and the four reads the trait requires.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StoreRequest {
    pub(crate) store: u64,
    pub(crate) call: StoreCall,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum StoreCall {
    LoadBars {
        venue: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
    },
    LoadBarsSourcedCapped {
        venue: String,
        source: String,
        symbol: String,
        interval: String,
        range: TimestampRange,
        budget: Option<usize>,
    },
    ScanQuotes {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanTrades {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanBookUpdates {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanDepth {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanCohort {
        venue: String,
        asset: String,
        range: TimestampRange,
    },
    ScanPerpMetrics {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanSymbolProperties {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanEquity {
        venue: String,
        symbol: String,
        range: TimestampRange,
    },
    ScanExecFills {
        venue: String,
        symbol: String,
    },
    ScanExecOrders {
        venue: String,
        symbol: String,
    },
    PropertiesAsOf {
        venue: String,
        symbol: String,
        timestamp: i64,
    },
    ListSeries,
    Inventory,
    SeriesGaps(SeriesId),
    SeriesFacts(SeriesId),
    KeptInstruments,
    StockSplits,
    /// The option-chain rows of `(venue, underlying)`: a backtest whose profile trades `[data.options]` legs.
    ScanChain {
        venue: String,
        underlying: String,
        range: TimestampRange,
    },
}
