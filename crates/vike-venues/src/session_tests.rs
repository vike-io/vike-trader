use super::*;
use vike_util::calendar::days_from_civil;

/// Epoch-ms for a UTC instant, built from the workspace's one civil-calendar math
/// (`vike_util::calendar`).
fn ms(y: i64, mo: u32, d: u32, h: i64, mi: i64) -> i64 {
    days_from_civil(y, mo, d) * 86_400_000 + h * 3_600_000 + mi * 60_000
}

const MIN: i64 = 60_000;

#[test]
fn week_minute_anchors_monday_at_zero() {
    // 2024-01-01 was a Monday.
    assert_eq!(week_minute(ms(2024, 1, 1, 0, 0)), 0);
    assert_eq!(week_minute(ms(2024, 1, 1, 0, 1)), 1);
    assert_eq!(week_minute(ms(2024, 1, 2, 0, 0)), DAY_MINUTES, "Tuesday");
    assert_eq!(week_minute(ms(2024, 1, 7, 0, 0)), 6 * DAY_MINUTES, "Sunday");
    // last minute of the week, then the wrap back to 0
    assert_eq!(week_minute(ms(2024, 1, 7, 23, 59)), WEEK_MINUTES - 1);
    assert_eq!(week_minute(ms(2024, 1, 8, 0, 0)), 0, "next Monday wraps to 0");
}

#[test]
fn week_minute_is_total_including_pre_epoch() {
    for t in [i64::MIN / 2, -86_400_000 - 1, -1, 0, 1, i64::MAX / 2] {
        let w = week_minute(t);
        assert!((0..WEEK_MINUTES).contains(&w), "week_minute({t}) = {w} out of range");
    }
    // 1970-01-01 was a Thursday (index 3), so epoch 0 sits at 3 whole days in.
    assert_eq!(week_minute(0), 3 * DAY_MINUTES);
}

#[test]
fn segment_is_half_open() {
    let s = SessionSegment::new(100, 200);
    assert!(!s.covers(99));
    assert!(s.covers(100), "start is inclusive");
    assert!(s.covers(199));
    assert!(!s.covers(200), "end is exclusive");
}

// --- the three shipped rows ---

#[test]
fn crypto_is_open_every_minute_of_the_week() {
    for wm in [0, 1, DAY_MINUTES, 4 * DAY_MINUTES + 1320, WEEK_MINUTES - 1] {
        // reconstruct an instant with that week-minute off a known Monday
        let t = ms(2024, 1, 1, 0, 0) + wm * MIN;
        assert!(CRYPTO_24_7.is_open(t), "crypto closed at week-minute {wm}");
    }
    assert_eq!(CRYPTO_24_7, SessionCalendar::ALWAYS_OPEN);
    // never closes → no next_close; already open → next_open is now
    let t = ms(2024, 1, 4, 12, 0);
    assert_eq!(CRYPTO_24_7.next_close(t), None);
    assert_eq!(CRYPTO_24_7.next_open(t), Some(t));
}

/// The FX week: open Mon 00:00 → Fri 22:00 UTC, closed until Sun 21:00 UTC. 2024-01-01 is a
/// Monday, so 2024-01-05 is that week's Friday and 2024-01-07 its Sunday.
#[test]
fn fx_week_weekend_gap_is_friday_2200_to_sunday_2100() {
    // mid-week: open
    assert!(FX_WEEK.is_open(ms(2024, 1, 3, 12, 0)));
    // Friday, either side of the 22:00 UTC close
    assert!(FX_WEEK.is_open(ms(2024, 1, 5, 21, 59)));
    assert!(!FX_WEEK.is_open(ms(2024, 1, 5, 22, 0)), "Fri 22:00 UTC closes the week");
    // the weekend
    assert!(!FX_WEEK.is_open(ms(2024, 1, 6, 12, 0)), "Saturday");
    assert!(!FX_WEEK.is_open(ms(2024, 1, 7, 20, 59)), "Sunday, pre-open");
    // Sunday 21:00 UTC reopens (the DST-union early edge)
    assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 0)));
    assert!(FX_WEEK.is_open(ms(2024, 1, 7, 23, 59)));
    // straight into Monday
    assert!(FX_WEEK.is_open(ms(2024, 1, 8, 0, 0)));
}

