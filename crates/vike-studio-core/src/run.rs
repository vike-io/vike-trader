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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex, OnceLock};
use vike_backtest::harness::walkforward::WindowSearch;
use vike_backtest::harness::{RankMetric, cmp_scores_desc, map_bounded};
use vike_backtest::hist_replay::{TickReplayConfig, replay_ticks};
use vike_backtest::metrics::{returns, sharpe};
use vike_backtest::overfit::{deflated_sharpe_with_effective_n, pbo_cscv, sharpe_moments};
use vike_backtest::report::{
    BacktestReport, DAILY_PERIODS_PER_YEAR, periods_per_year_for_interval,
};
use vike_backtest::validation::WalkMode;
use vike_backtest::walkforward::{WalkForwardReport, WindowOutcome, walk_forward_strategy};
use vike_backtest::{BacktestResult, EngineParams, SimBroker, StrategyEngine};
use vike_data::{DataError, HistStore, SeriesId, TsRange};
use vike_model::{Bar, Strategy};
use vike_script::RhaiStrategy;
use vike_strategy_plugin::host::{PluginStrategy, PluginVTable};
use vike_strategy_plugin::loader;

use crate::cost_model::FillMix;
use crate::spec::{StrategySpec, params_with_overrides};

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
/// the Studio's picker reaches it through [`series_lists`] rather than re-deriving the filter
/// (the retired `vike-mcp` `list_series` tool was the second caller; its successor,
/// `vike-cli mcp`, lists series over the datahub wire verb instead of a local store). Takes the
/// TRAIT (split-plane B12), so the Studio's picker enumerates a remote `RemoteHistStore` exactly
/// like a local `DataFusionHist`.
pub fn bar_series(store: &dyn HistStore) -> Result<Vec<(String, String, String)>, DataError> {
    Ok(bar_rows(&store.list_series()?))
}

/// [`bar_series`]'s filter, over an ALREADY-listed catalog — the pure half, so [`series_lists`]
/// can fold one `list_series` answer three ways instead of asking three times.
fn bar_rows(listed: &[SeriesId]) -> Vec<(String, String, String)> {
    listed
        .iter()
        .filter(|s| s.kind == "bar")
        .filter_map(|s| s.interval.clone().map(|iv| (s.venue.clone(), s.symbol.clone(), iv)))
        .collect()
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
    Ok(tick_rows(&store.list_series()?))
}

/// [`tick_series`]'s filter, over an ALREADY-listed catalog — the pure half (see [`bar_rows`]).
fn tick_rows(listed: &[SeriesId]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for s in listed {
        if !REPLAYABLE_TICK_KINDS.contains(&s.kind.as_str()) {
            continue;
        }
        let pair = (s.venue.clone(), s.symbol.clone());
        if !out.contains(&pair) {
            out.push(pair);
        }
    }
    out
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
    Ok(depth_only_rows(&store.list_series()?))
}

/// [`depth_only_series`]'s filter, over an ALREADY-listed catalog — the pure half (see
/// [`bar_rows`]).
fn depth_only_rows(listed: &[SeriesId]) -> Vec<(String, String)> {
    let mut depth: Vec<(String, String)> = Vec::new();
    let mut replayable: Vec<(String, String)> = Vec::new();
    for s in listed {
        let bucket = if s.kind == UNREPLAYABLE_TICK_KIND {
            &mut depth
        } else if REPLAYABLE_TICK_KINDS.contains(&s.kind.as_str()) {
            &mut replayable
        } else {
            continue;
        };
        let pair = (s.venue.clone(), s.symbol.clone());
        if !bucket.contains(&pair) {
            bucket.push(pair);
        }
    }
    depth.retain(|pair| !replayable.contains(pair));
    depth
}

/// Every list the Studio's slice picker draws, folded from ONE [`HistStore::list_series`] call.
///
/// The three fields are exactly what [`bar_series`], [`tick_series`] and [`depth_only_series`]
/// return — same filters, same order, same dedup, because [`series_lists`] and those three
/// functions call the SAME pure folds ([`bar_rows`]/[`tick_rows`]/[`depth_only_rows`]). There is
/// no second reading of the catalog rules for the two shapes to drift apart in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeriesLists {
    /// `(venue, symbol, interval)` per `kind=bar` series — [`bar_series`]'s answer.
    pub bars: Vec<(String, String, String)>,
    /// `(venue, symbol)` per instrument with a replayable tick lane — [`tick_series`]'s answer.
    pub ticks: Vec<(String, String)>,
    /// `(venue, symbol)` per instrument recorded ONLY as `kind=depth` — [`depth_only_series`]'s
    /// answer, the disclosure half.
    pub depth_only: Vec<(String, String)>,
}

