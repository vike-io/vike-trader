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
