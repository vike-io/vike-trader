use super::*;
use crate::BookLevel;

/// A book on a 0.5 grid with one accepted bid — the fixture most gate tests start from.
/// `coid` rests at `price`/`qty`, submitted at ts 10 and accepted at ts 20.
fn accepted_bid(coid: &str, price: f64, qty: f64) -> OwnOrderBook {
    let mut b = OwnOrderBook::new(0.5);
    assert!(b.on_submit(coid, OwnSide::Bid, price, qty, 10), "submit tracked");
    assert!(b.on_accepted(coid, 20), "accept stamped");
    b
}

// THE RACE: an order the venue just accepted is NOT yet in the public feed, so inside the
// accepted-buffer it must not be subtracted — otherwise filtration removes depth the feed
// never added (phantom-empty level).
#[test]
fn accepted_but_not_yet_public_is_excluded_by_the_buffer() {
    let b = accepted_bid("c1", 100.0, 3.0);
    // accepted at 20, buffer 1000 ⇒ not public until 1020
    let f = OwnQtyFilter::at(500).with_accepted_buffer(1_000);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "inside the buffer ⇒ 0");
    // one tick BEFORE the deadline is still inside
    let f = OwnQtyFilter::at(1_019).with_accepted_buffer(1_000);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "1019 < 1020 ⇒ still 0");
    // and the public ladder is therefore untouched — no phantom depth removed
    let levels = b.subtract_levels(
        OwnSide::Bid,
        &[BookLevel::new(100.0, 3.0), BookLevel::new(99.5, 8.0)],
        &f,
    );
    assert_eq!(
        levels,
        vec![BookLevel::new(100.0, 3.0), BookLevel::new(99.5, 8.0)],
        "public depth is left alone"
    );
}

// Once the buffer has elapsed the order IS assumed public and counts — and the level it wholly
// owns is dropped from the public ladder, shifting the derived best to the genuine next level.
#[test]
fn buffer_elapsed_includes_the_order_and_shifts_the_best() {
    let b = accepted_bid("c1", 100.0, 3.0);
    let f = OwnQtyFilter::at(1_020).with_accepted_buffer(1_000);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 3.0_f64.to_bits(), "at the deadline ⇒ counts");
    let later = OwnQtyFilter::at(9_999).with_accepted_buffer(1_000);
    assert_eq!(b.bid_qty_at(100.0, &later).to_bits(), 3.0_f64.to_bits(), "and stays counted");
    // whole level ours ⇒ dropped; a level we only partly own ⇒ reduced
    let levels = b.subtract_levels(
        OwnSide::Bid,
        &[BookLevel::new(100.0, 3.0), BookLevel::new(99.5, 8.0)],
        &f,
    );
    assert_eq!(levels, vec![BookLevel::new(99.5, 8.0)], "our whole level is removed");
    let levels = b.subtract_levels(OwnSide::Bid, &[BookLevel::new(100.0, 10.0)], &f);
    assert_eq!(levels, vec![BookLevel::new(100.0, 7.0)], "partly ours ⇒ only our size subtracted");
    // a zero buffer trusts the accept immediately (the pre-existing snapshot behavior)
    let now0 = OwnQtyFilter::at(20);
    assert_eq!(b.bid_qty_at(100.0, &now0).to_bits(), 3.0_f64.to_bits(), "0 buffer ⇒ at once");
}

// A never-accepted (Submitted) order is structurally invisible: it is not in the public book,
// so no mask can make it subtractable.
#[test]
fn submitted_but_unaccepted_never_counts() {
    let mut b = OwnOrderBook::new(0.5);
    b.on_submit("c1", OwnSide::Bid, 100.0, 3.0, 10);
    for f in
        [OwnQtyFilter::at(1_000_000), OwnQtyFilter::at(1_000_000).with_statuses(StatusMask::ALL)]
    {
        assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "unaccepted ⇒ 0");
    }
    assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::Submitted), "still Submitted");
    assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), None, "and unstamped");
}

