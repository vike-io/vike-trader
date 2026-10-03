use super::*;

// ---- find_gaps (moved here with the function, so they now run in the DEFAULT lane too rather
// ---- than only under `hist-datafusion`) ------------------------------------------------------

#[test]
fn no_gap_for_contiguous_days() {
    assert_eq!(find_gaps(&[1, 2, 3, 4], 1), Vec::new());
}

#[test]
fn empty_for_empty_or_single_day_input() {
    assert_eq!(find_gaps(&[], 1), Vec::new());
    assert_eq!(find_gaps(&[5], 1), Vec::new());
}

#[test]
fn one_gap_between_two_runs() {
    assert_eq!(find_gaps(&[1, 2, 5, 6], 1), vec![(3, 4)]);
}

#[test]
fn multiple_gaps_from_unsorted_duplicated_input() {
    // unsorted + duplicated input still finds every gap, sorted ascending.
    assert_eq!(find_gaps(&[10, 1, 1, 5, 3, 10, 7], 1), vec![(2, 2), (4, 4), (6, 6), (8, 9)]);
}

#[test]
fn zero_or_negative_step_is_a_noop() {
    assert_eq!(find_gaps(&[1, 5], 0), Vec::new());
    assert_eq!(find_gaps(&[1, 5], -1), Vec::new());
}

// ---- the cross-kind join ---------------------------------------------------------------------

fn quote(venue: &str, sym: &str) -> SeriesId {
    SeriesId::per_symbol("quote", venue, sym, None)
}
fn trade(venue: &str, sym: &str) -> SeriesId {
    SeriesId::per_symbol("trade", venue, sym, None)
}
fn book(venue: &str, sym: &str) -> SeriesId {
    SeriesId::per_symbol("book", venue, sym, None)
}
fn depth(venue: &str, sym: &str) -> SeriesId {
    SeriesId::per_symbol("depth", venue, sym, None)
}

#[test]
fn a_fully_covered_instrument_reports_nothing_to_explain() {
    let got = join_coverage(&[
        (trade("polymarket", "TOK"), vec![10, 11, 12]),
        (quote("polymarket", "TOK"), vec![10, 11, 12]),
        (book("polymarket", "TOK"), vec![10, 11, 12]),
    ]);
    assert_eq!(got.len(), 1);
    assert!(got[0].is_complete());
    assert!(got[0].partial_days().is_empty());
}

/// **The case this module exists for.** A Polymarket venue-backfill restores the trade tape and
/// nothing else — no book history exists to fetch. Per-kind, neither series looks wrong: trades
/// are contiguous, and the book series simply has no rows for those days. Joined, the hole is
/// unmissable, and a market-making backtest over day 11 would otherwise run on no book at all.
#[test]
fn a_trades_only_backfill_shows_up_as_a_partial_day() {
    let got = join_coverage(&[
        (trade("polymarket", "TOK"), vec![10, 11, 12]),
        (quote("polymarket", "TOK"), vec![10, 12]),
        (book("polymarket", "TOK"), vec![10, 12]),
    ]);

    let partial = got[0].partial_days();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].day, 11);
    assert_eq!(
        partial[0].missing_kinds,
        vec!["quote".to_string(), "book".to_string()],
        "in TICK_KINDS order, not the kinds map's alphabetical one"
    );
    assert!(!got[0].is_complete());
}

/// A kind that was NEVER recorded is a row with no days, not a missing map entry — a renderer
/// must not be able to confuse "no book" with "I did not look for book".
#[test]
fn a_never_recorded_kind_is_present_as_an_empty_row() {
    let got = join_coverage(&[(trade("binance", "BTCUSDT"), vec![5, 6])]);

    assert_eq!(got[0].kinds.len(), TICK_KINDS.len(), "every kind always appears");
    assert!(got[0].kinds["book"].absent());
    assert!(got[0].kinds["quote"].absent());
    assert!(got[0].kinds["depth"].absent());
    assert!(!got[0].kinds["trade"].absent());
}

/// **A trade-only instrument is not "partial" — it is complete at what it records.**
///
/// Held against the global [`TICK_KINDS`] this was partial on EVERY day it would ever have: a
/// permanent ⚠ carrying no day-to-day information, which is how an operator learns to ignore a
/// warning column. "There is no book here" is a different fact, already named by
/// [`KindDays::absent`] and already visible in the Data Manager as the absence of a book row.
#[test]
fn an_instrument_recording_one_kind_is_never_partial() {
    let got = join_coverage(&[(trade("binance", "BTCUSDT"), vec![5, 6])]);
    assert_eq!(got[0].recorded_kinds(), vec!["trade"]);
    assert!(got[0].partial_days().is_empty(), "{:?}", got[0].partial_days());
    assert!(got[0].is_complete());
}

