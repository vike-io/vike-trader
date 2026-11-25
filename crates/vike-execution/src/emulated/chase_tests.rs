use super::*;
use crate::emulated::emulated_tests::tagged_limits;
use vike_marketdata::L2BookLevel;
use vike_marketdata::test_support::{book, flat_bar_unit_volume, quote};
use vike_model::OrderPegAnchor;
use vike_model::strategy::OrderBrokerMock;

/// A buy of 2 at the best bid, re-priced on every move (on [`broker`]'s 0.5 grid).
fn config() -> OrderEmulationChaseConfig {
    OrderEmulationChaseConfig {
        side: 1,
        qty: 2.0,
        anchor: OrderPegAnchor::Best,
        offset: 0.0,
        refresh_threshold: 0.0,
        max_chase: None,
        timeout_ms: None,
    }
}

fn chaser(c: OrderEmulationChaseConfig) -> OrderEmulationChase {
    let s = OrderEmulationChase::new(c);
    assert_eq!(s.refusal(), None, "{c:?} runs");
    s
}

/// One quote at `now`, as `bid` / `bid + 1`.
fn touch(s: &mut OrderEmulationChase, b: &mut OrderBrokerMock, now: i64, bid: f64) {
    b.now = now;
    s.on_quote_tick(b, &quote(now, bid, bid + 1.0));
}

fn fill(size: f64) -> Fill {
    Fill {
        side: 1,
        size,
        price: 100.0,
        fee: 0.0,
        timestamp: 0,
        is_maker: true,
        symbol: String::new(),
        tag: None,
    }
}

fn event(kind: OrderEventKind) -> OrderEvent {
    event_of("c1", kind)
}

/// An event of the child the core minted as `id`, under the chaser's tag.
fn event_of(id: &str, kind: OrderEventKind) -> OrderEvent {
    OrderEvent { order_client_id: id.into(), tag: Some(TAG.into()), kind }
}

fn modified(b: &OrderBrokerMock) -> Vec<f64> {
    b.modified
        .iter()
        .map(|(_, q, p)| {
            assert_eq!(*q, None, "a child is never re-sized");
            p.expect("a re-price names a price")
        })
        .collect()
}

/// The grid the engine's gate would hold the mounted symbol to (`OrderBroker::instrument_grid`).
fn on_grid(tick: Option<f64>, min_qty: Option<f64>, min_notional: Option<f64>) -> OrderBrokerMock {
    let grid = vike_model::Grid { tick_size: tick, lot_size: None, min_notional, min_qty };
    OrderBrokerMock { grid: Some(grid), ..OrderBrokerMock::default() }
}

/// An instrument on a 0.5 price grid and no minimum.
fn broker() -> OrderBrokerMock {
    on_grid(Some(0.5), None, None)
}

#[test]
fn the_chaser_works_to_the_instruments_grid() {
    // The instrument's 0.5, so a bid of 100.3 rests the buy at 100.0.
    let (mut s, mut b) = (chaser(config()), broker());
    touch(&mut s, &mut b, 0, 100.3);
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 2.0, 100.0)]);
}

#[test]
fn a_child_under_the_instruments_minimum_is_never_sent() {
    // At the start: a parent of 2 at 100 is a notional of 200, under a 500 floor.
    let (mut s, mut b) = (chaser(config()), on_grid(None, None, Some(500.0)));
    touch(&mut s, &mut b, 0, 100.0);
    assert!(tagged_limits(&b).is_empty());
    let why = s.refusal().expect("refused at the start");
    assert!(why.contains("the child cannot go out") && why.contains("minimum 500"), "{why}");
    // Later: child 1 (adopted, so freed by its own event) ends with 0.5 of the parent left, under the
    // 0.75 minimum size, so the chaser stops in words rather than send it.
    let (mut s, mut b) = (chaser(config()), on_grid(None, Some(0.75), None));
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Accepted));
    touch(&mut s, &mut b, 0, 100.0);
    b.pos = 1.5;
    s.on_fill(&mut b, &fill(1.5));
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Filled));
    touch(&mut s, &mut b, 1, 100.0);
    assert!(tagged_limits(&b).is_empty(), "no child of 0.5 goes out");
    let why = s.refusal().expect("stopped");
    assert!(why.contains("what is left cannot go out: a child of 0.5"), "{why}");
}

