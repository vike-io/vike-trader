//! The HOST's side of a study file: the start message, the answers of the context, the learner and
//! the one backtest, and the finished outcome read back. All of it is the PARENT's (the process that
//! holds the store and the trainer and can start a strategy file's own sandboxed backtest); the
//! sandboxed child only relays, and answers the parent's reads of the store a study hands its backtest
//! ([`Serve`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::de::DeserializeOwned;
use vike_data::{
    ChainRow, CohortRow, ExecFillRow, ExecOrderRow, HistoryStore, HistoryStoreError, PerpMetricRow,
    SeriesCoverage, SeriesId, TimestampRange,
};
use vike_marketdata::{Bar, L2BookUpdate, TickQuote, TickTrade};
use vike_ml_prediction::Predictor;
use vike_ml_training::Capture;
use vike_model::{EquitySample, SymbolProperties};

use super::wire::{
    Call, Finished, Importance, Model, SimAnswer, Start, StoreCall, StoreRequest, answered, decode,
    encode,
};
use crate::{StudyContext, StudyError, StudyOutcome};

/// The start message for a study file run over `ctx` with the recipe `params`.
///
/// # Errors
/// A recipe that cannot be written as TOML text (a study error: nothing was started).
pub fn start(ctx: &StudyContext, params: &toml::Value) -> Result<Vec<u8>, StudyError> {
    let params = toml::to_string(params)
        .map_err(|e| StudyError::Study(format!("the recipe could not be handed over: {e}")))?;
    Ok(encode(&Start {
        params,
        window: ctx.window(),
        scratch: ctx.scratch().to_string_lossy().into_owned(),
        learner: ctx.learner().is_some(),
        sim: ctx.sim().is_some(),
        runs: ctx.runs().is_some(),
    }))
}

/// The study file's finished answer, read back into what the study returned.
///
/// # Errors
/// The study's own error, variant for variant; an answer that does not read is a study error saying
/// the file ended without finishing.
pub fn finished(answer: &[u8]) -> Result<StudyOutcome, StudyError> {
    match decode::<Finished>(answer) {
        Ok(Ok(outcome)) => outcome.outcome(),
        Ok(Err(failure)) => Err(failure.into()),
        Err(e) => Err(StudyError::Study(format!("the study file ended without finishing: {e}"))),
    }
}

/// The finished answer of a study file that could not be run at all (its artifact would not load):
/// the refusal, as the study's own error.
pub fn refused(why: &str) -> Vec<u8> {
    encode(&Finished::Err(super::wire::Failure::Study(why.to_string())))
}

/// How the parent reads a store the study handed its backtest: a request to the study file's
/// `vike_study_serve`, carried to wherever the study file runs.
pub type Serve = Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;

/// The PARENT's answers to a study file run over `ctx`: the context's read verbs, its learner and its
/// one backtest. The models a learner fits whose text it cannot hand back are HELD here, and scored
/// per row or per matrix. `serve` reaches the study file, for the store a study hands its backtest.
pub struct Parent<'a> {
    ctx: &'a StudyContext,
    serve: Serve,
    held: Mutex<HashMap<u64, Arc<dyn Predictor>>>,
    next: AtomicU64,
}

impl<'a> Parent<'a> {
    pub fn new(ctx: &'a StudyContext, serve: Serve) -> Self {
        Self { ctx, serve, held: Mutex::new(HashMap::new()), next: AtomicU64::new(0) }
    }

