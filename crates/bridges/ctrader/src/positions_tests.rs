use super::*;

fn pos(symbol_id: i64, side: i32, volume: i64, open_ts: i64) -> TrackedPosition {
    TrackedPosition { symbol_id, side, volume, open_ts }
}

#[test]
fn no_opposing_position_opens() {
    // Flat book.
    assert_eq!(plan_reduce(-1, 100_000, false, &[]), ClosePlan::Open);
    // Same-side add (a BUY while already long): opens.
    let open = vec![(1, pos(1, 1, 100_000, 10))];
    assert_eq!(plan_reduce(1, 100_000, false, &open), ClosePlan::Open);
}

#[test]
fn full_close_of_single_position() {
    let open = vec![(7, pos(1, 1, 100_000, 10))];
    assert_eq!(plan_reduce(-1, 100_000, false, &open), ClosePlan::Close(vec![(7, 100_000)]));
}

#[test]
fn partial_close_of_single_position() {
    let open = vec![(7, pos(1, 1, 100_000, 10))];
    // Reduce 40_000 of a 100_000 long → one close leg for 40_000 (order fills fully for 40k).
    assert_eq!(plan_reduce(-1, 40_000, false, &open), ClosePlan::Close(vec![(7, 40_000)]));
}

#[test]
fn fifo_close_across_two_same_symbol_positions_oldest_first() {
    // Two long positions: P2 is NEWER (open_ts 20), P1 OLDER (open_ts 10). Present out of order.
    let open = vec![(2, pos(1, 1, 40_000, 20)), (1, pos(1, 1, 60_000, 10))];
    // Flatten 100_000: close the OLDER (P1, 60k) first, then the newer (P2, 40k).
    assert_eq!(
        plan_reduce(-1, 100_000, false, &open),
        ClosePlan::Close(vec![(1, 60_000), (2, 40_000)])
    );
}

#[test]
fn fifo_partial_spanning_two_positions() {
    let open = vec![(1, pos(1, 1, 60_000, 10)), (2, pos(1, 1, 40_000, 20))];
    // Reduce 80_000: close P1 fully (60k) then 20k of P2.
    assert_eq!(
        plan_reduce(-1, 80_000, false, &open),
        ClosePlan::Close(vec![(1, 60_000), (2, 20_000)])
    );
}

#[test]
fn plain_over_reduce_is_a_flip_and_opens() {
    // Long 100_000, plain SELL 150_000 (a flip past flat) → normal new-order path (unchanged).
    let open = vec![(1, pos(1, 1, 100_000, 10))];
    assert_eq!(plan_reduce(-1, 150_000, false, &open), ClosePlan::Open);
}

#[test]
fn reduce_only_over_reduce_caps_at_available() {
    // reduce_only SELL 150_000 vs long 100_000 → close all 100_000, drop the excess.
    let open = vec![(1, pos(1, 1, 100_000, 10))];
    assert_eq!(plan_reduce(-1, 150_000, true, &open), ClosePlan::Close(vec![(1, 100_000)]));
}

#[test]
fn buy_closes_short_positions_only() {
    // Mixed hedging book: a short P1 and a long P2. A BUY may only close the SHORT.
    let open = vec![(1, pos(1, -1, 50_000, 10)), (2, pos(1, 1, 30_000, 20))];
    assert_eq!(plan_reduce(1, 50_000, false, &open), ClosePlan::Close(vec![(1, 50_000)]));
}

#[test]
fn snap_close_volume_full_and_partial() {
    assert_eq!(snap_close_volume(100_000, 100_000, 100_000), 100_000, "full close = exact");
    assert_eq!(snap_close_volume(120_000, 100_000, 100_000), 100_000, "capped at position");
    assert_eq!(snap_close_volume(40_000, 100_000, 100_000), 100_000, "40k rounds to 1 step=100k");
    assert_eq!(snap_close_volume(40_000, 500_000, 20_000), 40_000, "on-grid partial unchanged");
    assert_eq!(snap_close_volume(0, 100_000, 100_000), 100_000, "never drops a positive request");
    assert_eq!(snap_close_volume(33_333, 500_000, 0), 33_333, "unknown grid passes through");
}

#[test]
fn close_tracker_single_leg_accept_then_final() {
    let mut t = CloseTracker::default();
    t.register("c1", &[(7, 100_000)]);
    assert_eq!(t.coid_for(7), Some("c1"));
    // The one fill both accepts (first event) and finalizes.
    assert_eq!(t.on_event("c1", 100_000), CloseEmit::Final { accept_first: true });
    assert_eq!(t.coid_for(7), None, "final prunes the position entry");
}

#[test]
fn close_tracker_accept_event_then_fill() {
    let mut t = CloseTracker::default();
    t.register("c1", &[(7, 100_000)]);
    assert_eq!(t.on_event("c1", 0), CloseEmit::AcceptOnly);
    assert_eq!(t.on_event("c1", 100_000), CloseEmit::Final { accept_first: false });
}