#[test]
fn the_child_chases_a_touch_moving_away_follows_one_moving_towards_and_holds_on_a_crossed_book() {
    let (mut s, mut b) = (chaser(config()), broker());
    touch(&mut s, &mut b, 0, 100.0);
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 2.0, 100.0)]);
    touch(&mut s, &mut b, 1, 100.5); // away: the bid runs up, the buy chases it
    touch(&mut s, &mut b, 2, 99.5); // towards: the bid falls, the buy follows it down
    s.on_quote_tick(&mut b, &quote(3, 101.0, 100.0)); // crossed: nothing to price off
    touch(&mut s, &mut b, 4, 99.5); // the same price as the child: nothing to send
    assert_eq!(modified(&b), vec![100.5, 99.5]);
    assert_eq!(tagged_limits(&b).len(), 1, "a live child is re-priced, never submitted again");
    assert!(b.cancelled.is_empty() && s.refusal().is_none());

    // The book lane prices the same way.
    s.on_order_book(&mut b, &book(&[L2BookLevel::new(98.0, 1.0)], &[L2BookLevel::new(99.0, 1.0)]));
    assert_eq!(modified(&b).last(), Some(&98.0));
}

#[test]
fn every_price_is_snapped_to_the_grid_away_from_the_far_touch() {
    // Mid of 100 / 101 is 100.5; +0.3 of offset is 100.8 for a buy and 100.2 for a sell.
    let mid = OrderEmulationChaseConfig { anchor: OrderPegAnchor::Mid, offset: 0.3, ..config() };
    for (side, want) in [(1, 100.5), (-1, 100.5)] {
        let (mut s, mut b) = (chaser(OrderEmulationChaseConfig { side, ..mid }), broker());
        touch(&mut s, &mut b, 0, 100.0);
        assert_eq!(tagged_limits(&b)[0].3, want, "side {side}");
    }
    let (mut s, mut b) = (chaser(mid), on_grid(None, None, None));
    touch(&mut s, &mut b, 0, 100.0);
    assert_eq!(tagged_limits(&b)[0].3, 100.8, "no grid known: as computed");
}

/// The child's price is the ONE model rule for a passive price (`vike_model::snap_limit`, decision 0269 row 27,
/// the owner's D11): a bid a hundred-millionth of a tick under a grid point is OFF it, so a buy rests a tick
/// lower, and the price is the double nearest its decimal (0.3, not `3 · 0.1`).
#[test]
fn the_child_rests_where_the_model_rule_puts_a_passive_price() {
    let (mut s, mut b) = (chaser(config()), on_grid(Some(0.01), None, None));
    touch(&mut s, &mut b, 0, 100.009_999_999_9);
    assert_eq!(tagged_limits(&b)[0].3, 100.0, "the model rule, not the grid point above");
    let (mut s, mut b) = (chaser(config()), on_grid(Some(0.1), None, None));
    touch(&mut s, &mut b, 0, 0.3);
    assert_eq!(tagged_limits(&b)[0].3, 0.3);
    for (bid, tick) in [(100.009_999_999_9, 0.01), (0.3, 0.1), (100.3, 0.5)] {
        let (mut s, mut b) = (chaser(config()), on_grid(Some(tick), None, None));
        touch(&mut s, &mut b, 0, bid);
        assert_eq!(
            tagged_limits(&b)[0].3,
            vike_model::snap_limit(1, bid, Some(tick)),
            "{bid} on {tick}"
        );
    }
}

#[test]
fn the_refresh_threshold_holds_small_moves_and_passes_large_ones() {
    let mut s = chaser(OrderEmulationChaseConfig { refresh_threshold: 1.0, ..config() });
    let mut b = broker();
    touch(&mut s, &mut b, 0, 100.0);
    touch(&mut s, &mut b, 1, 100.5);
    assert!(b.modified.is_empty(), "0.5 is under the threshold");
    touch(&mut s, &mut b, 2, 101.0);
    touch(&mut s, &mut b, 3, 100.5);
    assert_eq!(modified(&b), vec![101.0], "1.0 away re-prices; 0.5 back does not");
}

