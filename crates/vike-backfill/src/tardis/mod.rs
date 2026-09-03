//! Tardis downloadable-CSV datasets backfill — intraday trades/quotes/L2-book (and bars via
//! resample) from `datasets.tardis.dev` into the `vike-data` hist store. A sibling of `pmxt`/`eod`.
//! Design: `docs/superpowers/specs/2026-07-15-databento-tardis-adapters-design.md`.

pub mod client;
pub mod ingest;
pub mod parse;

pub use client::{day_url, fetch_day, DATASETS_BASE};
pub use ingest::{backfill_range, ingest_day_str, TardisKind};

/// Every calendar day from `start` to `end_inclusive` (both `(year, month, day)`), ascending.
/// Thin adapter over the shared [`vike_model::time::days_in_range`] (which owns the civil-calendar
/// math — this crate no longer hand-rolls a `days_in_month` leap-year loop, finding F20), casting
/// the Tardis `(i32, u32, u32)` day tuples to/from the model's `i64` years. Empty when
/// `start > end_inclusive` (the old local copy returned `vec![start]` here — a benign, documented
/// change; the tardis CLI always passes `start <= end`).
pub fn days_in_range(
    start: (i32, u32, u32),
    end_inclusive: (i32, u32, u32),
) -> Vec<(i32, u32, u32)> {
    vike_model::time::days_in_range(
        (start.0 as i64, start.1, start.2),
        (end_inclusive.0 as i64, end_inclusive.1, end_inclusive.2),
    )
    .into_iter()
    .map(|(y, m, d)| (y as i32, m, d))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_in_range_spans_month_and_leap_boundaries() {
        assert_eq!(
            days_in_range((2024, 2, 27), (2024, 3, 1)),
            vec![(2024, 2, 27), (2024, 2, 28), (2024, 2, 29), (2024, 3, 1)]
        );
        assert_eq!(days_in_range((2023, 12, 31), (2024, 1, 1)), vec![(2023, 12, 31), (2024, 1, 1)]);
        assert_eq!(days_in_range((2024, 5, 5), (2024, 5, 5)), vec![(2024, 5, 5)]);
    }
}
