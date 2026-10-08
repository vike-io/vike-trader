use super::*;

// `whole_buckets` is private, and `crates/vike-backfill/tests/dukascopy_backfill.rs` reaches it only
// through `backfill_quotes_then_bars`, over small positive windows. Two claims its doc makes lie
// outside anything such a window can reach — a pre-1970 stamp rounds like any other, and a window
// at the edge of `i64` answers `None` rather than overflowing — so the helper is pinned directly.

const MINUTE: i64 = 60_000;

/// The three shapes the lane's integration tests drive, stated on the helper itself: on the grid,
/// an unaligned start, an unaligned end — plus a window that is exactly one bucket.
#[test]
fn the_trim_keeps_exactly_the_whole_buckets() {
    assert_eq!(whole_buckets(0, 119_999, MINUTE), Some((0, 119_999)), "on the grid: untouched");
    assert_eq!(whole_buckets(30_000, 119_999, MINUTE), Some((60_000, 119_999)), "start rounds UP");
    assert_eq!(whole_buckets(0, 90_000, MINUTE), Some((0, 59_999)), "end rounds DOWN");
    assert_eq!(whole_buckets(60_000, 119_999, MINUTE), Some((60_000, 119_999)), "one exact bucket");
}

/// A window holding no whole bucket answers `None` — whether it sits inside one bucket, straddles a
/// boundary without covering either side, or stops one millisecond short of its first bucket's end.
#[test]
fn a_window_holding_no_whole_bucket_answers_none() {
    assert_eq!(whole_buckets(10_000, 50_000, MINUTE), None, "inside one bucket");
    assert_eq!(
        whole_buckets(30_000, 89_999, MINUTE),
        None,
        "across a boundary, whole on neither side"
    );
    assert_eq!(whole_buckets(0, 59_998, MINUTE), None, "one millisecond short of the first bucket");
}

/// ⚠ A pre-1970 stamp rounds the same way as any other, which is why the helper uses `rem_euclid`
/// rather than `%`: `%` takes the SIGN of the dividend. Under `%` the start `-90_000` would round up
/// to `0` rather than `-60_000`, losing the whole `[-60_000, -1]` bucket, and the exclusive end
/// `-30_000` would move up to `0` rather than down to `-60_000`, keeping the partial
/// `[-60_000, -30_001]` one.
#[test]
fn a_pre_1970_window_rounds_like_any_other() {
    assert_eq!(whole_buckets(-90_000, -1, MINUTE), Some((-60_000, -1)));
    assert_eq!(whole_buckets(-120_000, -30_001, MINUTE), Some((-120_000, -60_001)));
}

/// A window at the edge of `i64` answers `None` instead of overflowing: unchecked arithmetic would
/// panic in a debug build and wrap into a nonsense range in a release one.
#[test]
fn a_window_at_the_edge_of_i64_answers_none_instead_of_overflowing() {
    assert_eq!(whole_buckets(i64::MAX - 10, i64::MAX, MINUTE), None, "the start cannot round up");
    assert_eq!(whole_buckets(i64::MIN, i64::MIN + 10, MINUTE), None, "the end cannot round down");
}

// ── the chunk grid ─────────────────────────────────────────────────────────────────────────────
//
// `chunk_len`, `chunk_windows` and `settled` are pinned directly for the reason `whole_buckets` is:
// the lane's integration tests reach them only at `1m`, over small positive windows or well clear
// of the margin's edge, while their docs make claims about other steps, pre-1970 stamps, the edges
// of `i64` and the margin's exact boundary.

/// A chunk is the most whole bars that fit in a UTC day — exactly a day when the step divides one,
/// a little less when it does not — and one bar when a bar is longer than a day.
#[test]
fn a_chunk_is_the_most_whole_bars_that_fit_in_a_day() {
    assert_eq!(chunk_len(MINUTE), MS_PER_DAY, "1m divides a day");
    assert_eq!(chunk_len(MS_PER_DAY), MS_PER_DAY, "1d is one bar");
    assert_eq!(chunk_len(7 * MINUTE), 205 * 7 * MINUTE, "7m does not divide a day: 205 bars fit");
    assert_eq!(chunk_len(2 * MS_PER_DAY), 2 * MS_PER_DAY, "a bar longer than a day is one chunk");
}

