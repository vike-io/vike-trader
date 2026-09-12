//! R5(b) runtime gates: the headless full path (submit → accept → fill → fold → snapshot)
//! against the SAME golden the R5(a) engine passed, plus the channel-contract behaviors —
//! lossless exec lane, latest-wins market conflation, surfaced command rejection, and panic
//! safe-state.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use vike_core::{CommandRejected, CoreConfig, spawn_core};
use vike_exec::testing::{RecordingClient, TestExecutionClient};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionEngine, MarketTick, OrderIntent, ReconcileSnapshot,
    RiskGate, RiskLimits, TradingState,
};
use vike_model::events::{Event, FillEvent, TradeId};
use vike_model::{OrderRequest, f64_to_hex_bits};

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r5").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn assert_bits(actual: f64, expected_hex: &serde_json::Value, what: &str) {
    assert_eq!(f64_to_hex_bits(actual), expected_hex.as_str().unwrap(), "{what}: got {actual}");
}

fn fill(tid: &str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // `&str` (not `&'static str`) because one caller mints `w{w}-{i}` at runtime — `TradeId::new`
        // is the wire constructor and `expect` is honest here: every caller passes a non-empty id.
        trade_id: TradeId::new(tid).expect("test trade ids are non-empty"),
        client_order_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 0,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

fn request(coid: &str, side: i32, qty: f64, price: Option<f64>) -> Box<OrderRequest> {
    Box::new(
        serde_json::from_value(serde_json::json!({
            "client_order_id": coid, "venue": "sim", "symbol": "BTCUSDT",
            "side": side, "qty": qty,
            "order_type": if price.is_some() { "limit" } else { "market" },
            "price": price
        }))
        .unwrap(),
    )
}

fn test_config(seed_cash: f64) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        ..CoreConfig::default()
    }
}

/// The R5 headless gate: replay the r5a `sim_roundtrip` golden THROUGH the runtime
/// (channels + core thread + coalesced snapshot) and require the identical end state
/// the in-process engine produced — proving the runtime plumbing preserves R5a semantics.
#[test]
fn headless_full_path_matches_r5a_golden() {
    let fx = fixture("hub.json");
    let sc = fx["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "sim_roundtrip")
        .expect("sim_roundtrip scenario");
    let venue = sc["venue"].as_str().unwrap();
    let engine = ExecutionEngine::new(
        Account::new(sc["multiplier"].as_f64().unwrap(), venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new(venue, sc["sim_mark"].as_f64().unwrap()),
        venue,
        sc["symbol"].as_str().unwrap(),
    );
    let handle = spawn_core(engine, test_config(sc["seed_cash"].as_f64().unwrap()));
    let cell = handle.snapshot_cell();

    for act in sc["actions"].as_array().unwrap() {
        match act["do"].as_str().unwrap() {
            "submit" => {
                let req: OrderRequest = serde_json::from_value(act["request"].clone()).unwrap();
                handle.try_command(Command::Order(OrderIntent::Submit(Box::new(req)))).unwrap();
            }
            "cancel" => handle
                .try_command(Command::Order(OrderIntent::Cancel(
                    act["coid"].as_str().unwrap().to_string(),
                )))
                .unwrap(),
            other => panic!("unexpected action {other}"),
        }
    }
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let e = &sc["expect"];
    assert_bits(snap.balance, &e["balance"], "balance");
    assert_bits(snap.portfolio.realized_pnl, &e["realized_pnl"], "realized_pnl");
    assert_bits(snap.portfolio.fees_paid, &e["fees_paid"], "fees_paid");
    assert_bits(snap.portfolio.funding_paid, &e["funding_paid"], "funding_paid");
    assert_bits(snap.portfolio.equity, &e["equity_all"], "equity");
    let mode = match snap.balance_mode {
        BalanceMode::Delta => "delta",
        BalanceMode::Authoritative => "authoritative",
    };
    assert_eq!(mode, e["balance_mode"].as_str().unwrap());

    let want_pos = e["positions"].as_array().unwrap();
    assert_eq!(snap.positions.len(), want_pos.len());
    for (p, w) in snap.positions.iter().zip(want_pos) {
        assert_eq!(p.venue, w[0].as_str().unwrap());
        assert_eq!(p.symbol, w[1].as_str().unwrap());
        assert_eq!(p.position_side, w[2].as_str().unwrap());
        assert_bits(p.size, &w[3], "pos size");
        assert_bits(p.avg_px, &w[4], "pos avg");
    }
    let want_marks = e["marks"].as_array().unwrap();
    assert_eq!(snap.marks.len(), want_marks.len());
    for ((v, s, px), w) in snap.marks.iter().zip(want_marks) {
        assert_eq!(v, w[0].as_str().unwrap());
        assert_eq!(s, w[1].as_str().unwrap());
        assert_bits(*px, &w[2], "mark px");
    }
    let want_reg = e["registry"].as_array().unwrap();
    assert_eq!(snap.orders.len(), want_reg.len());
    for (o, w) in snap.orders.iter().zip(want_reg) {
        assert_eq!(o.client_order_id, w[0].as_str().unwrap());
        assert_eq!(o.status.as_str(), w[1].as_str().unwrap());
        assert_bits(o.filled_qty, &w[2], "filled_qty");
        assert_bits(o.avg_fill_px, &w[3], "avg_fill_px");
        assert_eq!(o.venue_order_id.as_deref(), w[4].as_str());
    }
    // delivery order survives the runtime: recent_events kinds == the Python delivered list
    let want_delivered: Vec<&str> =
        e["delivered"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    let got_kinds: Vec<String> = snap
        .recent_events
        .iter()
        .map(|s| s.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(got_kinds, want_delivered, "delivery order through the runtime");
    assert!(snap.fault.is_none());
    assert_eq!(snap.rejected_commands, 0);
}

/// Lossless exec-event lane: 4 producer threads × 1250 unique fills, each sent TWICE
/// (reconnect replay) — every unique fill folds exactly once, no drops, no fault.
#[test]
fn lossless_event_lane_with_replays() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();

    let mut workers = Vec::new();
    for w in 0..4 {
        let sender = handle.event_sender();
        workers.push(std::thread::spawn(move || {
            for i in 0..1250u32 {
                let f = fill(&format!("w{w}-{i}"), 1.0, 100.0);
                sender.blocking_send(Event::Fill(f.clone())).unwrap();
                sender.blocking_send(Event::Fill(f)).unwrap(); // replay
            }
        }));
    }
    for w in workers {
        w.join().unwrap();
    }
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none());
    assert_eq!(snap.positions.len(), 1);
    assert_eq!(snap.positions[0].size, 5000.0, "each unique fill folded exactly once");
    assert_eq!(snap.positions[0].avg_px, 100.0);
}

/// Market lane: latest-wins conflation — the final tick always lands, intermediate ticks
/// may conflate (counted, surfaced), and the queue never grows with market data.
#[test]
fn market_conflation_latest_wins() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();
    let market = handle.market_sender();

    for i in 0..10_000u32 {
        market.publish(MarketTick {
            venue: "sim".into(),
            symbol: "BTCUSDT".into(),
            px: f64::from(i),
            ts: i64::from(i),
        });
    }
    market.publish(MarketTick {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        px: 424_242.0,
        ts: 10_000,
    });
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.marks.len(), 1);
    assert_eq!(snap.marks[0].2, 424_242.0, "final tick must never be lost");
    assert!(
        snap.conflated_market_drops > 0,
        "10k rapid ticks must conflate (drops surfaced, not silent)"
    );
}

/// FIFO order survives repeated idle → busy transitions: 32 submits are sent ONE AT A TIME, each
/// only after the previous has folded, so the core drains to idle — parking on `blocking_recv` —
/// between every single one. The registry (insertion-ordered) must then show o0..o31 in exactly
/// submission order, proving the drain loop pulls the first-queued message and never reorders
/// across parks.
#[test]
fn fifo_order_preserved_across_idle_transitions() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();

    const N: usize = 32;
    for i in 0..N {
        let coid = format!("o{i}");
        handle
            .try_command(Command::Order(OrderIntent::Submit(request(&coid, 1, 1.0, Some(100.0)))))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cell.load().orders.iter().any(|o| o.client_order_id == coid) {
            assert!(Instant::now() < deadline, "submit {coid} never folded");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), N, "every idle-delivered submit folded exactly once");
    for (i, o) in snap.orders.iter().enumerate() {
        assert_eq!(o.client_order_id, format!("o{i}"), "FIFO order preserved across idle parks");
    }
    assert!(snap.fault.is_none());
}

/// Panic policy: an injected handler panic must NOT kill the core — it enters safe-state
/// (HALTED + fault surfaced), subsequent orders are denied, and shutdown stays clean.
#[test]
fn fault_injection_enters_safe_state() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let mut cfg = test_config(0.0);
    cfg.panic_on_trade_id = Some("boom".into());
    let handle = spawn_core(engine, cfg);
    let cell = handle.snapshot_cell();
    let sender = handle.event_sender();

    handle
        .try_command(Command::Order(OrderIntent::Submit(request("o1", 1, 1.0, Some(100.0)))))
        .unwrap();
    sender.blocking_send(Event::Fill(fill("boom", 1.0, 100.0))).unwrap();
    // wait until the fault is visible, then prove the gate now denies everything
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while cell.load().fault.is_none() {
        assert!(std::time::Instant::now() < deadline, "fault never surfaced");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(handle.is_alive(), "core must survive the panic");
    handle
        .try_command(Command::Order(OrderIntent::Submit(request("o2", 1, 1.0, Some(100.0)))))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.trading_state, TradingState::Halted);
    assert!(snap.fault.as_deref().unwrap().contains("injected fault"));
    assert_eq!(snap.orders.len(), 1, "post-fault submit must be denied, not registered");
    assert_eq!(snap.orders[0].client_order_id, "o1");
    assert!(
        snap.recent_events.iter().any(|e| e.starts_with("OrderDenied o2")),
        "denial surfaced in the journal feed: {:?}",
        snap.recent_events
    );
    // the poisoned fill never folded (panic fired before dispatch touched the account)
    assert!(snap.positions.is_empty());
}

