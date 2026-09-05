//! The only impure part of the supervisor: the background loop that glues [`super::config`],
//! [`super::schedule`], [`super::heal`], [`super::registry`] and [`super::status`] together over a
//! real `DataFusionHist`.
//!
//! THREADING is the shared `vike_data::worker::Worker` harness — the same one
//! `vike_data::MaintenanceScheduler` uses, and for the same reasons: a `Condvar`-interruptible
//! inter-pass sleep (so `stop()` is prompt instead of waiting out a whole tick and has no
//! lost-wakeup window), `stop()` = flag-then-join (deterministic teardown), and a `Drop` mirroring
//! it so the thread is never leaked. The loop SHAPE stays here, per that harness's contract: the
//! stop flag is checked before each pass AND between sources, so a stop signalled mid-pass ends the
//! loop as soon as the in-flight backfill returns.
//!
//! FAULT ISOLATION. Nothing inside a pass can end the loop: a `series_gaps` failure degrades that
//! symbol to freshness-only for the pass, a backfill failure is recorded and the next symbol/source
//! runs, and a status-write failure is logged and ignored. Only the failure COUNT changes behaviour
//! — through [`super::schedule::effective_interval_ms`]'s bounded exponential backoff and the
//! optional [`super::schedule::is_parked`] hard stop.
//!
//! TWO FAILURE LANES, and only ONE of them drives that backoff. A source whose freshness fetch is
//! healthy but whose historical heal window 429s/404s must keep collecting "now" at its configured
//! cadence — otherwise healing 2019 pushes today's bars out to the 1h backoff cap, exactly the
//! priority inversion [`super::heal`]'s ordering exists to prevent. So ONLY a
//! [`JobReason::Fresh`] failure increments `consecutive_failures` (which feeds backoff + parking);
//! heal-lane failures (`series_gaps` lookups and `Heal` jobs) increment a separate `heal_failures`
//! counter that is published for observability and changes no schedule.
//!
//! CONCURRENCY. The loop visits sources sequentially and never re-enters a pass until the previous
//! one returns, so a supervised backfill never races another supervised backfill. Racing an
//! unrelated live append is safe for the same reason `MaintenanceScheduler` is: every store write
//! serializes on the per-series manifest lock.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vike_data::worker::{Shared, Worker};
use vike_data::{DataFusionHist, SeriesId};

use super::SupervisorError;
use super::config::{SourceConfig, SupervisorConfig, validate};
use super::heal::{JobReason, plan_pass};
use super::registry::{Collector, collector_by_name};
use super::schedule::{effective_interval_ms, is_due, is_parked, next_run_ms};
use super::status::{SourceStatus, SupervisorStatus, write_status};

/// Mutable per-source bookkeeping. Lives only inside the loop (or one [`run_once`] call) — the
/// published view is [`SourceStatus`], built from this by [`build_status`].
#[derive(Debug, Default)]
struct SourceState {
    last_run_ms: Option<i64>,
    /// FRESHNESS-lane consecutive failures — the ONLY counter that drives backoff/parking.
    consecutive_failures: u32,
    /// HEAL-lane consecutive failures. Published, but deliberately schedule-neutral (module doc).
    heal_failures: u32,
    heal_queue: usize,
    /// Free-running rotation cursor into the gap set, advanced by `max_heal_jobs` after every pass
    /// so a hole the venue will never serve costs one slot per pass instead of blocking every later
    /// hole forever. Reduced modulo the per-symbol chunk count inside
    /// [`super::heal::plan_heal_jobs`] (two symbols of one source have different chunk counts).
    heal_cursor: usize,
    rows_ingested: u64,
    passes: u64,
    last_error: Option<String>,
}

/// A running supervisor. Construct with [`CollectorSupervisor::start`]; end with
/// [`CollectorSupervisor::stop`] — or just drop it, since the [`Worker`] field's own `Drop` stops
/// and joins.
pub struct CollectorSupervisor {
    passes: Arc<AtomicU64>,
    worker: Worker,
}

