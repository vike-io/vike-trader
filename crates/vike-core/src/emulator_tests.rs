use super::*;

use vike_marketdata::test_support::shape_bar;

#[test]
fn stop_fires_on_cross_and_leaves_book() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", -1, 2.0, 95.0, None)); // protect a long: sell-stop below
    assert!(book.check_bar(&shape_bar(100.0, 101.0, 99.0, 100.5), None).is_empty());
    assert_eq!(book.len(), 1);
    let fired = book.check_bar(&shape_bar(97.0, 98.0, 94.0, 94.5), None);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].side, -1);
    assert_eq!(fired[0].qty, 2.0);
    assert!(book.is_empty());
}

#[test]
fn gap_open_fills_adverse() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    // gaps DOWN through the stop: fill at the (worse) open, not the trigger
    let fired = book.check_bar(&shape_bar(92.0, 93.0, 91.0, 92.5), None);
    assert_eq!(fired[0].trigger_price, 92.0);
}

#[test]
fn trailing_ratchets_in_place_then_fires() {
    let mut book = ConditionalArms::new();
    assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None)); // long protection, extreme 100 -> trigger 95
    // new high 110 ratchets the extreme; low 101 stays above the OLD trigger 95
    assert!(book.check_bar(&shape_bar(105.0, 110.0, 101.0, 108.0), None).is_empty());
    // trigger is now 105: a dip to 104 fires
    let fired = book.check_bar(&shape_bar(106.0, 107.0, 104.0, 104.5), None);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].trigger_price, 105.0);
}

#[test]
fn new_high_bar_cannot_stop_itself_out() {
    // the oracle checks the PRIOR extreme's trigger before ratcheting (orders.rs law)
    let mut book = ConditionalArms::new();
    assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None));
    // this bar's high 120 would imply trigger 115 — but its own low 103 must be
    // compared against the PRIOR trigger 95: no fire
    assert!(book.check_bar(&shape_bar(110.0, 120.0, 103.0, 118.0), None).is_empty());
}

#[test]
fn disarm_removes_one_arm_by_id_and_tolerates_unknown() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    assert!(book.add_stop("a2", -1, 1.0, 90.0, None));
    assert!(book.contains("a1") && book.contains("a2"));
    assert!(!book.disarm("nope"), "unknown id is a no-op, not a panic");
    assert_eq!(book.len(), 2);
    assert!(book.disarm("a1"));
    assert!(!book.contains("a1"));
    assert_eq!(book.len(), 1);
    // the survivor is a2: only the 90 stop is left, so a dip to 94 no longer fires
    assert!(book.check_price(94.0, 1).is_empty());
    let fired = book.check_price(89.0, 2);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].arm_id, "a2");
}

/// Emulator PR-2 (structural uniqueness): a duplicate arm id is REFUSED at add — the resting
/// arm keeps its terms, the book length is unchanged, and `disarm` therefore targets exactly
/// one arm by construction (the pre-PR-2 `Vec` book blindly pushed both and `disarm`
/// retain-dropped every match).
#[test]
fn duplicate_arm_id_is_refused_and_never_replaces_the_resting_arm() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    assert!(!book.add_stop("a1", -1, 3.0, 80.0, None), "same id again: refused");
    assert!(!book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None), "refused across kinds too");
    assert_eq!(book.len(), 1, "the book still holds exactly the FIRST arm");
    // the resting arm's ORIGINAL terms survive: 94 crosses the 95 stop (qty 1), which the
    // refused 80-stop replacement would not have
    let fired = book.check_price(94.0, 1);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].qty, 1.0, "the original arm's terms, not the duplicate's");
    // and the fire consumed THE one arm — nothing lingers under the refused duplicate id
    assert!(book.is_empty());
    assert!(!book.disarm("a1"), "no ghost arm left behind for the id");
}

