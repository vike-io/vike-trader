//! [`OrderWindows`] through the real `build`: what a publish carries as orders end and the engine clock moves,
//! and (ignored, a lane's) what a publish costs on a node holding thousands of orders that ended long ago.

use std::sync::Arc;
use std::time::{Duration, Instant};

use vike_execution::testing::RecordingClient;
use vike_execution::{
    Account, BalanceMode, CoreSnapshot, PriceBoardConfig, ReconcileBlock, RiskGate,
};
use vike_model::{OrderRecord, OrderRequest, OrderStatus, RiskLimits};

use super::*;

fn engine(venue: &str) -> Engine<RecordingClient> {
    Engine::new(
        Account::new(1.0, venue, None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        venue,
        "BTCUSDT",
    )
}

/// Register `order_client_id` on `e` in `status`.
fn register(e: &mut Engine<RecordingClient>, order_client_id: &str, status: OrderStatus) {
    let request = OrderRequest {
        order_client_id: order_client_id.to_string(),
        venue: e.venue.clone(),
        symbol: "BTCUSDT".to_string(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(100.0),
        ..OrderRequest::default()
    };
    let mut order = OrderRecord::new(request);
    order.status = status;
    e.registry.insert(order_client_id.to_string(), order);
}

/// One publish of `primary` and `extras` through `windows`, every other input empty.
fn publish(
    primary: &Engine<RecordingClient>,
    extras: &[(f64, Engine<RecordingClient>)],
    windows: &mut OrderWindows,
) -> CoreSnapshot {
    crate::snapshot::build(
        1,
        primary,
        extras,
        1_000.0,
        PriceBoardConfig::default(),
        vike_model::MarginCallConfig::default().maintenance_margin_requirement,
        &std::collections::VecDeque::new(),
        &indexmap::IndexMap::new(),
        std::sync::Arc::new([]),
        &None,
        ReconcileBlock::default(),
        Vec::new(),
        windows,
    )
}

fn ids(snap: &CoreSnapshot) -> Vec<&str> {
    snap.orders.iter().map(|o| o.order_client_id.as_str()).collect()
}

/// **An ended order leaves the published list an hour after a publish first found it ended, together with every
/// other one past its hour once the oldest is a sweep past it; an open one never does, and the registry keeps
/// them all.** Two engines (the window is per engine, by its place), an order already filled at the first publish,
/// one that fills half a minute later, one that fills just before the first sweep, one that stays open, one
/// registered late; the core clock steps across each line.
#[test]
fn an_ended_order_leaves_the_published_list_an_hour_after_it_ended() {
    const HOUR: i64 = ENDED_ORDER_PUBLISH_MS;
    const SWEEP: i64 = ENDED_ORDER_SWEEP_MS;
    let mut primary = engine("binance");
    register(&mut primary, "filled-early", OrderStatus::Filled);
    register(&mut primary, "fills-half-a-minute-later", OrderStatus::Accepted);
    register(&mut primary, "fills-before-the-sweep", OrderStatus::Accepted);
    register(&mut primary, "stays-open", OrderStatus::Accepted);
    let mut other = engine("bybit");
    register(&mut other, "bybit-canceled", OrderStatus::Canceled);
    let extras = vec![(1_000.0, other)];
    let mut windows = OrderWindows::default();
    let t0 = 1_000;

    windows.clock(t0);
    let all = [
        "filled-early",
        "fills-half-a-minute-later",
        "fills-before-the-sweep",
        "stays-open",
        "bybit-canceled",
    ];
    assert_eq!(ids(&publish(&primary, &extras, &mut windows)), all, "every order, registry order");

    windows.clock(t0 + 30_000);
    primary.registry.get_mut("fills-half-a-minute-later").expect("held").status =
        OrderStatus::Filled;
    assert_eq!(ids(&publish(&primary, &extras, &mut windows)), all);

    let before_the_sweep = t0 + HOUR + SWEEP - 1;
    windows.clock(before_the_sweep);
    primary.registry.get_mut("fills-before-the-sweep").expect("held").status = OrderStatus::Filled;
    register(&mut primary, "late", OrderStatus::Accepted);
    let mut and_late = all.to_vec();
    and_late.insert(4, "late");
    assert_eq!(
        ids(&publish(&primary, &extras, &mut windows)),
        and_late,
        "three are past their hour, and none is a sweep past it yet"
    );

    windows.clock(t0 + HOUR + SWEEP);
    assert_eq!(
        ids(&publish(&primary, &extras, &mut windows)),
        ["fills-before-the-sweep", "stays-open", "late"],
        "the oldest is a sweep past its hour: every one past its hour goes, on both engines, the one half a minute \
         younger too"
    );

    windows.clock(before_the_sweep + HOUR + SWEEP);
    assert_eq!(ids(&publish(&primary, &extras, &mut windows)), ["stays-open", "late"]);
    assert_eq!(primary.registry.len(), 5, "the registry keeps every order");
    assert_eq!(extras[0].1.registry.len(), 1, "on every engine");

    let fresh = publish(&primary, &extras, &mut OrderWindows::default());
    assert_eq!(fresh.orders.len(), 6, "a fresh window publishes every order once");
}

/// **An engine's own `now_ms` never ages an ended order: only the core clock does.** A strategy's order stamps its
/// engine with the time of the event that caused it, which in a paper or scripted run is another clock (the paper
/// book's quote times, 61,000 ms here, against the core's wall clock). The cross-venue hedge of
/// `crates/vike-tradehub-core/tests/scripted/xemm_scripted.rs` was stamped ended on the one and judged on the other,
/// so it left the list the moment it filled. MUTATION PROOF: count the hour on `engine.now_ms` and this fails.
#[test]
fn an_engines_event_time_never_ages_an_ended_order() {
    const WALL: i64 = 1_763_000_000_000;
    let mut taker = engine("bybit");
    register(&mut taker, "hedge", OrderStatus::Accepted);
    let mut windows = OrderWindows::default();
    windows.clock(WALL);
    taker.now_ms = 60_000;
    assert_eq!(ids(&publish(&taker, &[], &mut windows)), ["hedge"]);

    // It fills while its engine carries the triggering quote's time…
    taker.registry.get_mut("hedge").expect("held").status = OrderStatus::Filled;
    taker.now_ms = 61_000;
    windows.clock(WALL + 1_000);
    assert_eq!(ids(&publish(&taker, &[], &mut windows)), ["hedge"]);

    // …and the next message puts the wall clock on the engine again: a second has passed, not 56 years.
    taker.now_ms = WALL + 2_000;
    windows.clock(WALL + 2_000);
    assert_eq!(
        ids(&publish(&taker, &[], &mut windows)),
        ["hedge"],
        "an ended order stays its hour"
    );

    // A publish before any clock reading starts no hour and ends none.
    let mut unclocked = OrderWindows::default();
    assert_eq!(ids(&publish(&taker, &[], &mut unclocked)), ["hedge"]);
}

/// **An order made live again over its own key after the list let it go is published again**, in its registry place,
/// both ways a registry entry comes back: a re-registration's `insert` (a reconcile reaped it, the venue still held it)
/// and an in-place status write. MUTATION PROOF: drop `readmit_live` and both re-livened publishes lack it.
#[test]
fn an_order_made_live_again_after_it_left_the_list_is_published_again() {
    const PAST_THE_SWEEP: i64 = ENDED_ORDER_PUBLISH_MS + ENDED_ORDER_SWEEP_MS;
    let mut e = engine("binance");
    register(&mut e, "reaped", OrderStatus::Accepted);
    register(&mut e, "stays-open", OrderStatus::Accepted);
    let mut windows = OrderWindows::default();
    let mut now = 1_000;
    windows.clock(now);
    assert_eq!(ids(&publish(&e, &[], &mut windows)), ["reaped", "stays-open"]);

    for (round, relive) in ["re-registered", "status written in place"].into_iter().enumerate() {
        e.registry.get_mut("reaped").expect("held").status = OrderStatus::Canceled;
        assert_eq!(
            ids(&publish(&e, &[], &mut windows)),
            ["reaped", "stays-open"],
            "{relive}: its hour"
        );
        now += PAST_THE_SWEEP;
        windows.clock(now);
        assert_eq!(ids(&publish(&e, &[], &mut windows)), ["stays-open"], "{relive}: let go");

        if round == 0 {
            register(&mut e, "reaped", OrderStatus::Accepted);
        } else {
            e.registry.get_mut("reaped").expect("held").status = OrderStatus::Accepted;
        }
        assert_eq!(
            ids(&publish(&e, &[], &mut windows)),
            ["reaped", "stays-open"],
            "{relive}: live again, published again in its registry place"
        );
    }
    assert_eq!(e.registry.len(), 2, "over its own key: no new entry");
}

/// **A publish rebuilds the row of an order that moved and shares every other**, and a registry put in an engine's
/// place rebuilds them all (equal, not shared). MUTATION PROOF: reuse a kept row without asking its write stamp and
/// the written order publishes its old fill.
#[test]
fn a_publish_shares_the_rows_of_the_orders_that_did_not_move() {
    let mut e = engine("binance");
    register(&mut e, "filled", OrderStatus::Filled);
    register(&mut e, "open", OrderStatus::Accepted);
    register(&mut e, "other", OrderStatus::Accepted);
    let mut windows = OrderWindows::default();
    windows.clock(1_000);
    let first = publish(&e, &[], &mut windows);
    let again = publish(&e, &[], &mut windows);
    let shared =
        |a: &CoreSnapshot, b: &CoreSnapshot, at: usize| Arc::ptr_eq(&a.orders[at], &b.orders[at]);
    assert!((0..3).all(|at| shared(&first, &again, at)), "nothing moved: every row shared");

    e.registry.get_mut("open").expect("held").filled_qty = 0.5;
    let moved = publish(&e, &[], &mut windows);
    assert!(shared(&again, &moved, 0) && shared(&again, &moved, 2), "the others are shared");
    assert!(!shared(&again, &moved, 1), "the written one is rebuilt");
    assert_eq!(moved.orders[1].filled_qty, 0.5);

    e.registry = e.registry.iter().map(|(k, o)| (k.clone(), o.clone())).collect();
    let restored = publish(&e, &[], &mut windows);
    assert!(
        (0..3).all(|at| !shared(&moved, &restored, at) && moved.orders[at] == restored.orders[at]),
        "another registry: every row rebuilt, equal"
    );
}

/// **A reason the engine's bounded table lets go leaves the row it was shown on**, though nothing wrote that order: a
/// refusal's reason and its catalog id are a side table (`Engine::rejection`), and the 1,024 newer refusals of other
/// orders that evict them (`MAX_REJECTION_REASONS`) write only theirs. MUTATION PROOF: drop the reason from
/// `Kept::refresh`'s test and the kept row still shows it.
#[test]
fn a_reason_the_engine_let_go_leaves_the_kept_row() {
    use vike_execution::{EventBusOutbox, EventHandler};
    use vike_model::events::{Event, OrderRejected, refusal_id_of};
    let mut e = engine("binance");
    let mut outbox = EventBusOutbox::default();
    let mut refuse = |e: &mut Engine<RecordingClient>, order_client_id: &str| {
        register(e, order_client_id, OrderStatus::Submitted);
        let rejected = Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.into(),
            reason: format!("refused {order_client_id}").into(),
            timestamp: 1,
            refusal_id: refusal_id_of("order.venue.rejected"),
        });
        e.on_event(&rejected, &mut outbox);
    };
    refuse(&mut e, "first");
    let mut windows = OrderWindows::default();
    windows.clock(1_000);
    let shown = publish(&e, &[], &mut windows);
    assert_eq!(
        (shown.orders[0].reason.as_deref(), shown.orders[0].refusal_id.as_deref()),
        (Some("refused first"), Some("order.venue.rejected")),
        "premise: the reason is shown with its id"
    );

    for n in 0..1_024 {
        refuse(&mut e, &format!("later-{n}"));
    }
    assert_eq!(e.rejection_reason("first"), None, "premise: the bounded table let it go");
    let kept = publish(&e, &[], &mut windows);
    assert_eq!(kept.orders[0].order_client_id, "first");
    assert_eq!(
        (&kept.orders[0].reason, &kept.orders[0].refusal_id),
        (&None, &None),
        "the kept row follows the table"
    );
    assert_eq!(
        kept.orders,
        publish(&e, &[], &mut OrderWindows::default()).orders,
        "as a rebuild publishes it"
    );
}

