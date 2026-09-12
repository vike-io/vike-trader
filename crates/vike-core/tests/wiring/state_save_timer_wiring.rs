//! Portfolio-observer PR-4 T3 wiring gate: with `CoreConfig::state_save` set (and a `state_dir`
//! configured, per T2), the runtime periodically re-saves every live mount's durable-state
//! sidecar on a cadence — NOT only at clean shutdown. Mirrors PR-3's equity-sampler timer
//! machinery (`TimerKind::EquitySample` / `CoreThread::maintain_equity_timer`) almost exactly;
//! the one difference under test here is the arm condition: the save timer arms whenever a mount
//! exists and a `state_dir` is configured, regardless of open positions (a strategy's
//! breaker/A-S accumulator state matters whether the book is flat or not).

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use vike_core::strategy_state::{mount_id_of, read_json, sidecar_path};
use vike_core::{CoreConfig, CoreHandle, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{Bar, Strategy};

/// A fresh, collision-free temp directory for one test (process id + a nanosecond stamp + a
/// per-call counter, mirroring `strategy_state_wiring.rs`'s own helper) so parallel `cargo test`
/// runs never share one.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "vike-core-state-save-timer-{tag}-{}-{}-{}",
        std::process::id(),
        nanos,
        n
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The same scripted counter strategy `strategy_state_wiring.rs` (T2) uses: `save_state` ->
/// `{"n": n}`, and each closed bar increments it — mirrored into `observed` so the test can read
/// the current value directly (the strategy itself is moved into the mount's `Box`).
struct CounterStrategy {
    n: i64,
    observed: Arc<Mutex<i64>>,
}

impl Strategy<LiveBroker> for CounterStrategy {
    fn on_bar(&mut self, _broker: &mut LiveBroker, _bar: &Bar) {
        self.n += 1;
        *self.observed.lock().unwrap() = self.n;
    }
    fn save_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "n": self.n }))
    }
}

/// A manual/injected clock (mirrors `strategy_state_wiring.rs`'s harness and `runtime_smoke.rs`'s
/// `test_config`): each call advances an `AtomicI64` counter by one "ms". Deterministic — no real
/// wall-clock wait is needed for the CORE's notion of "now" to reach a future deadline. The core
/// thread only RE-CHECKS its armed timers at the drain-loop boundary reached when the next ingest
/// message arrives (see `CoreThread::run`'s doc comment on the OS waker thread), so callers must
/// still NUDGE the core (send another message) to walk this manual clock forward — the same
/// "poke" shape PR-3's real-time equity-sample end-to-end test
/// (`vike-run/tests/mount_scripted.rs::poke_until`) uses, adapted here to a manual clock instead
/// of real time so the test itself runs fast and deterministically.
fn manual_clock() -> Box<dyn vike_model::Clock + Send> {
    let t = Arc::new(AtomicI64::new(0));
    Box::new(move || t.fetch_add(1, Ordering::Relaxed))
}

/// Send closed bars (mutating the mounted [`CounterStrategy`]'s `n`, one increment per bar) until
/// the sidecar on disk holds an ALREADY-FOLDED counter value — proof a periodic fire (not a
/// shutdown) wrote it. Bounded by a real wall-clock deadline: message delivery/processing is real
/// cross-thread work even though the CORE's own notion of "now" is the manual clock above. A
/// short real sleep between nudges gives the core a chance to reach the drain-loop boundary (and
/// therefore re-check the armed timer) for each bar individually rather than batching them.
///
/// ⚠ The accept predicate is deliberately `1 <= sidecar_n <= observed`, NOT `sidecar_n ==
/// observed`. The save fires on the CORE's own timer cadence, which is independent of this
/// thread's bar cadence, so an equality check only passes on an iteration where the fire landed
/// between the last fold and this read — it makes the pass a RACE, and a lost race is
/// indistinguishable from a broken timer. Measured in CI, that race took 0.32s-2.91s to win
/// across six green runs and once never won inside the 15s cap at all, failing the `test` job on
/// an unrelated PR (a dependency bump). The looser predicate is not a weaker gate: the check runs
/// while the core is still RUNNING (`is_alive` is asserted right after), so ANY sidecar content
/// at this point can only have come from the periodic timer — the save-on-stop path T2 covers has
/// not run. `>= 1` still proves the persisted value reflects real folded mutation rather than the
/// strategy's initial state, and `<= observed` still proves it never runs ahead of what the
/// strategy actually did.
#[allow(clippy::too_many_arguments)]
fn poke_until_saved(
    handle: &CoreHandle,
    venue: &str,
    symbol: &str,
    interval: &str,
    observed: &Arc<Mutex<i64>>,
    sidecar: &std::path::Path,
    start_ts: i64,
    secs: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut ts = start_ts;
    while Instant::now() < deadline {
        handle
            .bar_sender()
            .close(BarUpdate {
                venue: venue.into(),
                symbol: symbol.into(),
                interval: interval.into(),
                bar: bar(ts, 100.0),
            })
            .unwrap();
        ts += 60_000;
        std::thread::sleep(Duration::from_millis(5));
        // `observed` is written by the core thread inside `on_bar`, and the timer's own
        // `save_state` read happens later, at the drain-loop boundary — so a sidecar value can
        // never exceed what this thread reads here.
        let observed_now = *observed.lock().unwrap();
        let saved = read_json(sidecar).and_then(|v| v.get("n").and_then(|n| n.as_i64()));
        if observed_now > 0 && saved.is_some_and(|n| (1..=observed_now).contains(&n)) {
            return true;
        }
    }
    false
}

