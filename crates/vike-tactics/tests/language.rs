//! Every verb of the language and every rule of its book, against the recording broker: what is
//! sent, in which order, under which tag, and what a fill or an order's end makes the book do.

mod common;

use common::{BAR_MS, Bench, Call, SYMBOL, Script, quiet, script};
use vike_model::OrderSide;
use vike_model::OrderSide::{Long, Short};
use vike_model::{
    Commission, ContingencyType, OrderEventKind, OrderPeg, OrderPegAnchor, OrderPlacement,
    OrderType, snap_limit, snap_stop,
};
use vike_tactics::{RuleDefaults, RuleRun, RuleRunOptions, Tactics, TacticsTrail};

const TICK: Option<f64> = Some(0.01);

fn place(tag: &str, side: i32, qty: f64, order: OrderPlacement) -> Call {
    Call::Place { tag: tag.to_string(), side, qty, order, reduce_only: false }
}

fn close_order(tag: &str, side: i32, qty: f64) -> Call {
    let order = OrderPlacement::Market;
    Call::Place { tag: tag.to_string(), side, qty, order, reduce_only: true }
}

fn leg(exit: &str, tag: &str, side: i32, qty: f64, order: OrderPlacement) -> Call {
    Call::Leg { exit: exit.to_string(), tag: tag.to_string(), side, qty, order }
}

fn cancel(tag: &str) -> Call {
    Call::Cancel { tag: tag.to_string() }
}

fn amend(tag: &str, qty: f64, price: Option<f64>, trigger: Option<f64>) -> Call {
    Call::Amend { tag: tag.to_string(), qty: Some(qty), price, trigger }
}

const MARKET: OrderPlacement = OrderPlacement::Market;

/// `.chase()`: pegged to the order's own side of the touch, no offset.
const CHASED: OrderPlacement =
    OrderPlacement::Pegged { peg: OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 } };

fn stop(trigger: f64) -> OrderPlacement {
    OrderPlacement::Stop { trigger }
}

fn limit(price: f64) -> OrderPlacement {
    OrderPlacement::Limit { price }
}

/// A bench whose run is `run(script(on_bar))`.
fn bench(on_bar: common::Step, run: impl FnOnce(RuleRun<Script>) -> RuleRun<Script>) -> Bench {
    Bench::new(run(RuleRun::new(script(on_bar), SYMBOL)))
}

#[test]
fn side_and_header() {
    assert_eq!((i32::from(Long), i32::from(Short)), (1, -1));
    assert_eq!((f64::from(Long), f64::from(Short)), (1.0, -1.0));
    assert_eq!((-Long, -Short), (Short, Long));
    let plain = RuleDefaults::PLAIN;
    assert_eq!((plain.size, plain.pyramiding, plain.fee), (1.0, 1, Commission::None));
}

#[test]
fn an_entry_is_a_market_order_of_the_header_size_unless_it_states_a_level() {
    fn kinds(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("M", Long);
            s.entry("S", Long).size(2.0).stop(100.001);
            s.entry("L", Long).limit(99.999);
            s.entry("SL", Long).stop(100.019).limit(100.029);
        }
    }
    let mut b = Bench::new(
        RuleRun::new(script(kinds), SYMBOL)
            .defaults(RuleDefaults { size: 3.0, ..RuleDefaults::PLAIN }),
    );
    let stop_limit = OrderPlacement::StopLimit { trigger: 100.01, limit: 100.02 };
    assert_eq!(
        b.bar(0, 100.0),
        [
            place("M#1", 1, 3.0, MARKET),
            place("S#2", 1, 2.0, stop(100.01)),
            place("L#3", 1, 3.0, limit(99.99)),
            place("SL#4", 1, 3.0, stop_limit),
        ],
        "a buy stop snaps up, a buy limit down, a stop-limit's trigger as a limit"
    );
}