#[test]
fn close_tracker_two_legs_partial_then_final() {
    let mut t = CloseTracker::default();
    t.register("c1", &[(1, 60_000), (2, 40_000)]);
    assert_eq!(t.on_event("c1", 60_000), CloseEmit::Partial { accept_first: true });
    assert_eq!(t.on_event("c1", 40_000), CloseEmit::Final { accept_first: false });
    assert_eq!(t.coid_for(1), None);
    assert_eq!(t.coid_for(2), None);
}

#[test]
fn is_closing_and_is_tracked_reflect_registration() {
    let mut t = CloseTracker::default();
    assert!(!t.is_closing(7));
    assert!(!t.is_tracked("c1"));
    t.register("c1", &[(7, 100_000)]);
    assert!(t.is_closing(7), "position is now in flight");
    assert!(t.is_tracked("c1"));
    t.forget("c1");
    assert!(!t.is_closing(7), "forget frees the position");
    assert!(!t.is_tracked("c1"));
}

#[test]
fn resolve_failure_with_no_progress_is_a_clean_reject() {
    let mut t = CloseTracker::default();
    t.register("c1", &[(7, 100_000)]);
    assert_eq!(t.resolve_failure("c1"), CloseFailure::Rejected);
    assert!(!t.is_tracked("c1"), "forgotten");
    assert!(!t.is_closing(7));
}

#[test]
fn resolve_failure_after_a_partial_fill_terminalizes_as_filled() {
    // leg-1 (P1) fills, THEN leg-2 (P2) fails: the coid is at PartiallyFilled, so it must
    // terminalize as OrderFilled (a Rejected would be an illegal, dropped transition).
    let mut t = CloseTracker::default();
    t.register("c1", &[(1, 60_000), (2, 40_000)]);
    assert_eq!(t.on_event("c1", 60_000), CloseEmit::Partial { accept_first: true });
    assert_eq!(t.resolve_failure("c1"), CloseFailure::TerminalFilled);
    assert!(!t.is_tracked("c1"));
    assert!(!t.is_closing(2), "the un-filled leg's position is freed too");
}

/// THE property the type exists for: an empty book that was never fetched and an empty book
/// that WAS fetched are different values. Collapse them (drop `fetched_at_ms`, or default it to
/// `Some`) and this test is the one that goes red — which is the difference between "you are
/// flat" and "I have never asked", i.e. between refusing a restart-case exit and admitting it.
#[test]
fn an_empty_book_distinguishes_never_fetched_from_fetched_and_flat() {
    let mut never = PositionBook::default();
    assert!(!never.is_fetched(), "a fresh book has NOT been told anything");
    assert_eq!(never.fetched_at_ms(), None);
    assert!(never.is_empty());

    never.replace_all(std::iter::empty(), 1_700_000_000_000);
    assert!(never.is_fetched(), "an empty reconcile answer IS an answer");
    assert_eq!(never.fetched_at_ms(), Some(1_700_000_000_000));
    assert!(never.is_empty(), "…and the account really is flat");
}

/// A socket death stops the book being EVIDENCE without changing what it routes against —
/// dropping the entries here would turn every reconnect into a hedge-opening window.
#[test]
fn invalidate_clears_the_evidence_but_keeps_the_routing_entries() {
    let mut book = PositionBook::default();
    book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_000);
    assert_eq!(book.for_symbol(1).len(), 1);

    book.invalidate();
    assert!(!book.is_fetched(), "the socket dropped — this is no longer evidence");
    assert_eq!(book.for_symbol(1), vec![(7, pos(1, 1, 100_000, 10))], "routing is unchanged");
    assert_eq!(
        plan_reduce(-1, 100_000, true, &book.for_symbol(1)),
        ClosePlan::Close(vec![(7, 100_000)])
    );
}

