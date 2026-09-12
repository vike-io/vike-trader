//! Live-parameter plane (audit co8): a mounted strategy is re-tuned mid-session via
//! `Command::UpdateParams` WITHOUT unmounting — so it keeps its resting orders / venue queue
//! position. Driven through the REAL runtime (spawn_core + tick lane + command lane + snapshot),
//! mirroring the runtime_smoke / StrategyMount harness. The epoch bump + the atomic field swap are
//! unit-tested next to `SpreadMaker` (now in the vike-mm crate); here we prove the END-TO-END effect:
//! the new half_spread/qty take effect on the NEXT quote, in place (modify, never cancel+resubmit).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use vike_core::{
    CoreConfig, LiveBroker, SpreadMakerParams, StrategyMount, StrategyParams, spawn_core,
};
use vike_exec::{
    Account, BalanceMode, Command, ExecutionClient, ExecutionEngine, OrderStatus, ParamsUpdate,
    QuoteUpdate, RiskGate, RiskLimits,
};
// `SpreadMaker` + `QuoteStyle` moved to vike-mm; vike-core dev-deps it so this runtime test mounts
// the maker end-to-end.
use vike_mm::{QuoteStyle, SpreadMaker};
use vike_model::events::{Event, OrderAccepted, OrderModified, OrderSubmitted};
use vike_model::{OrderRequest, QuoteTick, Strategy};

/// Accepts every order (so it rests, modifiable) and echoes `OrderModified` on modify — enough to
/// drive the runtime's tag→coid resolution, `engine.modify_order`, and the FSM self-transition fold
/// (the same shape as runtime_smoke's `ModifiableClient`). A modify updates the registry in place,
/// so the snapshot's order price/qty reflect a re-price WITHOUT a cancel+resubmit.
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

