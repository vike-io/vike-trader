//! Timestamp-keyed cross-window sync: bar indices don't align across symbols/intervals; ot does.

use crate::model::Bar;

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct SyncIn {
    pub crosshair_ts: Option<i64>,
    pub range_ts: Option<(i64, i64)>,
}

/// Nearest bar to `ts` by ot (bars ot-ascending). None on empty.
pub fn nearest_index_by_ts(bars: &[Bar], ts: i64) -> Option<usize> {
    if bars.is_empty() {
        return None;
    }
    let p = bars.partition_point(|b| b.ot < ts);
    Some(match (p.checked_sub(1), bars.get(p)) {
        (Some(lo), Some(hi)) => {
            if (ts - bars[lo].ot) <= (hi.ot - ts) {
                lo
            } else {
                p
            }
        }
        (Some(lo), None) => lo,
        (None, _) => 0,
    })
}

/// ts range → x bounds (i0 as f64 - 0.5, i1 as f64 + 0.5), indices clamped, i0<=i1. None on empty.
pub fn ts_range_to_index_bounds(bars: &[Bar], range: (i64, i64)) -> Option<(f64, f64)> {
    let (a, b) = if range.0 <= range.1 { range } else { (range.1, range.0) };
    let i0 = nearest_index_by_ts(bars, a)?;
    let i1 = nearest_index_by_ts(bars, b)?;
    let (i0, i1) = (i0.min(i1), i0.max(i1));
    Some((i0 as f64 - 0.5, i1 as f64 + 0.5))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars(ots: &[i64]) -> Vec<Bar> {
        ots.iter()
            .enumerate()
            .map(|(i, &ot)| Bar { t: i as f64, ot, o: 1.0, h: 2.0, l: 1.0, c: 1.5, v: 1.0 })
            .collect()
    }

    #[test]
    fn nearest_by_ts() {
        let b = bars(&[1000, 2000, 3000]);
        assert_eq!(nearest_index_by_ts(&b, 0), Some(0));
        assert_eq!(nearest_index_by_ts(&b, 1400), Some(0));
        assert_eq!(nearest_index_by_ts(&b, 1600), Some(1));
        assert_eq!(nearest_index_by_ts(&b, 2000), Some(1));
        assert_eq!(nearest_index_by_ts(&b, 99999), Some(2));
        assert_eq!(nearest_index_by_ts(&[], 5), None);
    }

    #[test]
    fn ts_range_to_bounds_clamps() {
        let b = bars(&[1000, 2000, 3000, 4000]);
        assert_eq!(ts_range_to_index_bounds(&b, (2000, 3000)), Some((0.5, 2.5)));
        assert_eq!(ts_range_to_index_bounds(&b, (0, 99999)), Some((-0.5, 3.5)));
        assert_eq!(ts_range_to_index_bounds(&b, (3000, 2000)), Some((0.5, 2.5))); // inverted input normalized
        assert_eq!(ts_range_to_index_bounds(&[], (1, 2)), None);
    }
}
