//! The Run pipeline — headless (no egui) so CI tests it directly. Resolve the [`StrategySpec`]
//! (compile the Rhai buffer, or build a native registry strategy), load the picked [`DataSlice`]
//! from the `HistStore`, backtest it, return a result.
//!
//! Two axes widened past the original "one Rhai script over one bar series":
//!
//! - **Strategy source** — every runner takes a [`StrategySpec`] (`spec.rs`) instead of a `&str`
//!   of Rhai. [`build_strategy`] is the ONE place either source becomes a
//!   `Box<dyn Strategy<SimBroker>>`; the runners below are source-agnostic from there on.
//! - **Data slice** — [`DataSlice`] carries a symbol LIST and a [`SliceKind`]. `Bars` is the
//!   original `load_bars` + `StrategyEngine` path; `Ticks` routes to `vike_backtest::hist_replay::
//!   replay_ticks` over the store's recorded quote/trade/book series instead. Single-symbol bar
//!   slices keep their exact previous behavior — [`DataSlice::bars`] is the constructor for them.

use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use vike_backtest::harness::{cmp_scores_desc, map_bounded};
use vike_backtest::hist_replay::{replay_ticks, TickReplayConfig};
use vike_backtest::metrics::{returns, sharpe};
use vike_backtest::overfit::{deflated_sharpe_with_effective_n, pbo_cscv, sharpe_moments};
use vike_backtest::validation::WalkMode;
use vike_backtest::walkforward::{walk_forward_strategy, WalkForwardReport};
use vike_backtest::{BacktestResult, EngineParams, SimBroker, StrategyEngine};
use vike_data::{DataError, HistStore, TsRange};
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;

use crate::spec::{params_with_overrides, StrategySpec};

/// A store handle every runner accepts: owned + `Send + Sync` because the tick path
/// (`replay_ticks`) needs an `Arc` it can clone into the engine's `'static` properties closure.
pub type StoreHandle = Arc<dyn HistStore + Send + Sync>;

/// Which recorded series a [`DataSlice`] replays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SliceKind {
    /// `kind=bar` series at `interval`, replayed through `StrategyEngine::run` (the original path).
    #[default]
    Bars,
    /// The recorded `kind=quote`/`trade`/`book` series for these symbols, replayed through
    /// `vike_backtest::hist_replay::replay_ticks` (`interval` is unused).
    Ticks,
}

/// The data window a Run executes over.
///
/// `symbols` is a LIST (was a single `symbol`): `StrategyEngine::run`/`run_ticks` have always
/// accepted several series, and a cross-instrument strategy needs them together in one run. Use
/// [`DataSlice::bars`] for the single-symbol bar case — it is exactly the old struct literal.
#[derive(Debug, Clone, PartialEq)]
pub struct DataSlice {
    pub venue: String,
    /// One or more symbols, in the order the engine should register them.
    pub symbols: Vec<String>,
    /// Bar step (`"1m"`). Unused — and conventionally empty — for [`SliceKind::Ticks`].
    pub interval: String,
    pub range: TsRange,
    pub kind: SliceKind,
}

impl DataSlice {
    /// A single-symbol BAR slice — the original `DataSlice { venue, symbol, interval, range }`.
    pub fn bars(
        venue: impl Into<String>,
        symbol: impl Into<String>,
        interval: impl Into<String>,
        range: TsRange,
    ) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols: vec![symbol.into()],
            interval: interval.into(),
            range,
            kind: SliceKind::Bars,
        }
    }

    /// A multi-symbol BAR slice. NB `StrategyEngine::new` asserts every symbol's bar series shares
    /// one length; [`load_slice_bars`] checks that itself and returns a [`RunError::Data`] rather
    /// than letting the assert panic a worker thread.
    pub fn multi_bars(
        venue: impl Into<String>,
        symbols: Vec<String>,
        interval: impl Into<String>,
        range: TsRange,
    ) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols,
            interval: interval.into(),
            range,
            kind: SliceKind::Bars,
        }
    }

    /// A TICK slice over the store's recorded quote/trade/book series.
    pub fn ticks(venue: impl Into<String>, symbols: Vec<String>, range: TsRange) -> Self {
        DataSlice {
            venue: venue.into(),
            symbols,
            interval: String::new(),
            range,
            kind: SliceKind::Ticks,
        }
    }

    /// The first symbol (`""` for the degenerate empty slice) — for the many single-symbol call
    /// sites that used to read `slice.symbol`.
    pub fn symbol(&self) -> &str {
        self.symbols.first().map(String::as_str).unwrap_or("")
    }

    /// A compact human label, e.g. `binance · BTCUSDT · 1m` or `polymarket · TKN+1 · ticks`.
    pub fn label(&self) -> String {
        let sym = match self.symbols.len() {
            0 => "(no symbol)".to_string(),
            1 => self.symbols[0].clone(),
            n => format!("{}+{}", self.symbols[0], n - 1),
        };
        let tail = match self.kind {
            SliceKind::Bars => self.interval.clone(),
            SliceKind::Ticks => "ticks".to_string(),
        };
        format!("{} · {sym} · {tail}", self.venue)
    }
}

/// The `(venue, symbol, interval)` triples of every `kind=bar` series in the store, in
/// `list_series`'s stable sorted order (tick series — `interval == None` — are dropped). The ONE
/// place the "bars only, interval present" filter over [`HistStore::list_series`] lives —
/// the Studio's `SlicePicker::refresh` calls it rather than re-deriving the filter (the retired
/// `vike-mcp` `list_series` tool was the second caller; its successor, `vike-cli mcp`, lists
/// series over the datahub wire verb instead of a local store). Takes the TRAIT (split-plane
/// B12), so the Studio's picker enumerates a remote `RemoteHistStore` exactly like a local
/// `DataFusionHist`.
pub fn bar_series(store: &dyn HistStore) -> Result<Vec<(String, String, String)>, DataError> {
    Ok(store
        .list_series()?
        .into_iter()
        .filter(|s| s.kind == "bar")
        .filter_map(|s| s.interval.map(|iv| (s.venue, s.symbol, iv)))
        .collect())
}

/// The store `kind=` values a tick slice can actually REPLAY — the lanes
/// `vike_backtest::hist_replay::replay_ticks` reads (`scan_quotes` / `scan_trades` /
/// `scan_book_updates`), one entry per lane, and the filter [`tick_series`] applies.
///
/// A named constant rather than an inline `matches!` because [`depth_only_series`] is the
/// complement of exactly this set, and the two must be derived from one list or the disclosure
/// silently stops covering a lane the replay later gains.
pub const REPLAYABLE_TICK_KINDS: [&str; 3] = ["quote", "trade", "book"];

/// The store `kind=` this module lists as RECORDED-but-not-replayable — see [`depth_only_series`]
/// for the argument, which is the whole reason the value has a name here at all.
pub const UNREPLAYABLE_TICK_KIND: &str = "depth";