/// The live case that exposed this: the recorder writes binance L2 to `depth` (the conflated
/// snapshot lane), never to `book` (the event lane). Both kinds cover both days, so there is
/// nothing to report — before, this instrument was flagged "missing book, quote" forever.
#[test]
fn a_trade_plus_depth_instrument_is_complete() {
    let got = join_coverage(&[
        (trade("binance", "BTCUSDT.P"), vec![5, 6]),
        (depth("binance", "BTCUSDT.P"), vec![5, 6]),
    ]);
    assert_eq!(got[0].recorded_kinds(), vec!["trade", "depth"], "TICK_KINDS order");
    assert!(got[0].is_complete(), "{:?}", got[0].partial_days());
}

/// …and the same instrument IS reported the day one of its two lanes stops. This is the signal
/// the column exists for, and narrowing to recorded kinds does not weaken it.
#[test]
fn a_depth_outage_beside_a_live_trade_tape_is_partial() {
    let got = join_coverage(&[
        (trade("binance", "BTCUSDT.P"), vec![5, 6, 7]),
        (depth("binance", "BTCUSDT.P"), vec![5, 7]),
    ]);
    let partial = got[0].partial_days();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].day, 6);
    assert_eq!(partial[0].missing_kinds, vec!["depth".to_string()]);
}

/// Gaps INSIDE one kind's own span are still reported per kind — the join adds the cross-kind
/// view, it does not replace `series_gaps`.
#[test]
fn a_gap_within_one_kind_is_reported_on_that_kind() {
    let got = join_coverage(&[
        (trade("okx", "BTC-USDT"), vec![1, 2, 5]),
        (quote("okx", "BTC-USDT"), vec![1, 2, 5]),
        (book("okx", "BTC-USDT"), vec![1, 2, 5]),
    ]);
    assert_eq!(got[0].kinds["trade"].gaps, vec![(3, 4)]);
    assert_eq!(got[0].kinds["trade"].missing_days(), 2);
    // Days 3-4 are absent from EVERY kind, so they are not in the spanned union and are not
    // "partial" — they are a plain gap, which `gaps` already reports.
    assert!(got[0].is_complete(), "a hole in all three kinds is a gap, not a partial day");
}

/// Grouped and per-symbol series of the same name are separate instruments: different
/// directories, different manifests. Merging them would hide a mid-migration split.
#[test]
fn grouped_and_per_symbol_series_do_not_merge() {
    let got = join_coverage(&[
        (SeriesId::per_symbol("trade", "polymarket", "fam", None), vec![1]),
        (SeriesId::grouped("trade", "polymarket", "fam"), vec![2]),
    ]);
    assert_eq!(got.len(), 2, "{got:#?}");
    assert!(got.iter().any(|c| !c.key.grouped));
    assert!(got.iter().any(|c| c.key.grouped));
}

/// Bars are derived from trades, so a missing bar day is an un-run resample, not a hole in the
/// tape. Including them would train an operator to ignore the report.
#[test]
fn non_tick_kinds_are_ignored() {
    let got = join_coverage(&[
        (SeriesId::per_symbol("bar", "binance", "BTCUSDT", Some("1m".into())), vec![1, 2]),
        (SeriesId::per_symbol("properties", "binance", "BTCUSDT", None), vec![1]),
        (trade("binance", "BTCUSDT"), vec![1, 2]),
    ]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kinds.len(), TICK_KINDS.len(), "only the tick kinds");
    assert!(!got[0].kinds.contains_key("bar"));
    assert!(!got[0].kinds.contains_key("properties"));
}

#[test]
fn instruments_are_venue_scoped() {
    let got = join_coverage(&[
        (trade("binance", "BTCUSDT"), vec![1]),
        (trade("okx", "BTCUSDT"), vec![1]),
    ]);
    assert_eq!(got.len(), 2);
}

#[test]
fn a_day_index_converts_to_its_utc_midnight() {
    let p = PartialDay { day: 2, missing_kinds: vec!["book".into()] };
    assert_eq!(p.start_ms(), 2 * 86_400_000);
}

// ---- window_shortfall ------------------------------------------------------------------------

