//! **The journal `Snapshot` is bounded by what is live, and a restart loses nothing by it** (`vike_execution`'s
//! `Engine::journal_snapshot`, `crates/vike-execution/src/account/fill_window.rs`), through the real core and the
//! production boot restore (`restore_for_boot` + `RestoreReport::apply_to`):
//!
//! - after 100,000 fills over 120 days, 220 ended orders and three working ones, a clean stop: the exit `Snapshot`
//!   holds the three working orders and a window of the trade ids; the restart holds the same positions, money and
//!   working orders, remembers the ids a late retry must still be refused under, and books NOTHING again when the
//!   venue re-delivers old fills (the window's by their ids, the older ones by the floor) or a reconcile pass
//!   reports them, while a new fill books;
//! - the node's reconcile read-back (`CoreConfig::reconcile_lookback_ms`) sizes the window, and a restart that raised
//!   it books no re-delivered fill twice;
//! - a crash in the middle of writing a `Snapshot`: the restore refolds from the `Snapshot` before it and holds the same
//!   book;
//! - an emulated order beside a history the `Snapshot` leaves out survives the restart.
//!
//! The core's clock advances by a STEP the test sets on every read, so a phase's stamps are the phase's whatever
//! the core's lag behind the sender; the test waits for each phase to land before it changes the step.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::scratch::Scratch;
use vike_core::replay::{RestoreOutcome, RestoreReport, restore_for_boot};
use vike_core::{CoreConfig, CoreHandle, JournalConfig, spawn_core};
use vike_execution::account::fill_window::WIDEST_WINDOW_DAYS;
use vike_execution::recon::{ReconcileBalanceTolerance, ReconcileMode, ReconcilePolicy};
use vike_execution::testing::{RecordingClient, engine_on};
use vike_execution::{
    Command, CoreSnapshot, Engine, EngineSnapshot, EngineSnapshotAccount, JOURNAL_CLOSED_PNLS,
    OrderIntent, ReconcileReports,
};
use vike_journal::{Journal, JournalRecord};
use vike_model::MS_PER_DAY;
use vike_model::events::{
    Event, FillEvent, LiquiditySide, OrderAccepted, OrderCanceled, OrderSubmitted, PositionSide,
    TradeId,
};
use vike_model::test_support::file::plant;
use vike_model::{AccountTier, FillReport, OrderPeg, OrderPegAnchor, OrderRequest, OrderType};

use crate::kit::handle::wait_for_within;
use crate::kit::journal::{journal_at, records};
use crate::kit::requests::sim_limit;

/// Day 20,000 of the epoch (2024-10-04): the first record.
const T0: i64 = 20_000 * MS_PER_DAY;
const MIN: i64 = 60_000;

/// The core's clock: each read hands out the time and moves it on by the step.
#[derive(Clone)]
struct StepClock {
    t: Arc<AtomicI64>,
    step: Arc<AtomicI64>,
}

impl StepClock {
    fn new() -> Self {
        StepClock { t: Arc::new(AtomicI64::new(T0)), step: Arc::new(AtomicI64::new(1)) }
    }
    fn boxed(&self) -> Box<dyn vike_model::Clock + Send> {
        let (t, step) = (Arc::clone(&self.t), Arc::clone(&self.step));
        Box::new(move || t.fetch_add(step.load(Ordering::SeqCst), Ordering::SeqCst))
    }
    fn now(&self) -> i64 {
        self.t.load(Ordering::SeqCst)
    }
    fn jump(&self, ms: i64) {
        self.t.fetch_add(ms, Ordering::SeqCst);
    }
    fn step(&self, ms: i64) {
        self.step.store(ms, Ordering::SeqCst);
    }
}

/// A `demo` engine on `sim` (its operator orders are restore candidates; a paper book empties at a restart).
fn engine() -> Engine<RecordingClient> {
    let mut e = engine_on("sim", "BTCUSDT", RecordingClient::default());
    e.set_tier(AccountTier::Demo);
    e
}

fn config(dir: &Path, clock: &StepClock, snapshot_every: u64) -> CoreConfig {
    CoreConfig {
        seed_cash: 10_000.0,
        clock: clock.boxed(),
        order_client_id_session: Some(("snapbnd".into(), 0)),
        journal: Some(journal(dir, snapshot_every)),
        ..CoreConfig::default()
    }
}