/// The FX-week boundaries to the MILLISECOND, and the DST stance stated in assertions.
///
/// DST stance: we do NOT model daylight-saving transitions. The true week boundary is New
/// York 17:00, which is 21:00 UTC under EDT and 22:00 UTC under EST — so the row declares the
/// **union**: it CLOSES at the later edge (Fri 22:00 UTC) and REOPENS at the earlier edge
/// (Sun 21:00 UTC). Consequence, asserted below: the gate under-blocks by up to one hour and
/// never over-blocks. `week_minute` is minute-granular, so a boundary that falls exactly on a
/// minute (all of ours do) is respected to the millisecond.
#[test]
fn fx_week_boundaries_to_the_millisecond() {
    // --- Friday close: 22:00:00.000 UTC (the later, EST edge — DST-union close) ---
    // 21:59:59.999 is the same minute as 21:59, still inside the open segment
    assert!(FX_WEEK.is_open(ms(2024, 1, 5, 21, 59) + 59_999), "Fri 21:59:59.999 — open");
    assert!(!FX_WEEK.is_open(ms(2024, 1, 5, 22, 0)), "Fri 22:00:00.000 — closed");
    // one ms before the close minute is still the previous (open) minute
    assert!(FX_WEEK.is_open(ms(2024, 1, 5, 22, 0) - 1), "Fri 21:59:59.999 (as 22:00 - 1ms)");

    // --- Sunday reopen: 21:00:00.000 UTC (the earlier, EDT edge — DST-union open) ---
    assert!(!FX_WEEK.is_open(ms(2024, 1, 7, 20, 59) + 59_999), "Sun 20:59:59.999 — closed");
    assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 0)), "Sun 21:00:00.000 — reopen");
    // The nominal "Sunday 22:00 GMT" is the LATE (EST) edge; we open an hour early and both
    // 21:59 and 22:00 are open — the deliberate under-block, never an over-block.
    assert!(FX_WEEK.is_open(ms(2024, 1, 7, 21, 59)), "Sun 21:59 — open (past the union edge)");
    assert!(FX_WEEK.is_open(ms(2024, 1, 7, 22, 0)), "Sun 22:00 — open (nominal GMT open)");

    // --- mid-week: open across a whole ordinary weekday ---
    assert!(FX_WEEK.is_open(ms(2024, 1, 3, 0, 0)), "Wed 00:00 — open");
    assert!(FX_WEEK.is_open(ms(2024, 1, 3, 12, 0)), "Wed 12:00 — open");
    assert!(FX_WEEK.is_open(ms(2024, 1, 3, 23, 59) + 59_999), "Wed 23:59:59.999 — open");
}

#[test]
fn fx_week_next_open_and_close_land_on_the_boundaries() {
    // from inside Saturday, the next open is Sunday 21:00 UTC
    assert_eq!(FX_WEEK.next_open(ms(2024, 1, 6, 12, 0)), Some(ms(2024, 1, 7, 21, 0)));
    // from mid-week, the next close is Friday 22:00 UTC
    assert_eq!(FX_WEEK.next_close(ms(2024, 1, 3, 12, 0)), Some(ms(2024, 1, 5, 22, 0)));
    // already open / already closed answer "now"
    let open_now = ms(2024, 1, 3, 12, 0);
    assert_eq!(FX_WEEK.next_open(open_now), Some(open_now));
    let closed_now = ms(2024, 1, 6, 12, 0);
    assert_eq!(FX_WEEK.next_close(closed_now), Some(closed_now));
    // the Sunday-open segment and Monday's segment are CONTIGUOUS across the week wrap, so a
    // Sunday-evening close scan runs through Monday to the following Friday rather than
    // stopping at the week boundary.
    assert_eq!(FX_WEEK.next_close(ms(2024, 1, 7, 22, 0)), Some(ms(2024, 1, 12, 22, 0)));
}

/// US equities: 13:30–21:00 UTC (the EDT∪EST union), weekdays only.
#[test]
fn us_equity_regular_session_bounds_and_weekend() {
    assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 13, 29)));
    assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 13, 30)), "union open edge (EDT 09:30)");
    assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 20, 59)));
    assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 21, 0)), "union close edge (EST 16:00)");
    // overnight
    assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 3, 2, 0)));
    // weekend
    assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 6, 15, 0)), "Saturday");
    assert!(!US_EQUITY_REGULAR.is_open(ms(2024, 1, 7, 15, 0)), "Sunday");
    // all five weekdays are open at 15:00 UTC
    for d in 1..=5 {
        assert!(US_EQUITY_REGULAR.is_open(ms(2024, 1, d, 15, 0)), "weekday {d}");
    }
}

