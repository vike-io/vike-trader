//! **Order restore through a real spawned core, two boots over one state directory.**
//!
//! Boot 1 mounts a strategy that rests ONE tagged `q` limit order and shuts down WITHOUT cancelling
//! it (`CoreConfig::cancel_orders_on_shutdown` is off by default), so the order is "still on the
//! venue" and the ownership file (`order_owners.jsonl`, decision 0113) remembers who owns it and
//! under which tag. Boot 2 is a new core over the same directory with the same mount; the venue's
//! side of the story is a `Command::ReconcileReports` through the core's own command lane, exactly
//! what the reconcile manager sends after a SUCCESSFUL fetch. What the mount heard is read off a
//! shared recorder AFTER the core is joined: `shutdown_and_join` folds every message queued ahead
//! of it and runs the teardown delivery, so it is the barrier, and no test sleeps or polls for an
//! event that must NOT arrive.
//!
//! The venue gate is opened by hand (`CoreConfig::restore_venue_gate`): every row of
//! `vike_model::venues::venue_restore` is off until its demo smoke lands, so without it the hook is
//! inert by design. The white-box suite of the same hook is
//! `crates/vike-core/src/runtime/tests/restore.rs`.

use std::sync::{Arc, Mutex};

use vike_core::order_owners::OrderOwnerLog;
use vike_core::{CoreConfig, CoreHandle, LiveBroker, spawn_core};
use vike_exec::recon::{BalanceTol, RESTORE_GONE_REASON, ReconMode, ReconPolicy};
use vike_exec::testing::RecordingClient;
use vike_exec::{Command, ReconcileReports};
use vike_model::events::{Event, OrderCanceled};
use vike_model::{Bar, OrderLifecycle, OrderStatusReport, Strategy};

use crate::kit::doubles::ModifiableClient;
use crate::kit::engines::engine_on;
use crate::kit::events::{binance_quote, ohlc_bar};
use crate::kit::handle::close_binance_bar;
use crate::kit::mounts::mount_of;
use crate::scratch::Scratch;

/// The core clock of both boots: ownership records are stamped with it and the boot's time-to-live
/// is measured back from it, so nothing ages out between the two boots.
const NOW: i64 = 1_700_000_000_000;

/// What the mount heard through `on_order_event`: `(Debug of the kind, tag)`.
type Heard = Arc<Mutex<Vec<(String, Option<String>)>>>;

/// Rests one `q` buy on its first closed bar and records every lifecycle event it hears.
struct Quoter {
    placed: bool,
    heard: Heard,
}

impl Strategy<LiveBroker> for Quoter {
    fn on_bar(&mut self, broker: &mut LiveBroker, _bar: &Bar) {
        if !self.placed {
            self.placed = true;
            broker.submit_limit_tagged("q", 1, 1.0, 100.0);
        }
    }

    fn on_order_event(&mut self, _broker: &mut LiveBroker, event: &OrderLifecycle) {
        self.heard.lock().unwrap().push((format!("{:?}", event.kind), event.tag.clone()));
    }
}

/// One boot: a binance/BTCUSDT core with the [`Quoter`] mounted and the ownership file in `dir`.
fn boot(dir: &Scratch, heard: &Heard) -> CoreHandle {
    let strategy = Box::new(Quoter { placed: false, heard: Arc::clone(heard) });
    let config = CoreConfig {
        seed_cash: 10_000.0,
        clock: Box::new(|| NOW),
        strategy: Some(mount_of("binance", "BTCUSDT", "1m", strategy)),
        order_owners: Some(OrderOwnerLog::in_state_dir(dir)),
        restore_venue_gate: |_| true,
        ..CoreConfig::default()
    };
    spawn_core(engine_on("binance", "BTCUSDT", RecordingClient::default()), config)
}

/// Boot 1: rest the order, shut down without cancelling it, and return its coid.
fn rest_one_order(dir: &Scratch) -> String {
    let handle = boot(dir, &Heard::default());
    let cell = handle.snapshot_cell();
    close_binance_bar(&handle, "BTCUSDT", ohlc_bar(60_000, 100.0, 101.0, 99.0, 100.0));
    handle.shutdown_and_join();
    let snap = cell.load_full();
    assert_eq!(snap.orders.len(), 1, "boot 1 rested exactly one order");
    snap.orders[0].client_order_id.clone()
}

/// The venue's open-order row for `coid`.
fn report(coid: &str, status: &str) -> OrderStatusReport {
    OrderStatusReport {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        venue_order_id: "v-1".into(),
        client_order_id: Some(coid.to_string()),
        side: 1,
        order_type: "limit".into(),
        qty: 1.0,
        filled_qty: 0.0,
        avg_px: 0.0,
        status: status.into(),
        ts: 5,
    }
}