fn journal(dir: &Path, snapshot_every: u64) -> JournalConfig {
    journal_at(dir, 32 << 20, snapshot_every)
}

/// One fill of the history, a cent of fee, stamped `ts`.
fn fill(id: &str, side: i32, qty: f64, px: f64, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::new(id).expect("non-empty"),
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side,
        last_qty: qty,
        last_price: px,
        commission: 0.01,
        commission_asset: String::new().into(),
        liquidity_side: "taker".into(),
        timestamp: ts,
        mark_price: None,
        position_side: PositionSide::Both,
    }
}

fn report(f: &FillEvent) -> FillReport {
    FillReport {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        trade_id: f.trade_id.clone(),
        venue_order_id: f.trade_id.as_str().into(),
        order_client_id: None,
        side: f.side,
        last_qty: f.last_qty,
        last_price: f.last_price,
        commission: f.commission,
        commission_asset: "USDT".into(),
        liquidity_side: LiquiditySide::Taker,
        timestamp: f.timestamp,
    }
}

/// Place `order_client_id` through the door, and have the venue accept it and (`cancel`) cancel it.
fn place(handle: &CoreHandle, order_client_id: &str, cancel: bool) {
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(sim_limit(
        order_client_id,
        1,
        1.0,
        90.0,
    )))));
    let events = handle.event_sender();
    let id = order_client_id.to_string();
    events
        .blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: id.clone(),
            timestamp: 0,
        }))
        .unwrap();
    events
        .blocking_send(Event::OrderAccepted(OrderAccepted {
            order_client_id: id.clone(),
            venue_order_id: Some(format!("v-{order_client_id}").into()),
            timestamp: 0,
        }))
        .unwrap();
    if cancel {
        events
            .blocking_send(Event::OrderCanceled(OrderCanceled {
                order_client_id: id,
                reason: "user".to_string().into(),
                timestamp: 0,
                post_only_would_cross: false,
                refusal_id: None,
            }))
            .unwrap();
    }
}

fn settled(handle: &CoreHandle, what: &str, pred: impl Fn(&CoreSnapshot) -> bool) {
    wait_for_within(handle, Duration::from_secs(600), what, pred);
}

fn ended(s: &CoreSnapshot) -> usize {
    s.orders.iter().filter(|o| o.status.is_terminal()).count()
}

fn position(s: &CoreSnapshot) -> f64 {
    s.positions.iter().map(|p| p.size).sum()
}

/// The money of an account snapshot, bit for bit, and its position.
fn money(a: &EngineSnapshotAccount) -> (u64, u64, u64, u64) {
    let size: f64 = a.positions.iter().map(|(_, p)| p.size).sum();
    (a.balance.to_bits(), a.realized_pnl.to_bits(), a.fees_paid.to_bits(), size.to_bits())
}

/// The last `Snapshot` of the journal at `dir`: the record, and its size in bytes as JSON.
fn last_snap(dir: &Path) -> (JournalRecord, usize) {
    let rec = records(dir)
        .into_iter()
        .rev()
        .find(|r| matches!(r, JournalRecord::Snapshot { .. }))
        .expect("a Snapshot");
    let bytes = serde_json::to_vec(&rec).unwrap().len();
    (rec, bytes)
}

fn engines_of(rec: &JournalRecord) -> &[EngineSnapshot] {
    let JournalRecord::Snapshot { engines, .. } = rec else { unreachable!() };
    engines
}

fn recent_of(rec: &JournalRecord) -> Vec<String> {
    let JournalRecord::Snapshot { recent_order_client_ids, .. } = rec else { unreachable!() };
    recent_order_client_ids.iter().map(|(c, _)| c.clone()).collect()
}