#[test]
fn the_same_name_again_amends_or_replaces() {
    fn again(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 | 1 => {
                s.entry("S", Long).stop(101.0);
            }
            2 => {
                s.entry("S", Long).limit(99.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(again);
    assert_eq!(b.bar(0, 100.0), [place("S#1", 1, 1.0, stop(101.0))]);
    assert_eq!(b.bar(1, 100.0), [amend("S#1", 1.0, None, Some(101.0))], "same kind: in place");
    assert_eq!(
        b.bar(2, 100.0),
        [cancel("S#1"), place("S#2", 1, 1.0, limit(99.0))],
        "another kind: a cancel and a new tag"
    );
}

#[test]
fn an_opposite_position_adds_itself_less_a_close_already_sent() {
    fn flip(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.entry("B", Short);
            }
            2 => {
                s.close("A");
                s.entry("C", Short);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(flip);
    b.bar(0, 100.0);
    assert_eq!(b.fill("A#1", 1.0, 100.0), []);
    assert_eq!(b.bar(1, 100.0), [place("B#2", -1, 2.0, MARKET)], "1 asked + the long of 1");
    assert_eq!(
        b.bar(2, 100.0),
        [close_order("drop A#3", -1, 1.0), place("C#4", -1, 1.0, MARKET)],
        "the close already sent takes the long off what the entry adds"
    );
}

#[test]
fn a_market_entry_the_other_way_sent_first_fills_first() {
    fn both(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("S", Short);
            s.entry("L", Long);
        }
    }
    let mut b = Bench::of(both);
    assert_eq!(
        b.bar(0, 100.0),
        [place("S#1", -1, 1.0, MARKET), place("L#2", 1, 2.0, MARKET)],
        "the long meets the short the first leaves"
    );
}

#[test]
fn pyramiding_refuses_and_cancels_the_entry_resting_under_the_name() {
    fn more(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
                s.entry("P", Long).stop(110.0);
            }
            1 => {
                s.entry("P", Long).stop(110.0);
                s.entry("Q", Long);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(more);
    b.bar(0, 100.0);
    assert_eq!(b.fill("A#1", 1.0, 100.0), [], "the stop on its own side asks what it asked");
    assert_eq!(
        b.bar(1, 100.0),
        [cancel("P#2")],
        "one lot held: both refused, the resting one cancelled"
    );

    let mut two =
        bench(more, |r| r.defaults(RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }));
    two.bar(0, 100.0);
    two.fill("A#1", 1.0, 100.0);
    assert_eq!(
        two.bar(1, 100.0),
        [amend("P#2", 1.0, None, Some(110.0)), place("Q#3", 1, 1.0, MARKET)],
        "a second lot is allowed"
    );
}

#[test]
fn an_entry_of_size_zero_only_closes() {
    fn zero(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.entry("Z", Short).size(0.0);
            }
            2 => {
                s.entry("R", Long).stop(110.0);
            }
            3 => {
                s.entry("R", Long).size(0.0).stop(110.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(zero);
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 100.0);
    assert_eq!(b.bar(1, 100.0), [place("Z#2", -1, 1.0, MARKET)], "it closes the long");
    b.fill("Z#2", 1.0, 100.0);
    assert_eq!(b.bar(2, 100.0), [place("R#3", 1, 1.0, stop(110.0))]);
    assert_eq!(b.bar(3, 100.0), [cancel("R#3")], "flat, it cancels the one resting under its name");
}

#[test]
fn a_free_order_fills_its_stated_size_whatever_is_held() {
    fn free(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
                s.order("F", Short).size(3.0).stop(95.0);
                s.order("G", Long).limit(90.0);
                s.order("N", Long).size(0.0);
            }
            1 => {
                s.order("M", Long).size(2.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(free);
    assert_eq!(
        b.bar(0, 100.0),
        [
            place("A#1", 1, 1.0, MARKET),
            place("F#2", -1, 3.0, stop(95.0)),
            place("G#3", 1, 1.0, limit(90.0))
        ],
        "a free order of size 0 sends nothing"
    );
    b.fill("A#1", 1.0, 100.0);
    assert_eq!(
        b.bar(1, 100.0),
        [place("M#4", 1, 2.0, MARKET)],
        "the pyramiding limit does not touch it"
    );
}

#[test]
fn closes_of_a_name_a_part_all_and_at_this_close() {
    fn closes(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.close("A").size(0.5);
                s.close("nothing");
            }
            2 => {
                s.close_all();
            }
            3 => {
                s.close("A").here();
            }
            _ => {}
        }
    }
    let mut b = Bench::of(closes);
    b.bar(0, 100.0);
    b.fill("A#1", 2.0, 100.0);
    assert_eq!(
        b.bar(1, 100.0),
        [close_order("drop A#2", -1, 0.5)],
        "a close of a name not held does nothing"
    );
    assert_eq!(b.bar(2, 100.0), [close_order("drop #3", -1, 2.0)]);
    assert_eq!(
        b.bar(3, 100.0),
        [
            cancel("drop A#2"),
            Call::AtClose { tag: "drop A#4".to_string(), side: -1, qty: 2.0, reduce_only: true },
        ],
        "a close at this close cannot amend the one resting"
    );
}

#[test]
fn an_entry_at_this_close_is_a_market_order_there_and_a_level_refuses_it() {
    fn at_close(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0).here();
            }
            1 => {
                s.entry("B", Long).size(1.0).limit(90.0).here();
            }
            _ => {}
        }
    }
    let mut b = Bench::of(at_close);
    assert_eq!(
        b.bar(0, 100.0),
        [Call::AtClose { tag: "A#1".to_string(), side: 1, qty: 2.0, reduce_only: false }]
    );
    b.fill("A#1", 2.0, 100.0);
    let refused = b.bar(1, 100.0);
    assert!(
        matches!(refused.as_slice(), [Call::Deny { tag, reason, .. }] if tag == "B#2" && reason.contains(".here()")),
        "{refused:?}"
    );
}

#[test]
fn stacked_closes_are_orders_of_their_own() {
    fn halves(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.close("A").size(0.5);
                s.close("A").size(0.5);
            }
            _ => {}
        }
    }
    let mut stacked = bench(halves, |r| r.stacked_closes(true));
    stacked.bar(0, 100.0);
    stacked.fill("A#1", 2.0, 100.0);
    assert_eq!(
        stacked.bar(1, 100.0),
        [close_order("drop A 2#2", -1, 0.5), close_order("drop A 3#3", -1, 0.5)]
    );
    let mut plain = Bench::of(halves);
    plain.bar(0, 100.0);
    plain.fill("A#1", 2.0, 100.0);
    assert_eq!(
        plain.bar(1, 100.0),
        [close_order("drop A#2", -1, 0.5), amend("drop A#2", 0.5, None, None)],
        "otherwise the later replaces the earlier"
    );
}