/// Fire order across a multi-arm book is the book's INSERTION order, and a disarm in the
/// middle does not reorder the survivors (`shift_remove`, never `swap_remove`).
#[test]
fn fire_order_is_insertion_order_and_survives_a_disarm() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("first", -1, 1.0, 95.0, None));
    assert!(book.add_stop("second", -1, 1.0, 96.0, None));
    assert!(book.add_stop("third", -1, 1.0, 97.0, None));
    assert!(book.disarm("second"));
    // one bar crosses ALL survivors: they fire in insertion order (first, third)
    let fired = book.check_bar(&shape_bar(100.0, 100.0, 90.0, 92.0), None);
    let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
    assert_eq!(ids, vec!["first", "third"]);
}

#[test]
fn fired_conditional_carries_its_arm_id() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("armed-7", -1, 1.0, 95.0, None));
    let fired = book.check_price(94.0, 1);
    assert_eq!(fired[0].arm_id, "armed-7");
}

#[test]
fn tick_check_uses_degenerate_bar() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    assert!(book.check_price(96.0, 1).is_empty());
    let fired = book.check_price(94.0, 2);
    assert_eq!(fired.len(), 1);
}

#[test]
fn clear_empties_the_book() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("a1", 1, 1.0, 100.0, None));
    assert!(book.add_trailing("a2", -1, 1.0, 5.0, 100.0, None));
    book.clear();
    assert!(book.is_empty());
}

// ---- trigger-source lanes (w2 trigger_by) ----

/// A Mark arm never fires off the Last lane (bars/ticks) — its price series simply did not
/// tick there — and DOES fire off the mark lane; a Last (and a None) arm is the mirror image.
#[test]
fn mark_arm_fires_only_on_the_mark_lane_and_last_only_on_last() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("l1", -1, 1.0, 95.0, Some(TriggerBy::Last)));
    assert!(book.add_stop("d1", -1, 1.0, 95.0, None)); // None = today's Last law
    assert!(book.has_mark_arms());

    // a crossing LAST price (bar and tick) fires l1+d1 but leaves m1 resting
    let fired = book.check_bar(&shape_bar(94.0, 94.0, 94.0, 94.0), None);
    let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
    assert_eq!(ids, vec!["l1", "d1"], "insertion order, Mark arm skipped");
    assert!(book.contains("m1"), "the Mark arm rests through a crossing LAST price");
    assert!(book.has_mark_arms());

    // a crossing MARK price fires m1 (last=99 mark=94 SL=95: the HL-timing scenario)
    let fired = book.check_mark(94.0, 94.0, 2);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].arm_id, "m1");
    assert_eq!(fired[0].trigger_price, 94.0);
    assert!(book.is_empty());
    assert!(!book.has_mark_arms(), "the fire decremented the mark-arm count");
}

/// The reverse guard: a crossing mark price never fires a Last/None arm.
#[test]
fn last_arms_never_fire_on_the_mark_lane() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("l1", -1, 1.0, 95.0, None));
    assert!(!book.has_mark_arms());
    assert!(book.check_mark(90.0, 90.0, 1).is_empty(), "mark lane skips Last arms");
    assert!(book.contains("l1"));
    assert_eq!(book.check_price(94.0, 2).len(), 1, "…the Last lane still fires it");
}

/// A Mark TRAILING arm ratchets its extreme on mark prices ONLY: a higher Last print must
/// neither ratchet nor fire it.
#[test]
fn mark_trailing_ratchets_only_on_the_mark_lane() {
    let mut book = ConditionalArms::new();
    assert!(book.add_trailing("mt", -1, 1.0, 5.0, 100.0, Some(TriggerBy::Mark)));
    // a LAST spike to 120 would ratchet the trigger to 115 — but this arm ignores Last
    assert!(book.check_bar(&shape_bar(110.0, 120.0, 103.0, 118.0), None).is_empty());
    // mark ratchets to 110 (trigger 105) without firing…
    assert!(book.check_mark(110.0, 110.0, 1).is_empty());
    // …then a mark touch of exactly 105 fires at the RATCHETED trigger 105 — proof the mark
    // spike moved the extreme (an UN-ratcheted trigger of 95 would not fire on a 105 touch),
    // and the Last spike to 120 never did (a 115 trigger would have fired at 110 already)
    let fired = book.check_mark(105.0, 105.0, 2);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].trigger_price, 105.0, "extreme ratcheted by mark, not by last");
}