/// The DISTINCT `(venue, symbol)` pairs that have any REPLAYABLE tick series
/// ([`REPLAYABLE_TICK_KINDS`]), in `list_series`'s stable sorted order — the tick twin of
/// [`bar_series`], and what the Studio's slice picker lists under "ticks".
///
/// Deduplicated across kinds on purpose: `replay_ticks` scans all three kinds for a symbol and
/// merges them, so one picker row per `(venue, symbol)` is exactly one runnable slice. A symbol
/// recorded as quotes AND trades must not appear twice.
///
/// ⚠ **`kind=depth` is deliberately NOT admitted here, and its exclusion is REPORTED rather than
/// silent** — [`depth_only_series`] names the instruments it costs. The exclusion needs writing
/// down precisely because adding the arm would compile and run: `crates/vike-data/src/
/// store_kind.rs`'s `STORE_KINDS` marks `depth` a tick lane carrying the same `BookUpdate` rows
/// and the same codec as `book`, so the rows would drop straight into `merge_ticks`. Two
/// independent facts say no anyway:
///
/// - **The replay has no depth lane at all.** `vike_backtest::hist_replay::replay_ticks` reads
///   `scan_quotes`/`scan_trades`/`scan_book_updates` and never `scan_depth`, and its per-series
///   lane filter `vike_backtest::hist_replay::SeriesKind` has no depth variant to select one. A
///   depth-only row listed here would pick a slice that loads nothing and die as
///   [`RunError::Data`] — a broken arm, not a feature.
/// - **Folding depth into the book lane is the thing the store layout exists to prevent.** That
///   `STORE_KINDS` row states its own identity: depth is the CONFLATING L2 lane, a periodic full
///   snapshot with every intermediate book state discarded, kept a DIFFERENT series from `book`
///   on purpose because the path IS the disclosure. `crates/vike-data/src/live_rec.rs`'s
///   `l2_snapshot` carries the consequence of ignoring that — a market-making backtest run over
///   teleporting depth reports fills it could never have got. Routing depth rows into the `books`
///   slot would hand a strategy conflated snapshots under the name of a lossless book, silently,
///   which is a worse defect than the one this doc exists to close.
pub fn tick_series(store: &dyn HistStore) -> Result<Vec<(String, String)>, DataError> {
    let mut out: Vec<(String, String)> = Vec::new();
    for s in store.list_series()? {
        if !REPLAYABLE_TICK_KINDS.contains(&s.kind.as_str()) {
            continue;
        }
        let pair = (s.venue, s.symbol);
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    Ok(out)
}

/// The DISTINCT `(venue, symbol)` pairs whose ONLY recorded tick data is
/// [`UNREPLAYABLE_TICK_KIND`] — present in the store, listed by [`HistStore::list_series`], and
/// not replayable by any slice this module can build.
///
/// This is the disclosure half of [`tick_series`]'s filter, and it exists because that filter's
/// omission used to be invisible. `subscribe_depth` is an independently subscribable lane on
/// several bridges, so an instrument recorded through it alone is a real store state — and it
/// appeared in NO Studio list, which a human reads as the store's complete runnable inventory.
/// "There is no such data" and "the data is there and cannot be replayed" then looked the same,
/// and only the second one has an action attached to it (record `book`/`trade`/`quote` too).
///
/// A pair that ALSO has a replayable lane is excluded: it is already a pickable tick row, and its
/// depth series adds nothing the picker can offer separately. So the returned set is exactly the
/// instruments [`tick_series`] costs, never a second opinion about ones it already lists.
///
/// Ordering and dedup follow [`tick_series`]: `list_series`'s stable sorted order, one entry per
/// `(venue, symbol)`.
pub fn depth_only_series(store: &dyn HistStore) -> Result<Vec<(String, String)>, DataError> {
    let mut depth: Vec<(String, String)> = Vec::new();
    let mut replayable: Vec<(String, String)> = Vec::new();
    for s in store.list_series()? {
        let bucket = if s.kind == UNREPLAYABLE_TICK_KIND {
            &mut depth
        } else if REPLAYABLE_TICK_KINDS.contains(&s.kind.as_str()) {
            &mut replayable
        } else {
            continue;
        };
        let pair = (s.venue, s.symbol);
        if !bucket.contains(&pair) {
            bucket.push(pair);
        }
    }
    depth.retain(|pair| !replayable.contains(pair));
    Ok(depth)
}

/// Why a Run could not produce a result.
#[derive(Debug, Clone)]
pub enum RunError {
    /// The Rhai script failed to compile (parse/compile error, line/col in the message).
    Compile(String),
    /// The data slice could not be loaded, or yielded no bars/ticks.
    Data(String),
    /// A NATIVE strategy could not be built: an unknown registry name, or a params table the
    /// registry rejected. The native twin of [`RunError::Compile`] — kept a separate variant so
    /// the UI can say "no such strategy" instead of "compile error".
    Strategy(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Compile(msg) => write!(f, "compile error: {msg}"),
            RunError::Data(msg) => write!(f, "{msg}"),
            RunError::Strategy(msg) => write!(f, "strategy error: {msg}"),
        }
    }
}

impl std::error::Error for RunError {}

pub type RunOutcome = Result<BacktestResult, RunError>;

/// Resolve a [`StrategySpec`] into the boxed strategy every runner drives — the ONE place the
/// Rhai/native branch is taken. `dyn` dispatch is the harness registry's own shape and is noise
/// next to loading the slice (see `vike_backtest::harness::registry`'s module doc).
pub fn build_strategy(spec: &StrategySpec) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => Ok(Box::new(
            RhaiStrategy::<SimBroker>::compile(src)
                .map_err(|e| RunError::Compile(e.to_string()))?,
        )),
        StrategySpec::Native { name, params } => {
            vike_backtest::harness::strategy_by_name(name, params)
                .map_err(|e| RunError::Strategy(e.to_string()))
        }
    }
}

/// [`build_strategy`] with one sweep point's `(name, value)` overrides applied: Rhai gets them as
/// script `param()` bindings (`compile_with_params`), native gets them merged into its params table.
pub fn build_strategy_with(
    spec: &StrategySpec,
    overrides: &[(String, f64)],
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => {
            let map: indexmap::IndexMap<String, f64> = overrides.iter().cloned().collect();
            Ok(Box::new(
                RhaiStrategy::<SimBroker>::compile_with_params(src, map)
                    .map_err(|e| RunError::Compile(e.to_string()))?,
            ))
        }
        StrategySpec::Native { name, params } => build_strategy(&StrategySpec::Native {
            name: name.clone(),
            params: params_with_overrides(params, overrides),
        }),
    }
}

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
fn run_tick_slice(
    strategy: Box<dyn Strategy<SimBroker>>,
    slice: &DataSlice,
    store: &StoreHandle,
    params: EngineParams,
) -> RunOutcome {
    if slice.symbols.is_empty() {
        return Err(RunError::Data("no symbol selected".into()));
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

/// Resolve + load + backtest, synchronously. Pure logic; call it off the UI thread via `spawn_run`.
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
/// `channel()` + `thread::spawn` + `tx.send(f())` site behind every `spawn_*` twin below (and
/// `vike-studio`'s `ChatPane::send`) — the UI polls the receiver with `try_recv()` each frame so the
/// work never blocks the egui update loop.
pub fn spawn_outcome<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx
}

/// Run on a worker thread; the single `RunOutcome` arrives on the returned receiver. The UI polls it
/// with `try_recv()` each frame so the backtest never blocks the egui update loop.
pub fn spawn_run(
    spec: StrategySpec,
    slice: DataSlice,
    store: StoreHandle,
    params: EngineParams,
) -> Receiver<RunOutcome> {
    spawn_outcome(move || run_slice(&spec, &slice, &store, params))
}

/// Walk-forward the strategy over `n_splits` OOS windows (anchored), default engine params. The thin
/// wrapper over [`run_walkforward_slice_with_params`] that supplies `EngineParams::default` — every
/// existing caller keeps its exact previous cost/cash behavior.
///
/// BAR slices only, and single-symbol: `walk_forward_strategy` splits ONE `&[Bar]` series into
/// windows. A tick or multi-symbol slice is a [`RunError::Data`] rather than a silent first-symbol
/// fallback.
pub fn run_walkforward_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
) -> Result<WalkForwardReport, RunError> {
    run_walkforward_slice_with_params(spec, slice, store, n_splits, EngineParams::default)
}