fn engine() -> ExecutionEngine<ModifiableClient> {
    ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        ModifiableClient::default(),
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

fn quote(ts: i64) -> QuoteUpdate {
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

fn update(params: StrategyParams) -> Command {
    Command::UpdateParams(Box::new(ParamsUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        params,
    }))
}

/// THE gate: a mounted `SpreadMaker` receives an `UpdateParams` mid-session; on the next quote the
/// new half_spread/qty take effect — re-priced IN PLACE (still exactly two Accepted orders, never
/// canceled + resubmitted), i.e. WITHOUT unmounting and without a spurious cancel.
#[test]
fn mounted_spreadmaker_retunes_live_without_unmount_or_cancel() {
    let handle = spawn_core(engine(), config(Box::new(SpreadMaker::new(1.0, 0.5))));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    // rest bid/ask off the first quotes (mid 100.1, half_spread 0.5 → bid 99.6 / ask 100.6, qty 1.0)
    ticks.quote(quote(1)).unwrap(); // submit tagged bid + ask (both Accepted)
    ticks.quote(quote(2)).unwrap(); // modify both in place (unchanged params)

    // live re-tune mid-session, on the same ingest lane (lossless, ordered after the quotes above):
    // widen half_spread 0.5 → 1.0 and grow qty 1.0 → 2.0.
    handle.send_command(update(StrategyParams::SpreadMaker(SpreadMakerParams {
        qty: 2.0,
        half_spread: 1.0,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    })));

    // the FIRST quote after the update: the maker re-prices/-sizes its resting orders in place.
    ticks.quote(quote(3)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    // STILL exactly the two resting orders — a cancel+resubmit would show 4 orders and/or Canceled.
    assert_eq!(snap.orders.len(), 2, "re-tuned in place — no cancel + resubmit");
    assert!(
        snap.orders.iter().all(|o| o.status == OrderStatus::Accepted),
        "both sides still resting after the re-tune (no spurious cancel): {:?}",
        snap.orders.iter().map(|o| (o.side, o.status)).collect::<Vec<_>>()
    );
    let bid = snap.orders.iter().find(|o| o.side == 1).expect("resting bid");
    let ask = snap.orders.iter().find(|o| o.side == -1).expect("resting ask");
    // new mid 100.1 ± 1.0, qty 2.0 (same-expression bit compare dodges literal ULP drift)
    let mid = 0.5 * (100.0_f64 + 100.2);
    assert_eq!(
        bid.price.unwrap().to_bits(),
        (mid - 1.0).to_bits(),
        "bid re-priced at the NEW half_spread"
    );
    assert_eq!(
        ask.price.unwrap().to_bits(),
        (mid + 1.0).to_bits(),
        "ask re-priced at the NEW half_spread"
    );
    assert_eq!(bid.qty.to_bits(), 2.0_f64.to_bits(), "bid re-sized to the NEW qty");
    assert_eq!(ask.qty.to_bits(), 2.0_f64.to_bits(), "ask re-sized to the NEW qty");
}

/// A strategy that does NOT consume the hook (uses the default no-op `on_params_updated`) is
/// UNAFFECTED: an `UpdateParams` targeting its mount neither faults the core nor perturbs its
/// dispatch — quotes before and after are handled identically, and it mints no orders.
#[derive(Default)]
struct QuoteCounter {
    quotes: Arc<AtomicUsize>,
}
impl Strategy<LiveBroker> for QuoteCounter {
    fn on_quote_tick(&mut self, _broker: &mut LiveBroker, _q: &QuoteTick) {
        self.quotes.fetch_add(1, Ordering::Relaxed);
    }
    // deliberately does NOT override on_params_updated → the default no-op runs
}

#[test]
fn update_params_to_a_non_consuming_strategy_is_harmless() {
    let quotes = Arc::<AtomicUsize>::default();
    let handle =
        spawn_core(engine(), config(Box::new(QuoteCounter { quotes: Arc::clone(&quotes) })));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(quote(1)).unwrap();
    // an update this strategy ignores — must not fault or disturb the following dispatch
    handle.send_command(update(StrategyParams::SpreadMaker(SpreadMakerParams {
        qty: 9.0,
        half_spread: 9.0,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    })));
    ticks.quote(quote(2)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "the update must not fault the core: {:?}", snap.fault);
    assert_eq!(quotes.load(Ordering::Relaxed), 2, "both quotes handled — dispatch undisturbed");
    assert!(snap.orders.is_empty(), "a non-consuming strategy mints no orders on a param update");
}

/// A param update targeting a mount that does NOT exist (wrong interval) is a silent no-op — never
/// a fault, never misrouted to the wrong mount.
#[test]
fn update_params_to_an_unknown_mount_is_a_silent_noop() {
    let quotes = Arc::<AtomicUsize>::default();
    let handle =
        spawn_core(engine(), config(Box::new(QuoteCounter { quotes: Arc::clone(&quotes) })));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(quote(1)).unwrap();
    // interval "5m" — no mount on (binance, BTCUSDT, 5m); the mount is "1m"
    handle.send_command(Command::UpdateParams(Box::new(ParamsUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "5m".into(),
        params: StrategyParams::SpreadMaker(SpreadMaker::new(2.0, 2.0).params()),
    })));
    ticks.quote(quote(2)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "unknown-mount update must not fault: {:?}", snap.fault);
    assert_eq!(quotes.load(Ordering::Relaxed), 2, "dispatch to the real mount is undisturbed");
}

/// The READ half of the same plane (`Strategy::params` → `MountView::params`): after an
/// `UpdateParams` the published snapshot reports the NEW tunables, not the boot ones. This is the
/// property the whole read exists for — the daemon's process-static mount block is stale the moment
/// the first re-tune lands, so a read-modify-write built on it silently reverts the first writer.
///
/// Asserted through the REAL runtime (the same spawn_core/command-lane harness as the write test
/// above), because the cold-path fill lives in the runtime's `mount_views`, not in the maker.
#[test]
fn a_retuned_mount_publishes_its_new_params_not_its_boot_params() {
    let handle = spawn_core(engine(), config(Box::new(SpreadMaker::new(1.0, 0.5))));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(quote(1)).unwrap();
    handle.send_command(update(StrategyParams::SpreadMaker(SpreadMakerParams {
        qty: 2.0,
        half_spread: 1.0,
        ..SpreadMaker::new(1.0, 0.5).params()
    })));
    ticks.quote(quote(2)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    let mount = snap
        .mounts
        .iter()
        .find(|m| m.kind == vike_core::MountRowKind::Mount)
        .expect("the mounted maker publishes a row");
    let StrategyParams::SpreadMaker(p) = mount.params.clone().expect("the maker publishes params")
    else {
        panic!("a SpreadMaker mount reports its own variant, never another family's");
    };
    // The values the RE-TUNE carried, never the `SpreadMaker::new(1.0, 0.5)` the mount was built
    // with — same-expression bit compares, the file's convention.
    assert_eq!(p.qty.to_bits(), 2.0_f64.to_bits(), "the published qty is the RE-TUNED one");
    assert_eq!(
        p.half_spread.to_bits(),
        1.0_f64.to_bits(),
        "the published half_spread is the RE-TUNED one"
    );
}

/// ...and the twin: a strategy that overrides nothing publishes `None`. `None` is a FIRST-CLASS
/// answer rather than a gap — the mounts that can answer this read are exactly the mounts
/// `Command::UpdateParams` can address, so a strategy with no `StrategyParams` variant is honestly
/// unreadable AND unwritable by this plane. A client that could not tell those apart would offer a
/// tuning surface on a mount no write can reach.
#[test]
fn a_strategy_overriding_nothing_publishes_no_params() {
    let quotes = Arc::<AtomicUsize>::default();
    let handle =
        spawn_core(engine(), config(Box::new(QuoteCounter { quotes: Arc::clone(&quotes) })));
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();

    ticks.quote(quote(1)).unwrap();
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    let mount = snap
        .mounts
        .iter()
        .find(|m| m.kind == vike_core::MountRowKind::Mount)
        .expect("the mount publishes a row whether or not it has params");
    assert!(
        mount.params.is_none(),
        "the trait default reports NOTHING rather than a fabricated bag: {:?}",
        mount.params
    );
    // ...and the RESIDUAL row, which describes no mount at all, is `None` for the same reason.
    assert!(
        snap.mounts
            .iter()
            .filter(|m| m.kind == vike_core::MountRowKind::Residual)
            .all(|m| m.params.is_none()),
        "the account-wide residual row has no strategy to ask"
    );
}