/// An Index arm matches NO lane (the core has no index lane): inert on both, never a
/// misfire off the wrong series. The runtime refuses such arms at apply time; this is the
/// structural backstop.
#[test]
fn index_arm_is_inert_on_both_lanes() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("i1", -1, 1.0, 95.0, Some(TriggerBy::Index)));
    assert!(!book.has_mark_arms());
    assert!(book.check_bar(&shape_bar(90.0, 90.0, 90.0, 90.0), None).is_empty());
    assert!(book.check_price(90.0, 1).is_empty());
    assert!(book.check_mark(90.0, 90.0, 2).is_empty());
    assert!(book.contains("i1"), "rests inert — disarmable, never misfired");
}

/// **A stop triggers on ITS side of the quote, never on the mid** (owner, 2026-11-15): a SELL stop on the
/// bid, a BUY stop on the ask, on the tick lane and on the mark lane alike. The owner's example: long, a sell
/// stop at 99 950, bid 99 950 / ask 99 960 (mid 99 955) fires it; a dead side evaluates none of its arms.
#[test]
fn a_sell_stop_triggers_on_the_bid_and_a_buy_stop_on_the_ask() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("sell", -1, 1.0, 99_950.0, None));
    assert!(book.add_stop("buy", 1, 1.0, 100_050.0, None));
    assert!(book.check_quote(99_951.0, 99_961.0, 1).is_empty(), "the bid is above the stop");
    let fired = book.check_quote(99_950.0, 99_960.0, 2);
    assert_eq!(fired.len(), 1, "the BID reached 99 950 while the mid is 99 955");
    assert_eq!((fired[0].arm_id.as_str(), fired[0].trigger_price), ("sell", 99_950.0));
    assert!(book.check_quote(f64::NAN, 100_049.0, 3).is_empty(), "the ask is below the buy stop");
    let fired = book.check_quote(0.0, 100_050.0, 4);
    assert_eq!(fired.len(), 1, "the ASK reached the buy stop; a dead bid is no price");
    assert_eq!(fired[0].arm_id, "buy");

    let mut marks = ConditionalArms::new();
    assert!(marks.add_stop("m", -1, 1.0, 99_950.0, Some(TriggerBy::Mark)));
    assert!(marks.check_mark(99_951.0, 99_959.0, 1).is_empty());
    assert_eq!(marks.check_mark(99_950.0, 99_960.0, 2).len(), 1, "the mark lane is side-aware too");
}

/// The bar of a 1-pip EURUSD window, its mid path, and its sides (the bid's and the ask's paths, half a pip either
/// side): the mid's low is 1.08485 and high 1.08515, the bid's low 1.08480, the ask's high 1.08520.
fn eurusd_bar_and_sides() -> (vike_marketdata::Bar, vike_marketdata::BarSides) {
    let mid = shape_bar(1.08500, 1.08515, 1.08485, 1.08500);
    let sides = vike_marketdata::BarSides {
        bid: shape_bar(1.08495, 1.08510, 1.08480, 1.08495),
        ask: shape_bar(1.08505, 1.08520, 1.08490, 1.08505),
    };
    (mid, sides)
}

/// **A closed bar judges a SELL stop on the bid's path** (REVIEW-FIX finding 7; owner, 2026-11-15/16): the cTrader
/// example, a sell stop at 1.08480. The bid touched 1.08480 inside the bar while the mid's low stayed at 1.08485:
/// with the sides it fires at this close (at the stop, the bid's open being above it); judged on the mid bar alone (a
/// bar with no sides, today's law) it does not.
#[test]
fn a_closed_bar_judges_a_sell_stop_on_its_bid_low() {
    let (mid, sides) = eurusd_bar_and_sides();
    let mut no_sides = ConditionalArms::new();
    assert!(no_sides.add_stop("sell", -1, 1.0, 1.08480, None));
    assert!(no_sides.check_bar(&mid, None).is_empty(), "the mid's low 1.08485 is above the stop");

    let mut book = ConditionalArms::new();
    assert!(book.add_stop("sell", -1, 1.0, 1.08480, None));
    let fired = book.check_bar(&mid, Some(&sides));
    assert_eq!(fired.len(), 1, "the bid's low reached 1.08480");
    assert_eq!(fired[0].trigger_price, 1.08480);
}

