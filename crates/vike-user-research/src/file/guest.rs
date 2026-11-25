//! The study FILE's side: the context a study is handed, built over its host, and the answer it
//! gives when it is done. The template's two exports call [`run`] and [`serve`]
//! (`crates/vike-rust-strategy/template/study_lib.rs.in`).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_data::{
    ChainRow, CohortRow, ExecFillRow, ExecForecastRow, ExecOrderRow, FinancialFactRow,
    HistoryStore, HistoryStoreError, InstitutionalHoldingRow, PerpMetricRow, TextRow, TextScoreRow,
    TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_ml_prediction::{GbdtModel, PredictionKind, Predictor, parse_model_text};
use vike_ml_training::{Capture, CapturedFit, GridPoint, Learner, TrainData};
use vike_model::{ContractValue, EquitySample, SymbolProperties};

use super::wire::{
    Call, Finished, Importance, Model, Outcome, Point, SimAnswer, Start, StoreCall, StoreRequest,
    Train, answered, decode, encode,
};
use crate::{KeptInstruments, SimOutcome, StudyContext, StudyFn, StudyRuns, StudySim};

/// The host as the study file holds it: one call, bytes in, bytes out, from any thread.
pub type Host = Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

/// Run `entry` over the start message `input`, every request it makes answered by `host`; the
/// finished answer. A study that panics answers a study error naming the panic.
pub fn run(entry: StudyFn, input: &[u8], host: Host) -> Vec<u8> {
    let finished: Finished = start(entry, input, host);
    encode(&finished)
}

fn start(entry: StudyFn, input: &[u8], host: Host) -> Finished {
    use super::wire::Failure;
    let start: Start = decode(input)
        .map_err(|e| Failure::Study(format!("the study file's start message: {e}")))?;
    // ⚠ `toml::from_str`, NOT `text.parse()`: the pinned `toml` parses a single VALUE with `parse`.
    let params: toml::Value = toml::from_str(&start.params)
        .map_err(|e| Failure::Study(format!("the recipe did not read back: {e}")))?;
    let kept = host.clone();
    let valued = host.clone();
    let mut ctx = StudyContext::new(
        Arc::new(HostStore { host: host.clone() }),
        start.window,
        PathBuf::from(start.scratch),
    )
    .with_kept_instruments(Arc::new(move || {
        match ask::<Result<KeptInstruments, HistoryStoreError>>(&kept, &Call::KeptInstruments) {
            Ok(answer) => answer,
            Err(e) => Err(HistoryStoreError::Io(format!("kept_instruments: {e}"))),
        }
    }))
    .with_contract_values_asked(Arc::new(move |venue: &str, symbol: &str| {
        let call = Call::KeptContractValue { venue: venue.to_string(), symbol: symbol.to_string() };
        match ask::<Result<Option<ContractValue>, HistoryStoreError>>(&valued, &call) {
            Ok(answer) => answer,
            Err(e) => Err(HistoryStoreError::Io(format!(
                "kept_contract_value of {venue} {symbol}: {e} (a host older than this study file answers \
                 nothing to it)"
            ))),
        }
    }));
    if start.learner {
        ctx = ctx.with_learner(Arc::new(HostLearner { host: host.clone() }));
    }
    if start.runs {
        ctx = ctx.with_runs(Arc::new(HostRuns { host: host.clone() }));
    }
    if start.sim {
        ctx = ctx.with_sim(Arc::new(HostSim { host }));
    }
    match catch_unwind(AssertUnwindSafe(|| entry(&ctx, &params))) {
        Ok(Ok(outcome)) => Ok(Outcome::of(&outcome)),
        Ok(Err(e)) => Err(e.into()),
        Err(panic) => Err(Failure::Study(format!("the study panicked: {}", panic_text(&*panic)))),
    }
}

/// A panic's message, when it carried one.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(no message)".to_string())
}

/// Ask `host` and read its typed answer.
fn ask<T: DeserializeOwned>(host: &Host, message: &impl Serialize) -> Result<T, String> {
    decode(&host(&encode(message)))
}

// ---- the context's store ---------------------------------------------------------------------

/// The store a study's context reads: each read verb asked of the parent. Only the context's read
/// verbs reach it (`StudyContext` holds it privately; its `chain_as_of` is the trait's own fold over
/// `scan_chain`), so every other verb is unreachable here. The
/// kept instruments are asked of the parent beside it ([`start`]), with the time the parent read them.
struct HostStore {
    host: Host,
}

