//! The loopback backtesthub this crate's tests (`backtest_plane.rs`, `backtesthub_profile_roundtrip.rs`),
//! vike-cli's backtest/search suites and vike-studio-core's wire-parity suite each spawned by hand until
//! 2026-10-31, and the research bins' `--json` provenance pin, compiled once here behind `test-support`
//! (only DEV edges turn it on: this crate's own, vike-cli's, vike-studio-core's, vike-backend's). Beside them, a store
//! that stamps its bars' days ([`stamping`]), so a sandboxed run's pin check is driven both ways.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vike_backtest::run::strategy_builder::StandIn;
use vike_data::{HistoryStore, HistoryStoreMemory};

use crate::backtesthub_server::{StudyDirs, serve_authed};
use crate::named_run::NamedRunLane;
use crate::studio_run::StrategyFiles;

/// Bind an ephemeral loopback listener, spawn the REAL backtesthub (`serve_authed`, key-less, the named
/// run disarmed) over a fresh in-memory store on a detached thread, and return the assigned address.
/// Every backtesthub serves the Studio runners and the study runner too, so this one serves all of them.
///
/// ⚠ Its `.rs` strategy files are built by `stand_in` (`StrategyFiles::stand_in`; every caller hands
/// `vike_strategy_lab::stand_in`, which this library cannot name), where a shipped daemon hands
/// them to a sandboxed child: these suites test the wire and the harness, not the sandbox.
pub fn spawn_server(stand_in: StandIn) -> SocketAddr {
    spawn_server_over(Arc::new(HistoryStoreMemory::new()), stand_in)
}

/// [`spawn_server`] over the caller's `store`, which is MOVED into the serve thread (a parity test keeps
/// its own clone for the local run it compares against).
pub fn spawn_server_over(
    store: Arc<dyn HistoryStore + Send + Sync>,
    stand_in: StandIn,
) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-server", move |listener| {
        let _ = serve_with_stand_ins(listener, store, stand_in);
    })
}

/// [`spawn_server_over`] whose runs root is `runs_root`: what a test of a SAVED run (a combinatorial
/// walk-forward or a permutation test, `crate::backtesthub_server`'s `saved_run`) spawns, so the run
/// lands in the test's own temp dir and never under the working directory.
pub fn spawn_server_over_saving_to(
    store: Arc<dyn HistoryStore + Send + Sync>,
    stand_in: StandIn,
    runs_root: PathBuf,
) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-server", move |listener| {
        let study = StudyDirs {
            runs_root,
            strategies: StrategyFiles::stand_in(stand_in),
            save_backtest_runs: true,
            ..StudyDirs::working_directory()
        };
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    })
}

/// A REAL hub over `store`: its `.rs` strategy files, read under `strategies`, run in a sandboxed child
/// (`StrategyFiles::hub`), as a shipped daemon's do. The child program is
/// `crate::strategy_sandbox::use_program`'s.
pub fn spawn_hub_over(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
) -> SocketAddr {
    spawn_hub(store, strategies, None)
}

/// [`spawn_hub_over`] that SAVES what it answers under `runs_root`, as a serving daemon does
/// (`crate::backtesthub_server`'s `saved_run`): a saved run of a `.rs` file proven through its real sandboxed child.
pub fn spawn_hub_over_saving_to(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
    runs_root: PathBuf,
) -> SocketAddr {
    spawn_hub(store, strategies, Some(runs_root))
}

/// **A hub built as the `--addr` daemon builds itself**, over `store`: its study directories from
/// `crate::backtest_cli::serve::serving_study` (this process's working directory and `vars`, so the project the test
/// entered; `keep_per_strategy` the `config.saved_runs_per_strategy` row), `build` its commit, and what it answers an
/// identical request from its saved runs with as the daemon makes it
/// (`crate::backtesthub_server::answer_from_saved::AnswersFromSaved::of_running_program`: the program its sandboxed
/// children run, `crate::strategy_sandbox::use_program`'s, hashed). It has no datahub, so it describes none.
pub fn spawn_serving_hub(
    store: Arc<dyn HistoryStore + Send + Sync>,
    vars: HashMap<String, String>,
    keep_per_strategy: Option<u32>,
    build: Option<String>,
) -> SocketAddr {
    let mut settings = vike_config::Settings::default();
    settings.config.saved_runs_per_strategy = keep_per_strategy;
    let answers =
        crate::backtesthub_server::answer_from_saved::AnswersFromSaved::of_running_program(None);
    let study = crate::backtest_cli::serve::serving_study(
        &vars,
        None,
        &settings,
        build.as_deref(),
        answers,
    );
    vike_log::loopback::spawn_on_loopback("backtesthub-test-hub", move |listener| {
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    })
}

