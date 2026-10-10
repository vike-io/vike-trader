//! `fetch`: the `backfill-serve` hint, the documented date bounds and the per-year cut.

use super::*;

// ── D2 (0094 follow-ups): the `--features backfill-serve` hint is not true of every failure ────

/// The hint is true of exactly ONE of [`vike_datahub_client::DatahubClient::backfill`]'s two
/// client-side capability refusals — the server never advertised `backfill` at all — and false of
/// its sibling, which means a `backfill-serve` server simply predates the funding lane. Spelled
/// against the two REAL messages that function formats, not a paraphrase of them.
#[test]
fn is_missing_backfill_feature_discriminates_the_two_capability_refusals() {
    assert!(
        is_missing_backfill_feature(
            "datahub server does not advertise `backfill` (advertised: []) — nothing was sent. \
             Backfill needs a server built with `--features backfill-serve` (or a newer server; \
             this verb is capability-negotiated, not version-gated)."
        ),
        "the server never advertised `backfill` at all — the hint belongs here"
    );
    assert!(
        !is_missing_backfill_feature(
            "datahub server does not advertise `backfill_funding` (advertised: [\"backfill\"]) — \
             nothing was sent. A funding-rate backfill (`VENUE:SYMBOL:funding`) needs a \
             `backfill-serve` server from a release that carries the funding lane."
        ),
        "this server DOES run backfill-serve — appending the hint here would contradict its own \
         sentence"
    );
}

/// A server-side `Response::Error` — a funding/spot/unknown-venue refusal from a server that
/// plainly runs `backfill-serve`, since it answered the verb at all — must not be read as the
/// missing-feature case either.
#[test]
fn is_missing_backfill_feature_is_false_for_an_ordinary_server_side_refusal() {
    assert!(!is_missing_backfill_feature("unknown venue 'not-a-venue'"));
    assert!(!is_missing_backfill_feature(
        "binance: \"BTCUSDT\" names SPOT, and funding is a perpetual's series — ask for \
         \"BTCUSDT.P\""
    ));
}

// ── the date help is what the parser takes (the OANDA history design's follow-up 3) ──────────