#[test]
fn an_exit_is_a_stop_and_a_target_linked_by_the_language_and_spent_once_fired() {
    fn bracket(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1.. => {
                s.exit("X").from("A").size(1.0).stop(98.005).take(102.004);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(bracket);
    b.bar(0, 100.0);
    b.fill("A#1", 2.0, 100.0);
    assert_eq!(
        b.bar(1, 100.0),
        [leg("X", "X#2", -1, 1.0, stop(98.0)), leg("X", "X#3", -1, 1.0, limit(102.01))],
        "a sell stop snaps down and a sell limit up, the stop first"
    );
    assert_eq!(
        b.bar(2, 100.0),
        [amend("X#2", 1.0, None, Some(98.0)), amend("X#3", 1.0, Some(102.01), None)],
        "issued again, its legs move in place"
    );
    assert_eq!(b.fill("X#2", 1.0, 98.0), [cancel("X#3")], "the stop's fill cancels the target");
    assert_eq!(b.bar(3, 100.0), [], "spent while a lot of its name lives");
}

#[test]
fn an_exit_with_no_lots_and_no_entry_coming_does_nothing() {
    fn alone(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.exit("X").stop(95.0);
        }
    }
    assert_eq!(Bench::of(alone).bar(0, 100.0), []);
}

#[test]
fn a_trailing_leg_from_its_activation() {
    fn trail(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.exit("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 50.0 });
            }
            2 => {
                s.exit("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 50.0 });
            }
            3 => {
                s.exit("A").trail(TacticsTrail { after_ticks: 200.0, by_ticks: 50.0 });
            }
            _ => {}
        }
    }
    let mut b = Bench::of(trail);
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 100.0);
    let at = |after: f64| OrderPlacement::Trailing {
        trail: 50.0 * 0.01,
        activation: Some(100.0 + after * 0.01),
    };
    assert_eq!(
        b.bar(1, 100.0),
        [leg("A", "A#2", -1, 1.0, at(100.0))],
        "named as its entry, it covers it"
    );
    assert_eq!(
        b.bar(2, 100.0),
        [amend("A#2", 1.0, None, None)],
        "the same trail moves its size only"
    );
    assert_eq!(
        b.bar(3, 100.0),
        [cancel("A#2"), leg("A", "A#3", -1, 1.0, at(200.0))],
        "another activation is a new trail"
    );
}

#[test]
fn a_trail_with_no_offset_is_an_if_touched_order_or_a_market_once_reached() {
    fn touch(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1.. => {
                s.exit("X").from("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 0.0 });
            }
            _ => {}
        }
    }
    let if_touched = |limit| OrderPlacement::TakeProfit { trigger: 101.0, limit };
    let mut b = Bench::of(touch);
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 100.0);
    assert_eq!(
        b.bar(1, 100.0),
        [leg("X", "X#2", -1, 1.0, if_touched(Some(101.0)))],
        "a limit at its level"
    );
    assert_eq!(
        b.bar(2, 101.5),
        [cancel("X#2"), leg("X", "X#3", -1, 1.0, MARKET)],
        "a level the mark has reached leaves with the next market fill"
    );

    let mut market = bench(touch, |r| r.touch_at_market(true));
    market.bar(0, 100.0);
    market.fill("A#1", 1.0, 100.0);
    assert_eq!(
        market.bar(1, 100.0),
        [leg("X", "X#2", -1, 1.0, if_touched(None))],
        "a market once touched"
    );
}

#[test]
fn a_trail_issued_with_its_entry_waits_for_the_fill_price() {
    fn with_entry(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
            s.exit("X").from("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 50.0 });
        }
    }
    let mut b = Bench::of(with_entry);
    assert_eq!(b.bar(0, 100.0), [place("A#1", 1, 1.0, MARKET)]);
    let order =
        OrderPlacement::Trailing { trail: 50.0 * 0.01, activation: Some(102.0 + 100.0 * 0.01) };
    assert_eq!(b.fill("A#1", 1.0, 102.0), [leg("X", "X#2", -1, 1.0, order)]);
}

