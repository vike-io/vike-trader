//! One strategy over one slice (bar or tick), the one worker-thread spawn, and Compare-all.

use std::sync::mpsc::{Receiver, channel};
use vike_analytics::BacktestResult;
use vike_backtest::hist_replay::{TickReplayConfig, replay_ticks};
use vike_data::StoreHandle;
use vike_marketdata::Bar;
use vike_model::Strategy;
use vike_sim::{EngineParams, SimBroker, StrategyEngine};

use super::{DataSlice, RunError, RunOutcome, SliceKind, build_strategy};
use crate::spec::StrategySpec;

/// Load a BAR slice's per-symbol series, in `slice.symbols` order.
///
/// Errors (never panics) on: a tick slice, an empty symbol list, a symbol with no bars in range,
/// or symbols whose series lengths disagree — the last is `StrategyEngine::new`'s own assert,
/// caught here so a mismatched multi-symbol pick is a red banner, not a worker-thread panic.
pub fn load_slice_bars(
    slice: &DataSlice,
    store: &StoreHandle,
) -> Result<Vec<(String, Vec<Bar>)>, RunError> {
    if slice.kind != SliceKind::Bars {
        return Err(RunError::Data("this run needs a bar slice, not a tick slice".into()));
    }
    if slice.symbols.is_empty() {
        return Err(RunError::Data("no symbol selected".into()));
    }
    // The simulator books one ledger currency and holds no rate series. A picked slice carries no
    // ledger field (the run is on the dollar), so a symbol whose name says another fiat quote is a red
    // banner here, in words that send the user to a profile run (`engine.account_ccy`) instead of to a
    // key the picker cannot set. The tick path below says the same through `run_tick_slice`.
    vike_backtest::harness::refuse_foreign_quoted_picks(slice.symbols.iter().map(String::as_str))
        .map_err(|e| RunError::Data(e.message().to_string()))?;
    let mut series: Vec<(String, Vec<Bar>)> = Vec::with_capacity(slice.symbols.len());
    for symbol in &slice.symbols {
        let bars = store
            .load_bars(&slice.venue, symbol, &slice.interval, slice.range)
            .map_err(|e| RunError::Data(e.to_string()))?;
        if bars.is_empty() {
            return Err(RunError::Data(format!(
                "no bars for {}:{} {} in the selected range",
                slice.venue, symbol, slice.interval
            )));
        }
        series.push((symbol.clone(), bars));
    }
    let lengths: std::collections::BTreeSet<usize> = series.iter().map(|(_, b)| b.len()).collect();
    if lengths.len() > 1 {
        return Err(RunError::Data(format!(
            "multi-symbol bar slices must be aligned — the picked symbols have {} different bar \
             counts ({:?}); StrategyEngine requires one length across symbols",
            lengths.len(),
            series.iter().map(|(s, b)| (s.as_str(), b.len())).collect::<Vec<_>>()
        )));
    }
    Ok(series)
}

/// Replay a TICK slice through `vike_backtest::hist_replay::replay_ticks` (quotes + trades + book
/// events, merged per symbol). No bar seeding, no PIT-properties snap, no feed-latency reordering —
/// the Studio's tick MVP is the raw venue-ordered replay; those knobs belong to a profile, not a
/// two-click picker.
pub(super) fn run_tick_slice(
    strategy: Box<dyn Strategy<SimBroker>>,
    slice: &DataSlice,
    store: &StoreHandle,
    params: EngineParams,
) -> RunOutcome {
    if slice.symbols.is_empty() {
        return Err(RunError::Data("no symbol selected".into()));
    }
    // A caller that STATES a ledger (`params.account_ccy`) is judged against it by the replay itself;
    // one that does not is on the dollar with no way to say otherwise, and gets the picker's words.
    if params.account_ccy.is_none() {
        vike_backtest::harness::refuse_foreign_quoted_picks(
            slice.symbols.iter().map(String::as_str),
        )
        .map_err(|e| RunError::Data(e.message().to_string()))?;
    }
    let cfg = TickReplayConfig {
        venue: slice.venue.clone(),
        symbols: slice.symbols.clone(),
        range: slice.range,
        seed_bar_interval_ms: None,
        params,
        snap_to_properties: false,
        feed_latency: false,
        series: None,
    };
    let result =
        replay_ticks(store.clone(), strategy, cfg).map_err(|e| RunError::Data(e.to_string()))?;
    // `replay_ticks` skips a symbol with no quotes/trades/books entirely, so an all-empty slice
    // runs an engine over nothing and returns a flat curve. Report that as a data error, matching
    // the bar path's "no bars in the selected range".
    if result.equity_curve.is_empty() {
        return Err(RunError::Data(format!(
            "no recorded ticks for {} {:?} in the selected range",
            slice.venue, slice.symbols
        )));
    }
    Ok(result)
}

/// Resolve + load + backtest, synchronously. Pure logic; call it off the UI thread via
/// [`spawn_outcome`].
pub fn run_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    params: EngineParams,
) -> RunOutcome {
    let strat = build_strategy(spec)?;
    match slice.kind {
        SliceKind::Bars => {
            let series = load_slice_bars(slice, store)?;
            Ok(StrategyEngine::new(series, strat, params).run())
        }
        SliceKind::Ticks => run_tick_slice(strat, slice, store, params),
    }
}

/// Spawn `f` on a worker thread; its single return value arrives on the returned receiver. The ONE
/// `channel()` + `thread::spawn` + `tx.send(f())` site behind `spawn_compare_all` below, the study
/// runner and every `vike-studio` worker (the remote backend's spawns, `ChatPane::send`) — the UI
/// polls the receiver with `try_recv()` each frame so the work never blocks the egui update loop.
pub fn spawn_outcome<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("studio-outcome".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .expect("spawn outcome thread");
    rx
}

/// One named strategy's outcome from a Saved-strategies "Compare all" — `run_slice`'s
/// `RunError` already stringified so the pane's `comparison_rows` builder (vike-studio's
/// `saved.rs`) doesn't need to know the `RunError` type, matching `handle_saved_action`'s
/// original synchronous shape.
pub type CompareOutcome = Vec<(String, Result<BacktestResult, String>)>;

/// Run every `(name, spec)` strategy over `slice`, synchronously, each with its own default
/// `EngineParams` (mirrors the pre-worker-thread `handle_saved_action` behavior: one fresh
/// `EngineParams::default()` per strategy, not a single shared/cloned params — `EngineParams`
/// itself isn't `Clone` because it can carry a `Box<dyn PositionSizer>`). Rhai and native rows mix
/// freely: each row is just a [`StrategySpec`]. Pure logic; call it off the UI thread via
/// `spawn_compare_all`.
pub fn compare_all_slice(
    strategies: &[(String, StrategySpec)],
    slice: &DataSlice,
    store: &StoreHandle,
) -> CompareOutcome {
    strategies
        .iter()
        .map(|(name, spec)| {
            let outcome =
                run_slice(spec, slice, store, EngineParams::default()).map_err(|e| e.to_string());
            (name.clone(), outcome)
        })
        .collect()
}

/// Run on a worker thread; the single [`CompareOutcome`] arrives on the returned receiver — the
/// Saved pane's "Compare all", through [`spawn_outcome`].
pub fn spawn_compare_all(
    strategies: Vec<(String, StrategySpec)>,
    slice: DataSlice,
    store: StoreHandle,
) -> Receiver<CompareOutcome> {
    spawn_outcome(move || compare_all_slice(&strategies, &slice, &store))
}
