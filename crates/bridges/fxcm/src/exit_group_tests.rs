//! One exit over several trades, as the exec thread keeps it: how the legs' fills and the venue's removals read as the ONE
//! order the core knows, and what a cancel and an amend (the rate and the size) do to the legs. The venue is scripted
//! ([`crate::exit_script::Script`]); the envelopes are the shim's own, decoded by the real mapper.

use serde_json::json;
use vike_model::events::Event;

use super::*;
use crate::event_mapper::map_fxcm_event;
use crate::exit_script::Script;
use crate::sys::FxcmError;

const RATE: f64 = 1.1175;
const COID: &str = "c-stop";

fn leg(order: &str, trade: &str, units: i64) -> ExitLeg {
    ExitLeg {
        order_id: order.into(),
        trade_id: trade.into(),
        units,
        rate: RATE,
        account_id: "7".into(),
        offer_id: "1".into(),
        taken_over: false,
    }
}

/// A SELL stop over three 1000-unit trades.
fn three() -> ExitGroup {
    ExitGroup::new(
        ExitKind::Stop,
        false,
        "EUR/USD",
        vec![leg("A1", "101", 1000), leg("A2", "102", 1000), leg("A3", "103", 1000)],
    )
}

fn names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::Fill(_) => "Fill",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            Event::OrderRejected(_) => "OrderRejected",
            Event::OrderModified(_) => "OrderModified",
            Event::OrderModifyRejected(_) => "OrderModifyRejected",
            _ => "other",
        })
        .collect()
}

/// The decoded events of a closing fill of `order` for `units` (the real mapper, routed to `COID`).
fn fired(order: &str, trade: &str, units: i64) -> Vec<Event> {
    map_fxcm_event(
        &json!({"kind":"fill","order_id":order,"trade_id":format!("{trade}-close-{order}"),
                "instrument":"EUR/USD","side":"S","amount":units,"rate":1.1170,"commission":0.0,
                "remain_trade_id":"","ts":0}),
        COID,
    )
}

fn removed(order: &str) -> Vec<Event> {
    map_fxcm_event(&json!({"kind":"canceled","order_id":order,"ts":0}), COID)
}

fn modified_qty(evs: &[Event]) -> Option<f64> {
    evs.iter().find_map(|e| match e {
        Event::OrderModified(m) => m.new_qty,
        _ => None,
    })
}

// ---- the core's numbers --------------------------------------------------------------------------------------------

#[test]
fn an_amend_is_read_as_a_rate_a_size_or_both() {
    assert_eq!(Want::of(3000.0, None, Some(1.12)), Ok(Want { qty: None, rate: Some(1.12) }));
    assert_eq!(Want::of(3000.0, Some(2000.0), None), Ok(Want { qty: Some(2000), rate: None }));
    assert_eq!(
        Want::of(3000.0, Some(2000.0), Some(1.12)),
        Ok(Want { qty: Some(2000), rate: Some(1.12) })
    );
    // the order's own size is no change
    assert_eq!(
        Want::of(3000.0, Some(3000.0), Some(1.12)),
        Ok(Want { qty: None, rate: Some(1.12) })
    );
}

#[test]
fn an_amend_that_names_nothing_or_nonsense_is_refused_in_words() {
    assert!(Want::of(3000.0, None, None).expect_err("names nothing").contains("names neither"));
    assert!(Want::of(3000.0, Some(3000.0), None).is_err(), "the same size and no rate is nothing");
    for bad in [0.0, -1000.0, 1500.5, f64::NAN, f64::INFINITY, 1e30] {
        let why = Want::of(3000.0, Some(bad), None).expect_err("not a size");
        assert!(why.contains("whole number of base units"), "{bad}: {why}");
    }
    for bad in [0.0, -1.0, f64::NAN] {
        assert!(
            Want::of(3000.0, None, Some(bad)).expect_err("not a rate").contains("not a positive")
        );
    }
}

