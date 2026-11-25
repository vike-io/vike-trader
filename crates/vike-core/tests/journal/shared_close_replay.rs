//! **A shared close's grace timeout, journaled and replayed** (decision 0271, its limit (a) closed,
//! `crates/vike-core/src/runtime/shared_close.rs`): a live core whose mount waited out the grace at a close it
//! shares with an input bar journals the timeout write-ahead of the step it released, at the clock of the boundary
//! that fired it, and the step's order after it; the mount that reads no input steps at the bar itself, before
//! both. The journal replays to the same engine state (the fence), with the orders in the order the live core
//! placed them.
//!
//! The clock is injected and moved past the grace by hand; the core's own wake (a thread `spawn_core` arms for a
//! held step) reaches the boundary, so nothing but the bar reaches the journal's command lane.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_core::replay::replay_offline;
use vike_core::{CoreConfig, InputRead, LiveBroker, Mount, SHARED_CLOSE_GRACE_MS, spawn_core};
use vike_execution::BarUpdate;
use vike_journal::JournalRecord;
use vike_marketdata::Bar;
use vike_model::{OrderBroker, OrderPlacement, Strategy};

use crate::kit::engines::sim_engine;
use crate::kit::journal::{clock, journal_at, records};
use crate::scratch::Scratch;

const H: i64 = 3_600_000;
const H4: i64 = 4 * H;
/// The input: the traded symbol's own 4h bars, stored at 0 and 4h (closing at 4h and 8h).
const HIGHER: &str = "sim:BTCUSDT:4h";
/// The core clock when the bar arrives.
const T0: i64 = 1_700_000_000_000;

/// What each decision read: `(mount, bar ts, the input's close)`.
type Log = Arc<Mutex<Vec<(&'static str, i64, Option<f64>)>>>;

/// Rests a buy limit at every decision, after logging what it read of the 4h input.
struct Placer {
    name: &'static str,
    reads: Vec<String>,
    log: Log,
}

impl Strategy<LiveBroker> for Placer {
    fn inputs(&self) -> Vec<String> {
        self.reads.clone()
    }
    fn on_bar(&mut self, b: &mut LiveBroker, bar: &Bar) {
        let read = b.input(HIGHER).map(|x| x.close);
        self.log.lock().unwrap().push((self.name, bar.timestamp, read));
        b.place("", "", 1, 1.0, OrderPlacement::Limit { price: 50.0 }, false);
    }
}

fn mount(name: &'static str, reads: &[&str], log: &Log) -> Mount {
    let strategy =
        Placer { name, reads: reads.iter().map(|s| s.to_string()).collect(), log: Arc::clone(log) };
    Mount {
        order_client_id_code: None,
        account: None,
        symbols: Vec::new(),
        controller_id: Some(name.to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        strategy: Box::new(strategy),
    }
}

fn bar(ts: i64) -> Bar {
    Bar::ohlcv(ts, 100.0, 100.0, 100.0, 100.0, 1.0)
}

fn until(what: &str, ok: impl Fn() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// **The grace's timeout is on disk before the step it released, and the journal replays**: the record order is
/// the plain mount's order, the timeout (stamped when the grace fired), the reading mount's order; the reading
/// mount read the 4h bar before the shared close; the replay reproduces the registry in that order.
#[test]
fn a_shared_close_timeout_is_journaled_and_replays_to_the_same_order() {
    let dir = Scratch::reserved("shared-close-replay");
    let now = Arc::new(AtomicI64::new(T0));
    let log = Log::default();
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: clock(&now),
        order_client_id_session: Some(("cafef00d".into(), 0)),
        strategy: Some(mount("reads", &[HIGHER], &log)),
        extra_mounts: vec![mount("plain", &[], &log)],
        input_opener: Some(Box::new(|name: &str, _: &str, _: &str, _: &str| match name {
            HIGHER => Ok(InputRead {
                key: ("sim".into(), "BTCUSDT".into(), "4h".into()),
                interval_ms: H4,
                bars: vec![Bar::ohlcv(0, 10.0, 10.0, 10.0, 10.0, 1.0), bar_at(H4, 20.0)],
                calendar: None,
            }),
            other => Err(format!("no series {other}")),
        })),
        journal: Some(journal_at(&dir, 1 << 20, 1_000)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(sim_engine(), config);
    // The decision at 12:00 shares its close with the 4h bar opened at 8h, which never comes.
    let update = BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1h".into(),
        bar: bar(11 * H),
        sides: None,
    };
    handle.bar_sender().close(update).unwrap();
    until("the plain mount's step", || log.lock().unwrap().len() == 1);
    assert_eq!(log.lock().unwrap()[0].0, "plain", "the mount that reads no input stepped at once");
    now.store(T0 + SHARED_CLOSE_GRACE_MS, Ordering::Relaxed);
    until("the held step's release", || log.lock().unwrap().len() == 2);
    handle.shutdown_and_join();

    assert_eq!(
        *log.lock().unwrap(),
        vec![("plain", 11 * H, None), ("reads", 11 * H, Some(20.0))],
        "the held step read the 4h bar before the shared close"
    );
    let recs = records(&dir);
    let at = |pick: &dyn Fn(&JournalRecord) -> bool| recs.iter().position(pick).expect("a record");
    let plain =
        at(&|r| matches!(r, JournalRecord::StrategySubmit { mount_id, .. } if mount_id == "plain"));
    let timeout = at(&|r| matches!(r, JournalRecord::SharedCloseTimeout { .. }));
    let reads =
        at(&|r| matches!(r, JournalRecord::StrategySubmit { mount_id, .. } if mount_id == "reads"));
    assert!(plain < timeout && timeout < reads, "plain's order, the timeout, then reads' order");
    let JournalRecord::SharedCloseTimeout { now_ms, mount_id, close_ms, input, .. } =
        &recs[timeout]
    else {
        unreachable!()
    };
    assert_eq!((mount_id.as_str(), *close_ms, input.as_str()), ("reads", 12 * H, HIGHER));
    assert_eq!(*now_ms, T0 + SHARED_CLOSE_GRACE_MS, "stamped when the grace fired");

    let order_client_ids = |e: &vike_execution::EngineSnapshot| -> Vec<String> {
        e.registry.iter().map(|(order_client_id, _)| order_client_id.clone()).collect()
    };
    let exit = recs.iter().rev().find_map(|r| match r {
        JournalRecord::Snapshot { engines, .. } => Some(order_client_ids(&engines[0])),
        _ => None,
    });
    let exit = exit.expect("the exit Snapshot");
    assert_eq!(exit.len(), 2, "two orders rest: {exit:?}");
    let out = replay_offline(&dir).expect("a session with a shared close's timeout replays");
    let replayed = order_client_ids(&out.engines[0]);
    assert_eq!(replayed, exit, "the replay placed the two orders in the order the live core did");
}

fn bar_at(ts: i64, close: f64) -> Bar {
    Bar::ohlcv(ts, close, close, close, close, 1.0)
}