/// A wire position row, for the reconcile-fidelity tests below.
fn wire(position_id: i64, status: ProtoOaPositionStatus, volume: i64) -> ProtoOaPosition {
    ProtoOaPosition {
        position_id,
        position_status: status as i32,
        trade_data: crate::proto::ProtoOaTradeData {
            symbol_id: 1,
            volume,
            trade_side: ProtoOaTradeSide::Buy as i32,
            open_timestamp: Some(10),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The split [`reconcile_rows`] exists for: a row the venue says is EMPTY is knowledge, a row
/// this build cannot classify is a HOLE, and only the second may cost the book its evidence
/// flag. `tracked_position`'s `Option` collapses the two, which is exactly how a decode failure
/// used to become a "flat" account.
///
/// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a): collapse the two `None` arms back into one
/// (`None => {}`, i.e. count nothing as unreadable) and this module runs
/// `24 tests run: 23 passed, 1 failed` with THIS the failure, plus
/// `17 tests run: 16 passed, 1 failed` end to end in
/// `crates/bridges/ctrader/tests/exec_halt.rs`.
#[test]
fn reconcile_rows_separates_a_known_empty_row_from_one_it_cannot_read() {
    use ProtoOaPositionStatus::*;
    // Knowledge: the venue told us these hold nothing.
    let (tracked, unreadable) = reconcile_rows(&[
        wire(1, PositionStatusOpen, 100_000),
        wire(2, PositionStatusClosed, 0),
        wire(3, PositionStatusCreated, 0),
    ]);
    assert_eq!(tracked, vec![(1, pos(1, 1, 100_000, 10))]);
    assert_eq!(unreadable, 0, "CLOSED and CREATED are answers, not holes");

    // Holes, one per shape: an ERROR position, a status value this build has no variant for,
    // and an OPEN row whose volume is not positive.
    let mut unknown_status = wire(5, PositionStatusOpen, 100_000);
    unknown_status.position_status = 99;
    let (tracked, unreadable) = reconcile_rows(&[
        wire(4, PositionStatusError, 100_000),
        unknown_status,
        wire(6, PositionStatusOpen, 0),
    ]);
    assert!(tracked.is_empty());
    assert_eq!(unreadable, 3, "every row this build could not classify must be COUNTED");
}

/// …and the consequence at the book: an answer with a hole in it REPLACES the routing entries
/// and is NOT evidence. Route `replace_unverified` to `replace_all` (i.e. mark it fetched
/// anyway) and this test goes red — which is the mutation that would turn an unreadable row
/// into a refused exit under `halt_admit = "verify"`.
#[test]
fn a_partially_unreadable_answer_replaces_the_routing_but_is_not_evidence() {
    let mut book = PositionBook::default();
    book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_000);
    assert!(book.is_fetched());

    book.replace_unverified([(8, pos(1, -1, 40_000, 20))]);
    assert!(
        !book.is_fetched(),
        "a reconcile answer this build could not read in full must not make the book \
             authoritative — an absence in it is a hole, not a flat account"
    );
    assert_eq!(book.fetched_at_ms(), None);
    assert_eq!(book.for_symbol(1), vec![(8, pos(1, -1, 40_000, 20))], "routing still updates");
}

/// ⚠ **THE COVERAGE RULE, and the trap it closes.** A reconcile answer that OMITS a position
/// the venue holds is well-formed and fully classifiable — nothing counts it — so provenance
/// alone (`is_fetched`) says "authoritative" about a symbol nobody ever mentioned. Coverage is
/// what refuses to speak for it.
///
/// ⚠ **MEASURED** (the CI box, 2026-08-08, lane a): disable the symbol arm of
/// [`Self::unauthoritative_for`] (answer `None` once `fetched_at_ms` is `Some`) and this
/// module runs `24 tests run: 22 passed, 2 failed` — THIS test and
/// `the_two_unauthoritative_reasons_are_distinguishable` — plus
/// `17 tests run: 15 passed, 2 failed` in `crates/bridges/ctrader/tests/exec_halt.rs`, where
/// `verify_admits_the_exit_from_a_position_the_reconcile_answer_omitted` is the same property
/// end to end against a venue that really is holding the position.
#[test]
fn a_fetched_book_is_only_evidence_about_symbols_it_positively_covers() {
    let mut book = PositionBook::default();
    assert!(
        book.unauthoritative_for(1).is_some(),
        "a never-fetched book is evidence about nothing"
    );

    // An authoritative, fully-read, EMPTY answer. Provenance is perfect; coverage is nil.
    book.replace_all(std::iter::empty(), 1_700_000_000_000);
    assert!(book.is_fetched(), "provenance is genuinely there");
    assert!(
        book.unauthoritative_for(1).is_some(),
        "…and it must STILL not speak for symbol 1: an answer that omits a position the venue \
             holds looks exactly like this, and reading it as `flat` refuses a live exit"
    );

    // One positive row in symbol 1 — now, and only now, symbol 1 may be spoken for.
    book.replace_all([(7, pos(1, 1, 100_000, 10))], 1_700_000_000_001);
    assert_eq!(book.unauthoritative_for(1), None, "the venue positively reported symbol 1");
    assert!(
        book.unauthoritative_for(9).is_some(),
        "a row in symbol 1 is not evidence about symbol 9 — that would be an absence again, \
             with extra steps"
    );

    // Coverage without provenance is not evidence either: both halves are required.
    book.invalidate();
    assert!(book.unauthoritative_for(1).is_some(), "the socket dropped — provenance is gone");
}

/// The two refusal reasons are DISTINCT strings, because they are distinct facts an operator
/// reads out of one incident's log: "I never got an answer" and "the answer never mentioned
/// your symbol" have different fixes.
#[test]
fn the_two_unauthoritative_reasons_are_distinguishable() {
    let mut book = PositionBook::default();
    let no_provenance = book.unauthoritative_for(1).expect("never fetched");
    book.replace_all(std::iter::empty(), 1_700_000_000_000);
    let no_coverage = book.unauthoritative_for(1).expect("fetched, but empty");
    assert_ne!(no_provenance, no_coverage);
    assert!(no_coverage.contains("no position in this symbol"), "{no_coverage}");
}

/// A reconcile REPLACES rather than merges: a position closed while we were not listening is
/// absent from the answer and must not survive in the book.
#[test]
fn replace_all_drops_positions_the_venue_no_longer_reports() {
    let mut book = PositionBook::default();
    book.upsert(1, pos(1, 1, 60_000, 10));
    book.upsert(2, pos(1, 1, 40_000, 20));
    book.replace_all([(2, pos(1, 1, 40_000, 20))], 1_700_000_000_001);
    assert_eq!(book.for_symbol(1), vec![(2, pos(1, 1, 40_000, 20))]);
    assert_eq!(book.len(), 1);
}

/// `for_symbol` is a filter, and `remove` is the close path.
#[test]
fn for_symbol_filters_and_remove_forgets() {
    let mut book = PositionBook::default();
    book.upsert(1, pos(1, 1, 60_000, 10));
    book.upsert(2, pos(9, -1, 40_000, 20));
    assert_eq!(book.for_symbol(1), vec![(1, pos(1, 1, 60_000, 10))]);
    assert_eq!(book.for_symbol(9), vec![(2, pos(9, -1, 40_000, 20))]);
    book.remove(1);
    assert!(book.for_symbol(1).is_empty());
    assert_eq!(book.len(), 1);
}

#[test]
fn opposing_available_sums_only_the_reducible_side() {
    // A SELL (order_side -1) opposes LONGs; a short in the book is NOT reducible by a SELL.
    let open =
        vec![(1, pos(1, 1, 60_000, 10)), (2, pos(1, 1, 40_000, 20)), (3, pos(1, -1, 25_000, 30))];
    assert_eq!(opposing_available(-1, &open), 100_000, "both longs");
    assert_eq!(opposing_available(1, &open), 25_000, "only the short");
    assert_eq!(opposing_available(0, &open), 0);
}

// ── bound exits (a resting exit tied to its position on a hedged account) ──────────────────────

/// A book holding `positions` (`(position_id, volume)` longs, oldest first).
fn book_of(positions: &[(i64, i64)]) -> (PositionBook, Vec<(i64, TrackedPosition)>) {
    let mut book = PositionBook::default();
    let mut list = Vec::new();
    for (n, &(id, volume)) in positions.iter().enumerate() {
        let p = pos(1, 1, volume, 10 + n as i64);
        book.upsert(id, p);
        list.push((id, p));
    }
    (book, list)
}

/// A bracket's stop and take-profit are different KINDS: both bind the one position, whole volume each.
/// Two stops are not: the second finds the stop capacity used up (or goes to the next position).
#[test]
fn a_bracket_pair_binds_one_position_once_per_kind() {
    let (mut book, free) = book_of(&[(7, 100_000)]);
    let sl = book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free);
    let tp = book.bind_exit_among("tp", -1, ExitKind::Limit, 100_000, &free);
    assert_eq!((sl, tp), (Ok((7, 100_000)), Ok((7, 100_000))));
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered)
    );
    assert_eq!(book.bound_exit("sl2"), None, "a refused exit leaves no row");
}

