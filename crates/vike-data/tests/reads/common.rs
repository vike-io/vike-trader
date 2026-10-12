//! Helpers shared by two or more modules of the `reads` group: the throwaway store a test opens, and
//! the range grid (with its time units) of the awkward series `bars_head.rs` and `research_head.rs`
//! each plant. Only code that was IDENTICAL in every file that defined it lives here; the helpers
//! that merely look alike (`plant`, `judge`, `grid`, `last_day_part`, `spoil`) stay in their files.

use vike_data::{DataFusionHist, TsRange};

pub(crate) const HOUR_MS: i64 = 3_600_000;
pub(crate) const DAY_MS: i64 = 86_400_000;

pub(crate) fn open() -> (tempfile::TempDir, DataFusionHist) {
    let dir = tempfile::tempdir().unwrap();
    let store = DataFusionHist::open(dir.path()).unwrap();
    (dir, store)
}

/// Every range the grid asks about — `bars_head.rs`'s over bars and `research_head.rs`'s over each
/// research kind, the same awkward series in both. Several START inside a part, which is the shape
/// that separates a head that keeps reading from one that stops after a single block.
pub(crate) fn ranges() -> Vec<TsRange> {
    let h = HOUR_MS;
    vec![
        TsRange::all(),
        // From day 0's last two timestamps on: the first block is day 0's whole part, which holds
        // two of them — short of most counts while the range goes on.
        TsRange { start: Some(20 * h), end: None },
        // Starts inside day 1's re-fetched window, ends inside day 3.
        TsRange::of(DAY_MS + 9 * h, 3 * DAY_MS + 5 * h),
        // Starts ON the timestamp day 2 stores twice.
        TsRange { start: Some(2 * DAY_MS + 4 * h), end: None },
        // Starts and ends inside one part.
        TsRange::of(DAY_MS + 7 * h, DAY_MS + 15 * h),
        TsRange { start: None, end: Some(2 * DAY_MS + 5 * h) },
        // Overlaps a part and holds none of its rows.
        TsRange::of(DAY_MS + h, DAY_MS + h + h / 2),
        // Only the last part, and past every part.
        TsRange { start: Some(3 * DAY_MS), end: None },
        TsRange::of(5 * DAY_MS, 6 * DAY_MS),
    ]
}