impl HostStore {
    fn rows<T: DeserializeOwned>(
        &self,
        verb: &str,
        call: &Call,
    ) -> Result<Vec<T>, HistoryStoreError> {
        match ask::<Result<Vec<T>, HistoryStoreError>>(&self.host, call) {
            Ok(answer) => answer,
            Err(e) => Err(HistoryStoreError::Io(format!("{verb}: {e}"))),
        }
    }

    fn unreachable(verb: &str) -> HistoryStoreError {
        HistoryStoreError::Unsupported(format!(
            "{verb}: a study's context reads the store through its own verbs only"
        ))
    }
}

impl HistoryStore for HostStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let call = Call::Bars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
        };
        self.rows("bars", &call)
    }

    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let call = Call::BarsInLane {
            lane: source.into(),
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
            budget,
        };
        self.rows("bars_in_lane", &call)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let call = Call::Quotes { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("quotes", &call)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let call = Call::Trades { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("trades", &call)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let call = Call::BookUpdates { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("book_updates", &call)
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let call = Call::Depth { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("depth", &call)
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let call = Call::Cohort { venue: venue.into(), asset: asset.into(), range };
        self.rows("cohort", &call)
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let call = Call::PerpMetrics { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("perp_metrics", &call)
    }

    /// Asked of the parent as the point-in-time read it is, never as the range read under it.
    fn texts_as_of(
        &self,
        text_kind: &str,
        collector: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        let call = Call::TextsAsOf {
            text_kind: text_kind.into(),
            collector: collector.into(),
            decision_ms,
            lookback_ms,
        };
        self.rows("texts_as_of", &call)
    }

    /// The context reads texts through `texts_as_of` alone, so the range read is not one of its verbs.
    fn scan_texts(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<TextRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_texts"))
    }

    /// The parent's range scan; `chain_as_of_within` is the trait's own fold over it, here.
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let call = Call::Chain { venue: venue.into(), underlying: underlying.into(), range };
        self.rows("chain", &call)
    }

    /// Asked of the parent as the point-in-time read it is, never as the range read under it.
    fn financial_facts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        let call = Call::FinancialFactsAsOf {
            venue: venue.into(),
            symbol: symbol.into(),
            decision_ms,
            lookback_ms,
        };
        self.rows("financial_facts_as_of", &call)
    }

    /// The context reads values through `financial_facts_as_of` alone.
    fn scan_financial_facts(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<FinancialFactRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_financial_facts"))
    }

    /// Asked of the parent as the point-in-time read it is, never as the range read under it.
    fn institutional_holdings_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        let call = Call::InstitutionalHoldingsAsOf {
            venue: venue.into(),
            symbol: symbol.into(),
            decision_ms,
            lookback_ms,
        };
        self.rows("institutional_holdings_as_of", &call)
    }

    /// The context reads 13F lines through `institutional_holdings_as_of` alone.
    fn scan_institutional_holdings(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<InstitutionalHoldingRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_institutional_holdings"))
    }

    /// Asked of the parent as the point-in-time read it is, as [`Self::texts_as_of`] is.
    fn text_scores_as_of(
        &self,
        text_kind: &str,
        model_name: &str,
        decision_ms: i64,
        lookback_ms: i64,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        let call = Call::TextScoresAsOf {
            text_kind: text_kind.into(),
            model_name: model_name.into(),
            decision_ms,
            lookback_ms,
        };
        self.rows("text_scores_as_of", &call)
    }

    /// The context reads scores through `text_scores_as_of` alone, so the range read is not one of
    /// its verbs.
    fn scan_text_scores(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<TextScoreRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_text_scores"))
    }

    /// Asked of the parent as the point-in-time read it is, never as the range read under it.
    fn forecasts_as_of(
        &self,
        venue: &str,
        symbol: &str,
        decision_ms: i64,
        start_ms: Option<i64>,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        let call = Call::ForecastsAsOf {
            venue: venue.into(),
            symbol: symbol.into(),
            decision_ms,
            start_ms,
        };
        self.rows("forecasts_as_of", &call)
    }

    /// The context reads forecasts through `forecasts_as_of` alone, so the range read is not one of its verbs.
    fn scan_forecasts(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<ExecForecastRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_forecasts"))
    }

    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_bars"))
    }

    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[TickQuote],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_quotes"))
    }

    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TickTrade],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_trades"))
    }

    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[L2BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_symbol_properties"))
    }

    fn scan_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        Err(Self::unreachable("scan_symbol_properties"))
    }

    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_equity"))
    }

    fn scan_equity(
        &self,
        _: &str,
        _: &str,
        _: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        Err(Self::unreachable("scan_equity"))
    }

    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_exec_fills"))
    }

    fn scan_exec_fills(&self, _: &str, _: &str) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_exec_fills"))
    }

    /// Every row in `range`, whatever `n` asks: the context asks for all of them (`usize::MAX`),
    /// and "every row" meets the head's "at least `n` unless it is all" for any `n`.
    fn scan_execution_fills_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        _: usize,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let call = Call::ExecutionFills { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("execution_fills", &call)
    }

    /// Every row in `range`: [`Self::scan_execution_fills_head`]'s reasoning.
    fn scan_execution_orders_head(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
        _: usize,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        let call = Call::ExecutionOrders { venue: venue.into(), symbol: symbol.into(), range };
        self.rows("execution_orders", &call)
    }

    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("append_exec_orders"))
    }

    fn scan_exec_orders(&self, _: &str, _: &str) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        Err(Self::unreachable("scan_exec_orders"))
    }

    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::unreachable("resample_trades_to_bars"))
    }
}

