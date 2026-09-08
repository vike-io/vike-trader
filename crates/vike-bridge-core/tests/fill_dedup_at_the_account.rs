//! The two SHARED bridge-core replay lanes, folded through a REAL `ExecutionEngine`, proving the
//! money-side dedup holds **at the `Account`** â not one type up, where it used to live.
//!
//! ## Why this file exists
//!
//! `vike_exec::Account::apply_fill` moves money (`balance -= commission`, `fees_paid +=`, the
//! position fold, `realized_pnl +=`) and is not safe to run twice for one venue execution. Its guard
//! used to be `ExecutionEngine`'s own `seen_trade_ids` set, checked BEFORE it called in â an
//! arrangement only as good as every caller's memory. The number of paths that re-deliver an
//! already-folded fill has been growing, and the two SHARED ones both live in this crate:
//!
//!   1. [`vike_bridge_core::exec_actor::run_loop`]'s **gap sentinel**, which calls a venue `resync`
//!      closure replaying a window of venue history (`allOrders`+`userTrades` on binance/aster,
//!      `/v5/order/history`+`/v5/execution/list` on bybit, `orders-history`+`fills-history` on okx,
//!      the deribit pair) â every one of them bounded by a ROW COUNT and by no time at all.
//!   2. [`vike_bridge_core::user_data::run_resync_supervisor`]'s **reconnect replay**, which re-fetches
//!      and re-emits history on every session-gen bump, for eight production consumers.
//!
//! Both are venue-agnostic, both re-emit fills the fold has already seen, and neither can see the
//! guard it depends on. The tests below drive the REAL loop (no network, no venue) into a REAL
//! engine and assert two things per lane: the equity delta of the replay is **bitwise zero**, and the
//! refusal was made by `Account` itself (`Account::duplicate_fills_refused`). The second assertion is
//! what pins the guard's LOCATION â the equity assertion alone passed while the check sat in
//! `ExecutionEngine`, so on its own it cannot tell you where the replay was stopped, and "where" is
//! the defect.
//!
//! â  Scope, stated plainly: these lanes are guarded WITHIN a process lifetime and always were. The
//! measured the CI box defect was a COLD-START one â the in-memory ledger is empty in a fresh process, so
//! the first replay after a restart re-folded the venue's retained history. `run_loop` now floors
//! that with a spawn-time timestamp (#1335, exercised below), `run_resync_supervisor` does not yet,
//! and NO shipped binary seeds the ledger from the journal. Nothing in this file closes the cold-start
//! hole; it closes the "a future caller forgets the guard" hole.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_bridge_core::exec_actor::{ExecCommand, run_loop};
use vike_bridge_core::rest::VenueRest;
use vike_bridge_core::transport::VenueApiError;
use vike_bridge_core::user_data::run_resync_supervisor;
use vike_exec::testing::RecordingClient;
use vike_exec::{
    Account, BalanceMode, EventHandler, ExecutionEngine, Ingest, Outbox, RiskGate, RiskLimits,
    event_channel,
};
use vike_model::OrderRequest;
use vike_model::events::{Event, FillEvent, OrderAccepted, OrderSubmitted};

const VENUE: &str = "binance";
const SYMBOL: &str = "BTCUSDT";

/// How long a cross-thread handshake may take before a test declares failure. Every use sits in a
/// poll-until-success loop, so this bounds only how long a genuinely broken test takes to fail.
const DEADLINE: Duration = Duration::from_secs(5);

/// One venue execution. `ts` is a venue timestamp, which `run_loop`'s pre-spawn floor reads.
fn fill(trade_id: &str, qty: f64, px: f64, comm: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: vike_model::events::TradeId::new(trade_id).expect("test ids are non-empty"),
        client_order_id: "c1".to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: qty,
        last_px: px,
        commission: comm,
        commission_asset: "USDT".into(),
        liquidity_side: "taker".into(),
        ts,
        mark_price: None,
        position_side: "BOTH".into(),
    }
}

/// A real engine over a real `Account`, with the fill's order pre-registered so the fold admits it.
fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    )
}