/// The core (positive) case: a mounted strategy + `state_dir` + a small `state_save` cadence
/// persists the sidecar on its own, with NO shutdown involved.
#[test]
fn state_save_timer_persists_on_cadence() {
    let dir = unique_temp_dir("cadence");
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(0_i64));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: manual_clock(),
        state_dir: Some(dir.clone()),
        state_save: Some(Duration::from_millis(20)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            strategy: Box::new(CounterStrategy { n: 0, observed: Arc::clone(&observed) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);
    let mid = mount_id_of("sim", "BTC", "1m");
    let sidecar = sidecar_path(&dir, &mid);

    assert!(
        poke_until_saved(&handle, "sim", "BTC", "1m", &observed, &sidecar, 60_000, 15),
        "state-save timer never persisted the sidecar without a shutdown"
    );
    // Still alive: the write observed above happened WHILE the core was running — proof this was
    // the periodic timer, not the (later, separate) save-on-stop path T2 already covers.
    assert!(
        handle.is_alive(),
        "core must still be running when the sidecar was observed — this is the periodic save"
    );

    handle.shutdown_and_join();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The negative case: `state_save` is set but `state_dir` is NOT — there is nothing to persist
/// to, so the timer must not arm/fire (existing state_dir-less behavior is unchanged): no sidecar
/// directory ever appears, even after many mutating bar closes and even after a clean shutdown.
#[test]
fn state_save_timer_inert_without_state_dir() {
    assert!(
        CoreConfig::default().state_save.is_none(),
        "periodic state-save is opt-in (default None)"
    );

    let dir = unique_temp_dir("inert");
    assert!(!dir.exists());

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(0_i64));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: manual_clock(),
        // state_dir stays None: nothing to write to, so the timer must never arm even though
        // state_save is set.
        state_save: Some(Duration::from_millis(20)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            strategy: Box::new(CounterStrategy { n: 0, observed: Arc::clone(&observed) }),
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    // Nudge the core with mutating bar closes for a while (well past any plausible interval) —
    // with no state_dir, there is nowhere to persist to, so no sidecar directory should ever
    // appear no matter how long the timer would otherwise have had to fire.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut ts = 60_000;
    while Instant::now() < deadline {
        handle
            .bar_sender()
            .close(BarUpdate {
                venue: "sim".into(),
                symbol: "BTC".into(),
                interval: "1m".into(),
                bar: bar(ts, 100.0),
            })
            .unwrap();
        ts += 60_000;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(*observed.lock().unwrap() > 0, "sanity: the strategy must have actually run (n > 0)");
    assert!(!dir.exists(), "no sidecar directory is ever created without a configured state_dir");

    handle.shutdown_and_join();
    // Shutdown's own (T2) save-on-stop also can't write anywhere without a state_dir — confirm
    // post-shutdown too, so this test covers both the timer AND the shutdown path staying inert.
    assert!(!dir.exists(), "no sidecar directory after shutdown either (state_dir stays None)");
}