// ---- the learner -----------------------------------------------------------------------------

/// The learner a study's context offers: every fit made by the parent, which holds the trainer.
struct HostLearner {
    host: Host,
}

/// A model the learner fitted: parsed beside the study from the learner's own text, or held by the
/// parent and asked per row, or per matrix (`Predictor::predict_batch`).
enum HostModel {
    Parsed(GbdtModel),
    /// Held by the parent; `kind` is the one the held model DECLARED (a network's comes from its
    /// provenance), never assumed.
    Held {
        model: u64,
        kind: PredictionKind,
        host: Host,
    },
}

impl HostModel {
    fn of(model: Model, host: &Host) -> Result<Self, String> {
        match model {
            Model::Text(text) => parse_model_text(&text)
                .map(HostModel::Parsed)
                .map_err(|e| format!("the fitted model's text did not parse: {e}")),
            Model::Held { model, kind } => {
                Ok(HostModel::Held { model, kind: kind.into(), host: host.clone() })
            }
        }
    }
}

impl Predictor for HostModel {
    fn kind(&self) -> PredictionKind {
        match self {
            HostModel::Parsed(model) => Predictor::kind(model),
            HostModel::Held { kind, .. } => *kind,
        }
    }

    fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
        match self {
            HostModel::Parsed(model) => Predictor::predict_into(model, row, out),
            HostModel::Held { model, kind, host } => {
                if out.len() != kind.output_count() {
                    return Err(format!(
                        "held model {model} answers {} number(s) per row ({kind:?}); the buffer \
                         holds {}",
                        kind.output_count(),
                        out.len()
                    ));
                }
                let call = Call::Predict { model: *model, row: row.to_vec() };
                // A refusal is the parent's sentence, never a guessed number.
                let answer = ask::<Result<Vec<f64>, String>>(host, &call)
                    .and_then(|answer| answer)
                    .map_err(|e| format!("held model {model} could not score a row: {e}"))?;
                if answer.len() != out.len() {
                    return Err(format!(
                        "held model {model} answered {} number(s), not {}",
                        answer.len(),
                        out.len()
                    ));
                }
                out.copy_from_slice(&answer);
                Ok(())
            }
        }
    }

    /// A held model's matrix crosses ONCE (`Call::PredictBatch`), not once per row: the same
    /// numbers in row order, and the first row's refusal in the per-row words.
    fn predict_batch(&self, flat_x: &[f64], n_features: usize) -> Result<Vec<f64>, String> {
        match self {
            HostModel::Parsed(model) => Predictor::predict_batch(model, flat_x, n_features),
            HostModel::Held { model, kind, host } => {
                let call = Call::PredictBatch { model: *model, rows: flat_x.to_vec(), n_features };
                let answer = ask::<Result<Vec<f64>, String>>(host, &call)
                    .and_then(|answer| answer)
                    .map_err(|e| format!("held model {model} could not score a row: {e}"))?;
                let want = flat_x.len().checked_div(n_features).unwrap_or(0) * kind.output_count();
                if answer.len() != want {
                    return Err(format!(
                        "held model {model} answered {} number(s), not {want}",
                        answer.len()
                    ));
                }
                Ok(answer)
            }
        }
    }
}

