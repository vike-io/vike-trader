//! Core control-ergonomics gates (the ergonomics pack): the compound `MarketExit` verb end-to-end,
//! MANAGED GTD EXPIRY on the boundary sweep cadence (mock clock — fires ONCE, at the deadline,
//! never when the feature is off), and the opt-in periodic portfolio journal record (written on
//! cadence, absent by default). Every feature here is OPT-IN: the "off" half of each pair asserts
//! the default runtime is unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vike_core::{CoreConfig, JournalConfig, journal::JournalRecord, spawn_core};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, OrderIntent, OrderStatus,
    RiskGate, RiskLimits,
};

use vike_model::events::{Event, OrderAccepted, OrderCanceled, OrderSubmitted};
use vike_model::{OrderRequest, TimeInForce};

/// A resting-order client: `submit` acks (so the order reaches `Accepted` and RESTS — unlike
/// `TestExecutionClient`, which fills immediately), `cancel` terminalizes it AND counts the calls,
/// so a test can prove the GTD sweep fires exactly once per order.
#[derive(Debug, Default)]
struct RestingClient {
    pending: std::collections::VecDeque<Event>,
    cancels: Arc<AtomicUsize>,
}

impl ExecutionClient for RestingClient {
    fn submit(&mut self, request: &OrderRequest) {
        // Initialized → Submitted → Accepted: the FSM only accepts the ack after the submit
        // transition, so emit both (a real adapter's venue does the same in two frames).
        self.pending.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.pending.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.cancels.fetch_add(1, Ordering::Relaxed);
        self.pending.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "test".into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
}

fn engine_with(client: RestingClient) -> ExecutionEngine<RestingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        client,
        "sim",
        "BTCUSDT",
    )
}

/// A resting limit order with an explicit GTD deadline (the terms already on `OrderRequest`).
fn gtd_request(coid: &str, expiry_ms: i64) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Gtd,
        gtd_expiry: Some(expiry_ms),
        ..Default::default()
    })
}

/// Poll the snapshot until `pred` holds, or fail after `secs`.
fn wait_until(
    cell: &arc_swap::ArcSwap<vike_core::CoreSnapshot>,
    secs: u64,
    what: &str,
    pred: impl Fn(&vike_core::CoreSnapshot) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if pred(&cell.load_full()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// MANAGED GTD, mock clock: an order rests untouched while its deadline is in the future, is
/// canceled once the (injected, test-driven) clock passes it, and — the fire-once guard — the
/// sweep issues EXACTLY ONE cancel no matter how many sweep ticks elapse afterwards.
#[test]
fn gtd_expiry_cancels_the_resting_order_once_at_its_deadline() {
    let cancels = Arc::new(AtomicUsize::new(0));
    let client = RestingClient { pending: Default::default(), cancels: Arc::clone(&cancels) };
    let now = Arc::new(AtomicI64::new(1_000));
    let clock = Arc::clone(&now);
    let cfg = CoreConfig {
        gtd_sweep: Some(Duration::from_millis(20)),
        clock: Box::new(move || clock.load(Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_with(client), cfg);
    let cell = handle.snapshot_cell();

    handle.try_command(Command::Order(OrderIntent::Submit(gtd_request("g1", 5_000)))).unwrap();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("g1").map(|o| o.status) == Some(OrderStatus::Accepted)
    });

    // Deadline still in the future (clock = 1000, expiry = 5000): several sweep ticks pass and the
    // order MUST be left alone.
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(cancels.load(Ordering::Relaxed), 0, "swept an order whose GTD had not passed");
    assert!(!cell.load_full().order("g1").unwrap().status.is_terminal(), "still resting");

    // Cross the deadline on the injected clock — the boundary sweep now cancels it.
    now.store(5_000, Ordering::Relaxed);
    wait_until(&cell, 5, "gtd cancel", |s| {
        s.order("g1").map(|o| o.status) == Some(OrderStatus::Canceled)
    });

    // FIRE-ONCE: many more ticks, still exactly one cancel.
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(cancels.load(Ordering::Relaxed), 1, "GTD expiry must cancel exactly once");
    handle.shutdown_and_join();
}

/// OFF BY DEFAULT: the same long-past-expiry order on a default config is never touched — no timer
/// is armed, no sweep runs, no cancel is issued.
#[test]
fn gtd_expiry_is_inert_when_the_sweep_is_not_configured() {
    let cancels = Arc::new(AtomicUsize::new(0));
    let client = RestingClient { pending: Default::default(), cancels: Arc::clone(&cancels) };
    let now = Arc::new(AtomicI64::new(9_000));
    let clock = Arc::clone(&now);
    let cfg = CoreConfig {
        clock: Box::new(move || clock.load(Ordering::Relaxed)),
        ..CoreConfig::default() // gtd_sweep: None
    };
    let handle = spawn_core(engine_with(client), cfg);
    let cell = handle.snapshot_cell();

    // expiry 1000 is already 8s in the past on the injected clock
    handle.try_command(Command::Order(OrderIntent::Submit(gtd_request("g2", 1_000)))).unwrap();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("g2").map(|o| o.status) == Some(OrderStatus::Accepted)
    });
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(cancels.load(Ordering::Relaxed), 0, "GTD sweep must be inert unless configured");
    assert!(
        !cell.load_full().order("g2").unwrap().status.is_terminal(),
        "untouched, still resting"
    );
    handle.shutdown_and_join();
}

