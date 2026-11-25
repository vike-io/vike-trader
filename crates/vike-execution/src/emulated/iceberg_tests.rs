use super::*;
use crate::emulated::emulated_tests::tagged_limits;
use vike_model::ContractValue;
use vike_model::strategy::OrderBrokerMock;

const PX: f64 = 100.0;

fn bar(ts: i64, low: f64) -> Bar {
    Bar {
        timestamp: ts,
        open: PX,
        high: PX,
        low,
        close: PX,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: Some("BTCUSDT".into()),
    }
}

fn config(total: f64, visible: f64) -> OrderEmulationIcebergConfig {
    OrderEmulationIcebergConfig { side: 1, total, visible, price: 99.0 }
}

fn ice(total: f64, visible: f64) -> OrderEmulationIceberg {
    OrderEmulationIceberg::new(config(total, visible))
}

/// The grid the engine's gate would hold the mounted symbol to (`OrderBroker::instrument_grid`).
fn grid(tick: Option<f64>, min_qty: Option<f64>, min_notional: Option<f64>) -> OrderBrokerMock {
    let grid = vike_model::Grid { tick_size: tick, lot_size: None, min_notional, min_qty };
    OrderBrokerMock { grid: Some(grid), ..OrderBrokerMock::default() }
}

fn event(kind: OrderEventKind) -> OrderEvent {
    event_of("c1", kind)
}

/// An event of the child the core minted as `id`, under the executor's tag.
fn event_of(id: &str, kind: OrderEventKind) -> OrderEvent {
    OrderEvent { order_client_id: id.into(), tag: Some(TAG.into()), kind }
}

/// A fill of the executor's child as the broker reports it: the position moves first, then `on_fill`.
fn fill(x: &mut OrderEmulationIceberg, b: &mut OrderBrokerMock, size: f64) {
    b.pos += size;
    let f = Fill {
        side: 1,
        size,
        price: 99.0,
        fee: 0.0,
        timestamp: 0,
        is_maker: true,
        symbol: String::new(),
        tag: None,
    };
    x.on_fill(b, &f);
}

fn sizes(b: &OrderBrokerMock) -> Vec<f64> {
    tagged_limits(b).iter().map(|(_, _, q, _)| *q).collect()
}

#[test]
fn slices_rest_one_at_a_time_and_refill_on_the_next_market_hook_never_inside_the_fill() {
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 1.0, 99.0)], "one child at a time");
    fill(&mut x, &mut b, 1.0);
    assert_eq!(
        tagged_limits(&b).len(),
        1,
        "no refill inside on_fill: the Filled event that follows would wipe it"
    );
    x.on_order_event(&mut b, &event(OrderEventKind::Filled));
    x.on_bar(&mut b, &bar(2, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0]);
    fill(&mut x, &mut b, 1.0);
    x.on_bar(&mut b, &bar(3, PX));
    fill(&mut x, &mut b, 1.0);
    x.on_bar(&mut b, &bar(4, PX));
    x.on_bar(&mut b, &bar(5, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0, 1.0], "done at the total, nothing more");
    assert!(b.cancelled.is_empty());
    assert!(x.refusal().is_some_and(|w| w.starts_with("done")), "{:?}", x.refusal());
}

#[test]
fn a_partial_slice_fill_refills_nothing_until_the_slice_has_filled() {
    let (mut x, mut b) = (ice(2.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 0.4);
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(tagged_limits(&b).len(), 1, "a slice 0.4 filled still rests its other 0.6");
    fill(&mut x, &mut b, 0.6);
    x.on_bar(&mut b, &bar(2, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0]);
}

#[test]
fn the_last_slice_is_what_remains() {
    let (mut x, mut b) = (ice(2.5, 1.0), OrderBrokerMock::default());
    for ts in 0..3 {
        x.on_bar(&mut b, &bar(ts, PX));
        let q = tagged_limits(&b).last().unwrap().2;
        fill(&mut x, &mut b, q);
    }
    x.on_bar(&mut b, &bar(9, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0, 0.5]);
}

#[test]
fn cancel_mid_way_pulls_the_child_and_stops_and_so_does_a_foreign_cancel() {
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 1.0);
    x.on_bar(&mut b, &bar(1, PX));
    x.cancel(&mut b);
    assert_eq!(b.cancelled, vec![TAG.to_string()]);
    // The venue's echo of our own cancel keeps our reason.
    x.on_order_event(&mut b, &event(OrderEventKind::Canceled { reason: String::new() }));
    x.on_bar(&mut b, &bar(2, PX));
    assert_eq!(tagged_limits(&b).len(), 2, "what is left of the parent stays unfilled");
    assert_eq!(x.refusal(), Some("cancelled by its owner"));

    // A child pulled by someone else (the venue UI, an operator) stops it too, never a re-send.
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    x.on_order_event(&mut b, &event(OrderEventKind::Canceled { reason: "ui".into() }));
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(tagged_limits(&b).len(), 1);
    assert!(b.cancelled.is_empty());
    assert!(x.refusal().is_some_and(|w| w.contains("outside")));
}