/// Capacity is by volume: a partial stop leaves room, the next is CAPPED to it, the one after is
/// refused; a BUY facing nothing finds nothing to bind.
#[test]
fn capacity_is_counted_in_volume_and_caps_the_next_exit() {
    let (mut book, free) = book_of(&[(7, 300_000)]);
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free), Ok((7, 100_000)));
    assert_eq!(book.bind_exit_among("b", -1, ExitKind::Stop, 500_000, &free), Ok((7, 200_000)));
    assert_eq!(
        book.bind_exit_among("c", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered)
    );
    assert_eq!(
        book.bind_exit_among("d", 1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::NothingToBind),
        "a BUY faces shorts and there are none"
    );
    assert_eq!(
        book.bind_exit_among("e", -1, ExitKind::Stop, 0, &free),
        Err(ExitRefusal::NothingToBind)
    );
}

/// FIFO: the oldest position takes the first exit of a kind, the next position the next one.
#[test]
fn the_oldest_position_takes_the_first_exit_of_a_kind() {
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free), Ok((5, 100_000)));
    assert_eq!(book.bind_exit_among("b", -1, ExitKind::Stop, 100_000, &free), Ok((6, 100_000)));
    assert_eq!(book.bind_exit_among("c", -1, ExitKind::Limit, 100_000, &free), Ok((5, 100_000)));
}