/// Fold the store's catalog into all three picker lists with a SINGLE [`HistStore::list_series`]
/// call.
///
/// The Studio's picker used to call [`bar_series`], then [`tick_series`], then
/// [`depth_only_series`] — three enumerations of one catalog, and on a `RemoteHistStore` (which is
/// connect-per-read since split-plane B12) three fresh TCP connects for one Refresh click. The
/// three filters are pure functions of the SAME list, so asking three times bought nothing but
/// latency.
///
/// It also makes an existing claim literally true rather than nearly true:
/// `crates/vike-studio/src/picker.rs`'s picker — the sentence now lives on `SlicePicker::apply`, the
/// fold `SlicePicker::refresh` shares with the off-thread path — has always documented that "every filter folds the same
/// `list_series` call, so a mixed outcome — bars listed, ticks refused — is unreachable". With
/// three calls that held only because every impl in the tree answers the verb the same way twice
/// in a row; with one call it holds by construction.
///
/// It deliberately does NOT cache: this is a one-shot maintenance walk (open / Refresh), and a
/// cache would be a second answer that can disagree with the store.
pub fn series_lists(store: &dyn HistStore) -> Result<SeriesLists, DataError> {
    let listed = store.list_series()?;
    Ok(SeriesLists {
        bars: bar_rows(&listed),
        ticks: tick_rows(&listed),
        depth_only: depth_only_rows(&listed),
    })
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

/// Where a [`StrategySpec::Plugin`] artifact is looked up: `<project>/user_data/plugins`, resolved
/// by `vike_model::state_path`'s ordinary project walk from this process's working directory.
///
/// ⚠⚠ **THIS IS A SECOND PROJECT WALK, and `crates/vike-boot/tests/one_owner.rs`'s `WALK` table
/// classifies it as one. Read this before reusing the shape.** `vike-boot`'s whole contract is
/// that ONE walk decides: every project-relative path a root uses is derived from `Booted`, because
/// the `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND and a second call answers with
/// whatever the working directory happens to sit above. This function performs exactly that second
/// call, so a daemon booted with `VIKE_SETTINGS_DIR` pointing somewhere other than its working
/// directory would load plugins from a DIFFERENT project than the one it booted into — silently,
/// with a `plugin artifact not found` at best and somebody else's artifact at worst.
///
/// ⚠ **`a_crate_that_boots_may_not_walk_again` does NOT catch it**, and that is worth knowing
/// rather than discovering: that rule's roster is the crates whose own source calls
/// `vike_boot::boot`. This is a LIBRARY, reached from a booted root (`vike-backend backtest`) one
/// crate down, so the walk is outside the rule by construction. The `WALK` row is what records it.
///
/// **The bound, MEASURED rather than hoped for.** On every SHIPPED unit the two answers cannot
/// disagree: `crates/vike-ops/tests/deploy_layout_gate.rs`'s `every_daemon_unit_is_one_project_folder`
/// requires each daemon unit's `VIKE_SETTINGS_DIR` to be exactly `<WorkingDirectory>/settings`, so
/// the boot's project and this walk's project are the same directory by gate. **Two holes survive
/// it**: an `EnvironmentFile=` line BEATS `Environment=`, so an operator's `<root>/.env` can move
/// `VIKE_SETTINGS_DIR` where no gate can see; and a hand-run daemon (`cd elsewhere;
/// VIKE_SETTINGS_DIR=… vike-backend backtest --addr`) diverges outright.
///
/// **The cure, and why it is not here yet.** [`plugins_dir_from`] already takes the directory as a
/// parameter — answer (2) of that gate's own panic message — so what is missing is a caller holding
/// `Booted`'s answer. Threading it through `build_strategy` reaches four runners, three `*_local`
/// wire entries and `StudioRunTable`'s boxed-fn type in another crate; and the one composition root
/// that can name both halves (`crates/vike/src/main.rs`, layer 68) is FORBIDDEN to resolve a path
/// at all — `crates/vike-ops/tests/multicall_gate.rs`'s `the_dispatcher_starts_nothing` fails a PR
/// that so much as names `state_path::` there. The shape that fits is the one `study_run_fn`
/// already uses beside it: hand the dispatcher a FUNCTION ITEM and let `backtest_cli`'s `--addr`
/// arm — the rung that already walks for this daemon's settings — call it with the resolved root.
/// That is a new seam across three crates, which is a design change rather than a fix, so it is
/// named here rather than smuggled in.
///
/// What is true today: every shipped unit runs `WorkingDirectory=<project>`, so this resolves the
/// project the daemon was installed as — the same walk `state_path::user_runs_dir` and its
/// siblings already answer every other `user_data/` question with. It reads no environment
/// variable, so it adds no row to `vike_ops::settings::SETTINGS` and none to its `LIBRARY_PIN`
/// ratchet.
///
/// The fallback is the plain relative path, which is what the builder service's own out-dir
/// setting already defaults to: with no project marker above the working
/// directory there is nothing to resolve, and refusing outright would turn "no project" into a
/// different failure from the `LoadError::Missing` a caller is about to get anyway — with a worse
/// message.
#[must_use]
pub fn plugins_dir() -> PathBuf {
    plugins_dir_from(std::env::current_dir().ok().as_deref())
}

/// [`plugins_dir`] with the working directory supplied — `None` meaning "this process has none"
/// (a deleted or unreadable CWD, which `std::env::current_dir` reports as an error).
///
/// ⚠ **Split out so the FALLBACK branch can be proved rather than assumed.** The test that used to
/// cover it handed the resolver a temp directory and asserted `None` only *if* the resolver
/// happened to answer `None` — a conditional assertion, so the fallback was still reachable by
/// nothing. It cannot be made unconditional against a real filesystem either: the walk climbs to
/// `/`, and no test can promise that no ancestor of a temp directory carries a project marker.
/// Passing `None` here is the one input that reaches the fallback deterministically, on every box.
fn plugins_dir_from(cwd: Option<&Path>) -> PathBuf {
    cwd.and_then(vike_model::state_path::user_plugins_dir).unwrap_or_else(|| {
        PathBuf::from(vike_model::state_path::PROJECT_USER_DATA_DIR)
            .join(vike_model::state_path::PLUGINS_SUBDIR)
    })
}

/// The artifact a `(name, sha)` pair names, under `plugins_dir` — the design's
/// `user_data/plugins/<name>-<sha>.so`, and the exact shape the builder service writes
/// (`vike_strategy_builder::builder::artifact_strategy_name` parses it back).
fn plugin_artifact_path(plugins_dir: &Path, name: &str, sha: &str) -> PathBuf {
    plugins_dir.join(format!("{name}-{sha}.so"))
}

/// Every plugin artifact this process has already `dlopen`ed, keyed by its path.
///
/// **Why a cache and not a load per call.** `build_strategy` is called once per Run and once per
/// SWEEP POINT, and a sweep runs the SAME artifact with N different params tables. Without this,
/// an N-point sweep would issue N `dlopen`s of one file; with it, the artifact is opened once and
/// `create` is called N times — which is exactly the cost model
/// [`build_strategy_with`]'s own comment describes ("N constructions, not N compiles") and the
/// opposite of Rhai's per-point recompile.
///
/// **Why keeping the vtable past the `LoadedPlugin` is sound.** `vike_strategy_plugin::loader`
/// never calls `dlclose` — deliberately, and its module header says so — so the mapping the
/// function pointers point into lives for the life of the process whether or not anything holds
/// the handle. [`PluginVTable`] is `Copy` and holds only `extern "C"` function pointers, which are
/// `Send + Sync`.
///
/// The lock is held ACROSS the load on purpose: two sweep workers reaching a cold artifact at the
/// same moment should produce one `dlopen`, not two.
fn loaded_plugin_vtable(path: &Path) -> Result<PluginVTable, RunError> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, PluginVTable>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    // A poisoned lock here means a previous caller panicked while holding it; the map is a plain
    // path -> POD table with no invariant a panic could have broken, so recovering is strictly
    // better than turning one unrelated panic into every later Run failing.
    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(vt) = guard.get(path) {
        return Ok(*vt);
    }
    let loaded = loader::load(path).map_err(|e| RunError::Strategy(e.to_string()))?;
    guard.insert(path.to_path_buf(), loaded.vtable);
    Ok(loaded.vtable)
}

/// Resolve a [`StrategySpec`] into the boxed strategy every runner drives — the ONE place the
/// Rhai/native/plugin branch is taken. `dyn` dispatch is the harness registry's own shape and is
/// noise next to loading the slice (see `vike_backtest::harness::registry`'s module doc).
///
/// The plugin arm resolves its artifact under [`plugins_dir`]; [`build_strategy_in`] is the same
/// function with that directory supplied, which is what a caller that already knows where
/// `user_data` is should use.
pub fn build_strategy(spec: &StrategySpec) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    build_strategy_in(spec, &plugins_dir())
}

