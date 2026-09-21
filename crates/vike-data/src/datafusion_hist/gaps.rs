//! Per-series GAP DETECTION: the manifest's `date=` file index already lists every UTC day a
//! series has a sealed part for, so the set of MISSING days within its `[first,last]` span is
//! derivable read-only, no Parquet scan — the Data Manager's "where's the hole in this series"
//! view (`DataFusionHist::series_gaps`).
//!
//! Split in two: [`find_gaps`] is the pure integer-position gap-finder (no manifest/date knowledge
//! at all), which now lives in [`crate::coverage`] so the UNGATED cross-kind report shares the one
//! implementation; [`parse_utc_date`] is the manifest-date-key parser that feeds it, and stays here
//! because it is store knowledge. [`super::DataFusionHist::series_gaps`] wires the two together over
//! one series' manifest.

use vike_model::time::days_from_civil;

use crate::hist::DataError;
// NOTE the pure integer gap-finder `find_gaps` now lives in the crate base (`crate::coverage`) so
// the UNGATED cross-kind coverage report can use the SAME rule — this module sits inside the
// `hist-datafusion`-gated tree, and a second copy of the rule is exactly the duplication the store's
// dedup discipline forbids. What stays here is what genuinely belongs with the store: manifest-date
// parsing and the per-series wiring.

/// Epoch-ms per UTC day (matches [`vike_model::time::epoch_ms_to_utc_date`]'s day-floor).
pub(super) const DAY_MS: i64 = 86_400_000;

/// Parse a manifest [`super::manifest::FileEntry::date`] key (`"YYYY-MM-DD"`) into days-since-
/// epoch — the exact inverse of [`vike_model::time::epoch_ms_to_utc_date`]'s civil-date half
/// (`days_from_civil ∘ civil_from_days == identity`, proven in `vike_model::time`'s own tests).
/// Manifest dates are always written by `epoch_ms_to_utc_date` itself, so this should never see
/// anything malformed in practice, but a hand-edited or corrupted manifest shouldn't panic —
/// out-of-range/unparseable input is a [`DataError::Query`] instead.
pub(super) fn parse_utc_date(s: &str) -> Result<i64, DataError> {
    let bad = || DataError::Query(format!("bad manifest date {s:?}"));
    let mut parts = s.splitn(3, '-');
    let y: i64 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let m: u32 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    let d: u32 = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(bad());
    }
    Ok(days_from_civil(y, m, d))
}

/// A `(start_day, end_day)` UTC-day gap (as [`find_gaps`] returns) → the inclusive epoch-ms range
/// spanning those whole days, `[00:00:00.000 of start_day, 23:59:59.999 of end_day]` — the same
/// inclusive-bound convention `TsRange`/`FileEntry.ts_min..ts_max` use elsewhere in the store, so
/// callers can plot a gap directly on the same ts timeline as coverage.
pub(super) fn day_gap_to_ms_range(gap: (i64, i64)) -> (i64, i64) {
    let (start_day, end_day) = gap;
    (start_day * DAY_MS, (end_day + 1) * DAY_MS - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::time::epoch_ms_to_utc_date;

    #[test]
    fn parse_utc_date_is_the_inverse_of_epoch_ms_to_utc_date() {
        for ms in [0_i64, DAY_MS, -DAY_MS, 951_782_400_000, 1_609_459_200_000] {
            let s = epoch_ms_to_utc_date(ms);
            assert_eq!(parse_utc_date(&s).unwrap(), ms.div_euclid(DAY_MS));
        }
    }

    #[test]
    fn parse_utc_date_rejects_garbage() {
        assert!(parse_utc_date("not-a-date").is_err());
        assert!(parse_utc_date("2026-13-01").is_err()); // month out of range
        assert!(parse_utc_date("2026-07-32").is_err()); // day out of range
        assert!(parse_utc_date("2026-07-12-extra").is_err()); // trailing segment
    }

    #[test]
    fn day_gap_to_ms_range_spans_the_whole_missing_days() {
        // day 3 -> epoch-ms 3*DAY_MS = 1970-01-04T00:00:00.000
        assert_eq!(day_gap_to_ms_range((3, 3)), (3 * DAY_MS, 4 * DAY_MS - 1));
        assert_eq!(day_gap_to_ms_range((3, 4)), (3 * DAY_MS, 5 * DAY_MS - 1));
    }
}