// ---- the fills ------------------------------------------------------------------------------------------------------

/// Each leg that fires is a fill of the ONE order: partial until the leg that leaves none resting, which is the fill.
#[test]
fn the_fills_of_the_legs_read_as_one_order() {
    let mut group = three();
    let (first, ended) = group.fold("A1", fired("A1", "101", 1000));
    assert_eq!(names(&first), ["Fill", "OrderPartiallyFilled"]);
    assert!(!ended);
    let (second, ended) = group.fold("A3", fired("A3", "103", 1000));
    assert_eq!(names(&second), ["Fill", "OrderPartiallyFilled"], "a leg need not fire in order");
    assert!(!ended);
    let (last, ended) = group.fold("A2", fired("A2", "102", 1000));
    assert_eq!(names(&last), ["Fill", "OrderFilled"]);
    assert!(ended, "the last leg ends the order");
    assert_eq!(group.total(), 3000);
}

/// An exit of one trade ends exactly as it always did.
#[test]
fn an_exit_of_one_leg_is_filled_or_cancelled_as_before() {
    let one = || ExitGroup::new(ExitKind::Limit, false, "EUR/USD", vec![leg("A1", "101", 1000)]);
    let (evs, ended) = one().fold("A1", fired("A1", "101", 1000));
    assert_eq!((names(&evs), ended), (vec!["Fill", "OrderFilled"], true));
    let (evs, ended) = one().fold("A1", removed("A1"));
    assert_eq!((names(&evs), ended), (vec!["OrderCanceled"], true));
}

/// A ForexConnect reconnect re-surfaces trades: a leg's fill delivered twice counts once, and the order does not end early.
#[test]
fn a_leg_whose_fill_is_delivered_twice_counts_once() {
    let mut group = three();
    group.fold("A1", fired("A1", "101", 1000));
    let (again, ended) = group.fold("A1", fired("A1", "101", 1000));
    assert_eq!(names(&again), ["Fill", "OrderPartiallyFilled"]);
    assert!(!ended);
    assert_eq!(group.covered(), 2000, "A2 and A3 still rest");
    assert_eq!(group.total(), 3000);
}

// ---- the venue's removal of a leg ---------------------------------------------------------------------------------

/// A trade closed another way takes its exits with it: while other legs rest the core is told the cover the venue still
/// backs, and the order ends cancelled with its last leg.
#[test]
fn a_leg_the_venue_removes_narrows_the_order_and_the_last_one_ends_it() {
    let mut group = three();
    let (evs, ended) = group.fold("A2", removed("A2"));
    assert_eq!(names(&evs), ["OrderModified"]);
    assert_eq!(modified_qty(&evs), Some(2000.0));
    assert!(!ended);
    let (evs, ended) = group.fold("A1", removed("A1"));
    assert_eq!(modified_qty(&evs), Some(1000.0));
    assert!(!ended);
    let (evs, ended) = group.fold("A3", removed("A3"));
    assert_eq!(names(&evs), ["OrderCanceled"]);
    assert!(ended);
}

/// What fired counts in the size the venue backs: one leg fired, one removed, one resting -> 2000 of the 3000.
#[test]
fn the_narrowed_size_counts_what_already_fired() {
    let mut group = three();
    group.fold("A1", fired("A1", "101", 1000));
    let (evs, _) = group.fold("A2", removed("A2"));
    assert_eq!(modified_qty(&evs), Some(2000.0));
    let (evs, ended) = group.fold("A3", removed("A3"));
    assert_eq!(
        names(&evs),
        ["OrderCanceled"],
        "cancelled after a partial fill: the legal terminal"
    );
    assert!(ended);
}