/// A window is cut at every chunk edge inside it and nowhere else: a window inside one chunk comes
/// back whole, one reaching a chunk's first millisecond gains a one-millisecond last piece, one
/// crossing two edges comes back in three — and an inverted window is passed through whole, which
/// is what keeps it byte-identical to the one fetch it always made.
#[test]
fn a_window_is_cut_at_every_chunk_edge_inside_it_and_nowhere_else() {
    let cut = |start, end| chunk_windows(start, end, MS_PER_DAY).collect::<Vec<_>>();
    assert_eq!(cut(30_000, 119_999), vec![(30_000, 119_999)], "inside one chunk");
    assert_eq!(cut(0, MS_PER_DAY - 1), vec![(0, MS_PER_DAY - 1)], "exactly one chunk");
    assert_eq!(
        cut(0, MS_PER_DAY),
        vec![(0, MS_PER_DAY - 1), (MS_PER_DAY, MS_PER_DAY)],
        "one millisecond into the next chunk"
    );
    assert_eq!(
        cut(30_000, 2 * MS_PER_DAY + 90_000),
        vec![
            (30_000, MS_PER_DAY - 1),
            (MS_PER_DAY, 2 * MS_PER_DAY - 1),
            (2 * MS_PER_DAY, 2 * MS_PER_DAY + 90_000),
        ],
        "across two edges"
    );
    assert_eq!(cut(119_999, 30_000), vec![(119_999, 30_000)], "an inverted window");
}

/// ⚠ A pre-1970 window is cut like any other, which is why the cut uses `rem_euclid`: `%` takes the
/// SIGN of the dividend, so under it the start `-MS_PER_DAY - 30_000` would sit `-30_000` into its
/// chunk rather than 30 s short of that chunk's end. Its chunk would then seem to end at `-1`, and
/// the window would come back whole — uncut at the `-MS_PER_DAY` edge inside it.
#[test]
fn a_pre_1970_window_is_cut_like_any_other() {
    assert_eq!(
        chunk_windows(-MS_PER_DAY - 30_000, -MS_PER_DAY + 30_000, MS_PER_DAY).collect::<Vec<_>>(),
        vec![(-MS_PER_DAY - 30_000, -MS_PER_DAY - 1), (-MS_PER_DAY, -MS_PER_DAY + 30_000)]
    );
}

/// A chunk takes the shared keys only once its last millisecond is MORE than the publication margin
/// old: at exactly the margin it is still recent, and so is a chunk ending now or later. Pinned
/// here, against a fixed `now`, because the lane itself reads the wall clock — its integration test
/// can only stand well clear of this edge, never on it.
#[test]
fn a_chunk_is_settled_only_once_it_ends_more_than_the_margin_ago() {
    let now = 10 * MS_PER_DAY;
    assert!(settled(now - PUBLICATION_MARGIN_MS - 1, now), "one millisecond past the margin");
    assert!(!settled(now - PUBLICATION_MARGIN_MS, now), "exactly the margin: still recent");
    assert!(!settled(now, now), "ending now");
    assert!(!settled(now + MS_PER_DAY, now), "ending tomorrow");
}

/// A window at the edge of `i64` is cut without overflowing. At the top, the last chunk starting
/// below `i64::MAX` would END past it, so its piece saturates to the window's own end — and the cut
/// stops there rather than stepping to `i64::MAX + 1`. At the bottom, the chunk holding `i64::MIN`
/// STARTS below it, which the cut never computes: it works from each piece's end.
#[test]
fn a_window_at_the_edge_of_i64_is_cut_without_overflowing() {
    let last_edge = i64::MAX - i64::MAX.rem_euclid(MS_PER_DAY);
    assert_eq!(
        chunk_windows(last_edge - 10, i64::MAX, MS_PER_DAY).collect::<Vec<_>>(),
        vec![(last_edge - 10, last_edge - 1), (last_edge, i64::MAX)]
    );
    assert_eq!(
        chunk_windows(i64::MIN, i64::MIN + 10, MS_PER_DAY).collect::<Vec<_>>(),
        vec![(i64::MIN, i64::MIN + 10)]
    );
}

// ── the archive half: the day table, the key grammar, and the HTTP lane's pieces ───────────────
//
// `crates/vike-backfill/tests/dukascopy_archive.rs` drives these through a real store; the claims
// pinned here are the ones a store-backed test reaches only one row at a time — the table's ORDER,
// a key that does not parse, another symbol's key, the margin's exact edge, and the cut of a chunk
// the archive holds part of.

/// 2024-01-15 00:00 UTC.
const DAY: i64 = 19_737 * MS_PER_DAY;
const DAY_END: i64 = DAY + MS_PER_DAY - 1;
/// Long after [`DAY`], so it is settled unless a test says otherwise.
const LATER: i64 = DAY + 400 * MS_PER_DAY;

fn owners(keys: &[String]) -> DayOwners {
    DayOwners::parse("EURUSD", keys)
}