/// **Every `OrderView` field is classified by what tells a kept row it moved**, so a new field does not compile here
/// until someone says which (no `..` below, on purpose). RECORD: read off the registry `OrderRecord` (or its key), so a
/// write stamp covers it (`OrderRegistry::write_stamp`). ENGINE: the same for every row of an engine, compared once per
/// publish (`OrderWindow::venue`, `OrderWindow::account`). SIDE TABLE: moves without a registry write (a bounded table
/// evicts a reason, a modify's price expires on the engine clock), so `Kept::refresh` re-reads it for every refused or
/// modifiable row. A field of none of the three needs its own test in `Kept::refresh` before it joins.
#[test]
fn every_published_row_field_is_classified_by_what_moves_it() {
    let mut e = engine("binance");
    register(&mut e, "c-1", OrderStatus::Accepted);
    let snap = publish(&e, &[], &mut OrderWindows::default());
    let OrderView {
        order_client_id: _,    // RECORD (its registry key)
        venue: _,              // ENGINE (`Engine::venue`)
        account: _,            // ENGINE (the engine's block's label)
        symbol: _,             // RECORD
        side: _,               // RECORD
        qty: _,                // RECORD
        order_type: _,         // RECORD
        price: _,              // RECORD
        trigger_price: _,      // RECORD
        status: _,             // RECORD
        venue_order_id: _,     // RECORD
        filled_qty: _,         // RECORD
        average_fill_price: _, // RECORD
        data_delay: _,         // RECORD
        reason: _,             // SIDE TABLE (`Engine::rejection`)
        refusal_id: _,         // SIDE TABLE (`Engine::rejection`, beside the reason)
        pending_price: _,      // SIDE TABLE (`Engine::pending_modify_price`)
        trail: _,              // RECORD
        limit_offset: _,       // RECORD
        attributes: _,         // RECORD
        trail_percent: _,      // RECORD
        adjust: _,             // RECORD
        release: _,            // RECORD
        display_quantity: _,   // RECORD
    } = &*snap.orders[0];
}