impl Drop for HostModel {
    fn drop(&mut self) {
        if let HostModel::Held { model, host, .. } = self {
            let _ = host(&encode(&Call::DropModel { model: *model }));
        }
    }
}

impl Learner for HostLearner {
    type Model = HostModel;

    fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<HostModel, String> {
        let call = Call::Fit { data: Train::of(d), point: Point::from(p), seed };
        let model = ask::<Result<Model, String>>(&self.host, &call)??;
        HostModel::of(model, &self.host)
    }

    fn fit_identity(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Option<[u8; 32]> {
        let call = Call::FitIdentity { data: Train::of(d), point: Point::from(p), seed };
        ask::<Option<[u8; 32]>>(&self.host, &call).ok().flatten()
    }

    fn fit_captured(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
    ) -> Result<CapturedFit<HostModel>, String> {
        let call = Call::FitCaptured {
            data: Train::of(d),
            point: Point::from(p),
            seed,
            importance: want.importance,
            text: want.text,
        };
        let (model, importance, text) =
            ask::<Result<(Model, Option<Importance>, Option<String>), String>>(&self.host, &call)??;
        Ok((HostModel::of(model, &self.host)?, importance.map(Importance::importance), text))
    }

    fn fit_chained(
        &self,
        d: &TrainData<'_>,
        p: &GridPoint,
        seed: u64,
        want: Capture,
        chain: &str,
        step: i64,
    ) -> Result<CapturedFit<HostModel>, String> {
        let call = Call::FitChained {
            data: Train::of(d),
            point: Point::from(p),
            seed,
            importance: want.importance,
            text: want.text,
            chain: chain.to_string(),
            step,
        };
        let (model, importance, text) =
            ask::<Result<(Model, Option<Importance>, Option<String>), String>>(&self.host, &call)??;
        Ok((HostModel::of(model, &self.host)?, importance.map(Importance::importance), text))
    }
}

// ---- the saved runs ------------------------------------------------------------------------

/// The saved runs a study's context offers: each read asked of the parent, which holds the runs root.
struct HostRuns {
    host: Host,
}

impl StudyRuns for HostRuns {
    fn forecasts(&self, run_id: &str) -> Result<Vec<vike_model::ForecastRow>, String> {
        let call = Call::RunForecasts { run_id: run_id.to_string() };
        ask::<Result<Vec<vike_model::ForecastRow>, String>>(&self.host, &call)
            .map_err(|e| format!("run {run_id}'s forecasts: {e}"))?
    }

    fn trades(&self, run_id: &str) -> Result<Vec<vike_model::Trade>, String> {
        let call = Call::RunTrades { run_id: run_id.to_string() };
        ask::<Result<Vec<vike_model::Trade>, String>>(&self.host, &call)
            .map_err(|e| format!("run {run_id}'s trades: {e}"))?
    }