/// Every slice goes out under the one tag, so an event counts only for the child it names by ORDER
/// ID: a late `Filled` or cancel of slice 1 after slice 2 went out sends no third slice and stops
/// nothing, and the slices never add up past the parent.
#[test]
fn a_late_event_of_an_older_slice_neither_frees_nor_stops_the_current_one() {
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    x.on_order_event(&mut b, &event_of("c1", OrderEventKind::Accepted));
    fill(&mut x, &mut b, 1.0);
    x.on_order_event(&mut b, &event_of("c1", OrderEventKind::Filled));
    x.on_bar(&mut b, &bar(1, PX));
    x.on_order_event(&mut b, &event_of("c2", OrderEventKind::Accepted));
    // Slice 1's stragglers, after slice 2 went out.
    x.on_order_event(&mut b, &event_of("c1", OrderEventKind::Filled));
    x.on_order_event(&mut b, &event_of("c1", OrderEventKind::Canceled { reason: "late".into() }));
    x.on_bar(&mut b, &bar(2, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0], "no third slice beside slice 2");
    assert_eq!(x.refusal(), None, "a late cancel of slice 1 stops nothing");
    assert!(b.cancelled.is_empty());
    // A fill names no order: one heard while slice 2 rests counts once toward the parent and frees
    // nothing early; the last slice is what is left.
    fill(&mut x, &mut b, 0.5);
    x.on_bar(&mut b, &bar(3, PX));
    assert_eq!(
        tagged_limits(&b).len(),
        2,
        "slice 2 is freed by its own terminal, not a fill's size"
    );
    fill(&mut x, &mut b, 0.5);
    x.on_order_event(&mut b, &event_of("c2", OrderEventKind::Filled));
    x.on_bar(&mut b, &bar(4, PX));
    x.on_order_event(&mut b, &event_of("c3", OrderEventKind::Accepted));
    fill(&mut x, &mut b, 1.0);
    x.on_order_event(&mut b, &event_of("c3", OrderEventKind::Filled));
    x.on_bar(&mut b, &bar(5, PX));
    assert_eq!(sizes(&b), vec![1.0, 1.0, 1.0]);
    assert!(b.pos <= 3.0 + 1e-9, "the parent never overfills: {}", b.pos);
    assert!(x.refusal().is_some_and(|w| w.starts_with("done")), "{:?}", x.refusal());
}

#[test]
fn a_restored_slot_places_nothing_until_the_core_names_the_tag_or_the_deadline_passes() {
    // Saved between two slices; the crash may have come after the next one was sent.
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 1.0);
    let saved = Strategy::<OrderBrokerMock>::save_state(&x).expect("state");

    let restored = || {
        let mut y = ice(3.0, 1.0);
        Strategy::<OrderBrokerMock>::load_state(&mut y, &saved);
        (y, OrderBrokerMock { pos: b.pos, now: 1_000, ..OrderBrokerMock::default() })
    };
    // Nothing names the tag: the deadline pulls whatever it still names, then the slice goes out.
    let (mut y, mut b2) = restored();
    y.on_bar(&mut b2, &bar(1, PX));
    b2.now += ADOPTION_DEADLINE_MS - 1;
    y.on_bar(&mut b2, &bar(2, PX));
    assert_eq!(b2.submits() + b2.cancelled.len(), 0, "nothing before the deadline");
    b2.now += 1;
    y.on_bar(&mut b2, &bar(3, PX));
    assert_eq!(b2.cancelled, vec![TAG.to_string()]);
    assert!(sizes(&b2).is_empty(), "not in the hook that pulls the tag");
    y.on_bar(&mut b2, &bar(4, PX));
    assert_eq!(sizes(&b2), vec![1.0]);
    // The pulled child's late cancel, as the live core sends it once the tag names slice 2: tagless.
    let late = OrderEvent {
        order_client_id: "c0".into(),
        tag: None,
        kind: OrderEventKind::Canceled { reason: String::new() },
    };
    y.on_order_event(&mut b2, &late);
    assert_eq!(y.refusal(), None, "it stops nothing");
    assert_eq!(b2.cancelled.len(), 1);

    // The core names the tag first (a child the save missed, filled while down): no deadline wait.
    let (mut y, mut b2) = restored();
    y.on_bar(&mut b2, &bar(1, PX));
    b2.pos += 1.0;
    y.on_order_event(&mut b2, &event(OrderEventKind::Filled));
    y.on_bar(&mut b2, &bar(2, PX));
    assert!(b2.cancelled.is_empty());
    assert_eq!(sizes(&b2), vec![1.0], "the last slice, sized off the position");
}