/// The echo of a leg that already fired, and the news of a venue order that is none of this group's, say nothing.
#[test]
fn the_echo_of_a_fired_leg_and_a_stranger_say_nothing() {
    let mut group = three();
    group.fold("A1", fired("A1", "101", 1000));
    assert!(group.fold("A1", removed("A1")).0.is_empty());
    assert!(group.fold("ZZ", removed("ZZ")).0.is_empty());
    assert_eq!(group.covered(), 2000);
}

// ---- a partial close ----------------------------------------------------------------------------------------------

/// The venue keeps the exit on the remainder (same order id, the amount cut, the NEW trade id): the leg follows, and the size
/// the venue backs is the remainder's.
#[test]
fn a_partial_close_moves_the_leg_to_the_remainder_and_cuts_it() {
    let mut group = three();
    group.follow_split("102", "202", 400);
    let moved: Vec<(&str, &str, i64)> =
        group.legs().iter().map(|l| (l.order_id.as_str(), l.trade_id.as_str(), l.units)).collect();
    assert_eq!(moved, [("A1", "101", 1000), ("A2", "202", 600), ("A3", "103", 1000)]);
    assert_eq!(group.covered(), 2600);
    // a trade no leg covers is none of this group's
    group.follow_split("999", "998", 100);
    assert_eq!(group.covered(), 2600);
    // the exit that then fires closes the remainder: the order still ends with its last leg, whatever the cut
    group.fold("A1", fired("A1", "101", 1000));
    group.fold("A3", fired("A3", "103", 1000));
    let (last, ended) = group.fold("A2", fired("A2", "202", 600));
    assert_eq!(names(&last), ["Fill", "OrderFilled"]);
    assert!(ended);
}

// ---- a cancel -----------------------------------------------------------------------------------------------------

#[test]
fn a_cancel_removes_every_leg() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    let done = group.cancel(&mut venue);
    assert_eq!(done.removed, ["A1", "A2", "A3"]);
    assert_eq!(done.refused, None);
    assert!(group.legs().is_empty());
    assert_eq!(venue.log, ["delete A1", "delete A2", "delete A3"]);
}

/// A leg that will not go does not keep the others resting: all are tried, and the words name what remains.
#[test]
fn a_leg_that_will_not_go_leaves_the_rest_removed_and_is_named() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    venue.delete_refused.insert("A2".into());
    let done = group.cancel(&mut venue);
    assert_eq!(done.removed, ["A1", "A3"]);
    let why = done.refused.expect("one is still resting");
    assert!(why.contains("1 of the 3 exits") && why.contains("order A2 on trade 102"), "{why}");
    assert_eq!(group.legs().len(), 1);
    assert_eq!(group.total(), 1000);
}

#[test]
fn a_cancel_past_the_time_bound_leaves_the_rest_and_says_so() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    // `may_start` is asked before the second leg: no time is left from the first answer on
    venue.out_of_time_from = Some(1);
    let done = group.cancel(&mut venue);
    assert_eq!(done.removed, ["A1"]);
    assert!(done.refused.expect("two remain").contains("time bound"));
    assert_eq!(venue.log, ["delete A1"]);
}

// ---- an amend of the rate ---------------------------------------------------------------------------------------------

fn no_one_owns(_: &str) -> bool {
    false
}

#[test]
fn the_rate_moves_every_leg() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    let done = group.amend(Want { qty: None, rate: Some(1.12) }, &no_one_owns, &mut venue);
    assert_eq!(done, Amended { rate: Some(1.12), ..Amended::default() });
    assert_eq!(venue.log, ["edit A1 1.12", "edit A2 1.12", "edit A3 1.12"]);
    assert!(group.legs().iter().all(|l| l.rate == 1.12));
    assert_eq!(group.rate(), 1.12);
}

