//! The DELIVERY-WINDOW sweep (`watchdog/delivery_window.rs`; the owner's ruling of 2026-11-22): our resting
//! OPENING orders on a dated contract are cancelled once its window starts, a reduce-only order stays, a name that
//! states no day is never swept, and an emulated parent whose child the window pulls or refuses STOPS.

use super::*;
use vike_execution::testing::engine_on;
use vike_model::events::{OrderCanceled, OrderRejected};
use vike_model::{OrderPeg, OrderPegAnchor, OrderRecord, OrderStatus, OrderType};

use crate::runtime::test_support::core_of;

type Core = CoreThread<RecordingClient>;

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
/// bybit's dated contract, delivered 2026-12-25 at 08:00 UTC (its row: the name's day at the venue's 08:00).
const DATED: &str = "BTCUSDT_261225";
/// 2026-12-25T08:00:00Z, the expiry [`DATED`]'s name states on bybit.
const EXPIRY: i64 = 1_798_185_600_000;

fn core_on(venue: &str, symbol: &str) -> Core {
    core_of(engine_on(venue, symbol, RecordingClient::default()), Vec::new(), CoreConfig::default())
}

/// A buy limit of ours resting at the venue on engine 0's `symbol` (`reduce_only` as given), as the venue
/// accepted it.
fn rest(c: &mut Core, order_client_id: &str, symbol: &str, reduce_only: bool) {
    let request = OrderRequest {
        order_client_id: order_client_id.into(),
        venue: c.engine.venue.clone(),
        symbol: symbol.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Limit,
        price: Some(100.0),
        reduce_only,
        ..Default::default()
    };
    let mut order = OrderRecord::new(request);
    order.status = OrderStatus::Accepted;
    c.engine.registry.insert(order_client_id.to_string(), order);
}

fn said(c: &Core) -> usize {
    c.recent.iter().filter(|l| l.contains("DELIVERY WINDOW")).count()
}

/// **The window's start cancels our resting opening order, once**: nothing a sweep before it, one cancel and one
/// line at the first sweep inside it, nothing more while the venue's answer is on its way, and the fire-once memory
/// lets the id go once the venue's `OrderCanceled` has ended it.
#[test]
fn an_opening_order_resting_on_a_dated_contract_is_cancelled_once_when_its_window_starts() {
    let mut c = core_on("bybit", DATED);
    rest(&mut c, "open-1", DATED, false);

    c.sweep_risk_clock(EXPIRY - DAY - 30_000);
    assert!(c.engine.client.cancels.is_empty(), "before the window");
    assert_eq!(said(&c), 0);

    c.sweep_risk_clock(EXPIRY - DAY + 30_000);
    assert_eq!(c.engine.client.cancels, vec!["open-1".to_string()], "the window started");
    assert_eq!(said(&c), 1, "{:?}", c.recent);
    assert!(c.recent.iter().any(|l| l.contains("open-1") && l.contains(DATED)), "{:?}", c.recent);

    c.sweep_risk_clock(EXPIRY - DAY + 60_000);
    assert_eq!(c.engine.client.cancels.len(), 1, "asked once: the venue's answer is on its way");

    let _ = c.handle(Ingest::Event(Event::OrderCanceled(OrderCanceled {
        order_client_id: "open-1".into(),
        reason: "canceled".into(),
        timestamp: 0,
        post_only_would_cross: false,
        refusal_id: None,
    })));
    c.sweep_risk_clock(EXPIRY - DAY + 90_000);
    assert!(c.delivery_canceled.is_empty(), "the ended order leaves the fire-once memory");
    assert_eq!(c.engine.client.cancels.len(), 1);
}

/// **A reduce-only order stays** inside the window and past the delivery: it is taken until the expiry (okx's rule
/// for every venue), and the venue retires it at the delivery itself.
#[test]
fn a_reduce_only_order_on_a_dated_contract_is_never_cancelled() {
    let mut c = core_on("bybit", DATED);
    rest(&mut c, "close-1", DATED, true);
    for now in [EXPIRY - HOUR, EXPIRY + HOUR] {
        c.sweep_risk_clock(now);
    }
    assert!(c.engine.client.cancels.is_empty());
    assert_eq!(said(&c), 0);
}