    /// Answer one request. Callable from several threads at once: a study asks from its own.
    pub fn answer(&self, request: &[u8]) -> Vec<u8> {
        let call = match decode::<Call>(request) {
            Ok(call) => call,
            // Answered with nothing: the study file reads that as "no answer", in its own words.
            Err(_) => return Vec::new(),
        };
        let ctx = self.ctx;
        match call {
            Call::Bars { venue, symbol, interval, range } => {
                answered(ctx.bars(&venue, &symbol, &interval, range))
            }
            Call::BarsInLane { lane, venue, symbol, interval, range, budget } => {
                answered(ctx.bars_in_lane_capped(&lane, &venue, &symbol, &interval, range, budget))
            }
            Call::Quotes { venue, symbol, range } => answered(ctx.quotes(&venue, &symbol, range)),
            Call::Trades { venue, symbol, range } => answered(ctx.trades(&venue, &symbol, range)),
            Call::BookUpdates { venue, symbol, range } => {
                answered(ctx.book_updates(&venue, &symbol, range))
            }
            Call::Depth { venue, symbol, range } => answered(ctx.depth(&venue, &symbol, range)),
            Call::Cohort { venue, asset, range } => answered(ctx.cohort(&venue, &asset, range)),
            Call::PerpMetrics { venue, symbol, range } => {
                answered(ctx.perp_metrics(&venue, &symbol, range))
            }
            Call::TextsAsOf { text_kind, collector, decision_ms, lookback_ms } => {
                answered(ctx.texts_as_of(&text_kind, &collector, decision_ms, lookback_ms))
            }
            Call::FinancialFactsAsOf { venue, symbol, decision_ms, lookback_ms } => {
                answered(ctx.financial_facts_as_of(&venue, &symbol, decision_ms, lookback_ms))
            }
            Call::InstitutionalHoldingsAsOf { venue, symbol, decision_ms, lookback_ms } => {
                answered(ctx.institutional_holdings_as_of(
                    &venue,
                    &symbol,
                    decision_ms,
                    lookback_ms,
                ))
            }
            Call::TextScoresAsOf { text_kind, model_name, decision_ms, lookback_ms } => {
                answered(ctx.text_scores_as_of(&text_kind, &model_name, decision_ms, lookback_ms))
            }
            Call::ExecutionFills { venue, symbol, range } => {
                answered(ctx.execution_fills(&venue, &symbol, range))
            }
            Call::ExecutionOrders { venue, symbol, range } => {
                answered(ctx.execution_orders(&venue, &symbol, range))
            }
            Call::KeptInstruments => answered(ctx.kept_instruments()),
            Call::KeptContractValue { venue, symbol } => {
                answered(ctx.kept_contract_value(&venue, &symbol))
            }
            // ⚠ A `fit` is asked as a `fit_captured` for the model's TEXT, so the study parses the
            // model beside itself and scores rows without crossing: `vike_ml_training::GbdtLearner` trains the
            // same model both ways (one trainer call, one parse of its text). A learner that keeps no
            // text answers its model, which is held here.
            Call::Fit { data, point, seed } => {
                let fitted = self.learner().and_then(|l| {
                    l.fit_captured(
                        &data.data(),
                        &point.into(),
                        seed,
                        Capture { importance: false, text: true },
                    )
                });
                encode(&fitted.map(|(model, _, text)| self.model(model, text)))
            }
            Call::FitCaptured { data, point, seed, importance, text } => {
                let want = Capture { importance, text: true };
                let fitted = self
                    .learner()
                    .and_then(|l| l.fit_captured(&data.data(), &point.into(), seed, want));
                encode(&fitted.map(|(model, imp, model_text)| {
                    let kept = if text { model_text.clone() } else { None };
                    (self.model(model, model_text), imp.map(Importance::of), kept)
                }))
            }
            Call::FitChained { data, point, seed, importance, text, chain, step } => {
                let want = Capture { importance, text: true };
                let fitted = self.learner().and_then(|l| {
                    l.fit_chained(&data.data(), &point.into(), seed, want, &chain, step)
                });
                encode(&fitted.map(|(model, imp, model_text)| {
                    let kept = if text { model_text.clone() } else { None };
                    (self.model(model, model_text), imp.map(Importance::of), kept)
                }))
            }
            Call::FitIdentity { data, point, seed } => encode(
                &self
                    .learner()
                    .ok()
                    .and_then(|l| l.fit_identity(&data.data(), &point.into(), seed)),
            ),
            Call::Predict { model, row } => {
                let answer = self.held(model).and_then(|m| {
                    let mut out = vec![0.0; m.kind().output_count()];
                    m.predict_into(&row, &mut out).map(|()| out)
                });
                encode(&answer)
            }
            Call::PredictBatch { model, rows, n_features } => {
                encode(&self.held(model).and_then(|m| m.predict_batch(&rows, n_features)))
            }
            Call::DropModel { model } => {
                self.held.lock().unwrap_or_else(PoisonError::into_inner).remove(&model);
                encode(&())
            }
            Call::Sim { profile, store } => encode(&self.backtest(&profile, store)),
            Call::Chain { venue, underlying, range } => {
                answered(ctx.chain(&venue, &underlying, range))
            }
            Call::RunForecasts { run_id } => encode(
                &self
                    .ctx
                    .runs()
                    .ok_or_else(|| "this run offered no saved runs".to_string())
                    .and_then(|runs| runs.forecasts(&run_id)),
            ),
            Call::RunTrades { run_id } => encode(
                &self
                    .ctx
                    .runs()
                    .ok_or_else(|| "this run offered no saved runs".to_string())
                    .and_then(|runs| runs.trades(&run_id)),
            ),
            Call::RunForward { run_id } => encode(
                &self
                    .ctx
                    .runs()
                    .ok_or_else(|| "this run offered no saved runs".to_string())
                    .and_then(|runs| runs.forward(&run_id)),
            ),
            Call::ForecastsAsOf { venue, symbol, decision_ms, start_ms } => {
                answered(ctx.forecasts_as_of(&venue, &symbol, decision_ms, start_ms))
            }
        }
    }