    fn forward(&self, run_id: &str) -> Result<crate::StudyForwardRun, String> {
        let call = Call::RunForward { run_id: run_id.to_string() };
        ask::<Result<crate::StudyForwardRun, String>>(&self.host, &call)
            .map_err(|e| format!("forward run {run_id}: {e}"))?
    }
}

// ---- the one backtest ------------------------------------------------------------------------

/// The stores a study handed its backtest, by number, for as long as that backtest runs: what the
/// host's reads of them ([`serve`]) reach.
type HandedStores = Vec<(u64, Arc<dyn HistoryStore + Send + Sync>)>;
static STORES: Mutex<HandedStores> = Mutex::new(Vec::new());
static NEXT_STORE: AtomicU64 = AtomicU64::new(0);

/// The backtest a study's context offers: run by the parent (the host's `StudySim`), over the store
/// the study hands it, read back through [`serve`].
struct HostSim {
    host: Host,
}

impl StudySim for HostSim {
    fn run_one(
        &self,
        profile: &toml::Value,
        store: Arc<dyn HistoryStore + Send + Sync>,
    ) -> Result<SimOutcome, String> {
        let profile = toml::to_string(profile)
            .map_err(|e| format!("profile does not describe a backtest: {e}"))?;
        let id = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
        STORES.lock().unwrap_or_else(PoisonError::into_inner).push((id, store));
        let answer =
            ask::<Result<SimAnswer, String>>(&self.host, &Call::Sim { profile, store: id });
        STORES.lock().unwrap_or_else(PoisonError::into_inner).retain(|(key, _)| *key != id);
        let answer = answer.map_err(|e| format!("the backtest: {e}"))??;
        Ok(SimOutcome {
            total_return: answer.total_return,
            n_trades: answer.n_trades,
            win_rate: answer.win_rate,
        })
    }
}

/// Answer one read the host makes of a store this study handed its backtest.
pub fn serve(request: &[u8]) -> Vec<u8> {
    let StoreRequest { store, call } = match decode::<StoreRequest>(request) {
        Ok(request) => request,
        Err(e) => return answered::<()>(Err(HistoryStoreError::Io(format!("a store read: {e}")))),
    };
    let held = STORES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(key, _)| *key == store)
        .map(|(_, s)| s.clone());
    let Some(s) = held else {
        return answered::<()>(Err(HistoryStoreError::Io(format!(
            "store {store} was read after its backtest ended"
        ))));
    };
    match call {
        StoreCall::LoadBars { venue, symbol, interval, range } => {
            answered(s.load_bars(&venue, &symbol, &interval, range))
        }
        StoreCall::LoadBarsSourcedCapped { venue, source, symbol, interval, range, budget } => {
            answered(s.load_bars_sourced_capped(&venue, &source, &symbol, &interval, range, budget))
        }
        StoreCall::ScanQuotes { venue, symbol, range } => {
            answered(s.scan_quotes(&venue, &symbol, range))
        }
        StoreCall::ScanTrades { venue, symbol, range } => {
            answered(s.scan_trades(&venue, &symbol, range))
        }
        StoreCall::ScanBookUpdates { venue, symbol, range } => {
            answered(s.scan_book_updates(&venue, &symbol, range))
        }
        StoreCall::ScanDepth { venue, symbol, range } => {
            answered(s.scan_depth(&venue, &symbol, range))
        }
        StoreCall::ScanCohort { venue, asset, range } => {
            answered(s.scan_cohort(&venue, &asset, range))
        }
        StoreCall::ScanPerpMetrics { venue, symbol, range } => {
            answered(s.scan_perp_metrics(&venue, &symbol, range))
        }
        StoreCall::ScanSymbolProperties { venue, symbol, range } => {
            answered(s.scan_symbol_properties(&venue, &symbol, range))
        }
        StoreCall::ScanEquity { venue, symbol, range } => {
            answered(s.scan_equity(&venue, &symbol, range))
        }
        StoreCall::ScanExecFills { venue, symbol } => answered(s.scan_exec_fills(&venue, &symbol)),
        StoreCall::ScanExecOrders { venue, symbol } => {
            answered(s.scan_exec_orders(&venue, &symbol))
        }
        StoreCall::PropertiesAsOf { venue, symbol, timestamp } => {
            answered(s.properties_as_of(&venue, &symbol, timestamp))
        }
        StoreCall::ListSeries => answered(s.list_series()),
        StoreCall::Inventory => answered(s.inventory()),
        StoreCall::SeriesGaps(id) => answered(s.series_gaps(&id)),
        StoreCall::SeriesFacts(id) => answered(s.series_facts(&id)),
        StoreCall::KeptInstruments => answered(s.kept_instruments()),
        StoreCall::StockSplits => answered(s.stock_splits()),
        StoreCall::ScanChain { venue, underlying, range } => {
            answered(s.scan_chain(&venue, &underlying, range))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use vike_data::HistoryStoreMemory;
    use vike_ml_training::DEFAULT_POINT;

    use super::super::host::Parent;
    use super::*;

    /// A model with no text, so the parent HOLDS it: two numbers per row, a row starting below
    /// zero refused.
    struct Pair;

    impl Predictor for Pair {
        fn kind(&self) -> PredictionKind {
            PredictionKind::ClassProbabilities { class_count: 2 }
        }

        fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
            if row[0] < 0.0 {
                return Err(format!("row {} refused", row[0]));
            }
            out.copy_from_slice(&[row[0] + row[1], row[0] - row[1]]);
            Ok(())
        }
    }

    struct PairLearner;

    impl Learner for PairLearner {
        type Model = Pair;

        fn fit(&self, _: &TrainData<'_>, _: &GridPoint, _: u64) -> Result<Pair, String> {
            Ok(Pair)
        }
    }

    /// Both halves in this process, every crossing counted: a held model's batch is ONE crossing
    /// for the matrix where row by row is one per row, with the same numbers in the same order and
    /// the same refusal.
    #[test]
    fn a_held_models_batch_crosses_once_and_answers_what_each_row_answers() {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let ctx = StudyContext::new(store, TimestampRange::of(0, 1), PathBuf::from("unused"))
            .with_learner(Arc::new(PairLearner));
        // Leaked: the host is `'static`, as the study file's is.
        let parent =
            Arc::new(Parent::new(Box::leak(Box::new(ctx)), Arc::new(|_: &[u8]| Vec::new())));
        let crossings = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&crossings);
        let host: Host = Arc::new(move |request: &[u8]| {
            counted.fetch_add(1, Ordering::Relaxed);
            parent.answer(request)
        });
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        let model = HostLearner { host }.fit(&data, &DEFAULT_POINT, 0).expect("a fit");
        assert!(matches!(model, HostModel::Held { .. }), "a model with no text is held");

        let rows = [1.0, 2.0, 3.0, 5.0, 8.0, 13.0];
        crossings.store(0, Ordering::Relaxed);
        let mut per_row = Vec::new();
        for row in rows.chunks(2) {
            let mut out = [0.0; 2];
            model.predict_into(row, &mut out).expect("a row");
            per_row.extend(out);
        }
        assert_eq!(crossings.load(Ordering::Relaxed), 3, "row by row crosses once per row");
        crossings.store(0, Ordering::Relaxed);
        assert_eq!(model.predict_batch(&rows, 2), Ok(per_row));
        assert_eq!(crossings.load(Ordering::Relaxed), 1, "a batch crosses once for the matrix");

        let refused = [1.0, 2.0, -3.0, 5.0, -8.0, 13.0];
        let first = model.predict_into(&refused[2..4], &mut [0.0; 2]).expect_err("refused");
        assert_eq!(model.predict_batch(&refused, 2), Err(first), "the first row's refusal");
        assert!(model.predict_batch(&rows, 4).expect_err("ragged").contains("whole rows"));
    }