/// **An adopted order whose reduce-only flag nobody remembers is reported ONCE and never cancelled** (the owner's safe
/// default): a venue report carries no such flag and a `false` one is never written, so an adopted order whose
/// lifetime no memory stated (`lifetime_unknown`) may be a protective stop from an ownership line older than the flag.
/// An order of ours whose terms are known is still swept beside it.
#[test]
fn an_adopted_order_whose_reduce_only_flag_is_unknown_is_reported_once_and_never_cancelled() {
    let mut c = core_on("bybit", DATED);
    rest(&mut c, "adopted-1", DATED, false);
    c.engine.registry.get_mut("adopted-1").expect("resting").lifetime_unknown = true;
    for now in [EXPIRY - DAY + 30_000, EXPIRY - DAY + 60_000, EXPIRY + HOUR] {
        c.sweep_risk_clock(now);
    }
    assert!(c.engine.client.cancels.is_empty(), "never cancelled");
    assert_eq!(said(&c), 1, "reported once: {:?}", c.recent);
    assert!(
        c.recent.iter().any(|l| l.contains("adopted-1") && l.contains("NOT cancelled")),
        "{:?}",
        c.recent
    );

    rest(&mut c, "ours-1", DATED, false);
    c.sweep_risk_clock(EXPIRY + 2 * HOUR);
    assert_eq!(
        c.engine.client.cancels,
        vec!["ours-1".to_string()],
        "a known opening order is swept"
    );
}

/// **Every venue by its own row**: a deribit OPTION (its venue name's `DDMMMYY`, at 08:00) inside its 24 h, ibkr's
/// future at five days before 00:00 UTC of its last trading day, and an order past its delivery too.
#[test]
fn a_deribit_option_and_an_ibkr_future_are_swept_by_their_own_windows() {
    let option = "BTC_USDC-25DEC26-80000-C";
    let mut c = core_on("deribit", option);
    rest(&mut c, "opt-1", option, false);
    c.sweep_risk_clock(EXPIRY - DAY - 30_000);
    assert!(c.engine.client.cancels.is_empty(), "before the option's window");
    c.sweep_risk_clock(EXPIRY - DAY + 30_000);
    assert_eq!(c.engine.client.cancels, vec!["opt-1".to_string()]);

    // ES_261218: 2026-12-18T00:00:00Z.
    let last_trade = 1_797_552_000_000;
    let mut c = core_on("ibkr", "ES_261218");
    rest(&mut c, "es-1", "ES_261218", false);
    c.sweep_risk_clock(last_trade - 5 * DAY - 30_000);
    assert!(c.engine.client.cancels.is_empty(), "more than five days out");
    c.sweep_risk_clock(last_trade - 5 * DAY + 30_000);
    assert_eq!(c.engine.client.cancels, vec!["es-1".to_string()]);

    let mut c = core_on("bybit", DATED);
    rest(&mut c, "late-1", DATED, false);
    c.sweep_risk_clock(EXPIRY + HOUR);
    assert_eq!(
        c.engine.client.cancels,
        vec!["late-1".to_string()],
        "a delivered contract's order too"
    );
}

/// **A name that states no day is never swept**: a perpetual and a spot pair of a venue with a window, and a venue
/// with none (oanda's FX pair).
#[test]
fn a_perpetual_a_spot_pair_and_a_venue_without_a_window_are_never_swept() {
    for (venue, symbol) in [("bybit", "BTCUSDT.P"), ("bybit", "BTCUSDT"), ("oanda", "EURUSD")] {
        let mut c = core_on(venue, symbol);
        rest(&mut c, "any-1", symbol, false);
        for now in [EXPIRY - HOUR, EXPIRY + HOUR] {
            c.sweep_risk_clock(now);
        }
        assert!(c.engine.client.cancels.is_empty(), "{venue} {symbol}");
    }
}

/// **The sweep rides the risk clock**: a core whose clock runs (`price_only`) reaches it at the timer, with no
/// message and no bar.
#[test]
fn the_window_sweep_runs_on_the_risk_clock() {
    let config = CoreConfig {
        price_only: Some(PriceOnlyConfig {
            venues: std::collections::BTreeSet::new(),
            sweep_interval: std::time::Duration::from_secs(30),
            max_price_age: std::time::Duration::from_secs(60),
            price_source: std::collections::BTreeMap::new(),
        }),
        ..CoreConfig::default()
    };
    let mut c = core_of(engine_on("bybit", DATED, RecordingClient::default()), Vec::new(), config);
    rest(&mut c, "open-1", DATED, false);
    let start = EXPIRY - 23 * HOUR;
    c.arm_boundary_timers(start);
    c.drive_due_timers(start + 30_000);
    assert_eq!(c.engine.client.cancels, vec!["open-1".to_string()]);
}