/// A hub over `store` reading `strategies`, saving under `saving_to` when one is given.
fn spawn_hub(
    store: Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
    saving_to: Option<PathBuf>,
) -> SocketAddr {
    vike_log::loopback::spawn_on_loopback("backtesthub-test-hub", move |listener| {
        let mut study = StudyDirs {
            strategies: StrategyFiles::hub(strategies),
            ..StudyDirs::working_directory()
        };
        if let Some(runs_root) = saving_to {
            study.runs_root = runs_root;
            study.save_backtest_runs = true;
        }
        let _ = serve_authed(
            listener,
            store,
            study,
            None,
            NamedRunLane::DISARMED,
            vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
        );
    })
}

/// The answer THIS process gives `request` with the strategy file loaded HERE (`build`, the file's
/// `text`): the dispatch and the strategy files the sandboxed child answers with
/// (`crate::strategy_sandbox::serve_child`), in this process, as a CLIENT reads it: the daemon's one strip
/// (`saved_run::take_trade_ledger`) applied, so it compares with a socket's answer. The in-process half of a sandbox
/// parity test.
pub fn answer_in_this_process(
    request: vike_backtesthub_client::proto::Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    build: crate::studio_run::StrategyFileBuild,
    text: String,
) -> vike_backtesthub_client::proto::Response {
    let study = StudyDirs {
        strategies: crate::strategy_sandbox::loaded_strategy_files(
            &request,
            vec![(Ok(build), None)],
            vec![(text, std::collections::BTreeMap::new())],
        ),
        ..StudyDirs::working_directory()
    };
    let answer = crate::backtesthub_server::dispatch::handle_request_here(
        request,
        store,
        &study,
        NamedRunLane::DISARMED,
        None,
    );
    crate::backtesthub_server::saved_run::take_trade_ledger(answer).0
}

/// The answer a SANDBOXED child gives `request`, its `.rs` file read under `strategies` as a hub reads it:
/// the daemon's own route, without the socket, its model runs read under the working directory's runs root
/// as [`spawn_hub_over`]'s hub reads them. What the run-cost measurement drives. `None` when the request runs
/// no readable `.rs` file there.
pub fn answer_in_a_child(
    request: &vike_backtesthub_client::proto::Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    strategies: PathBuf,
) -> Option<vike_backtesthub_client::proto::Response> {
    // The daemon's own route: each file built (by the builder this process asks, or already built) and its model
    // texts read, then the child.
    let study =
        StudyDirs { strategies: StrategyFiles::hub(strategies), ..StudyDirs::working_directory() };
    Some(match crate::strategy_sandbox::resolve(request, &study)? {
        Ok(runs) => crate::strategy_sandbox::run_in_child(request, &runs, store, None, None).0,
        Err(why) => vike_backtesthub_client::proto::Response::Error(
            vike_studio_core::RunError::Strategy(why).wire_line(),
        ),
    })
}

/// How [`stamping`] stamps the UTC days of its bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayStamps {
    /// Every day stamped and settled: the store PINS the bars a run marks.
    Settled,
    /// Every day stamped, the last one PROVISIONAL (rows a later settled commit replaces).
    LastProvisional,
    /// No stamp at all: the trait's own answer, a store (or a datahub) without `SeriesDays`.
    Unknown,
}

/// `inner` with each UTC day of its bars stamped as `days` says (`HistoryStore::series_days` of a bar read, the datahub's `SeriesDays`,
/// which a memory store does not answer): a store that pins, or does not pin, the bars a run marks, so a test drives
/// both answers of a summarized single bar run's pin check (decision 0078's amendment of 2026-11-19 on what the hub
/// holds: `crate::strategy_sandbox`'s `pinned_log`). Every other read a bar run makes is `inner`'s.
pub fn stamping(
    inner: Arc<dyn HistoryStore + Send + Sync>,
    days: DayStamps,
) -> Arc<dyn HistoryStore + Send + Sync> {
    stamping_restamped_by(inner, days, Arc::default())
}