#[test]
fn the_start_refusal_is_judged_again_after_a_restart_and_no_saved_state_clears_it() {
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    let saved = Strategy::<OrderBrokerMock>::save_state(&x).expect("state");
    let document = saved.to_json_value().expect("msgpack").to_string();
    assert!(!document.contains("refuse"), "the start refusal is never saved: {document}");
    // The same saved run, restarted on an instrument whose minimum the visible slice no longer clears.
    let mut y = ice(3.0, 1.0);
    Strategy::<OrderBrokerMock>::load_state(&mut y, &saved);
    let mut b2 = OrderBrokerMock { now: ADOPTION_DEADLINE_MS * 2, ..grid(None, None, Some(500.0)) };
    y.on_bar(&mut b2, &bar(1, PX));
    assert!(y.refusal().is_some_and(|w| w.contains("minimum 500")), "{:?}", y.refusal());
    assert_eq!(b2.submits(), 0);
}

#[test]
fn a_restart_keeps_its_live_child_and_resubmits_nothing() {
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 0.5);
    let saved = Strategy::<OrderBrokerMock>::save_state(&x).expect("state");

    let mut y = ice(3.0, 1.0);
    let mut b2 = OrderBrokerMock { pos: b.pos, ..OrderBrokerMock::default() };
    Strategy::<OrderBrokerMock>::load_state(&mut y, &saved);
    y.on_bar(&mut b2, &bar(1, PX));
    assert!(
        tagged_limits(&b2).is_empty(),
        "the restored child is live: a second submit would orphan it"
    );
    fill(&mut y, &mut b2, 0.5);
    y.on_bar(&mut b2, &bar(2, PX));
    assert_eq!(sizes(&b2), vec![1.0], "the restored slice filled, so the next one rests");
}

#[test]
fn a_child_the_saved_state_predates_is_adopted_and_progress_reads_the_position() {
    // Saved after the first slice filled, before the second was placed.
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 1.0);
    let saved = Strategy::<OrderBrokerMock>::save_state(&x).expect("state");
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(tagged_limits(&b).len(), 2, "...and the crash came after the second slice was sent");

    let mut y = ice(3.0, 1.0);
    let mut b2 = OrderBrokerMock { pos: b.pos, ..OrderBrokerMock::default() };
    Strategy::<OrderBrokerMock>::load_state(&mut y, &saved);
    y.on_order_event(&mut b2, &event(OrderEventKind::Accepted));
    y.on_bar(&mut b2, &bar(2, PX));
    assert!(tagged_limits(&b2).is_empty(), "the adopted child rests: nothing is sent");
    // It filled while the daemon was down: the position shows it, `on_fill` never does (0116).
    b2.pos += 1.0;
    y.on_order_event(&mut b2, &event(OrderEventKind::Filled));
    y.on_bar(&mut b2, &bar(3, PX));
    assert_eq!(sizes(&b2), vec![1.0], "the last slice, sized off the position delta");
    b2.pos += 1.0;
    y.on_order_event(&mut b2, &event_of("c2", OrderEventKind::Filled));
    y.on_bar(&mut b2, &bar(4, PX));
    assert_eq!(tagged_limits(&b2).len(), 1, "the total has filled by the position; nothing more");
    assert!(b2.cancelled.is_empty(), "the last slice ended on its own event, nothing to pull");
}