/// Audit C3 end-to-end: an adapter that accepts `submit` but never acks (RecordingClient emits
/// nothing) leaves the order stuck pre-ack; with the opt-in watchdog enabled, the timer thread
/// fires `Ingest::Watchdog` → sweep → (soft-warn, then after the confirm-grace) synthesized
/// `OrderRejected`, so the order is terminalized rather than stranded forever. Uses the DEFAULT
/// LiveClock so real wall-time elapses; the confirm-grace is set small so the whole ladder
/// (timeout + grace) completes well inside the wait deadline.
#[test]
fn watchdog_terminalizes_stuck_order_end_to_end() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let cfg = CoreConfig {
        submit_ack_timeout: Some(Duration::from_millis(50)),
        // small confirm-grace so the ladder terminalizes quickly in the test (the default 5s would
        // otherwise push the reject past the wait deadline below)
        submit_ack_confirm_grace: Duration::from_millis(50),
        ..CoreConfig::default() // default clock = LiveClock (real wall time)
    };
    let handle = spawn_core(engine, cfg);
    let cell = handle.snapshot_cell();

    handle
        .try_command(Command::Order(OrderIntent::Submit(request("w1", 1, 1.0, Some(100.0)))))
        .unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snap = cell.load_full();
        if snap
            .orders
            .iter()
            .any(|o| o.client_order_id == "w1" && o.status == OrderStatus::Rejected)
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "watchdog never terminalized the stuck order"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "watchdog is a backstop, not a fault");
    assert!(
        snap.recent_events.iter().any(|e| e.starts_with("OrderRejected w1")),
        "reject surfaced in the journal feed: {:?}",
        snap.recent_events
    );
}

/// Audit exec#2 drift DETECTION: a reconcile snapshot whose venue truth DIVERGES from the
/// locally-folded Account (here a 1.0 → 2.0 position-size gap) surfaces a `DRIFT` warning in the
/// GUI-visible recent-events ring (the margin-call watchdog's channel), while venue truth STILL
/// wins the seed — apply_snapshot's overwrite is unchanged, we only ADDED the alert. Local state
/// is pre-folded before spawn so the diff is deterministic (no event/command ordering race).
#[test]
fn reconcile_drift_surfaces_in_recent_events() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    engine.account.apply_fill(&fill("seed", 1.0, 100.0)); // local BTCUSDT[BOTH] leg = 1.0
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();

    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 2.0)], // venue truth: 2.0 (drift vs local 1.0)
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default()
    };
    handle.try_command(Command::ApplySnapshot(Box::new(snap))).unwrap();
    handle.shutdown_and_join();

    let s = cell.load_full();
    assert!(
        s.recent_events.iter().any(|e| e.starts_with("DRIFT position") && e.contains("BTCUSDT")),
        "position drift must surface in the ring: {:?}",
        s.recent_events
    );
    // venue truth still wins the seed — detection did NOT change the overwrite
    let pos = s.positions.iter().find(|p| p.symbol == "BTCUSDT").expect("BTCUSDT position seeded");
    assert_eq!(pos.size, 2.0, "apply_snapshot overwrite unchanged: venue truth wins");
}

/// Twin of the above: a reconcile snapshot that MATCHES the locally-folded Account surfaces NO
/// drift warning — the tolerance gate must not cry wolf on an in-sync reconcile (every startup +
/// WS-reconnect resync would otherwise spam the ring).
#[test]
fn reconcile_match_surfaces_no_drift() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    engine.account.apply_fill(&fill("seed", 1.0, 100.0)); // local BTCUSDT[BOTH] leg = 1.0
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();

    let snap = ReconcileSnapshot {
        positions: vec![("BTCUSDT".to_string(), 1.0)], // matches local exactly → no drift
        position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
        position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
        ..Default::default() // balance 0.0 → not diffed; no open orders either side
    };
    handle.try_command(Command::ApplySnapshot(Box::new(snap))).unwrap();
    handle.shutdown_and_join();

    let s = cell.load_full();
    assert!(
        !s.recent_events.iter().any(|e| e.starts_with("DRIFT")),
        "matching snapshot must not surface drift: {:?}",
        s.recent_events
    );
}

/// Audit exec#2 CONTINUOUS cadence: the opt-in `CoreHandle::spawn_periodic_reconcile` driver
/// re-issues `Command::ApplySnapshot` on an interval, so a venue-vs-local divergence surfaces as a
/// `DRIFT` line WITHOUT the caller hand-delivering a snapshot — the reconnect/periodic follow-up to
/// the startup-only reconcile. `fetch_snapshot` runs on the DRIVER thread (never the fold), proven
/// by the invocation counter. Venue truth still wins the seed (apply_snapshot overwrite unchanged).
#[test]
fn periodic_reconcile_driver_surfaces_drift() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    engine.account.apply_fill(&fill("seed", 1.0, 100.0)); // local BTCUSDT[BOTH] leg = 1.0
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();

    // Every tick the driver "fetches" venue truth of 2.0 — a deterministic drift vs the local 1.0.
    let fetches = Arc::new(AtomicUsize::new(0));
    let fetches_t = Arc::clone(&fetches);
    let driver = handle.spawn_periodic_reconcile(Duration::from_millis(5), move || {
        fetches_t.fetch_add(1, Ordering::Relaxed);
        Some(ReconcileSnapshot {
            positions: vec![("BTCUSDT".to_string(), 2.0)],
            position_sides: vec![("BTCUSDT".to_string(), "BOTH".to_string())],
            position_avg_px: vec![("BTCUSDT".to_string(), 100.0)],
            ..Default::default()
        })
    });

    // Poll until the driver-delivered ApplySnapshot reaches diff_snapshot and the DRIFT line lands
    // in the GUI-visible ring — proving the trigger, the command lane, and the diff are all wired.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let s = cell.load_full();
        if s.recent_events.iter().any(|e| e.starts_with("DRIFT position") && e.contains("BTCUSDT"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "periodic reconcile never surfaced drift: {:?}",
            cell.load().recent_events
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(fetches.load(Ordering::Relaxed) > 0, "fetch_snapshot ran on the driver thread");

    driver.shutdown();
    handle.shutdown_and_join();

    // Venue truth STILL wins the seed — the continuous cadence added detection, not a new overwrite.
    let pos = cell.load().positions.iter().find(|p| p.symbol == "BTCUSDT").map(|p| p.size);
    assert_eq!(pos, Some(2.0), "apply_snapshot overwrite unchanged: venue truth wins");
}

/// The reconcile driver is bounded + self-cleaning: it holds a WEAK ingest sender (the audit-C3
/// watchdog contract), so dropping the `CoreHandle` (last strong sender gone → the core's clean
/// exit) makes the driver self-exit on its own — no stop signal, never keeps the core alive.
#[test]
fn periodic_reconcile_driver_self_exits_when_core_gone() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        TestExecutionClient::new("sim", 100.0),
        "sim",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, test_config(0.0));
    let driver = handle
        .spawn_periodic_reconcile(Duration::from_millis(5), || Some(ReconcileSnapshot::default()));

    // Core gone: dropping the handle drops the last strong ingest sender.
    drop(handle);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !driver.is_finished() {
        assert!(Instant::now() < deadline, "driver did not self-exit when the core was gone");
        std::thread::sleep(Duration::from_millis(5));
    }
    driver.shutdown(); // idempotent join of the already-finished thread
}

/// GUI command path: a full ingest queue rejects with `Busy` — returned to the caller AND
/// counted in the snapshot. Never silent, never blocking.
#[test]
fn command_rejection_is_surfaced() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let mut cfg = test_config(0.0);
    cfg.ingest_capacity = 1;
    // stall the core inside the first Submit dispatch so the queue backs up
    let first = std::sync::atomic::AtomicBool::new(true);
    cfg.clock = Box::new(move || {
        if first.swap(false, Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(300));
        }
        0
    });
    let handle = spawn_core(engine, cfg);
    let cell = handle.snapshot_cell();

    handle
        .try_command(Command::Order(OrderIntent::Submit(request("s1", 1, 1.0, Some(100.0)))))
        .unwrap();
    let mut saw_busy = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !saw_busy {
        assert!(std::time::Instant::now() < deadline, "never saw Busy");
        match handle.try_command(Command::Order(OrderIntent::Submit(request(
            "s2",
            1,
            1.0,
            Some(100.0),
        )))) {
            Err(CommandRejected::Busy) => saw_busy = true,
            Ok(()) | Err(CommandRejected::Gone) => std::thread::sleep(Duration::from_micros(50)),
        }
    }
    handle.shutdown_and_join();
    assert!(cell.load().rejected_commands >= 1, "rejections must be surfaced in the snapshot");
}

/// The venue-side command vocabulary of the exec-actor double below —
/// `crates/vike-bridge-core/src/exec_actor.rs`'s `ExecCommand` in miniature.
enum ActorCmd {
    /// One venue command's worth of events, emitted on the ACTOR's own thread. Stands in for
    /// `run_loop`'s gap-sentinel arm (`for ev in resync() { events.blocking_send(ev) }`), which
    /// replays a window of venue history through the same lossless send.
    Burst,
    /// The operator's cancel — the command whose turnaround this test measures.
    Cancel,
    Shutdown,
}

/// One market-data message for the flood below, on the `(venue, symbol)` the core is mounted on.
/// The book rides behind the `Arc` the real L2 pumps hand over, so the flood is a refcount bump
/// per message rather than a book copy — the shape `BookUpdate` documents.
fn sim_book_update(book: &Arc<L2Book>) -> BookUpdate {
    BookUpdate { venue: "sim".into(), symbol: "BTCUSDT".into(), book: Arc::clone(book) }
}

