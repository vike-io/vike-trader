//! steal/core-live-scheduler: the WALL-CLOCK schedule fires `Strategy::on_schedule` in the LIVE
//! runtime off the timer-wheel poll boundary (never per message), journals each fire write-ahead,
//! and stays byte-identical when no mount has a schedule.
//!
//! The clock is injected (an `AtomicI64` a closure reads), so no test sleeps on real wall time to
//! reach a scheduled instant — it STORES the instant and lets the boundary waker (spawned only
//! because a schedule is active) reach the drain-loop boundary within one poll cadence.

use crate::scratch::Scratch;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::journal::{CommandJournal, JournalFileConfig, JournalRecord};
use vike_core::replay::replay_offline;
use vike_core::{
    spawn_core, CoreConfig, JournalConfig, LiveBroker, LiveSchedule, StrategyMount, TimeRule,
};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, Ingest, RiskGate, RiskLimits};
use vike_model::{Bar, Strategy, MS_PER_DAY};

/// A midnight UTC epoch (day 20_650 ≈ mid-2026) the tests key their instants off.
const D: i64 = 20_650 * MS_PER_DAY;

/// A scratch journal directory, removed when the returned guard drops. The journal's own `open`
/// calls `create_dir_all`, so the path is RESERVED rather than created. Hold the guard for the
/// whole test — see `crates/vike-core/src/scratch.rs` for the leak this closed.
fn unique_dir(tag: &str) -> Scratch {
    Scratch::reserved(&format!("sched-{tag}"))
}

/// Records every `on_schedule` fire (count + `(now, tag)`), and — when `submit` — rests a tagged
/// limit per fire so the fire has a JOURNALED effect (a `StrategySubmit`) for the replay test.
struct SchedRecorder {
    fires: Arc<AtomicUsize>,
    log: Arc<Mutex<Vec<(i64, String)>>>,
    submit: bool,
}
impl Strategy<LiveBroker> for SchedRecorder {
    fn on_schedule(&mut self, broker: &mut LiveBroker, tag: &str) {
        self.fires.fetch_add(1, Ordering::Relaxed);
        self.log.lock().unwrap().push((broker.now, tag.to_string()));
        if self.submit {
            // an explicit-price LIMIT so the order registers deterministically without any mark —
            // its coid is minted, so it rides the StrategySubmit journal lane replay re-applies.
            broker.submit_limit_tagged("sched", 1, 1.0, 100.0);
        }
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

fn mount(
    fires: &Arc<AtomicUsize>,
    log: &Arc<Mutex<Vec<(i64, String)>>>,
    submit: bool,
) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(SchedRecorder {
            fires: Arc::clone(fires),
            log: Arc::clone(log),
            submit,
        }),
        underlying_symbol: None,
    }
}

/// Keyed by the MOUNT ID. For this controller-id-free mount that is the legacy
/// `{venue}__{symbol}__{interval}` derivation — the same three parts the old tuple key spelled, now
/// as one identity segment.
fn schedules(rule: TimeRule, tag: &str) -> HashMap<String, LiveSchedule> {
    let mut m = HashMap::new();
    let mut s = LiveSchedule::new();
    s.on(rule, tag);
    m.insert(vike_core::strategy_state::mount_id_of("sim", "BTCUSDT", "1m"), s);
    m
}