#[test]
fn max_chase_bounds_the_move_against_the_parent_then_pulls_the_child_and_stops_in_words() {
    let mut s = chaser(OrderEmulationChaseConfig { max_chase: Some(1.0), ..config() });
    let mut b = broker();
    touch(&mut s, &mut b, 0, 100.0);
    touch(&mut s, &mut b, 1, 90.0); // in the parent's favour: unbounded
    touch(&mut s, &mut b, 2, 101.0); // exactly max_chase above the first price: allowed
    assert_eq!(modified(&b), vec![90.0, 101.0]);
    touch(&mut s, &mut b, 3, 101.5);
    assert_eq!(b.cancelled, vec![TAG.to_string()]);
    let why = s.refusal().expect("stopped");
    assert!(why.contains("max_chase") && why.contains("the rest stays unfilled"), "{why}");
    touch(&mut s, &mut b, 4, 100.0);
    assert_eq!(
        (tagged_limits(&b).len(), b.modified.len()),
        (1, 2),
        "a stopped chaser sends nothing"
    );
    assert_eq!(b.placed.len(), 1, "it ends by a cancel, never a market order");
}

#[test]
fn the_timeout_pulls_the_child_and_stops_in_words() {
    let mut s = chaser(OrderEmulationChaseConfig { timeout_ms: Some(5_000), ..config() });
    let mut b = broker();
    touch(&mut s, &mut b, 1_000, 100.0);
    touch(&mut s, &mut b, 5_999, 100.5);
    assert!(b.cancelled.is_empty());
    b.now = 6_000;
    s.on_bar(&mut b, &flat_bar_unit_volume(6_000, 100.0));
    assert_eq!(b.cancelled, vec![TAG.to_string()], "measured from the first child");
    assert!(s.refusal().expect("stopped").contains("timed out 5000 ms"));
    touch(&mut s, &mut b, 6_001, 99.0);
    assert_eq!(tagged_limits(&b).len(), 1);
}

#[test]
fn a_fill_mid_chase_shrinks_the_parent_and_the_position_counts_fills_on_fill_never_heard() {
    let (mut s, mut b) = (chaser(config()), broker());
    touch(&mut s, &mut b, 0, 100.0);
    b.pos = 0.5;
    s.on_fill(&mut b, &fill(0.5));
    touch(&mut s, &mut b, 1, 100.5);
    assert_eq!(modified(&b), vec![100.5], "a partly filled child moves its price alone");
    // 1.5 more filled while nobody heard it (the daemon was down): the position says so.
    b.pos = 2.0;
    touch(&mut s, &mut b, 2, 101.0);
    assert_eq!(b.cancelled, vec![TAG.to_string()], "a child left resting is pulled");
    assert_eq!(s.refusal(), Some("the parent is done: 2 of 2 filled"));

    // A whole fill heard on `on_fill` ends it too, with nothing left to cancel.
    let (mut s, mut b) = (chaser(config()), broker());
    touch(&mut s, &mut b, 0, 100.0);
    s.on_fill(&mut b, &fill(2.0));
    assert!(s.refusal().is_some() && b.cancelled.is_empty());
}

#[test]
fn a_child_filled_short_of_the_parent_is_followed_by_one_child_for_the_rest() {
    let (mut s, mut b) = (chaser(config()), broker());
    // A tagged child nobody recorded: adopted, size unknown, so only its terminal event frees it.
    s.on_order_event(&mut b, &event(OrderEventKind::Accepted));
    touch(&mut s, &mut b, 0, 100.0);
    assert!(tagged_limits(&b).is_empty(), "an adopted child is re-priced, not submitted beside");
    assert_eq!(modified(&b), vec![100.0]);
    b.pos = 0.5;
    s.on_fill(&mut b, &fill(0.5));
    s.on_order_event(&mut b, &event(OrderEventKind::Filled));
    assert!(s.refusal().is_none(), "1.5 of the parent is left");
    touch(&mut s, &mut b, 1, 100.0);
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 1.5, 100.0)]);
}