/// A leg that refuses puts back the ones that moved: the core's single price must stay true of every leg.
#[test]
fn a_refused_rate_puts_the_moved_legs_back() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    venue.edit_refused.insert("A3".into());
    let done = group.amend(Want { qty: None, rate: Some(1.12) }, &no_one_owns, &mut venue);
    assert_eq!((done.qty, done.rate), (None, None), "nothing holds as asked");
    let why = done.refused.expect("refused");
    assert!(
        why.contains("trade 103 was not moved")
            && why.contains("were put back, so no rate changed"),
        "{why}"
    );
    assert_eq!(
        venue.log,
        ["edit A1 1.12", "edit A2 1.12", "edit A3 1.12", "edit A1 1.1175", "edit A2 1.1175"]
    );
    assert!(group.legs().iter().all(|l| l.rate == RATE));
}

/// A leg that cannot be put back is named, and its own rate is remembered so the next amend moves only what is not there.
#[test]
fn a_leg_that_cannot_be_put_back_is_named_and_keeps_the_rate_it_really_has() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    venue.edit_refused.insert("A3".into());
    venue.edit_refused_again.insert("A1".into());
    let done = group.amend(Want { qty: None, rate: Some(1.12) }, &no_one_owns, &mut venue);
    let why = done.refused.expect("refused");
    assert!(
        why.contains("trade 103 was not moved")
            && why.contains("could NOT be put back")
            && why.contains("order A1 on trade 101 stays at 1.12"),
        "{why}"
    );
    assert_eq!(group.legs().iter().map(|l| l.rate).collect::<Vec<_>>(), [1.12, RATE, RATE]);
    // the next amend to that rate moves what is not at it and leaves A1 alone
    let mut again = Script::new("[]", "[]");
    let done = group.amend(Want { qty: None, rate: Some(1.12) }, &no_one_owns, &mut again);
    assert_eq!(done.rate, Some(1.12));
    assert_eq!(again.log, ["edit A2 1.12", "edit A3 1.12"]);
}

#[test]
fn a_rate_every_leg_is_at_writes_nothing_and_is_told_as_standing() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    let done = group.amend(Want { qty: None, rate: Some(RATE) }, &no_one_owns, &mut venue);
    assert_eq!(done.rate, Some(RATE));
    assert!(venue.log.is_empty());
}

// ---- an amend of the size ---------------------------------------------------------------------------------------------

#[test]
fn a_smaller_size_removes_whole_legs_from_the_end() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    let done = group.amend(Want { qty: Some(1000), rate: None }, &no_one_owns, &mut venue);
    assert_eq!(done.qty, Some(1000));
    assert_eq!(done.removed, ["A3", "A2"]);
    assert_eq!(done.refused, None);
    assert_eq!(venue.log, ["delete A3", "delete A2"]);
    assert_eq!(group.legs().len(), 1);
    assert_eq!(group.total(), 1000);
}

/// A size inside a trade is refused with the totals that can be asked for, and nothing is sent.
#[test]
fn a_size_that_is_not_a_sum_of_whole_trades_is_refused_with_the_totals_to_ask_for() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    let done = group.amend(Want { qty: Some(1500), rate: None }, &no_one_owns, &mut venue);
    let why = done.refused.expect("refused");
    assert!(
        why.contains("covers whole trades")
            && why.contains("101 (1000), 102 (1000), 103 (1000)")
            && why.contains("1000, 2000, 3000")
            && why.contains("ORA-20134"),
        "{why}"
    );
    assert!(venue.log.is_empty());
    assert_eq!((done.qty, done.rate), (None, None));
    assert_eq!(group.legs().len(), 3);
}

/// The totals count what already fired: after one leg fired, 3000 is still the order's size and 2000 leaves one resting.
#[test]
fn the_size_asked_for_is_the_orders_total_and_counts_what_fired() {
    let mut group = three();
    group.fold("A1", fired("A1", "101", 1000));
    let mut venue = Script::new("[]", "[]");
    let done = group.amend(Want { qty: Some(2000), rate: None }, &no_one_owns, &mut venue);
    assert_eq!(done.qty, Some(2000));
    assert_eq!(done.removed, ["A3"]);
    assert_eq!(group.covered(), 1000);
    // a size that leaves nothing for the exit to cover is a cancel, not an amend
    let why = group
        .amend(Want { qty: Some(1000), rate: None }, &no_one_owns, &mut venue)
        .refused
        .expect("refused");
    assert!(why.contains("leaves nothing") && why.contains("cancel the order instead"), "{why}");
}