/// One successful pass over the venue's `orders`, as the reconcile manager builds it for a venue's
/// sole account (`route_key: None`).
fn pass(orders: Vec<OrderStatusReport>) -> Command {
    Command::ReconcileReports(Box::new(ReconcileReports {
        venue: "binance".into(),
        since: 0,
        orders,
        fills: Vec::new(),
        positions: Vec::new(),
        policy: ReconPolicy { default: ReconMode::Quarantine, ..Default::default() },
        balance: None,
        generate_missing_orders: false,
        reconcile_balance: false,
        balance_tol: BalanceTol::default(),
        route_key: None,
    }))
}

fn accepted_q() -> (String, Option<String>) {
    ("Accepted".to_string(), Some("q".to_string()))
}

/// **The headline.** The venue still has the order: after the pass the registry holds it live, the
/// mount heard `Accepted` with its tag, a second pass changes nothing, and a REAL venue cancel that
/// arrives afterwards reaches the mount's `on_order_event` WITH the tag it was submitted under.
#[test]
fn a_resting_order_is_adopted_and_its_real_cancel_reaches_the_mount_with_its_tag() {
    let dir = Scratch::reserved("vcr-restore-adopt");
    let coid = rest_one_order(&dir);

    let heard = Heard::default();
    let handle = boot(&dir, &heard);
    let cell = handle.snapshot_cell();
    handle.send_command(pass(vec![report(&coid, "ACCEPTED")]));
    handle.send_command(pass(vec![report(&coid, "ACCEPTED")])); // a second pass: no-op
    handle
        .event_sender()
        .blocking_send(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.clone(),
            reason: String::new().into(),
            ts: 9,
        }))
        .unwrap();
    handle.shutdown_and_join();

    let got = heard.lock().unwrap().clone();
    assert_eq!(got.len(), 2, "one Accepted and one Canceled, nothing twice: {got:?}");
    assert_eq!(got[0], accepted_q(), "the liveness confirmation, tagged: {got:?}");
    assert!(got[1].0.starts_with("Canceled"), "{got:?}");
    assert_eq!(got[1].1.as_deref(), Some("q"), "the REAL cancel carries the restored tag: {got:?}");

    let snap = cell.load_full();
    let adopted = snap.orders.iter().find(|o| o.client_order_id == coid);
    assert!(adopted.is_none_or(|o| !o.status.is_live()), "the cancel folded: {:?}", snap.orders);
    let notes = snap.recent_events.iter().filter(|l| l.contains("RESTORE binance")).count();
    assert_eq!(notes, 1, "one note for the one pass that did something");
}

/// **Absent at the venue.** The order is not reported (it was cancelled or filled-and-gone while the
/// process was down; a venue-side cancel that RACED the restart is exactly this): the mount hears a
/// synthetic `Canceled` carrying the restore reason and its tag, the registry never holds the
/// order, and the next boot has nothing to restore.
#[test]
fn an_order_absent_at_the_venue_is_canceled_to_its_mount_and_forgotten() {
    let dir = Scratch::reserved("vcr-restore-gone");
    let coid = rest_one_order(&dir);

    let heard = Heard::default();
    let handle = boot(&dir, &heard);
    let cell = handle.snapshot_cell();
    handle.send_command(pass(Vec::new()));
    handle.shutdown_and_join();

    let got = heard.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.starts_with("Canceled"), "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON), "the reason rides the event: {got:?}");
    assert_eq!(got[0].1.as_deref(), Some("q"), "stamped with the restored tag: {got:?}");
    assert!(cell.load_full().orders.is_empty(), "a gone order is never adopted");

    // Boot 3: the file forgot the order, so nothing is told to anybody.
    let again = Heard::default();
    let handle = boot(&dir, &again);
    handle.send_command(pass(Vec::new()));
    handle.shutdown_and_join();
    assert!(again.lock().unwrap().is_empty(), "{coid} was forgotten: {:?}", again.lock().unwrap());
}

/// **The venue reports the order closed** (its history row says `CANCELED`): a terminal report is
/// proof, so the mount hears the same synthetic `Canceled` and the order is not adopted.
#[test]
fn an_order_the_venue_reports_closed_is_canceled_to_its_mount() {
    let dir = Scratch::reserved("vcr-restore-closed");
    let coid = rest_one_order(&dir);

    let heard = Heard::default();
    let handle = boot(&dir, &heard);
    let cell = handle.snapshot_cell();
    handle.send_command(pass(vec![report(&coid, "CANCELED")]));
    handle.shutdown_and_join();

    let got = heard.lock().unwrap().clone();
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].0.contains(RESTORE_GONE_REASON) && got[0].1.as_deref() == Some("q"), "{got:?}");
    assert!(cell.load_full().orders.is_empty(), "not adopted");
}