/// Both children go out under the one tag, so an event counts only for the child it names by ORDER
/// ID: a late `Filled` or cancel of child 1 after child 2 went out sends no third child, stops
/// nothing, and the parent never overfills.
#[test]
fn a_late_event_of_an_older_child_neither_frees_nor_stops_the_current_one() {
    let (mut s, mut b) = (chaser(config()), broker());
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Accepted));
    touch(&mut s, &mut b, 0, 100.0);
    b.pos = 0.5;
    s.on_fill(&mut b, &fill(0.5));
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Filled));
    touch(&mut s, &mut b, 1, 100.0);
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 1.5, 100.0)], "child 2, for the rest");
    s.on_order_event(&mut b, &event_of("c2", OrderEventKind::Accepted));
    // Child 1's stragglers, after child 2 went out.
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Filled));
    s.on_order_event(&mut b, &event_of("c1", OrderEventKind::Canceled { reason: "late".into() }));
    touch(&mut s, &mut b, 2, 100.5);
    assert_eq!(tagged_limits(&b).len(), 1, "no third child beside child 2");
    assert_eq!(modified(&b).last(), Some(&100.5), "child 2 is still the one chased");
    assert_eq!(s.refusal(), None, "a late cancel of child 1 stops nothing");
    // Child 2 fills the rest: the parent is done at its size, never past it.
    b.pos = 2.0;
    s.on_fill(&mut b, &fill(1.5));
    assert_eq!(s.refusal(), Some("the parent is done: 2 of 2 filled"));
    assert_eq!(tagged_limits(&b).len(), 1);
}

#[test]
fn a_restart_restores_the_child_and_re_prices_it_instead_of_submitting_a_second() {
    let mut s = chaser(OrderEmulationChaseConfig { max_chase: Some(1.0), ..config() });
    let mut b = broker();
    touch(&mut s, &mut b, 0, 100.0);
    b.pos = 0.5;
    s.on_fill(&mut b, &fill(0.5));
    let saved = Strategy::<OrderBrokerMock>::save_state(&s).expect("a chaser saves its state");

    let mut after = chaser(OrderEmulationChaseConfig { max_chase: Some(1.0), ..config() });
    let mut b = OrderBrokerMock { pos: 0.5, ..broker() };
    Strategy::<OrderBrokerMock>::load_state(&mut after, &saved);
    touch(&mut after, &mut b, 1, 100.5);
    assert!(tagged_limits(&b).is_empty(), "the restored child is the one live child");
    assert_eq!(modified(&b), vec![100.5]);
    touch(&mut after, &mut b, 2, 101.5);
    let why = after.refusal().expect("max chase");
    assert!(why.contains("max_chase"), "measured from the SAVED first price: {why}");
    assert!(why.contains("0.5 of 2 filled"), "{why}");
}

/// The crash window: the state was saved BEFORE the child went out. A quote ahead of the tagged
/// event places nothing beside the child the saved state does not record; the event adopts it.
#[test]
fn after_a_restart_a_quote_before_the_tagged_event_places_nothing_and_the_event_adopts() {
    let fresh = chaser(config());
    let saved = Strategy::<OrderBrokerMock>::save_state(&fresh).expect("saved before any child");
    let mut s = chaser(config());
    let mut b = broker();
    Strategy::<OrderBrokerMock>::load_state(&mut s, &saved);
    touch(&mut s, &mut b, 1_000, 100.0);
    touch(&mut s, &mut b, 2_000, 100.5);
    assert_eq!(b.submits() + b.modified.len() + b.cancelled.len(), 0, "nothing goes out");
    s.on_order_event(&mut b, &event(OrderEventKind::Accepted));
    touch(&mut s, &mut b, 3_000, 100.5);
    assert!(tagged_limits(&b).is_empty(), "the adopted child is the one child");
    assert_eq!(modified(&b), vec![100.5]);
}

/// No event within `ADOPTION_DEADLINE_MS` of the first hook: the restored child is taken as gone,
/// pulled by its tag first, and the next touch rests the one child.
#[test]
fn after_a_restart_the_adoption_deadline_releases_the_slot_after_pulling_its_tag() {
    let mut s = chaser(config());
    let mut b = broker();
    // A missing payload (none was saved) keeps the fresh progress, and is still a restart.
    Strategy::<OrderBrokerMock>::load_state(
        &mut s,
        &vike_model::StrategyState::from_json_value(&serde_json::Value::Null),
    );
    touch(&mut s, &mut b, 1_000, 100.0);
    touch(&mut s, &mut b, 1_000 + ADOPTION_DEADLINE_MS - 1, 100.0);
    assert_eq!(b.submits() + b.cancelled.len(), 0);
    touch(&mut s, &mut b, 1_000 + ADOPTION_DEADLINE_MS, 100.0);
    assert_eq!(b.cancelled, vec![TAG.to_string()], "whatever the tag named is pulled");
    assert!(
        tagged_limits(&b).is_empty(),
        "not in the hook that pulls: the core drains its cancel after a submit"
    );
    touch(&mut s, &mut b, 1_000 + ADOPTION_DEADLINE_MS + 1, 100.0);
    assert_eq!(tagged_limits(&b), vec![(TAG.to_string(), 1, 2.0, 100.0)]);
}