#[test]
fn levels_in_percent_of_the_entry() {
    fn pct(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
                s.exit("A").stop_pct(1.0).take_pct(2.0);
            }
            1 => {
                s.exit("A").stop_pct(1.0).take_pct(2.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(pct);
    assert_eq!(b.bar(0, 100.0), [place("A#1", 1, 1.0, MARKET)], "they wait for the entry's price");
    let cut = snap_stop(-1, 105.0 - 105.0 * 1.0 / 100.0, TICK);
    let take = snap_limit(-1, 105.0 + 105.0 * 2.0 / 100.0, TICK);
    assert_eq!(
        b.fill("A#1", 1.0, 105.0),
        [leg("A", "A#2", -1, 1.0, stop(cut)), leg("A", "A#3", -1, 1.0, limit(take))]
    );
    assert_eq!(
        b.bar(1, 105.0),
        [amend("A#2", 1.0, None, Some(cut)), amend("A#3", 1.0, Some(take), None)],
        "from the held entry's price, the same levels"
    );
}

#[test]
fn bound_exits_share_their_entry_in_the_order_issued() {
    fn pair(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("E", Long).size(2.0).stop(105.0);
            s.exit("half").from("E").size(0.5).stop(95.0);
            s.exit("rest").from("E").stop(99.0);
        }
    }
    let mut b = bench(pair, |r| r.bound_exits(true));
    assert_eq!(b.bar(0, 100.0), [place("E#1", 1, 2.0, stop(105.0))], "both wait for the entry");
    assert_eq!(
        b.fill("E#1", 2.0, 105.0),
        [leg("half", "half#2", -1, 0.5, stop(95.0)), leg("rest", "rest#3", -1, 1.5, stop(99.0))]
    );

    let mut plain = Bench::of(pair);
    assert_eq!(
        plain.bar(0, 100.0),
        [
            place("E#1", 1, 2.0, stop(105.0)),
            leg("half", "half#2", -1, 0.5, stop(95.0)),
            leg("rest", "rest#3", -1, 2.0, stop(99.0)),
        ],
        "the reference's backtest rule: each rests at once, sized to the whole entry"
    );
}

#[test]
fn live_an_exit_issued_with_its_entry_is_its_oto_child() {
    fn bracket(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("E", Long).stop(105.0);
                s.exit("X").from("E").stop(95.0).take(110.0);
            }
            1 => {
                s.exit("X").from("E").stop(96.0).take(110.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(bracket);
    b.broker.reference = false;
    let calls = b.bar(0, 100.0);
    let [Call::Group(legs)] = calls.as_slice() else { panic!("one order list: {calls:?}") };
    let shape: Vec<_> = legs
        .iter()
        .map(|(tag, l)| {
            (
                tag.as_str(),
                l.order_type.clone(),
                l.reduce_only,
                l.parent,
                l.contingency,
                l.linked.clone(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("E#1", OrderType::Stop, false, None, None, vec![]),
            ("X#2", OrderType::Stop, true, Some(0), Some(ContingencyType::Oco), vec![2]),
            ("X#3", OrderType::Limit, true, Some(0), Some(ContingencyType::Oco), vec![1]),
        ],
        "never a reduce-only order resting while flat"
    );
    assert_eq!(
        b.bar(1, 100.0),
        [amend("X#2", 1.0, None, Some(96.0)), amend("X#3", 1.0, Some(110.0), None)],
        "the children move in place"
    );
}

#[test]
fn live_an_exit_issued_after_its_entry_was_sent_waits_for_the_fill() {
    fn later(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("E", Long).stop(105.0);
            }
            1 => {
                s.exit("X").from("E").stop(95.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(later);
    b.broker.reference = false;
    assert_eq!(
        b.bar(0, 100.0),
        [place("E#1", 1, 1.0, stop(105.0))],
        "an entry alone is sent at the end of its pass"
    );
    assert_eq!(b.bar(1, 100.0), [], "no leg can join an order already sent");
    assert_eq!(b.fill("E#1", 1.0, 105.0), [leg("X", "X#2", -1, 1.0, stop(95.0))]);
}

#[test]
fn a_group_member_s_fill_cancels_the_rest() {
    fn grouped(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).limit(98.0).group("G");
            s.entry("B", Short).limit(105.0).group("G");
        }
    }
    let mut b = Bench::of(grouped);
    b.bar(0, 100.0);
    assert_eq!(b.fill("A#1", 1.0, 98.0), [cancel("B#2")]);
}

#[test]
fn cancel_by_name_and_all() {
    fn pulls(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).stop(110.0);
                s.order("F", Short).limit(120.0);
            }
            1 => s.cancel("A"),
            2 => s.cancel_all(),
            _ => {}
        }
    }
    let mut b = Bench::of(pulls);
    b.bar(0, 100.0);
    assert_eq!(b.bar(1, 100.0), [cancel("A#1")]);
    assert_eq!(b.bar(2, 100.0), [cancel("F#2")], "a cancelled order is not cancelled twice");
}

#[test]
fn the_fills_a_day_cap_cancels_closes_and_blocks_the_day() {
    fn trade(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.entry("B", Short);
                s.order("F", Long).stop(150.0);
            }
            _ => {
                s.entry("C", Long);
            }
        }
    }
    let mut b = bench(trade, |r| r.fills_a_day(2));
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 100.0);
    b.bar(1, 100.0);
    assert_eq!(
        b.fill("B#2", 2.0, 101.0),
        [cancel("F#3"), close_order("cap close#4", 1, 1.0)],
        "the reversal is the day's second fill"
    );
    assert_eq!(b.bar(2, 101.0), [], "no verb acts again that day");
    let day = vike_model::MS_PER_DAY / BAR_MS;
    assert_eq!(
        b.bar(day, 101.0),
        [place("C#5", 1, 1.0, MARKET)],
        "the next day it does, the cap's close (still resting here) taken off the short it adds"
    );
}