/// RUN 1: twenty orders that end at the start, `n` fills over about `days` (alternating sides, the last one half
/// a unit, so the run ends long 0.5), two hundred orders that end ten minutes before the end and three working
/// ones at the end. Returns the fills.
fn history(handle: &CoreHandle, clock: &StepClock, n: usize, days: i64) -> Vec<FillEvent> {
    for i in 0..20 {
        place(handle, &format!("stale-{i}"), true);
    }
    settled(handle, "the stale orders", |s| ended(s) == 20);
    let step = days * MS_PER_DAY / n as i64;
    clock.step(step);
    let start = clock.now();
    let events = handle.event_sender();
    let mut fills = Vec::with_capacity(n);
    for i in 0..n {
        let qty = if i + 1 == n { 0.5 } else { 1.0 };
        let side = if i % 2 == 0 { 1 } else { -1 };
        let px = 100.0 + (i % 9) as f64;
        let f = fill(&format!("f{i:07}"), side, qty, px, start + i as i64 * step);
        events.blocking_send(Event::Fill(f.clone())).unwrap();
        fills.push(f);
    }
    settled(handle, "every fill", |s| position(s) == 0.5);
    clock.step(1);
    for i in 0..200 {
        place(handle, &format!("ended-{i}"), true);
    }
    // The twenty stale orders ended about 120 days ago, so the PUBLISHED list let them go an hour after
    // (`vike_core::snapshot::ENDED_ORDER_PUBLISH_MS`); the registry, and so the `Snapshot`, still holds all 220.
    settled(handle, "the ended orders", |s| ended(s) == 200);
    clock.jump(10 * MIN);
    for k in 0..3 {
        place(handle, &format!("work-{k}"), false);
    }
    settled(handle, "the working orders", |s| s.orders.len() == 203 && ended(s) == 200);
    fills
}

fn restore(dir: &Path, now: i64) -> RestoreReport {
    let scratch = Scratch::created("snap-bound-restore");
    restore_for_boot(Some(&journal(dir, 10_000)), &scratch, now, &[]).expect("the journal reads")
}