/// The first position whose room holds the WHOLE exit takes it (the oldest first); an exit that fits none
/// is capped when exactly one position has room, and REFUSED (`Spans`) when more than one does, so it never
/// silently covers one position and leaves the others naked.
#[test]
fn an_exit_that_fits_no_position_is_capped_for_one_and_refused_for_several() {
    // Two positions with room: 1000 units asked fits the oldest; 1200 or 5000 fit neither.
    let (mut book, free) = book_of(&[(5, 100_000), (6, 50_000)]);
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free), Ok((5, 100_000)));
    book.unbind_exit("a");
    assert_eq!(
        book.bind_exit_among("b", -1, ExitKind::Stop, 120_000, &free),
        Err(ExitRefusal::Spans)
    );
    assert_eq!(book.bound_exit("b"), None, "a refused exit leaves no row");
    // The smaller one fits the second position when the first has too little room left.
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 60_000, &free), Ok((5, 60_000)));
    assert_eq!(book.bind_exit_among("b", -1, ExitKind::Stop, 100_000, &free), Ok((6, 100_000)));
    // Only one position with room left: capped, as `reduce_only` means.
    assert_eq!(book.bind_exit_among("c", -1, ExitKind::Stop, 100_000, &free), Ok((5, 40_000)));
}

/// A cancel of a bound exit by the engine stops it counting at once (a cancel and an immediate re-arm must
/// bind), and a refused cancel (or one that never reached the venue) puts it back.
#[test]
fn a_cancelling_exit_stops_counting_and_a_refused_cancel_puts_it_back() {
    let (mut book, free) = book_of(&[(7, 100_000)]);
    book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free).unwrap();
    assert_eq!(book.exit_room("sl"), Some(100_000), "its own volume is the room of its own amend");
    book.mark_cancelling("sl");
    assert_eq!(
        book.bind_exit_among("sl2", -1, ExitKind::Stop, 100_000, &free),
        Ok((7, 100_000)),
        "the cancelled stop no longer covers: its replacement binds"
    );
    book.unbind_exit("sl2");
    book.cancel_failed("sl");
    assert_eq!(
        book.bind_exit_among("sl3", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered),
        "the venue refused the cancel: the stop still rests and covers"
    );
    // The same for a cancel whose WRITE failed.
    book.mark_cancelling("sl");
    book.cancel_unsent("sl");
    assert!(book.exit_room("sl").is_some());
    assert_eq!(
        book.bind_exit_among("sl4", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered)
    );
}

/// The room an AMEND may use is the position's volume less every OTHER live exit of the kind.
#[test]
fn the_room_an_amend_may_use_leaves_out_the_exit_itself() {
    let (mut book, free) = book_of(&[(7, 300_000)]);
    book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.bind_exit_among("b", -1, ExitKind::Stop, 100_000, &free).unwrap();
    assert_eq!(book.exit_room("a"), Some(200_000), "300_000 less b's 100_000");
    book.set_exit_volume("a", 200_000);
    assert_eq!(book.exit_room("b"), Some(100_000), "300_000 less a's 200_000");
    assert_eq!(book.exit_room("nobody"), None);
}

/// A cancel written on a socket that then died may never have arrived: the book going stale makes the
/// bridge's own cancel of an orphan owed again (and the engine's cancelling exit count again).
#[test]
fn a_dead_socket_makes_a_written_orphan_cancel_owed_again() {
    let (mut book, free) = book_of(&[(7, 100_000)]);
    book.bind_exit_among("tp", -1, ExitKind::Limit, 100_000, &free).unwrap();
    book.remove(7);
    book.orphan_exits_on(7, None);
    book.mark_cancel_sent("tp");
    assert!(book.exits_to_cancel().is_empty());
    // A LIVE socket's `invalidate` (an unclassifiable position ref, an answer for another account) leaves
    // the written cancel alone: it is still on its way, and writing it again would only be refused.
    book.invalidate();
    assert!(book.exits_to_cancel().is_empty(), "a cancel on a live socket is not written twice");
    book.socket_died();
    assert_eq!(book.exits_to_cancel(), vec!["tp".to_string()], "re-sent after the reconnect");
    assert!(book.cancel_in_flight("tp"), "and the engine's own cancel stays held back meanwhile");
}