#[test]
fn the_trading_window_and_its_last_bar() {
    fn every(s: &mut Tactics<'_>, k: i64) {
        s.order(&format!("F{k}"), Long);
    }
    fn after(s: &mut Tactics<'_>, k: i64) {
        s.order(&format!("R{k}"), Short);
    }
    let rule = Script { after_fill: after, ..script(every) };
    let mut b = Bench::new(RuleRun::new(rule, SYMBOL).trading(BAR_MS, 3 * BAR_MS));
    assert_eq!(b.bar(0, 100.0), [], "before the window");
    assert_eq!(b.bar(1, 100.0), [place("F1#1", 1, 1.0, MARKET)]);
    assert_eq!(b.bar(2, 100.0), [place("F2#2", 1, 1.0, MARKET)]);
    let last = common::bar(3, 100.0);
    let fill = vike_model::Fill {
        side: 1,
        size: 1.0,
        price: 100.0,
        fee: 0.0,
        timestamp: 3 * BAR_MS,
        is_maker: false,
        symbol: SYMBOL.to_string(),
        tag: Some("F2#2".to_string()),
    };
    b.broker.position = 1.0;
    b.run.on_fill_within(&mut b.broker, &fill, &last);
    assert_eq!(
        std::mem::take(&mut b.broker.calls),
        [place("R3#3", -1, 1.0, MARKET)],
        "a fill inside the window's last bar makes the rule act there"
    );
    assert_eq!(b.bar(3, 100.0), [], "on the window's last bar itself, nothing");
}

/// A rule reads its run's window and tick, and the four switches go in and come out as one value.
#[test]
fn a_rule_reads_the_runs_window_and_tick_and_the_switches_are_one_value() {
    fn reads(s: &mut Tactics<'_>, _k: i64) {
        assert_eq!((s.window(), s.tick()), (Some((BAR_MS, 3 * BAR_MS)), TICK));
    }
    Bench::new(RuleRun::new(script(reads), SYMBOL).trading(BAR_MS, 3 * BAR_MS)).bar(0, 100.0);
    fn unbounded(s: &mut Tactics<'_>, _k: i64) {
        assert_eq!(s.window(), None, "a run with no window states none");
    }
    Bench::of(unbounded).bar(0, 100.0);

    let run = || RuleRun::new(script(quiet), SYMBOL);
    assert_eq!(run().options(), RuleRunOptions::OFF);
    let set = RuleRunOptions { bound_exits: true, fills_a_day: Some(3), ..RuleRunOptions::OFF };
    assert_eq!(run().bound_exits(true).fills_a_day(3).options(), set, "the builders' own value");
    let all = RuleRunOptions { stacked_closes: true, touch_at_market: true, ..set };
    assert_eq!(run().with(all).options(), all);
    assert_eq!(
        run().with(all).with(RuleRunOptions::OFF).options(),
        RuleRunOptions::OFF,
        "a cap lifted"
    );
}

#[test]
fn after_fill_runs_for_a_fill_inside_the_bar_under_way_only() {
    fn enter(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
            s.exit("X").from("A").take(101.0);
        }
    }
    fn turn(s: &mut Tactics<'_>, _k: i64) {
        if s.position() == 0.0 {
            s.entry("B", Short);
        }
    }
    let mut b = Bench::new(RuleRun::new(Script { after_fill: turn, ..script(enter) }, SYMBOL));
    b.bar(0, 100.0);
    assert_eq!(
        b.fill_at("A#1", 1.0, 100.0, 0),
        [],
        "a fill stamped with the closed bar (at its close)"
    );
    assert_eq!(
        b.fill_at("X#2", 1.0, 101.0, BAR_MS),
        [place("B#3", -1, 1.0, MARKET)],
        "inside the bar under way"
    );
    assert_eq!(b.run.rule().reruns, [0], "it read the last bar the seam shows");
}

#[test]
fn an_order_s_end_takes_it_off_the_book_and_its_waiting_exit_dies() {
    fn refused(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("E", Long).stop(105.0);
                s.exit("X").from("E").stop(95.0);
            }
            1 => {
                s.entry("E", Long);
            }
            _ => {}
        }
    }
    let run = |end: bool| {
        let mut b = bench(refused, |r| r.bound_exits(true));
        b.bar(0, 100.0);
        if end {
            b.end("E#1", OrderEventKind::Rejected { reason: "no stop here".to_string() });
        }
        b.bar(1, 100.0);
        b.fill("E#2", 1.0, 100.0)
    };
    assert_eq!(run(true), [], "the exit waiting for the refused entry died with it");
    assert_eq!(
        run(false),
        [leg("X", "X#3", -1, 1.0, stop(95.0))],
        "without the end the exit still waits for the name, and arms on its next entry"
    );
}