#[test]
fn the_position_finishing_the_parent_pulls_a_child_whose_fills_were_never_heard() {
    let (mut x, mut b) = (ice(2.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    b.pos = 2.0;
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(b.cancelled, vec![TAG.to_string()]);
    assert_eq!(tagged_limits(&b).len(), 1);
}

/// The minimum notional is judged on the CONTRACT's notional, the gate's: one ES contract (unit 50) at
/// 99 is 4,950, over a 1,000 floor, where `qty · price` (99) refused the start; ten Deribit dollars at
/// 99 are 10, under a 50 floor, where `qty · price` (990) let them out.
#[test]
fn the_minimum_notional_is_the_contracts_notional() {
    let start = |visible, value, floor| {
        let b = OrderBrokerMock { value: Some(value), ..grid(None, None, Some(floor)) };
        let (mut x, mut b) = (ice(3.0 * visible, visible), b);
        x.on_bar(&mut b, &bar(0, PX));
        (x.refusal().map(str::to_string), sizes(&b))
    };
    let es = start(1.0, ContractValue::linear(50.0), 1_000.0);
    assert_eq!(es, (None, vec![1.0]), "4,950 clears a 1,000 floor");
    let (why, sent) = start(10.0, ContractValue::inverse(1.0), 50.0);
    assert!(sent.is_empty(), "{sent:?}");
    assert!(why.is_some_and(|w| w.contains("notional of 10") && w.contains("minimum 50")));
    // the default unit is the plain `qty · price` it always was
    assert_eq!(start(1.0, ContractValue::default(), 99.0), (None, vec![1.0]));
}

#[test]
fn a_slice_under_the_instruments_minimum_is_never_sent() {
    // The start: a visible slice under the instrument's minimum is refused in words on the first hook.
    let start = |total, visible, mut b: OrderBrokerMock| {
        let mut x = ice(total, visible);
        x.on_bar(&mut b, &bar(0, PX));
        (x.refusal().map(str::to_string), sizes(&b))
    };
    // Binance's demo pass: a visible slice of 0.05 at 99 is a notional of 4.95, under a 5 floor.
    let (why, sent) = start(1.0, 0.05, grid(None, None, Some(5.0)));
    let why = why.expect("refused");
    assert!(sent.is_empty() && why.contains("notional") && why.contains("minimum 5"), "{why}");
    let (why, sent) = start(1.0, 0.05, grid(None, Some(0.1), None));
    assert!(sent.is_empty() && why.is_some_and(|w| w.contains("minimum size 0.1")));
    let at_both = start(3.0, 1.0, grid(None, Some(1.0), Some(99.0)));
    assert_eq!(at_both, (None, vec![1.0]), "a slice exactly at both minimums goes out");
    assert_eq!(start(3.0, 1.0, OrderBrokerMock::default()).1, vec![1.0], "no grid: no floor");

    // A last slice under the minimum (10 in 3s leaves 1, against a minimum of 2) rides on the one
    // before, so 3, 3 and 4 go out and never a 1.
    let (mut x, mut b) = (ice(10.0, 3.0), grid(None, Some(2.0), None));
    for ts in 0..3 {
        x.on_bar(&mut b, &bar(ts, PX));
        let q = tagged_limits(&b).last().expect("a slice").2;
        fill(&mut x, &mut b, q);
    }
    x.on_bar(&mut b, &bar(9, PX));
    assert_eq!(sizes(&b), vec![3.0, 3.0, 4.0]);
    assert!(x.refusal().is_some_and(|w| w.starts_with("done")), "{:?}", x.refusal());

    // Fills from elsewhere leave less than any slice can carry: it stops in words, sending nothing.
    let (mut x, mut b) = (ice(3.0, 1.0), grid(None, Some(1.0), None));
    x.on_bar(&mut b, &bar(0, PX));
    fill(&mut x, &mut b, 1.0);
    b.pos += 1.5;
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(sizes(&b), vec![1.0]);
    assert!(x.refusal().is_some_and(|w| w.contains("what is left cannot go out")));

    // A venue that refuses a slice anyway stops the executor rather than see it re-sent.
    let (mut x, mut b) = (ice(3.0, 1.0), OrderBrokerMock::default());
    x.on_bar(&mut b, &bar(0, PX));
    x.on_order_event(&mut b, &event(OrderEventKind::Rejected { reason: "MIN_NOTIONAL".into() }));
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(tagged_limits(&b).len(), 1);
    assert!(x.refusal().is_some_and(|w| w.contains("MIN_NOTIONAL")));
}

#[test]
fn the_price_snaps_passively_to_the_instruments_grid() {
    // The first slice's price, or the start refusal, on an instrument of grid `tick`.
    let first = |side, price, tick| {
        let c = OrderEmulationIcebergConfig { side, total: 1.0, visible: 1.0, price };
        let (mut x, mut b) = (OrderEmulationIceberg::new(c), grid(tick, None, None));
        x.on_bar(&mut b, &bar(0, PX));
        tagged_limits(&b)
            .first()
            .map(|t| t.3)
            .ok_or_else(|| x.refusal().unwrap_or_default().to_string())
    };
    let near = |got: Result<f64, String>, want: f64| got.is_ok_and(|px| (px - want).abs() < 1e-9);
    assert!(near(first(1, 100.07, Some(0.05)), 100.05), "a buy rounds down");
    assert!(near(first(-1, 100.07, Some(0.05)), 100.1), "a sell rounds up");
    assert!(near(first(1, 100.1, Some(0.1)), 100.1), "on the grid stays put despite float dust");
    // No grid: sent as typed; a grid coarser than the price snaps a buy to nothing, refused.
    assert!(near(first(1, 100.07, None), 100.07));
    let why = first(1, 99.0, Some(1000.0)).unwrap_err();
    assert!(why.contains("snaps to 0"), "{why}");
}

/// The slice's price is the ONE model rule for a passive price (`vike_model::snap_limit`, decision 0269 row 27,
/// the owner's D11): a sell a hundred-thousandth of a tick above a grid point stays on it, and the price is the
/// double nearest its decimal (0.3, not `3 · 0.1`).
#[test]
fn the_slice_rests_where_the_model_rule_puts_a_passive_price() {
    let first = |side, price, tick| {
        let c = OrderEmulationIcebergConfig { side, total: 1.0, visible: 1.0, price };
        let (mut x, mut b) = (OrderEmulationIceberg::new(c), grid(Some(tick), None, None));
        x.on_bar(&mut b, &bar(0, PX));
        tagged_limits(&b)[0].3
    };
    assert_eq!(first(-1, 100.000_000_1, 0.01), 100.0, "the model rule, not a tick up");
    assert_eq!(first(1, 0.3, 0.1), 0.3);
    for (side, price, tick) in [(-1, 100.000_000_1, 0.01), (1, 0.3, 0.1), (-1, 100.07, 0.05)] {
        assert_eq!(first(side, price, tick), vike_model::snap_limit(side, price, Some(tick)));
    }
}

/// **A saved state that does not read STOPS the executor** instead of slicing the parent again from a
/// state it does not have: under the live core's deadline too, the first hook pulls whatever the tag
/// names, once, and no slice goes out. A MISSING state (none was saved, the journal's `null`) is a
/// fresh parent and starts as before: with no child to await (the core's deadline `0`), the first
/// hook gives the slot up and the next one rests the first slice.
#[test]
fn a_saved_state_that_does_not_read_stops_the_executor_and_a_missing_one_starts_fresh() {
    let mut x = ice(3.0, 1.0).adoption_deadline(i64::MAX);
    Strategy::<OrderBrokerMock>::load_state(
        &mut x,
        &vike_model::StrategyState::from_json_value(&serde_json::json!("not a saved state")),
    );
    assert!(x.refusal().is_some(), "stopped at the restore");
    let mut b = OrderBrokerMock::default();
    x.on_bar(&mut b, &bar(0, PX));
    x.on_bar(&mut b, &bar(1, PX));
    assert_eq!(b.cancelled, vec![TAG.to_string()], "what the tag names is pulled, once");
    assert_eq!(b.submits(), 0, "no slice goes out");

    let mut y = ice(3.0, 1.0).adoption_deadline(0);
    Strategy::<OrderBrokerMock>::load_state(
        &mut y,
        &vike_model::StrategyState::from_json_value(&serde_json::Value::Null),
    );
    assert_eq!(y.refusal(), None, "a missing state is a fresh parent");
    let mut b = OrderBrokerMock::default();
    y.on_bar(&mut b, &bar(0, PX));
    y.on_bar(&mut b, &bar(1, PX));
    assert_eq!(sizes(&b), vec![1.0], "the first slice, as before");
}

/// The live core's deadline (`i64::MAX`): a restored slot never gives its slice up, so nothing is
/// cancelled by the tag and no slice is placed beside it, however long the engine names nothing.
#[test]
fn with_no_adoption_deadline_a_restored_slot_never_cancels_and_never_places() {
    let mut x = ice(3.0, 1.0).adoption_deadline(i64::MAX);
    Strategy::<OrderBrokerMock>::load_state(
        &mut x,
        &vike_model::StrategyState::from_json_value(&serde_json::json!({})),
    );
    let mut b = OrderBrokerMock::default();
    for now in [1_000, 1_000 + ADOPTION_DEADLINE_MS, i64::MAX / 2] {
        b.now = now;
        x.on_bar(&mut b, &bar(now, PX));
    }
    assert_eq!(b.submits() + b.cancelled.len(), 0, "{:?}", x.refusal());
}