/// Every concrete `YYYY-MM-DD` date in `text`, with its `THH` hour suffix when it carries one, in
/// order. The placeholder spelling itself (`YYYY-MM-DD`) has no digits and is not a token.
fn date_tokens(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= b.len() {
        let starts_a_word = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        if starts_a_word
            && digits(i..i + 4)
            && b[i + 4] == b'-'
            && digits(i + 5..i + 7)
            && b[i + 7] == b'-'
            && digits(i + 8..i + 10)
        {
            let mut end = i + 10;
            if end + 3 <= b.len() && b[end] == b'T' && digits(end + 1..end + 3) {
                end += 3;
            }
            out.push(&text[i..end]);
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// `USAGE`'s option row for `flag` — from its line to the next option row.
fn usage_row(flag: &str) -> &'static str {
    let at = USAGE.find(&format!("\n  {flag} ")).unwrap_or_else(|| panic!("no {flag} row"));
    let rest = &USAGE[at + 1..];
    let end = rest[1..].find("\n  -").map_or(rest.len(), |n| n + 1);
    &rest[..end]
}

/// **Every bound this verb's help documents is one [`parse_date_label`] takes** — the parser
/// `fetch_window_ms` reads `--from`/`--to` through. It takes epoch-ms or a UTC date `YYYY-MM-DD`
/// and REFUSES an hour label, and the help said `YYYY-MM-DDTHH` in three places — the window error,
/// the skill's prose and the skill's own fetch example — from the day the route moved to a datahub
/// until this test fed each documented example through the parser. Each half below would have gone
/// red on that text.
///
/// ⚠ The `--from` row documents TWO spellings because two processes parse it: this binary for
/// `fetch`/`universe`/a remote export, the ENGINE for an engine export (`parse_ts`, which takes
/// the hour label and refuses a bare date). So the row's hour example must be one THIS parser
/// refuses — that is what the row says about it — and its date examples ones it takes.
#[test]
fn a_documented_fetch_bound_is_one_the_parser_takes() {
    // The window error: names the date spelling, an example the parser takes, and no hour label.
    let err = window_from(None, None, None).unwrap_err();
    assert!(err.contains("YYYY-MM-DD") && !err.contains("YYYY-MM-DDTHH"), "{err}");
    let examples = date_tokens(&err);
    assert!(!examples.is_empty(), "the window error names no example a reader can copy: {err}");
    for ex in examples {
        parse_date_label(ex).unwrap_or_else(|e| panic!("the window error's {ex:?}: {e}"));
    }

    // USAGE's `--from` row (and `--to` says "same spellings").
    let row = usage_row("--from LABEL");
    let (hours, dates): (Vec<&str>, Vec<&str>) =
        date_tokens(row).into_iter().partition(|t| t.contains('T'));
    assert!(!dates.is_empty() && !hours.is_empty(), "the row names both spellings: {row}");
    for d in dates {
        parse_date_label(d).unwrap_or_else(|e| panic!("the --from row's date {d:?}: {e}"));
    }
    for h in hours {
        assert!(parse_date_label(h).is_err(), "{h:?} is said to be refused HERE and is not");
        assert!(vike_model::time::parse_hour_label(h).is_some(), "{h:?} is not even an hour label");
    }
    assert!(usage_row("--to LABEL").contains("same spellings"), "{}", usage_row("--to LABEL"));

    // The get-market-data skill, as RENDERED (the copy that ships and that an agent executes):
    // every `--from`/`--to` value on a `data hist fetch` line, and the sentence describing them.
    let skill_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/get-market-data/SKILL.md");
    let skill = std::fs::read_to_string(&skill_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", skill_path.display()));
    let mut bounds = 0;
    for line in skill.lines().filter(|l| l.trim_start().starts_with("vike-cli data hist fetch ")) {
        let words: Vec<&str> = line.split_whitespace().collect();
        for pair in words.windows(2).filter(|p| p[0] == "--from" || p[0] == "--to") {
            parse_date_label(pair[1])
                .unwrap_or_else(|e| panic!("the skill's fetch example {line:?}: {e}"));
            bounds += 1;
        }
    }
    assert!(bounds >= 2, "the skill's fetch examples carry no --from/--to to check");
    let at = skill.find("OR `--from` and").expect("the skill's window sentence");
    let sentence = &skill[at..at + skill[at..].find("never a mixture").expect("its end")];
    assert!(
        sentence.contains("`YYYY-MM-DD`") && !sentence.contains("YYYY-MM-DDTHH"),
        "the skill's fetch window sentence: {sentence}"
    );
}

// ── `fetch`'s per-year cut (`super::fetch_split`) ──────────────────────────────────────────────

/// The tests of `crate::cmd::data`'s `fetch_split` module — here, in a module of their own, rather
/// than in a `fetch_split_tests.rs` file.
mod fetch_split_cases {
    use std::cell::RefCell;
    use std::time::Duration;

    use vike_catalog::{ChannelState, HistoryLane, history_channels_for};
    use vike_datahub_client::BackfillDone;
    use vike_model::{
        MS_PER_DAY, VENUES,
        time::{civil_from_days, days_from_civil},
    };

    use crate::cmd::data::hist::fetch_split::{
        PieceDone, Plan, elapsed, header_line, plan, progress_line, run_pieces, splits_by_year,
        stores_whole_utc_days,
    };

    /// Midnight UTC of `y-m-d`, in epoch-ms.
    fn day(y: i64, m: u32, d: u32) -> i64 {
        days_from_civil(y, m, d) * MS_PER_DAY
    }

    fn pieces(plan: &Plan) -> Vec<(i64, i64)> {
        plan.pieces().collect()
    }

    /// The properties every cut must have, whatever the window: the pieces tile `[start, end]`
    /// with no instant in two and none in neither, every interior cut is a 1 January midnight UTC,
    /// and `Plan::count` — computed without walking — agrees with the walk.
    fn assert_tiles(plan: &Plan) {
        let (start, end) = plan.window();
        let got = pieces(plan);
        assert_eq!(got.len() as u64, plan.count(), "count() disagrees with the walk: {got:?}");
        assert_eq!(got.first().map(|p| p.0), Some(start), "the first piece keeps the user's start");
        assert_eq!(got.last().map(|p| p.1), Some(end), "the last piece keeps the user's end");
        for pair in got.windows(2) {
            let ((_, to), (from, _)) = (pair[0], pair[1]);
            assert_eq!(to + 1, from, "a gap or an overlap between {pair:?}");
            assert_eq!(from.rem_euclid(MS_PER_DAY), 0, "a cut off a UTC midnight: {from}");
            let (_, m, d) = civil_from_days(from.div_euclid(MS_PER_DAY));
            assert_eq!((m, d), (1, 1), "a cut that is not 1 January: {from}");
        }
        for (from, to) in &got {
            assert!(from <= to, "an inverted piece {from}..{to} in {got:?}");
        }
    }

    // ─── the rule: WHICH venues may be cut ─────────────────────────────────────────────────────

    /// **A one-shot lane's window is NEVER cut** — the hazard `fetch_split` exists for. Each of
    /// these venues' lanes keys a commit by the request's own bounds (or, for dukascopy, by a
    /// partial chunk's own cut and the recent tail's request bounds), so a per-year request would
    /// store a long window's rows a second time beside a later whole-window request's. Three years
    /// is far past the one-year floor, so only the lane rule can be what keeps each of these one
    /// request.
    #[test]
    fn a_one_shot_lanes_window_is_one_request_however_long() {
        let (start, end) = (day(2021, 7, 1), day(2024, 3, 1));
        for venue in ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "dukascopy"] {
            let plan = plan(venue, start, end);
            assert!(!plan.is_split(), "{venue}'s lane keys by the request, and was cut: {plan:?}");
            assert_eq!(pieces(&plan), vec![(start, end)], "{venue} must send the window as typed");
            assert_eq!(plan.count(), 1, "{venue}");
        }
    }

    /// ...and the day-grid lane IS cut, at the same window. Without this the case above would pass
    /// for a module that never split anything.
    #[test]
    fn the_day_grid_lane_is_cut_into_years() {
        let (start, end) = (day(2021, 7, 1), day(2024, 3, 1));
        let plan = plan("oanda", start, end);
        assert!(plan.is_split(), "oanda's lane stores whole UTC days and was not cut: {plan:?}");
        assert_eq!(
            pieces(&plan),
            vec![
                (start, day(2022, 1, 1) - 1),
                (day(2022, 1, 1), day(2023, 1, 1) - 1),
                (day(2023, 1, 1), day(2024, 1, 1) - 1),
                (day(2024, 1, 1), end),
            ]
        );
        assert_tiles(&plan);
    }

    /// The rule is DERIVED from the history table, never a venue list: across the whole roster, a
    /// venue is cut exactly when the table claims at least one built lane for it and every one it
    /// claims is the day-grid lane. An unknown venue string declares nothing and is one request.
    #[test]
    fn a_venue_is_cut_exactly_when_every_lane_it_claims_stores_whole_days() {
        let mut cut = Vec::new();
        for &venue in VENUES {
            let lanes: Vec<HistoryLane> = history_channels_for(venue)
                .iter()
                .filter_map(|row| match row.state {
                    ChannelState::Built(lane) => Some(lane),
                    ChannelState::Designed(_) => None,
                })
                .collect();
            let expected =
                !lanes.is_empty() && lanes.iter().all(|l| *l == HistoryLane::CredentialedKlines);
            assert_eq!(splits_by_year(venue), expected, "{venue}: lanes {lanes:?}");
            if expected {
                cut.push(venue);
            }
        }
        assert_eq!(cut, vec!["oanda"], "the venues cut today — a new one is a decision, not drift");
        assert!(!splits_by_year("not-a-venue"));
        assert!(!plan("not-a-venue", day(2000, 1, 1), day(2010, 1, 1)).is_split());
    }

    /// `stores_whole_utc_days` answers each lane by its ingest, read in `fetch_split`'s module doc.
    #[test]
    fn only_the_credentialed_lane_stores_whole_days() {
        assert!(stores_whole_utc_days(HistoryLane::CredentialedKlines));
        for lane in [HistoryLane::Klines, HistoryLane::TickBars, HistoryLane::Funding] {
            assert!(!stores_whole_utc_days(lane), "{lane:?} keys by the request");
        }
    }

    // ─── the cut: arithmetic ───────────────────────────────────────────────────────────────────

    /// A window of one year or less is never cut, even when it crosses a 1 January — a split exists
    /// to report progress on a LONG fetch, and a year is the unit an operator ran by hand before it.
    #[test]
    fn a_year_or_less_is_one_piece_even_across_new_year() {
        // Exactly one calendar year, crossing 2024-01-01.
        let exact = Plan::by_year(day(2023, 3, 15), day(2024, 3, 15));
        assert!(!exact.is_split());
        assert_eq!(pieces(&exact), vec![(day(2023, 3, 15), day(2024, 3, 15))]);
        // Two months across new year.
        assert!(!Plan::by_year(day(2023, 12, 1), day(2024, 2, 1)).is_split());
        // A whole calendar year written the way the operator page writes it.
        assert!(!Plan::by_year(day(2024, 1, 1), day(2024, 12, 31)).is_split());
        // ...and with `--to` on the next year's first day.
        assert!(!Plan::by_year(day(2024, 1, 1), day(2025, 1, 1)).is_split());
        // One millisecond past a year IS longer than a year.
        let over = Plan::by_year(day(2023, 3, 15), day(2024, 3, 15) + 1);
        assert!(over.is_split());
        assert_eq!(
            pieces(&over),
            vec![(day(2023, 3, 15), day(2024, 1, 1) - 1), (day(2024, 1, 1), day(2024, 3, 15) + 1)]
        );
    }

    /// A window ending EXACTLY on 1 January 00:00 keeps that instant in its last piece: the cut
    /// points are strictly inside the window, so no one-millisecond piece is minted for it.
    #[test]
    fn a_window_ending_on_new_year_keeps_that_instant_in_its_last_piece() {
        let plan = Plan::by_year(day(2023, 1, 1), day(2025, 1, 1));
        assert_eq!(plan.count(), 2);
        assert_eq!(
            pieces(&plan),
            vec![(day(2023, 1, 1), day(2024, 1, 1) - 1), (day(2024, 1, 1), day(2025, 1, 1))]
        );
        assert_tiles(&plan);
    }

    /// A window STARTING on a 1 January cuts at the NEXT one — a cut at the start would be an
    /// inverted first piece.
    #[test]
    fn a_window_starting_on_new_year_cuts_at_the_next_one() {
        let plan = Plan::by_year(day(2020, 1, 1), day(2021, 6, 1));
        assert_eq!(
            pieces(&plan),
            vec![(day(2020, 1, 1), day(2021, 1, 1) - 1), (day(2021, 1, 1), day(2021, 6, 1))]
        );
        assert_tiles(&plan);
    }

    /// The first and last pieces keep the operator's bounds to the MILLISECOND — a `--days` window
    /// starts and ends mid-day, and the outer edges must be what one request would have sent.
    #[test]
    fn the_outer_pieces_keep_mid_day_bounds() {
        let start = day(2019, 5, 17) + 13 * 3_600_000 + 1_234;
        let end = day(2021, 8, 2) + 22 * 3_600_000 + 59_999;
        let plan = Plan::by_year(start, end);
        let got = pieces(&plan);
        assert_eq!(got.len(), 3, "{got:?}");
        assert_eq!(got[0], (start, day(2020, 1, 1) - 1));
        assert_eq!(got[2], (day(2021, 1, 1), end));
        assert_tiles(&plan);
    }

    /// Leap years: a leap year's piece is 366 days, and "one year after 29 February" is 1 March.
    #[test]
    fn leap_years_are_cut_and_measured_by_the_calendar() {
        let plan = Plan::by_year(day(2023, 6, 1), day(2025, 6, 1));
        let got = pieces(&plan);
        assert_eq!(got[1], (day(2024, 1, 1), day(2025, 1, 1) - 1));
        assert_eq!((got[1].1 + 1 - got[1].0) / MS_PER_DAY, 366, "2024 is a leap year");
        assert_tiles(&plan);

        // From 29 February, one year later is 1 March — so a window to that instant is NOT longer
        // than a year, and one millisecond more is.
        let feb29 = day(2024, 2, 29) + 12 * 3_600_000;
        let mar1 = day(2025, 3, 1) + 12 * 3_600_000;
        assert!(!Plan::by_year(feb29, mar1).is_split());
        assert!(Plan::by_year(feb29, mar1 + 1).is_split());
    }

    /// Before 1970 the calendar is the same calendar: the cut uses `div_euclid`, so a negative
    /// instant finds its year like any other.
    #[test]
    fn a_pre_1970_window_is_cut_at_its_own_new_years() {
        let plan = Plan::by_year(day(1968, 6, 1), day(1970, 6, 1));
        assert_eq!(
            pieces(&plan),
            vec![
                (day(1968, 6, 1), day(1969, 1, 1) - 1),
                (day(1969, 1, 1), day(1970, 1, 1) - 1),
                (day(1970, 1, 1), day(1970, 6, 1)),
            ]
        );
        assert_tiles(&plan);
    }

    /// The whole of OANDA's dense 5-second history, the case this exists for: 2005-01-03 to a day
    /// in 2026 is 22 requests, the first and last ragged and the twenty between them whole years.
    #[test]
    fn the_whole_oanda_history_is_twenty_two_requests() {
        let plan = plan("oanda", day(2005, 1, 3), day(2026, 9, 29));
        assert_eq!(plan.count(), 22);
        let got = pieces(&plan);
        assert_eq!(got[0], (day(2005, 1, 3), day(2006, 1, 1) - 1));
        assert_eq!(got[21], (day(2026, 1, 1), day(2026, 9, 29)));
        assert_tiles(&plan);
    }

    /// A sweep of windows, including ragged edges on both sides and every relation to new year,
    /// holds every tiling property — and `Plan::count`'s arithmetic agrees with the walk on each.
    #[test]
    fn every_window_in_a_sweep_tiles_its_range() {
        let offsets = [0, 1, MS_PER_DAY - 1, 40 * MS_PER_DAY + 7, 200 * MS_PER_DAY];
        for start_year in [1969, 1999, 2004, 2020] {
            for years in 0..4 {
                for &a in &offsets {
                    for &b in &offsets {
                        let start = day(start_year, 1, 1) + a;
                        let end = day(start_year + years, 1, 1) + b;
                        if start > end {
                            continue;
                        }
                        assert_tiles(&Plan::by_year(start, end));
                        assert_tiles(&Plan::whole(start, end));
                    }
                }
            }
        }
    }

    /// The edges of `i64` neither panic nor overflow: a window at the very top cannot be longer
    /// than a year past its start, so it is one piece; a window across all of `i64` counts without
    /// walking and yields its first pieces lazily.
    #[test]
    fn the_edges_of_i64_neither_panic_nor_allocate() {
        let top = Plan::by_year(i64::MAX - 10, i64::MAX);
        assert_eq!(pieces(&top), vec![(i64::MAX - 10, i64::MAX)]);
        let all = Plan::by_year(i64::MIN, i64::MAX);
        assert!(all.is_split());
        assert!(all.count() > 500_000_000, "{}", all.count());
        let first: Vec<_> = all.pieces().take(3).collect();
        assert_eq!(first[0].0, i64::MIN);
        assert_eq!(first[0].1 + 1, first[1].0);
    }

    // ─── the run: stop at the first failure ────────────────────────────────────────────────────

    fn done(rows: u64, first: Option<i64>, last: Option<i64>) -> BackfillDone {
        BackfillDone { rows_written: rows, first_ts: first, last_ts: last }
    }

    fn three_years() -> Plan {
        Plan::by_year(day(2021, 7, 1), day(2023, 3, 1))
    }

    /// Every piece answers: the requests go out in order with each piece's own bounds, one progress
    /// line follows the header per piece, and the merge reads like one request's answer.
    #[test]
    fn every_piece_is_sent_in_order_and_reported() {
        let plan = three_years();
        let sent = RefCell::new(Vec::new());
        let lines = RefCell::new(Vec::new());
        let out = run_pieces(
            &plan,
            "HEADER",
            |from, to| {
                sent.borrow_mut().push((from, to));
                Ok(match sent.borrow().len() {
                    1 => done(184, Some(from + 5), Some(to - 5)),
                    // An empty year: nothing stored in its range.
                    2 => done(0, None, None),
                    _ => done(60, Some(from + 9), Some(to - 9)),
                })
            },
            |line| lines.borrow_mut().push(line.to_string()),
        )
        .expect("every piece answered");

        assert_eq!(*sent.borrow(), pieces(&plan));
        assert_eq!(out.rows_written, 244);
        assert_eq!(out.first_ts, Some(day(2021, 7, 1) + 5), "the earliest piece that held rows");
        assert_eq!(out.last_ts, Some(day(2023, 3, 1) - 9), "the latest piece that held rows");
        assert_eq!(out.pieces.len(), 3);

        let lines = lines.borrow();
        assert_eq!(lines.len(), 4, "the header, then one line per piece: {lines:?}");
        assert_eq!(lines[0], "HEADER");
        for (line, want) in lines[1..].iter().zip([
            "  1/3 [2021-07-01 .. 2021-12-31]: 184 rows written in ",
            "  2/3 [2022-01-01 .. 2022-12-31]: 0 rows written in ",
            "  3/3 [2023-01-01 .. 2023-03-01]: 60 rows written in ",
        ]) {
            assert!(line.starts_with(want), "{line:?} is not {want:?}…");
        }
    }

    /// **A failed piece STOPS the run.** Piece 2 of 3 fails: piece 3 is never sent, no line is
    /// printed for the failed piece, and the failure names the piece, the rows the pieces before it
    /// wrote and the datahub's own text. A run that carried on past a failure would report a whole
    /// window while leaving a year out of it — and would send requests after the venue or the store
    /// had said no.
    #[test]
    fn a_failed_piece_stops_the_run_and_names_what_is_stored() {
        let plan = three_years();
        let sent = RefCell::new(Vec::new());
        let lines = RefCell::new(Vec::new());
        let failed = run_pieces(
            &plan,
            "HEADER",
            |from, to| {
                sent.borrow_mut().push((from, to));
                match sent.borrow().len() {
                    1 => Ok(done(184, Some(from), Some(to))),
                    _ => Err("backfill oanda/EUR_USD@1h failed: chunk 166 of 365 failed".into()),
                }
            },
            |line| lines.borrow_mut().push(line.to_string()),
        )
        .expect_err("piece 2 failed");

        let sent = sent.borrow();
        assert_eq!(sent.len(), 2, "piece 3 was sent after piece 2 failed: {sent:?}");
        assert_eq!(lines.borrow().len(), 2, "the header and piece 1 only: {:?}", lines.borrow());
        assert_eq!((failed.index, failed.count), (2, 3));
        assert_eq!((failed.from, failed.to), (day(2022, 1, 1), day(2023, 1, 1) - 1));
        assert_eq!(failed.rows_before, 184);

        let msg = failed.message("oanda");
        assert!(msg.contains("piece 2 of 3 [2022-01-01 .. 2022-12-31] failed"), "{msg}");
        assert!(msg.contains("the piece before it wrote 184 rows, which stay stored"), "{msg}");
        assert!(msg.contains("Re-run the same command to resume"), "{msg}");
        assert!(msg.contains("chunk 166 of 365 failed"), "the datahub's own text survives: {msg}");
    }

    /// A first piece that fails is said to be the first, rather than claiming rows were written.
    #[test]
    fn a_failed_first_piece_says_nothing_was_written_before_it() {
        let failed = run_pieces(&three_years(), "H", |_, _| Err("refused".to_string()), |_| {})
            .expect_err("piece 1 failed");
        assert_eq!((failed.index, failed.rows_before), (1, 0));
        let msg = failed.message("oanda");
        assert!(msg.contains("piece 1 of 3"), "{msg}");
        assert!(msg.contains("it was the first, so this run wrote nothing before it"), "{msg}");
        assert!(msg.ends_with("refused"), "{msg}");
    }

    /// A later failure counts every piece before it, by number and by rows.
    #[test]
    fn a_later_failure_counts_every_piece_before_it() {
        let plan = Plan::by_year(day(2019, 1, 1), day(2023, 6, 1));
        let mut n = 0;
        let failed = run_pieces(
            &plan,
            "H",
            |_, _| {
                n += 1;
                if n < 4 { Ok(done(10, None, None)) } else { Err("x".into()) }
            },
            |_| {},
        )
        .expect_err("piece 4 failed");
        assert_eq!((failed.index, failed.count, failed.rows_before), (4, 5, 30));
        assert!(failed.message("oanda").contains("the 3 pieces before it wrote 30 rows"));
    }

    // ─── the lines ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn elapsed_reads_the_way_an_operator_reads_a_long_fetch() {
        assert_eq!(elapsed(Duration::from_millis(0)), "0.0s");
        assert_eq!(elapsed(Duration::from_millis(4_240)), "4.2s");
        assert_eq!(elapsed(Duration::from_secs(59)), "59.0s");
        assert_eq!(elapsed(Duration::from_secs(60)), "1m00s");
        assert_eq!(elapsed(Duration::from_secs(252)), "4m12s");
        assert_eq!(elapsed(Duration::from_secs(3_600)), "1h00m");
        assert_eq!(elapsed(Duration::from_secs(5_430)), "1h30m");
    }

    #[test]
    fn the_header_names_the_window_the_count_and_why_the_cut_is_safe() {
        let plan = plan("oanda", day(2005, 1, 3), day(2026, 9, 29));
        let line = header_line("oanda:EUR_USD:5s", "oanda", &plan);
        let want = "fetching oanda:EUR_USD:5s [2005-01-03 .. 2026-09-29] as 22 requests";
        assert!(line.starts_with(want), "{line}");
        assert!(line.contains("one per calendar year"), "{line}");
        assert!(line.contains("stores whole UTC days"), "{line}");
    }

    #[test]
    fn a_progress_line_carries_the_window_the_rows_and_the_time() {
        let piece = PieceDone {
            from: day(2006, 1, 1),
            to: day(2007, 1, 1) - 1,
            done: done(3_012_345, None, None),
            elapsed: Duration::from_secs(252),
        };
        assert_eq!(
            progress_line(2, 22, &piece),
            "  2/22 [2006-01-01 .. 2006-12-31]: 3012345 rows written in 4m12s"
        );
    }
}