/// **A closed bar judges a BUY stop on the ask's path**: a buy stop at 1.08520 fires when the ask's high reaches it,
/// though the mid's high (1.08515) does not; with no sides it waits, as before.
#[test]
fn a_closed_bar_judges_a_buy_stop_on_its_ask_high() {
    let (mid, sides) = eurusd_bar_and_sides();
    let mut no_sides = ConditionalArms::new();
    assert!(no_sides.add_stop("buy", 1, 1.0, 1.08520, None));
    assert!(no_sides.check_bar(&mid, None).is_empty(), "the mid's high 1.08515 is below the stop");

    let mut book = ConditionalArms::new();
    assert!(book.add_stop("buy", 1, 1.0, 1.08520, None));
    let fired = book.check_bar(&mid, Some(&sides));
    assert_eq!(fired.len(), 1, "the ask's high reached 1.08520");
    assert_eq!(fired[0].trigger_price, 1.08520);
}

/// The mark-arm count survives disarms and clear (the runtime's fast-path guard must never
/// go stale in either direction).
#[test]
fn mark_arm_count_tracks_disarm_and_clear() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("m2", -1, 1.0, 90.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("l1", -1, 1.0, 85.0, None));
    assert!(book.has_mark_arms());
    assert!(book.disarm("m1"));
    assert!(book.has_mark_arms(), "one Mark arm still rests");
    assert!(book.disarm("m2"));
    assert!(!book.has_mark_arms(), "no Mark arms left — the guard must clear");
    assert!(book.add_stop("m3", -1, 1.0, 80.0, Some(TriggerBy::Mark)));
    book.clear();
    assert!(!book.has_mark_arms());
    assert!(book.is_empty());
}

// ---- the three kinds that release a LIMIT, and the trailing activation (wave 3) ----

/// An arm of `kind` from its terms, on the Last lane.
fn arm(kind: OrderKind, side: i32, set: impl FnOnce(&mut OrderResting)) -> OrderEmulationArm {
    let mut o = OrderResting::new(kind, side, 1.0);
    set(&mut o);
    OrderEmulationArm::of(o, None).expect("holdable terms")
}

/// **A stop-limit fires a LIMIT at its limit, whether or not the price reached it.** A sell T=95 L=94:
/// a tick at 96 is nothing; 94.5 crosses T and reaches L (the oracle's price 94.5, the limit 94); a gap to
/// 93 crosses T and passes L (the oracle rests it as a `Limit`, the book fires it all the same, at the
/// tick). Both fires carry the arm's own terms, the trigger and the limit, untouched; the closed bar
/// fires it too.
#[test]
fn a_stop_limit_fires_a_limit_at_its_limit_with_its_terms() {
    let stop_limit = || {
        arm(OrderKind::StopLimit, -1, |o| {
            o.price = Some(95.0);
            o.limit = Some(94.0);
        })
    };
    for (px, trigger_px) in [(94.5, 94.5), (93.0, 93.0)] {
        let mut book = ConditionalArms::new();
        assert!(book.add("s1", stop_limit()));
        assert!(book.check_price(96.0, 1).is_empty());
        let [f] = book.check_price(px, 2).try_into().expect("one fire");
        assert_eq!((f.trigger_price, f.limit), (trigger_px, Some(94.0)), "at {px}");
        assert_eq!(f.armed, stop_limit(), "the arm as it rested, not the oracle's Limit");
        assert!(book.is_empty());
    }
    let mut book = ConditionalArms::new();
    assert!(book.add("s1", stop_limit()));
    let [f] =
        book.check_bar(&shape_bar(97.0, 98.0, 93.0, 93.5), None).try_into().expect("one fire");
    assert_eq!((f.trigger_price, f.limit), (95.0, Some(94.0)));
}