/// Fold a stream into the engine and report `(equity_delta_bits_moved, refusals_added)`.
fn fold(eng: &mut ExecutionEngine<RecordingClient>, events: &[Event]) -> (bool, u64) {
    let mut outbox = Outbox::default();
    let equity_before = eng.account.equity_all(10_000.0);
    let refused_before = eng.account.duplicate_fills_refused;
    for ev in events {
        eng.on_event(ev, &mut outbox);
    }
    (
        eng.account.equity_all(10_000.0).to_bits() != equity_before.to_bits(),
        eng.account.duplicate_fills_refused - refused_before,
    )
}

// ---- LANE 1: `exec_actor::run_loop`'s gap sentinel ----------------------------------------------

/// A `VenueRest` double whose `submit_order` reports the order accepted and nothing else. The FILLS
/// arrive through the sentinel's `resync` closure, exactly as they do for the real crypto venues.
struct SubmitOnly;

impl VenueRest for SubmitOnly {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        vec![
            Event::OrderSubmitted(OrderSubmitted {
                client_order_id: request.client_order_id.clone(),
                ts: request.ts,
            }),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: Some("v1".into()),
                ts: request.ts,
            }),
        ]
    }

    fn cancel_order(&self, _client_order_id: &str) -> Result<(), VenueApiError> {
        Ok(())
    }
}

/// Drive the REAL `run_loop` through ONE submit and let its gap sentinel fire twice, with the venue
/// `resync` closure returning the SAME row-count-bounded history window both times â which is exactly
/// what `userTrades`/`/v5/execution/list`/`fills-history` do, since no venue closure passes a start
/// time. Returns everything that reached the ingest lane.
fn run_loop_with_a_repeating_history_window(
    history: impl Fn() -> Vec<Event> + Send + 'static,
) -> Vec<Event> {
    let (events, mut rx) = event_channel(64);
    let (tx, cmd_rx) = mpsc::channel::<ExecCommand>();
    let passes = Arc::new(AtomicU64::new(0));

    let passes_t = passes.clone();
    let loop_thread = std::thread::spawn(move || {
        run_loop(
            &SubmitOnly,
            &events,
            cmd_rx,
            move || {
                passes_t.fetch_add(1, Ordering::Relaxed);
                history()
            },
            Duration::from_millis(5),
        );
    });

    let req: OrderRequest = serde_json::from_value(serde_json::json!({
        "client_order_id": "c1", "venue": VENUE, "symbol": SYMBOL,
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0
    }))
    .unwrap();
    tx.send(ExecCommand::Submit(Box::new(req.clone()))).unwrap();

    // Wait for the sentinel to fire, then poke it again (a submit re-arms it) and wait once more.
    let deadline = Instant::now() + DEADLINE;
    while passes.load(Ordering::Relaxed) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    tx.send(ExecCommand::Submit(Box::new(req))).unwrap();
    while passes.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(passes.load(Ordering::Relaxed) >= 2, "the gap sentinel must have fired twice");

    tx.send(ExecCommand::Shutdown).unwrap();
    loop_thread.join().unwrap();

    let mut out = Vec::new();
    while let Ok(ingest) = rx.try_recv() {
        if let Ingest::Event(ev) = ingest {
            out.push(ev);
        }
    }
    out
}