impl CollectorSupervisor {
    /// Validate `cfg`, resolve every declared collector, and spawn the loop: an immediate first
    /// pass, then one every `cfg.tick_secs` until [`stop`](Self::stop). `store` is shared (`Arc`) so
    /// the caller keeps using it while the supervisor collects into it.
    ///
    /// Fails ONLY on a bad config (unknown collector, duplicate name, …) — every runtime fault is
    /// handled inside the loop. An empty roster is legal and spawns an idle loop that dispatches
    /// nothing.
    pub fn start(
        store: Arc<DataFusionHist>,
        cfg: SupervisorConfig,
        status_path: Option<PathBuf>,
    ) -> Result<Self, SupervisorError> {
        validate(&cfg)?;
        let collectors = resolve_collectors(&cfg)?;
        let tick = Duration::from_secs(cfg.tick_secs.max(1));
        let passes = Arc::new(AtomicU64::new(0));
        let p = Arc::clone(&passes);
        let worker = Worker::spawn("vike-backfill-sup", move |shared| {
            run_loop(shared, &p, &store, &cfg, &collectors, status_path.as_deref(), tick);
        });
        Ok(Self { passes, worker })
    }

    /// Signal the loop to stop and JOIN its thread — deterministic teardown (the thread has
    /// returned when this returns). Idempotent; the `Drop` does the same.
    pub fn stop(&mut self) {
        self.worker.stop();
    }

    /// Completed loop passes so far. Monotonic, so a caller can poll it.
    pub fn passes_completed(&self) -> u64 {
        self.passes.load(Ordering::SeqCst)
    }
}

/// Run EXACTLY ONE pass over every configured source and return the resulting status (also written
/// to `status_path` when given). The unattended-loop's one-shot twin — what the bin's `--once` flag
/// runs, and the shape an external scheduler (cron, systemd timer) would drive.
pub fn run_once(
    store: &DataFusionHist,
    cfg: &SupervisorConfig,
    status_path: Option<&Path>,
) -> Result<SupervisorStatus, SupervisorError> {
    validate(cfg)?;
    let collectors = resolve_collectors(cfg)?;
    let mut states: Vec<SourceState> = cfg.sources.iter().map(|_| SourceState::default()).collect();
    // ONE clock read for the whole pass+publish, so `generated_ms`/`next_run_ms` can never be
    // milliseconds ahead of the `last_run_ms` they were derived from.
    let now = vike_model::now_ms();
    pass(store, cfg, &collectors, &mut states, now, None);
    let status = build_status(cfg, &collectors, &states, 1, now);
    if let Some(path) = status_path {
        write_status(path, &status)?;
    }
    Ok(status)
}

/// Bind every declared source to its registry row up front, so an unknown collector is a STARTUP
/// error instead of a per-pass surprise.
fn resolve_collectors(cfg: &SupervisorConfig) -> Result<Vec<&'static Collector>, SupervisorError> {
    cfg.sources
        .iter()
        .map(|s| {
            collector_by_name(&s.collector).ok_or_else(|| {
                SupervisorError::Invalid(format!(
                    "source {:?}: unknown collector {:?}",
                    s.name, s.collector
                ))
            })
        })
        .collect()
}