/// [`build_strategy`] with the plugin-artifact directory supplied rather than resolved.
pub fn build_strategy_in(
    spec: &StrategySpec,
    plugins_dir: &Path,
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => Ok(Box::new(
            RhaiStrategy::<SimBroker>::compile(src)
                .map_err(|e| RunError::Compile(e.to_string()))?,
        )),
        StrategySpec::Native { name, params } => {
            vike_backtest::harness::strategy_by_name(name, params)
                .map_err(|e| RunError::Strategy(e.to_string()))
        }
        // ⚠ **Fix-round-1 CRITICAL finding, and the PERMANENT half of the fix — read this before
        // deleting the empty-sha arm below when the loader lands.** Every caller that builds a
        // `StrategySpec::Plugin` from an absent sha (`StudioState::current_spec`,
        // `SavedStrategy::spec`) does so via `plugin_sha.clone().unwrap_or_default()` — an EMPTY
        // STRING, not a sentinel the type can distinguish from a real sha. This function is the
        // ONE place every Plugin spec is actually built, on EVERY path that reaches it: locally
        // (`run_slice`/`compare_all_slice`) and remotely (this same function runs again on the
        // compute daemon, reached through `to_strategy_spec`/`run_sweep_local`/
        // `run_walkforward_local`). So refusing an empty sha HERE, unconditionally, closes every
        // caller at once — today's four (Run/Sweep/Walk-Forward/Compare-all) and any future one —
        // rather than relying on each dispatcher to remember to ask `run_blocked_reason` first
        // (studio.rs now does too, as the belt; this is the buckle).
        //
        // ⚠ **This arm SURVIVED the loader wiring, which is what it was written to demand.** The
        // loader call is in the arm below, for a NON-empty sha; a real
        // `vike_strategy_plugin::loader::load` would itself fail to resolve `<name>-.so`, but that
        // is an ACCIDENT of a lookup failing, not a stated refusal — and relying on an accident is
        // exactly what this record was written to stop doing. It stays UNCONDITIONAL.
        StrategySpec::Plugin { name, sha, .. } if sha.trim().is_empty() => {
            Err(RunError::Strategy(format!(
                "plugin `{name}` has no sha — an empty sha names no artifact. Build it first \
                 (Studio: Run/Sweep/Walk-Forward are disabled until a Build returns one; \
                 Compare-all: this row reports this refusal as its result rather than a fabricated \
                 one)."
            )))
        }
        // THE JOIN — the design's Flow step 6. This arm used to return "the runtime loader is not
        // wired into this Run pipeline", which was the whole of what
        // `docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md` recorded.
        //
        // It runs on BOTH sides of the wire and that is the point: locally through
        // `run_slice`/`compare_all_slice`, and on the compute daemon through
        // `wire_run::to_strategy_spec` → `run_slice_local`. The daemon therefore loads the
        // artifact WITHOUT ever knowing a builder exists — it is handed a sha and reads a file,
        // exactly as the design requires ("the backtest server never talks to the builder — they
        // hand off through the FILESYSTEM"). Nothing here polls for an artifact to appear: a
        // missing file is a refusal on the spot, naming the build, never a wait.
        //
        // ⚠ `unsafe` stays where it was quarantined. Everything this arm touches
        // (`loader::load`, `PluginStrategy`) is the safe face of `vike-strategy-plugin`; this
        // crate declares no `unsafe` of its own and inherits the workspace `forbid`.
        StrategySpec::Plugin { name, sha, params } => {
            let path = plugin_artifact_path(plugins_dir, name, sha);
            let vtable = loaded_plugin_vtable(&path)?;
            // Params cross the C-ABI as TEXT, never as a `toml::Value` — no Rust type crosses that
            // boundary by value, and the plugin parses the document on its own side. An empty
            // table serializes to an empty string, which is a valid empty TOML document (the one
            // `vike-strategy-plugin`'s own `a_template_built_plugin_accepts_a_real_params_document`
            // pins, after a `str::parse::<toml::Value>()` in the template made every params string
            // — the empty one included — produce a null handle and a strategy that traded nothing).
            let params_toml = toml::to_string(params).map_err(|e| {
                RunError::Strategy(format!(
                    "plugin `{name}`: serializing its params table to TOML failed: {e}"
                ))
            })?;
            Ok(Box::new(PluginStrategy::<SimBroker>::new(vtable, &params_toml)))
        }
    }
}

/// [`build_strategy`] with one sweep point's `(name, value)` overrides applied: Rhai gets them as
/// script `param()` bindings (`compile_with_params`), native gets them merged into its params table.
pub fn build_strategy_with(
    spec: &StrategySpec,
    overrides: &[(String, f64)],
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    build_strategy_with_in(spec, overrides, &plugins_dir())
}