    /// Answers only chained fits, recording each chain and step it was asked; a negative step is
    /// refused in words.
    struct ChainLearner(std::sync::Mutex<Vec<(String, i64)>>);

    impl Learner for ChainLearner {
        type Model = Pair;

        fn fit(&self, _: &TrainData<'_>, _: &GridPoint, _: u64) -> Result<Pair, String> {
            Err("only chained fits".to_string())
        }

        fn fit_chained(
            &self,
            _: &TrainData<'_>,
            _: &GridPoint,
            _: u64,
            _: Capture,
            chain: &str,
            step: i64,
        ) -> Result<CapturedFit<Pair>, String> {
            if step < 0 {
                return Err(format!("chain `{chain}`: step {step} refused"));
            }
            self.0.lock().expect("unpoisoned").push((chain.to_string(), step));
            Ok((Pair, None, None))
        }
    }

    /// A study's chained fit reaches the parent's learner AS a chained fit, with its chain and
    /// step, and the learner's refusal comes back in its words.
    #[test]
    fn a_chained_fit_crosses_with_its_chain_and_step() {
        let learner = Arc::new(ChainLearner(std::sync::Mutex::default()));
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let ctx = StudyContext::new(store, TimestampRange::of(0, 1), PathBuf::from("unused"))
            .with_learner(learner.clone());
        let parent =
            Arc::new(Parent::new(Box::leak(Box::new(ctx)), Arc::new(|_: &[u8]| Vec::new())));
        let host: Host = Arc::new(move |request: &[u8]| parent.answer(request));
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        let study = HostLearner { host };
        let want = Capture::default();
        let (model, _, _) =
            study.fit_chained(&data, &DEFAULT_POINT, 0, want, "long", 7).expect("a chained fit");
        assert!(matches!(model, HostModel::Held { .. }), "a model with no text is held");
        let refused = study.fit_chained(&data, &DEFAULT_POINT, 0, want, "long", -1).map(|_| ());
        assert_eq!(refused, Err("chain `long`: step -1 refused".to_string()));
        assert_eq!(*learner.0.lock().expect("unpoisoned"), vec![("long".to_string(), 7)]);
    }
}