/// CHARACTERIZATION (this REVEALS what the shared lane costs; it attempts no cure). The vike-core
/// ingest lane is ONE FIFO shared by every producer: `spawn_core` creates a single
/// `mpsc::channel::<Ingest>(config.ingest_capacity)` and hands clones to the market, bar, tick,
/// event and command senders alike, and `CoreThread::run` drains it strictly in arrival order —
/// there is no lane priority anywhere in that loop. `command_rejection_is_surfaced` above is the
/// only other test that saturates the bound, and it both fills AND asserts on the COMMAND lane;
/// the three `runtime_latency.rs` harnesses each drive exactly ONE lane. So nothing measured what
/// a MARKET-DATA producer does to a CONTROL producer.
///
/// This test puts two producers on the one lane and measures the two control paths that matter:
///
///   (a) an EXEC-ACTOR-shaped venue thread. `crates/vike-bridge-core/src/exec_actor.rs`'s
///       `run_loop` calls `events.blocking_send(ev)` on the SAME thread that then re-enters
///       `rx.recv_timeout` for the next `ExecCommand` — so while that send waits for lane space,
///       a pending Cancel is not even READ, let alone sent to the venue. The double below
///       reproduces exactly that shape (a `std::sync::mpsc` command channel, a `blocking_send`
///       burst per command, the next `recv_timeout` strictly after it). It is reproduced rather
///       than imported because vike-core has no dev-dependency on vike-bridge-core and gaining
///       one is a manifest edit, not a test.
///
///   (b) the LOSSLESS operator path, `CommandSink::send_blocking` — the one `vike-tradehub`'s
///       stdio loop uses. Its contract is that a full lane makes it WAIT and then fold, never
///       drop; the `SetTradingState` it carries is observable in the final snapshot, so the
///       delivery is proved rather than assumed.
///
/// ⚠ `CoreHandle::try_command` deliberately gets NO admission assertion here.
/// `CommandRejected::Busy` on a full lane is the DOCUMENTED contract for the network-peer path
/// (`crates/vike-tradehub/src/server.rs`'s `accept_command`, whose `Busy` arm becomes
/// `AcceptError::Busy` because a remote peer must never back-pressure a per-connection thread),
/// not a defect — `command_rejection_is_surfaced` above pins it,
/// and this test uses it only as the SATURATION WITNESS that keeps the two measurements from
/// being vacuous.
///
/// THE STATED BOUND. `ingest_capacity` is 4 and every folded message costs `FOLD_STALL`, so the
/// arithmetic a fair FIFO permit queue implies is a handful of stalls — single-digit milliseconds.
/// `BOUND` is three orders of magnitude above that, so a red here is a genuine unbounded stall or
/// a deadlock, never runner jitter. If a reserved control lane is ever added to `CoreThread::run`
/// (HOT-FOLD work gated by the latency gate — it belongs on its own branch, not here), these
/// numbers should collapse toward one fold stall and this test still passes; tighten the bound in
/// that PR rather than deleting the measurement.
#[test]
fn two_producers_share_one_ingest_fifo_under_a_market_flood() {
    // Wall time one folded message costs. `CoreThread::dispatch` reads the injected clock ONCE
    // per message, so a sleeping clock IS a per-message fold stall — the same seam
    // `command_rejection_is_surfaced` uses, held open for the whole test instead of one dispatch.
    const FOLD_STALL: Duration = Duration::from_millis(1);
    // Events one venue command produces on the actor thread (see `ActorCmd::Burst`).
    const BURST: usize = 32;
    // The stated bound — see the test doc. Also the hang guard: every wait below is deadlined on
    // it, so a genuine deadlock fails the test instead of wedging the run.
    const BOUND: Duration = Duration::from_secs(5);

    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    );
    let mut cfg = test_config(0.0);
    cfg.ingest_capacity = 4;
    cfg.clock = Box::new(|| {
        std::thread::sleep(FOLD_STALL);
        0
    });
    let handle = spawn_core(engine, cfg);
    let cell = handle.snapshot_cell();

    // ---- producer 1: the market-data flood, on the LOSSLESS L2 lane ----
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flood = Arc::clone(&stop);
    let ticks = handle.tick_sender();
    let flood = std::thread::spawn(move || {
        let mut b = L2Book::new(0.01);
        b.apply_snapshot(1, &[(100.0, 5.0)], &[(101.0, 5.0)]);
        let b = Arc::new(b);
        let mut sent = 0usize;
        while !stop_flood.load(Ordering::Relaxed) {
            if ticks.book(sim_book_update(&b)).is_err() {
                break;
            }
            sent += 1;
        }
        sent
    });

    // ---- producer 2: the exec-actor-shaped venue command thread ----
    let (actor_tx, actor_rx) = std::sync::mpsc::channel::<ActorCmd>();
    let events = handle.event_sender();
    let bursting = Arc::new(AtomicBool::new(false));
    let cancel_issued = Arc::new(AtomicBool::new(false));
    let bursting_t = Arc::clone(&bursting);
    let cancel_t = Arc::clone(&cancel_issued);
    let actor = std::thread::spawn(move || {
        // `run_loop`'s shape verbatim: take the next command, then push that command's events
        // through the shared lane ON THIS THREAD, and only afterwards look for the next command.
        while let Ok(cmd) = actor_rx.recv_timeout(Duration::from_secs(30)) {
            match cmd {
                ActorCmd::Burst => {
                    bursting_t.store(true, Ordering::Relaxed);
                    for i in 0..BURST {
                        // an unknown coid: the venue-history shape the restart law describes, and
                        // it folds without touching the account (no margin/liquidation lane can
                        // move underneath the measurement).
                        let ev = Event::OrderCanceled(OrderCanceled {
                            client_order_id: format!("resync-{i}"),
                            reason: "resync".into(),
                            ts: 0,
                        });
                        if events.blocking_send(ev).is_err() {
                            return;
                        }
                    }
                }
                // The venue REST cancel would go out HERE — reaching this line IS the turnaround
                // an operator waiting on a pulled quote is paying for.
                ActorCmd::Cancel => cancel_t.store(true, Ordering::Relaxed),
                ActorCmd::Shutdown => return,
            }
        }
    });

    // ---- the saturation witness ----
    let saturate_by = Instant::now() + BOUND;
    loop {
        match handle.try_command(Command::Order(OrderIntent::Cancel("no-such-order".into()))) {
            Err(CommandRejected::Busy) => break,
            Err(CommandRejected::Gone) => panic!("the core exited before the lane saturated"),
            Ok(()) => {
                assert!(
                    Instant::now() < saturate_by,
                    "the market flood never filled ingest_capacity=4 — both measurements below \
                     would be vacuous"
                );
                std::thread::sleep(Duration::from_micros(200));
            }
        }
    }

    // ---- (a) the exec actor's cancel turnaround ----
    actor_tx.send(ActorCmd::Burst).expect("actor thread alive");
    let burst_by = Instant::now() + BOUND;
    while !bursting.load(Ordering::Relaxed) {
        assert!(Instant::now() < burst_by, "the exec actor never started its event burst");
        std::thread::sleep(Duration::from_micros(200));
    }
    let cancel_sent = Instant::now();
    actor_tx.send(ActorCmd::Cancel).expect("actor thread alive");
    while !cancel_issued.load(Ordering::Relaxed) {
        assert!(
            cancel_sent.elapsed() < BOUND,
            "(a) the exec actor had still not READ its Cancel after {:?} — its command thread is \
             parked inside `events.blocking_send` on the shared ingest lane behind a market flood",
            cancel_sent.elapsed()
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    let cancel_turnaround = cancel_sent.elapsed();
    // ⚠ REPORTED, not merely bounded, and that distinction earned its keep. `BOUND` is a 5 s hang
    // guard nobody chose as a budget, so this test could prove the head-of-line MECHANISM while
    // saying nothing about its MAGNITUDE — and magnitude is the only thing that decides whether a
    // cure is worth touching a `p99 < 10us` fold.
    //
    // Printing them refuted the cure that was planned off this test. (a) reads ~68-99 ms and looks
    // like a command waiting behind market data; it is not. It is the exec ACTOR blocked in
    // `events.blocking_send` on the shared lane — EVENT back-pressure. A reserved control lane for
    // commands was built, measured, and moved this number by nothing (99.1 ms -> 96.1 ms) while
    // making (b) about a hundred times faster, because (b) is the metric that was actually about
    // commands. Whatever eventually addresses (a) has to target the actor's event send.
    println!("MEASURED cancel turnaround under flood: {cancel_turnaround:?}");

    // ---- (b) admission on the LOSSLESS control lane ----
    // Timed on its own thread so a lane that never grants a permit fails the deadline below
    // instead of wedging the test run in an untimed `blocking_send`.
    let sink = handle.command_sink();
    let admitted: Arc<Mutex<Option<Duration>>> = Arc::new(Mutex::new(None));
    let admitted_t = Arc::clone(&admitted);
    let control = std::thread::spawn(move || {
        let t = Instant::now();
        sink.send_blocking(Command::SetTradingState(TradingState::Reducing));
        *admitted_t.lock().unwrap() = Some(t.elapsed());
    });
    let admit_by = Instant::now() + BOUND;
    loop {
        let done = admitted.lock().unwrap().is_some();
        if done {
            break;
        }
        assert!(
            Instant::now() < admit_by,
            "(b) `CommandSink::send_blocking` had not been admitted within {BOUND:?} — the \
             lossless operator path is starved by the market flood on the shared lane"
        );
        std::thread::sleep(Duration::from_micros(200));
    }
    let control_admission = (*admitted.lock().unwrap()).expect("just observed Some");
    println!("MEASURED lossless control-lane admission under flood: {control_admission:?}");

    stop.store(true, Ordering::Relaxed);
    let flooded = flood.join().expect("flood thread");
    let _ = actor_tx.send(ActorCmd::Shutdown);
    actor.join().expect("actor thread");
    control.join().expect("control thread");
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "saturation is back-pressure, not a fault: {:?}", snap.fault);
    assert!(flooded > 0, "the flood produced nothing to saturate the lane with");
    assert_eq!(
        snap.trading_state,
        TradingState::Reducing,
        "(b) `CommandSink::send_blocking` is the LOSSLESS path: a full lane must make it WAIT and \
         then FOLD, never drop the command"
    );
    // The two measurements, against the one stated bound (each wait loop above is deadlined on it
    // too, so these restate the pin where a reader will look for it).
    assert!(
        cancel_turnaround < BOUND,
        "(a) exec-actor cancel turnaround {cancel_turnaround:?} exceeded the bound {BOUND:?}"
    );
    assert!(
        control_admission < BOUND,
        "(b) control-lane admission {control_admission:?} exceeded the bound {BOUND:?}"
    );
}