/// [`run_walkforward_slice`] with CALLER-SUPPLIED engine cost/cash: `make_params` builds a FRESH
/// [`EngineParams`] each time it is called, so every window (and the stitched cash line seeded from
/// `make_params().cash`) uses the SAME cost knobs. A per-call factory rather than one shared value
/// because `EngineParams` is not `Clone` (it can carry a `Box<dyn PositionSizer>`); the default-params
/// [`run_walkforward_slice`] delegates here with `EngineParams::default`. This is the seam the
/// compute-to-data `RunWalkforward` verb reaches (so a profile's `[engine]` cash/fee_rate/slippage
/// applies server-side), mirroring how [`run_slice`] already takes an `EngineParams`.
pub fn run_walkforward_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
    make_params: impl Fn() -> EngineParams,
) -> Result<WalkForwardReport, RunError> {
    // pre-resolve so a bad script/name fails as Compile/Strategy before the loop (the closure's
    // own build can't return an error).
    build_strategy(spec)?;
    if slice.symbols.len() > 1 {
        return Err(RunError::Data(
            "walk-forward runs over ONE bar series — pick a single symbol".into(),
        ));
    }
    let series = load_slice_bars(slice, store)?;
    let (sym, bars) = series.into_iter().next().expect("load_slice_bars rejects an empty list");
    let cash = make_params().cash;
    let rep = walk_forward_strategy(&bars, n_splits, WalkMode::Anchored, cash, 252.0, |window| {
        let strat = build_strategy(spec).expect("pre-checked above");
        StrategyEngine::new(vec![(sym.clone(), window.to_vec())], strat, make_params()).run()
    });
    Ok(rep)
}

/// Worker-thread twin of [`run_walkforward_slice`] (the SP2 `spawn_run` pattern).
pub fn spawn_walkforward(
    spec: StrategySpec,
    slice: DataSlice,
    store: StoreHandle,
    n_splits: usize,
) -> Receiver<Result<WalkForwardReport, RunError>> {
    spawn_outcome(move || run_walkforward_slice(&spec, &slice, &store, n_splits))
}

/// One sweep grid point's outcome.
#[derive(Debug, Clone)]
pub struct SweepEntry {
    pub overrides: Vec<(String, f64)>,
    pub result: BacktestResult,
}

/// A ranked parameter sweep + its deflated Sharpe and PBO across the trial set.
#[derive(Debug, Clone)]
pub struct StudioSweep {
    pub entries: Vec<SweepEntry>, // ranked best-first by annualized Sharpe
    pub dsr: f64,
    /// Probability of backtest overfitting (CSCV) across the trials; `NaN` = not assessed
    /// (< 2 trials, too-short slice, or non-finite returns).
    pub pbo: f64,
    pub best_index: usize, // always 0 (entries are pre-ranked) — kept explicit for the UI
}

/// CSCV split count for PBO — even (López de Prado canonical S = 16; `C(16,8) = 12_870` combos,
/// computed off the UI thread inside `run_sweep_slice`). Guarded by `T >= N_SPLITS`.
const N_SPLITS: usize = 16;

/// The N per-trial per-observation return columns, each truncated to the common length T (the min
/// column length — all trials run over identical bars so lengths are equal in practice; the
/// truncation is a defensive guard against a jagged matrix). Column-major — exactly what
/// `deflated_sharpe_with_effective_n` wants; PBO uses [`transpose`].
fn trial_columns(entries: &[SweepEntry]) -> (Vec<Vec<f64>>, usize) {
    let mut cols: Vec<Vec<f64>> = entries.iter().map(|e| returns(&e.result.equity_curve)).collect();
    let t = cols.iter().map(|c| c.len()).min().unwrap_or(0);
    for c in &mut cols {
        c.truncate(t);
    }
    (cols, t)
}

/// Column-major `columns[N][T]` -> row-major `matrix[T][N]` (`matrix[t][j] = columns[j][t]`) — the
/// layout [`pbo_cscv`] indexes.
fn transpose(columns: &[Vec<f64>], t: usize) -> Vec<Vec<f64>> {
    (0..t).map(|row| columns.iter().map(|c| c[row]).collect()).collect()
}

/// Rank sweep entries best-first by annualized Sharpe through the harness's ONE ranking
/// comparator ([`vike_backtest::harness::cmp_scores_desc`]): higher Sharpe first, a NaN
/// ("unrankable") Sharpe — e.g. an equity curve poisoned by a NaN from a blown-up strategy —
/// LAST, so a degenerate point can never sit at `best_index` 0 and feed the DSR. (The old
/// hand-rolled `partial_cmp(..).unwrap_or(Equal)` compared a NaN Equal to everything, so a
/// NaN point placed first by grid order stayed ranked #1.) `sort_by` is stable, so
/// equal-Sharpe points keep grid order.
fn rank_entries_by_sharpe(entries: &mut [SweepEntry]) {
    entries.sort_by(|a, b| {
        cmp_scores_desc(
            sharpe(&a.result.equity_curve, 252.0),
            sharpe(&b.result.equity_curve, 252.0),
        )
    });
}

/// Cartesian product of a `(name, values)` grid into per-point `(name, value)` override lists.
fn cartesian(grid: &[(String, Vec<f64>)]) -> Vec<Vec<(String, f64)>> {
    let mut out: Vec<Vec<(String, f64)>> = vec![Vec::new()];
    for (name, values) in grid {
        let mut next = Vec::new();
        for base in &out {
            for &v in values {
                let mut row = base.clone();
                row.push((name.clone(), v));
                next.push(row);
            }
        }
        out = next;
    }
    out
}

/// Sweep the strategy over the grid: build-per-point (with that point's overrides), backtest each,
/// rank by annualized Sharpe (NaN Sharpe LAST — see [`rank_entries_by_sharpe`]), and compute the
/// deflated Sharpe across the trial Sharpes. A point that fails to build is skipped (resilient,
/// like the harness `run_sweep`); all-failing -> error.
///
/// **Parallel, on the harness's BOUNDED pool** (`vike_backtest::harness::map_bounded` —
/// `install_bounded`/`sweep_threads`, `VIKE_SWEEP_THREADS`-capped, never rayon's global pool). Same
/// reasoning as the harness sweep: each point materializes its own copy of the slice (a tick point
/// re-scans the store outright), so concurrency is an `N_threads x slice` memory multiplier on
/// boxes that also host live trading.
///
/// **Determinism does not rest on scheduling.** `map_bounded` reassembles by index, so the surviving
/// entries reach the ranking sort in exact `cartesian(grid)` order — the same sequence a
/// one-at-a-time loop produced — and `sort_by` is stable, so equal-Sharpe points keep grid order.
pub fn run_sweep_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<f64>)],
) -> Result<StudioSweep, RunError> {
    run_sweep_slice_with_params(spec, slice, store, grid, EngineParams::default)
}