/// A reconnect judges the rows that were WRITTEN against the venue's pending orders: a written row that is
/// not pending is gone (never accepted, or ended during the gap); a row not yet written (queued behind the
/// reconnect) is kept.
#[test]
fn a_reconnect_drops_written_rows_that_are_not_pending_and_keeps_the_unwritten() {
    let (mut book, free) = book_of(&[(7, 300_000)]);
    book.bind_exit_among("written-gone", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.bind_exit_among("written-pending", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.bind_exit_among("queued", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.mark_sent("written-gone");
    book.mark_sent("written-pending");
    let present: std::collections::HashSet<String> =
        ["written-pending".to_string()].into_iter().collect();
    book.forget_sent_exits_not_in(&present);
    assert_eq!(book.bound_exit("written-gone"), None);
    assert!(book.bound_exit("written-pending").is_some());
    assert!(book.bound_exit("queued").is_some(), "not written yet: it is still coming");
}

/// A position that is no longer in the book (the snapshot was a moment old) is skipped, not bound.
#[test]
fn a_position_gone_from_the_book_is_not_bound() {
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    book.remove(5);
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free), Ok((6, 100_000)));
}

/// Orphaning: a position seen gone marks its bound exits, except the order of the event itself; they
/// are owed a cancel until it is written; the engine's own cancel of an owed one is held back; a
/// REFUSED bridge cancel is given up (no loop) and the engine's cancel passes.
#[test]
fn a_position_seen_gone_orphans_its_exits_and_the_cancel_is_owed_until_written() {
    let (mut book, free) = book_of(&[(7, 100_000), (8, 100_000)]);
    book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.bind_exit_among("tp", -1, ExitKind::Limit, 100_000, &free).unwrap();
    book.bind_exit_among("other", -1, ExitKind::Stop, 100_000, &free).unwrap(); // on position 8
    assert!(book.exits_to_cancel().is_empty());

    book.remove(7);
    book.orphan_exits_on(7, Some("sl")); // the stop is the order that filled
    assert_eq!(book.exits_to_cancel(), vec!["tp".to_string()]);
    assert!(book.cancel_in_flight("tp"), "the engine's own cancel of tp is held back");
    assert!(!book.cancel_in_flight("sl") && !book.cancel_in_flight("other"));

    book.mark_cancel_sent("tp");
    assert!(book.exits_to_cancel().is_empty(), "written once");
    assert!(book.cancel_in_flight("tp"));

    book.cancel_failed("tp");
    assert!(!book.cancel_in_flight("tp"), "refused by the venue: give up, let the engine cancel");
    assert!(book.exits_to_cancel().is_empty(), "and never retry in a loop");

    book.unbind_exit("tp");
    assert_eq!(book.bound_exit("tp"), None);
}

/// Only an answer read IN FULL is knowledge that a position is gone: `replace_all` orphans the exits
/// of a position it no longer lists, `replace_unverified` and `invalidate` do not (an absence there is
/// not evidence, so the exit of a possibly-open position is never cancelled on it).
#[test]
fn only_an_authoritative_replace_orphans_exits() {
    let (mut book, free) = book_of(&[(7, 100_000)]);
    book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free).unwrap();
    book.invalidate();
    book.replace_unverified([]);
    assert!(book.exits_to_cancel().is_empty(), "an unverified answer is no evidence");
    book.replace_all([(7, pos(1, 1, 100_000, 10))], 1);
    assert!(book.exits_to_cancel().is_empty(), "the position is still listed");
    book.replace_all([], 2);
    assert_eq!(book.exits_to_cancel(), vec!["sl".to_string()], "an answer read in full lists none");
}

// ── the exits that outlived their position (0.1.97) ──────────────────────────────────────────────────

/// A resting order of the reconcile answer: `kind`, naming `position_id` (or none), `order_id` apart.
fn resting(order_id: i64, kind: ProtoOaOrderType, position_id: Option<i64>) -> ProtoOaOrder {
    ProtoOaOrder {
        order_id,
        trade_data: crate::proto::ProtoOaTradeData {
            symbol_id: 1,
            volume: 100_000,
            trade_side: ProtoOaTradeSide::Sell as i32,
            label: Some(format!("coid-{order_id}")),
            ..Default::default()
        },
        order_type: kind as i32,
        order_status: ProtoOaOrderStatus::OrderStatusAccepted as i32,
        stop_price: Some(1.05),
        limit_price: Some(1.2),
        position_id,
        ..Default::default()
    }
}

fn filled(order_id: i64) -> ProtoOaOrder {
    ProtoOaOrder {
        order_status: ProtoOaOrderStatus::OrderStatusFilled as i32,
        ..resting(order_id, ProtoOaOrderType::Market, Some(9))
    }
}

/// Only a STOP or LIMIT that names a position the book does not hold open is a candidate, and only against a book
/// the venue's answer was read into in full: an unfetched book (or one with a hole) could be missing the very
/// position the order is bound to, and an exit already bound to an open position is no orphan.
#[test]
fn candidates_are_resting_stops_and_limits_on_a_position_the_full_book_does_not_hold() {
    let orders = [
        resting(1, ProtoOaOrderType::Stop, Some(9)), // names a gone position
        resting(2, ProtoOaOrderType::Limit, Some(9)), // the same, a limit
        resting(3, ProtoOaOrderType::Stop, Some(7)), // names an OPEN position
        resting(4, ProtoOaOrderType::Stop, None),    // names none
        resting(5, ProtoOaOrderType::Market, Some(9)), // not an exit kind
        resting(6, ProtoOaOrderType::StopLossTakeProfit, Some(9)), // a position's own protection
    ];
    let (mut book, _) = book_of(&[(7, 100_000)]);
    assert!(orphan_candidates(&orders, &book).is_empty(), "an unfetched book is no evidence");
    book.replace_all([(7, pos(1, 1, 100_000, 10))], 1);
    let ids: Vec<i64> = orphan_candidates(&orders, &book).iter().map(|o| o.order_id).collect();
    assert_eq!(ids, vec![1, 2]);
    // An exit the bridge already holds a row for (bound on an open position) is not a candidate.
    book.bind_exit("coid-1", 9, ExitKind::Stop, 100_000);
    let ids: Vec<i64> = orphan_candidates(&orders, &book).iter().map(|o| o.order_id).collect();
    assert_eq!(ids, vec![2]);
    book.replace_unverified([(7, pos(1, 1, 100_000, 10))]);
    assert!(orphan_candidates(&orders, &book).is_empty(), "a book with a hole is no evidence");
}

