//! The headless full path against the R5 golden, the channel contracts, reconcile, the shared FIFO.

use std::path::PathBuf;
use std::time::{Duration, Instant};
use vike_core::CommandRejected;
use vike_exec::testing::TestExecutionClient;
use vike_exec::{MarketTick, ReconcileSnapshot};
use vike_model::f64_to_hex_bits;

use super::*;

fn fixture(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/r5").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn assert_bits(actual: f64, expected_hex: &serde_json::Value, what: &str) {
    assert_eq!(f64_to_hex_bits(actual), expected_hex.as_str().unwrap(), "{what}: got {actual}");
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
    let engine = sim_engine();
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
    let engine = sim_engine();
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
    let engine = sim_engine();
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
    let engine = sim_engine();
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
    let engine = sim_engine();
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
    let engine = sim_engine_with(TestExecutionClient::new("sim", 100.0));
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
    let engine = sim_engine();
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
/// (`crates/vike-tradehub/src/server/control.rs`'s `accept_command`, whose `Busy` arm becomes
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

    let engine = sim_engine();
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
        b.apply_snapshot(1, &[BookLevel::new(100.0, 5.0)], &[BookLevel::new(101.0, 5.0)]);
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

/// R5c bar lanes: REST seed replaces the series; closed bars append losslessly (with
/// idempotent re-close on reconnect overlap and stale-replay drops); forming updates
/// conflate latest-wins and never regress behind a close.
#[test]
fn bar_lanes_seed_close_forming() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
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