/// **An if-touched take-profit fires from the favourable side**: a sell at 105 with no limit is a MARKET
/// (market-if-touched), with one a LIMIT (limit-if-touched); 104 touches neither.
#[test]
fn a_take_profit_fires_a_market_without_a_limit_and_a_limit_with_one() {
    for limit in [None, Some(104.5)] {
        let mut book = ConditionalArms::new();
        assert!(book.add(
            "t1",
            arm(OrderKind::TakeProfit, -1, |o| {
                o.price = Some(105.0);
                o.limit = limit;
            })
        ));
        assert!(book.check_price(104.0, 1).is_empty(), "{limit:?}");
        let [f] = book.check_price(105.5, 2).try_into().expect("one fire");
        assert_eq!(f.limit, limit, "{limit:?}");
    }
}

/// **A trailing-limit ratchets like a trailing stop and fires a LIMIT its offset past the stop level.** A
/// sell, trail 5 from 100, offset 1: 110 ratchets the stop to 105; 104 fires it at 105 with the limit 104.
/// The fire carries the ratcheted extreme and no limit of its own (the limit is computed at the fire).
#[test]
fn a_trailing_limit_fires_a_limit_its_offset_past_the_stop_level() {
    let mut book = ConditionalArms::new();
    assert!(book.add(
        "tl",
        arm(OrderKind::TrailingLimit, -1, |o| {
            o.trail = Some(5.0);
            o.extreme = Some(100.0);
            o.limit_offset = Some(1.0);
        })
    ));
    assert!(book.check_price(110.0, 1).is_empty());
    let [f] = book.check_price(104.0, 2).try_into().expect("one fire");
    assert_eq!((f.trigger_price, f.limit), (104.0, Some(104.0)));
    assert_eq!((f.armed.order.extreme, f.armed.order.limit), (Some(110.0), None));
    assert_eq!(f.armed.trigger_level(), 105.0);
}

/// **A trailing arm with an activation level trails nothing until the price reaches it.** A sell, trail 5,
/// activation 110: 100 and 94 fire nothing and seed nothing (no stop exists yet); 111 activates it (the
/// extreme is 111, the stop 106) and fires nothing; 106 fires a MARKET.
#[test]
fn a_trailing_arm_waits_for_its_activation_level() {
    let mut book = ConditionalArms::new();
    let waiting = arm(OrderKind::Trailing, -1, |o| {
        o.trail = Some(5.0);
        o.activation = Some(110.0);
    });
    assert_eq!(waiting.trigger_level(), 110.0, "the level it waits for");
    assert!(book.add("ta", waiting));
    assert!(book.check_price(100.0, 1).is_empty());
    assert!(book.check_price(94.0, 2).is_empty(), "no stop before the activation");
    assert_eq!(book.iter().next().map(|(_, a)| a.order.extreme), Some(None));
    assert!(book.check_price(111.0, 3).is_empty(), "the activating tick fires nothing");
    assert_eq!(book.iter().next().map(|(_, a)| a.trigger_level()), Some(106.0));
    let [f] = book.check_price(106.0, 4).try_into().expect("one fire");
    assert_eq!((f.trigger_price, f.limit), (106.0, None));
}

/// **A re-arm keeps an UNREACHED activation** (the arm waits for its level again, never trailing early) and
/// drops a reached one (an arm with an extreme trails again from the mark, which the arm seeds); every other
/// term, the new size aside, is the arm's own.
#[test]
fn a_rearm_keeps_an_unreached_activation_and_trails_from_the_mark_after_a_reached_one() {
    let waiting = arm(OrderKind::TrailingLimit, -1, |o| {
        o.trail = Some(5.0);
        o.activation = Some(110.0);
        o.limit_offset = Some(1.0);
    });
    let again = waiting.rearmed(0.5);
    assert_eq!((again.order.activation, again.order.extreme), (Some(110.0), None));
    assert_eq!(
        (again.order.size, again.order.trail, again.order.limit_offset),
        (0.5, Some(5.0), Some(1.0))
    );

    let mut reached = waiting.clone();
    reached.order.extreme = Some(111.0);
    let again = reached.rearmed(0.5);
    assert_eq!((again.order.activation, again.order.extreme), (None, None), "trails from the mark");
    assert_eq!(again.order.limit_offset, Some(1.0));

    let stop_limit = arm(OrderKind::StopLimit, -1, |o| {
        o.price = Some(95.0);
        o.limit = Some(94.0);
    });
    let again = stop_limit.rearmed(0.5);
    assert_eq!(
        (again.order.price, again.order.limit, again.order.size),
        (Some(95.0), Some(94.0), 0.5)
    );
}