fn bar(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> vike_model::Bar {
    vike_model::Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: v,
        funding: None,
        bid: None,
        ask: None,
        symbol: None, // series key carries the symbol
    }
}

/// R5c bar lanes: REST seed replaces the series; closed bars append losslessly (with
/// idempotent re-close on reconnect overlap and stale-replay drops); forming updates
/// conflate latest-wins and never regress behind a close.
#[test]
fn bar_lanes_seed_close_forming() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let handle = spawn_core(engine, test_config(0.0));
    let cell = handle.snapshot_cell();
    let bars = handle.bar_sender();
    let market = handle.market_sender();
    let key = || ("binance".into(), "BTCUSDT".into(), "1m".into());

    // seed 3 closed bars
    bars.seed(vike_exec::BarSeed {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bars: (0..3).map(|i| bar(60_000 * i, 100.0, 101.0, 99.0, 100.5, 10.0)).collect(),
    })
    .unwrap();
    // forming updates for the NEXT window — 500 rapid ones conflate, last wins
    for i in 0..500u32 {
        market.publish_forming(
            "binance",
            "BTCUSDT",
            "1m",
            bar(180_000, 100.5, 100.5 + f64::from(i), 100.0, 100.5 + f64::from(i), 1.0),
        );
    }
    // that window closes (lossless lane), then closes AGAIN via reconnect replay
    let closed = bar(180_000, 100.5, 101.5, 100.0, 101.0, 42.0);
    bars.close(vike_exec::BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: closed.clone(),
    })
    .unwrap();
    bars.close(vike_exec::BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: closed.clone(),
    })
    .unwrap(); // idempotent re-close
    bars.close(vike_exec::BarUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: bar(120_000, 1.0, 1.0, 1.0, 1.0, 1.0),
    })
    .unwrap(); // stale replay — must be dropped
    handle.shutdown_and_join();

    let snap = cell.load_full();
    let series = snap.bars.get(&key()).expect("series present");
    assert_eq!(series.closed.len(), 4, "3 seeded + 1 closed (re-close idempotent, stale dropped)");
    assert_eq!(series.closed[3].ts, 180_000);
    assert_eq!(series.closed[3].volume, 42.0);
    assert_eq!(series.closed[2].ts, 120_000, "stale replay must not overwrite");
    assert!(series.forming.is_none(), "the close supersedes the forming state of that window");
    assert!(snap.conflated_market_drops > 0, "forming updates must conflate");
}

// ---- Piece 3: live tick / L2 dispatch ----

use vike_core::{LiveBroker, StrategyMount};
use vike_exec::{BookUpdate, QuoteUpdate, TradeUpdate};
use vike_model::{Broker, L2Book, QuoteTick, Strategy, TradeTick};

/// Counts each tick handler and submits a market order from it — proving both that the live
/// runtime dispatches the sub-bar lanes AND that a submission made inside a tick handler routes
/// through the one live path (mint → RiskGate → engine).
#[derive(Default)]
struct TickCounter {
    quotes: Arc<AtomicUsize>,
    trades: Arc<AtomicUsize>,
    books: Arc<AtomicUsize>,
}

impl Strategy<LiveBroker> for TickCounter {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
    fn on_trade_tick(&mut self, broker: &mut LiveBroker, _t: &TradeTick) {
        self.trades.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
    fn on_order_book(&mut self, broker: &mut LiveBroker, _b: &L2Book) {
        self.books.fetch_add(1, Ordering::Relaxed);
        broker.submit_market("BTCUSDT", 1, 1.0);
    }
}

#[test]
fn tick_lanes_dispatch_and_route_orders() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let quotes = Arc::<AtomicUsize>::default();
    let trades = Arc::<AtomicUsize>::default();
    let books = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(TickCounter {
            quotes: Arc::clone(&quotes),
            trades: Arc::clone(&trades),
            books: Arc::clone(&books),
        }),
    });
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    ticks
        .trade(TradeUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            trade: TradeTick {
                ts: 2,
                local_ts: 0,
                price: 100.1,
                size: 0.5,
                is_buyer_maker: false,
                symbol: String::new(),
            },
        })
        .unwrap();
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(1, &[(100.0, 5.0)], &[(101.0, 5.0)]); // two-sided → mid = 100.5
    ticks
        .book(BookUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            book: Arc::new(book),
        })
        .unwrap();

    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 1, "on_quote_tick fired once");
    assert_eq!(trades.load(Ordering::Relaxed), 1, "on_trade_tick fired once");
    assert_eq!(books.load(Ordering::Relaxed), 1, "on_order_book fired once");

    // each handler's market order routed through the live path into the engine registry
    let snap = cell.load();
    assert_eq!(snap.orders.len(), 3, "one routed market order per tick handler");
    assert!(snap.orders.iter().all(|o| o.order_type == "market" && o.side == 1 && o.qty == 1.0));
}

// ---- Piece 2: modify / cancel-replace (strategy-assigned tags) ----

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use vike_exec::{ExecutionClient, OrderStatus};
use vike_model::events::{OrderAccepted, OrderModified, OrderSubmitted};

/// Accepts every order (so it rests, modifiable) and echoes OrderModified on modify — enough to
/// drive the runtime's tag→coid resolution, engine.modify_order, and the FSM self-transition fold.
#[derive(Default)]
struct ModifiableClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for ModifiableClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.events.push_back(Event::OrderModified(OrderModified {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: None,
            new_qty,
            new_price,
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// Rests a tagged limit on the first quote, then re-prices/-sizes it by tag on later quotes —
/// never seeing the client_order_id (the runtime mints and tracks it under the tag).
struct TagModifier {
    submitted: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for TagModifier {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if self.submitted.swap(true, Ordering::Relaxed) {
            broker.modify("bid", Some(2.0), Some(99.0)); // 2nd+ quote: re-quote by tag
        } else {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0); // 1st quote: rest a tagged order
        }
    }
}

#[test]
fn modify_by_tag_reprices_resting_order_in_place() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ModifiableClient::default(),
        "binance",
        "BTCUSDT",
    );
    let submitted = Arc::<AtomicBool>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(TagModifier { submitted: Arc::clone(&submitted) }),
    });
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    let quote = |ts| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    ticks.quote(quote(1)).unwrap(); // submit tagged "bid" (qty 1 @ 100)
    ticks.quote(quote(2)).unwrap(); // modify "bid" -> qty 2 @ 99

    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "modified in place — NOT canceled + re-created");
    let o = &snap.orders[0];
    assert_eq!(o.status, OrderStatus::Accepted, "still resting after modify");
    assert_eq!(o.qty, 2.0, "qty modified by tag");
    assert_eq!(o.price, Some(99.0), "price modified by tag");
}

// ---- Piece 5: batch submit / mass-cancel ----

use vike_model::events::OrderCanceled;

/// Accepts every order and cancels on request — proves the default `submit_batch`/`cancel_batch`
/// fan-out produces one independent event stream per order.
#[derive(Default)]
struct BatchTestClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for BatchTestClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.events.push_back(Event::OrderCanceled(OrderCanceled {
            client_order_id: client_order_id.to_string(),
            reason: "batch".into(),
            ts: 0,
        }));
    }
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

fn limit(coid: &str, side: i32, qty: f64, px: f64) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side,
        qty,
        order_type: "limit".into(),
        price: Some(px),
        ..Default::default()
    }
}

fn batch_engine(limits: RiskLimits) -> ExecutionEngine<BatchTestClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(limits),
        BatchTestClient::default(),
        "binance",
        "BTCUSDT",
    )
}

#[test]
fn command_batch_submit_then_cancel_batch() {
    let handle = spawn_core(batch_engine(RiskLimits::new()), test_config(0.0));
    let cell = handle.snapshot_cell();
    handle
        .try_command(Command::Order(OrderIntent::SubmitBatch(vec![
            limit("b1", 1, 1.0, 100.0),
            limit("b2", 1, 1.0, 100.5),
            limit("b3", -1, 1.0, 101.0),
        ])))
        .unwrap();
    handle
        .try_command(Command::Order(OrderIntent::CancelBatch(vec!["b1".into(), "b3".into()])))
        .unwrap();
    handle.shutdown_and_join(); // lossless: both commands fold before join

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 3, "all three batch orders registered");
    let status =
        |coid: &str| snap.orders.iter().find(|o| o.client_order_id == coid).unwrap().status;
    assert_eq!(status("b1"), OrderStatus::Canceled);
    assert_eq!(status("b2"), OrderStatus::Accepted, "not in the cancel batch");
    assert_eq!(status("b3"), OrderStatus::Canceled);
}

#[test]
fn batch_submit_denies_over_cap_and_submits_the_rest() {
    let limits = RiskLimits { max_notional_per_order: Some(1_000.0), ..RiskLimits::new() };
    let handle = spawn_core(batch_engine(limits), test_config(0.0));
    let cell = handle.snapshot_cell();
    handle
        .try_command(Command::Order(OrderIntent::SubmitBatch(vec![
            limit("ok1", 1, 1.0, 100.0),   // notional 100 — ok
            limit("big", 1, 1.0e9, 100.0), // notional 1e11 — DENIED
            limit("ok2", 1, 2.0, 100.0),   // notional 200 — ok
        ])))
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2, "the over-cap order is denied, not registered");
    assert!(
        snap.orders.iter().all(|o| o.client_order_id != "big"),
        "denied order absent; the rest of the batch still submitted"
    );
}