/// [`stamping`] whose every stamp moves when `restamp` does: a test adds to it to RE-IMPORT the stored days (decision
/// 0078's amendment of 2026-11-20: a run answered from its saved run must miss once a day it read is restamped).
pub fn stamping_restamped_by(
    inner: Arc<dyn HistoryStore + Send + Sync>,
    days: DayStamps,
    restamp: Arc<std::sync::atomic::AtomicU64>,
) -> Arc<dyn HistoryStore + Send + Sync> {
    Arc::new(Stamping { inner, days, stamped: Mutex::default(), restamp })
}

/// [`stamping`]'s store.
struct Stamping {
    inner: Arc<dyn HistoryStore + Send + Sync>,
    days: DayStamps,
    /// Each series' UTC days, by `venue/lane/symbol/interval`, read at its first ask.
    stamped: Mutex<HashMap<String, Vec<i64>>>,
    /// Added to every stamp ([`stamping_restamped_by`]).
    restamp: Arc<std::sync::atomic::AtomicU64>,
}

impl HistoryStore for Stamping {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: vike_data::TimestampRange,
    ) -> Result<Vec<vike_marketdata::Bar>, vike_data::HistoryStoreError> {
        self.inner.load_bars(venue, symbol, interval, range)
    }
    fn load_bars_sourced_capped(
        &self,
        venue: &str,
        source: &str,
        symbol: &str,
        interval: &str,
        range: vike_data::TimestampRange,
        budget: Option<usize>,
    ) -> Result<Vec<vike_marketdata::Bar>, vike_data::HistoryStoreError> {
        self.inner.load_bars_sourced_capped(venue, source, symbol, interval, range, budget)
    }
    fn scan_symbol_properties(
        &self,
        venue: &str,
        symbol: &str,
        range: vike_data::TimestampRange,
    ) -> Result<Vec<(i64, vike_model::SymbolProperties)>, vike_data::HistoryStoreError> {
        self.inner.scan_symbol_properties(venue, symbol, range)
    }
    fn list_series(&self) -> Result<Vec<vike_data::SeriesId>, vike_data::HistoryStoreError> {
        self.inner.list_series()
    }
    fn inventory(
        &self,
    ) -> Result<Vec<(vike_data::SeriesId, vike_data::SeriesCoverage)>, vike_data::HistoryStoreError>
    {
        self.inner.inventory()
    }
    fn series_gaps(
        &self,
        id: &vike_data::SeriesId,
    ) -> Result<Vec<(i64, i64)>, vike_data::HistoryStoreError> {
        self.inner.series_gaps(id)
    }
    fn series_facts(
        &self,
        id: &vike_data::SeriesId,
    ) -> Result<(vike_data::SeriesCoverage, Vec<String>), vike_data::HistoryStoreError> {
        self.inner.series_facts(id)
    }
    fn kept_instruments(
        &self,
    ) -> Result<Option<Vec<vike_model::Instrument>>, vike_data::HistoryStoreError> {
        self.inner.kept_instruments()
    }
    fn stock_splits(
        &self,
    ) -> Result<Option<Vec<vike_model::StockSplit>>, vike_data::HistoryStoreError> {
        self.inner.stock_splits()
    }
    fn series_days(
        &self,
        kind: vike_model::StoreKindId,
        venue: &str,
        source: Option<&str>,
        symbol: &str,
        interval: Option<&str>,
        range: vike_data::TimestampRange,
    ) -> Result<Option<Vec<vike_data::BarDay>>, vike_data::HistoryStoreError> {
        // Bars only: a run's pin check stamps the bars it marked.
        let (vike_model::StoreKindId::Bar, Some(interval)) = (kind, interval) else {
            return Ok(None);
        };
        if self.days == DayStamps::Unknown {
            return Ok(None);
        }
        // shortcut: each series is stamped ONCE, at its first ask (as a datahub answers from its manifests, reading
        // no row), so a test whose bars change after that ask reads stale stamps; key it by a bar count if one must.
        let key = format!("{venue}/{}/{symbol}/{interval}", source.unwrap_or_default());
        let mut stamped = self.stamped.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !stamped.contains_key(&key) {
            let all = vike_data::TimestampRange::all();
            let bars = match source {
                None => self.inner.load_bars(venue, symbol, interval, all)?,
                Some(lane) => {
                    self.inner.load_bars_sourced_capped(venue, lane, symbol, interval, all, None)?
                }
            };
            let day_of =
                |bar: &vike_marketdata::Bar| bar.timestamp.div_euclid(vike_model::MS_PER_DAY);
            let mut days: Vec<i64> = bars.iter().map(day_of).collect();
            days.dedup();
            stamped.insert(key.clone(), days);
        }
        // Every day a bar of which may overlap `range`: the whole day against the range's ends.
        let span =
            |day: i64| (day * vike_model::MS_PER_DAY, (day + 1) * vike_model::MS_PER_DAY - 1);
        let mut days: Vec<vike_data::BarDay> = stamped[&key]
            .iter()
            .copied()
            .filter(|&day| {
                let (first, last) = span(day);
                range.start.is_none_or(|s| last >= s) && range.end.is_none_or(|e| first <= e)
            })
            .map(|day| {
                let restamp = self.restamp.load(std::sync::atomic::Ordering::Relaxed);
                vike_data::BarDay { day, stamp: day as u64 * 31 + 7 + restamp, provisional: false }
            })
            .collect();
        if self.days == DayStamps::LastProvisional
            && let Some(last) = days.last_mut()
        {
            last.provisional = true;
        }
        Ok(Some(days))
    }
    vike_data::hist_store_stubs!(inert: writes, scan_quotes, scan_trades, scan_book_updates,
        scan_equity, scan_exec_fills, scan_exec_orders);
}