/// [`build_strategy_with`] with the plugin-artifact directory supplied rather than resolved — the
/// sweep twin of [`build_strategy_in`].
pub fn build_strategy_with_in(
    spec: &StrategySpec,
    overrides: &[(String, f64)],
    plugins_dir: &Path,
) -> Result<Box<dyn Strategy<SimBroker>>, RunError> {
    match spec {
        StrategySpec::Rhai(src) => {
            let map: indexmap::IndexMap<String, f64> = overrides.iter().cloned().collect();
            Ok(Box::new(
                RhaiStrategy::<SimBroker>::compile_with_params(src, map)
                    .map_err(|e| RunError::Compile(e.to_string()))?,
            ))
        }
        StrategySpec::Native { name, params } => build_strategy_in(
            &StrategySpec::Native {
                name: name.clone(),
                params: params_with_overrides(params, overrides),
            },
            plugins_dir,
        ),
        // A plugin's overrides merge into its params table exactly as Native's do above — the
        // plugin's `create` takes params as TOML, so a sweep point's overrides land in the same
        // table an ordinary Run would build.
        //
        // Worth stating here because it is the OPPOSITE performance shape from Rhai, and it is now
        // a fact rather than a forecast: a sweep of N grid points `dlopen`s the `.so` ONCE — the
        // artifact is already built, and `loaded_plugin_vtable` caches the opened table by path —
        // and calls `create` N times, one per params table. Rhai recompiles the script per point
        // (`compile_with_params`); the plugin does not. The cost of a plugin sweep is N
        // constructions, not N compiles and not N loads.
        StrategySpec::Plugin { name, sha, params } => build_strategy_in(
            &StrategySpec::Plugin {
                name: name.clone(),
                sha: sha.clone(),
                params: params_with_overrides(params, overrides),
            },
            plugins_dir,
        ),
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
///
/// `n_splits` must be `>= 1`, and the Sharpe annualization is derived from `slice.interval` rather
/// than fixed at the daily 252. Both belong to [`run_walkforward_slice_with_params`], whose doc
/// carries the argument for each — this wrapper adds only the default `EngineParams`.
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
/// [`run_walkforward_slice`] delegates here with `EngineParams::default`.
///
/// ⚠ **This is the FIXED-parameter spelling, and it is no longer the only one.** It delegates to
/// [`run_walkforward_slice_searching`] with [`WindowSearchPlan::none`] — the control — so the two
/// walks are one driver rather than two programs, and the cost knobs still apply the same way. The
/// compute-to-data `RunWalkforward` verb reaches the SEARCHING entry directly
/// (`crate::wire_run`'s `run_walkforward_local`), because it also needs the realised fill mix that
/// entry returns.
///
/// The `Send + Sync` bound on the factory arrived with that delegation: the searching arm scores
/// candidates on the harness's bounded parallel pool, which requires it. `EngineParams::default`
/// and every wire-params closure satisfy it.
///
/// # The annualization comes from the SLICE, not from a literal
///
/// `WalkForwardReport::oos_sharpe` is scaled by `sqrt(periods_per_year)`, and that factor has to be
/// the number of RETURN OBSERVATIONS a year of THESE bars produces — one per bar. It is
/// [`periods_per_year_for_interval`] over `slice.interval`: the ONE home for the derivation, and
/// the same function `vike_backtest::harness::report::periods_per_year` wraps for the profile
/// plane. So the CLI/MCP door and the Studio door cannot report a different Sharpe for the same
/// strategy over the same bars.
///
/// This call used to pass a bare `252.0` while the harness derived its factor — and the harness's
/// own doc asserted the two planes "made the same two derivations", with no test comparing them.
/// They disagreed by `sqrt(24) ≈ 4.9x` on 1h bars and by `sqrt(1440) ≈ 37.9x` on 1m. A `"1d"`
/// slice is bit-identical to what it always reported ([`DAILY_PERIODS_PER_YEAR`] is preserved
/// exactly), so only intraday and longer-than-daily slices move.
///
/// The TICK branch the harness wrapper carries has no twin here, deliberately: [`load_slice_bars`]
/// runs first and refuses a [`SliceKind::Ticks`] slice as [`RunError::Data`], so the derivation is
/// reachable only on the bar path. A `kind`-check beside it would be unreachable code wearing the
/// shape of a safety net.
///
/// # `n_splits` has a floor, because the failure below it is SILENT
///
/// `vike_backtest::validation::walk_forward_splits` loops `1..=n_splits`, so `0` produces no
/// splits at all: `walk_forward_strategy` never enters its window loop, the stitched curve stays
/// empty, `equity` never leaves `cash`, and `wf_consistency` divides by `windows.len().max(1)`.
/// The report comes back all zeros — `oos_return` 0, `oos_sharpe` 0, `wf_consistency` 0, no
/// windows — which is a run that NEVER HAPPENED wearing the shape of a strategy that made nothing,
/// and nothing anywhere returns an error. So it is refused here as a [`RunError::Data`].
///
/// This plane checked it at no level while `vike_backtest::harness::run_walkforward` checks it at
/// two. The GUI was shielded only by `crates/vike-studio/src/studio.rs`'s `start_walkforward`
/// passing a hardcoded literal; the compute-to-data `RunWalkforward` verb
/// (`crate::wire_run`'s `run_walkforward_local`) takes `n_splits` from its caller and applies
/// no floor of its own, so the hole was wire-reachable.
pub fn run_walkforward_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<WalkForwardReport, RunError> {
    run_walkforward_slice_searching(
        spec,
        slice,
        store,
        n_splits,
        WindowSearchPlan::none(),
        make_params,
    )
    .map(|(report, _mix)| report)
}

/// WHICH search a window runs on its own training half, and under what ranking — the Studio door's
/// spelling of the knobs `[walkforward].search` / `[walkforward].rank_by` carry on the profile door.
///
/// ⚠ **[`WindowSearch::None`] is a VALUE here, not an absence**, and that is `0063`'s commitment
/// rather than a style choice: the profile door already treats the no-search walk as a first-class
/// mode on measured grounds (across seven selection criteria, "no search at all" landed inside one
/// seed's noise band at a fraction of the cost), and an optimizer whose null control is unreachable
/// cannot be evaluated. [`WindowSearchPlan::none`] is that control, and it reaches the SAME driver
/// the searching plan does — which is what makes the two comparable by construction instead of by
/// review.
#[derive(Debug, Clone, Copy)]
pub struct WindowSearchPlan<'a> {
    /// The search mode. [`WindowSearch::None`] walks the spec's own parameters.
    pub search: WindowSearch,
    /// The `(name, values)` axes each window scores on its TRAINING half — the same grid shape
    /// [`run_paramscan_slice_with_params`] takes. Ignored under [`WindowSearch::None`].
    pub grid: &'a [(String, Vec<f64>)],
    /// Which metric picks each window's winner. Ignored under [`WindowSearch::None`].
    pub rank: RankMetric,
}

impl<'a> WindowSearchPlan<'a> {
    /// **The CONTROL** — every window walks the spec's own parameters, and the report is the
    /// fixed-parameter walk's. See this type's own doc for why it is a value rather than an absent
    /// argument.
    pub fn none() -> Self {
        WindowSearchPlan { search: WindowSearch::None, grid: &[], rank: RankMetric::default() }
    }

    /// A SEARCHING plan: score `grid` on each window's training half and rank by `rank`.
    pub fn sweep(grid: &'a [(String, Vec<f64>)], rank: RankMetric) -> Self {
        WindowSearchPlan { search: WindowSearch::Sweep, grid, rank }
    }
}

/// **The ONE walk-forward driver this crate has**, and the entry both the control and the searching
/// arm reach — `0063`'s "the no-search control stays reachable" is a property of this signature
/// rather than of a convention.
///
/// Returns the stitched report AND the [`FillMix`] its OUT-OF-SAMPLE windows realised, because a
/// cost-model stamp cannot be read off an equity curve and the run that charged the fees is the
/// only thing that knows. The mix folds the VALIDATION runs only: a search's training scores are in
/// no reported number, so counting their fills would describe work the report does not contain.
///
/// # What the searching arm does, per window
///
/// Expand `plan.grid`, score every point on the window's TRAINING half through
/// `plan.rank`'s objective, take the best-scoring point — and only it — onto the window's
/// validation half, and record what was chosen in `WfWindow::chosen_params`. That is the protocol
/// "walk-forward" usually names, and it asks a different question from the control's: the control
/// asks whether FIXED parameters were stable out of sample, this asks whether the PROCEDURE of
/// fit-then-trade survives out of sample. Both stitch through the same runner and return the same
/// report type, which is the entire point of routing the control through here.
///
/// ⚠ **A `Sweep` plan with an EMPTY grid is REFUSED, not silently demoted.** `cartesian(&[])`
/// answers ONE point carrying no overrides, so an empty grid would make the search "no search"
/// while every window still recorded a choice and the report still said it had optimized — the
/// exact refusal `vike_backtest::harness::walkforward::run_walkforward_optimized_with` states for a
/// profile naming `search = "sweep"` with no `[sweep]` table.
///
/// ⚠ **A window in which every candidate FAILED is an error, not an empty choice.** The window
/// closure cannot return `Err` (the runner's signature owns it), so the failure is captured and
/// raised after the walk — the harness optimizer's own shape.
///
/// The annualization, the single-symbol rule and the `n_splits >= 1` floor are unchanged and are
/// argued on [`run_walkforward_slice_with_params`].
pub fn run_walkforward_slice_searching(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    n_splits: usize,
    plan: WindowSearchPlan<'_>,
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<(WalkForwardReport, FillMix), RunError> {
    // pre-resolve so a bad script/name fails as Compile/Strategy before the loop (the closure's
    // own build can't return an error).
    build_strategy(spec)?;
    if slice.symbols.len() > 1 {
        return Err(RunError::Data(
            "walk-forward runs over ONE bar series — pick a single symbol".into(),
        ));
    }
    // Zero windows is an all-zeros REPORT, not an error, anywhere below this line — see the
    // `n_splits` section of this function's doc for the mechanism. Refused before the store read,
    // because a run that cannot produce a window should not cost a bar load either.
    if n_splits == 0 {
        return Err(RunError::Data(
            "walk-forward needs at least ONE out-of-sample window — n_splits must be >= 1".into(),
        ));
    }
    // Refused BEFORE the store read, like the `n_splits` floor above and for the same reason: a
    // request that cannot honour what it asked for should not cost a bar load either.
    if plan.search.searches() && plan.grid.is_empty() {
        return Err(RunError::Data(
            "a searching walk-forward needs a parameter GRID — with no axes the search expands to \
             ONE point carrying no overrides, which is the fixed walk wearing a report that says \
             it optimized. Send at least one axis, or ask for search = \"none\", the control."
                .into(),
        ));
    }
    let series = load_slice_bars(slice, store)?;
    let (sym, bars) = series.into_iter().next().expect("load_slice_bars rejects an empty list");
    let cash = make_params().cash;
    // The annualization is DERIVED from this slice's own bar step (see the doc above). `slice` is
    // known to be `SliceKind::Bars` by here — `load_slice_bars` refuses anything else — so
    // `interval` is a real bar step rather than the empty string a tick slice carries.
    //
    // `train` is used only by the SEARCHING arm; under the control it is ignored deliberately —
    // that walk is an out-of-sample stability check rather than an optimize-then-test protocol.
    //
    // ⚠ Both halves above arrived from DIFFERENT branches and this call is where they met. Taking
    // either side alone was wrong in a way only the merged tree shows: the branch's side still
    // carried the bare `252.0`, so merging it would have SILENTLY REVERTED the annualization fix
    // that shipped separately.
    let periods_per_year = periods_per_year_for_interval(&slice.interval);
    let objective = plan.rank.objective();
    // Out-of-band failure propagation: the window closure owns no `Err` channel, so a window whose
    // whole candidate set failed is recorded here and raised after the walk (the harness
    // optimizer's own shape).
    let mut failed_window: Option<String> = None;
    let mut mix = FillMix::default();
    let rep = walk_forward_strategy(
        &bars,
        n_splits,
        WalkMode::Anchored,
        cash,
        periods_per_year,
        |train, window| {
            let chosen = match plan.search {
                WindowSearch::None => None,
                WindowSearch::Sweep => {
                    let winner = best_point_on_train(
                        spec,
                        &sym,
                        train,
                        plan.grid,
                        &objective,
                        periods_per_year,
                        &make_params,
                    );
                    if winner.is_none() {
                        // Recorded, not raised: the runner owns this closure's signature and it
                        // has no `Err` channel. The walk still produces an outcome for this
                        // window; the whole report is discarded when the error is raised below.
                        failed_window.get_or_insert_with(|| {
                            format!(
                                "every candidate failed to run on the training half of the \
                                 out-of-sample window [{}, {}) — a walk cannot report a winner it \
                                 never found",
                                window.first().map_or(0, |b| b.ts),
                                window.last().map_or(0, |b| b.ts),
                            )
                        });
                    }
                    winner
                }
            };
            let strat = match &chosen {
                None => build_strategy(spec).expect("pre-checked above"),
                Some(overrides) => build_strategy_with(spec, overrides)
                    .expect("a winner is a candidate that already built once, on the train half"),
            };
            let result =
                StrategyEngine::new(vec![(sym.clone(), window.to_vec())], strat, make_params())
                    .run();
            // The realised mix describes the VALIDATION runs, which are the only ones any reported
            // number comes from.
            mix.add(&result);
            WindowOutcome {
                result,
                // Rendered onto the wire as TOML text by `wire_run`'s `to_wire_wf_window`; a Studio
                // axis is an `f64`, so `Float` is the whole of the mapping.
                chosen_params: chosen.map(|overrides| {
                    overrides
                        .into_iter()
                        .map(|(k, v)| (k, toml::Value::Float(v)))
                        .collect::<Vec<_>>()
                }),
            }
        },
    );
    if let Some(why) = failed_window {
        return Err(RunError::Data(why));
    }
    Ok((rep, mix))
}

/// One SCORED candidate: its objective value, and the parameter overrides that produced it.
///
/// Named rather than spelled inline because [`best_point_on_train`] holds it twice — once wrapped
/// in the `Option` a skipped candidate leaves behind — and the nested spelling is what
/// `clippy::type_complexity` objects to. The pair is deliberately score-FIRST, so
/// [`cmp_scores_desc`] reads off `.0` the way the harness's own ranking does.
type ScoredPoint = (f64, Vec<(String, f64)>);

/// Score every grid point on one window's TRAINING half and return the winner's overrides, or
/// `None` when every candidate failed to build or run.
///
/// Ranked through the harness's ONE comparator ([`cmp_scores_desc`]) so a NaN objective — a
/// degenerate candidate, e.g. a Sharpe over a zero-variance curve — sorts LAST and can never win.
/// `sort_by` is stable and [`map_bounded`] reassembles by index, so equal-scoring candidates keep
/// grid order and the winner does not depend on scheduling.
///
/// Parallel on the harness's BOUNDED pool, the same one [`run_paramscan_slice_with_params`] enters:
/// each candidate clones the training series, so concurrency is an `N_threads x window` memory
/// multiplier on boxes that may also be hosting live trading.
fn best_point_on_train(
    spec: &StrategySpec,
    sym: &str,
    train: &[Bar],
    grid: &[(String, Vec<f64>)],
    objective: &vike_backtest::objective::Objective,
    periods_per_year: f64,
    make_params: &(impl Fn() -> EngineParams + Send + Sync),
) -> Option<Vec<(String, f64)>> {
    let points = cartesian(grid);
    let scored: Vec<Option<ScoredPoint>> = map_bounded(points, |overrides| {
        let strat = build_strategy_with(spec, &overrides).ok()?; // skip a failing candidate
        let result =
            StrategyEngine::new(vec![(sym.to_string(), train.to_vec())], strat, make_params())
                .run();
        let report = BacktestReport::from_result(None, &result, periods_per_year);
        Some((objective(&report), overrides))
    });
    let mut scored: Vec<ScoredPoint> = scored.into_iter().flatten().collect();
    if scored.is_empty() {
        return None;
    }
    scored.sort_by(|a, b| cmp_scores_desc(a.0, b.0));
    Some(scored.swap_remove(0).1)
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
pub struct ParamscanEntry {
    pub overrides: Vec<(String, f64)>,
    pub result: BacktestResult,
}

/// A ranked parameter sweep + its deflated Sharpe and PBO across the trial set.
#[derive(Debug, Clone)]
pub struct StudioParamscan {
    pub entries: Vec<ParamscanEntry>, // ranked best-first by annualized Sharpe
    pub dsr: f64,
    /// Probability of backtest overfitting (CSCV) across the trials; `NaN` = not assessed
    /// (< 2 trials, too-short slice, or non-finite returns).
    pub pbo: f64,
    pub best_index: usize, // always 0 (entries are pre-ranked) — kept explicit for the UI
}

/// CSCV split count for PBO — even (López de Prado canonical S = 16; `C(16,8) = 12_870` combos,
/// computed off the UI thread inside `run_paramscan_slice`). Guarded by `T >= N_SPLITS`.
const N_SPLITS: usize = 16;

/// The N per-trial per-observation return columns, each truncated to the common length T (the min
/// column length — all trials run over identical bars so lengths are equal in practice; the
/// truncation is a defensive guard against a jagged matrix). Column-major — exactly what
/// `deflated_sharpe_with_effective_n` wants; PBO uses [`transpose`].
fn trial_columns(entries: &[ParamscanEntry]) -> (Vec<Vec<f64>>, usize) {
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
///
/// ⚠ **The annualization factor here is ORDERING-INERT, and that is why it stays a constant while
/// [`run_walkforward_slice_with_params`] derives its own from the slice.** `metrics::sharpe`
/// multiplies by `sqrt(periods_per_year)`, so any positive factor is one uniform positive scale
/// applied to EVERY entry: it cannot reorder a comparison, and a NaN stays NaN under it. Nothing
/// downstream reads this number either — the DSR and PBO inputs are
/// `overfit::sharpe_moments(..).sr_per_obs`, which is PER-OBSERVATION and annualization-independent
/// by construction (see the comment beside the DSR block in [`run_paramscan_slice_with_params`]).
/// [`DAILY_PERIODS_PER_YEAR`] is therefore the honest spelling of "the scale does not matter here":
/// deriving it per slice would change no rank, no `best_index`, no `dsr` and no `pbo`, so a future
/// reader who "fixes" it into a derived value will have changed exactly nothing.
/// `ranking_order_is_invariant_to_the_annualization_factor` below is that claim's test.
fn rank_entries_by_sharpe(entries: &mut [ParamscanEntry]) {
    entries.sort_by(|a, b| {
        cmp_scores_desc(
            sharpe(&a.result.equity_curve, DAILY_PERIODS_PER_YEAR),
            sharpe(&b.result.equity_curve, DAILY_PERIODS_PER_YEAR),
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

// THE SWEEP-THREADS FORWARDER IS DELETED, and what it forwarded is not.
//
// It re-exposed `vike_backtest::harness::install_sweep_threads` for exactly one caller:
// `vike-desktop`, which links `vike-studio` (which re-exported this in turn) and does NOT
// link `vike-backtest`, so without the seam the GUI could not hand its resolved
// `preferences.sweep_threads` to the pool its in-process `Backend::Local` sweep entered.
//
// That backend is gone. A sweep now runs in the compute daemon, which IS vike-backtest and
// calls `crate::harness::install_sweep_threads` directly, so the forwarder had no caller left
// and a re-export whose consumer is gone is a second name that rots. `run_paramscan_slice`
// below is untouched and still enters the same bounded pool - it is reached from
// `crate::wire_run` on the daemon side now instead of from a GUI thread here.

/// Sweep the strategy over the grid: build-per-point (with that point's overrides), backtest each,
/// rank by annualized Sharpe (NaN Sharpe LAST — see [`rank_entries_by_sharpe`]), and compute the
/// deflated Sharpe across the trial Sharpes. A point that fails to build is skipped (resilient,
/// like the harness `run_paramscan`); all-failing -> error.
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
pub fn run_paramscan_slice(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<f64>)],
) -> Result<StudioParamscan, RunError> {
    run_paramscan_slice_with_params(spec, slice, store, grid, EngineParams::default)
}

/// [`run_paramscan_slice`] with CALLER-SUPPLIED engine cost/cash: `make_params` builds a FRESH
/// [`EngineParams`] for EACH grid point, so every point runs under the SAME cost knobs (cash /
/// fee_rate / slippage). A per-point factory rather than one shared value because `EngineParams` is
/// not `Clone` (it can carry a `Box<dyn PositionSizer>`) AND each point runs on the bounded parallel
/// pool — hence the `Send + Sync` bound on the factory. The default-params [`run_paramscan_slice`]
/// delegates here with `EngineParams::default`. This is the seam the compute-to-data `RunSweep` verb
/// reaches so a profile's `[engine]` costs apply server-side, mirroring how [`run_slice`] already
/// takes an `EngineParams`.
pub fn run_paramscan_slice_with_params(
    spec: &StrategySpec,
    slice: &DataSlice,
    store: &StoreHandle,
    grid: &[(String, Vec<f64>)],
    make_params: impl Fn() -> EngineParams + Send + Sync,
) -> Result<StudioParamscan, RunError> {
    // Bar slices load ONCE and every point clones the series (the load is the expensive part and it
    // is identical across points). Tick slices have no such hoist: `replay_ticks` owns its own
    // scan, so each point re-reads the store — hence the bounded pool above.
    let bars = match slice.kind {
        SliceKind::Bars => Some(load_slice_bars(slice, store)?),
        SliceKind::Ticks => None,
    };
    let points = cartesian(grid);
    let rows: Vec<Option<ParamscanEntry>> = map_bounded(points, |overrides| {
        let strat = build_strategy_with(spec, &overrides).ok()?; // skip a failing point
        let result = match &bars {
            Some(series) => StrategyEngine::new(series.clone(), strat, make_params()).run(),
            None => run_tick_slice(strat, slice, store, make_params()).ok()?,
        };
        Some(ParamscanEntry { overrides, result })
    });
    let mut entries: Vec<ParamscanEntry> = rows.into_iter().flatten().collect();
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
    Ok(StudioParamscan { entries, dsr, pbo, best_index: 0 })
}

/// Worker-thread twin of [`run_paramscan_slice`].
pub fn spawn_paramscan(
    spec: StrategySpec,
    slice: DataSlice,
    store: StoreHandle,
    grid: Vec<(String, Vec<f64>)>,
) -> Receiver<Result<StudioParamscan, RunError>> {
    spawn_outcome(move || run_paramscan_slice(&spec, &slice, &store, &grid))
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
/// Saved pane's "Compare all" twin of `spawn_run`/`spawn_paramscan`/`spawn_walkforward`.
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
    use vike_model::BookLevel;
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

    /// **Fix-round-1 CRITICAL, pinned.** An absent sha must never resolve as if it named a real
    /// artifact — this is the permanent guard every caller (Run/Sweep/Walk-Forward/Compare-all)
    /// relies on regardless of whether it remembered to ask `run_blocked_reason` first. Both an
    /// empty string (the studio-side `unwrap_or_default()` shape) and a whitespace-only one are
    /// refused, and the refusal names the EMPTINESS rather than falling through to whatever the
    /// loader would have said about `<name>-.so` — the two must stay DISTINGUISHABLE so this
    /// guard cannot be deleted by mistake as "redundant" now that the loader HAS landed. (This
    /// said "the generic 'loader not wired' message … once the loader lands"; that message is
    /// gone and its arm is the real `dlopen` now. The requirement is unchanged and is why the
    /// sibling test below pins the OTHER refusal by its own words.)
    #[test]
    fn a_plugin_spec_with_no_sha_is_refused_and_never_reaches_the_loader_arm() {
        let empty = StrategySpec::plugin("my_strat", "", crate::spec::empty_params());
        // `Box<dyn Strategy<SimBroker>>` is not `Debug`, so the Ok side cannot ride `expect_err`/
        // `{:?}` on the whole `Result` — match it out explicitly instead.
        match build_strategy(&empty) {
            Err(RunError::Strategy(msg)) => {
                assert!(msg.contains("no sha"), "{msg}");
                assert!(msg.contains("names no artifact"), "{msg}");
            }
            Ok(_) => panic!("an empty sha must be refused"),
            Err(other) => panic!("wrong RunError variant: {other:?}"),
        }

        let whitespace = StrategySpec::plugin("my_strat", "   ", crate::spec::empty_params());
        assert!(build_strategy(&whitespace).is_err(), "whitespace-only is not a sha either");

        // The overrides door delegates to `build_strategy`, so the same refusal is reachable
        // through it too — a sweep point can never smuggle an empty sha past this guard.
        let via_overrides = build_strategy_with(&empty, &[("fast".to_string(), 5.0)]);
        assert!(via_overrides.is_err(), "the overrides path must inherit the same refusal");
    }

    /// The complement: a NON-empty sha reaches the LOADER arm, and with no artifact on disk the
    /// loader's own `Missing` refusal is what comes back — naming the path and telling the author
    /// to build it. That proves the two refusals are genuinely distinct rather than one silently
    /// swallowing the other.
    ///
    /// ⚠ This test used to assert the message said "not wired". It is the one assertion the join
    /// was supposed to invalidate, and it now pins the OPPOSITE property: reaching the loader is
    /// the success condition, and "no artifact at this path" is the loader speaking rather than
    /// this function guessing. A missing artifact must NEVER be a silent fall back to some other
    /// strategy — the design's own list of red tests says so in as many words.
    #[test]
    fn a_plugin_spec_with_a_real_looking_sha_reaches_the_loader_and_reports_a_missing_artifact() {
        let dir = tempfile::tempdir().expect("scratch dir");
        let spec = StrategySpec::plugin("my_strat", "a".repeat(64), crate::spec::empty_params());
        match build_strategy_in(&spec, dir.path()) {
            Err(RunError::Strategy(msg)) => {
                assert!(msg.contains("my_strat"), "the refusal must name the artifact: {msg}");
                assert!(msg.to_lowercase().contains("build"), "{msg}");
                assert!(
                    !msg.contains("no sha"),
                    "a real sha must not trip the emptiness refusal: {msg}"
                );
            }
            Ok(_) => panic!("no artifact exists at that path, so nothing can load"),
            Err(other) => panic!("wrong RunError variant: {other:?}"),
        }
    }

    /// [`plugins_dir`] resolves the PROJECT's `user_data/plugins`, which is where the builder
    /// service's artifacts land and therefore the only place the host may look.
    ///
    /// ⚠ **The leaf assertion alone could not fail for its stated reason**, and that is what this
    /// test used to be: both branches of [`plugins_dir`] end in `user_data/plugins`, so a WALK
    /// that resolved nothing — the fallback, a bare relative path — passed it identically to a
    /// walk that found the project. The distinguishing property is ABSOLUTENESS plus the PROJECT
    /// ROOT: only the resolved branch answers an absolute path under the project this test runs
    /// in. Both are asserted, and the sibling below drives the FALLBACK branch through the same
    /// function so the two are shown to differ rather than assumed to.
    #[test]
    fn the_plugin_lookup_directory_is_the_resolved_projects_user_data_plugins() {
        let dir = plugins_dir();
        let rendered = dir.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with("user_data/plugins"), "{rendered}");
        // The resolved branch, distinguished from the fallback: an ABSOLUTE path whose parent
        // chain is the project this test runs in. A `cargo test -p` runs with the CWD at the
        // crate directory, so a walk that found nothing would answer the bare relative literal.
        assert!(
            dir.is_absolute(),
            "the walk must have RESOLVED a project, not fallen through to the relative default: \
             {rendered}"
        );
        let cwd = std::env::current_dir().expect("cwd");
        assert!(
            cwd.starts_with(dir.parent().and_then(std::path::Path::parent).expect("<project>")),
            "the resolved project must be an ANCESTOR of the working directory: {rendered} vs {}",
            cwd.display()
        );
    }

    /// ...and BOTH branches of [`plugins_dir_from`], driven through the production function.
    ///
    /// ⚠ **The fallback assertion used to sit behind an `if let Some(..)`**, so on any box where
    /// the resolver answered — which is every box — it asserted nothing at all, and the sibling
    /// above claimed the fallback was "exercised separately below" when it was exercised by
    /// nothing. It cannot be made unconditional through the filesystem: the walk climbs to `/`
    /// and no test can promise that no ancestor of a temp directory carries a project marker.
    /// `None` is the one input that reaches the fallback on every box, which is why
    /// [`plugins_dir_from`] takes the working directory as a parameter.
    #[test]
    fn both_branches_of_the_plugin_directory_resolution_are_reachable_and_differ() {
        // THE FALLBACK, unconditionally: no working directory, so nothing to walk from.
        let fallback = plugins_dir_from(None);
        assert_eq!(fallback, PathBuf::from("user_data").join("plugins"));
        assert!(!fallback.is_absolute(), "the fallback is the bare relative default: {fallback:?}");

        // THE RESOLVED BRANCH, over a planted project — an ABSOLUTE answer under that project.
        let tmp = tempfile::tempdir().expect("scratch");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(a.join("settings")).expect("plant marker a");
        std::fs::create_dir_all(b.join("settings")).expect("plant marker b");
        let ra = plugins_dir_from(Some(&a));
        let rb = plugins_dir_from(Some(&b));
        assert_eq!(ra, a.join("user_data").join("plugins"));
        assert!(ra.is_absolute());

        // The two branches genuinely differ, and the resolver is a FUNCTION OF ITS START PATH:
        // two projects cannot answer one directory, which is what stops a run loading a plugin
        // from one project while writing its run into another.
        assert_ne!(ra, fallback, "the two branches must not produce the same answer");
        assert_ne!(ra, rb, "two projects must not share one plugin directory");
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
        let sw = run_paramscan_slice(&spec, &slice(), &store, &grid).unwrap();
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
            bids: vec![BookLevel::new(100.0, 1.0)],
            asks: vec![BookLevel::new(100.5, 1.0)],
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

    /// THE equality pin of the one-call fold: [`series_lists`] answers exactly what the three
    /// separate enumerations answer, over a store holding all three shapes at once (a bar series,
    /// a replayable tick lane, and a depth-only instrument). Without a fixture carrying all three
    /// the assertion would pass on empty vectors and prove nothing.
    #[test]
    fn series_lists_equals_the_three_separate_enumerations() {
        let dir = tempfile::tempdir().unwrap();
        let store = DataFusionHist::open(dir.path()).unwrap();
        store.append_bars("binance", "BTCUSDT", "1m", &[bar_at(0), bar_at(60_000)], None).unwrap();
        let t = vec![TradeTick {
            ts: 1,
            local_ts: 1,
            price: 100.25,
            size: 1.0,
            is_buyer_maker: false,
            symbol: "ETHUSDT".into(),
        }];
        store.append_trades("binance", "ETHUSDT", &t, None).unwrap();
        store.append_depth("okx", "SOL-USDT", &[depth_snapshot(1, "SOL-USDT")], None).unwrap();

        let one = series_lists(&store).unwrap();
        assert_eq!(one.bars, bar_series(&store).unwrap());
        assert_eq!(one.ticks, tick_series(&store).unwrap());
        assert_eq!(one.depth_only, depth_only_series(&store).unwrap());
        // ...and the fixture actually exercises all three lists, so the equality above is not
        // three comparisons of empty vectors.
        assert_eq!(one.bars.len(), 1);
        assert_eq!(one.ticks, vec![("binance".to_string(), "ETHUSDT".to_string())]);
        assert_eq!(one.depth_only, vec![("okx".to_string(), "SOL-USDT".to_string())]);
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

    /// **The annualization bug.** `oos_sharpe` is scaled by `sqrt(periods_per_year)`, and that
    /// factor must be the observation count the SLICE's own bar step produces — 252 · 1440 for the
    /// 1m fixture — not the daily 252 this plane hardcoded while the harness plane derived its
    /// own. The two doors disagreed by `sqrt(1440)` on exactly this interval, which is the one the
    /// CI roundtrip fixture uses, and no test compared them.
    #[test]
    fn walkforward_annualizes_off_the_slice_interval_not_a_bare_252() {
        let (_dir, store, _b) = seeded_store();
        assert_eq!(slice().interval, "1m", "the fixture interval is what makes 1440 the ratio");
        let rep = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 4).unwrap();

        // `oos_sharpe` IS `sharpe(oos_equity_curve, factor)` — both come out of the same stitch —
        // so recomputing the curve at each candidate factor says which one the run actually used,
        // bit for bit, with no tolerance to argue about.
        let at_slice = sharpe(&rep.oos_equity_curve, periods_per_year_for_interval("1m"));
        let at_daily = sharpe(&rep.oos_equity_curve, DAILY_PERIODS_PER_YEAR);
        assert_eq!(
            rep.oos_sharpe.to_bits(),
            at_slice.to_bits(),
            "a 1m slice must annualize at 252 * 1440, i.e. one observation per BAR"
        );
        assert!(
            at_daily != 0.0,
            "precondition: a flat OOS curve would make every comparison below vacuous"
        );
        assert_ne!(rep.oos_sharpe.to_bits(), at_daily.to_bits(), "...and NOT at the daily anchor");

        // The SIZE of the correction, pinned because it is what a reader needs in order to reason
        // about an `oos_sharpe` printed by this door BEFORE the fix.
        let ratio = rep.oos_sharpe / at_daily;
        assert!((ratio - 1440.0_f64.sqrt()).abs() < 1e-9, "expected ~37.9x, got {ratio}");
    }

    /// **The `n_splits` floor.** Zero windows is not an empty result, it is a run that never
    /// happened wearing the shape of a strategy that made nothing — so it is refused rather than
    /// reported. The second half drives `walk_forward_strategy` at zero directly, because a guard
    /// is only worth having if the thing it prevents is real.
    #[test]
    fn zero_splits_is_refused_rather_than_reported_as_an_all_zeros_run() {
        let (_dir, store, bars) = seeded_store();
        let err = run_walkforward_slice(&rhai(SCRIPT), &slice(), &store, 0)
            .expect_err("n_splits = 0 must not produce a report");
        let RunError::Data(msg) = &err else { panic!("expected a Data refusal, got {err:?}") };
        assert!(msg.contains("n_splits"), "the refusal must name the parameter, got {msg:?}");

        // What the guard prevents. Both scalars are arbitrary here: with no splits the window
        // closure is never called, nothing is stitched, and `sharpe` of an empty curve is 0 at any
        // annualization.
        // ⚠ `|_, _|`, not `|_|`. This test arrived on one branch while the runner's closure grew a
        // second parameter (the TRAIN half) on another, and the mismatch was invisible to both:
        // each side compiled alone, and only the merged tree names it.
        let unguarded =
            walk_forward_strategy(&bars, 0, WalkMode::Anchored, 1_000.0, 1.0, |_, _| {
                unreachable!("zero splits means the window closure is never called")
            });
        assert!(unguarded.windows.is_empty(), "no windows");
        assert!(unguarded.oos_equity_curve.is_empty(), "nothing stitched");
        assert_eq!(unguarded.oos_return, 0.0, "reads as a strategy that made nothing");
        assert_eq!(unguarded.oos_sharpe, 0.0, "...at a Sharpe of zero");
        assert_eq!(unguarded.wf_consistency, 0.0, "...over `windows.len().max(1)` windows");
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
        let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
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
        let a = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        let b = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
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

    fn entry(curve: Vec<f64>) -> ParamscanEntry {
        let last = *curve.last().unwrap();
        ParamscanEntry {
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

    /// [`rank_entries_by_sharpe`]'s annualization factor is ORDERING-INERT, which is the whole
    /// reason it stays [`DAILY_PERIODS_PER_YEAR`] while [`run_walkforward_slice_with_params`]
    /// derives its own from the slice. `metrics::sharpe` multiplies by `sqrt(periods_per_year)` —
    /// ONE uniform positive scale applied to every entry — so no factor can reorder the ranking,
    /// and a NaN score stays NaN under all of them. Gated here so that doc comment is not a claim
    /// nothing checks, and so a later reader cannot derive the value and believe something moved.
    #[test]
    fn ranking_order_is_invariant_to_the_annualization_factor() {
        // `entry` stamps `final_equity` from the curve's last point, and these four differ, so the
        // equity bits identify WHICH entry landed at each rank.
        let curves = [
            vec![1000.0, 1010.0, 1005.0, 1030.0],
            vec![1000.0, 990.0, 1002.0, 995.0],
            vec![1000.0, f64::NAN, 1000.0, 1000.0],
            vec![1000.0, 1001.0, 1002.0, 1004.0],
        ];
        let order_at = |ppy: f64| -> Vec<u64> {
            let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
            entries.sort_by(|a, b| {
                cmp_scores_desc(
                    sharpe(&a.result.equity_curve, ppy),
                    sharpe(&b.result.equity_curve, ppy),
                )
            });
            entries.iter().map(|e| e.result.final_equity.to_bits()).collect()
        };

        let daily = order_at(DAILY_PERIODS_PER_YEAR);
        for interval in ["1m", "5m", "1h", "1d", "7d"] {
            assert_eq!(
                order_at(periods_per_year_for_interval(interval)),
                daily,
                "the {interval} factor must not reorder a sweep"
            );
        }

        // ...and the SHIPPED ranker produces exactly that order, with the NaN point still last.
        let mut entries: Vec<ParamscanEntry> = curves.iter().cloned().map(entry).collect();
        rank_entries_by_sharpe(&mut entries);
        let ranked: Vec<u64> = entries.iter().map(|e| e.result.final_equity.to_bits()).collect();
        assert_eq!(ranked, daily, "the shipped ranker agrees with the scale-free order");
        let last = &entries.last().expect("four entries").result.equity_curve;
        assert!(sharpe(last, DAILY_PERIODS_PER_YEAR).is_nan(), "the NaN point still sorts last");
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
        let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
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
        let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();
        assert!(sw.pbo.is_nan(), "single trial -> PBO not assessable, got {}", sw.pbo);
    }

    #[test]
    fn sweep_dsr_is_effective_n_deflated() {
        let (_dir, store, _b) = seeded_store();
        // closely-spaced fast values -> strongly correlated equity curves -> effective_n < N,
        // so the effective-N DSR differs from the plain deflated_sharpe_ratio on the same inputs.
        let grid = vec![("fast".to_string(), vec![3.0, 4.0, 5.0, 6.0])];
        let sw = run_paramscan_slice(&rhai(SWEEP_SCRIPT), &slice(), &store, &grid).unwrap();

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
