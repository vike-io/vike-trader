//! The live `Strategy::on_fill` write-once fill lane, and the equity-drawdown latch.

use super::*;
use crate::kit::handle::{close_binance_bar, wait_for_snapshot};
use vike_marketdata::test_support::flat_bar_unit_volume;

// ---- Live Strategy::on_fill delivery (the write-once fill lane) ----

/// Records every `on_fill` delivery; reacts to the FIRST fill with a buffered market order to
/// prove the on_fill context drains through the one live path like every other handler.
struct FillRecorder {
    fills: Arc<Mutex<Vec<Fill>>>,
    /// Rests ONE limit order on the first closed bar: the order whose coid the fills below carry,
    /// because a fill only reaches the mount that MINTED its order (decision 0116).
    quoted: bool,
}
impl Strategy<LiveBroker> for FillRecorder {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &vike_model::Bar) {
        if !self.quoted {
            self.quoted = true;
            broker.submit_limit("BTCUSDT", 1, 1.0, 99.0);
        }
    }
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

/// The live `on_fill` gate: exactly one delivery per ACCOUNT-APPLIED fill — a reconnect replay
/// (same trade_id) is deduped BEFORE dispatch, an other-symbol fill never reaches the strategy,
/// and a handler that submits from `on_fill` flows the one live path without deadlock.
#[test]
fn live_on_fill_fires_once_per_applied_fill() {
    let engine = engine_on("binance", "BTCUSDT", RecordingClient::default());
    let fills = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(mount_of(
        "binance",
        "BTCUSDT",
        "1m",
        Box::new(FillRecorder { fills: Arc::clone(&fills), quoted: false }),
    ));
    let handle = spawn_core(engine, config);
    let cell = handle.snapshot_cell();
    let events = handle.event_sender();

    // the strategy rests its own order; the fills below belong to THAT order
    close_binance_bar(&handle, "BTCUSDT", flat_bar_unit_volume(60_000, 100.0));
    wait_for_snapshot(&cell, 10, "the strategy's order to rest", |s| s.orders.len() == 1);
    let mine = cell.load_full().orders[0].client_order_id.clone();
    events.blocking_send(ext_fill(&mine, "t1", "BTCUSDT", 2.0, 100.0)).unwrap();
    // reconnect replay: same trade_id — the account fold dedups, so NO second on_fill
    events.blocking_send(ext_fill(&mine, "t1", "BTCUSDT", 2.0, 100.0)).unwrap();
    // account-wide stream noise: another symbol's fill never reaches this strategy
    events.blocking_send(ext_fill(&mine, "t2", "ETHUSDT", 1.0, 50.0)).unwrap();
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
        2,
        "the order submitted FROM on_fill flowed mint -> gate -> client (beside the resting one)"
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
    fn submit(&mut self, request: &OrderRequest) {
        // both fills belong to the submitted order, as a venue's would
        for id in ["d1", "d2"] {
            let mut f = sim_fill(id, 2.0, 100.0);
            f.client_order_id = request.client_order_id.clone();
            self.events.push_back(Event::Fill(f));
        }
    }
    fn cancel(&mut self, _client_order_id: &str) {}
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

/// Records `broker.position()` at each on_fill delivery.
struct PositionRecorder {
    positions: Arc<Mutex<Vec<f64>>>,
    /// Submits the order the client fills on the first closed bar: a fill only reaches the mount
    /// that MINTED its order (decision 0116), so the strategy has to be the one that asks.
    ordered: bool,
}
impl Strategy<LiveBroker> for PositionRecorder {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &vike_model::Bar) {
        if !self.ordered {
            self.ordered = true;
            broker.submit_limit("BTCUSDT", 1, 4.0, 100.0);
        }
    }
    fn on_fill(&mut self, broker: &mut LiveBroker, _fill: &Fill) {
        self.positions.lock().unwrap().push(broker.position("BTCUSDT"));
    }
}

/// Backtest-parity gate for multi-fill batches: each on_fill must see the position after
/// ITS fill (2.0 then 4.0), not the post-batch state (4.0 twice) — the state snapshot is
/// taken per fill at the apply_fill fold, like the backtest engine's synchronous firing.
#[test]
fn on_fill_sees_per_fill_state_in_a_multi_fill_batch() {
    let engine = sim_engine_with(DoubleFillClient::default());
    let positions = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(mount_of(
        "sim",
        "BTCUSDT",
        "1m",
        Box::new(PositionRecorder { positions: Arc::clone(&positions), ordered: false }),
    ));
    let handle = spawn_core(engine, config);
    dd_close(&handle.bar_sender(), 60_000, 100.0); // the strategy orders 4, the client fills 2 + 2
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
    let engine = sim_engine_with(BarFillClient::default());
    let views = Arc::new(Mutex::new(Vec::new()));
    let mut config = test_config(1000.0);
    config.strategy = Some(mount_of(
        "sim",
        "BTCUSDT",
        "1m",
        Box::new(FillViewRecorder { views: Arc::clone(&views), submitted: false }),
    ));
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