#[test]
fn a_fill_no_order_made_only_closes_lots() {
    fn guarded(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
        }
        if k == 1 {
            s.exit("X").from("A").stop(95.0);
        }
    }
    let mut b = Bench::of(guarded);
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 100.0);
    b.bar(1, 100.0);
    assert_eq!(
        b.fill_side(None, -1, 1.0, 90.0, 2 * BAR_MS),
        [cancel("X#2")],
        "the engine's margin call empties the lot, and its exit dies"
    );
    assert_eq!(b.run.rule().reruns, [0], "the rule ran after its entry's fill, not after this one");
}

#[test]
fn reads_lots_first_in_first_out_and_their_average() {
    fn three(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long);
            }
            1 => {
                s.entry("B", Long);
            }
            2 => {
                s.entry("S", Short).size(1.0);
            }
            _ => {}
        }
    }
    let mut b = bench(three, |r| r.defaults(RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN }));
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 10.0);
    b.bar(1, 100.0);
    b.fill("B#2", 1.0, 11.0);
    // the engine's own average, which the lots' must not be read from
    b.broker.avg = 99.0;
    b.bar(2, 100.0);
    b.fill("S#3", 3.0, 12.0);
    b.bar(3, 100.0);
    let rule = b.run.rule();
    assert!(rule.seen[0].2.is_nan(), "flat, the average is NaN");
    assert_eq!(rule.seen[1].2, 10.0, "one lot, its own price");
    assert_eq!(rule.seen[2], (2, 2.0, 10.5), "two lots, their average");
    let lots: Vec<_> =
        rule.lots[3].0.iter().map(|l| (l.name.as_str(), l.side, l.qty, l.price)).collect();
    assert_eq!(
        lots,
        [("S", Short, 1.0, 12.0)],
        "the reversal closed both lots and opened the rest"
    );
    let named: Vec<_> = rule.lots[2].0.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(named, ["A", "B"]);
}

/// The lots match by SYMBOL, as the closed-trade ledger's do: a fill of another symbol (here an
/// engine's own close of a position the rule does not trade) closes none of the rule's lots.
#[test]
fn a_fill_of_another_symbol_leaves_the_lots() {
    fn one(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long);
        }
    }
    let mut b = Bench::of(one);
    b.bar(0, 100.0);
    b.fill("A#1", 1.0, 10.0);
    let other = vike_model::Fill {
        side: -1,
        size: 1.0,
        price: 11.0,
        fee: 0.0,
        timestamp: BAR_MS,
        is_maker: false,
        symbol: "OTHER".to_string(),
        tag: None,
    };
    vike_model::Strategy::on_fill(&mut b.run, &mut b.broker, &other);
    b.bar(1, 100.0);
    let lots: Vec<_> = b.run.rule().lots[1].0.iter().map(|l| (l.name.as_str(), l.qty)).collect();
    assert_eq!(lots, [("A", 1.0)], "the other symbol's fill is not this rule's");
}