/// What the flag carries: the order, its position, side, size, kind and price, for an operator to cancel by hand.
#[test]
fn an_orphaned_exit_carries_what_a_cancel_by_hand_needs() {
    let stop = OrphanedExit::of_order(&resting(11, ProtoOaOrderType::Stop, Some(9))).unwrap();
    assert_eq!(
        (stop.order_id, stop.position_id, stop.kind, stop.side, stop.volume, stop.price),
        (11, 9, ExitKind::Stop, -1, 100_000, Some(1.05))
    );
    assert_eq!(stop.coid.as_deref(), Some("coid-11"));
    let limit = OrphanedExit::of_order(&resting(12, ProtoOaOrderType::Limit, Some(9))).unwrap();
    assert_eq!((limit.kind, limit.price), (ExitKind::Limit, Some(1.2)));
    assert_eq!(OrphanedExit::of_order(&resting(13, ProtoOaOrderType::Stop, None)), None);
    assert_eq!(OrphanedExit::of_order(&resting(14, ProtoOaOrderType::Market, Some(9))), None);
}

/// A FILLED order of the position other than the pending exit proves the position existed and is gone; the
/// pending order alone, a cancelled or rejected one and an empty list prove nothing.
#[test]
fn only_a_fill_other_than_the_exit_itself_proves_the_position_existed() {
    let exit = resting(1, ProtoOaOrderType::Stop, Some(9));
    assert!(proves_a_closed_position(&[exit.clone(), filled(2)], 1));
    assert!(!proves_a_closed_position(std::slice::from_ref(&exit), 1), "the pending order alone");
    assert!(!proves_a_closed_position(&[], 1));
    // The pending exit itself reported FILLED is not a proof about ANOTHER order's position.
    assert!(!proves_a_closed_position(&[filled(1)], 1));
    let cancelled = ProtoOaOrder {
        order_status: ProtoOaOrderStatus::OrderStatusCancelled as i32,
        ..resting(3, ProtoOaOrderType::Stop, Some(9))
    };
    assert!(!proves_a_closed_position(&[exit, cancelled], 1));
}

/// The flagged list lives in the book until each order ends; ending one leaves the others.
#[test]
fn a_flagged_exit_is_forgotten_when_its_order_ends() {
    let (mut book, _) = book_of(&[]);
    let a = OrphanedExit::of_order(&resting(11, ProtoOaOrderType::Stop, Some(9))).unwrap();
    let b = OrphanedExit::of_order(&resting(12, ProtoOaOrderType::Limit, Some(9))).unwrap();
    book.record_orphaned_exits(vec![a, b.clone()]);
    assert_eq!(book.orphaned_exits().len(), 2);
    book.forget_orphaned_exit(11);
    assert_eq!(book.orphaned_exits(), &[b]);
    book.forget_orphaned_exit(99);
    assert_eq!(book.orphaned_exits().len(), 1);
}

// ── an exit that spans positions: planned as one bound order per position ──────────────────────────

/// A grid like EURUSD's: one thousand units is the minimum and the step.
const GRID: VolumeGrid =
    VolumeGrid { lot_size: 10_000_000, min_volume: 100_000, step_volume: 100_000, max_volume: 0 };