/// Submits two tagged quotes on the first tick, then pulls ALL quotes via mass_cancel on the next.
struct QuoteThenPull {
    submitted: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for QuoteThenPull {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if self.submitted.swap(true, Ordering::Relaxed) {
            broker.mass_cancel();
        } else {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
            broker.submit_limit_tagged("ask", -1, 1.0, 101.0);
        }
    }
}

#[test]
fn mass_cancel_pulls_all_live_orders() {
    let submitted = Arc::<AtomicBool>::default();
    let mut config = test_config(0.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(QuoteThenPull { submitted: Arc::clone(&submitted) }),
    });
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let quote = |ts| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    ticks.quote(quote(1)).unwrap(); // submit bid + ask (both become Accepted)
    ticks.quote(quote(2)).unwrap(); // mass_cancel

    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2);
    assert!(
        snap.orders.iter().all(|o| o.status == OrderStatus::Canceled),
        "mass_cancel canceled every resting order"
    );
}

// ---- Piece 6: bracket / OCO ----

/// Submits one long bracket (limit entry + SL + TP) on the first quote.
struct BracketOnce {
    done: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for BracketOnce {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.done.swap(true, Ordering::Relaxed) {
            broker.submit_bracket(1, 2.0, Some(100.0), 95.0, 110.0);
        }
    }
}

#[test]
fn submit_bracket_sends_only_the_entry_holding_the_exits() {
    // Live-runtime OTO/OCO (submit-hold): a bracket sends ONLY the OTO entry to the venue; the
    // protective stop-loss / take-profit are HELD off the venue until the entry fills, so a naked
    // exit can never trigger before the position exists. (The entry-fill RELEASE and the OCO
    // cancel-sibling drive are pinned synchronously in the runtime `apply` tests — the same
    // `dispatch` fold this spawned thread runs.)
    let done = Arc::<AtomicBool>::default();
    let mut config = test_config(0.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(BracketOnce { done: Arc::clone(&done) }),
    });
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "only the OTO entry is live; the two exits are held");
    let entry = &snap.orders[0];
    assert_eq!(entry.side, 1, "the entry, not an exit");
    assert_eq!(entry.order_type, "limit");
    assert_eq!(entry.price, Some(100.0));
}

// ---- End-to-end: the SpreadMaker market-maker composes tick dispatch + tagged submit + modify ----

#[test]
fn spread_maker_quotes_then_modifies_in_place() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ModifiableClient::default(),
        "binance",
        "BTCUSDT",
    );
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(vike_mm::SpreadMaker::new(1.0, 0.5)),
    });
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let quote = |ts, bid, ask| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    // quote 1: mid 100.1 → rest bid @ 99.6, ask @ 100.6
    ticks.quote(quote(1, 100.0, 100.2)).unwrap();
    // quote 2: mid 101.1 → MODIFY bid → 100.6, ask → 101.6 (no cancel/re-submit)
    ticks.quote(quote(2, 101.0, 101.2)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 2, "exactly one resting bid + ask (modified, not re-created)");
    let bid = snap.orders.iter().find(|o| o.side == 1).expect("resting bid");
    let ask = snap.orders.iter().find(|o| o.side == -1).expect("resting ask");
    // both re-priced in place to the SECOND quote's mid (101.1) ± half_spread(0.5)
    assert!((bid.price.unwrap() - 100.6).abs() < 1e-6, "bid modified to new mid: {:?}", bid.price);
    assert!((ask.price.unwrap() - 101.6).abs() < 1e-6, "ask modified to new mid: {:?}", ask.price);
    assert_eq!(bid.status, OrderStatus::Accepted, "still resting");
    assert_eq!(ask.status, OrderStatus::Accepted);
}

/// End-to-end: the per-side fill-rate breaker (audit mm1) PULLS the over-hit side through the real
/// runtime. A SpreadMaker rests both quotes, then a run of same-side (bid/buy) fills with no
/// offsetting asks nets past the threshold → the next quote cancels the bid (venue emits
/// OrderCanceled) while the ask keeps resting. Proves `on_fill` → suppression → `cancel_tagged` →
/// `engine.cancel_order` compose over the live path, and that EVENT-time (fill/quote ts) drives it.
#[test]
fn fill_rate_breaker_pulls_over_hit_side_end_to_end() {
    // BatchTestClient accepts every order and emits OrderCanceled on cancel (so a pulled side shows
    // Canceled); the default no-op modify keeps the un-pulled ask resting as Accepted.
    let mut config = test_config(1000.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        // window 1000ms, trip at net size 2.5, long cooldown so it stays suppressed to the assert
        strategy: Box::new(
            vike_mm::SpreadMaker::new(1.0, 0.5).with_fill_breaker(1000, 2.5, 100_000),
        ),
    });
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    let events = handle.event_sender();
    let quote = |ts, bid, ask| QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    // tick 1: rest a tagged bid + ask (both Accepted by the client)
    ticks.quote(quote(1, 100.0, 101.0)).unwrap();
    // three BID (buy, side +1) fills, distinct trade_ids so none dedup, no offsetting asks →
    // net +3 ≥ 2.5 arms the bid-side cooldown inside on_fill
    events.blocking_send(ext_fill("bf1", "BTCUSDT", 1.0, 100.0)).unwrap();
    events.blocking_send(ext_fill("bf2", "BTCUSDT", 1.0, 100.0)).unwrap();
    events.blocking_send(ext_fill("bf3", "BTCUSDT", 1.0, 100.0)).unwrap();
    // tick 2 (still inside the cooldown): the bid is PULLED, the ask keeps quoting
    ticks.quote(quote(40, 99.0, 100.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    let side = |s: i32| snap.orders.iter().find(|o| o.side == s).expect("an order for the side");
    assert_eq!(snap.orders.len(), 2, "still the one bid + one ask (pulled/kept, not re-created)");
    assert_eq!(
        side(1).status,
        OrderStatus::Canceled,
        "the over-hit bid was pulled via the runtime"
    );
    assert_eq!(side(-1).status, OrderStatus::Accepted, "the un-hit ask still rests");
}

// ---- Live Strategy::on_fill delivery (the write-once fill lane) ----

use std::sync::Mutex;
use vike_model::Fill;

/// Records every `on_fill` delivery; reacts to the FIRST fill with a buffered market order to
/// prove the on_fill context drains through the one live path like every other handler.
struct FillRecorder {
    fills: Arc<Mutex<Vec<Fill>>>,
}
impl Strategy<LiveBroker> for FillRecorder {
    fn on_fill(&mut self, broker: &mut LiveBroker, fill: &Fill) {
        let first = {
            let mut seen = self.fills.lock().unwrap();
            let first = seen.is_empty();
            seen.push(fill.clone());
            first
        };
        if first {
            broker.submit_market("BTCUSDT", 1, 1.0);
        }
    }
}

fn ext_fill(trade_id: &'static str, symbol: &str, qty: f64, px: f64) -> Event {
    Event::Fill(FillEvent {
        trade_id: trade_id.into(),
        client_order_id: "ext1".into(),
        venue: "binance".into(),
        symbol: symbol.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.1,
        commission_asset: String::new().into(),
        liquidity_side: "maker".into(),
        ts: 5,
        mark_price: None,
        position_side: "BOTH".into(),
    })
}

/// The live `on_fill` gate: exactly one delivery per ACCOUNT-APPLIED fill — a reconnect replay
/// (same trade_id) is deduped BEFORE dispatch, an other-symbol fill never reaches the strategy,
/// and a handler that submits from `on_fill` flows the one live path without deadlock.
#[test]
fn live_on_fill_fires_once_per_applied_fill() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let fills = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(FillRecorder { fills: Arc::clone(&fills) }),
    });
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();

    events.blocking_send(ext_fill("t1", "BTCUSDT", 2.0, 100.0)).unwrap();
    // reconnect replay: same trade_id — the account fold dedups, so NO second on_fill
    events.blocking_send(ext_fill("t1", "BTCUSDT", 2.0, 100.0)).unwrap();
    // account-wide stream noise: another symbol's fill never reaches this strategy
    events.blocking_send(ext_fill("t2", "ETHUSDT", 1.0, 50.0)).unwrap();
    handle.shutdown_and_join();

    let seen = fills.lock().unwrap();
    assert_eq!(
        seen.len(),
        1,
        "one on_fill per applied fill (replay deduped, other symbol dropped): {seen:?}"
    );
    let f = &seen[0];
    assert_eq!((f.side, f.size, f.price, f.fee), (1, 2.0, 100.0, 0.1));
    assert!(f.is_maker, "liquidity_side 'maker' maps to is_maker");
    assert_eq!(f.symbol, "BTCUSDT");
    assert_eq!(f.ts, 5);

    let snap = cell.load();
    assert!(snap.fault.is_none(), "no panic in the on_fill path: {:?}", snap.fault);
    assert_eq!(
        snap.orders.len(),
        1,
        "the order submitted FROM on_fill flowed mint -> gate -> client"
    );
}