/// The worker body: pass, publish, interruptibly sleep — until stopped. The flag is checked at the
/// top (a stop before the first tick runs zero passes) and the sleep is itself stop-aware.
fn run_loop(
    shared: &Shared,
    passes: &AtomicU64,
    store: &DataFusionHist,
    cfg: &SupervisorConfig,
    collectors: &[&'static Collector],
    status_path: Option<&Path>,
    tick: Duration,
) {
    let mut states: Vec<SourceState> = cfg.sources.iter().map(|_| SourceState::default()).collect();
    while !shared.is_stopped() {
        // ONE clock read per iteration — the pass and the status it publishes must agree.
        let now = vike_model::now_ms();
        pass(store, cfg, collectors, &mut states, now, Some(shared));
        let n = passes.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(path) = status_path {
            let status = build_status(cfg, collectors, &states, n, now);
            if let Err(e) = write_status(path, &status) {
                tracing::warn!("collector-supervisor: status write failed: {e}");
            }
        }
        shared.sleep_interruptible(tick);
    }
}

/// One pass over the whole roster: run each source that is due (and not parked). `shared`, when
/// given, lets the pass abandon the remaining sources the moment a stop is signalled.
fn pass(
    store: &DataFusionHist,
    cfg: &SupervisorConfig,
    collectors: &[&'static Collector],
    states: &mut [SourceState],
    now_ms: i64,
    shared: Option<&Shared>,
) {
    for ((src, collector), st) in
        cfg.sources.iter().zip(collectors.iter().copied()).zip(states.iter_mut())
    {
        if shared.is_some_and(Shared::is_stopped) {
            break;
        }
        if is_parked(st.consecutive_failures, src.max_consecutive_failures) {
            continue;
        }
        let interval_ms =
            effective_interval_ms(src.cadence_ms(), st.consecutive_failures, src.max_backoff_ms());
        if !is_due(st.last_run_ms, interval_ms, now_ms) {
            continue;
        }
        run_source(store, src, collector, st, now_ms);
    }
}

/// Collapse a pass's errors into the single published `last_error` string WITHOUT losing the fact
/// that there were others: one error is quoted verbatim, N are summarized as `"N errors; first:
/// …"`. (Last-write-wins would let one symbol's error silently erase another's.)
fn summarize_errors(errors: &[String]) -> Option<String> {
    match errors.len() {
        0 => None,
        1 => Some(errors[0].clone()),
        n => Some(format!("{n} errors; first: {}", errors[0])),
    }
}

/// One source's pass: per symbol, read its stored watermark (the freshness anchor) and its gaps
/// (when healing), plan the pass, and dispatch every planned job through the registry's EXISTING
/// backfill fn. Errors are collected onto the state, not propagated — this function cannot fail.
///
/// The two lanes are tracked separately (module doc): `fresh_failed` is what the schedule sees.
fn run_source(
    store: &DataFusionHist,
    src: &SourceConfig,
    collector: &Collector,
    st: &mut SourceState,
    now_ms: i64,
) {
    let mut rows: u64 = 0;
    let mut heal_queue = 0usize;
    let mut errors: Vec<String> = Vec::new();
    let mut fresh_failed = false;
    let mut heal_failed = false;

    for symbol in &src.symbols {
        let id = SeriesId::per_symbol(
            src.kind.clone(),
            collector.venue.to_string(),
            symbol.clone(),
            Some(src.interval.clone()),
        );

        // THE FRESHNESS ANCHOR. Without it both ends of the window would move with the clock, so
        // every pass would mint a new commit key over an almost-identical span and the store — which
        // dedups by BATCH KEY, never by row value — would append the same bars again. A read failure
        // degrades to the plain lookback bound (0 = "unknown watermark"), which is correct but
        // duplicate-prone, so it is recorded; it is NOT a venue fault, so neither lane counts it.
        let last_ts = match store.series_coverage(&id) {
            Ok(cov) if cov.rows > 0 => cov.last_ts,
            Ok(_) => 0,
            Err(e) => {
                tracing::warn!(
                    "collector-supervisor: source {} symbol {symbol}: series_coverage failed: {e}",
                    src.name
                );
                errors.push(format!("{symbol}: series_coverage: {e}"));
                0
            }
        };

        let gaps = if src.heal {
            match store.series_gaps(&id) {
                Ok(g) => g,
                Err(e) => {
                    // Degrade to freshness-only for this symbol this pass; never abandon the pass.
                    tracing::warn!(
                        "collector-supervisor: source {} symbol {symbol}: series_gaps failed: {e}",
                        src.name
                    );
                    errors.push(format!("{symbol}: series_gaps: {e}"));
                    heal_failed = true;
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };

        let jobs = plan_pass(src, symbol.as_str(), &gaps, now_ms, last_ts, st.heal_cursor);
        heal_queue += jobs.iter().filter(|j| j.reason == JobReason::Heal).count();

        for job in &jobs {
            let dispatch = collector.backfill;
            match dispatch(
                store,
                job.symbol.as_str(),
                job.interval.as_str(),
                job.start_ms,
                job.end_ms,
            ) {
                Ok(n) => rows = rows.saturating_add(n as u64),
                Err(e) => {
                    tracing::warn!(
                        "collector-supervisor: source {} {} {} [{}, {}] ({:?}) failed: {e}",
                        src.name,
                        job.symbol,
                        job.interval,
                        job.start_ms,
                        job.end_ms,
                        job.reason
                    );
                    errors.push(format!(
                        "{} {} [{}, {}]: {e}",
                        job.symbol, job.interval, job.start_ms, job.end_ms
                    ));
                    match job.reason {
                        JobReason::Fresh => fresh_failed = true,
                        JobReason::Heal => heal_failed = true,
                    }
                }
            }
        }
    }

    st.last_run_ms = Some(now_ms);
    st.passes = st.passes.saturating_add(1);
    st.heal_queue = heal_queue;
    st.rows_ingested = st.rows_ingested.saturating_add(rows);
    // ONLY the freshness lane moves the schedule.
    st.consecutive_failures =
        if fresh_failed { st.consecutive_failures.saturating_add(1) } else { 0 };
    st.heal_failures = if heal_failed { st.heal_failures.saturating_add(1) } else { 0 };
    st.last_error = summarize_errors(&errors);
    // Rotate past whatever this pass just attempted, so the next one reaches the later holes even if
    // these can never be closed. `wrapping_add` (not modulo here) because each symbol's chunk count
    // differs — the planner reduces the cursor per symbol.
    if src.heal {
        st.heal_cursor = st.heal_cursor.wrapping_add(src.max_heal_jobs);
    }
}

/// Project the loop's private state into the published [`SupervisorStatus`]. Pure — no clock, no
/// store, no filesystem — so the status projection is unit-testable on its own.
fn build_status(
    cfg: &SupervisorConfig,
    collectors: &[&'static Collector],
    states: &[SourceState],
    passes: u64,
    now_ms: i64,
) -> SupervisorStatus {
    let sources = cfg
        .sources
        .iter()
        .zip(collectors.iter().copied())
        .zip(states.iter())
        .map(|((src, collector), st)| {
            let interval_ms = effective_interval_ms(
                src.cadence_ms(),
                st.consecutive_failures,
                src.max_backoff_ms(),
            );
            SourceStatus {
                name: src.name.clone(),
                collector: collector.name.to_string(),
                venue: collector.venue.to_string(),
                kind: src.kind.clone(),
                interval: src.interval.clone(),
                symbols: src.symbols.clone(),
                last_run_ms: st.last_run_ms,
                next_run_ms: next_run_ms(st.last_run_ms, interval_ms, now_ms),
                consecutive_failures: st.consecutive_failures,
                heal_failures: st.heal_failures,
                parked: is_parked(st.consecutive_failures, src.max_consecutive_failures),
                heal_queue: st.heal_queue,
                rows_ingested: st.rows_ingested,
                passes: st.passes,
                last_error: st.last_error.clone(),
            }
        })
        .collect();
    SupervisorStatus { generated_ms: now_ms, passes, sources }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::config::parse_supervisor_config;

    const CFG: &str = r#"
[[source]]
name = "binance-btc"
collector = "binance_klines"
symbols = ["BTCUSDT"]
interval = "1m"
cadence_secs = 60
max_backoff_secs = 3600
max_consecutive_failures = 3
"#;

    /// THE OFF-PATH PIN: with no `[[source]]` declared (the default / an empty config file) there is
    /// nothing to resolve and nothing to publish — a running supervisor is a pure no-op over the
    /// store, dispatching zero backfills. This is why merging the supervisor cannot change any
    /// existing behaviour: it only ever does what a config explicitly asks for.
    fn assert_inert(cfg: &SupervisorConfig) {
        let collectors = resolve_collectors(cfg).expect("an empty roster resolves");
        assert!(collectors.is_empty(), "nothing to dispatch");
        // `pass` over an empty roster iterates nothing, so it touches neither the store nor the
        // network; the projection below is the observable half of that no-op.
        let states: Vec<SourceState> = Vec::new();
        let status = build_status(cfg, &collectors, &states, 0, 1_700_000_000_000);
        assert!(status.sources.is_empty());
        assert_eq!(status.passes, 0);
        assert_eq!(status.generated_ms, 1_700_000_000_000);
    }

    #[test]
    fn the_default_config_is_inert() {
        assert_inert(&SupervisorConfig::default());
        assert_inert(&parse_supervisor_config("").unwrap());
        assert_inert(&parse_supervisor_config("tick_secs = 5").unwrap());
    }

    #[test]
    fn resolve_collectors_binds_each_declared_source() {
        let cfg = parse_supervisor_config(CFG).unwrap();
        let collectors = resolve_collectors(&cfg).unwrap();
        assert_eq!(collectors.len(), 1);
        assert_eq!(collectors[0].name, "binance_klines");
        assert_eq!(collectors[0].venue, "binance");
    }

    #[test]
    fn build_status_projects_a_never_run_source_as_due_now() {
        let cfg = parse_supervisor_config(CFG).unwrap();
        let collectors = resolve_collectors(&cfg).unwrap();
        let states = vec![SourceState::default()];
        let now = 1_700_000_000_000_i64;
        let status = build_status(&cfg, &collectors, &states, 3, now);
        assert_eq!(status.passes, 3);
        assert_eq!(status.sources.len(), 1);
        let row = &status.sources[0];
        assert_eq!(row.name, "binance-btc");
        assert_eq!(row.collector, "binance_klines");
        assert_eq!(row.venue, "binance");
        assert_eq!(row.kind, "bar");
        assert_eq!(row.interval, "1m");
        assert_eq!(row.symbols, vec!["BTCUSDT".to_string()]);
        assert_eq!(row.last_run_ms, None);
        assert_eq!(row.next_run_ms, now, "never run = due immediately");
        assert_eq!(row.consecutive_failures, 0);
        assert!(!row.parked);
        assert_eq!(row.last_error, None);
    }

    #[test]
    fn build_status_reflects_backoff_and_parking() {
        let cfg = parse_supervisor_config(CFG).unwrap();
        let collectors = resolve_collectors(&cfg).unwrap();
        let now = 1_700_000_000_000_i64;
        let last = now - 10_000;

        // 2 failures: cadence 60s doubled twice = 240s, under the 1h cap; not yet parked (limit 3).
        let states = vec![SourceState {
            last_run_ms: Some(last),
            consecutive_failures: 2,
            heal_failures: 7,
            heal_queue: 5,
            rows_ingested: 12,
            passes: 4,
            last_error: Some("venue fetch: boom".to_string()),
            ..Default::default()
        }];
        let status = build_status(&cfg, &collectors, &states, 1, now);
        let row = &status.sources[0];
        assert_eq!(row.next_run_ms, last + 240_000, "backoff is reflected in next_run_ms");
        assert_eq!(row.consecutive_failures, 2);
        assert_eq!(row.heal_failures, 7, "the heal lane is published…");
        assert!(!row.parked);
        assert_eq!(row.heal_queue, 5);
        assert_eq!(row.rows_ingested, 12);
        assert_eq!(row.passes, 4);
        assert_eq!(row.last_error.as_deref(), Some("venue fetch: boom"));

        // 3 failures = the configured limit -> parked.
        let states = vec![SourceState {
            last_run_ms: Some(last),
            consecutive_failures: 3,
            ..Default::default()
        }];
        assert!(build_status(&cfg, &collectors, &states, 1, now).sources[0].parked);
    }

    #[test]
    fn heal_lane_failures_never_slow_the_freshness_cadence() {
        // THE PRIORITY PIN: a source whose historical heal window keeps 429ing, but whose freshness
        // fetch is healthy, must keep collecting "now" at its configured cadence — not get pushed
        // to the 1h backoff cap and stop recording today's bars.
        let cfg = parse_supervisor_config(CFG).unwrap();
        let collectors = resolve_collectors(&cfg).unwrap();
        let now = 1_700_000_000_000_i64;
        let last = now - 10_000;
        let states = vec![SourceState {
            last_run_ms: Some(last),
            consecutive_failures: 0,
            heal_failures: 40,
            last_error: Some("2 errors; first: BTCUSDT 1m [0, 1]: 429".to_string()),
            ..Default::default()
        }];
        let row = &build_status(&cfg, &collectors, &states, 1, now).sources[0];
        assert_eq!(row.next_run_ms, last + 60_000, "cadence untouched by 40 heal failures");
        assert!(!row.parked, "the parking limit keys off the freshness lane only");
        assert_eq!(row.heal_failures, 40);
    }

    #[test]
    fn a_multi_error_pass_publishes_a_count_instead_of_last_write_wins() {
        assert_eq!(summarize_errors(&[]), None);
        assert_eq!(summarize_errors(&["a".to_string()]), Some("a".to_string()));
        assert_eq!(
            summarize_errors(&["a".to_string(), "b".to_string(), "c".to_string()]),
            Some("3 errors; first: a".to_string()),
            "one symbol's error must not silently erase another's"
        );
    }

    #[test]
    fn a_hand_built_config_naming_an_unknown_collector_is_rejected_before_any_thread_spawns() {
        // A hand-built (not parsed) config skips `parse_supervisor_config`'s validation, so
        // `start`/`run_once` must re-validate rather than trust the caller.
        let cfg = SupervisorConfig {
            sources: vec![SourceConfig {
                name: "x".to_string(),
                collector: "nope_klines".to_string(),
                symbols: vec!["BTCUSDT".to_string()],
                kind: "bar".to_string(),
                interval: "1m".to_string(),
                cadence_secs: 60,
                lookback_ms: 1_000,
                heal: true,
                max_heal_jobs: 1,
                max_backoff_secs: 60,
                max_consecutive_failures: None,
            }],
            ..Default::default()
        };
        let err = validate(&cfg).unwrap_err();
        assert!(err.to_string().contains("nope_klines"), "{err}");
        assert!(resolve_collectors(&cfg).is_err());
    }
}