/// [`run_sweep_slice`] with CALLER-SUPPLIED engine cost/cash: `make_params` builds a FRESH
/// [`EngineParams`] for EACH grid point, so every point runs under the SAME cost knobs (cash /
/// fee_rate / slippage). A per-point factory rather than one shared value because `EngineParams` is
/// not `Clone` (it can carry a `Box<dyn PositionSizer>`) AND each point runs on the bounded parallel
/// pool — hence the `Send + Sync` bound on the factory. The default-params [`run_sweep_slice`]
/// delegates here with `EngineParams::default`. This is the seam the compute-to-data `RunSweep` verb
/// reaches so a profile's `[engine]` costs apply server-side, mirroring how [`run_slice`] already
/// takes an `EngineParams`.
pub fn run_sweep_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<f64>)],
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<StudioSweep, RunError> {
    // Bar slices load ONCE and every point clones the series (the load is the expensive part and it
    // is identical across points). Tick slices have no such hoist: `replay_ticks` owns its own
    // scan, so each point re-reads the store — hence the bounded pool above.
    let bars = match slice.kind {
        SliceKind::Bars => Some(load_slice_bars(slice, store)?),
        SliceKind::Ticks => None,
    };
    let points = cartesian(grid);
    let rows: Vec<Option<SweepEntry>> = map_bounded(points, |overrides| {
        let strat = build_strategy_with(spec, &overrides).ok()?; // skip a failing point
        let result = match &bars {
            Some(series) => StrategyEngine::new(series.clone(), strat, make_params()).run(),
            None => run_tick_slice(strat, slice, store, make_params()).ok()?,
        };
        Some(SweepEntry { overrides, result })
    });
    let mut entries: Vec<SweepEntry> = rows.into_iter().flatten().collect();
    if entries.is_empty() {
        return Err(RunError::Compile("every sweep point failed to run".into()));
    }
    rank_entries_by_sharpe(&mut entries);
    // Every DSR input comes from the shared `overfit::sharpe_moments` derivation, which owns the
    // per-period-Sharpe and non-excess-kurtosis conventions (see its doc — both are footguns; the
    // annualized-Sharpe one already shipped as a bug in vike-ai). The ranking sort above is
    // deliberately the ANNUALIZED Sharpe: a display/ordering quantity, never a DSR input.
    let trials: Vec<f64> =
        entries.iter().map(|e| sharpe_moments(&e.result.equity_curve).sr_per_obs).collect();
    let (columns, t) = trial_columns(&entries);
    let best = &entries[0].result;
    let m = sharpe_moments(&best.equity_curve);
    let dsr =
        deflated_sharpe_with_effective_n(m.sr_per_obs, &trials, &columns, m.n_obs, m.skew, m.kurt);
    let pbo = if entries.len() >= 2 && t >= N_SPLITS {
        pbo_cscv(&transpose(&columns, t), N_SPLITS)
    } else {
        f64::NAN
    };
    Ok(StudioSweep { entries, dsr, pbo, best_index: 0 })
}