// A partial fill decrements the REMAINING size (so filtration subtracts less), and a decrement
// to zero-or-below removes the order outright (a full fill is terminal).
#[test]
fn partial_fill_decrements_then_removes_when_exhausted() {
    let mut b = accepted_bid("c1", 100.0, 5.0);
    let f = OwnQtyFilter::at(100);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "starts at 5");

    assert!(b.on_partial_fill("c1", 2.0), "known coid");
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 3.0_f64.to_bits(), "5 − 2 = 3 remaining");
    assert_eq!(b.len(), 1, "still tracked");
    // the accept stamp survives a partial — the residual never left the book
    assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(20), "stamp unchanged");

    assert!(b.on_partial_fill("c1", 3.0), "the fill that exhausts it");
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "nothing left");
    assert!(b.is_empty(), "an exhausted order is removed");
    assert!(!b.contains("c1"), "and its index entry with it");
    assert!(b.levels(OwnSide::Bid, &f).is_empty(), "its level is gone too");
    // an over-fill (venue rounding) also just removes it, never leaves negative size
    let mut b2 = accepted_bid("c2", 100.0, 5.0);
    assert!(b2.on_partial_fill("c2", 7.5));
    assert!(b2.is_empty(), "an over-fill removes the order");
    assert!(!b2.on_partial_fill("unknown", 1.0), "unknown coid is a no-op");
}

// A pending cancel is still RESTING at the venue, so it counts by default; the
// ACCEPTED_ONLY mask is the opt-out for a maker that wants to lean on depth it is reclaiming.
#[test]
fn pending_cancel_counts_by_default_and_is_droppable_by_mask() {
    let mut b = accepted_bid("c1", 100.0, 4.0);
    assert!(b.on_cancel_pending("c1"), "known coid");
    assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::PendingCancel));

    let default = OwnQtyFilter::at(100);
    assert_eq!(default.statuses, StatusMask::RESTING, "RESTING is the default mask");
    assert_eq!(b.bid_qty_at(100.0, &default).to_bits(), 4.0_f64.to_bits(), "still counted");

    let accepted_only = OwnQtyFilter::at(100).with_statuses(StatusMask::ACCEPTED_ONLY);
    assert_eq!(b.bid_qty_at(100.0, &accepted_only).to_bits(), 0.0_f64.to_bits(), "masked out");
    // and it shows through to the ladder: masked out ⇒ the public level is left intact
    assert_eq!(
        b.subtract_levels(OwnSide::Bid, &[BookLevel::new(100.0, 4.0)], &accepted_only),
        vec![BookLevel::new(100.0, 4.0)],
        "masked-out size is not subtracted"
    );
    assert!(
        b.subtract_levels(OwnSide::Bid, &[BookLevel::new(100.0, 4.0)], &default).is_empty(),
        "counted size still removes the level"
    );
    // a late ack must NOT un-request the cancel
    assert!(b.on_accepted("c1", 50), "stamp refreshes");
    assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::PendingCancel), "stays pending");
    assert!(!b.on_cancel_pending("unknown"), "unknown coid is a no-op");
}

// Terminal removes the order, its index entry and (when it empties) its level — and leaves
// every OTHER order at that level untouched.
#[test]
fn terminal_removes_the_order_and_empty_levels() {
    let mut b = accepted_bid("c1", 100.0, 2.0);
    b.on_submit("c2", OwnSide::Bid, 100.0, 3.0, 11);
    b.on_accepted("c2", 21);
    b.on_submit("c3", OwnSide::Ask, 101.0, 4.0, 12);
    b.on_accepted("c3", 22);
    let f = OwnQtyFilter::at(100);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "2 + 3 at the level");

    assert!(b.on_terminal("c1"), "known coid");
    assert_eq!(b.len(), 2, "only c1 left the book");
    assert!(!b.contains("c1"), "index entry gone");
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 3.0_f64.to_bits(), "c2 still rests there");

    assert!(b.on_terminal("c2"), "the last order at the level");
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "level now empty");
    assert!(b.levels(OwnSide::Bid, &f).is_empty(), "the empty level was dropped");
    assert_eq!(b.ask_qty_at(101.0, &f).to_bits(), 4.0_f64.to_bits(), "the ask side is intact");
    assert!(!b.on_terminal("c1"), "a second terminal is a no-op");
}

