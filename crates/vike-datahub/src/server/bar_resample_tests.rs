use super::*;
use vike_datahub_client::refusals::DatahubRefusalId;

const MINUTE: i64 = 60_000;

fn minute_bars(n: i64, from: i64) -> Vec<Bar> {
    (0..n)
        .filter(|i| i % 97 != 13) // a few holes, so a window is not always full
        .map(|i| {
            let close = 100.0 + (i % 41) as f64 * 0.75 + 1.0 / 3.0;
            Bar::ohlcv(
                from + i * MINUTE,
                close - 0.5,
                close + 1.0 + (i % 7) as f64,
                close - 2.0,
                close,
                (i % 9) as f64,
            )
        })
        .collect()
}

/// A store read that keeps `load_bars_head`'s contract: the rows of `window`, cut after `n` (whole
/// timestamps: every row has its own here), and counts how many times it was asked.
fn paged_reader<'a>(
    rows: &'a [Bar],
    asked: &'a mut usize,
) -> impl FnMut(TimestampRange, usize) -> Result<Vec<Bar>, HistoryStoreError> + 'a {
    move |window, n| {
        *asked += 1;
        Ok(rows
            .iter()
            .filter(|b| window.start.is_none_or(|s| b.timestamp >= s))
            .filter(|b| window.end.is_none_or(|e| b.timestamp <= e))
            .take(n)
            .cloned()
            .collect())
    }
}

fn bits(rows: &[Bar]) -> Vec<(i64, [u64; 5])> {
    rows.iter()
        .map(|b| (b.timestamp, [b.open, b.high, b.low, b.close, b.volume].map(f64::to_bits)))
        .collect()
}

/// Whatever the page, a window that spans two (or many) pages is folded ONCE, so the answer is `resample`
/// of the whole range's rows: pages of one row, of a prime, at a window's edge and of the whole range.
#[test]
fn the_answer_is_resample_of_the_whole_range_at_every_page_size() {
    let from = 1_704_067_200_000; // a day boundary
    let rows = minute_bars(3 * 1_440 + 11, from);
    let range = TimestampRange::of(from + 7 * MINUTE + 5, from + 2 * 1_440 * MINUTE + 333 * MINUTE);
    let in_range: Vec<Bar> = rows
        .iter()
        .filter(|b| b.timestamp >= range.start.unwrap() && b.timestamp <= range.end.unwrap())
        .cloned()
        .collect();
    for (to, step) in
        [("5m", 5 * MINUTE), ("15m", 15 * MINUTE), ("1h", 60 * MINUTE), ("1d", 1_440 * MINUTE)]
    {
        let by_hand = resample(&in_range, step);
        for page in [1, 2, 59, 60, 61, 997, 1_440, 1_000_000] {
            let mut asked = 0;
            let got = resampled_bars(
                "1m",
                range,
                None,
                to,
                page,
                usize::MAX,
                paged_reader(&rows, &mut asked),
            )
            .expect("an answer");
            assert!(bits(&got) == bits(&by_hand), "{to} at a page of {page}");
            assert!(asked >= in_range.len().div_ceil(page), "{to} at {page}: every page was read");
        }
    }
}

/// The rows read are bounded, and a read past the bound is refused by name rather than answered short.
#[test]
fn a_range_that_reads_more_rows_than_the_scan_ceiling_is_refused() {
    let rows = minute_bars(1_000, 0);
    let mut asked = 0;
    let why = resampled_bars(
        "1m",
        TimestampRange::of(0, 1_000 * MINUTE),
        None,
        "1h",
        100,
        500,
        paged_reader(&rows, &mut asked),
    )
    .unwrap_err();
    assert_eq!(why.id(), DatahubRefusalId::ResampleOverScan, "{why}");
    assert!(asked <= 6, "stopped at the ceiling, not after the range: {asked} pages");
}

/// What is refused before any read: a `limit`, an open end, a target finer than or not a multiple of the
/// stored interval, a target with no width, and a range of more windows than one reply carries.
#[test]
fn what_cannot_be_answered_whole_is_refused_before_any_read() {
    let day = TimestampRange::of(0, 1_440 * MINUTE);
    let cases: [(&str, TimestampRange, Option<u32>, DatahubRefusalId); 6] = [
        ("1h", day, Some(5), DatahubRefusalId::ResampleWithLimit),
        (
            "1h",
            TimestampRange { start: Some(0), end: None },
            None,
            DatahubRefusalId::ResampleOpenRange,
        ),
        ("90s", day, None, DatahubRefusalId::ResampleNotAMultiple),
        ("30s", day, None, DatahubRefusalId::ResampleNotAMultiple),
        ("1w", day, None, DatahubRefusalId::ResampleUnmeasurable),
        (
            "1m",
            TimestampRange::of(0, RESAMPLED_BARS_CEILING as i64 * MINUTE),
            None,
            DatahubRefusalId::ResampleOverWindows,
        ),
    ];
    for (to, range, limit, id) in cases {
        let mut asked = 0;
        let why =
            resampled_bars("1m", range, limit, to, 100, usize::MAX, paged_reader(&[], &mut asked))
                .unwrap_err();
        assert_eq!(why.id(), id, "{to}: {why}");
        assert_eq!(asked, 0, "{to}: refused before any read");
    }
}

/// The ceiling counts the windows the range TOUCHES, so a range of exactly the ceiling's windows passes.
#[test]
fn a_range_of_exactly_the_ceilings_windows_is_answered() {
    let range = TimestampRange::of(0, RESAMPLED_BARS_CEILING as i64 * MINUTE - 1);
    let mut asked = 0;
    assert!(
        resampled_bars("1m", range, None, "1m", 100, usize::MAX, paged_reader(&[], &mut asked))
            .is_ok()
    );
}