/// Worker-thread twin of [`run_sweep_slice`].
pub fn spawn_sweep(
    spec: StrategySpec,
    slice: DataSlice,
    store: StoreHandle,
    grid: Vec<(String, Vec<f64>)>,
) -> Receiver<Result<StudioSweep, RunError>> {
    spawn_outcome(move || run_sweep_slice(&spec, &slice, &store, &grid))
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
/// Saved pane's "Compare all" twin of `spawn_run`/`spawn_sweep`/`spawn_walkforward`.
pub fn spawn_compare_all(
    strategies: Vec<(String, StrategySpec)>,
    slice: DataSlice,
    store: StoreHandle,
) -> Receiver<CompareOutcome> {
    spawn_outcome(move || compare_all_slice(&strategies, &slice, &store))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::params_from_rows;
    use std::sync::Arc;
    use vike_data::{DataFusionHist, HistStore, TsRange};
    use vike_model::{Bar, BookUpdate, BookUpdateKind, QuoteTick, TradeTick};

    const SCRIPT: &str = r#"
const QTY = 1.0;
fn on_bar() {
    let f = sma(5); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

    fn rhai(src: &str) -> StrategySpec {
        StrategySpec::rhai(src)
    }

    fn seeded_store() -> (tempfile::TempDir, StoreHandle, Vec<Bar>) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        // deterministic oscillating walk so the cross triggers
        let mut px = 100.0f64;
        let mut seed = 0x1234_5678u64;
        let bars: Vec<Bar> = (0..400)
            .map(|i| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                px = (px + ((seed >> 32) as f64 / u32::MAX as f64 - 0.5) * 2.0).max(1.0);
                Bar {
                    ts: 60_000 * (i as i64 + 1),
                    open: px,
                    high: px,
                    low: px,
                    close: px,
                    volume: 0.0,
                    funding: None,
                    bid: None,
                    ask: None,
                    symbol: Some("BTCUSDT".into()),
                }
            })
            .collect();
        store.append_bars("binance", "BTCUSDT", "1m", &bars, None).unwrap();
        (dir, Arc::new(store) as StoreHandle, bars)
    }

    fn slice() -> DataSlice {
        DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all())
    }

    #[test]
    fn run_slice_backtests_over_the_store() {
        let (_dir, store, _bars) = seeded_store();
        let out = run_slice(&rhai(SCRIPT), &slice(), &store, EngineParams::default());
        let res = out.expect("should backtest");
        assert!(res.n_trades > 0, "the cross should trade over 400 bars");
    }

    #[test]
    fn compile_error_maps_to_runerror_compile() {
        let (_dir, store, _b) = seeded_store();
        let out = run_slice(&rhai("fn on_bar( {"), &slice(), &store, EngineParams::default());
        assert!(matches!(out, Err(RunError::Compile(_))));
    }

    #[test]
    fn empty_slice_maps_to_runerror_data() {
        let (_dir, store, _b) = seeded_store();
        let missing = DataSlice::bars("binance", "NOPE", "1m", TsRange::all());
        let out = run_slice(&rhai(SCRIPT), &missing, &store, EngineParams::default());
        assert!(matches!(out, Err(RunError::Data(_))));
    }

    #[test]
    fn spawn_run_delivers_the_same_outcome() {
        let (_dir, store, _b) = seeded_store();
        let rx = spawn_run(rhai(SCRIPT), slice(), store.clone(), EngineParams::default());
        let out = rx.recv().expect("worker sends one outcome");
        assert!(out.unwrap().n_trades > 0);
    }

    // ---- native (registry) strategies -------------------------------------------------------

    /// The native path runs a REGISTRY strategy — no Rhai anywhere — and its params table is
    /// honored: `buy_hold` with `size = 2` buys 2 units once, so the run produces a position and
    /// a non-flat curve.
    #[test]
    fn native_strategy_runs_from_the_registry_with_params() {
        let (_dir, store, _b) = seeded_store();
        let spec = StrategySpec::native(
            "buy_hold",
            params_from_rows(&[("size".into(), "2".into()), ("symbol".into(), "BTCUSDT".into())]),
        );
        let res = run_slice(&spec, &slice(), &store, EngineParams::default())
            .expect("buy_hold should run natively");
        assert!(!res.equity_curve.is_empty(), "a native run produces an equity curve");
    }

    #[test]
    fn every_registered_native_strategy_resolves() {
        for name in crate::spec::native_strategies() {
            assert!(
                build_strategy(&StrategySpec::native_default(*name)).is_ok(),
                "{name} should build"
            );
        }
    }

    #[test]
    fn unknown_native_name_is_a_strategy_error_not_a_compile_error() {
        let (_dir, store, _b) = seeded_store();
        let spec = StrategySpec::native_default("no_such_strategy");
        let out = run_slice(&spec, &slice(), &store, EngineParams::default());
        assert!(matches!(out, Err(RunError::Strategy(_))), "got {out:?}");
    }

    #[test]
    fn native_walkforward_and_sweep_use_the_registry_too() {
        let (_dir, store, _b) = seeded_store();
        let spec = StrategySpec::native(
            "buy_hold",
            params_from_rows(&[("symbol".into(), "BTCUSDT".into())]),
        );
        let rep = run_walkforward_slice(&spec, &slice(), &store, 4).unwrap();
        assert_eq!(rep.windows.len(), 4);

        let grid = vec![("size".to_string(), vec![1.0, 2.0, 3.0])];
        let sw = run_sweep_slice(&spec, &slice(), &store, &grid).unwrap();
        assert_eq!(sw.entries.len(), 3, "a native sweep overrides strategy.params per point");
    }

    // ---- tick slices -------------------------------------------------------------------------

    fn tick_store() -> (tempfile::TempDir, StoreHandle) {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let quotes: Vec<QuoteTick> = (0..200)
            .map(|i| QuoteTick {
                ts: 1_000 * (i as i64 + 1),
                local_ts: 1_000 * (i as i64 + 1),
                bid: 100.0 + (i % 5) as f64,
                ask: 100.5 + (i % 5) as f64,
                bid_size: 5.0,
                ask_size: 5.0,
                symbol: "TKN".to_string(),
            })
            .collect();
        let trades: Vec<TradeTick> = (0..200)
            .map(|i| TradeTick {
                ts: 1_000 * (i as i64 + 1),
                local_ts: 1_000 * (i as i64 + 1),
                price: 100.25 + (i % 5) as f64,
                size: 1.0,
                is_buyer_maker: i % 2 == 0,
                symbol: "TKN".to_string(),
            })
            .collect();
        store.append_quotes("polymarket", "TKN", &quotes, None).unwrap();
        store.append_trades("polymarket", "TKN", &trades, None).unwrap();
        (dir, Arc::new(store) as StoreHandle)
    }

    #[test]
    fn tick_series_lists_each_venue_symbol_once_across_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let q = vec![QuoteTick {
            ts: 1,
            local_ts: 1,
            bid: 1.0,
            ask: 1.1,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "TKN".into(),
        }];
        let t = vec![TradeTick {
            ts: 1,
            local_ts: 1,
            price: 1.05,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "TKN".into(),
        }];
        store.append_quotes("polymarket", "TKN", &q, None).unwrap();
        store.append_trades("polymarket", "TKN", &t, None).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0)], None).unwrap();

        let ticks = tick_series(&store).unwrap();
        assert_eq!(ticks, vec![("polymarket".to_string(), "TKN".to_string())]);
        // and the bar filter still sees only the bar series
        assert_eq!(
            bar_series(&store).unwrap(),
            vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
        );
    }

    /// One conflating-L2 snapshot, the shape `LiveDataSink::l2_snapshot` records.
    fn depth_snapshot(ts: i64, symbol: &str) -> BookUpdate {
        BookUpdate {
            ts,
            local_ts: ts,
            seq: 0,
            kind: BookUpdateKind::Snapshot,
            tick_size: 0.01,
            bids: vec![(100.0, 1.0)],
            asks: vec![(100.5, 1.0)],
            symbol: symbol.to_string(),
        }
    }

    /// **The instrument that used to vanish.** Recorded through `subscribe_depth` alone, it is a
    /// real series on disk that `tick_series` cannot replay — and it appeared in NO list, so the
    /// Studio showed the same nothing for "not recorded" and for "recorded, unrunnable".
    /// [`depth_only_series`] is that instrument's one appearance.
    #[test]
    fn a_depth_only_instrument_is_not_replayable_but_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

        assert!(
            tick_series(&store).unwrap().is_empty(),
            "depth is the CONFLATING lane — replay_ticks reads no depth, so no runnable row exists"
        );
        assert_eq!(
            depth_only_series(&store).unwrap(),
            vec![("binance".to_string(), "BTCUSDT".to_string())],
            "...and the instrument is NAMED rather than dropped in silence"
        );
    }

    /// The complement: an instrument with a REPLAYABLE lane is a tick row, so its depth series is
    /// not a second entry anywhere. The disclosure reports what the filter COSTS, never a second
    /// opinion about an instrument the picker already lists.
    #[test]
    fn depth_beside_a_replayable_lane_is_not_reported_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let t = vec![TradeTick {
            ts: 1,
            local_ts: 1,
            price: 100.25,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "BTCUSDT".into(),
        }];
        store.append_trades("binance", "BTCUSDT", &t, None).unwrap();
        store.append_depth("binance", "BTCUSDT", &[depth_snapshot(1, "BTCUSDT")], None).unwrap();

        assert_eq!(
            tick_series(&store).unwrap(),
            vec![("binance".to_string(), "BTCUSDT".to_string())],
            "the trade tape makes it runnable"
        );
        assert!(depth_only_series(&store).unwrap().is_empty(), "already listed — nothing is lost");
    }

    /// The two sets are complements over the tick lanes, not two independently maintained lists:
    /// every `kind=` either replays or is the one disclosed exclusion, and nothing is both.
    #[test]
    fn the_replayable_and_disclosed_tick_kinds_do_not_overlap() {
        assert!(
            !REPLAYABLE_TICK_KINDS.contains(&UNREPLAYABLE_TICK_KIND),
            "a kind cannot both replay and be disclosed as unreplayable"
        );
    }

    fn bar_at(ts: i64) -> Bar {
        Bar {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        }
    }

    /// A `SliceKind::Ticks` slice routes to `replay_ticks` (NOT `load_bars`): the store below has
    /// no bar series at all, so a bar-path run could not possibly produce this result.
    #[test]
    fn tick_slice_replays_recorded_ticks() {
        let (_dir, store) = tick_store();
        let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
        let spec =
            StrategySpec::native("buy_hold", params_from_rows(&[("symbol".into(), "TKN".into())]));
        let res = run_slice(&spec, &slice, &store, EngineParams::default())
            .expect("tick replay should run");
        assert!(!res.equity_curve.is_empty(), "the tick replay stamps an equity curve");
    }

    #[test]
    fn tick_slice_with_no_recorded_ticks_is_a_data_error() {
        let (_dir, store) = tick_store();
        let slice = DataSlice::ticks("polymarket", vec!["MISSING".to_string()], TsRange::all());
        let out = run_slice(&rhai("fn on_bar() {}"), &slice, &store, EngineParams::default());
        assert!(matches!(out, Err(RunError::Data(_))), "got {out:?}");
    }

    #[test]
    fn a_bar_only_runner_rejects_a_tick_slice_instead_of_panicking() {
        let (_dir, store) = tick_store();
        let slice = DataSlice::ticks("polymarket", vec!["TKN".to_string()], TsRange::all());
        assert!(matches!(
            run_walkforward_slice(&rhai(SCRIPT), &slice, &store, 4),
            Err(RunError::Data(_))
        ));
    }

    // ---- multi-symbol ------------------------------------------------------------------------

    #[test]
    fn multi_symbol_bar_slice_registers_every_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let mk = |sym: &str, base: f64| -> Vec<Bar> {
            (0..50)
                .map(|i| {
                    let c = base + (i % 7) as f64;
                    Bar {
                        symbol: Some(sym.to_string()),
                        open: c,
                        high: c,
                        low: c,
                        close: c,
                        ..bar_at(60_000 * (i as i64 + 1))
                    }
                })
                .collect()
        };
        store.append_bars("binance", "BTCUSDT", "1m", &mk("BTCUSDT", 100.0), None).unwrap();
        store.append_bars("binance", "ETHUSDT", "1m", &mk("ETHUSDT", 50.0), None).unwrap();
        let store: StoreHandle = Arc::new(store);

        let slice = DataSlice::multi_bars(
            "binance",
            vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()],
            "1m",
            TsRange::all(),
        );
        let series = load_slice_bars(&slice, &store).unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].0, "BTCUSDT");
        assert_eq!(series[1].0, "ETHUSDT");

        let res = run_slice(
            &StrategySpec::native_default("rotation_top_k"),
            &slice,
            &store,
            EngineParams::default(),
        )
        .expect("a two-symbol run should backtest");
        assert!(!res.equity_curve.is_empty());
    }

    /// Misaligned per-symbol bar counts are `StrategyEngine::new`'s own assert — surfaced as a
    /// `RunError` here rather than panicking the worker thread.
    #[test]
    fn misaligned_multi_symbol_slice_is_a_data_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        let bars_a: Vec<Bar> = (0..20).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
        let bars_b: Vec<Bar> = (0..5).map(|i| bar_at(60_000 * (i as i64 + 1))).collect();
        store.append_bars("binance", "AAA", "1m", &bars_a, None).unwrap();
        store.append_bars("binance", "BBB", "1m", &bars_b, None).unwrap();
        let store: StoreHandle = Arc::new(store);

        let slice = DataSlice::multi_bars(
            "binance",
            vec!["AAA".to_string(), "BBB".to_string()],
            "1m",
            TsRange::all(),
        );
        assert!(matches!(load_slice_bars(&slice, &store), Err(RunError::Data(_))));
    }

    #[test]
    fn slice_label_and_symbol_accessor() {
        let s = DataSlice::bars("binance", "BTCUSDT", "1m", TsRange::all());
        assert_eq!(s.symbol(), "BTCUSDT");
        assert_eq!(s.label(), "binance · BTCUSDT · 1m");
        let t = DataSlice::ticks("polymarket", vec!["A".into(), "B".into()], TsRange::all());
        assert_eq!(t.label(), "polymarket · A+1 · ticks");
    }

    // ---- compare / walk-forward / sweep ------------------------------------------------------

    const NOOP_SCRIPT: &str = "fn on_bar() {}";

    #[test]
    fn compare_all_slice_runs_every_strategy_and_carries_names() {
        let (_dir, store, _b) = seeded_store();
        let strategies =
            vec![("noop".to_string(), rhai(NOOP_SCRIPT)), ("cross".to_string(), rhai(SCRIPT))];
        let results = compare_all_slice(&strategies, &slice(), &store);
        assert_eq!(results.len(), 2);
        let noop = results.iter().find(|(n, _)| n == "noop").unwrap();
        assert_eq!(noop.1.as_ref().unwrap().n_trades, 0);
        let cross = results.iter().find(|(n, _)| n == "cross").unwrap();
        assert!(cross.1.as_ref().unwrap().n_trades > 0);
    }

    /// A Rhai row and a NATIVE row compare side by side in one pass — the compare pane's widening.
    #[test]
    fn compare_all_slice_mixes_rhai_and_native_rows() {
        let (_dir, store, _b) = seeded_store();
        let strategies = vec![
            ("cross".to_string(), rhai(SCRIPT)),
            (
                "hold".to_string(),
                StrategySpec::native(
                    "buy_hold",
                    params_from_rows(&[("symbol".into(), "BTCUSDT".into())]),
                ),
            ),
            ("nope".to_string(), StrategySpec::native_default("not_a_strategy")),
        ];
        let results = compare_all_slice(&strategies, &slice(), &store);
        assert_eq!(results.len(), 3);
        assert!(results.iter().find(|(n, _)| n == "cross").unwrap().1.is_ok());
        assert!(results.iter().find(|(n, _)| n == "hold").unwrap().1.is_ok());
        let bad = results.iter().find(|(n, _)| n == "nope").unwrap();
        assert!(bad.1.is_err(), "an unknown native name surfaces as a failed row, not a panic");
    }

    #[test]
    fn compare_all_slice_stringifies_a_per_strategy_failure_without_dropping_the_row() {
        let (_dir, store, _b) = seeded_store();
        let strategies = vec![
            ("broken".to_string(), rhai("fn on_bar( {")),
            ("ok".to_string(), rhai(NOOP_SCRIPT)),
        ];
        let results = compare_all_slice(&strategies, &slice(), &store);
        assert_eq!(results.len(), 2, "a failing strategy stays in the output, not dropped");
        let broken = results.iter().find(|(n, _)| n == "broken").unwrap();
        assert!(broken.1.is_err());
        let ok = results.iter().find(|(n, _)| n == "ok").unwrap();
        assert!(ok.1.is_ok());
    }

    #[test]
    fn spawn_compare_all_delivers_the_same_outcome_as_the_sync_call() {
        let (_dir, store, _b) = seeded_store();
        let strategies = vec![("cross".to_string(), rhai(SCRIPT))];
        let rx = spawn_compare_all(strategies.clone(), slice(), store.clone());
        let got = rx.recv().expect("worker sends one outcome");
        let want = compare_all_slice(&strategies, &slice(), &store);
        assert_eq!(got.len(), want.len());
        assert_eq!(got[0].0, want[0].0);
        assert_eq!(got[0].1.as_ref().unwrap().n_trades, want[0].1.as_ref().unwrap().n_trades);
    }

    #[test]
    fn walkforward_slice_stitches_oos_windows() {
        let (_dir, store, _b) = seeded_store();
        let rep = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 4).unwrap();
        assert_eq!(rep.windows.len(), 4);
        assert!(!rep.oos_equity_curve.is_empty());
        assert!((0.0..=1.0).contains(&rep.wf_consistency));
    }

    #[test]
    fn walkforward_compile_error_maps_to_compile() {
        let (_dir, store, _b) = seeded_store();
        assert!(matches!(
            run_walkforward_slice(&rhai("fn on_bar( {"), &slice(), &store, 4),
            Err(RunError::Compile(_))
        ));
    }

    const SWEEP_SCRIPT: &str = r#"