// Queue order is INSERTION order and survives a removal from the MIDDLE of a level
// (`shift_remove`, never `swap_remove`) — and a re-registered coid goes to the BACK, which is
// what a venue does to a re-priced order.
#[test]
fn level_iteration_is_deterministic_insertion_order() {
    let mut b = OwnOrderBook::new(0.5);
    for (i, coid) in ["a", "b", "c", "d"].iter().enumerate() {
        b.on_submit(coid, OwnSide::Bid, 100.0, 1.0 + i as f64, 10 + i as i64);
        b.on_accepted(coid, 20 + i as i64);
    }
    let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
    assert_eq!(coids, vec!["a", "b", "c", "d"], "insertion order preserved");

    // remove from the MIDDLE: a swap-remove would move "d" into b's slot
    b.on_terminal("b");
    let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
    assert_eq!(coids, vec!["a", "c", "d"], "middle removal keeps the remaining queue order");

    // re-registering an existing coid puts it at the BACK (queue priority is forfeited)
    b.on_submit("a", OwnSide::Bid, 100.0, 9.0, 30);
    let coids: Vec<&str> = b.orders_at(OwnSide::Bid, 100.0).map(|(c, _)| c).collect();
    assert_eq!(coids, vec!["c", "d", "a"], "a re-registered order re-enters at the back");
    assert_eq!(b.len(), 3, "still three orders — re-register did not duplicate");

    // the summed qty is a deterministic insertion-order fold, repeatable across calls
    let f = OwnQtyFilter::at(1_000).with_statuses(StatusMask::ALL);
    // c(3) + d(4) count; "a" was re-submitted (unaccepted) so it is not visible
    let first = b.bid_qty_at(100.0, &f);
    assert_eq!(first.to_bits(), 7.0_f64.to_bits(), "3 + 4 in queue order");
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), first.to_bits(), "repeatable, bit-for-bit");

    // multi-LEVEL order is price-ordered, best-first, on both sides
    let mut m = OwnOrderBook::new(0.5);
    for (coid, side, px) in [
        ("b1", OwnSide::Bid, 99.5),
        ("b2", OwnSide::Bid, 100.0),
        ("a1", OwnSide::Ask, 101.0),
        ("a2", OwnSide::Ask, 100.5),
    ] {
        m.on_submit(coid, side, px, 1.0, 10);
        m.on_accepted(coid, 10);
    }
    let g = OwnQtyFilter::at(100);
    assert_eq!(m.levels(OwnSide::Bid, &g), vec![(100.0, 1.0), (99.5, 1.0)], "bids high→low");
    assert_eq!(m.levels(OwnSide::Ask, &g), vec![(100.5, 1.0), (101.0, 1.0)], "asks low→high");
}

// Prices match a level by TICK (the same round_ties_even quantization the public book uses), so
// a float that lands on the same tick still matches; and an unknown grid is fully inert.
#[test]
fn tick_matching_and_the_inert_no_grid_book() {
    let b = accepted_bid("c1", 100.0, 3.0);
    let f = OwnQtyFilter::at(100);
    // 100.0 and a float a hair off it quantize to the same tick on a 0.5 grid
    assert_eq!(b.bid_qty_at(100.0 + 1e-12, &f).to_bits(), 3.0_f64.to_bits(), "same tick");
    assert_eq!(b.bid_qty_at(99.5, &f).to_bits(), 0.0_f64.to_bits(), "a different tick ⇒ 0");
    assert_eq!(b.bid_qty_at(f64::NAN, &f).to_bits(), 0.0_f64.to_bits(), "non-finite ⇒ 0");
    assert_eq!(b.ask_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "the other side ⇒ 0");

    // no grid ⇒ nothing is tracked and nothing is ever subtracted
    let mut inert = OwnOrderBook::new(0.0);
    assert!(!inert.on_submit("c1", OwnSide::Bid, 100.0, 3.0, 10), "no grid ⇒ not tracked");
    assert!(inert.is_empty(), "inert book stays empty");
    assert!(!inert.on_accepted("c1", 20), "and its later events are no-ops");
    assert_eq!(inert.bid_qty_at(100.0, &f).to_bits(), 0.0_f64.to_bits(), "queries are 0");
    assert_eq!(
        inert.subtract_levels(OwnSide::Bid, &[BookLevel::new(100.0, 3.0)], &f),
        vec![BookLevel::new(100.0, 3.0)],
        "the public ladder is returned unchanged"
    );
    // a nonsense submit is refused without corrupting the book
    let mut b2 = OwnOrderBook::new(0.5);
    assert!(!b2.on_submit("x", OwnSide::Bid, 100.0, 0.0, 1), "zero qty refused");
    assert!(!b2.on_submit("x", OwnSide::Bid, f64::INFINITY, 1.0, 1), "non-finite px refused");
    assert!(b2.is_empty(), "nothing tracked");
}