/// **100,000 fills, a clean stop, a restart, and the venue sending everything again.**
#[test]
fn a_restart_after_a_hundred_thousand_fills_holds_the_same_book_and_books_no_redelivery_twice() {
    let dir = Scratch::reserved("snap-bound-100k");
    let clock = StepClock::new();
    let n = 100_000;
    let handle = spawn_core(engine(), config(&dir, &clock, 10_000));
    let fills = history(&handle, &clock, n, 120);
    handle.shutdown_and_join();
    let end = clock.now();

    // ---- the exit `Snapshot`: what is live, and a window of ids.
    let (exit, bytes) = last_snap(&dir);
    let e = &engines_of(&exit)[0];
    let working: Vec<&str> = e.registry.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(working, ["work-0", "work-1", "work-2"], "the working orders and no ended one");
    assert_eq!(e.ended_orders_left_out, 220);
    let per_day = n as i64 / 120;
    let window = (WIDEST_WINDOW_DAYS as i64 + 2) * per_day;
    assert!(
        (e.seen_trade_ids.len() as i64) < window,
        "a window of ids: {}",
        e.seen_trade_ids.len()
    );
    assert!(e.seen_trade_ids.len() < n / 2);
    assert!(e.fill_floor_ms.is_some(), "the window left ids out, and says where");
    assert!(e.account.closed_pnls.len() <= JOURNAL_CLOSED_PNLS);
    let recent = recent_of(&exit);
    assert_eq!(recent.iter().filter(|c| c.starts_with("ended-")).count(), 200, "within the day");
    assert!(!recent.iter().any(|c| c.starts_with("stale-")), "past the late-retry window");
    assert_eq!(
        Journal::latest_segment_version(&dir).unwrap(),
        Some(34),
        "stamped 34: the floor since format 34, above the reports' 33"
    );
    println!(
        "SNAP-BOUND exit Snapshot {bytes} bytes: registry {} orders ({} ended left out), {} trade ids, \
         recent_order_client_ids {}",
        e.registry.len(),
        e.ended_orders_left_out,
        e.seen_trade_ids.len(),
        recent.len()
    );
    assert!(bytes < 4 << 20, "the exit Snapshot is bounded: {bytes} bytes");

    // ---- the boot restore: the same book, the same working orders, the same late-retry memory.
    let restored = restore(&dir, end);
    assert_eq!(restored.outcome(), &RestoreOutcome::Full, "{:?}", restored.notes());
    let before = money(&e.account);
    assert_eq!(money(&restored.restored_engines()[0].account), before);
    let candidates: Vec<&str> =
        restored.journal_orders().iter().map(|o| o.order_client_id.as_str()).collect();
    assert_eq!(
        candidates,
        ["work-0", "work-1", "work-2"],
        "the working orders, for the first pass"
    );
    let memory: Vec<&str> =
        restored.recent_order_client_ids().iter().map(|(c, _)| c.as_str()).collect();
    for i in 0..200 {
        let order_client_id = format!("ended-{i}");
        assert!(
            memory.contains(&order_client_id.as_str()),
            "{order_client_id} is still refused as a retry"
        );
    }
    assert!(memory.contains(&"work-1"));
    assert!(!memory.iter().any(|c| c.starts_with("stale-")), "as before the bound: past the day");

    // ---- RUN 2: the venue re-delivers the oldest and the newest fills, a pass reports them, a new fill.
    let mut config_under_test = config(&dir, &clock, 10_000);
    restored.apply_to(&mut config_under_test);
    clock.jump(MIN);
    let handle = spawn_core(engine(), config_under_test);
    let events = handle.event_sender();
    for f in fills[..5_000].iter().chain(&fills[n - 5_000..]) {
        events.blocking_send(Event::Fill(f.clone())).unwrap();
    }
    // ...and the oldest one once more WITHOUT its timestamp (a frame that omitted the field): nothing can prove
    // it new, so it is held for the operator, never booked.
    let unstamped = FillEvent { timestamp: 0, ..fills[0].clone() };
    events.blocking_send(Event::Fill(unstamped.clone())).unwrap();
    let reports: Vec<FillReport> =
        fills[..1_000].iter().chain(&fills[n - 1_000..]).map(report).collect();
    handle.send_command(Command::ReconcileReports(Box::new(ReconcileReports {
        admitted_symbols: None,
        lists_trigger_orders: false,
        venue: "sim".into(),
        since: 0,
        orders: Vec::new(),
        fills: reports,
        positions: Vec::new(),
        policy: ReconcilePolicy { default: ReconcileMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: ReconcileBalanceTolerance::default(),
        route_key: None,
    })));
    let pass = wait_for_within(&handle, Duration::from_secs(120), "the pass", |s| {
        s.reconcile.last_pass_timestamp > 0
    });
    let held: Vec<_> = pass
        .reconcile
        .alerts
        .iter()
        .filter(|a| a.kind == vike_model::ReconcileDivergenceKind::MissingFill)
        .collect();
    assert_eq!(
        held.len(),
        1,
        "no folded fill is raised as missing, the unstamped one is: {held:?}"
    );
    assert!(
        held[0].detail.contains("NO timestamp") && held[0].detail.contains("f0000000"),
        "{held:?}"
    );
    // The operator's confirm books nothing either (the engine would drop the fill: it cannot be proven new).
    handle.send_command(Command::ConfirmReconcile(held[0].id));
    settled(&handle, "the confirm", |s| {
        s.reconcile
            .alerts
            .iter()
            .all(|a| a.kind != vike_model::ReconcileDivergenceKind::MissingFill)
    });
    let held = f64::from_bits(before.3);
    events.blocking_send(Event::Fill(fill("new-1", 1, 1.0, 100.0, clock.now()))).unwrap();
    settled(&handle, "the new fill", |s| position(s) == held + 1.0);
    handle.shutdown_and_join();
    let (after, _) = last_snap(&dir);
    let (balance, realized, fees, _) = money(&engines_of(&after)[0].account);
    assert_eq!(realized, before.1, "no re-delivered fill realized anything");
    assert_eq!(
        (f64::from_bits(balance), f64::from_bits(fees)),
        (f64::from_bits(before.0) - 0.01, f64::from_bits(before.2) + 0.01),
        "only the new fill paid its fee"
    );
}

/// **The node's read-back sizes the window, and raising it across a restart books nothing twice.** A core told the
/// default one-hour reconcile lookback (`CoreConfig::reconcile_lookback_ms`) journals its trade ids as four days back
/// from the newest (`vike_execution::account::fill_window::window_days`) and says so in the `Snapshot`; the restart's
/// core is told thirty days, and the venue re-delivering every fill of the history books none of them again.
#[test]
fn the_configured_read_back_sizes_the_window_and_a_raised_one_books_no_redelivery_twice() {
    const HOUR_MS: i64 = 3_600_000;
    let dir = Scratch::reserved("snap-bound-read-back");
    let clock = StepClock::new();
    let n = 2_000;
    let mut first = config(&dir, &clock, 10_000);
    first.reconcile_lookback_ms = Some(HOUR_MS);
    let handle = spawn_core(engine(), first);
    let fills = history(&handle, &clock, n, 20);
    handle.shutdown_and_join();
    let end = clock.now();
    let (exit, _) = last_snap(&dir);
    let e = &engines_of(&exit)[0];
    assert_eq!(e.fill_window_days, Some(4), "the hour, the skew and the margin");
    assert!(e.fill_floor_ms.is_some(), "twenty days span the window");
    let per_day = n / 20;
    assert!(
        e.seen_trade_ids.len() <= 6 * per_day,
        "four days and the newest: {}",
        e.seen_trade_ids.len()
    );

    let restored = restore(&dir, end);
    assert_eq!(restored.outcome(), &RestoreOutcome::Full, "{:?}", restored.notes());
    let mut raised = config(&dir, &clock, 10_000);
    raised.reconcile_lookback_ms = Some(30 * MS_PER_DAY);
    restored.apply_to(&mut raised);
    clock.jump(MIN);
    let handle = spawn_core(engine(), raised);
    let events = handle.event_sender();
    for f in &fills {
        events.blocking_send(Event::Fill(f.clone())).unwrap();
    }
    // Behind every re-delivery in the lane: once it folds, so has every one of them.
    events.blocking_send(Event::Fill(fill("new-1", 1, 1.0, 100.0, clock.now()))).unwrap();
    settled(&handle, "the new fill after every re-delivery", |s| position(s) == 1.5);
    handle.shutdown_and_join();
    let (after, _) = last_snap(&dir);
    let e = &engines_of(&after)[0];
    assert_eq!(e.fill_window_days, Some(33), "the raised read-back's window");
    let (_, realized, _, size) = money(&e.account);
    assert_eq!(
        realized,
        money(&engines_of(&exit)[0].account).1,
        "no re-delivered fill realized anything"
    );
    assert_eq!(f64::from_bits(size), 0.5 + 1.0, "the history's half unit and the new one");
}

/// The offset of the LAST frame of `seg`, and its payload length (the frame walk `vike_journal::end_of_frames`
/// does, remembering where each frame started).
fn last_frame(seg: &Path) -> (usize, usize) {
    let bytes = std::fs::read(seg).unwrap();
    let (mut at, mut last) = (16usize, (0usize, 0usize));
    loop {
        let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        if len == 0 {
            return last;
        }
        last = (at, len);
        at += 8 + len;
    }
}

fn last_segment(dir: &Path) -> std::path::PathBuf {
    let mut segments: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "vjl"))
        .collect();
    segments.sort();
    segments.pop().unwrap()
}