/// The archive's key is the HTTP lane's shape in a namespace of its own, and the parser reads the
/// three lanes' heads off their formatters — so a template edited in one place is read in one place.
#[test]
fn the_archive_key_has_the_canonical_shape_and_each_head_is_read_off_its_formatter() {
    assert_eq!(
        archive_quote_commit_key("EURUSD", 0, 86_399_999),
        "dukascopy-archive:EURUSD:0-86399999"
    );
    assert_eq!(key_head(quote_commit_key, "EURUSD"), "dukascopy:EURUSD:");
    assert_eq!(key_head(provisional_quote_commit_key, "EURUSD"), "dukascopy-provisional:EURUSD:");
    assert_eq!(key_head(archive_quote_commit_key, "EURUSD"), "dukascopy-archive:EURUSD:");
}

/// Bounds parse on either side of 1970 and refuse anything else — which the table then reads as
/// meeting every day.
#[test]
fn a_keys_bounds_parse_across_1970_and_garbage_parses_to_none() {
    assert_eq!(parse_bounds("1-2"), Some((1, 2)));
    assert_eq!(parse_bounds("-5-10"), Some((-5, 10)));
    assert_eq!(parse_bounds("-10--5"), Some((-10, -5)));
    for garbage in ["", "1", "1-", "-1", "a-b", "1-2-3", "1:2", "*-*"] {
        assert_eq!(parse_bounds(garbage), None, "{garbage:?}");
    }
}

/// THE TABLE'S ORDER, row by row: each row is put beside the row below it that would also apply, and
/// must win — so a recent day is never imported and a held day is never reported as overlapped.
#[test]
fn the_day_table_is_checked_in_order_and_the_first_row_that_applies_decides() {
    let archive = archive_quote_commit_key("EURUSD", DAY, DAY_END);
    let http = quote_commit_key("EURUSD", DAY, DAY_END);
    let provisional = provisional_quote_commit_key("EURUSD", DAY, DAY_END);
    let ragged = quote_commit_key("EURUSD", DAY + 1_000, DAY_END);

    // TOO RECENT beats a held day: a day ending inside the margin is never even looked up.
    let recent_now = DAY_END + PUBLICATION_MARGIN_MS;
    assert_eq!(
        owners(std::slice::from_ref(&archive)).classify(DAY, recent_now),
        ArchiveDayClass::TooRecent
    );
    // ...and the margin's edge is the HTTP lane's own `settled`: one millisecond later it is not.
    assert_eq!(owners(&[]).classify(DAY, recent_now + 1), ArchiveDayClass::Free);
    // HELD by the archive beats HELD by the HTTP lane, and both beat the provisional twin each
    // settled commit pre-spends beside them.
    let both = [archive.clone(), http.clone(), provisional.clone()];
    assert_eq!(owners(&both).classify(DAY, LATER), ArchiveDayClass::HeldByArchive);
    assert_eq!(
        owners(&[http.clone(), provisional.clone()]).classify(DAY, LATER),
        ArchiveDayClass::HeldByHttp
    );
    // HELD by the HTTP lane beats an overlap: its canonical day key plus a ragged edge is held.
    assert_eq!(owners(&[http, ragged.clone()]).classify(DAY, LATER), ArchiveDayClass::HeldByHttp);
    // SUPERSEDE only when the provisional day key is the ONLY key meeting the day...
    assert_eq!(
        owners(std::slice::from_ref(&provisional)).classify(DAY, LATER),
        ArchiveDayClass::Supersede { key: provisional.clone() }
    );
    // ...and OVERLAPPED the moment anything else meets it too.
    assert_eq!(
        owners(&[provisional.clone(), ragged.clone()]).classify(DAY, LATER),
        ArchiveDayClass::Overlapped { keys: vec![provisional, ragged] }
    );
}