#[test]
fn us_equity_next_open_skips_the_weekend() {
    // Friday after the close → Monday's open
    assert_eq!(US_EQUITY_REGULAR.next_open(ms(2024, 1, 5, 22, 0)), Some(ms(2024, 1, 8, 13, 30)));
    // Thursday after the close → Friday's open
    assert_eq!(US_EQUITY_REGULAR.next_open(ms(2024, 1, 4, 22, 0)), Some(ms(2024, 1, 5, 13, 30)));
    // inside the session → this session's close
    assert_eq!(US_EQUITY_REGULAR.next_close(ms(2024, 1, 3, 15, 0)), Some(ms(2024, 1, 3, 21, 0)));
}

/// The known limitation, pinned so it cannot be forgotten: no holiday database, so a US market
/// holiday reads OPEN. 2024-12-25 (Christmas) was a Wednesday; NYSE was closed.
#[test]
fn holidays_are_not_modeled_and_read_as_open() {
    assert!(
        US_EQUITY_REGULAR.is_open(ms(2024, 12, 25, 15, 0)),
        "holiday calendars are a deliberate deferral (see the module doc)"
    );
}

// --- the registry ---

/// [`session_for`] is the VENUE-ONLY default: single-session venues get their row; the
/// mixed-asset venues (alpaca/ibkr) get `ALWAYS_OPEN` on purpose (venue alone can't pick a
/// session — the (venue, asset-class) resolver does, tested in `vike-catalog`). One arm per
/// roster venue, so a new venue cannot slip in unclassified.
#[test]
fn venue_only_default_classes_every_roster_venue() {
    // `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    // re-indented by rustfmt once a row ending in a trailing `//` comment is generated above
    // it, which defeats `--remove`. Gated by `crates/vike-ops/tests/venues/new_venue_gate/rustfmt_rule.rs`'s
    // `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
        let rows: &[(&str, SessionCalendar)] = &[
            ("binance", CRYPTO_24_7),
            ("bybit", CRYPTO_24_7),
            ("okx", CRYPTO_24_7),
            ("deribit", CRYPTO_24_7),
            ("aster", CRYPTO_24_7),
            ("hyperliquid", CRYPTO_24_7),
            ("polymarket", CRYPTO_24_7),
            ("oanda", FX_WEEK),
            ("ig", FX_WEEK),
            ("fxcm", FX_WEEK),
            ("dukascopy", FX_WEEK),
            ("ctrader", FX_WEEK),
            // mixed-asset venues: venue alone is insufficient ⇒ fail-permissive
            ("alpaca", SessionCalendar::ALWAYS_OPEN),
            ("ibkr", SessionCalendar::ALWAYS_OPEN),
            // vike:new-venue:row ("{venue}", SessionCalendar::ALWAYS_OPEN), // TODO(new-venue: {venue}): match the arm above
        ];
    assert_eq!(rows.len(), vike_model::VENUES.len(), "one declared row per roster venue");
    for &v in vike_model::VENUES {
        let (_, want) = rows
            .iter()
            .find(|(rv, _)| *rv == v)
            .unwrap_or_else(|| panic!("no SessionCalendar row declared for roster venue {v}"));
        assert_eq!(session_for(v), *want, "{v}: venue-only default must serve its declared row");
    }
    // the restricting equity session is NOT reachable by venue alone — that is the fix
    assert_ne!(session_for("alpaca"), US_EQUITY_REGULAR);
    assert_ne!(session_for("ibkr"), US_EQUITY_REGULAR);
}

/// [`is_crypto_native`] is THE set of crypto exchanges, and [`session_for`]'s 24/7 books are that set
/// plus the one documented exception, polymarket. Pinned by NAME, because the calendars cannot say:
/// [`CRYPTO_24_7`] equals [`SessionCalendar::ALWAYS_OPEN`] by value, so a venue-only default of
/// "24/7" is indistinguishable from the fail-permissive one and a value comparison would pass for
/// every roster venue. A venue joining the set, or the exception changing, is an edit to this test.
#[test]
fn the_crypto_native_set_is_pinned_and_polymarket_is_the_one_24_7_book_outside_it() {
    let mut native: Vec<&str> =
        vike_model::VENUES.iter().copied().filter(|v| is_crypto_native(v)).collect();
    native.sort_unstable();
    assert_eq!(native, ["aster", "binance", "bybit", "deribit", "hyperliquid", "okx"]);
    for v in &native {
        assert_eq!(session_for(v), CRYPTO_24_7, "{v}: a crypto exchange's book is 24/7");
    }
    // the one difference from `session_for`'s 24/7 books: a prediction market, no derivatives listed
    assert!(!is_crypto_native("polymarket"));
    assert_eq!(session_for("polymarket"), CRYPTO_24_7);
    // fail-permissive on a name the set does not know, like every resolver here
    assert!(!is_crypto_native("nasdaq"));
}