const QTY = 1.0;
let fast = param("fast", 5.0);
fn on_bar() {
    let f = sma(fast.to_int()); let s = sma(20);
    if s.is_nan() { return; }
    let target = if f > s { QTY } else { -QTY };
    let delta = target - position();
    if abs(delta) > 1e-12 { market(if delta > 0.0 { 1 } else { -1 }, abs(delta)); }
}
"#;

    #[test]
    fn sweep_slice_ranks_and_deflates() {
        let (_dir, store, _b) = seeded_store();
        let grid = vec![("fast".to_string(), vec![3.0, 5.0, 8.0])];
        let sw = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        assert_eq!(sw.entries.len(), 3, "one entry per grid point");
        assert!((0.0..=1.0).contains(&sw.dsr));
        assert_eq!(sw.best_index, 0);
        // ranked best-first by annualized sharpe (non-increasing)
        let s: Vec<f64> = sw
            .entries
            .iter()
            .map(|e| vike_backtest::metrics::sharpe(&e.result.equity_curve, 252.0))
            .collect();
        for w in s.windows(2) {
            if w[0].is_nan() || w[1].is_nan() {
                continue;
            }
            assert!(w[0] >= w[1]);
        }
    }

    /// DETERMINISM GATE for the bounded-pool sweep: the SAME grid run twice must produce the same
    /// entries in the same rank order with BIT-identical equity, no matter which point finished
    /// first. `map_bounded` reassembles by index and the ranking sort is stable, so ordering can
    /// never depend on scheduling.
    #[test]
    fn parallel_sweep_is_deterministic_across_repeated_runs() {
        let (_dir, store, _b) = seeded_store();
        // Enough points that the bounded pool really interleaves them.
        let grid = vec![("fast".to_string(), vec![2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])];
        let a = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        let b = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        assert_eq!(a.entries.len(), 8);
        assert_eq!(a.entries.len(), b.entries.len());
        for (x, y) in a.entries.iter().zip(&b.entries) {
            assert_eq!(x.overrides, y.overrides, "same point at the same rank");
            assert_eq!(
                x.result.final_equity.to_bits(),
                y.result.final_equity.to_bits(),
                "bit-identical equity across runs"
            );
        }
        assert_eq!(a.dsr.to_bits(), b.dsr.to_bits());
    }

    fn entry(curve: Vec<f64>) -> SweepEntry {
        let last = *curve.last().unwrap();
        SweepEntry {
            overrides: vec![],
            result: BacktestResult {
                equity_curve: curve,
                final_equity: last,
                ..Default::default()
            },
        }
    }

    /// Regression (mirrors the harness sweep's `nan_sharpe_ranks_last_never_first`): a NaN-Sharpe
    /// entry constructed FIRST must rank LAST — with the old
    /// `partial_cmp(..).unwrap_or(Equal)` sort it compared Equal to every finite point and,
    /// grid-placed first, stayed at rank #1 (= `best_index` 0, feeding the DSR).
    #[test]
    fn nan_sharpe_entry_ranks_last_never_best_index() {
        // A NaN in the equity curve (a blown-up strategy) poisons mean/var -> NaN sharpe.
        let degenerate = entry(vec![1000.0, f64::NAN, 1000.0, 1000.0]);
        let good = entry(vec![1000.0, 1010.0, 1005.0, 1030.0]);
        assert!(sharpe(&degenerate.result.equity_curve, 252.0).is_nan(), "precondition");
        assert!(sharpe(&good.result.equity_curve, 252.0).is_finite(), "precondition");

        // NaN entry deliberately placed first, so a broken comparator would leave it ranked #1.
        let mut entries = vec![degenerate, good];
        rank_entries_by_sharpe(&mut entries);
        assert!(
            sharpe(&entries[0].result.equity_curve, 252.0).is_finite(),
            "finite-Sharpe point is best_index 0"
        );
        assert!(
            sharpe(&entries[1].result.equity_curve, 252.0).is_nan(),
            "NaN-Sharpe point ranks last, not first"
        );
    }

    #[test]
    fn trial_columns_are_per_trial_returns_and_transpose_is_consistent() {
        let entries =
            vec![entry(vec![100.0, 110.0, 121.0, 133.0]), entry(vec![100.0, 90.0, 99.0, 108.0])];
        let (cols, t) = trial_columns(&entries);
        assert_eq!(cols.len(), 2, "N columns");
        assert!(cols.iter().all(|c| c.len() == t), "each column length T");
        for (j, e) in entries.iter().enumerate() {
            let r = vike_backtest::metrics::returns(&e.result.equity_curve);
            assert_eq!(cols[j], r[..t].to_vec(), "column j is trial j's returns");
        }
        let m = transpose(&cols, t);
        assert_eq!(m.len(), t, "matrix has T rows");
        assert!(m.iter().all(|row| row.len() == 2), "each row width N");
        for ti in 0..t {
            for j in 0..2 {
                assert_eq!(m[ti][j], cols[j][ti], "matrix[t][j] == columns[j][t]");
            }
        }
    }

    #[test]
    fn sweep_populates_pbo_for_a_multi_point_grid() {
        let (_dir, store, _b) = seeded_store();
        let grid = vec![("fast".to_string(), vec![3.0, 5.0, 8.0])];
        let sw = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        assert!(
            (0.0..=1.0).contains(&sw.pbo) || sw.pbo.is_nan(),
            "pbo in [0,1] or NaN, got {}",
            sw.pbo
        );
    }

    #[test]
    fn sweep_pbo_is_nan_for_a_single_point_grid() {
        let (_dir, store, _b) = seeded_store();
        let grid = vec![("fast".to_string(), vec![5.0])];
        let sw = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        assert!(sw.pbo.is_nan(), "single trial -> PBO not assessable, got {}", sw.pbo);
    }

    #[test]
    fn sweep_dsr_is_effective_n_deflated() {
        let (_dir, store, _b) = seeded_store();
        // closely-spaced fast values -> strongly correlated equity curves -> effective_n < N,
        // so the effective-N DSR differs from the plain deflated_sharpe_ratio on the same inputs.
        let grid = vec![("fast".to_string(), vec![3.0, 4.0, 5.0, 6.0])];
        let sw = run_sweep_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();

        let per_obs = |r: &BacktestResult| {
            vike_backtest::metrics::sharpe(&r.equity_curve, 252.0) / 252.0_f64.sqrt()
        };
        let trials: Vec<f64> = sw.entries.iter().map(|e| per_obs(&e.result)).collect();
        let best = &sw.entries[0].result;
        let n = vike_backtest::metrics::returns(&best.equity_curve).len();
        let skew = vike_backtest::metrics::returns_skewness(&best.equity_curve);
        let kurt = vike_backtest::metrics::returns_kurtosis(&best.equity_curve) + 3.0;
        let plain =
            vike_backtest::overfit::deflated_sharpe_ratio(per_obs(best), &trials, n, skew, kurt);

        assert!(
            (sw.dsr - plain).abs() > 1e-9,
            "effective-N DSR ({}) should differ from plain DSR ({})",
            sw.dsr,
            plain
        );
    }

    #[test]
    fn pbo_discriminates_overfit_from_stable() {
        // Hand-built per-trial columns (length 48 >= N_SPLITS) fed straight through the transpose
        // into pbo_cscv — asserts the ORDERING (oracle-parity retired; we port the math), decoupled
        // from any strategy that might not reliably overfit the RNG seed.
        let t = 48usize;
        let const_col = |v: f64| vec![v; t];
        let half_col = |first: f64, second: f64| -> Vec<f64> {
            (0..t).map(|i| if i < t / 2 { first } else { second }).collect()
        };
        // Stable: one column dominates in every row -> IS-best is always OOS-best -> PBO ~ 0.
        let stable = vec![const_col(1.0), const_col(0.0), const_col(0.0), const_col(0.0)];
        // Overfit: the IS-best flips to OOS-worst across the timeline halves -> high PBO.
        let overfit = vec![half_col(1.0, 0.0), half_col(0.0, 1.0), const_col(0.5), const_col(0.5)];
        let stable_pbo = vike_backtest::overfit::pbo_cscv(&transpose(&stable, t), N_SPLITS);
        let overfit_pbo = vike_backtest::overfit::pbo_cscv(&transpose(&overfit, t), N_SPLITS);
        assert!(
            overfit_pbo > 0.0,
            "overfit matrix should register some overfit, got {overfit_pbo}"
        );
        assert!(
            overfit_pbo > stable_pbo,
            "overfit PBO {overfit_pbo} should exceed stable PBO {stable_pbo}"
        );
    }

    /// split-plane B12: `bar_series`/`tick_series` are TRAIT enumerations — they must answer over
    /// a [`StoreHandle`] whose backing store is not a `DataFusionHist` at all (the production case
    /// is the RPC-backed `RemoteHistStore`; here the smallest seeded fake stands in, because
    /// `vike_data::test_support::MemHistStore` can only ever list the seams it stores for real —
    /// it cannot hold the `kind=bar` series this test needs). Every verb except `list_series`
    /// refuses, so the test also PROVES the two
    /// functions touch nothing but the catalog verb.
    #[test]
    fn series_enumeration_needs_only_the_trait() {
        use vike_data::{ExecFillRow, ExecOrderRow, SeriesCoverage, SeriesId};
        use vike_model::{BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

        /// `list_series` answers from the seeded list; every other verb is a hard error.
        struct CatalogOnlyStore(Vec<SeriesId>);
        fn refuse<T>(method: &str) -> Result<T, DataError> {
            Err(DataError::Query(format!("CatalogOnlyStore: {method} must not be called")))
        }
        impl HistStore for CatalogOnlyStore {
            fn list_series(&self) -> Result<Vec<SeriesId>, DataError> {
                Ok(self.0.clone())
            }
            fn inventory(&self) -> Result<Vec<(SeriesId, SeriesCoverage)>, DataError> {
                refuse("inventory")
            }
            fn load_bars(
                &self,
                _v: &str,
                _s: &str,
                _i: &str,
                _r: TsRange,
            ) -> Result<Vec<Bar>, DataError> {
                refuse("load_bars")
            }
            fn scan_quotes(
                &self,
                _v: &str,
                _s: &str,
                _r: TsRange,
            ) -> Result<Vec<QuoteTick>, DataError> {
                refuse("scan_quotes")
            }
            fn scan_trades(
                &self,
                _v: &str,
                _s: &str,
                _r: TsRange,
            ) -> Result<Vec<TradeTick>, DataError> {
                refuse("scan_trades")
            }
            fn scan_book_updates(
                &self,
                _v: &str,
                _s: &str,
                _r: TsRange,
            ) -> Result<Vec<BookUpdate>, DataError> {
                refuse("scan_book_updates")
            }
            fn append_bars(
                &self,
                _v: &str,
                _s: &str,
                _i: &str,
                _b: &[Bar],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_bars")
            }
            fn append_quotes(
                &self,
                _v: &str,
                _s: &str,
                _t: &[QuoteTick],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_quotes")
            }
            fn append_trades(
                &self,
                _v: &str,
                _s: &str,
                _t: &[TradeTick],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_trades")
            }
            fn append_book_updates(
                &self,
                _v: &str,
                _s: &str,
                _u: &[BookUpdate],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_book_updates")
            }
            fn append_symbol_properties(
                &self,
                _v: &str,
                _s: &str,
                _rows: &[(i64, SymbolProperties)],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_symbol_properties")
            }
            fn scan_symbol_properties(
                &self,
                _v: &str,
                _s: &str,
                _r: TsRange,
            ) -> Result<Vec<(i64, SymbolProperties)>, DataError> {
                refuse("scan_symbol_properties")
            }
            fn append_equity(
                &self,
                _v: &str,
                _s: &str,
                _rows: &[EquitySample],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_equity")
            }
            fn scan_equity(
                &self,
                _v: &str,
                _s: &str,
                _r: TsRange,
            ) -> Result<Vec<EquitySample>, DataError> {
                refuse("scan_equity")
            }
            fn append_exec_fills(
                &self,
                _v: &str,
                _s: &str,
                _rows: &[ExecFillRow],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_exec_fills")
            }
            fn scan_exec_fills(&self, _v: &str, _s: &str) -> Result<Vec<ExecFillRow>, DataError> {
                refuse("scan_exec_fills")
            }
            fn append_exec_orders(
                &self,
                _v: &str,
                _s: &str,
                _rows: &[ExecOrderRow],
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("append_exec_orders")
            }
            fn scan_exec_orders(&self, _v: &str, _s: &str) -> Result<Vec<ExecOrderRow>, DataError> {
                refuse("scan_exec_orders")
            }
            fn resample_quotes_to_bars(
                &self,
                _v: &str,
                _s: &str,
                _i: &str,
                _r: TsRange,
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("resample_quotes_to_bars")
            }
            fn resample_trades_to_bars(
                &self,
                _v: &str,
                _s: &str,
                _i: &str,
                _r: TsRange,
                _k: Option<&str>,
            ) -> Result<usize, DataError> {
                refuse("resample_trades_to_bars")
            }
        }

        let store: StoreHandle = Arc::new(CatalogOnlyStore(vec![
            SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".to_string())),
            SeriesId::per_symbol("quote", "polymarket", "TKN", None),
            SeriesId::per_symbol("trade", "polymarket", "TKN", None),
        ]));

        assert_eq!(
            bar_series(store.as_ref()).unwrap(),
            vec![("binance".to_string(), "BTCUSDT".to_string(), "1m".to_string())]
        );
        // quote + trade for the same (venue, symbol) still dedupe to ONE runnable tick row.
        assert_eq!(
            tick_series(store.as_ref()).unwrap(),
            vec![("polymarket".to_string(), "TKN".to_string())]
        );
    }
}