/// **The measurement** (a lane's, `--release --ignored --nocapture`): one publish's `build` on a node holding
/// `ended` FILLED orders and five open ones, while every ended order is inside its hour (what every publish cost
/// before the window: the whole registry copied) and once they are past it (what it costs now), for 2,000 ended
/// orders (the CI box held 2,021 after 3 h 20 m on 2026-11-19) and for 20,000. Medians of 200 builds.
#[test]
#[ignore = "a measurement: a few seconds of builds; run on a lane with --release --nocapture"]
fn measure_publish_with_thousands_of_ended_orders() {
    const BUILDS: usize = 200;
    let median = |mut runs: Vec<Duration>| {
        runs.sort();
        runs[runs.len() / 2].as_secs_f64() * 1e6
    };
    println!("one publish's build: ended FILLED orders + 5 open; median of {BUILDS}, µs");
    for ended in [2_000usize, 20_000] {
        let mut e = engine("binance");
        for n in 0..ended {
            register(&mut e, &format!("ended-{n}"), OrderStatus::Filled);
        }
        for n in 0..5 {
            register(&mut e, &format!("open-{n}"), OrderStatus::Accepted);
        }
        let mut windows = OrderWindows::default();
        let timed = |e: &Engine<RecordingClient>, windows: &mut OrderWindows| {
            let mut runs = Vec::with_capacity(BUILDS);
            let mut count = 0;
            for _ in 0..BUILDS {
                let started = Instant::now();
                let snap = publish(e, &[], windows);
                runs.push(started.elapsed());
                count = snap.orders.len();
                drop(snap);
            }
            (median(runs), count)
        };
        windows.clock(0);
        let (within, published_within) = timed(&e, &mut windows);
        windows.clock(ENDED_ORDER_PUBLISH_MS + ENDED_ORDER_SWEEP_MS);
        let (past, published_past) = timed(&e, &mut windows);
        println!(
            "{ended:>6} ended | within the hour (before) {within:>9.1} µs, {published_within} orders | past it (after) \
             {past:>7.1} µs, {published_past} orders | {:>6.1}x",
            within / past
        );
        assert_eq!(published_past, 5, "only the open orders are published");
    }
}