    /// The study's one backtest: the host's `StudySim` (the context's) run over the store the study
    /// handed it, every read of that store asked of the study file through `serve`. Run HERE, in the
    /// parent, so a strategy FILE in the profile takes the same builder, loader and sandbox as every
    /// backtest (the jailed study process can start no process).
    fn backtest(&self, profile: &str, store: u64) -> Result<SimAnswer, String> {
        let sim = self.ctx.sim().ok_or_else(|| "this run offered no backtest".to_string())?;
        // TOML text back to the value the study built, as `toml::from_str` reads a document.
        let profile = toml::from_str::<toml::Value>(profile)
            .map_err(|e| format!("profile does not describe a backtest: {e}"))?;
        let store = Arc::new(GuestStore { store, serve: Arc::clone(&self.serve) });
        let o = sim.run_one(&profile, store)?;
        Ok(SimAnswer { total_return: o.total_return, n_trades: o.n_trades, win_rate: o.win_rate })
    }

    /// The held model `model`, taken out of the map so its answer is computed with no lock held: one
    /// fold's network crossing its pipe never waits behind another's.
    fn held(&self, model: u64) -> Result<Arc<dyn Predictor>, String> {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.get(&model).cloned().ok_or_else(|| format!("no held model {model}"))
    }

    fn learner(&self) -> Result<&dyn crate::StudyLearner, String> {
        self.ctx.learner().ok_or_else(|| "this run offered no learner".to_string())
    }

    /// A fitted model as it crosses: its text, or held here under a new number with the kind of
    /// answer it declares.
    ///
    /// ⚠ Only a TREE model's text crosses as the model: the study parses it beside itself
    /// (`vike_ml_prediction::parse_model_text`). Another learner's text, a network's base64 ONNX
    /// (`vike_ml_training::NeuralLearner`, decision 0292), still reaches the study as the text it KEEPS,
    /// but its model is held here: the jail has no ONNX runtime to read it with. A held network's
    /// kind is the one its provenance names (a value for a regression or the Sharpe objective).
    fn model(&self, model: Box<dyn Predictor>, text: Option<String>) -> Model {
        match text {
            Some(text) if vike_ml_prediction::parse_model_text(&text).is_ok() => Model::Text(text),
            _ => {
                let kind = model.kind().into();
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                self.held.lock().unwrap_or_else(PoisonError::into_inner).insert(id, model.into());
                Model::Held { model: id, kind }
            }
        }
    }
}

/// The store the study handed its backtest, read through the study file: each verb a backtest reads
/// is asked of the study's own store, which answers as it would in the study's process. The verbs
/// the trait builds on these (`bar_edges`, `load_bars_head`, the `_capped` reads) stay the trait's
/// defaults over them, as they do for the sandboxed backtest's own piped store; a write is refused
/// (a backtest writes nothing).
struct GuestStore {
    store: u64,
    serve: Serve,
}

impl GuestStore {
    fn ask<T: DeserializeOwned>(
        &self,
        verb: &str,
        call: StoreCall,
    ) -> Result<T, HistoryStoreError> {
        let answer = (self.serve)(&encode(&StoreRequest { store: self.store, call }));
        match decode::<Result<T, HistoryStoreError>>(&answer) {
            Ok(answer) => answer,
            Err(e) => Err(HistoryStoreError::Io(format!("the study's store, {verb}: {e}"))),
        }
    }

    fn read_only(verb: &str) -> HistoryStoreError {
        HistoryStoreError::Unsupported(format!(
            "{verb}: a study's backtest reads its store and writes nothing to it"
        ))
    }
}