/// One boot of a `SpreadMaker` mount over `dir`'s ownership file, on a venue client that accepts and
/// re-prices every order. No `state_dir` is set, so NO maker state is saved at teardown or loaded at
/// boot: a restarted maker comes up with `placed == false` on both sides, which is exactly the
/// state of a maker that placed its quote AFTER its last periodic state save and then crashed.
fn maker_boot(dir: &Scratch) -> CoreHandle {
    let config = CoreConfig {
        seed_cash: 1.0,
        clock: Box::new(|| NOW),
        strategy: Some(mount_of(
            "binance",
            "BTCUSDT",
            "1m",
            Box::new(vike_mm::SpreadMaker::new(1.0, 0.5)),
        )),
        order_owners: Some(OrderOwnerLog::in_state_dir(dir)),
        restore_venue_gate: |_| true,
        ..CoreConfig::default()
    };
    spawn_core(engine_on("binance", "BTCUSDT", ModifiableClient::default()), config)
}

/// `binance_quote` moved to a 110.0 / 110.2 touch, so a re-price is visible in the order prices.
fn moved_quote(ts: i64) -> vike_exec::QuoteUpdate {
    let mut q = binance_quote(ts);
    q.quote.bid = 110.0;
    q.quote.ask = 110.2;
    q
}

/// **The crash window the saved state cannot close.** Boot 1's maker rests a bid and an ask and
/// dies without a state save, so boot 2's maker comes up unplaced. Its first quote tick lands BEFORE
/// the first reconcile pass: the core refuses the two submits (a restored order still holds each
/// tag), so no second order exists. The pass adopts both resting orders and tells the maker through
/// the synthetic tagged `Accepted`; the maker adopts them, and its next tick RE-PRICES the original
/// orders in place instead of quoting beside them. Without either half, the registry ends with four
/// orders (a second pair, or the adopted pair orphaned beside a fresh one).
#[test]
fn an_unplaced_maker_adopts_the_restored_quotes_and_never_quotes_beside_them() {
    let dir = Scratch::reserved("vcr-restore-maker-adopts");
    let handle = maker_boot(&dir);
    let cell = handle.snapshot_cell();
    handle.tick_sender().quote(binance_quote(1)).unwrap();
    handle.shutdown_and_join();
    let first = cell.load_full();
    assert_eq!(first.orders.len(), 2, "boot 1 rested a bid and an ask: {:?}", first.orders);
    let mut originals: Vec<String> =
        first.orders.iter().map(|o| o.client_order_id.clone()).collect();
    originals.sort();

    let handle = maker_boot(&dir);
    let cell = handle.snapshot_cell();
    let ticks = handle.tick_sender();
    ticks.quote(binance_quote(2)).unwrap(); // BEFORE the pass: the submits are refused
    // The venue's rows carry each order's real side, as the engine's own record of boot 1 has it.
    let reports: Vec<_> = first
        .orders
        .iter()
        .map(|o| OrderStatusReport { side: o.side, ..report(&o.client_order_id, "ACCEPTED") })
        .collect();
    handle.send_command(pass(reports));
    ticks.quote(moved_quote(3)).unwrap(); // AFTER the pass: the maker re-prices what it adopted
    handle.shutdown_and_join();

    let snap = cell.load_full();
    assert!(snap.fault.is_none(), "no fault: {:?}", snap.fault);
    let mut now_resting: Vec<String> =
        snap.orders.iter().map(|o| o.client_order_id.clone()).collect();
    now_resting.sort();
    assert_eq!(
        now_resting, originals,
        "no second order beside the restored pair: {:?}",
        snap.orders
    );
    let mid = 0.5 * (110.0_f64 + 110.2);
    let bid = snap.orders.iter().find(|o| o.side == 1).expect("the bid");
    let ask = snap.orders.iter().find(|o| o.side == -1).expect("the ask");
    assert_eq!(
        bid.price.unwrap().to_bits(),
        (mid - 0.5).to_bits(),
        "the adopted bid was re-priced"
    );
    assert_eq!(
        ask.price.unwrap().to_bits(),
        (mid + 0.5).to_bits(),
        "the adopted ask was re-priced"
    );
}