/// A fresh touch of [`DATED`] on the board, then one boundary pass of the emulated parents.
fn touch(c: &mut Core, ts: i64) {
    c.engine.price_board.set_quote("bybit", DATED, 100.0, 101.0, ts);
    c.drive_emulated(ts);
}

/// The venue's `[OrderSubmitted, OrderAccepted]` for `order_client_id`, folded and delivered.
fn accept(c: &mut Core, order_client_id: &str) {
    let _ = c.handle(Ingest::Event(Event::OrderSubmitted(OrderSubmitted {
        order_client_id: order_client_id.to_string(),
        timestamp: 0,
    })));
    let _ = c.handle(Ingest::Event(Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(format!("v-{order_client_id}").into()),
        timestamp: 0,
    })));
}

/// A chase (a pegged order bybit does not hold) and an iceberg (a visible size it does not hold) on [`DATED`].
fn parents() -> [OrderRequest; 2] {
    let base = OrderRequest {
        venue: "bybit".into(),
        symbol: DATED.into(),
        side: 1,
        qty: 2.0,
        ..Default::default()
    };
    [
        OrderRequest {
            order_type: OrderType::Pegged,
            peg: Some(OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 }),
            ..base.clone()
        },
        OrderRequest {
            order_type: OrderType::Limit,
            price: Some(100.0),
            display_quantity: Some(1.0),
            ..base
        },
    ]
}

/// **A parent whose child the window pulls or the venue refuses STOPS and never re-places** (seams section 3):
/// (a) a child resting when the window starts is cancelled by the sweep, its `OrderCanceled` comes back, and the
/// parent ends with one child sent across three more clock ticks and fresh touches; (b) a parent started inside
/// the window meets the bridge's refusal of its child (`OrderRejected`, the guard's words) and ends the same way.
#[test]
fn an_emulated_parent_whose_child_the_window_refuses_or_cancels_stops_and_never_replaces() {
    for request in parents() {
        let kind = format!("{:?}", request.order_type);
        // (a) pulled by the sweep
        let mut c = core_on("bybit", DATED);
        let parent = c.apply_intent(OrderIntent::Submit(Box::new(request.clone())), 0).pop();
        let parent = parent.expect("an order client id");
        touch(&mut c, 1);
        assert_eq!(c.engine.client.submissions.len(), 1, "{kind}: one child out");
        let child = c.engine.client.submissions[0].order_client_id.clone();
        accept(&mut c, &child);
        c.sweep_risk_clock(EXPIRY - DAY + 30_000);
        assert_eq!(
            c.engine.client.cancels,
            vec![child.clone()],
            "{kind}: the sweep pulls the child"
        );
        let _ = c.handle(Ingest::Event(Event::OrderCanceled(OrderCanceled {
            order_client_id: child.clone(),
            reason: "canceled".into(),
            timestamp: 0,
            post_only_would_cross: false,
            refusal_id: None,
        })));
        for k in 0..3 {
            c.sweep_risk_clock(EXPIRY - DAY + 60_000 + 30_000 * k);
            touch(&mut c, 2 + k);
        }
        assert_eq!(c.engine.client.submissions.len(), 1, "{kind}: never re-placed");
        assert!(c.emulated.working.is_empty(), "{kind}: the parent ended");
        let ended = c.engine.registry.get(&parent).expect("the parent is listed, finished");
        assert!(!ended.status.is_live(), "{kind}: {:?}", ended.status);

        // (b) refused by the bridge at send
        let mut c = core_on("bybit", DATED);
        c.apply_intent(OrderIntent::Submit(Box::new(request)), 0);
        touch(&mut c, 1);
        let child = c.engine.client.submissions[0].order_client_id.clone();
        let _ = c.handle(Ingest::Event(Event::OrderRejected(OrderRejected {
            order_client_id: child,
            reason: "bybit: BTCUSDT_261225 is delivered in 23 h and takes no OPENING order".into(),
            timestamp: 0,
            cause: None,
            refusal_id: None,
        })));
        for k in 0..3 {
            touch(&mut c, 2 + k);
        }
        assert_eq!(c.engine.client.submissions.len(), 1, "{kind}: never re-sent after the refusal");
        assert!(c.emulated.working.is_empty(), "{kind}: the parent ended");
    }
}