/// The fail-PERMISSIVE fallback — the deliberate inversion of `venue_caps`'s fail-closed one.
#[test]
fn unknown_venue_is_always_open() {
    assert_eq!(session_for("nasdaq"), SessionCalendar::ALWAYS_OPEN);
    // ...at every minute, including the FX weekend and a US holiday
    for t in [ms(2024, 1, 6, 12, 0), ms(2024, 12, 25, 15, 0), ms(2024, 1, 3, 3, 0)] {
        assert!(venue_is_open("nasdaq", t), "an unknown venue must never be blocked");
    }
    assert_eq!(SessionCalendar::default(), SessionCalendar::ALWAYS_OPEN);
}

#[test]
fn venue_is_open_matches_the_calendar_query() {
    let weekend = ms(2024, 1, 6, 12, 0);
    assert!(!venue_is_open("dukascopy", weekend));
    assert!(venue_is_open("binance", weekend));
    // alpaca is a mixed-asset venue: venue alone can't restrict it, so the venue-only surface
    // reports open even on a Saturday. The (venue, asset-class) resolver is what closes it.
    assert!(venue_is_open("alpaca", weekend));
    for v in vike_model::VENUES {
        assert_eq!(venue_is_open(v, weekend), session_for(v).is_open(weekend), "{v}");
    }
}

#[test]
fn state_mirrors_is_open() {
    let weekend = ms(2024, 1, 6, 12, 0);
    assert_eq!(FX_WEEK.state(weekend), SessionState::Closed);
    assert_eq!(CRYPTO_24_7.state(weekend), SessionState::Open);
}

// --- structural invariants every row must hold ---

#[test]
fn every_row_has_ascending_non_overlapping_in_range_segments() {
    for (name, cal) in [
        ("ALWAYS_OPEN", SessionCalendar::ALWAYS_OPEN),
        ("CRYPTO_24_7", CRYPTO_24_7),
        ("FX_WEEK", FX_WEEK),
        ("US_EQUITY_REGULAR", US_EQUITY_REGULAR),
    ] {
        let mut prev_end = 0;
        for s in cal.segments {
            assert!(s.start_min < s.end_min, "{name}: empty/inverted segment {s:?}");
            assert!(s.start_min >= prev_end, "{name}: segments must ascend, got {s:?}");
            assert!(
                (0..=WEEK_MINUTES).contains(&s.end_min) && s.start_min >= 0,
                "{name}: segment {s:?} out of the week"
            );
            prev_end = s.end_min;
        }
        assert!(!cal.tz.is_empty(), "{name}: tz provenance must be recorded");
    }
}

/// `next_open`/`next_close` agree with `is_open` at and just before every boundary they
/// report, for every row — the property that keeps the scan and the predicate one law.
#[test]
fn scan_results_agree_with_is_open() {
    let base = ms(2024, 1, 1, 0, 0);
    for cal in [CRYPTO_24_7, FX_WEEK, US_EQUITY_REGULAR] {
        for k in (0..WEEK_MINUTES).step_by(37) {
            let t = base + k * MIN;
            if let Some(o) = cal.next_open(t) {
                assert!(cal.is_open(o), "next_open({t}) = {o} is not open");
                assert!(o >= t);
                if o > t {
                    assert!(!cal.is_open(o - MIN), "next_open({t}) = {o} is not the FIRST open");
                }
            }
            if let Some(c) = cal.next_close(t) {
                assert!(!cal.is_open(c), "next_close({t}) = {c} is not closed");
                assert!(c >= t);
                if c > t {
                    assert!(cal.is_open(c - MIN), "next_close({t}) = {c} is not the FIRST close");
                }
            }
        }
    }
}

// --- the exact (civil) query: the live door's closed-market note reads it, never the fill gate ---