/// A removal that stops halfway is told as what holds: the size the venue still backs, then the refusal.
#[test]
fn a_removal_that_stops_halfway_tells_what_holds() {
    let mut group = three();
    let mut venue = Script::new("[]", "[]");
    venue.delete_refused.insert("A2".into());
    let done = group.amend(Want { qty: Some(1000), rate: None }, &no_one_owns, &mut venue);
    assert_eq!(done.removed, ["A3"]);
    assert_eq!(done.qty, Some(2000), "A3 went, A2 will not");
    let why = done.refused.clone().expect("refused");
    assert!(
        why.contains("lowered only as far as 2000") && why.contains("order A2 on trade 102"),
        "{why}"
    );
    assert_eq!(names(&done.events(COID, 7)), ["OrderModified", "OrderModifyRejected"]);
}

const BOOK: &str = r#"[
  {"trade_id":"101","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.1},
  {"trade_id":"102","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.2},
  {"trade_id":"103","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.3},
  {"trade_id":"104","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.4},
  {"trade_id":"105","instrument":"EUR/USD","buysell":"B","amount":1000,"open_time":46321.5}
]"#;

/// The group's own exits are on the venue: the book the amend reads shows them, and they are not free.
fn exits_of(group: &ExitGroup) -> String {
    let rows: Vec<String> = group
        .legs()
        .iter()
        .map(|l| {
            format!(
                r#"{{"order_id":"{}","trade_id":"{}","type":"S","buysell":"S","amount":{},"rate":{},"account_id":"7","offer_id":"1"}}"#,
                l.order_id, l.trade_id, l.units, l.rate
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

#[test]
fn a_larger_size_adds_exits_on_the_next_free_trades() {
    let mut group = ExitGroup::new(
        ExitKind::Stop,
        false,
        "EUR/USD",
        vec![leg("A1", "101", 1000), leg("A2", "102", 1000)],
    );
    let mut venue = Script::new(BOOK, &exits_of(&group));
    // the group's own order ids are routed: they are not for taking over
    let ours = |oid: &str| oid == "A1" || oid == "A2";
    let done = group.amend(Want { qty: Some(4000), rate: None }, &ours, &mut venue);
    assert_eq!(done.qty, Some(4000));
    assert_eq!(done.added, ["P1", "P2"]);
    assert_eq!(done.refused, None);
    assert_eq!(venue.log, ["place 103 1000 1.1175", "place 104 1000 1.1175"]);
    assert_eq!(group.legs().len(), 4);
    assert_eq!(group.total(), 4000);
}

/// A new exit rests at the rate the group rests at now: price and size together move the legs first, then add at the new one.
#[test]
fn a_size_and_a_rate_together_add_the_new_exits_at_the_new_rate() {
    let mut group = ExitGroup::new(ExitKind::Stop, false, "EUR/USD", vec![leg("A1", "101", 1000)]);
    let mut venue = Script::new(BOOK, &exits_of(&group));
    let ours = |oid: &str| oid == "A1";
    let done = group.amend(Want { qty: Some(2000), rate: Some(1.12) }, &ours, &mut venue);
    assert_eq!((done.qty, done.rate), (Some(2000), Some(1.12)));
    assert_eq!(venue.log, ["edit A1 1.12", "place 102 1000 1.12"]);
}

/// More than the open trades hold is refused, and nothing is sent: a stop never covers more than the position.
#[test]
fn a_size_past_the_position_is_refused_and_sends_nothing() {
    let mut group = ExitGroup::new(
        ExitKind::Stop,
        false,
        "EUR/USD",
        vec![leg("A1", "101", 1000), leg("A2", "102", 1000), leg("A3", "103", 1000)],
    );
    let mut venue = Script::new(BOOK, &exits_of(&group));
    let ours = |oid: &str| matches!(oid, "A1" | "A2" | "A3");
    // the book holds five trades, two free: 3000 + 3000 asks for three more
    let done = group.amend(Want { qty: Some(6000), rate: None }, &ours, &mut venue);
    let why = done.refused.expect("refused");
    assert!(why.contains("WHOLE trades") || why.contains("only 2000 more units"), "{why}");
    assert!(venue.log.is_empty(), "{:?}", venue.log);
    assert_eq!(group.legs().len(), 3);
}

/// A new exit that fails removes the ones this amend added: the size stays what it was.
#[test]
fn a_larger_size_that_fails_part_way_removes_what_it_added() {
    let mut group = ExitGroup::new(ExitKind::Stop, false, "EUR/USD", vec![leg("A1", "101", 1000)]);
    let mut venue = Script::new(BOOK, &exits_of(&group));
    venue.placements.push_back(Ok(crate::exit_script::accepted("N1", RATE)));
    venue.placements.push_back(Err(FxcmError::Native { code: -6, message: "ORA-20114".into() }));
    let ours = |oid: &str| oid == "A1";
    let done = group.amend(Want { qty: Some(3000), rate: None }, &ours, &mut venue);
    let why = done.refused.expect("refused");
    assert!(why.contains("ORA-20114") && why.contains("removed again"), "{why}");
    assert_eq!((done.qty, done.added.len()), (None, 0));
    assert_eq!(group.legs().len(), 1);
    assert_eq!(venue.log.last().map(String::as_str), Some("delete N1"));
}

/// An exit the venue holds at the group's rate that this process did not place is taken over by a larger size too.
#[test]
fn a_larger_size_takes_over_an_exit_that_rests_on_the_next_trade() {
    let mut group = ExitGroup::new(ExitKind::Stop, false, "EUR/USD", vec![leg("A1", "101", 1000)]);
    let rows = format!(
        "{},{}",
        exits_of(&group).trim_matches(|c| c == '[' || c == ']'),
        r#"{"order_id":"Z9","trade_id":"102","type":"S","buysell":"S","amount":1000,"rate":1.1175,"account_id":"7","offer_id":"1"}"#
    );
    let mut venue = Script::new(BOOK, &format!("[{rows}]"));
    let ours = |oid: &str| oid == "A1";
    let done = group.amend(Want { qty: Some(2000), rate: None }, &ours, &mut venue);
    assert_eq!(done.qty, Some(2000));
    assert_eq!(done.added, ["Z9"]);
    assert!(venue.log.is_empty(), "taken over, nothing written: {:?}", venue.log);
}

// ---- what the core is told ------------------------------------------------------------------------------------------

#[test]
fn an_amend_is_told_as_modified_and_or_rejected() {
    let both =
        Amended { qty: Some(2000), rate: None, refused: Some("no".into()), ..Amended::default() };
    assert_eq!(names(&both.events(COID, 1)), ["OrderModified", "OrderModifyRejected"]);
    let only_refused = Amended { refused: Some("no".into()), ..Amended::default() };
    assert_eq!(names(&only_refused.events(COID, 1)), ["OrderModifyRejected"]);
    let applied = Amended { qty: Some(2000), rate: Some(1.12), ..Amended::default() };
    let evs = applied.events(COID, 1);
    assert_eq!(names(&evs), ["OrderModified"]);
    match &evs[0] {
        Event::OrderModified(m) => {
            assert_eq!(
                (m.new_qty, m.new_price, m.client_order_id.as_str()),
                (Some(2000.0), Some(1.12), COID)
            );
        }
        other => panic!("{other:?}"),
    }
}