// on_modify: a price move re-arms the race gate (back of the new level, awaiting a fresh ack);
// a qty INCREASE re-arms it too (the feed has not shown the added size); a qty REDUCTION keeps
// the stamp (subtracting less can only under-subtract, the safe direction).
#[test]
fn modify_repricing_rearms_the_buffer_and_resize_rules_hold() {
    let mut b = accepted_bid("c1", 100.0, 3.0);
    let f = |now: i64| OwnQtyFilter::at(now).with_accepted_buffer(100);
    assert_eq!(b.bid_qty_at(100.0, &f(1_000)).to_bits(), 3.0_f64.to_bits(), "resting + visible");

    // re-price to 99.5 at ts 1000: gone from the old level, and NOT yet visible at the new one
    assert!(b.on_modify("c1", 99.5, 3.0, 1_000), "known coid");
    assert_eq!(b.bid_qty_at(100.0, &f(1_000)).to_bits(), 0.0_f64.to_bits(), "old level empty");
    assert_eq!(b.bid_qty_at(99.5, &f(1_000)).to_bits(), 0.0_f64.to_bits(), "new one unacked");
    assert_eq!(b.get("c1").map(|o| o.status), Some(OwnStatus::Submitted), "awaiting re-ack");
    assert_eq!(b.len(), 1, "still exactly one order");
    // once re-acked and past the buffer it counts at the NEW price
    b.on_accepted("c1", 1_010);
    assert_eq!(b.bid_qty_at(99.5, &f(1_110)).to_bits(), 3.0_f64.to_bits(), "visible again");

    // qty INCREASE re-arms the buffer from the modify ts
    assert!(b.on_modify("c1", 99.5, 5.0, 1_200), "size up");
    assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(1_200), "stamp re-armed");
    assert_eq!(b.bid_qty_at(99.5, &f(1_250)).to_bits(), 0.0_f64.to_bits(), "inside the buffer");
    assert_eq!(b.bid_qty_at(99.5, &f(1_300)).to_bits(), 5.0_f64.to_bits(), "then the new size");
    // qty REDUCTION keeps the stamp — no re-arm, subtract the smaller size at once
    assert!(b.on_modify("c1", 99.5, 2.0, 1_400), "size down");
    assert_eq!(b.get("c1").and_then(|o| o.ts_accepted), Some(1_200), "stamp untouched");
    assert_eq!(b.bid_qty_at(99.5, &f(1_400)).to_bits(), 2.0_f64.to_bits(), "smaller size");
    assert!(!b.on_modify("unknown", 99.5, 1.0, 1_500), "unknown coid is a no-op");
}

// Two own orders at the SAME level sum — the case a single `(price, size)` snapshot cannot
// represent at all, and the reason this structure exists.
#[test]
fn multiple_own_orders_at_one_level_sum() {
    let mut b = accepted_bid("c1", 100.0, 2.0);
    b.on_submit("c2", OwnSide::Bid, 100.0, 3.0, 11);
    b.on_accepted("c2", 21);
    let f = OwnQtyFilter::at(100);
    assert_eq!(b.bid_qty_at(100.0, &f).to_bits(), 5.0_f64.to_bits(), "2 + 3 subtractable");
    // a public level of 12 with 5 ours leaves 7; with only c1 visible it would leave 10
    assert_eq!(
        b.subtract_levels(OwnSide::Bid, &[BookLevel::new(100.0, 12.0)], &f),
        vec![BookLevel::new(100.0, 7.0)],
        "BOTH own orders are subtracted, not just the last one"
    );
    // and the buffer gates them INDEPENDENTLY: at ts 20 only c1 (accepted at 20) is past a
    // zero-buffer gate; c2 (accepted at 21) is not yet accepted at all
    let early = OwnQtyFilter::at(20);
    assert_eq!(b.bid_qty_at(100.0, &early).to_bits(), 2.0_f64.to_bits(), "only c1 yet");
}
