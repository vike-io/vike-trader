//! **An ended order's engine row and mount entry retire, and its late fill still finds both.**
//!
//! `order_client_id_venue` got a row per order routed to a non-primary engine and lost none, so a strategy on a
//! labelled account that minted a million orders held a million rows for the life of the process. A row now retires
//! with the order's `order_client_id_mount` entry `ORDER_CLIENT_ID_PRUNE_LINGER_MS` after the order ended
//! (`prune_terminal_order_client_ids`), and every read asks `engine_of_order_client_id`, which answers for a row
//! that is gone: the engine whose registry holds the order, else the one engine whose account code an id of ours
//! names. A fill's mount asks `mount_of_fill`: its entry, else the one live mount whose strategy code the id names.
//!
//! The harness: two accounts of ONE exchange on one symbol (so neither the venue nor the symbol can tell them apart)
//! on a venue whose v2 ids carry the account code, and a coded strategy mount on the labelled one.

use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
use vike_model::events::{FillEvent, OrderCanceled, OrderRejected, PositionSide, TradeId};
use vike_model::orders::client_id::{OrderClientIdAccountCode, OrderClientIdStrategyCode};

/// A venue whose ids carry the account code (every venue but Binance's compact shape).
const VENUE: &str = "bybit";

/// What the mount's strategy heard: the size of each fill.
type Heard = Arc<std::sync::Mutex<Vec<f64>>>;

/// Quotes one limit per `on_feed_status` and records every fill it hears.
struct Quoter {
    heard: Heard,
}

impl Strategy<LiveBroker> for Quoter {
    fn on_feed_status(&mut self, broker: &mut LiveBroker, _status: FeedStatus) {
        broker.submit_limit_tagged("bid", 1, 1.0, 100.0);
    }

    fn on_fill(&mut self, _broker: &mut LiveBroker, fill: &Fill) {
        self.heard.lock().unwrap().push(fill.size);
    }
}

/// One engine of [`VENUE`] under `route_key`, told the account code `code`.
fn coded_engine(route_key: &str, code: i64) -> Engine<RecordingClient> {
    let mut e = Engine::new(
        Account::new(1_000.0, VENUE, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        VENUE,
        SYMBOL,
    );
    e.route_key = route_key.to_string();
    e.account_code = OrderClientIdAccountCode::of_id(code);
    e.collect_applied_fills = true;
    e
}

/// The default account at engine 0 (code 01), `ALT` at engine 1 (code 02), and the mount (strategy code 0001) on
/// `ALT`; the core's clock reads the returned cell. `past_sessions` are the core's [`CoreConfig::order_client_id_past_sessions`].
fn coded_core(
    past_sessions: Vec<(String, u64)>,
) -> (CoreThread<RecordingClient>, Arc<AtomicI64>, Heard) {
    let clock = Arc::new(AtomicI64::new(0));
    let heard: Heard = Arc::default();
    let reads = Arc::clone(&clock);
    let config = CoreConfig {
        seed_cash: 1_000.0,
        clock: Box::new(move || reads.load(Ordering::Relaxed)),
        order_client_id_past_sessions: past_sessions,
        strategy: Some(Mount {
            order_client_id_code: OrderClientIdStrategyCode::parse("0001"),
            account: Some(alt()),
            symbols: Vec::new(),
            controller_id: Some("m-alt".into()),
            underlying_symbol: None,
            venue: VENUE.into(),
            symbol: SYMBOL.into(),
            interval: INTERVAL.into(),
            strategy: Box::new(Quoter { heard: Arc::clone(&heard) }),
        }),
        ..CoreConfig::default()
    };
    let core =
        core_of(coded_engine(VENUE, 1), vec![(1_000.0, coded_engine("bybit#ALT", 2))], config);
    (core, clock, heard)
}

/// The mount quotes once; the id `ALT` was sent.
fn quote(core: &mut CoreThread<RecordingClient>) -> String {
    core.drive_strategy_feed_status(VENUE, SYMBOL, FeedStatus::Live);
    core.engine(1)
        .client
        .submissions
        .last()
        .expect("the mount quoted on ALT")
        .order_client_id
        .clone()
}

/// Fold `event` as the run loop does: the dispatch, then the fill and order-event delivery.
fn fold(core: &mut CoreThread<RecordingClient>, event: Event) {
    core.dispatch(Ingest::Event(event));
    core.dispatch_applied_fills();
}

fn rejected(order_client_id: &str) -> Event {
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.to_string(),
        reason: "venue said no".into(),
        timestamp: 0,
        refusal_id: None,
    })
}

fn canceled(order_client_id: &str) -> Event {
    Event::OrderCanceled(OrderCanceled {
        order_client_id: order_client_id.to_string(),
        reason: "venue".into(),
        timestamp: 0,
        post_only_would_cross: false,
        refusal_id: None,
    })
}

/// A venue fill of one order, as a bridge emits it: the canonical venue, no account, no ring.
fn fill_of(order_client_id: &str, trade_id: &'static str) -> Event {
    Event::Fill(FillEvent {
        trade_id: TradeId::from(trade_id),
        order_client_id: order_client_id.to_string(),
        venue: VENUE.into(),
        symbol: SYMBOL.into(),
        side: 1,
        last_qty: 1.0,
        last_price: 100.0,
        commission: 0.0,
        commission_asset: String::new().into(),
        liquidity_side: "taker".to_string().into(),
        timestamp: 0,
        mark_price: Some(100.0),
        position_side: PositionSide::Both,
    })
}