/// **A crash in the middle of writing a `Snapshot`**: the exit `Snapshot`'s second half never reached the disk. The
/// restore refolds from the `Snapshot` before it, through the tail, and holds the book the core held, ended orders
/// and a window of ids left out of every `Snapshot` included.
#[test]
fn a_crash_in_the_middle_of_a_snap_restores_the_same_book_from_the_one_before() {
    let dir = Scratch::reserved("snap-bound-torn");
    let clock = StepClock::new();
    let handle = spawn_core(engine(), config(&dir, &clock, 1_000));
    let fills = history(&handle, &clock, 6_000, 120);
    handle.shutdown_and_join();
    let (exit, _) = last_snap(&dir);
    let want = money(&engines_of(&exit)[0].account);
    assert!(engines_of(&exit)[0].fill_floor_ms.is_some());

    let seg = last_segment(&dir);
    let (at, len) = last_frame(&seg);
    plant(&seg, at + 8 + len / 2, &vec![0u8; len - len / 2]);

    let restored = restore(&dir, clock.now());
    let RestoreOutcome::TornTail { refolded, .. } = restored.outcome() else {
        panic!("a torn tail: {:?}", restored.outcome())
    };
    assert!(
        *refolded,
        "the readable prefix refolds from the Snapshot before: {:?}",
        restored.notes()
    );
    let e = &restored.restored_engines()[0];
    assert_eq!(money(&e.account), want, "the same money and position");
    let working: Vec<&str> = e.registry.iter().map(|(c, _)| c.as_str()).collect();
    assert_eq!(working, ["work-0", "work-1", "work-2"]);

    // The venue sends everything again: nothing moves.
    let mut config_under_test = config(&dir, &clock, 1_000);
    restored.apply_to(&mut config_under_test);
    clock.jump(MIN);
    let handle = spawn_core(engine(), config_under_test);
    let events = handle.event_sender();
    for f in &fills {
        events.blocking_send(Event::Fill(f.clone())).unwrap();
    }
    handle.shutdown_and_join();
    let (after, _) = last_snap(&dir);
    assert_eq!(money(&engines_of(&after)[0].account), want, "no fill booked twice");
}

