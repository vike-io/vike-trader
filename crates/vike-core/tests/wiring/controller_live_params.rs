//! Live-parameter plane for the position-executor `ControllerHarness` (stage 6) — the controller →
//! executor twin of `live_params.rs`. A mounted `ControllerHarness<MomentumController>` is re-tuned
//! mid-session via `Command::UpdateParams` carrying a `StrategyParams::PositionController` payload,
//! driven through the REAL runtime (spawn_core + tick lane + command lane + snapshot). No new runtime
//! machinery: the harness IS a `Strategy`, so `drive_strategy_params` routes the update to its
//! `on_params_updated` exactly as it does for the maker. Two proofs the unit tests (in
//! `vike-model/src/controller.rs`) cannot show end-to-end:
//!  1. the re-tune lands through the plumbing — a size grown BEFORE any open takes effect on the next
//!     opened entry (routing + hot-swap actually reached the mounted harness); and
//!  2. a re-tune DURING an in-flight entry does not tear it down or resize it (in-flight keeps its
//!     armed intent; only future opens use the new params) — and never faults the core.
//!
//! The `MomentumController` reads `broker.price(symbol)`, which the runtime sets to the quote mid on
//! the tick path, so a quote sequence with a rising mid drives its open decision. The harness keys
//! its executors off `QuoteTick.symbol`, so the test populates it (the routing symbol otherwise rides
//! the `QuoteUpdate` wrapper and the tick carries an empty symbol on single-symbol live paths).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use vike_core::{
    ControllerParams, CoreConfig, LiveBroker, StrategyMount, StrategyParams, spawn_core,
};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, OrderStatus, ParamsUpdate,
    QuoteUpdate, RiskGate, RiskLimits,
};
use vike_model::events::{Event, OrderAccepted, OrderSubmitted};
use vike_model::{OrderRequest, QuoteTick, Strategy, TripleBarrier};
use vike_strategy::{ControllerHarness, MomentumController};

/// Accepts every order (so a market entry rests as `Accepted` in the snapshot) — no fills, so an
/// opened executor stays `EntryWorking` with an observable in-flight entry order. The same
/// accept-only shape `live_params.rs` uses.
#[derive(Default)]
struct AcceptingClient {
    events: VecDeque<Event>,
}
impl ExecutionClient for AcceptingClient {
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
    fn poll_events(&mut self) -> Option<Event> {
        self.events.pop_front()
    }
}

fn engine() -> ExecutionEngine<AcceptingClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        AcceptingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn config(strategy: Box<dyn Strategy<LiveBroker> + Send>) -> CoreConfig {
    let t = Arc::new(AtomicI64::new(0));
    CoreConfig {
        seed_cash: 1.0,
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

/// A quote whose mid is exactly `px` (bid == ask), tagged with the symbol the harness keys on.
fn quote_at(ts: i64, px: f64) -> QuoteUpdate {
    QuoteUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: px,
            ask: px,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: "BTCUSDT".into(),
        },
    }
}

/// A boxed harness: momentum `threshold`, intent size `qty`, post-exit `cooldown_ms`, TP +10.
fn harness_boxed(
    qty: f64,
    threshold: f64,
    cooldown_ms: i64,
) -> Box<dyn Strategy<LiveBroker> + Send> {
    let ctl =
        MomentumController::new(qty, threshold, TripleBarrier::new(Some(10.0), None, None, None));
    Box::new(ControllerHarness::new(ctl, "binance", cooldown_ms))
}

fn update(params: StrategyParams) -> Command {
    Command::UpdateParams(Box::new(ParamsUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params,
    }))
}

/// THE gate: a mounted `ControllerHarness` receives an `UpdateParams` on the shared ingest lane
/// BEFORE it opens anything; the grown intent size takes effect on the position it opens next — i.e.
/// the re-tune routed to the mounted harness and hot-swapped its controller WITHOUT unmounting.
#[test]
fn mounted_controller_harness_retunes_intent_size_live() {
    let handle = spawn_core(engine(), config(harness_boxed(1.0, 0.0, 0)));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // first quote sets the momentum reference (no open on the first invitation)
    ticks.quote(quote_at(1, 100.0)).unwrap();
    // live re-tune (same ingest lane, ordered after the quote above): grow the intent size 1 -> 5
    handle.send_command(update(StrategyParams::PositionController(ControllerParams::new(
        0,
        5.0,
        TripleBarrier::new(Some(10.0), None, None, None),
        0.0,
    ))));
    // the next rising quote opens a long — at the NEW size, proving the swap landed through the runtime
    ticks.quote(quote_at(2, 101.0)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    assert_eq!(snap.orders.len(), 1, "one entry opened after the re-tune");
    let entry = &snap.orders[0];
    assert_eq!(entry.side, 1, "a long entry (rising mid past threshold 0)");
    assert_eq!(
        entry.qty.to_bits(),
        5.0_f64.to_bits(),
        "entry uses the hot-swapped size — the UpdateParams reached the mounted harness"
    );
    assert_eq!(
        entry.status,
        OrderStatus::Accepted,
        "entry resting; the harness stayed mounted through the update (no unmount)"
    );
}

/// A re-tune DURING an in-flight entry must not tear it down or resize it: the executor keeps its
/// original size, no second entry is minted, and the core never faults — the "re-tune only affects
/// future opens; resting work is undisturbed" rule, proven end-to-end.
#[test]
fn retune_does_not_tear_down_the_in_flight_entry() {
    let handle = spawn_core(engine(), config(harness_boxed(1.0, 0.0, 0)));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(quote_at(1, 100.0)).unwrap(); // momentum reference
    ticks.quote(quote_at(2, 101.0)).unwrap(); // open long, size 1 -> in-flight entry (Accepted)

    // grow the size 1 -> 9 mid-flight: must NOT resize or cancel the working entry.
    handle.send_command(update(StrategyParams::PositionController(ControllerParams::new(
        0,
        9.0,
        TripleBarrier::new(Some(99.0), None, None, None),
        0.0,
    ))));
    ticks.quote(quote_at(3, 102.0)).unwrap(); // drives the in-flight executor; opens no 2nd for the pair
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "the re-tune must not fault the core: {:?}", snap.fault);
    assert_eq!(
        snap.orders.len(),
        1,
        "still exactly the one in-flight entry — not torn down, and no second opened"
    );
    let entry = &snap.orders[0];
    assert_eq!(
        entry.qty.to_bits(),
        1.0_f64.to_bits(),
        "in-flight entry keeps its ORIGINAL size (re-tune only affects future opens)"
    );
    assert_eq!(
        entry.status,
        OrderStatus::Accepted,
        "in-flight entry still resting — the re-tune did not cancel it"
    );
}