fn wait_count(c: &Arc<AtomicUsize>, want: usize, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while c.load(Ordering::Relaxed) < want {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: have {}, want {}",
            c.load(Ordering::Relaxed),
            want
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A daily 00:00-UTC rule fires `on_schedule` at each midnight instant (and NOWHERE else), driven
/// by the injected clock. The first poll ESTABLISHES the latch (no spurious fire on mount); every
/// subsequent midnight crossing fires exactly once, stamped with that midnight's instant.
#[test]
fn daily_utc_rule_fires_on_schedule_at_each_midnight() {
    let fires = Arc::<AtomicUsize>::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicI64::new(D - 12 * 3_600_000)); // previous day, 12:00 UTC
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new({
            let c = Arc::clone(&clock);
            move || c.load(Ordering::Relaxed)
        }),
        strategy: Some(mount(&fires, &log, false)),
        mount_schedules: schedules(TimeRule::daily_at_utc(0, 0), "midnight"),
        schedule_poll: Some(Duration::from_millis(20)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(), cfg);

    // ESTABLISH: nudge the clock forward WITHIN the pre-midnight day so the first poll latches the
    // schedule (establish-then-fire ⇒ no spurious fire on mount). The settle covers several waker
    // cadences; if the establish somehow did not run the fire-waits below fail LOUDLY (never a
    // silent wrong-pass), because then the first real crossing would only establish, not fire.
    clock.store(D - 12 * 3_600_000 + 500, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(fires.load(Ordering::Relaxed), 0, "the establishing poll must not fire on_schedule");

    // cross midnight D ⇒ the first real fire, at exactly D
    clock.store(D, Ordering::Relaxed);
    wait_count(&fires, 1, "first midnight fire");

    // later the same day ⇒ no re-fire
    clock.store(D + 8 * 3_600_000, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fires.load(Ordering::Relaxed), 1, "no re-fire within the same day");

    // cross the next midnight ⇒ a second fire, at exactly D + one day
    clock.store(D + MS_PER_DAY, Ordering::Relaxed);
    wait_count(&fires, 2, "second midnight fire");

    handle.shutdown_and_join();

    let log = log.lock().unwrap();
    assert_eq!(log.len(), 2, "exactly two fires: {log:?}");
    assert!(log.iter().all(|(_, tag)| tag == "midnight"), "the registered tag fired: {log:?}");
    assert_eq!(log[0].0, D, "fired at exactly the midnight instant");
    assert_eq!(log[1].0, D + MS_PER_DAY, "and at the next midnight");
}

/// OFF/byte-identical: a mount with NO schedule (the default `mount_schedules` empty) never fires
/// `on_schedule`, even as the injected clock crosses two midnights — while the core still processes
/// market data normally (the bar's mark lands in the snapshot).
#[test]
fn a_mount_with_no_schedule_never_fires_on_schedule() {
    let fires = Arc::<AtomicUsize>::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicI64::new(D - 12 * 3_600_000));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new({
            let c = Arc::clone(&clock);
            move || c.load(Ordering::Relaxed)
        }),
        strategy: Some(mount(&fires, &log, false)),
        // mount_schedules EMPTY (default) — the byte-identical OFF path: no timer, no waker cadence.
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(), cfg);
    let cell = handle.snapshot_cell();

    // a real closed bar still folds (proves the core is alive), and the clock crosses midnights —
    // a scheduled mount WOULD fire; this one has no schedule.
    clock.store(D, Ordering::Relaxed);
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: Bar {
                ts: D,
                open: 100.0,
                high: 100.0,
                low: 100.0,
                close: 100.0,
                volume: 1.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            },
        })
        .unwrap();
    clock.store(D + MS_PER_DAY, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(200));
    handle.shutdown_and_join();

    assert_eq!(fires.load(Ordering::Relaxed), 0, "no schedule ⇒ on_schedule never fires");
    assert!(log.lock().unwrap().is_empty());
    assert!(
        cell.load_full().marks.iter().any(|(_, s, px)| s == "BTCUSDT" && *px == 100.0),
        "the core still folded the bar's mark — it is alive, just schedule-free"
    );
}

/// The fire is journaled write-ahead (a `ScheduleFire` marker), the order it drove rides the normal
/// `StrategySubmit` lane, and a scheduled session REPLAYS (it does NOT forfeit replay like the
/// stuck-order watchdog) reproducing that order.
#[test]
fn a_scheduled_fire_is_journaled_and_replays() {
    let dir = unique_dir("replay");
    let fires = Arc::<AtomicUsize>::default();
    let log = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(AtomicI64::new(D - 12 * 3_600_000));
    let cfg = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new({
            let c = Arc::clone(&clock);
            move || c.load(Ordering::Relaxed)
        }),
        coid_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount(&fires, &log, true)), // submit one order per fire
        mount_schedules: schedules(TimeRule::daily_at_utc(0, 0), "midnight"),
        schedule_poll: Some(Duration::from_millis(20)),
        journal: Some(JournalConfig {
            dir: dir.to_path_buf(),
            file: JournalFileConfig { segment_bytes: 1024 * 1024, flush_every: 1 },
            // large ⇒ no cadence Snap fires; the exit Snap captures the whole session, which is what
            // replay restores from and fences against (the schedule session must NOT refuse).
            snapshot_every: 1024,
        }),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(), cfg);

    // establish, then cross ONE midnight ⇒ exactly one fire ⇒ one journaled order
    clock.store(D - 12 * 3_600_000 + 500, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(400));
    clock.store(D, Ordering::Relaxed);
    wait_count(&fires, 1, "the scheduled fire");
    handle.shutdown_and_join();

    let recs = CommandJournal::read_all(&dir).unwrap();
    // the FIRE is journaled write-ahead...
    let sched: Vec<_> = recs
        .iter()
        .filter_map(|r| match r {
            JournalRecord::ScheduleFire { mount_id, tag, .. } => {
                Some((mount_id.clone(), tag.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(sched.len(), 1, "exactly one ScheduleFire record");
    assert_eq!(sched[0].1, "midnight", "the fired tag is recorded");
    // ...and the order it produced rode the normal StrategySubmit lane, AFTER the decision marker.
    let fire_pos =
        recs.iter().position(|r| matches!(r, JournalRecord::ScheduleFire { .. })).unwrap();
    let sub_pos = recs
        .iter()
        .position(|r| matches!(r, JournalRecord::StrategySubmit { .. }))
        .expect("the fire's order is journaled as StrategySubmit");
    assert!(fire_pos < sub_pos, "the decision is on disk BEFORE the order it drove");

    // NO waker refusal record ⇒ the schedule did NOT arm the replay-forfeit gate (unlike the
    // stuck-order watchdog / dead-man switch).
    assert!(
        !recs.iter().any(|r| matches!(r, JournalRecord::Cmd { msg: Ingest::Watchdog, .. })),
        "a schedule-only session journals no waker refusal record — so it replays"
    );

    // it REPLAYS, and the fire's order is reproduced.
    let out = replay_offline(&dir).expect("a scheduled session must replay");
    assert!(!out.engines[0].registry.is_empty(), "the fire's order is reproduced by replay");

    let _ = std::fs::remove_dir_all(&dir);
}