#[test]
fn a_resting_entry_on_the_position_s_side_asks_only_what_it_asked() {
    fn resize(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("S", Short);
            }
            1 => {
                s.entry("P", Long).stop(110.0);
                s.order("F", Long).size(2.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(resize);
    b.bar(0, 100.0);
    b.fill("S#1", 1.0, 100.0);
    assert_eq!(
        b.bar(1, 100.0),
        [place("P#2", 1, 2.0, stop(110.0)), place("F#3", 1, 2.0, MARKET)],
        "facing the short it adds it"
    );
    assert_eq!(b.fill("F#3", 2.0, 100.0), [amend("P#2", 1.0, None, None)], "now long, it asks 1");
}

#[test]
fn a_rule_is_object_safe_and_side_reads_through_snaps() {
    let rules: Vec<Box<dyn vike_tactics::Rule>> = vec![Box::new(script(quiet))];
    assert_eq!(rules.len(), 1);
    fn snaps(s: &mut Tactics<'_>, _k: i64) {
        assert_eq!(
            (s.snap_stop(OrderSide::Long, 10.001), s.snap_stop(OrderSide::Short, 10.001)),
            (10.01, 10.0)
        );
        assert_eq!(
            (s.snap_limit(OrderSide::Long, 10.001), s.snap_limit(OrderSide::Short, 10.001)),
            (10.0, 10.01)
        );
    }
    Bench::of(snaps).bar(0, 100.0);
}

/// `Tactics::sizing`: the equity, the header's commission and the run's slippage and quantity step,
/// sized by the percent-of-equity law; a boxed rule (what a table of rules hands out) runs as it is.
#[test]
fn sizing_reads_the_header_and_the_run_and_a_boxed_rule_runs() {
    use vike_model::Strategy;
    fn sizes(s: &mut Tactics<'_>, _k: i64) {
        let sizing = s.sizing();
        assert_eq!((sizing.equity(), sizing.qty_step()), (1_000.0, Some(0.001)));
        assert_eq!(
            (sizing.slipped(Long, 100.0), sizing.slipped(Short, 100.0)),
            (100.0 + 0.02, 100.0 - 0.02)
        );
        // 500 over one unit of 100 with 0.1% on it, 4.995004..., floored to the step
        assert_eq!(sizing.size_for_share(0.5, 100.0), 4.995);
        assert_eq!((sizing.size_for_cash(250.0, 100.0), sizing.floor(1.234_56)), (2.5, 1.234));
        s.entry("L", Long).size(sizing.size_for_share(0.5, 100.0));
    }
    fn plain(s: &mut Tactics<'_>, _k: i64) {
        let sizing = s.sizing();
        assert_eq!(
            (sizing.qty_step(), sizing.slipped(Long, 100.0)),
            (None, 100.0),
            "nothing stated"
        );
        assert_eq!(sizing.size_for_share(0.5, 100.0), 5.0, "no commission, no step");
    }
    let header = RuleDefaults { fee: Commission::percent(0.1), ..RuleDefaults::PLAIN };
    let boxed: Box<dyn vike_tactics::Rule + Send> = Box::new(script(sizes));
    let mut run = RuleRun::new(boxed, SYMBOL).defaults(header).slippage(0.02).qty_step(0.001);
    let mut broker = common::Recorder { mark: 100.0, reference: true, ..Default::default() };
    run.on_bar(&mut broker, &common::bar(0, 100.0));
    assert_eq!(broker.calls, [place("L#1", 1, 4.995, MARKET)], "the boxed rule sent its entry");
    Bench::of(plain).bar(0, 100.0);
}

#[test]
fn a_chase_is_a_pegged_order_and_an_iceberg_a_limit_showing_its_slice() {
    fn kinds(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("C", Long).chase();
            s.order("O", Short).chase();
            s.entry("I", Long).size(30.0).limit(99.999).iceberg(10.0);
            s.order("J", Short).size(3.0).limit(100.001).iceberg(1.0);
        }
    }
    assert_eq!(
        Bench::of(kinds).bar(0, 100.0),
        [
            place("C#1", 1, 1.0, CHASED),
            place("O#2", -1, 1.0, CHASED),
            place("I#3", 1, 30.0, OrderPlacement::Iceberg { price: 99.99, display: 10.0 }),
            place("J#4", -1, 3.0, OrderPlacement::Iceberg { price: 100.01, display: 1.0 }),
        ],
        "pegged to its own side with no offset; an iceberg's limit snaps as a limit does"
    );
}

#[test]
fn an_iceberg_of_zero_and_terms_no_kind_states_are_refused_by_name() {
    fn bad(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("I", Long).size(10.0).limit(99.0).iceberg(5.0);
            }
            1 => {
                s.entry("I", Long).size(10.0).limit(99.0).iceberg(0.0);
            }
            2 => {
                s.entry("A", Long).iceberg(5.0);
                s.entry("B", Long).stop(101.0).limit(99.0).iceberg(5.0);
                s.entry("C", Long).limit(99.0).chase();
                s.order("D", Long).chase().iceberg(1.0);
                s.order("E", Long).limit(99.0).iceberg(f64::NAN);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(bad);
    let iceberg = OrderPlacement::Iceberg { price: 99.0, display: 5.0 };
    assert_eq!(b.bar(0, 100.0), [place("I#1", 1, 10.0, iceberg)]);
    let hidden = "order \"I\": .iceberg(0) shows no slice: the visible size must be above zero (a \
                  hidden order is refused): order refused; the order resting under \"I\" is cancelled";
    assert_eq!(
        b.bar(1, 100.0),
        [cancel("I#1"), Call::Deny { tag: "I#2".into(), qty: 10.0, reason: hidden.into() }],
        "a hidden order is denied by name, never dropped; as a re-issue it cancels the one resting"
    );
    let denied: Vec<(String, bool)> = b
        .bar(2, 100.0)
        .into_iter()
        .map(|c| match c {
            Call::Deny { tag, reason, .. } => {
                let named =
                    [".iceberg(5) is a limit's", ".chase() rests", ".iceberg(NaN) shows no"];
                (tag, named.iter().any(|w| reason.contains(w)))
            }
            other => panic!("only refusals: {other:?}"),
        })
        .collect();
    let tags = ["A#3", "B#4", "C#5", "D#6", "E#7"].map(|t| (t.to_string(), true));
    assert_eq!(denied, tags, "each refused by name under a tag of its own");
}

#[test]
fn a_pegged_order_is_replaced_never_amended() {
    fn again(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("S", Short);
            }
            1 | 2 => {
                s.entry("C", Long).chase();
            }
            3 => {
                s.order("F", Long).size(2.0);
            }
            _ => {}
        }
    }
    let mut b = Bench::of(again);
    b.bar(0, 100.0);
    b.fill("S#1", 1.0, 100.0);
    assert_eq!(b.bar(1, 100.0), [place("C#2", 1, 2.0, CHASED)], "facing the short it adds it");
    assert_eq!(b.bar(2, 100.0), [], "the same order again sends nothing (the seam amends no peg)");
    assert_eq!(b.bar(3, 100.0), [place("F#3", 1, 2.0, MARKET)]);
    assert_eq!(
        b.fill("F#3", 2.0, 100.0),
        [cancel("C#2"), place("C#4", 1, 1.0, CHASED)],
        "now long, it asks 1: placed anew under a new tag, never amended"
    );
}