/// Local fill builder for the sim venue (the `request` helper's venue).
fn sim_fill(trade_id: &str, qty: f64, px: f64) -> FillEvent {
    FillEvent {
        // `&str`: `BarFillClient::on_bar` mints `bf{seq}` per bar, so this cannot be `&'static str`.
        trade_id: TradeId::new(trade_id).expect("test trade ids are non-empty"),
        client_order_id: "c1".into(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        ts: 7,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// Emits TWO fills in ONE poll batch per submit — the multi-fill case where per-fill
/// state capture matters.
#[derive(Default)]
struct DoubleFillClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for DoubleFillClient {
    fn submit(&mut self, _request: &OrderRequest) {
        self.events.push_back(Event::Fill(sim_fill("d1", 2.0, 100.0)));
        self.events.push_back(Event::Fill(sim_fill("d2", 2.0, 100.0)));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// Records `broker.position()` at each on_fill delivery.
struct PositionRecorder {
    positions: Arc<Mutex<Vec<f64>>>,
}
impl Strategy<LiveBroker> for PositionRecorder {
    fn on_fill(&mut self, broker: &mut LiveBroker, _fill: &Fill) {
        self.positions.lock().unwrap().push(broker.position("BTCUSDT"));
    }
}

/// Backtest-parity gate for multi-fill batches: each on_fill must see the position after
/// ITS fill (2.0 then 4.0), not the post-batch state (4.0 twice) — the state snapshot is
/// taken per fill at the apply_fill fold, like the backtest engine's synchronous firing.
#[test]
fn on_fill_sees_per_fill_state_in_a_multi_fill_batch() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        DoubleFillClient::default(),
        "sim",
        "BTCUSDT",
    );
    let positions = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(PositionRecorder { positions: Arc::clone(&positions) }),
    });
    let handle = spawn_core(engine, config);
    handle
        .try_command(Command::Order(OrderIntent::Submit(request("c1", 1, 4.0, Some(100.0)))))
        .unwrap();
    handle.shutdown_and_join();

    assert_eq!(
        *positions.lock().unwrap(),
        vec![2.0, 4.0],
        "per-fill position (backtest firing point), not post-batch"
    );
}

/// Paper-mode client: rests submitted orders and fills them on the NEXT closed bar at its
/// open (the backtest engine's next-open discipline), like PaperExecutionClient.
#[derive(Default)]
struct BarFillClient {
    resting: Vec<OrderRequest>,
    events: VecDeque<Event>,
    seq: u64,
}
impl ExecutionClient for BarFillClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.resting.push(request.clone());
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
    fn on_bar(&mut self, bar: &vike_model::Bar) {
        for r in self.resting.drain(..) {
            self.seq += 1;
            let mut f = sim_fill(&format!("bf{}", self.seq), r.qty, bar.open);
            f.client_order_id = r.client_order_id.clone();
            f.ts = bar.ts;
            self.events.push_back(Event::Fill(f));
        }
    }
}

/// Records the fill-time market view: (index, closed-bar count, price) per on_fill.
struct FillViewRecorder {
    views: Arc<Mutex<Vec<(usize, usize, f64)>>>,
    submitted: bool,
}
impl Strategy<LiveBroker> for FillViewRecorder {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &vike_model::Bar) {
        if !self.submitted {
            self.submitted = true;
            broker.submit_limit("BTCUSDT", 1, 1.0, 99.0);
        }
    }
    fn on_fill(&mut self, broker: &mut LiveBroker, _fill: &Fill) {
        self.views.lock().unwrap().push((
            broker.index(),
            broker.bars("BTCUSDT").len(),
            broker.price("BTCUSDT"),
        ));
    }
}

/// Backtest-parity gate for the paper bar path: an order from bar 1 fills at bar 2's OPEN,
/// so its on_fill must see history through bar 1 only (index 0, one closed bar) at bar 1's
/// close as the price — bar 2's close is not knowable at the fill moment. (In backtest the
/// same callback fires with index=i-1, bars through i-1, price=close(i-1).)
#[test]
fn paper_on_fill_sees_pre_bar_history_no_lookahead() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "sim", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        BarFillClient::default(),
        "sim",
        "BTCUSDT",
    );
    let views = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(FillViewRecorder { views: Arc::clone(&views), submitted: false }),
    });
    let handle = spawn_core(engine, config);
    let bars = handle.bar_sender();
    let close = |ts: i64, o: f64, c: f64| vike_exec::BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: bar(ts, o, c.max(o), o.min(c), c, 1.0),
    };
    bars.close(close(60_000, 100.0, 101.0)).unwrap(); // bar 1: strategy rests a limit
    bars.close(close(120_000, 98.0, 102.0)).unwrap(); // bar 2: fills at open BEFORE append
    handle.shutdown_and_join();

    assert_eq!(
        *views.lock().unwrap(),
        vec![(0usize, 1usize, 101.0)],
        "fill-time view = history through bar 1 at bar 1's close (no bar-2 look-ahead)"
    );
}

// ---- equity-drawdown latch → liquidate-only (audit exec#4) -------------------------------
//
// The latch folds an account-equity high-water-mark on the SAME per-closed-bar sweep the
// margin-call watchdog uses (marks fresh, off the event fold) and, past a configured drawdown
// threshold, sets trading_state = Reducing so the existing RiskGate permits only reduce-only.

/// An engine pre-loaded with a LONG `qty` @ `entry` on sim/BTCUSDT (commission-free seed fill, so
/// balance/realized stay 0 and equity == seed at mark==entry). Each closed bar's mark is set to
/// bar.close by the per-bar sweep, so a bar's close moves account equity by `qty·(close−entry)` —
/// enough to drive the drawdown latch end-to-end through the real runtime.
fn dd_long_engine(qty: f64, entry: f64) -> ExecutionEngine<RecordingClient> {
    let mut account = Account::new(1.0, "sim", None, BalanceMode::Delta);
    account.apply_fill(&fill("dd-seed", qty, entry)); // fill() is side=1 (long), sim/BTCUSDT, fee 0
    ExecutionEngine::new(
        account,
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "sim",
        "BTCUSDT",
    )
}

/// Close one flat (o=h=l=c) 1m bar on sim/BTCUSDT — the mark the latch sweep reads.
fn dd_close(bars: &vike_exec::BarSender, ts: i64, close: f64) {
    bars.close(vike_exec::BarUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        bar: bar(ts, close, close, close, close, 1.0),
    })
    .unwrap();
}

/// Equity rises to a peak, then falls PAST the threshold → the core LATCHES into liquidate-only
/// (trading_state = Reducing) and a warning naming HWM/equity/drawdown% lands in the ring.
#[test]
fn drawdown_latch_trips_past_threshold() {
    // long 10 @ 100, seed 1000 ⇒ equity = 1000 + 10·(mark−100); threshold 20%.
    let engine = dd_long_engine(10.0, 100.0);
    let handle = spawn_core(engine, CoreConfig { max_drawdown: Some(0.20), ..test_config(1000.0) });
    let cell = handle.snapshot_cell();
    let bars = handle.bar_sender();

    dd_close(&bars, 60_000, 100.0); // equity 1000 → HWM 1000
    dd_close(&bars, 120_000, 120.0); // equity 1200 → HWM 1200
    dd_close(&bars, 180_000, 70.0); // equity 700 → drawdown 41.7% > 20% ⇒ LATCH
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.trading_state, TradingState::Reducing, "must latch to liquidate-only");
    let warn = snap
        .recent_events
        .iter()
        .find(|e| e.starts_with("DRAWDOWN LATCH"))
        .expect("a drawdown-latch warning in the recent-events ring");
    // ⚠ The NUMBERS are unchanged by the move off `resolved_equity` onto `capital_base + own_pnl`:
    // this is an all-`Delta` core, where `seed + own_pnl` IS `resolved_equity` (see
    // `CoreThread::sweep_drawdown_latch`). Only the LABEL moved, which is the point — the fix bites
    // exactly where an `Authoritative` venue wallet exists, and nowhere else.
    assert!(warn.contains("peak=1200.00"), "warning names the HWM: {warn}");
    assert!(warn.contains("curve=700.00"), "warning names the current curve: {warn}");
    assert!(warn.contains("capital_base=1000.00"), "…and the two halves it is made of: {warn}");
    assert!(warn.contains("own_pnl=-300.00"), "…own PnL = 10·(70−100): {warn}");
}

/// A dip that stays WITHIN the threshold never latches — trading_state stays Active, no warning.
#[test]
fn drawdown_dip_within_threshold_does_not_latch() {
    let engine = dd_long_engine(10.0, 100.0);
    let handle = spawn_core(engine, CoreConfig { max_drawdown: Some(0.20), ..test_config(1000.0) });
    let cell = handle.snapshot_cell();
    let bars = handle.bar_sender();

    dd_close(&bars, 60_000, 100.0); // equity 1000 → HWM 1000
    dd_close(&bars, 120_000, 120.0); // equity 1200 → HWM 1200
    dd_close(&bars, 180_000, 110.0); // equity 1100 → drawdown 8.3% < 20% ⇒ no latch
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.trading_state, TradingState::Active, "a sub-threshold dip must not latch");
    assert!(
        !snap.recent_events.iter().any(|e| e.contains("DRAWDOWN LATCH")),
        "no drawdown warning below threshold"
    );
}

/// Once latched, a later equity RECOVERY (even to a NEW high) keeps it Reducing — the latch never
/// auto-un-latches (un-latching is a deliberate manual Command::SetTradingState).
#[test]
fn drawdown_latch_stays_reducing_after_recovery() {
    let engine = dd_long_engine(10.0, 100.0);
    let handle = spawn_core(engine, CoreConfig { max_drawdown: Some(0.20), ..test_config(1000.0) });
    let cell = handle.snapshot_cell();
    let bars = handle.bar_sender();

    dd_close(&bars, 60_000, 100.0); // equity 1000 → HWM 1000
    dd_close(&bars, 120_000, 120.0); // equity 1200 → HWM 1200
    dd_close(&bars, 180_000, 70.0); // equity 700 → LATCH (Reducing)
    dd_close(&bars, 240_000, 130.0); // equity 1300 → new high, but STAYS Reducing (no un-latch)
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(
        snap.trading_state,
        TradingState::Reducing,
        "a recovery to a new equity high must NOT auto-un-latch"
    );
}