impl HistoryStore for GuestStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        let call = StoreCall::LoadBars {
            venue: venue.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
        };
        self.ask("load_bars", call)
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
        let call = StoreCall::LoadBarsSourcedCapped {
            venue: venue.into(),
            source: source.into(),
            symbol: symbol.into(),
            interval: interval.into(),
            range,
            budget,
        };
        self.ask("load_bars_sourced_capped", call)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        let call = StoreCall::ScanQuotes { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_quotes", call)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        let call = StoreCall::ScanTrades { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_trades", call)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let call = StoreCall::ScanBookUpdates { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_book_updates", call)
    }

    fn scan_depth(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        let call = StoreCall::ScanDepth { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_depth", call)
    }

    fn scan_cohort(
        &self,
        venue: &str,
        asset: &str,
        range: TimestampRange,
    ) -> Result<Vec<CohortRow>, HistoryStoreError> {
        let call = StoreCall::ScanCohort { venue: venue.into(), asset: asset.into(), range };
        self.ask("scan_cohort", call)
    }

    fn scan_perp_metrics(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<PerpMetricRow>, HistoryStoreError> {
        let call = StoreCall::ScanPerpMetrics { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_perp_metrics", call)
    }

    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<(i64, SymbolProperties)>, HistoryStoreError> {
        let call =
            StoreCall::ScanSymbolProperties { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_symbol_properties", call)
    }

    fn scan_equity(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<EquitySample>, HistoryStoreError> {
        let call = StoreCall::ScanEquity { venue: venue.into(), symbol: symbol.into(), range };
        self.ask("scan_equity", call)
    }

    fn scan_exec_fills(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecFillRow>, HistoryStoreError> {
        let call = StoreCall::ScanExecFills { venue: venue.into(), symbol: symbol.into() };
        self.ask("scan_exec_fills", call)
    }

    fn scan_exec_orders(
        &self,
        venue: &str,
        symbol: &str,
    ) -> Result<Vec<ExecOrderRow>, HistoryStoreError> {
        let call = StoreCall::ScanExecOrders { venue: venue.into(), symbol: symbol.into() };
        self.ask("scan_exec_orders", call)
    }

    fn properties_as_of(
        &self,
        venue: &str,
        symbol: &str,
        ts: i64,
    ) -> Result<Option<SymbolProperties>, HistoryStoreError> {
        let call =
            StoreCall::PropertiesAsOf { venue: venue.into(), symbol: symbol.into(), timestamp: ts };
        self.ask("properties_as_of", call)
    }

    fn list_series(&self) -> Result<Vec<SeriesId>, HistoryStoreError> {
        self.ask("list_series", StoreCall::ListSeries)
    }

    fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, HistoryStoreError> {
        self.ask("inventory", StoreCall::Inventory)
    }

    fn series_gaps(&self, id: &SeriesId) -> Result<Vec<(i64, i64)>, HistoryStoreError> {
        self.ask("series_gaps", StoreCall::SeriesGaps(id.clone()))
    }

    fn series_facts(
        &self,
        id: &SeriesId,
    ) -> Result<(SeriesCoverage, Vec<String>), HistoryStoreError> {
        self.ask("series_facts", StoreCall::SeriesFacts(id.clone()))
    }

    fn kept_instruments(&self) -> Result<Option<Vec<vike_model::Instrument>>, HistoryStoreError> {
        self.ask("kept_instruments", StoreCall::KeptInstruments)
    }

    // The operator's share splits, beside the kept rows: a study's backtest adjusts by them as a plain one does.
    fn stock_splits(&self) -> Result<Option<Vec<vike_model::StockSplit>>, HistoryStoreError> {
        self.ask("stock_splits", StoreCall::StockSplits)
    }

    // The option-chain rows a backtest's `[data.options]` legs are priced from.
    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        let call =
            StoreCall::ScanChain { venue: venue.into(), underlying: underlying.into(), range };
        self.ask("scan_chain", call)
    }

    fn append_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[Bar],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_bars"))
    }

    fn append_quotes(
        &self,
        _: &str,
        _: &str,
        _: &[TickQuote],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_quotes"))
    }

    fn append_trades(
        &self,
        _: &str,
        _: &str,
        _: &[TickTrade],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_trades"))
    }

    fn append_book_updates(
        &self,
        _: &str,
        _: &str,
        _: &[L2BookUpdate],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_book_updates"))
    }

    fn append_symbol_properties(
        &self,
        _: &str,
        _: &str,
        _: &[(i64, SymbolProperties)],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_symbol_properties"))
    }

    fn append_equity(
        &self,
        _: &str,
        _: &str,
        _: &[EquitySample],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_equity"))
    }

    fn append_exec_fills(
        &self,
        _: &str,
        _: &str,
        _: &[ExecFillRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_exec_fills"))
    }

    fn append_exec_orders(
        &self,
        _: &str,
        _: &str,
        _: &[ExecOrderRow],
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("append_exec_orders"))
    }

    fn resample_quotes_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("resample_quotes_to_bars"))
    }

    fn resample_trades_to_bars(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TimestampRange,
        _: Option<&str>,
    ) -> Result<usize, HistoryStoreError> {
        Err(Self::read_only("resample_trades_to_bars"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use vike_data::HistoryStoreMemory;
    use vike_ml_training::seam::test_support::ScriptedLearner;
    use vike_ml_training::{CapturedFit, DEFAULT_POINT, GridPoint, Learner, TrainData};

    use super::super::wire::{Kind, Point, Train};
    use super::*;

    /// A learner whose captured text is NOT a tree model's: a network's base64 ONNX
    /// (`vike_ml_training::NeuralLearner`'s spelling), here four bytes.
    struct NetworkText(ScriptedLearner);

    impl Learner for NetworkText {
        type Model = <ScriptedLearner as Learner>::Model;

        fn fit(&self, d: &TrainData<'_>, p: &GridPoint, seed: u64) -> Result<Self::Model, String> {
            self.0.fit(d, p, seed)
        }

        fn fit_captured(
            &self,
            d: &TrainData<'_>,
            p: &GridPoint,
            seed: u64,
            _want: Capture,
        ) -> Result<CapturedFit<Self::Model>, String> {
            Ok((self.0.fit(d, p, seed)?, None, Some("AAECAw==".to_string())))
        }
    }

    /// A model whose text the study cannot parse is HELD by the parent and scored per row, while the
    /// study still gets the text it asked to keep (decision 0292: a network crosses this way).
    #[test]
    fn a_model_whose_text_is_no_tree_is_held_and_its_text_still_reaches_the_study() {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let ctx = StudyContext::new(store, TimestampRange::of(0, 1), PathBuf::from("unused"))
            .with_learner(Arc::new(NetworkText(ScriptedLearner::constant(0.25))));
        let parent = Parent::new(&ctx, Arc::new(|_: &[u8]| Vec::new()));
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        let call = Call::FitCaptured {
            data: Train::of(&data),
            point: Point::from(&DEFAULT_POINT),
            seed: 0,
            importance: false,
            text: true,
        };
        let answer = decode::<Result<(Model, Option<Importance>, Option<String>), String>>(
            &parent.answer(&encode(&call)),
        )
        .expect("an answer")
        .expect("a fit");
        let Model::Held { model: id, kind: Kind::Probability } = answer.0 else {
            panic!("held as the probability it declares, not crossed as text: {:?}", answer.0)
        };
        assert_eq!(answer.2.as_deref(), Some("AAECAw=="), "the kept text still reaches the study");
        let row = Call::Predict { model: id, row: vec![0.0] };
        let score =
            decode::<Result<Vec<f64>, String>>(&parent.answer(&encode(&row))).expect("decoded");
        assert_eq!(score, Ok(vec![0.25]));
    }

    /// A held model that answers a VALUE (a network trained on a regression or the Sharpe
    /// objective, W24) crosses as a value: its kind is the one it declares, never a probability.
    struct Position;

    impl Predictor for Position {
        fn kind(&self) -> vike_ml_prediction::PredictionKind {
            vike_ml_prediction::PredictionKind::Value
        }

        fn predict_into(&self, row: &[f64], out: &mut [f64]) -> Result<(), String> {
            out[0] = -row[0];
            Ok(())
        }
    }

    struct PositionNetwork;

    impl Learner for PositionNetwork {
        type Model = Position;

        fn fit(&self, _: &TrainData<'_>, _: &GridPoint, _: u64) -> Result<Position, String> {
            Ok(Position)
        }

        fn fit_captured(
            &self,
            _: &TrainData<'_>,
            _: &GridPoint,
            _: u64,
            _: Capture,
        ) -> Result<CapturedFit<Position>, String> {
            Ok((Position, None, Some("AAECAw==".to_string())))
        }
    }

    #[test]
    fn a_held_value_model_crosses_as_a_value() {
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
        let ctx = StudyContext::new(store, TimestampRange::of(0, 1), PathBuf::from("unused"))
            .with_learner(Arc::new(PositionNetwork));
        let parent = Parent::new(&ctx, Arc::new(|_: &[u8]| Vec::new()));
        let (x, y) = ([0.0, 1.0], [0.0_f32, 1.0]);
        let data = TrainData::new(&x, &y, 1).expect("data");
        let call =
            Call::Fit { data: Train::of(&data), point: Point::from(&DEFAULT_POINT), seed: 0 };
        let model = decode::<Result<Model, String>>(&parent.answer(&encode(&call)))
            .expect("an answer")
            .expect("a fit");
        let Model::Held { model: id, kind: Kind::Value } = model else {
            panic!("held as the value it declares: {model:?}")
        };
        let row = Call::Predict { model: id, row: vec![0.5] };
        let answer =
            decode::<Result<Vec<f64>, String>>(&parent.answer(&encode(&row))).expect("decoded");
        assert_eq!(answer, Ok(vec![-0.5]));
    }
}