fn span(start_ms: i64, end_ms: i64, kind: Shortfall) -> MissingSpan {
    MissingSpan { start_ms, end_ms, kind }
}

/// The case `find_gaps` is structurally blind to, and the reason this function exists: the tape
/// is perfectly contiguous and still does not reach what the window asked for.
#[test]
fn a_contiguous_tape_that_starts_late_is_a_leading_shortfall() {
    let got = window_shortfall(Some(100), Some(900), Some((500, 900)), &[]);
    assert_eq!(got, vec![span(100, 499, Shortfall::Leading)]);
    assert_eq!(missing_ms(&got), 400);
}

#[test]
fn a_tape_that_ends_early_is_a_trailing_shortfall() {
    let got = window_shortfall(Some(100), Some(900), Some((100, 500)), &[]);
    assert_eq!(got, vec![span(501, 900, Shortfall::Trailing)]);
}

#[test]
fn a_fully_covered_window_is_short_of_nothing() {
    assert!(window_shortfall(Some(100), Some(900), Some((50, 1000)), &[]).is_empty());
    assert!(window_shortfall(Some(100), Some(900), Some((100, 900)), &[]).is_empty());
}

/// The load-bearing rule: an unbounded side ASKED for nothing, so it cannot be short. Without
/// this, every profile with no `data.from` reports the whole of time as missing.
#[test]
fn an_unbounded_window_side_is_never_short() {
    assert!(window_shortfall(None, None, Some((500, 600)), &[]).is_empty());
    assert_eq!(
        window_shortfall(None, Some(900), Some((500, 600)), &[]),
        vec![span(601, 900, Shortfall::Trailing)]
    );
    assert_eq!(
        window_shortfall(Some(100), None, Some((500, 600)), &[]),
        vec![span(100, 499, Shortfall::Leading)]
    );
}

/// An ABSENT series and an all-zero one are different findings, and the type says which.
#[test]
fn an_absent_series_is_everything_not_a_leading_plus_trailing_pair() {
    assert_eq!(
        window_shortfall(Some(100), Some(900), None, &[]),
        vec![span(100, 900, Shortfall::Everything)]
    );
    // The same window against a series that really does hold one row at the epoch reads as the
    // two EXISTENCE facts instead — which is the answer `series_dir_of`'s all-zero manifest
    // fold would otherwise have impersonated.
    assert_eq!(
        window_shortfall(Some(100), Some(900), Some((0, 0)), &[]),
        vec![span(100, 900, Shortfall::Trailing)]
    );
}

#[test]
fn an_absent_series_over_an_unbounded_window_does_not_overflow() {
    let got = window_shortfall(None, None, None, &[]);
    assert_eq!(got, vec![span(i64::MIN, i64::MAX, Shortfall::Everything)]);
    // Saturating rather than panicking in debug / wrapping in release.
    assert_eq!(got[0].len_ms(), i64::MAX);
    assert_eq!(missing_ms(&got), i64::MAX);
}

#[test]
fn interior_gaps_are_clipped_to_the_window_and_ones_outside_it_are_dropped() {
    let got = window_shortfall(
        Some(200),
        Some(800),
        Some((100, 900)),
        &[(120, 250), (400, 450), (850, 880)],
    );
    assert_eq!(
        got,
        vec![span(200, 250, Shortfall::Interior), span(400, 450, Shortfall::Interior)],
        "the first gap is clipped to the window's start and the last falls outside it"
    );
}

#[test]
fn all_three_shortfalls_come_back_in_time_order() {
    let got = window_shortfall(Some(0), Some(1000), Some((100, 900)), &[(400, 500)]);
    assert_eq!(
        got,
        vec![
            span(0, 99, Shortfall::Leading),
            span(400, 500, Shortfall::Interior),
            span(901, 1000, Shortfall::Trailing),
        ]
    );
}

/// An inverted window is the PROFILE's mistake and already has its own refusal; reporting it
/// again here would name a data fault that does not exist.
#[test]
fn an_inverted_window_asked_for_nothing() {
    assert!(window_shortfall(Some(900), Some(100), None, &[(1, 2)]).is_empty());
    assert!(window_shortfall(Some(900), Some(100), Some((0, 10)), &[]).is_empty());
}

#[test]
fn an_inverted_recorded_span_is_reported_as_wholly_missing() {
    assert_eq!(
        window_shortfall(Some(100), Some(900), Some((900, 100)), &[]),
        vec![span(100, 900, Shortfall::Everything)]
    );
}