/// Disabled (max_drawdown = None, the default) → the latch code never runs: even a catastrophic
/// crash leaves trading_state Active with no warning (behavior matches the pre-change path).
#[test]
fn drawdown_latch_disabled_never_latches() {
    let engine = dd_long_engine(10.0, 100.0);
    // test_config leaves max_drawdown None (the default) → sweep_drawdown_latch is never called.
    let handle = spawn_core(engine, test_config(1000.0));
    let cell = handle.snapshot_cell();
    let bars = handle.bar_sender();

    dd_close(&bars, 60_000, 100.0);
    dd_close(&bars, 120_000, 120.0);
    dd_close(&bars, 180_000, 10.0); // equity 100 — a 91% crash from the would-be HWM
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.trading_state, TradingState::Active, "disabled latch must never fire");
    assert!(
        !snap.recent_events.iter().any(|e| e.contains("DRAWDOWN LATCH")),
        "disabled latch pushes no warning"
    );
}

// ---- feed-health hook: Strategy::on_feed_status (audit co7) ------------------------------------
//
// Feed-death signals (StreamHealth Gap/Stale/Live) reach a MOUNTED strategy through the additive
// Ingest::StreamStatus lane + the runtime's drive_strategy_feed_status dispatch. The hook fires
// ONLY on a status change (an occasional control event), NOT per market message — so it never
// touches the per-tick / event-fold hot path.

use vike_exec::StreamStatusUpdate;
use vike_model::FeedStatus;

/// Records every `on_feed_status` transition, and separately counts `on_quote_tick` — so one test
/// can assert (a) a StreamStatus lane message fires `on_feed_status` with the mapped `FeedStatus`,
/// and (b) a plain market quote fires `on_quote_tick` but NOT `on_feed_status`.
struct FeedStatusRecorder {
    statuses: Arc<Mutex<Vec<FeedStatus>>>,
    quotes: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for FeedStatusRecorder {
    fn on_quote_tick(&mut self, _broker: &mut LiveBroker, _q: &QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    fn on_feed_status(&mut self, _broker: &mut LiveBroker, status: FeedStatus) {
        self.statuses.lock().unwrap().push(status);
    }
}

/// The required gate: a stream-status transition on the ingest lane fires `on_feed_status` (down
/// then up), while a normal market quote fires `on_quote_tick` and NEVER `on_feed_status`. Both
/// lanes are lossless + shutdown is lossless, so all three messages fold before join — deterministic.
#[test]
fn feed_status_hook_fires_on_change_not_per_market_message() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let quotes = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(FeedStatusRecorder {
            statuses: Arc::clone(&statuses),
            quotes: Arc::clone(&quotes),
        }),
    });
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // a normal market message (quote) — fires on_quote_tick, must NOT fire on_feed_status
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    // feed dies, then recovers — each transition fires on_feed_status exactly once
    let status = |s: FeedStatus| StreamStatusUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        stream: "quotes".into(),
        status: s,
    };
    ticks.stream_status(status(FeedStatus::Disconnected)).unwrap();
    ticks.stream_status(status(FeedStatus::Live)).unwrap();

    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 1, "the market quote fired on_quote_tick once");
    assert_eq!(
        *statuses.lock().unwrap(),
        vec![FeedStatus::Disconnected, FeedStatus::Live],
        "on_feed_status fired once per status change (down then up) and NOT for the market quote"
    );
}

/// A status update for a DIFFERENT (venue, symbol) than the mount must never reach the strategy —
/// the dispatch routes by (venue, symbol), so an unrelated feed's death is ignored.
#[test]
fn feed_status_for_other_symbol_is_ignored() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let statuses = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(FeedStatusRecorder {
            statuses: Arc::clone(&statuses),
            quotes: Arc::<AtomicUsize>::default(),
        }),
    });
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // ETHUSDT feed dies — the BTCUSDT mount must not hear it
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    // the mounted symbol's feed dies — this one DOES reach the strategy
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    handle.shutdown_and_join();

    assert_eq!(
        *statuses.lock().unwrap(),
        vec![FeedStatus::Disconnected],
        "only the mounted (venue, symbol)'s status change reaches the strategy"
    );
}

// ---- flow-toxicity hook: Strategy::on_flow (RTDS wallet-toxicity guard, 5c) --------------------
//
// Per-side toxic-flow readings reach a MOUNTED strategy through the additive Ingest::Flow lane +
// the runtime's drive_strategy_flow dispatch — the exact twin of the feed-status lane above. The
// hook fires only on a toxicity update (an occasional control event), routed by the mount's OWN
// (venue, symbol), never touching the per-tick / event-fold hot path.

use vike_exec::FlowUpdate;
use vike_model::FlowToxicity;

/// Records every `on_flow` reading so a test can assert the runtime routed the right FlowToxicity.
struct FlowRecorder {
    flows: Arc<Mutex<Vec<FlowToxicity>>>,
}
impl Strategy<LiveBroker> for FlowRecorder {
    fn on_flow(&mut self, _broker: &mut LiveBroker, flow: FlowToxicity) {
        self.flows.lock().unwrap().push(flow);
    }
}

/// A flow update for the MOUNTED (venue, symbol) fires `on_flow` with exactly the delivered reading,
/// while a flow for a DIFFERENT (venue, symbol) never reaches the strategy — the dispatch routes by
/// the mount's own key, the same predicate the feed-status lane uses.
#[test]
fn flow_hook_fires_for_the_mounted_pair_only() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let flows = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(FlowRecorder { flows: Arc::clone(&flows) }),
    });
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();

    // a DIFFERENT symbol's toxicity — the BTCUSDT mount must not hear it
    ticks
        .flow(FlowUpdate {
            venue: "binance".into(),
            symbol: "ETHUSDT".into(),
            flow: FlowToxicity { bid: 0.9, ask: 0.9, ts: 1 },
        })
        .unwrap();
    // the mounted (venue, symbol)'s toxicity — this one DOES reach the strategy
    ticks
        .flow(FlowUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            flow: FlowToxicity { bid: 0.25, ask: 0.75, ts: 2 },
        })
        .unwrap();

    handle.shutdown_and_join();

    assert_eq!(
        *flows.lock().unwrap(),
        vec![FlowToxicity { bid: 0.25, ask: 0.75, ts: 2 }],
        "only the mounted (venue, symbol)'s toxicity reaches on_flow"
    );
}

/// Rests a tagged quote on the first market quote, then PULLS it via `mass_cancel` when the feed
/// disconnects — the canonical "pull my quotes when my feed dies" behavior. Proves orders buffered
/// INSIDE `on_feed_status` route through the one live path (drain_broker) exactly like the tick lanes.
struct PullOnDisconnect {
    rested: Arc<AtomicBool>,
}
impl Strategy<LiveBroker> for PullOnDisconnect {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.rested.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
        }
    }
    fn on_feed_status(&mut self, broker: &mut LiveBroker, status: FeedStatus) {
        if matches!(status, FeedStatus::Disconnected | FeedStatus::Stale) {
            broker.mass_cancel(); // pull all resting quotes on a dead/stale feed
        }
    }
}

#[test]
fn feed_status_disconnect_pulls_resting_quotes() {
    let rested = Arc::<AtomicBool>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(PullOnDisconnect { rested: Arc::clone(&rested) }),
    });
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // rest a tagged quote (BatchTestClient accepts → Accepted)
    ticks
        .quote(QuoteUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            quote: QuoteTick {
                ts: 1,
                local_ts: 0,
                bid: 100.0,
                ask: 100.2,
                bid_size: 1.0,
                ask_size: 1.0,
                symbol: String::new(),
            },
        })
        .unwrap();
    // feed dies → the strategy pulls its resting quote from inside on_feed_status
    ticks
        .stream_status(StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "quotes".into(),
            status: FeedStatus::Disconnected,
        })
        .unwrap();
    handle.shutdown_and_join();

    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "the one resting quote (pulled, not re-created)");
    assert_eq!(
        snap.orders[0].status,
        OrderStatus::Canceled,
        "the resting quote was pulled on disconnect via on_feed_status → mass_cancel"
    );
}

// ---- order-lifecycle hook: Strategy::on_order_event (position-executor stage 3) ----------------
//
// A NON-FILL order outcome (a venue REJECT / CANCEL / EXPIRE, or a RiskGate DENY) reaches the
// mounted strategy that OWNS the order through the additive on_order_event hook: the engine captures
// the transition (tagged with the order's (venue, symbol)) and the runtime routes it to that mount's
// hook — the on_fill lane's twin. It fires at ORDER cadence, never per market message, so a plain
// quote NEVER fires it. Fills stay on on_fill (never double-delivered here).

use vike_model::events::OrderRejected;
use vike_model::{OrderEventKind, OrderLifecycle};

/// Rejects every order at the venue (emits OrderRejected for the submitted coid). The gate passes
/// first, so the order registers then goes Submitted → Rejected — the venue-reject path
/// on_order_event must surface.
#[derive(Default)]
struct RejectingClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for RejectingClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.events.push_back(Event::OrderRejected(OrderRejected {
            client_order_id: request.client_order_id.clone(),
            reason: "venue nope".into(),
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {} // never canceled in the reject test
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// Records every on_order_event, counts on_quote_tick, and submits ONE tagged limit on its first
/// quote (so a coid is minted and its outcome routes back). When `cancel_on_later` is set it cancels
/// that tagged order on every subsequent quote — driving a venue cancel through the same hook.
struct OrderEventRecorder {
    events: Arc<Mutex<Vec<OrderLifecycle>>>,
    quotes: Arc<AtomicUsize>,
    submitted: Arc<AtomicBool>,
    cancel_on_later: bool,
    tag: &'static str,
}
impl Strategy<LiveBroker> for OrderEventRecorder {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
        if !self.submitted.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged(self.tag, 1, 1.0, 100.0);
        } else if self.cancel_on_later {
            broker.cancel_tagged(self.tag);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderLifecycle) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn oe_quote(ts: i64) -> QuoteUpdate {
    QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    }
}

/// The required gate: a venue REJECT of the strategy's own order fires on_order_event exactly once
/// (with the reject reason + the strategy's coid), while a plain market quote fires on_quote_tick and
/// NEVER on_order_event. The tick lane is lossless + shutdown is lossless, so all messages fold
/// before join — deterministic.
#[test]
fn order_event_hook_fires_on_reject_not_per_market_message() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RejectingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let quotes = Arc::<AtomicUsize>::default();
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::clone(&quotes),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: false,
            tag: "entry",
        }),
    });
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(oe_quote(1)).unwrap(); // submit → venue REJECTS → on_order_event(Rejected)
    ticks.quote(oe_quote(2)).unwrap(); // a plain market quote — on_quote_tick, NOT on_order_event
    handle.shutdown_and_join();

    assert_eq!(quotes.load(Ordering::Relaxed), 2, "both market quotes fired on_quote_tick");
    let got = events.lock().unwrap().clone();
    assert_eq!(
        got.len(),
        1,
        "exactly ONE order event (the reject), NOT one per market quote: {got:?}"
    );
    let snap = cell.load();
    assert_eq!(snap.orders.len(), 1, "the one submitted-then-rejected order");
    assert_eq!(snap.orders[0].status, OrderStatus::Rejected);
    assert_eq!(
        got[0],
        OrderLifecycle {
            client_order_id: snap.orders[0].client_order_id.clone(),
            // The STRATEGY's own name for the order — it submitted with `submit_limit_tagged`, so
            // the runtime stamps the tag it filed in `strategy_tags`. This is the half a tagged-order
            // strategy can actually match on: it never sees the coid beside it.
            tag: Some("entry".into()),
            kind: OrderEventKind::Rejected { reason: "venue nope".into() },
        },
        "on_order_event delivered the venue reject (reason + the strategy's OWN coid AND tag)",
    );
}

