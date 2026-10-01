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
