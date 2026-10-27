use super::*;

fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        ts: 0,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn stop_fires_on_cross_and_leaves_book() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("a1", -1, 2.0, 95.0, None)); // protect a long: sell-stop below
    assert!(book.check_bar(&bar(100.0, 101.0, 99.0, 100.5)).is_empty());
    assert_eq!(book.len(), 1);
    let fired = book.check_bar(&bar(97.0, 98.0, 94.0, 94.5));
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].side, -1);
    assert_eq!(fired[0].qty, 2.0);
    assert!(book.is_empty());
}

#[test]
fn gap_open_fills_adverse() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    // gaps DOWN through the stop: fill at the (worse) open, not the trigger
    let fired = book.check_bar(&bar(92.0, 93.0, 91.0, 92.5));
    assert_eq!(fired[0].trigger_px, 92.0);
}

#[test]
fn trailing_ratchets_in_place_then_fires() {
    let mut book = ConditionalBook::new();
    assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None)); // long protection, extreme 100 -> trigger 95
    // new high 110 ratchets the extreme; low 101 stays above the OLD trigger 95
    assert!(book.check_bar(&bar(105.0, 110.0, 101.0, 108.0)).is_empty());
    // trigger is now 105: a dip to 104 fires
    let fired = book.check_bar(&bar(106.0, 107.0, 104.0, 104.5));
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].trigger_px, 105.0);
}

#[test]
fn new_high_bar_cannot_stop_itself_out() {
    // the oracle checks the PRIOR extreme's trigger before ratcheting (orders.rs law)
    let mut book = ConditionalBook::new();
    assert!(book.add_trailing("a1", -1, 1.0, 5.0, 100.0, None));
    // this bar's high 120 would imply trigger 115 — but its own low 103 must be
    // compared against the PRIOR trigger 95: no fire
    assert!(book.check_bar(&bar(110.0, 120.0, 103.0, 118.0)).is_empty());
}

#[test]
fn disarm_removes_one_arm_by_id_and_tolerates_unknown() {
    let mut book = ConditionalBook::new();
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
    let mut book = ConditionalBook::new();
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
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("first", -1, 1.0, 95.0, None));
    assert!(book.add_stop("second", -1, 1.0, 96.0, None));
    assert!(book.add_stop("third", -1, 1.0, 97.0, None));
    assert!(book.disarm("second"));
    // one bar crosses ALL survivors: they fire in insertion order (first, third)
    let fired = book.check_bar(&bar(100.0, 100.0, 90.0, 92.0));
    let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
    assert_eq!(ids, vec!["first", "third"]);
}

#[test]
fn fired_conditional_carries_its_arm_id() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("armed-7", -1, 1.0, 95.0, None));
    let fired = book.check_price(94.0, 1);
    assert_eq!(fired[0].arm_id, "armed-7");
}

#[test]
fn tick_check_uses_degenerate_bar() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("a1", -1, 1.0, 95.0, None));
    assert!(book.check_price(96.0, 1).is_empty());
    let fired = book.check_price(94.0, 2);
    assert_eq!(fired.len(), 1);
}

#[test]
fn clear_empties_the_book() {
    let mut book = ConditionalBook::new();
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
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("l1", -1, 1.0, 95.0, Some(TriggerBy::Last)));
    assert!(book.add_stop("d1", -1, 1.0, 95.0, None)); // None = today's Last law
    assert!(book.has_mark_arms());

    // a crossing LAST price (bar and tick) fires l1+d1 but leaves m1 resting
    let fired = book.check_bar(&bar(94.0, 94.0, 94.0, 94.0));
    let ids: Vec<&str> = fired.iter().map(|f| f.arm_id.as_str()).collect();
    assert_eq!(ids, vec!["l1", "d1"], "insertion order, Mark arm skipped");
    assert!(book.contains("m1"), "the Mark arm rests through a crossing LAST price");
    assert!(book.has_mark_arms());

    // a crossing MARK price fires m1 (last=99 mark=94 SL=95: the HL-timing scenario)
    let fired = book.check_mark(94.0, 2);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].arm_id, "m1");
    assert_eq!(fired[0].trigger_px, 94.0);
    assert!(book.is_empty());
    assert!(!book.has_mark_arms(), "the fire decremented the mark-arm count");
}

/// The reverse guard: a crossing mark price never fires a Last/None arm.
#[test]
fn last_arms_never_fire_on_the_mark_lane() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("l1", -1, 1.0, 95.0, None));
    assert!(!book.has_mark_arms());
    assert!(book.check_mark(90.0, 1).is_empty(), "mark lane skips Last arms");
    assert!(book.contains("l1"));
    assert_eq!(book.check_price(94.0, 2).len(), 1, "…the Last lane still fires it");
}

/// A Mark TRAILING arm ratchets its extreme on mark prices ONLY: a higher Last print must
/// neither ratchet nor fire it.
#[test]
fn mark_trailing_ratchets_only_on_the_mark_lane() {
    let mut book = ConditionalBook::new();
    assert!(book.add_trailing("mt", -1, 1.0, 5.0, 100.0, Some(TriggerBy::Mark)));
    // a LAST spike to 120 would ratchet the trigger to 115 — but this arm ignores Last
    assert!(book.check_bar(&bar(110.0, 120.0, 103.0, 118.0)).is_empty());
    // mark ratchets to 110 (trigger 105) without firing…
    assert!(book.check_mark(110.0, 1).is_empty());
    // …then a mark touch of exactly 105 fires at the RATCHETED trigger 105 — proof the mark
    // spike moved the extreme (an UN-ratcheted trigger of 95 would not fire on a 105 touch),
    // and the Last spike to 120 never did (a 115 trigger would have fired at 110 already)
    let fired = book.check_mark(105.0, 2);
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].trigger_px, 105.0, "extreme ratcheted by mark, not by last");
}

/// An Index arm matches NO lane (the core has no index lane): inert on both, never a
/// misfire off the wrong series. The runtime refuses such arms at apply time; this is the
/// structural backstop.
#[test]
fn index_arm_is_inert_on_both_lanes() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("i1", -1, 1.0, 95.0, Some(TriggerBy::Index)));
    assert!(!book.has_mark_arms());
    assert!(book.check_bar(&bar(90.0, 90.0, 90.0, 90.0)).is_empty());
    assert!(book.check_price(90.0, 1).is_empty());
    assert!(book.check_mark(90.0, 2).is_empty());
    assert!(book.contains("i1"), "rests inert — disarmable, never misfired");
}

/// The mark-arm count survives disarms and clear (the runtime's fast-path guard must never
/// go stale in either direction).
#[test]
fn mark_arm_count_tracks_disarm_and_clear() {
    let mut book = ConditionalBook::new();
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

/// `iter` exposes each arm's trigger source (the Snap capture surface carries it through a
/// restart).
#[test]
fn iter_carries_the_trigger_source() {
    let mut book = ConditionalBook::new();
    assert!(book.add_stop("m1", -1, 1.0, 95.0, Some(TriggerBy::Mark)));
    assert!(book.add_stop("l1", -1, 1.0, 90.0, None));
    let got: Vec<(&str, Option<TriggerBy>)> =
        book.iter().map(|(id, a)| (id, a.trigger_by)).collect();
    assert_eq!(got, vec![("m1", Some(TriggerBy::Mark)), ("l1", None)]);
}