/// **The book holds nothing the oracle could not judge**: a stop-limit with no limit, a trailing-limit with
/// no offset, a trailing stop with neither an extreme nor an activation, a plain limit. Each is refused at
/// `of` and at `add`, and the book stays empty (the fold never meets an `expect`).
#[test]
fn terms_the_oracle_cannot_judge_are_never_armed() {
    let bad = [
        (OrderKind::StopLimit, Some(95.0), None, None),
        (OrderKind::TrailingLimit, None, Some(5.0), Some(100.0)),
        (OrderKind::Trailing, None, Some(5.0), None),
        (OrderKind::Limit, Some(95.0), None, None),
    ];
    let mut book = ConditionalArms::new();
    for (kind, price, trail, extreme) in bad {
        let mut o = OrderResting::new(kind, -1, 1.0);
        o.price = price;
        o.trail = trail;
        o.extreme = extreme;
        assert!(OrderEmulationArm::of(o.clone(), None).is_none(), "{kind:?}");
        assert!(!book.add("x", OrderEmulationArm { order: o, trigger_by: None }), "{kind:?}");
    }
    assert!(book.is_empty());
}

/// **The journal round trip.** Every kind the book holds comes back from its own record as the same arm: a
/// stop and a trailing stop through the legacy fields (no `kind_terms`, the bytes they always had), the
/// others through `kind_terms` (legacy fields `None`, so an older build skips them rather than arming a
/// plain stop); and the intent that re-arms each states it the same way.
#[test]
fn every_arm_round_trips_through_its_record_and_its_intent() {
    let arms = [
        arm(OrderKind::Stop, -1, |o| o.price = Some(95.0)),
        arm(OrderKind::Trailing, -1, |o| {
            o.trail = Some(5.0);
            o.extreme = Some(100.0);
        }),
        arm(OrderKind::Trailing, -1, |o| {
            o.trail = Some(5.0);
            o.activation = Some(110.0);
        }),
        arm(OrderKind::StopLimit, -1, |o| {
            o.price = Some(95.0);
            o.limit = Some(94.0);
        }),
        arm(OrderKind::TakeProfit, -1, |o| o.price = Some(105.0)),
        arm(OrderKind::TrailingLimit, 1, |o| {
            o.trail = Some(5.0);
            o.extreme = Some(90.0);
            o.limit_offset = Some(0.5);
        }),
    ];
    for (i, a) in arms.iter().enumerate() {
        let r = vike_journal::OrderEmulationRecord::of_arm(
            "v".into(),
            "s".into(),
            &a.order,
            None,
            true,
        );
        assert_eq!(r.kind_terms.is_none(), i < 2, "{i}: only today's two are legacy");
        if r.kind_terms.is_some() {
            assert_eq!((r.price, r.trail, r.extreme), (None, None, None), "{i}");
        }
        assert_eq!(OrderEmulationArm::from_record(&r).as_ref(), Some(a), "{i}: the record");
        let c = a.intent("v".into(), "s".into());
        assert_eq!(c.kind_terms.is_none(), i < 2, "{i}: the intent");
    }
}

/// `iter` exposes each arm's trigger source (the Snapshot capture surface carries it through a
/// restart).
#[test]
fn iter_carries_the_trigger_source() {
    let mut book = ConditionalArms::new();
    assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("l1", -1, 1.0, 90.0, None));
    let got: Vec<(&str, Option<TriggerBy>)> =
        book.iter().map(|(id, a)| (id, a.trigger_by)).collect();
    assert_eq!(got, vec![("m1", Some(TriggerBy::Mark)), ("l1", None)]);
}