/// What MEETS a day and what does not: a millisecond of overlap at either edge meets it; a key ending
/// the millisecond before or starting the millisecond after does not; a provisional window cut other
/// than to the day overlaps rather than supersedes; a key whose bounds do not parse meets EVERY day;
/// and another symbol's or another producer's key owns no day of this series.
#[test]
fn a_day_is_met_by_any_shared_millisecond_and_by_nothing_else() {
    let at_start = quote_commit_key("EURUSD", DAY - 5_000, DAY);
    let at_end = provisional_quote_commit_key("EURUSD", DAY_END, DAY_END + 5_000);
    let before = quote_commit_key("EURUSD", DAY - MS_PER_DAY, DAY - 1);
    let after = archive_quote_commit_key("EURUSD", DAY_END + 1, DAY_END + MS_PER_DAY);
    let other_cut = provisional_quote_commit_key("EURUSD", DAY, DAY + 3_600_000);
    let unparsed = "dukascopy:EURUSD:yesterday".to_string();
    let other_symbol = quote_commit_key("EURUSDX", DAY, DAY_END);
    let other_producer = format!("live-dukascopy-EURUSD-q-{DAY}-{DAY_END}-0");

    for key in [&at_start, &at_end, &other_cut, &unparsed] {
        assert!(
            matches!(
                owners(std::slice::from_ref(key)).classify(DAY, LATER),
                ArchiveDayClass::Overlapped { .. }
            ),
            "{key} meets the day"
        );
    }
    for key in [&before, &after, &other_symbol, &other_producer] {
        assert_eq!(
            owners(std::slice::from_ref(key)).classify(DAY, LATER),
            ArchiveDayClass::Free,
            "{key}"
        );
    }
    // An unparsed key meets EVERY day, not just one — the conservative direction.
    let far = DAY + 3_000 * MS_PER_DAY;
    assert!(matches!(
        owners(&[unparsed]).classify(far, far + 400 * MS_PER_DAY),
        ArchiveDayClass::Overlapped { .. }
    ));
}

/// The UTC days a chunk touches that could carry an archive key: the days it spans, none before 1970
/// (no archive day is), none for an inverted window, and no overflow at the edge of `i64`.
#[test]
fn a_chunk_touches_the_utc_days_it_spans_from_1970_on() {
    assert_eq!(archive_days_touched((DAY + 5, DAY_END)), vec![DAY]);
    assert_eq!(archive_days_touched((DAY - 5, DAY + 5)), vec![DAY - MS_PER_DAY, DAY]);
    assert_eq!(archive_days_touched((DAY_END, DAY)), Vec::<i64>::new(), "inverted");
    assert_eq!(archive_days_touched((-MS_PER_DAY, 5)), vec![0], "nothing before 1970");
    let last = archive_days_touched((i64::MAX - 10, i64::MAX));
    assert!(last.iter().all(|day| day.checked_add(MS_PER_DAY - 1).is_some()), "{last:?}");
}

/// The pieces of a chunk the HTTP lane may still fetch: the whole chunk — inverted included — when
/// the archive holds none of its days, what is left either side of the days it holds, and nothing
/// when it holds them all.
#[test]
fn a_chunk_is_cut_around_the_days_the_archive_holds() {
    let (from, to) = (DAY - 3_600_000, DAY + 3_600_000);
    assert_eq!(unowned_pieces((from, to), &[]), vec![(from, to)], "nothing held: the chunk");
    assert_eq!(unowned_pieces((to, from), &[]), vec![(to, from)], "inverted, untouched");
    assert_eq!(unowned_pieces((from, to), &[DAY]), vec![(from, DAY - 1)], "the second day held");
    assert_eq!(
        unowned_pieces((from, to), &[DAY - MS_PER_DAY]),
        vec![(DAY, to)],
        "the first day held"
    );
    assert_eq!(
        unowned_pieces((from, to), &[DAY - MS_PER_DAY, DAY]),
        Vec::<(i64, i64)>::new(),
        "both held"
    );
    // A chunk of a longer interval, with its MIDDLE day held, comes back in two pieces.
    let (start, end) = (DAY, DAY + 3 * MS_PER_DAY - 1);
    assert_eq!(
        unowned_pieces((start, end), &[DAY + MS_PER_DAY]),
        vec![(DAY, DAY_END), (DAY + 2 * MS_PER_DAY, end)]
    );
}

/// What was fetched before an import landed is cut to the pieces left under the lock: a piece the
/// archive now holds whole is dropped, a piece it now holds part of keeps the ticks inside its new
/// bounds, under those bounds.
#[test]
fn fetched_ticks_are_cut_to_the_pieces_left_under_the_lock() {
    let tick = |ts| QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0,
        ask: 1.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: "EURUSD".to_string(),
    };
    let (from, to) = (DAY - 3_600_000, DAY + 3_600_000);
    let fetched = vec![((from, to), vec![tick(from), tick(DAY - 1), tick(DAY), tick(to)])];
    let clipped = clip_to(fetched.clone(), &unowned_pieces((from, to), &[DAY]));
    assert_eq!(clipped.len(), 1);
    assert_eq!(clipped[0].0, (from, DAY - 1));
    assert_eq!(clipped[0].1.iter().map(|q| q.ts).collect::<Vec<_>>(), vec![from, DAY - 1]);
    assert!(clip_to(fetched, &[]).is_empty(), "held whole: nothing left to store");
}