/// The US daylight-saving dates, pinned to the instant: second Sunday of March at 02:00 EST
/// (07:00 UTC) and first Sunday of November at 02:00 EDT (06:00 UTC), for the years around the
/// week of the note's brief (a leap year, and a year whose March Sunday is the 14th).
#[test]
fn new_york_offset_changes_at_the_us_dst_instants() {
    for (y, march, nov) in [(2024, 10, 3), (2025, 9, 2), (2026, 8, 1), (2027, 14, 7)] {
        assert_eq!(new_york_utc_offset_min(ms(y, 3, march, 6, 59)), 300, "{y}: still EST");
        assert_eq!(new_york_utc_offset_min(ms(y, 3, march, 7, 0)), 240, "{y}: EDT from 07:00 UTC");
        assert_eq!(new_york_utc_offset_min(ms(y, 11, nov, 5, 59)), 240, "{y}: still EDT");
        assert_eq!(new_york_utc_offset_min(ms(y, 11, nov, 6, 0)), 300, "{y}: EST from 06:00 UTC");
        assert_eq!(new_york_utc_offset_min(ms(y, 7, 1, 12, 0)), 240, "{y}: midsummer");
        assert_eq!(new_york_utc_offset_min(ms(y, 1, 15, 12, 0)), 300, "{y}: midwinter");
    }
}

/// The New York close, in UTC, either side of the November 2026 change (clocks go back on Sunday
/// 1 November): Friday 30 October 17:00 EDT is 21:00 UTC, and the Sunday 1 November reopen at
/// 17:00 EST is 22:00 UTC. The union row says OPEN for the hour after each, which is what the exact
/// query must not.
#[test]
fn fx_week_exact_boundaries_either_side_of_the_november_2026_change() {
    // Friday 30 Oct, EDT: the week closes at 21:00 UTC, not the union's 22:00
    assert!(FX_WEEK.is_open_civil(ms(2026, 10, 30, 20, 59)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 10, 30, 21, 0)), "17:00 EDT = 21:00 UTC closes it");
    assert!(!FX_WEEK.is_open_civil(ms(2026, 10, 30, 21, 59)));
    assert!(FX_WEEK.is_open(ms(2026, 10, 30, 21, 30)), "the union row leaves that hour open");
    // Sunday 1 Nov, EST (the clocks went back at 06:00 UTC): it reopens at 22:00 UTC, not 21:00
    assert!(!FX_WEEK.is_open_civil(ms(2026, 11, 1, 21, 0)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 11, 1, 21, 59)));
    assert!(FX_WEEK.is_open_civil(ms(2026, 11, 1, 22, 0)), "17:00 EST = 22:00 UTC reopens it");
    assert!(FX_WEEK.is_open(ms(2026, 11, 1, 21, 30)), "the union row opens an hour early");
    // the first full week on standard time: Friday 6 Nov closes at 22:00 UTC, Sunday 8 Nov opens at 22:00
    assert!(FX_WEEK.is_open_civil(ms(2026, 11, 6, 21, 59)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 11, 6, 22, 0)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 11, 8, 21, 59)));
    assert!(FX_WEEK.is_open_civil(ms(2026, 11, 8, 22, 0)));
    // the week wrap is not a boundary: Sunday evening runs straight into Monday
    assert!(FX_WEEK.is_open_civil(ms(2026, 11, 8, 23, 59)));
    assert!(FX_WEEK.is_open_civil(ms(2026, 11, 9, 0, 0)));
}

/// …and either side of the March 2026 change (clocks go forward on Sunday 8 March at 07:00 UTC):
/// Friday 6 March is still EST (closes 22:00 UTC) and the reopen that Sunday is 17:00 EDT = 21:00 UTC.
#[test]
fn fx_week_exact_boundaries_either_side_of_the_march_2026_change() {
    assert!(FX_WEEK.is_open_civil(ms(2026, 3, 6, 21, 59)), "EST: the close is 22:00 UTC");
    assert!(!FX_WEEK.is_open_civil(ms(2026, 3, 6, 22, 0)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 3, 8, 20, 59)), "EDT: the reopen is 21:00 UTC");
    assert!(FX_WEEK.is_open_civil(ms(2026, 3, 8, 21, 0)));
    // and the Friday after it closes at 21:00 UTC
    assert!(FX_WEEK.is_open_civil(ms(2026, 3, 13, 20, 59)));
    assert!(!FX_WEEK.is_open_civil(ms(2026, 3, 13, 21, 0)));
}