/// An exit that fits no ONE position while several have room splits over them, oldest first, each leg at the
/// room that position has (or what the exit still needs), each bound under `<parent>L<n>`; a leg below the
/// minimum volume is not an order, and fewer than two legs is the same refusal as before.
#[test]
fn an_exit_that_fits_no_position_splits_over_the_positions_with_room() {
    let (mut book, free) = book_of(&[(5, 100_000), (6, 200_000)]);
    assert_eq!(
        book.bind_exit_among("p", -1, ExitKind::Stop, 250_000, &free),
        Err(ExitRefusal::Spans),
        "the single-position bind refuses first"
    );
    let legs = book.split_exit_among("p", -1, ExitKind::Stop, 250_000, &free, GRID).unwrap();
    let plan: Vec<_> = legs.iter().map(|l| (l.coid.as_str(), l.position_id, l.volume)).collect();
    assert_eq!(plan, [("pL1", 5, 100_000), ("pL2", 6, 100_000)], "snapped down to the step");
    assert_eq!(book.bound_exit("pL1").map(|b| (b.position_id, b.volume)), Some((5, 100_000)));
    assert_eq!(book.bound_exit("pL2").map(|b| (b.position_id, b.volume)), Some((6, 100_000)));
    assert_eq!(book.live_exit_legs("p"), Some(vec!["pL1".to_string(), "pL2".to_string()]));
    assert_eq!(
        book.bind_exit_among("z", -1, ExitKind::Stop, 100_000, &free),
        Ok((6, 100_000)),
        "position 5 is covered, position 6 still has the rest of its room"
    );

    // Rooms smaller than the exit cover what they allow.
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    let legs = book.split_exit_among("q", -1, ExitKind::Stop, 900_000, &free, GRID).unwrap();
    assert_eq!(legs.iter().map(|l| l.volume).sum::<i64>(), 200_000);

    // The second leg would be below the minimum volume: one leg is no split.
    let (mut book, free) = book_of(&[(5, 100_000), (6, 50_000)]);
    assert_eq!(
        book.split_exit_among("r", -1, ExitKind::Stop, 150_000, &free, GRID),
        Err(ExitRefusal::Spans)
    );
    assert!(
        book.bound_exit("rL1").is_none() && book.live_exit_legs("r").is_none(),
        "nothing left behind"
    );

    // A position another stop already covers gives no room; nothing facing the order is nothing to bind.
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    book.bind_exit_among("other", -1, ExitKind::Stop, 100_000, &free).unwrap();
    assert_eq!(
        book.split_exit_among("s", -1, ExitKind::Stop, 200_000, &free, GRID),
        Err(ExitRefusal::Spans),
        "only position 6 has room: one leg"
    );
    assert_eq!(
        book.split_exit_among("t", 1, ExitKind::Stop, 200_000, &free, GRID),
        Err(ExitRefusal::NothingToBind)
    );
}

/// A split exit that never reached the venue forgets its group and every leg's row; a reconnect that dropped a
/// leg's row retires the leg, and the exit with it when it was the last one resting.
#[test]
fn a_split_exit_is_forgotten_whole_and_a_leg_without_a_row_is_retired() {
    let (mut book, free) = book_of(&[(5, 100_000), (6, 100_000)]);
    book.split_exit_among("p", -1, ExitKind::Stop, 200_000, &free, GRID).unwrap();
    book.forget_split_exit("p");
    assert!(book.bound_exit("pL1").is_none() && book.bound_exit("pL2").is_none());
    assert_eq!(book.live_exit_legs("p"), None);

    book.split_exit_among("p", -1, ExitKind::Stop, 200_000, &free, GRID).unwrap();
    book.retire_exit_legs_without_rows();
    assert_eq!(
        book.live_exit_legs("p").map(|l| l.len()),
        Some(2),
        "both rows are there: none retires"
    );
    book.unbind_exit("pL1");
    book.retire_exit_legs_without_rows();
    assert_eq!(
        book.live_exit_legs("p"),
        Some(vec!["pL2".to_string()]),
        "the leg that vanished no longer rests"
    );
    book.unbind_exit("pL2");
    book.retire_exit_legs_without_rows();
    assert_eq!(book.live_exit_legs("p"), None, "no leg rests: the exit is forgotten");
}

/// The cover count follows the REQUEST: an exit counts at the larger of its size and the one an amend asked for
/// until the venue answers (confirmed: its new size; refused: its old one).
#[test]
fn an_exit_counts_at_the_larger_of_its_size_and_the_one_an_amend_asked_for() {
    let (mut book, free) = book_of(&[(7, 200_000)]);
    book.bind_exit_among("sl", -1, ExitKind::Stop, 100_000, &free).unwrap();
    assert_eq!(book.bind_exit_among("a", -1, ExitKind::Stop, 100_000, &free), Ok((7, 100_000)));
    book.unbind_exit("a");

    book.request_exit_volume("sl", 200_000);
    assert_eq!(
        book.bind_exit_among("b", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered),
        "the asked size is counted before the venue confirms it"
    );
    book.exit_amend_failed("sl");
    assert_eq!(book.bind_exit_among("c", -1, ExitKind::Stop, 100_000, &free), Ok((7, 100_000)));
    book.unbind_exit("c");

    // Confirmed at 2000 units; a later ask for less frees nothing until the venue confirms that too.
    book.request_exit_volume("sl", 200_000);
    book.set_exit_volume("sl", 200_000);
    assert_eq!(
        book.bound_exit("sl").and_then(|b| b.asked_volume),
        None,
        "the answer ends the request"
    );
    book.request_exit_volume("sl", 50_000);
    assert_eq!(
        book.bind_exit_among("d", -1, ExitKind::Stop, 100_000, &free),
        Err(ExitRefusal::Covered),
        "its size still counts"
    );
    book.set_exit_volume("sl", 50_000);
    assert_eq!(book.bind_exit_among("e", -1, ExitKind::Stop, 100_000, &free), Ok((7, 100_000)));
}