/// ⚠ **THE BOUND.** A hundred thousand orders minted on `ALT` and ended, one every 100 ms: the engine rows, the
/// mount entries and the prune queue hold the orders of the last linger window, never one per order since boot.
#[test]
fn a_hundred_thousand_ended_orders_leave_bounded_maps() {
    const ORDERS: i64 = 100_000;
    const STEP_MS: i64 = 100;
    let (mut core, clock, _) = coded_core(Vec::new());
    for i in 0..ORDERS {
        clock.store(i * STEP_MS, Ordering::Relaxed);
        let id = quote(&mut core);
        fold(&mut core, rejected(&id));
    }
    assert_eq!(core.engine(1).client.submissions.len(), ORDERS as usize, "every quote went to ALT");
    let window = (ORDER_CLIENT_ID_PRUNE_LINGER_MS / STEP_MS) as usize;
    let sizes = (
        core.order_client_id_venue.len(),
        core.order_client_id_mount.len(),
        core.order_client_id_terminal.len(),
    );
    assert!(
        sizes.0 <= window && sizes.1 <= window && sizes.2 <= window,
        "rows, entries and queue are bounded by one linger window ({window}): {sizes:?}"
    );
    assert!(sizes.0 > 0 && sizes.1 > 0, "the window's own orders keep theirs: {sizes:?}");
}

/// ⚠ **A late fill and a late cancel of an ended order whose row and entry RETIRED** reach `ALT` and the mount.
/// Without the row the fill fell through every rung of `route_event` (no stamped key, no row, no ring, a symbol both
/// accounts trade) to the foreign-fill refusal, and the cancel to the primary.
#[test]
fn a_late_fill_and_cancel_after_the_rows_retired_reach_the_orders_account_and_mount() {
    let (mut core, _, heard) = coded_core(Vec::new());
    let id = quote(&mut core);
    assert_eq!(core.order_client_id_venue.get(&id), Some(&1), "precondition: ALT's row");
    fold(&mut core, rejected(&id));
    core.engine.now_ms = ORDER_CLIENT_ID_PRUNE_LINGER_MS;
    core.dispatch_applied_fills();
    assert!(
        !core.order_client_id_venue.contains_key(&id)
            && !core.order_client_id_mount.contains_key(&id),
        "premise: the ended order's row and entry retired"
    );

    assert_eq!(
        core.route_event(&canceled(&id)),
        Some(1),
        "a late cancel reaches ALT, not the primary"
    );
    let fill = fill_of(&id, "late-1");
    assert_eq!(
        core.route_event(&fill),
        Some(1),
        "a late fill reaches ALT, not the foreign refusal"
    );
    fold(&mut core, fill);
    assert_eq!(
        crate::runtime::test_support::held_at(&core, 1, VENUE, SYMBOL),
        1.0,
        "booked on ALT"
    );
    assert_eq!(
        crate::runtime::test_support::held_at(&core, 0, VENUE, SYMBOL),
        0.0,
        "and not on the default"
    );
    assert_eq!(
        core.mount_attribution[0].size.to_bits(),
        1.0_f64.to_bits(),
        "into the mount's ledger"
    );
    assert_eq!(heard.lock().unwrap().clone(), vec![1.0], "and its strategy heard it");
}

/// **After a restart no registry holds an order that ended before it**: a late fill of it finds `ALT` and the mount
/// by its id's account and strategy codes, as a refold of that tail does, but only while the id is OURS (a past
/// session of this install minted it). The same id with no session that minted it is another install's: refused as
/// foreign, booked nowhere.
#[test]
fn a_late_fill_of_an_order_that_ended_before_a_restart_finds_it_by_its_codes_when_ours() {
    let (mut first, _, _) = coded_core(Vec::new());
    let id = quote(&mut first);
    let session = first.order_client_id_gen.state();

    let (mut next, _, heard) = coded_core(vec![session]);
    assert!(
        (0..=next.extra_engines.len()).all(|i| !next.engine(i).registry.contains_key(id.as_str())),
        "premise: no registry of the new process holds it"
    );
    let fill = fill_of(&id, "late-2");
    assert_eq!(next.route_event(&fill), Some(1), "ALT, by the id's account code");
    fold(&mut next, fill);
    assert_eq!(
        crate::runtime::test_support::held_at(&next, 1, VENUE, SYMBOL),
        1.0,
        "booked on ALT"
    );
    assert_eq!(
        next.mount_attribution[0].size.to_bits(),
        1.0_f64.to_bits(),
        "into the mount's ledger"
    );
    assert_eq!(heard.lock().unwrap().clone(), vec![1.0], "and its strategy heard it");

    let (mut stranger, _, heard) = coded_core(Vec::new());
    let fill = fill_of(&id, "late-3");
    assert_eq!(stranger.route_event(&fill), None, "another install's id: the foreign-fill refusal");
    fold(&mut stranger, fill);
    assert_eq!(
        stranger.mount_attribution[0].size.to_bits(),
        0.0_f64.to_bits(),
        "no mount's ledger"
    );
    assert!(heard.lock().unwrap().is_empty(), "no strategy hears it");
}