/// The reopen the note names: from a closed instant, the first open instant in civil time.
#[test]
fn next_open_civil_names_the_real_reopen() {
    assert_eq!(
        FX_WEEK.next_open_civil(ms(2026, 10, 30, 21, 30)),
        Some(ms(2026, 11, 1, 22, 0)),
        "Friday 30 Oct, closed: reopens Sunday 1 Nov 22:00 UTC (EST)"
    );
    assert_eq!(
        FX_WEEK.next_open_civil(ms(2026, 3, 6, 22, 30)),
        Some(ms(2026, 3, 8, 21, 0)),
        "Friday 6 Mar, closed: reopens Sunday 8 Mar 21:00 UTC (EDT), the 47-hour weekend"
    );
    assert_eq!(
        FX_WEEK.next_open_civil(ms(2026, 10, 28, 12, 0)),
        Some(ms(2026, 10, 28, 12, 0)),
        "open now answers now"
    );
    assert_eq!(
        US_EQUITY_REGULAR.next_open_civil(ms(2026, 10, 30, 21, 0)),
        Some(ms(2026, 11, 2, 14, 30)),
        "Friday after the close: Monday 2 Nov, 09:30 EST = 14:30 UTC"
    );
}

/// US equities, 09:30-16:00 New York: 13:30-20:00 UTC under EDT, 14:30-21:00 under EST, and the
/// first Monday after each change reads the new offset.
#[test]
fn us_equity_exact_session_follows_the_daylight_saving_offset() {
    // EDT, Wednesday 1 July 2026
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 7, 1, 13, 29)));
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 7, 1, 13, 30)));
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 7, 1, 19, 59)));
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 7, 1, 20, 0)), "16:00 EDT = 20:00 UTC");
    // EST, Wednesday 7 January 2026
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 1, 7, 14, 29)), "09:30 EST = 14:30 UTC");
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 1, 7, 14, 30)));
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 1, 7, 20, 59)));
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 1, 7, 21, 0)));
    // Monday 2 Nov 2026, the first session after the clocks went back
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 11, 2, 14, 29)));
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 11, 2, 14, 30)));
    // Monday 9 Mar 2026, the first session after they went forward
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 3, 9, 13, 29)));
    assert!(US_EQUITY_REGULAR.is_open_civil(ms(2026, 3, 9, 13, 30)));
    // the weekend is closed all day
    assert!(!US_EQUITY_REGULAR.is_open_civil(ms(2026, 7, 4, 15, 0)));
}

/// The narrowing is derived from the rows, so check it against an INDEPENDENT statement of the
/// civil week: every minute of 2026, for both New-York-quoted rows, against the New York local
/// clock read straight off `new_york_utc_offset_min` (FX: closed from Friday 17:00 to Sunday 17:00
/// local; equities: Monday-Friday 09:30-16:00 local).
#[test]
fn the_exact_query_is_the_new_york_local_week_every_minute_of_2026() {
    let from = ms(2026, 1, 1, 0, 0);
    let to = ms(2027, 1, 1, 0, 0);
    let mut t = from;
    while t < to {
        let local = week_minute(t - new_york_utc_offset_min(t) * MIN);
        let fx = !(4 * DAY_MINUTES + 17 * 60..6 * DAY_MINUTES + 17 * 60).contains(&local);
        let eq = local < 5 * DAY_MINUTES && (570..960).contains(&(local % DAY_MINUTES));
        assert_eq!(FX_WEEK.is_open_civil(t), fx, "FX at {t}");
        assert_eq!(US_EQUITY_REGULAR.is_open_civil(t), eq, "equity at {t}");
        t += MIN;
    }
}

/// A calendar quoted in another zone, and the 24/7 one, answer exactly as `is_open` does.
#[test]
fn a_calendar_outside_new_york_is_its_own_union_answer() {
    const UTC_ROW: SessionCalendar = SessionCalendar {
        tz: "UTC",
        segments: &[
            SessionSegment::new(60, 120),
            SessionSegment::new(DAY_MINUTES, 2 * DAY_MINUTES),
        ],
    };
    let base = ms(2026, 11, 2, 0, 0);
    for k in (0..WEEK_MINUTES).step_by(7) {
        assert_eq!(UTC_ROW.is_open_civil(base + k * MIN), UTC_ROW.is_open(base + k * MIN));
    }
    assert!(CRYPTO_24_7.is_open_civil(ms(2026, 10, 31, 12, 0)), "crypto is open on a Saturday");
    assert_eq!(CRYPTO_24_7.next_open_civil(ms(2026, 10, 31, 12, 0)), Some(ms(2026, 10, 31, 12, 0)));
}
