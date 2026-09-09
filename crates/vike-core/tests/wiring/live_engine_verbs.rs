//! Phase C gates: the live `order_target_*` verbs (port of
//! `exec/live_portfolio_engine.py`) and the ConditionalBook emulator (port of
//! `exec/conditionals.py`) through the REAL runtime — spawn_core + bar lane + snapshot.
//! Orders are asserted via the snapshot's registry view (`CoreSnapshot.orders`), the same
//! surface the GUI reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use vike_core::{CoreConfig, LiveBroker, StrategyMount, spawn_core};
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, BalanceMode, BarUpdate, ExecutionEngine, RiskGate, RiskLimits};
use vike_model::{Bar, Strategy};

fn bar(ts: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts,
        open,
        high,
        low,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn engine() -> ExecutionEngine<RecordingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn config(seed_cash: f64, strategy: Box<dyn Strategy<LiveBroker> + Send>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash,
        clock: Box::new(move || t.fetch_add(1, Ordering::Relaxed)),
        strategy: Some(StrategyMount {
            account: None,
            symbols: Vec::new(),
            controller_id: None,
            underlying_symbol: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            strategy,
        }),
        ..CoreConfig::default()
    }
}

fn close_bar(handle: &vike_core::CoreHandle, b: Bar) {
    handle
        .bar_sender()
        .close(BarUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            bar: b,
        })
        .unwrap();
}

// ---- order_target_percent --------------------------------------------------------------

/// Calls order_target_percent(0.5) on the first bar only.
struct TargetHalf {
    done: bool,
}
impl Strategy<LiveBroker> for TargetHalf {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.done {
            self.done = true;
            broker.order_target_percent(0.5);
        }
    }
}

#[test]
fn order_target_percent_sizes_from_equity() {
    let handle = spawn_core(engine(), config(10_000.0, Box::new(TargetHalf { done: false })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0, 101.0, 99.0, 100.0));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1, "one rebalance order expected");
    let o = &snap.orders[0];
    // oracle law: units_from_percent(0.5, 10_000, 100, 1) = 50, flat position -> delta 50
    assert_eq!(o.side, 1);
    assert!((o.qty - 50.0).abs() < 1e-12, "qty {}", o.qty);
    assert_eq!(o.order_type, "market");
}

/// order_target to the CURRENT position must be a no-op (1e-12 dead-band).
struct TargetFlatNoop;
impl Strategy<LiveBroker> for TargetFlatNoop {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        broker.order_target(0.0); // already flat
        broker.order_target_value(0.0); // 0 notional at px>0 -> target 0 -> no-op
    }
}

#[test]
fn order_target_at_position_is_noop() {
    let handle = spawn_core(engine(), config(10_000.0, Box::new(TargetFlatNoop)));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0, 101.0, 99.0, 100.0));
    handle.shutdown_and_join();
    assert!(cell.load_full().orders.is_empty());
}

// ---- ConditionalBook: stop -------------------------------------------------------------

/// Arms a protective sell-stop at 95 on the first bar, then does nothing.
struct ArmStop {
    armed: bool,
}
impl Strategy<LiveBroker> for ArmStop {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.armed {
            self.armed = true;
            broker.submit_stop(-1, 2.0, 95.0);
        }
    }
}

#[test]
fn armed_stop_fires_on_the_crossing_bar_only() {
    let handle = spawn_core(engine(), config(10_000.0, Box::new(ArmStop { armed: false })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0, 101.0, 99.0, 100.0)); // arm (low 99 > 95)
    close_bar(&handle, bar(120_000, 100.0, 100.5, 97.0, 98.0)); // still above: no fire
    close_bar(&handle, bar(180_000, 96.0, 97.0, 94.0, 94.5)); // crosses 95: fire
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1, "exactly the fired conditional");
    let o = &snap.orders[0];
    assert_eq!(o.side, -1);
    assert!((o.qty - 2.0).abs() < 1e-12);
    assert_eq!(o.order_type, "market"); // oracle: fires as a plain market through the gate
}

// ---- ConditionalBook: trailing ratchet -------------------------------------------------

/// Arms a trailing stop (trail 5) on the first bar; the extreme must ratchet on new highs.
struct ArmTrailing {
    armed: bool,
}
impl Strategy<LiveBroker> for ArmTrailing {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.armed {
            self.armed = true;
            broker.submit_trailing(-1, 1.0, 5.0);
        }
    }
}

#[test]
fn trailing_stop_ratchets_then_fires() {
    let handle = spawn_core(engine(), config(10_000.0, Box::new(ArmTrailing { armed: false })));
    let cell = handle.snapshot_cell();
    // arm on close 100 -> extreme seeds from the mark (bar close), trigger 95
    close_bar(&handle, bar(60_000, 100.0, 100.0, 100.0, 100.0));
    // ratchet: high 110 (low 101 stays above old trigger 95) -> trigger becomes 105
    close_bar(&handle, bar(120_000, 105.0, 110.0, 101.0, 108.0));
    // low 104 crosses the RATCHETED trigger 105 -> fire
    close_bar(&handle, bar(180_000, 106.0, 107.0, 104.0, 104.5));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1, "trailing stop fired once");
    assert_eq!(snap.orders[0].side, -1);
}

// ---- mass_cancel clears the book -------------------------------------------------------

/// Arms a stop on bar 0, mass-cancels on bar 1 — the stop must NOT fire on bar 2.
struct ArmThenCancel {
    step: u32,
}
impl Strategy<LiveBroker> for ArmThenCancel {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        match self.step {
            0 => broker.submit_stop(-1, 2.0, 95.0),
            1 => broker.mass_cancel(),
            _ => {}
        }
        self.step += 1;
    }
}

#[test]
fn mass_cancel_clears_the_conditional_book() {
    let handle = spawn_core(engine(), config(10_000.0, Box::new(ArmThenCancel { step: 0 })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 100.0, 101.0, 99.0, 100.0)); // arm
    close_bar(&handle, bar(120_000, 100.0, 100.5, 99.0, 100.0)); // cancel-all
    close_bar(&handle, bar(180_000, 90.0, 91.0, 89.0, 90.0)); // would have fired
    handle.shutdown_and_join();

    assert!(cell.load_full().orders.is_empty(), "cleared stop must never fire");
}

// ---- set_holdings (margin-aware, lot grid) ----------------------------------------------

/// Calls set_holdings(0.5) on the first bar only.
struct SetHoldingsHalf {
    done: bool,
}
impl Strategy<LiveBroker> for SetHoldingsHalf {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.done {
            self.done = true;
            broker.set_holdings(0.5);
        }
    }
}

#[test]
fn set_holdings_lands_under_target_on_the_lot_grid() {
    // LEAN law: target notional 0.5*10_000 = 5_000 at price 300 = 16.67 units ->
    // lands UNDER on the 1.0-lot grid: exactly 16, never 17
    let mut eng = engine();
    eng.gate.limits.lot_size = Some(1.0);
    let handle = spawn_core(eng, config(10_000.0, Box::new(SetHoldingsHalf { done: false })));
    let cell = handle.snapshot_cell();
    close_bar(&handle, bar(60_000, 300.0, 301.0, 299.0, 300.0));
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1);
    assert!((snap.orders[0].qty - 16.0).abs() < 1e-12, "qty {}", snap.orders[0].qty);
}