/// LANE 1. The sentinel's history window replays the same execution on every pass; the `Account`
/// refuses every copy after the first, and the replayed passes move equity by bitwise zero.
#[test]
fn the_gap_sentinels_repeating_history_window_folds_once_at_the_account() {
    // The row's `ts` must be POST-spawn or #1335's floor removes it before the ledger ever sees it,
    // and this test would then measure that floor instead of the dedup (which is what the sibling
    // test below is for). So the ts is stamped LAZILY, on the sentinel's FIRST pass â which is also
    // the realistic shape: a venue history row for an order this process placed carries a venue time
    // after the exec thread came up, and every later pass replays that SAME fixed row.
    let stamped: Arc<std::sync::OnceLock<i64>> = Arc::new(std::sync::OnceLock::new());
    let stream = run_loop_with_a_repeating_history_window(move || {
        let ts = *stamped.get_or_init(vike_model::clock::now_ms);
        vec![Event::Fill(fill("hist-1", 2.0, 100.0, 0.5, ts))]
    });

    let fills: Vec<&Event> = stream.iter().filter(|e| matches!(e, Event::Fill(_))).collect();
    assert!(
        fills.len() >= 2,
        "the sentinel must have re-emitted the same history row on each pass, got {fills:?}"
    );

    let mut eng = engine();
    let (moved, refused) = fold(&mut eng, &stream);
    assert!(moved, "the FIRST copy of a real fill must move equity");
    assert_eq!(
        refused,
        fills.len() as u64 - 1,
        "every copy after the first must be refused BY THE ACCOUNT"
    );
    assert_eq!(
        eng.account.positions[&(VENUE.into(), SYMBOL.into(), "BOTH".into())].size,
        2.0,
        "position is ONE fill's worth, whatever the sentinel replayed"
    );
    assert_eq!(eng.account.balance.to_bits(), (-0.5_f64).to_bits(), "ONE commission, not N");
    assert_eq!(eng.account.colliding_fills_refused, 0, "identical re-deliveries, not collisions");
}

/// LANE 1, second guarantee â #1335's spawn-time floor, still doing its job. A history row stamped
/// BEFORE this loop's spawn belongs to a previous session and never reaches the lane at all, so it
/// cannot even become a dedup question. The two guards are independent and this pins that: the floor
/// stops PRE-START history, the account ledger stops WITHIN-SESSION replays.
#[test]
fn a_pre_spawn_history_row_never_reaches_the_account_at_all() {
    let stream = run_loop_with_a_repeating_history_window(|| {
        // epoch+1ms: unambiguously before this process
        vec![Event::Fill(fill("ancient-1", 2.0, 100.0, 0.5, 1))]
    });
    assert!(
        !stream.iter().any(|e| matches!(e, Event::Fill(_))),
        "a pre-spawn fill must be floored by run_loop, never emitted: {stream:?}"
    );

    let mut eng = engine();
    let (moved, refused) = fold(&mut eng, &stream);
    assert!(!moved, "no fill reached the fold, so equity cannot have moved");
    assert_eq!(refused, 0, "and nothing needed refusing â the floor is upstream of the ledger");
}

// ---- LANE 2: `user_data::run_resync_supervisor`'s reconnect replay ------------------------------