/// A venue ACCEPT then CANCEL of the strategy's own order each fire on_order_event once, in order,
/// for the same coid — and fills are NOT among them (they flow on_fill). Uses BatchTestClient
/// (accepts on submit, cancels on cancel).
#[test]
fn order_event_hook_fires_on_accept_then_cancel() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::<AtomicUsize>::default(),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: true,
            tag: "entry",
        }),
    });
    let handle = spawn_core(batch_engine(RiskLimits::new()), config);
    let ticks = handle.tick_sender();

    ticks.quote(oe_quote(1)).unwrap(); // submit → BatchTestClient ACCEPTS → on_order_event(Accepted)
    ticks.quote(oe_quote(2)).unwrap(); // cancel_tagged → CANCELED → on_order_event(Canceled)
    handle.shutdown_and_join();

    let got = events.lock().unwrap().clone();
    let kinds: Vec<_> = got.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![OrderEventKind::Accepted, OrderEventKind::Canceled { reason: "batch".into() }],
        "on_order_event saw the venue accept then the cancel, in order (no fill among them): {got:?}",
    );
    assert!(
        got.iter().all(|e| e.client_order_id == got[0].client_order_id),
        "both transitions carried the SAME order coid: {got:?}",
    );
}

/// Accepts every order at the venue, then FULLY FILLS it — the market maker's ordinary outcome, and
/// the one that used to reach a tagged-order strategy as nothing at all.
#[derive(Default)]
struct FillingClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for FillingClient {
    fn submit(&mut self, request: &OrderRequest) {
        // Initialized → Submitted → Accepted → Filled: the FSM takes each step, so the submit echo
        // leads (the `BatchTestClient` shape).
        self.events.push_back(Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        }));
        self.events.push_back(Event::OrderAccepted(OrderAccepted {
            client_order_id: request.client_order_id.clone(),
            venue_order_id: None,
            ts: request.ts,
        }));
        // ...then the fill, in the order a real venue mapper emits it: `Fill` (the MONEY lane —
        // `Account::apply_fill`, which is what populates `applied_fills` and drives `on_fill`) then
        // `OrderFilled` (the FSM terminal). `crates/bridges/binance/src/family/event_mapper.rs`'s
        // `map_execution_report` produces exactly this pair, in exactly this order.
        let fill = FillEvent {
            trade_id: "t1".into(),
            client_order_id: request.client_order_id.clone(),
            venue: request.venue.clone().into(),
            symbol: request.symbol.clone().into(),
            side: request.side,
            last_qty: request.qty, // the WHOLE order — this terminalizes it
            last_px: request.price.unwrap_or(100.0),
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "maker".into(),
            ts: request.ts,
            mark_price: None,
            position_side: "BOTH".into(),
        };
        self.events.push_back(Event::Fill(fill.clone()));
        self.events.push_back(Event::OrderFilled(vike_model::events::OrderFilled {
            client_order_id: request.client_order_id.clone(),
            fill,
            ts: request.ts,
        }));
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// ⚠ THE WEDGE, at the runtime seam. A tagged quote that FILLS COMPLETELY must reach its strategy as
/// a terminal `OrderLifecycle` carrying the strategy's OWN tag — the only name a tagged-order
/// strategy can match, since it never sees a client-order-id.
///
/// Before this existed the fill lane delivered `on_fill` and stopped: `OrderLifecycle::from_event`
/// returns `None` for every fill event, so a full fill produced NO lifecycle event at all, and
/// `Fill` carries neither order identity nor remaining quantity. A maker therefore went on believing
/// its filled quote was resting, re-priced a terminal coid every tick, and
/// `ExecutionEngine::modify_order`'s not-modifiable early return swallowed each one silently —
/// measured on the the CI box live mount as a permanent `orders:2` ceiling with `working` at 0.
#[test]
fn a_fully_filled_tagged_order_reaches_the_strategy_as_a_terminal_event_with_its_tag() {
    let engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        FillingClient::default(),
        "binance",
        "BTCUSDT",
    );
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(OrderEventRecorder {
            events: Arc::clone(&events),
            quotes: Arc::<AtomicUsize>::default(),
            submitted: Arc::<AtomicBool>::default(),
            cancel_on_later: false,
            tag: "bid",
        }),
    });
    let handle = spawn_core(engine, config);
    let ticks = handle.tick_sender();
    ticks.quote(oe_quote(1)).unwrap(); // submit "bid" → the client queues ACCEPTED + a FULL fill
    ticks.quote(oe_quote(2)).unwrap(); // a second dispatch folds them (as the reject test does)
    handle.shutdown_and_join();

    let got = events.lock().unwrap().clone();
    let kinds: Vec<_> = got.iter().map(|e| e.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![OrderEventKind::Accepted, OrderEventKind::Filled],
        "the accept, then the ORDER's completion — the death `on_fill` cannot express: {got:?}",
    );
    assert!(
        got.iter().all(|e| e.tag.as_deref() == Some("bid")),
        "every transition named the strategy's OWN tag, the only name it can match: {got:?}",
    );
}

/// Records on_order_event per mount, tagging each event with the mount that received it, so a test
/// can prove an event for a coid owned by one mount NEVER reaches the other.
struct TaggedOrderEventRecorder {
    label: &'static str,
    events: Arc<Mutex<Vec<(&'static str, OrderLifecycle)>>>,
    submitted: Arc<AtomicBool>,
    tag: &'static str,
}
impl Strategy<LiveBroker> for TaggedOrderEventRecorder {
    fn on_quote_tick(&mut self, broker: &mut LiveBroker, _q: &QuoteTick) {
        if !self.submitted.swap(true, Ordering::Relaxed) {
            broker.submit_limit_tagged(self.tag, 1, 1.0, 100.0);
        }
    }
    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderLifecycle) {
        self.events.lock().unwrap().push((self.label, event.clone()));
    }
}

/// Multi-mount isolation: two mounts (BTCUSDT + ETHUSDT over ONE engine via extra_symbols /
/// extra_mounts) each submit their own order; the ACCEPT for each routes to the mount that OWNS that
/// order's (venue, symbol) and to NO other — the same routing key on_fill uses.
#[test]
fn order_event_routes_to_the_owning_mount_only() {
    let mut engine = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        BatchTestClient::default(),
        "binance",
        "BTCUSDT",
    );
    engine.extra_symbols = vec!["ETHUSDT".into()];

    let events = Arc::new(Mutex::new(Vec::new()));
    let mount = |label, symbol: &str, tag| StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(TaggedOrderEventRecorder {
            label,
            events: Arc::clone(&events),
            submitted: Arc::<AtomicBool>::default(),
            tag,
        }),
    };
    let mut config = test_config(1.0);
    config.strategy = Some(mount("btc", "BTCUSDT", "b"));
    config.extra_mounts = vec![mount("eth", "ETHUSDT", "e")];
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    let quote = |symbol: &str, ts: i64| QuoteUpdate {
        venue: "binance".into(),
        symbol: symbol.into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 100.0,
            ask: 100.2,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    };
    ticks.quote(quote("BTCUSDT", 1)).unwrap(); // btc mount submits → Accepted(btc coid)
    ticks.quote(quote("ETHUSDT", 2)).unwrap(); // eth mount submits → Accepted(eth coid)
    handle.shutdown_and_join();

    let snap = cell.load();
    let coid_of = |symbol: &str| {
        snap.orders.iter().find(|o| o.symbol == symbol).unwrap().client_order_id.clone()
    };
    let (btc_coid, eth_coid) = (coid_of("BTCUSDT"), coid_of("ETHUSDT"));
    assert_ne!(btc_coid, eth_coid, "distinct orders, one per symbol");

    let got = events.lock().unwrap().clone();
    // each mount heard EXACTLY its own order's accept — and never the other's
    let for_label = |label: &str| -> Vec<OrderLifecycle> {
        got.iter().filter(|(l, _)| *l == label).map(|(_, e)| e.clone()).collect()
    };
    assert_eq!(
        for_label("btc"),
        vec![OrderLifecycle {
            client_order_id: btc_coid.clone(),
            tag: Some("b".into()),
            kind: OrderEventKind::Accepted
        }],
        "the BTC mount heard only its own order's accept: {got:?}",
    );
    assert_eq!(
        for_label("eth"),
        vec![OrderLifecycle {
            client_order_id: eth_coid.clone(),
            tag: Some("e".into()),
            kind: OrderEventKind::Accepted
        }],
        "the ETH mount heard only its own order's accept: {got:?}",
    );
}