/// A resting limit order with `Day` time-in-force (no explicit deadline — the session is the UTC
/// day of the order's creation clock).
fn day_request(coid: &str) -> Box<OrderRequest> {
    Box::new(OrderRequest {
        client_order_id: coid.into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(100.0),
        time_in_force: TimeInForce::Day,
        ..Default::default()
    })
}

/// MANAGED DAY (dedup A4), mock clock: the sweep now enforces `Day` too (before A4 it silently
/// ignored every non-`Gtd` order). A `Day` order rests untouched within its own UTC day and is
/// canceled once the injected clock crosses into the next UTC day — anchored on its creation clock,
/// terminalizing via the same managed cancel the GTD arm uses.
#[test]
fn day_expiry_cancels_the_resting_order_at_the_utc_day_boundary() {
    const MS_PER_DAY: i64 = vike_model::MS_PER_DAY;
    let cancels = Arc::new(AtomicUsize::new(0));
    let client = RestingClient { pending: Default::default(), cancels: Arc::clone(&cancels) };
    // start just after midnight of UTC day 20_650, so `created_ms` anchors on that day
    let day0 = 20_650 * MS_PER_DAY;
    let now = Arc::new(AtomicI64::new(day0 + 1_000));
    let clock = Arc::clone(&now);
    let cfg = CoreConfig {
        gtd_sweep: Some(Duration::from_millis(20)),
        clock: Box::new(move || clock.load(Ordering::Relaxed)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine_with(client), cfg);
    let cell = handle.snapshot_cell();

    handle.try_command(Command::Order(OrderIntent::Submit(day_request("d1")))).unwrap();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("d1").map(|o| o.status) == Some(OrderStatus::Accepted)
    });

    // still the same UTC day (advance well within it): the order must be left alone.
    now.store(day0 + MS_PER_DAY - 1, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(cancels.load(Ordering::Relaxed), 0, "a Day order must survive its own UTC day");
    assert!(!cell.load_full().order("d1").unwrap().status.is_terminal(), "still resting");

    // Cross well into a later UTC day. (The sweep timer re-arms at `injected_now + cadence`, so
    // the same-day poke above pushed the next fire a cadence past `day0 + MS_PER_DAY - 1`; jump a
    // full day clear of it so the boundary sweep is guaranteed to fire — the DECISION boundary
    // itself is unit-tested exactly in `vike_model::tif_expired`.)
    now.store(day0 + 2 * MS_PER_DAY, Ordering::Relaxed);
    wait_until(&cell, 5, "day cancel", |s| {
        s.order("d1").map(|o| o.status) == Some(OrderStatus::Canceled)
    });
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(cancels.load(Ordering::Relaxed), 1, "Day expiry must cancel exactly once");
    handle.shutdown_and_join();
}