#[test]
fn a_bar_run_is_refused_in_words_and_places_nothing() {
    let (mut s, mut b) = (chaser(config()), broker());
    assert_eq!(s.refusal(), None, "nothing seen yet");
    for ts in 0..3 {
        s.on_bar(&mut b, &flat_bar_unit_volume(ts, 100.0));
    }
    assert_eq!(s.refusal(), Some(BAR_RUN_REFUSAL));
    assert_eq!(b.submits() + b.modified.len() + b.cancelled.len(), 0);
    touch(&mut s, &mut b, 3, 100.0);
    assert_eq!(s.refusal(), None, "a quote is a book to follow");
    assert_eq!(tagged_limits(&b).len(), 1);
}

#[test]
fn terms_that_cannot_run_are_refused_in_words_and_place_nothing() {
    let mut s = OrderEmulationChase::new(OrderEmulationChaseConfig { qty: 0.0, ..config() });
    assert!(s.refusal().is_some_and(|w| w.contains("size 0")), "{:?}", s.refusal());
    let mut b = broker();
    touch(&mut s, &mut b, 0, 100.0);
    assert_eq!(b.submits(), 0);
}

/// The live core's deadline (`i64::MAX`): a restored slot never gives its child up, so nothing is
/// cancelled by the tag and nothing is placed beside it, however long the engine names nothing.
#[test]
fn with_no_adoption_deadline_a_restored_slot_never_cancels_and_never_places() {
    let mut s = chaser(config()).adoption_deadline(i64::MAX);
    let mut b = broker();
    Strategy::<OrderBrokerMock>::load_state(
        &mut s,
        &vike_model::StrategyState::from_json_value(&serde_json::Value::Null),
    );
    for now in [1_000, 1_000 + ADOPTION_DEADLINE_MS, i64::MAX / 2] {
        touch(&mut s, &mut b, now, 100.0);
    }
    assert_eq!(b.submits() + b.modified.len() + b.cancelled.len(), 0);
    s.on_order_event(&mut b, &event(OrderEventKind::Accepted));
    touch(&mut s, &mut b, i64::MAX / 2 + 1, 100.5);
    assert_eq!(
        (tagged_limits(&b).len(), modified(&b)),
        (0, vec![100.5]),
        "the named child is re-priced"
    );
}

/// **A saved state that does not read ENDS the chaser** instead of chasing again from a state it does
/// not have: under the live core's deadline too, the first touch pulls whatever the tag names, once,
/// and nothing is placed or re-priced.
#[test]
fn a_saved_state_that_does_not_read_ends_the_chaser_and_pulls_its_tag() {
    let mut s = chaser(config()).adoption_deadline(i64::MAX);
    let mut b = broker();
    Strategy::<OrderBrokerMock>::load_state(
        &mut s,
        &vike_model::StrategyState::from_json_value(&serde_json::json!("not a saved progress")),
    );
    assert!(s.refusal().is_some(), "ended at the restore");
    touch(&mut s, &mut b, 1_000, 100.0);
    touch(&mut s, &mut b, 2_000, 100.5);
    assert_eq!(b.cancelled, vec![TAG.to_string()], "what the tag names is pulled, once");
    assert_eq!(b.submits() + b.modified.len(), 0, "no child is placed or re-priced");
}

#[test]
fn a_child_the_venue_ends_stops_the_chaser_in_words_rather_than_resubmitting() {
    let (mut s, mut b) = (chaser(config()), broker());
    touch(&mut s, &mut b, 0, 100.0);
    s.on_order_event(&mut b, &event(OrderEventKind::Rejected { reason: "min notional".into() }));
    let why = s.refusal().expect("stopped");
    assert!(why.contains("min notional") && why.contains("0 of 2 filled"), "{why}");
    touch(&mut s, &mut b, 1, 100.5);
    assert_eq!(
        (tagged_limits(&b).len(), b.cancelled.len()),
        (1, 0),
        "no second child, nothing to pull"
    );
}