/// Drive the REAL `run_resync_supervisor` across `reconnects` session-gen bumps, with the venue
/// `fetch_and_map` closure returning the same history both times (no watermark exists on this lane â
/// that is the point). Returns everything it emitted, in order.
fn resync_across_reconnects(history: Vec<Event>, reconnects: u64) -> Vec<Event> {
    let generation = Arc::new(AtomicU64::new(0));
    let weak = Arc::downgrade(&generation);
    let stop = Arc::new(AtomicBool::new(false));
    let fetches = Arc::new(AtomicU64::new(0));
    let emitted = Arc::new(std::sync::Mutex::new(Vec::<Event>::new()));

    let (stop_t, fetches_t, emitted_t) = (stop.clone(), fetches.clone(), emitted.clone());
    let handle = std::thread::spawn(move || {
        run_resync_supervisor(
            weak,
            0,
            // ⚠ `spawn_ms = 0` = NO pre-spawn floor, deliberately: this test proves the ACCOUNT
            // refuses the duplicate on its own. With a floor armed the replayed fills would never
            // reach the ledger and the test would pass for the wrong reason.
            0,
            &stop_t,
            Duration::from_millis(1),
            Duration::from_millis(2), // tiny settle so the test is fast
            || {
                fetches_t.fetch_add(1, Ordering::Relaxed);
                history.clone()
            },
            |ev| {
                emitted_t.lock().unwrap().push(ev);
                true
            },
            None,
        );
    });

    let deadline = Instant::now() + DEADLINE;
    for n in 1..=reconnects {
        generation.fetch_add(1, Ordering::Relaxed); // a reconnect: the pump bumps the session gen
        while fetches.load(Ordering::Relaxed) < n && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(
        fetches.load(Ordering::Relaxed),
        reconnects,
        "the supervisor must have replayed once per reconnect"
    );

    stop.store(true, Ordering::Relaxed);
    drop(generation);
    handle.join().unwrap();

    emitted.lock().unwrap().clone()
}

/// LANE 2. Three reconnects, one history window with no watermark: the same execution is re-emitted
/// three times and the `Account` folds it once. This lane has NO spawn-time floor (unlike lane 1), so
/// the account ledger is the ONLY thing standing between it and a triple-counted commission.
#[test]
fn the_resync_supervisors_reconnect_replay_folds_once_at_the_account() {
    let stream =
        resync_across_reconnects(vec![Event::Fill(fill("resync-1", 2.0, 100.0, 0.5, 1_000))], 3);

    let fills = stream.iter().filter(|e| matches!(e, Event::Fill(_))).count();
    assert_eq!(fills, 3, "one replay per reconnect, no watermark on this lane: {stream:?}");

    let mut eng = engine();
    let (moved, refused) = fold(&mut eng, &stream);
    assert!(moved, "the first copy is a real fill");
    assert_eq!(refused, 2, "the other two must be refused BY THE ACCOUNT");
    assert_eq!(
        eng.account.positions[&(VENUE.into(), SYMBOL.into(), "BOTH".into())].size,
        2.0,
        "three replays, ONE fill's worth of position"
    );
    assert_eq!(
        eng.account.balance.to_bits(),
        (-0.5_f64).to_bits(),
        "and ONE commission â a triple-count would read -1.5"
    );
    assert_eq!(eng.account.fees_paid.to_bits(), 0.5_f64.to_bits());
}

/// LANE 2, the invariant stated as an equity delta over a MIXED window â the shape a real reconnect
/// produces: rows the fold has already seen alongside a genuinely new one that executed while the
/// socket was down. The new fill must land; the replayed ones must move equity by bitwise zero.
#[test]
fn a_mixed_replay_window_lands_only_the_genuinely_new_fill() {
    let already_folded = Event::Fill(fill("resync-1", 2.0, 100.0, 0.5, 1_000));
    let executed_while_down = Event::Fill(fill("resync-2", 3.0, 101.0, 0.7, 2_000));

    let mut eng = engine();
    // Session 1 folded the first fill.
    let (moved, _) = fold(&mut eng, std::slice::from_ref(&already_folded));
    assert!(moved);
    let equity_after_session_1 = eng.account.equity_all(10_000.0);

    // Reconnect: the supervisor replays BOTH rows (no watermark), twice over two reconnects.
    let stream = resync_across_reconnects(vec![already_folded, executed_while_down.clone()], 2);
    let (moved, refused) = fold(&mut eng, &stream);

    assert!(moved, "the gap fill is REAL and must land â a dedup must never swallow it");
    assert_eq!(refused, 3, "2 replays of resync-1 + 1 replay of resync-2");
    assert_eq!(
        eng.account.positions[&(VENUE.into(), SYMBOL.into(), "BOTH".into())].size,
        5.0,
        "2.0 + 3.0, each folded exactly once"
    );

    // And a THIRD reconnect, now that both are folded, moves equity by exactly nothing.
    let equity_before = eng.account.equity_all(10_000.0);
    assert_ne!(
        equity_before.to_bits(),
        equity_after_session_1.to_bits(),
        "sanity: the gap fill did change equity"
    );
    let stream = resync_across_reconnects(
        vec![Event::Fill(fill("resync-1", 2.0, 100.0, 0.5, 1_000)), executed_while_down],
        1,
    );
    let (moved, refused) = fold(&mut eng, &stream);
    assert!(!moved, "a fully-replayed window must move equity by BITWISE ZERO");
    assert_eq!(refused, 2, "both rows refused at the account");
}