#[test]
fn live_an_exit_issued_with_a_chased_entry_waits_for_its_fill() {
    fn chased(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("E", Long).chase();
            s.exit("X").from("E").stop(95.0);
        }
    }
    let mut b = Bench::of(chased);
    b.broker.reference = false;
    assert_eq!(
        b.bar(0, 100.0),
        [place("E#1", 1, 1.0, CHASED)],
        "an order list's leg carries no peg: the entry goes alone"
    );
    assert_eq!(b.fill("E#1", 1.0, 100.0), [leg("X", "X#2", -1, 1.0, stop(95.0))]);
}

// ---- several symbols (`RuleRun::trades`, `Tactics::on`) ----

/// `call` on the symbol `Y`, the run's second.
fn on_y(call: Call) -> Call {
    Call::On("Y".to_string(), Box::new(call))
}

#[test]
fn on_moves_the_verbs_to_another_symbol_under_its_own_tags_until_the_hook_ends() {
    fn two(s: &mut Tactics<'_>, k: i64) {
        assert_eq!(s.symbol(), SYMBOL, "each hook starts on the run's own symbol");
        match k {
            0 => {
                s.order("A", Long);
                s.on("Y").order("L", Long).limit(99.0);
                s.order("M", Short);
                assert_eq!(s.symbol(), "Y", "on holds until the next on");
                s.on("").order("B", Short);
            }
            1 => {
                s.order("C", Long);
                s.on(SYMBOL).order("D", Long);
            }
            _ => {}
        }
    }
    let mut b = bench(two, |r| r.trades(&["Y"]));
    assert_eq!(
        b.bar(0, 100.0),
        [
            place("A#1", 1, 1.0, MARKET),
            on_y(place("Y/L#1", 1, 1.0, limit(99.0))),
            on_y(place("Y/M#2", -1, 1.0, MARKET)),
            place("B#2", -1, 1.0, MARKET),
        ],
        "another symbol's tags are <symbol>/<name>#<n>, numbered per symbol"
    );
    assert_eq!(
        b.bar(1, 100.0),
        [place("C#3", 1, 1.0, MARKET), place("D#4", 1, 1.0, MARKET)],
        "\"\" and the run's own symbol both name it, whose tags stay <name>#<n>"
    );
}

#[test]
fn a_fill_goes_to_its_symbols_book_and_an_end_to_the_book_of_its_tag() {
    fn guarded(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.on("Y").entry("L", Long);
                s.exit("L").stop(95.0);
                s.entry("W", Long).limit(90.0);
            }
            1 => {
                assert_eq!((s.position(), s.position_of("Y")), (0.0, 1.0));
                assert!(s.lots().is_empty(), "the run's own book holds none of Y's lots");
                let lots: Vec<_> = s.on("Y").lots().iter().map(|l| l.name.clone()).collect();
                assert_eq!((lots, s.position()), (vec!["L".to_string()], 1.0));
                s.entry("W", Long).limit(90.0);
            }
            _ => {}
        }
    }
    let two = |r: RuleRun<Script>| {
        r.trades(&["Y"]).defaults(RuleDefaults { pyramiding: 2, ..RuleDefaults::PLAIN })
    };
    let mut b = bench(guarded, two);
    assert_eq!(
        b.bar(0, 100.0),
        [
            on_y(place("Y/L#1", 1, 1.0, MARKET)),
            on_y(leg("L", "Y/L#2", -1, 1.0, stop(95.0))),
            on_y(place("Y/W#3", 1, 1.0, limit(90.0))),
        ]
    );
    assert_eq!(b.fill_on("Y", "Y/L#1", 1.0, 100.0), []);
    b.end("Y/W#3", OrderEventKind::Canceled { reason: "gone".to_string() });
    assert_eq!(
        b.bar(1, 100.0),
        [on_y(place("Y/W#4", 1, 1.0, limit(90.0)))],
        "its end took Y/W#3 off Y's book: named again, it is placed anew, not amended"
    );
    assert_eq!(b.fill_on("Y", "Y/L#2", 1.0, 95.0), [], "Y's stop fills on Y's book");
}

#[test]
fn a_run_states_its_tick_step_and_slippage_for_its_own_symbol_alone() {
    fn sizes(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            let own = s.sizing();
            let own = (own.qty_step(), own.slipped(Long, 100.0), s.tick());
            assert_eq!(own, (Some(0.5), 100.25, TICK));
            let other = s.on("Y").sizing();
            assert_eq!(
                (other.qty_step(), other.slipped(Long, 100.0), s.tick()),
                (None, 100.0, None),
                "another symbol takes the broker's grid (none here) and no slippage"
            );
        }
    }
    let mut b = bench(sizes, |r| r.trades(&["Y"]).qty_step(0.5).slippage(0.25));
    assert_eq!(b.bar(0, 100.0), []);
}