/// The test entry `crate::backtesthub_server::serve` is (key-less, the named run disarmed, the working
/// directory's folders), with its `.rs` strategy files built by `stand_in`.
pub fn serve_with_stand_ins(
    listener: std::net::TcpListener,
    store: Arc<dyn HistoryStore + Send + Sync>,
    stand_in: StandIn,
) -> std::io::Result<()> {
    let study = StudyDirs {
        strategies: StrategyFiles::stand_in(stand_in),
        ..StudyDirs::working_directory()
    };
    serve_authed(
        listener,
        store,
        study,
        None,
        NamedRunLane::DISARMED,
        vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT,
    )
}

/// **A research bin's `--json` document SAYS which percentile convention produced its numbers**,
/// asserted against the bin's own SOURCE, `bin_source` (the caller reads its own file), because `main`
/// assembles that document out of a dozen locals and no unit test can call it — the device
/// `crates/vike-tradehub/tests/source_gates/live_lock_claim_order_gate.rs` applies to the composition
/// roots, with that gate's rule: if this reddens because the emitting site was legitimately
/// re-spelled, UPDATE the needle, never delete the test.
///
/// The needle is ASSEMBLED rather than written out, so the joined string appears in no file and can
/// only match the emitting site — a literal would let a test pass on its own text, the
/// self-reference hazard `crates/vike-ops/tests/docs/unrun_command_gate.rs` declares for itself. And
/// EXACTLY one match is required, which fails a typo'd needle at zero (so the test cannot go vacuous)
/// and a marker duplicated onto a per-anchor lane or a per-entry row at two: the stamp belongs at RUN
/// level. The stamp must then name the percentile convention actually in force.
pub fn assert_json_stamps_its_percentile_once(bin_source: &str) {
    let key = "stats_provenance";
    let needle = format!("\"{key}\": binutil::{key}()");
    assert_eq!(
        bin_source.matches(&needle).count(),
        1,
        "the --json summary must stamp `{needle}` exactly once, at RUN level"
    );
    assert_eq!(
        crate::binutil::stats_provenance()["percentile_method"],
        serde_json::json!(vike_math::statistics::PERCENTILE_METHOD),
        "the stamp must name the percentile convention the bin's numbers come from"
    );
}

/// [`answer_in_this_process`] of a `[[mounts]]` portfolio's `request`: each mount's file loaded HERE (`mounts`, one
/// `(build, text)` a `.rs` mount in mount order), as the sandboxed child loads one a mount. The in-process half of a
/// portfolio's sandbox parity test.
pub fn answer_in_this_process_of_mounts(
    request: vike_backtesthub_client::proto::Request,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    mounts: Vec<(crate::studio_run::StrategyFileBuild, String)>,
) -> vike_backtesthub_client::proto::Response {
    let (builds, texts): (Vec<_>, Vec<_>) = mounts
        .into_iter()
        .map(|(build, text)| ((Ok(build), None), (text, std::collections::BTreeMap::new())))
        .unzip();
    let study = StudyDirs {
        strategies: crate::strategy_sandbox::loaded_strategy_files(&request, builds, texts),
        ..StudyDirs::working_directory()
    };
    let answer = crate::backtesthub_server::dispatch::handle_request_here(
        request,
        store,
        &study,
        NamedRunLane::DISARMED,
        None,
    );
    crate::backtesthub_server::saved_run::take_trade_ledger(answer).0
}