/// **The live core and a replay core keep the same bounded `Snapshot`**: a journal whose `Snapshot`s left ended orders
/// and a window's trade ids out replays from its FIRST `Snapshot` through every later one to the very state hash, books
/// and id counters the live core recorded last (`replay_offline`'s three fences).
#[test]
fn a_bounded_journal_passes_the_determinism_fence() {
    let dir = Scratch::reserved("snap-bound-fence");
    let clock = StepClock::new();
    let handle = spawn_core(engine(), config(&dir, &clock, 1_000));
    history(&handle, &clock, 6_000, 120);
    handle.shutdown_and_join();
    let snaps: Vec<EngineSnapshot> = records(&dir)
        .iter()
        .filter_map(|r| match r {
            JournalRecord::Snapshot { engines, .. } => Some(engines[0].clone()),
            _ => None,
        })
        .collect();
    assert!(snaps.len() > 3, "several cadence Snapshots");
    let last = snaps.last().unwrap();
    assert!(last.fill_floor_ms.is_some() && last.ended_orders_left_out == 220, "it bit");
    let out = vike_core::replay::replay_offline(&dir).expect("the replay holds every fence");
    assert_eq!(out.engines[0].ended_orders_left_out, 220);
    assert_eq!(out.engines[0].seen_trade_ids, last.seen_trade_ids, "the same window");
    assert_eq!(out.engines[0].registry, last.registry, "the same live orders");
}

/// **An emulated order survives the restart beside the history the `Snapshot` leaves out.**
#[test]
fn an_emulated_order_survives_a_restart_beside_a_left_out_history() {
    let dir = Scratch::reserved("snap-bound-emulated");
    let clock = StepClock::new();
    let handle = spawn_core(engine(), config(&dir, &clock, u64::MAX));
    for i in 0..50 {
        place(&handle, &format!("ended-{i}"), true);
    }
    settled(&handle, "the ended orders", |s| ended(s) == 50);
    clock.jump(10 * MIN);
    let pegged = OrderRequest {
        order_client_id: String::new(),
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Pegged,
        peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 }),
        ..Default::default()
    };
    handle.send_command(Command::Order(OrderIntent::Submit(Box::new(pegged))));
    place(&handle, "work-0", false);
    settled(&handle, "the emulated row", |s| {
        s.emulated_orders.iter().any(|e| e.kind == "chase") && s.orders.len() == 51
    });
    handle.shutdown_and_join();
    let (exit, _) = last_snap(&dir);
    let e = &engines_of(&exit)[0];
    assert_eq!(e.ended_orders_left_out, 50, "every ended order left out");
    assert_eq!(e.registry.len(), 1, "the working order rides");

    let restored = restore(&dir, clock.now());
    assert_eq!(restored.outcome(), &RestoreOutcome::Full);
    let mut config_under_test = config(&dir, &clock, u64::MAX);
    restored.apply_to(&mut config_under_test);
    let parent = config_under_test
        .emulated
        .first()
        .expect("the parent is restored")
        .request
        .order_client_id
        .clone();
    let handle = spawn_core(engine(), config_under_test);
    settled(&handle, "worked again, listed once", |s| {
        s.emulated_orders.iter().filter(|e| e.id == parent).count() == 1
    });
    handle.shutdown_and_join();
}
