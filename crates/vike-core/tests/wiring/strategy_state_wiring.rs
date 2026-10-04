//! Portfolio-observer PR-4 T2 wiring gate: with `CoreConfig::state_dir` set, a mount LOADS its
//! durable state from the sidecar (`Strategy::load_state`) before it ever runs, and a clean
//! shutdown SAVES the strategy's (possibly mutated) durable state back out (`Strategy::save_state`)
//! best-effort. The sidecar is opt-in (`state_dir` defaults to `None`), so this is the only core
//! test that touches one — mirrors `counters_wiring.rs` / `journal_wiring.rs`'s shape.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use vike_core::strategy_state::{mount_id_of, read_json, sidecar_path, write_json_atomic};
use vike_core::{CoreConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{Bar, Strategy};

/// A fresh, collision-free temp directory for one test (process id + a nanosecond stamp + a
/// per-call counter, mirroring `vike_core::strategy_state`'s own test helper) so parallel
/// `cargo test` runs never share one.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "vike-core-strategy-state-wiring-{tag}-{}-{}-{}",
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

/// A scripted strategy that persists a single counter `n`: `save_state` -> `{"n": n}`,
/// `load_state` restores it, and each closed bar increments it. Also mirrors the current value
/// into a captured `Arc<Mutex<i64>>` (`observed`) so the test can read it directly — the strategy
/// itself is moved into the mount's `Box` and unreachable after `spawn_core`.
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
    fn load_state(&mut self, state: &serde_json::Value) {
        if let Some(n) = state.get("n").and_then(|v| v.as_i64()) {
            self.n = n;
        }
        *self.observed.lock().unwrap() = self.n;
    }
}

/// A strategy whose `save_state` always panics — used to prove the runtime's save-on-stop loop
/// guards EACH mount independently: one mount panicking must not stop a LATER mount (in
/// iteration order) from having its own `save_state` called and written.
struct PanicOnSaveStrategy;

impl Strategy<LiveBroker> for PanicOnSaveStrategy {
    fn save_state(&self) -> Option<serde_json::Value> {
        panic!("boom: save_state panicked");
    }
}

/// A strategy whose `load_state` always panics (standing in for a corrupt/incompatible
/// sidecar) — used to prove the mount-time guard fails OPEN: the strategy keeps its
/// freshly-constructed (un-loaded) state and the core still builds/starts normally afterward.
/// Mirrors `CounterStrategy`'s counter/observed shape so a subsequent `on_bar` can prove `n`
/// was never touched by the panicking `load_state`.
struct PanicOnLoadStrategy {
    n: i64,
    observed: Arc<Mutex<i64>>,
}

impl Strategy<LiveBroker> for PanicOnLoadStrategy {
    fn on_bar(&mut self, _broker: &mut LiveBroker, _bar: &Bar) {
        self.n += 1;
        *self.observed.lock().unwrap() = self.n;
    }
    fn load_state(&mut self, _state: &serde_json::Value) {
        panic!("boom: load_state panicked");
    }
}

#[test]
fn mount_loads_on_start_and_saves_on_stop() {
    let dir = unique_temp_dir("mount");
    // pre-seed the sidecar for the mount_id this mount will derive
    let mid = mount_id_of("sim", "BTC", "1m");
    write_json_atomic(&sidecar_path(&dir, &mid), &serde_json::json!({"n": 5})).unwrap();

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(-1_i64));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        state_dir: Some(dir.clone()),
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
    // `assemble_core` (and thus `load_state`) runs synchronously on THIS thread before
    // `spawn_core` returns — only `CoreThread::run` moves to the "vt-core" thread — so the loaded
    // value is already visible with no wait/race.
    let handle = spawn_core(engine, config);

    // (a) the strategy observed n==5 right after mount: load_state ran before it ever traded.
    assert_eq!(*observed.lock().unwrap(), 5, "load_state ran at mount, before the strategy ran");

    // mutate n via a closed bar (on_bar increments the loaded 5 -> 6)
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            bar: bar(60_000, 100.0),
        })
        .unwrap();
    handle.shutdown_and_join();

    // (b) after the strategy mutates n and the core shuts down cleanly, the sidecar reflects it.
    assert_eq!(*observed.lock().unwrap(), 6, "on_bar incremented the loaded n");
    let saved = read_json(&sidecar_path(&dir, &mid)).expect("sidecar written on clean shutdown");
    assert_eq!(saved, serde_json::json!({"n": 6}), "save_state persisted the mutated n");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `state_dir: None` (the default) must stay byte-identical: no sidecar directory is ever