/// MARKET EXIT end-to-end through the real command lane: one compound verb cancels the resting
/// order and submits the reduce-only closing order for the open position, with no new command or
/// journal vocabulary involved.
#[test]
fn market_exit_cancels_orders_and_flattens_positions_end_to_end() {
    let cancels = Arc::new(AtomicUsize::new(0));
    let client = RestingClient { pending: Default::default(), cancels: Arc::clone(&cancels) };
    let mut engine = engine_with(client);
    // seed an open +2 position (venue, symbol, position_side)
    engine.account.positions.insert(
        ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
        vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
    );
    let handle = spawn_core(engine, CoreConfig::default());
    let cell = handle.snapshot_cell();

    handle.try_command(Command::Order(OrderIntent::Submit(gtd_request("m1", i64::MAX)))).unwrap();
    wait_until(&cell, 5, "order accepted", |s| {
        s.order("m1").map(|o| o.status) == Some(OrderStatus::Accepted)
    });

    handle.try_command(Command::Order(OrderIntent::MarketExit { venue: None })).unwrap();
    wait_until(&cell, 5, "resting order canceled", |s| {
        s.order("m1").map(|o| o.status) == Some(OrderStatus::Canceled)
    });
    wait_until(&cell, 5, "flatten order registered", |s| {
        s.orders.iter().any(|o| o.client_order_id != "m1" && o.symbol == "BTCUSDT")
    });

    let snap = cell.load_full();
    let flat = snap.orders.iter().find(|o| o.client_order_id != "m1").expect("flatten leg");
    assert_eq!((flat.side, flat.qty, flat.order_type.as_str()), (-1, 2.0, "market"));
    handle.shutdown_and_join();
}

/// PERIODIC PORTFOLIO SNAPSHOT: with the cadence configured (and a journal to write into), the
/// core appends `PortfolioSnap` observation records carrying the per-venue equity row — and
/// appends NONE when the cadence is left unset, which is the default.
#[test]
fn portfolio_snapshot_cadence_writes_records_only_when_configured() {
    for (label, interval, want_records) in
        [("on", Some(Duration::from_millis(20)), true), ("off", None, false)]
    {
        let dir = crate::scratch::Scratch::reserved(&format!("portfolio-{label}"));
        let mut engine = engine_with(RestingClient::default());
        engine.account.positions.insert(
            ("sim".into(), "BTCUSDT".into(), "BOTH".into()),
            vike_exec::PositionEntry { size: 2.0, avg_px: 100.0, ..Default::default() },
        );
        let cfg = CoreConfig {
            seed_cash: 1_000.0,
            journal: Some(JournalConfig::at(dir.path())),
            portfolio_snapshot_interval: interval,
            ..CoreConfig::default()
        };
        let handle = spawn_core(engine, cfg);
        std::thread::sleep(Duration::from_millis(200));
        handle.shutdown_and_join();

        let records = vike_core::journal::CommandJournal::read_all(&dir).expect("read journal");
        let snaps: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                JournalRecord::PortfolioSnap { sample, .. } => Some(sample),
                _ => None,
            })
            .collect();
        if want_records {
            assert!(!snaps.is_empty(), "cadence on: expected PortfolioSnap records");
            let s = snaps[0];
            assert_eq!(s.venues.len(), 1);
            assert_eq!(s.venues[0].venue, "sim");
            assert_eq!(s.positions.len(), 1, "the one OPEN position is carried");
            assert_eq!((s.positions[0].symbol.as_str(), s.positions[0].size), ("BTCUSDT", 2.0));
        } else {
            assert!(snaps.is_empty(), "cadence off: no PortfolioSnap record may be written");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