/// created, and a strategy that has never seen `load_state` starts from its own `n: 0`.
#[test]
fn state_dir_disabled_by_default_touches_no_sidecar() {
    assert!(
        CoreConfig::default().state_dir.is_none(),
        "sidecar persistence is opt-in (default None)"
    );

    let dir = unique_temp_dir("disabled");
    assert!(!dir.exists());

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(-1_i64));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        // state_dir stays None (CoreConfig::default())
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
    assert_eq!(*observed.lock().unwrap(), -1, "no load_state call without a configured state_dir");
    handle.shutdown_and_join();

    assert!(!dir.exists(), "no sidecar directory is ever created when state_dir is None");
}

/// Fix (PR-4 T2 review): `save_state` is arbitrary user strategy code and must be
/// `catch_unwind`-guarded like every other strategy-hook call site in `runtime/mod.rs` — a
/// panic in ONE mount's `save_state` must not unwind `run()`'s teardown (which would kill the
/// "vt-core" thread mid-shutdown and skip every mount sequenced after the panicking one).
#[test]
fn save_state_panic_does_not_kill_core_or_skip_other_mounts() {
    let dir = unique_temp_dir("save-panic");

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(-1_i64));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        state_dir: Some(dir.clone()),
        // mount 0 (primary): panics in save_state
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            strategy: Box::new(PanicOnSaveStrategy),
        }),
        // mount 1 (extra): saves normally — sequenced AFTER the panicking mount, so it only gets
        // its save_state called/written if the panic does not abort the rest of the save loop.
        extra_mounts: vec![StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "ETH".into(),
            interval: "1m".into(),
            strategy: Box::new(CounterStrategy { n: 0, observed: Arc::clone(&observed) }),
        }],
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine, config);

    // (a) the panicking save_state must not unwind run() / kill the vt-core thread mid-teardown:
    // shutdown_and_join must complete (return) rather than the test process observing a stuck
    // join or a panic propagating out of this call.
    handle.shutdown_and_join();

    // (b) mount 1 (sequenced after the panicking mount 0) still got its sidecar written — proof
    // the panic on mount 0 did not abort the rest of the save-on-stop loop.
    let eth_mid = mount_id_of("sim", "ETH", "1m");
    let saved = read_json(&sidecar_path(&dir, &eth_mid))
        .expect("second mount's sidecar written despite the first mount's save_state panicking");
    assert_eq!(saved, serde_json::json!({"n": 0}));

    // the panicking mount itself never produced a value, so it must not leave a sidecar behind
    let btc_mid = mount_id_of("sim", "BTC", "1m");
    assert!(
        read_json(&sidecar_path(&dir, &btc_mid)).is_none(),
        "a panicking save_state must not leave a sidecar behind"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Fix (PR-4 T2 review): `load_state` runs on arbitrary user strategy code at mount time
/// (inside `assemble_core`, synchronously on the CALLER's thread before `spawn_core` even
/// returns) and must fail OPEN on a corrupt/incompatible sidecar — never crash app startup.
#[test]
fn load_state_panic_starts_fresh() {
    let dir = unique_temp_dir("load-panic");
    // pre-seed a sidecar for the mount_id this mount will derive; PanicOnLoadStrategy's
    // load_state panics unconditionally, standing in for "content that makes load_state panic".
    let mid = mount_id_of("sim", "BTC", "1m");
    write_json_atomic(&sidecar_path(&dir, &mid), &serde_json::json!({"n": 999})).unwrap();

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTC",
    );
    let observed = Arc::new(Mutex::new(-1_i64));
    let t = Arc::new(AtomicI64::new(0));
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        state_dir: Some(dir.clone()),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            strategy: Box::new(PanicOnLoadStrategy { n: 0, observed: Arc::clone(&observed) }),
        }),
        ..CoreConfig::default()
    };
    // Must not panic: `assemble_core` (and thus the guarded `load_state` call) runs
    // synchronously on THIS thread before `spawn_core` returns (the same guarantee
    // `mount_loads_on_start_and_saves_on_stop` above relies on) — an unguarded panic here would
    // crash app startup outright, not just fail an assertion below.
    let handle = spawn_core(engine, config);

    // fresh (un-loaded) state: on_bar increments from the strategy's own constructed n=0, not
    // the corrupt sidecar's n=999 — proof load_state's panic left no partial mutation behind.
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTC".into(),
            interval: "1m".into(),
            bar: bar(60_000, 100.0),
        })
        .unwrap();
    handle.shutdown_and_join();

    assert_eq!(
        *observed.lock().unwrap(),
        1,
        "load_state panicked on the corrupt sidecar -> strategy started fresh (n=0), on_bar \
         incremented it to 1 (not 1000, the sidecar's n=999 + 1)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
