//! Volume-at-price profile with POC + 70% value area, merged from per-bar footprints.
use std::collections::BTreeMap;

use crate::footprint::FootprintBar;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PriceBin {
    pub price: f64,
    pub buy_vol: f64,
    pub sell_vol: f64,
}

#[derive(Clone, PartialEq, Debug)]
pub struct VolumeProfile {
    pub tick_size: f64,
    pub bins: Vec<PriceBin>,
    pub poc: f64,
    pub value_area: (f64, f64),
}

/// Price → tick-sized bucket index.
fn bucket(price: f64, tick_size: f64) -> i64 {
    (price / tick_size).round() as i64
}

/// Grid→bins transform: (sorted bucket->(buy,sell)) → ascending `PriceBin`s.
fn grid_to_cells(map: &BTreeMap<i64, (f64, f64)>, tick_size: f64) -> Vec<PriceBin> {
    map.iter()
        .map(|(&bk, &(b, s))| PriceBin { price: bk as f64 * tick_size, buy_vol: b, sell_vol: s })
        .collect()
}

/// Shared reducer: (sorted bucket->(buy,sell)) → VolumeProfile.
fn build(map: BTreeMap<i64, (f64, f64)>, tick_size: f64) -> VolumeProfile {
    let bins: Vec<PriceBin> = grid_to_cells(&map, tick_size);

    if bins.is_empty() {
        return VolumeProfile { tick_size, bins, poc: 0.0, value_area: (0.0, 0.0) };
    }

    let total: f64 = bins.iter().map(|b| b.buy_vol + b.sell_vol).sum();

    // POC: max total, ties → lower price (bins already ascending; take the first max).
    let mut poc_i = 0usize;
    let mut poc_v = bins[0].buy_vol + bins[0].sell_vol;
    for (i, b) in bins.iter().enumerate().skip(1) {
        let v = b.buy_vol + b.sell_vol;
        if v > poc_v {
            poc_v = v;
            poc_i = i;
        }
    }

    // Value area: greedy one-bucket expansion from POC.
    let (mut lo, mut hi) = (poc_i, poc_i);
    let mut va = poc_v;
    let target = 0.70 * total;
    let vol = |i: usize| bins[i].buy_vol + bins[i].sell_vol;
    while va < target {
        let up = if hi + 1 < bins.len() { Some(vol(hi + 1)) } else { None };
        let dn = if lo > 0 { Some(vol(lo - 1)) } else { None };
        match (up, dn) {
            (Some(u), Some(d)) => {
                if u > d {
                    hi += 1;
                    va += u;
                } else {
                    lo -= 1;
                    va += d;
                }
            } // tie → down (lower price)
            (Some(u), None) => {
                hi += 1;
                va += u;
            }
            (None, Some(d)) => {
                lo -= 1;
                va += d;
            }
            (None, None) => break,
        }
    }

    VolumeProfile {
        tick_size,
        bins: bins.clone(),
        poc: bins[poc_i].price,
        value_area: (bins[lo].price, bins[hi].price),
    }
}

impl VolumeProfile {
    /// Merge the cells of `footprints[lo..=hi]` (clamped to range) into one volume-at-price
    /// profile with POC + 70% value area. `tick_size` must match the footprints' bucketing.
    /// Empty/out-of-range → poc 0.0, value_area (0.0,0.0), empty bins.
    pub fn from_footprints(
        footprints: &[FootprintBar],
        tick_size: f64,
        lo: usize,
        hi: usize,
    ) -> VolumeProfile {
        let mut map: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        let hi = hi.min(footprints.len().saturating_sub(1));
        if footprints.is_empty() || lo > hi {
            return build(map, tick_size); // empty → poc 0, va (0,0)
        }
        for fp in &footprints[lo..=hi] {
            for c in &fp.cells {
                let e = map.entry(bucket(c.price, tick_size)).or_insert((0.0, 0.0));
                e.0 += c.buy_vol;
                e.1 += c.sell_vol;
            }
        }
        build(map, tick_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(price: f64, buy_vol: f64, sell_vol: f64) -> PriceBin {
        PriceBin { price, buy_vol, sell_vol }
    }

    fn bar(bar_index: u64, cells: Vec<PriceBin>) -> FootprintBar {
        FootprintBar { bar_index, cells }
    }

    // One bar, tick 1.0. Prices: 99(buy1), 100(buy5), 101(sell10), 102(buy3). total=19, POC=101.
    fn one_bar() -> Vec<FootprintBar> {
        vec![bar(
            0,
            vec![
                bin(99.0, 1.0, 0.0),
                bin(100.0, 5.0, 0.0),
                bin(101.0, 0.0, 10.0),
                bin(102.0, 3.0, 0.0),
            ],
        )]
    }

    #[test]
    fn profile_bins_and_poc() {
        let p = VolumeProfile::from_footprints(&one_bar(), 1.0, 0, 0);
        assert_eq!(p.bins.len(), 4);
        assert_eq!(p.bins[0], bin(99.0, 1.0, 0.0)); // ascending
        assert_eq!(p.bins[2], bin(101.0, 0.0, 10.0));
        assert_eq!(p.poc, 101.0);
    }

    #[test]
    fn profile_value_area_covers_70pct_and_contains_poc() {
        let p = VolumeProfile::from_footprints(&one_bar(), 1.0, 0, 0);
        // total 19, 70% = 13.3. POC 101(10). Add 100(5) → 15 ≥ 13.3. VA = [100,101].
        assert_eq!(p.value_area, (100.0, 101.0));
        assert!(p.value_area.0 <= p.poc && p.poc <= p.value_area.1);
        let sum: f64 = p.bins.iter().map(|b| b.buy_vol + b.sell_vol).sum();
        assert_eq!(sum, 19.0);
    }

    #[test]
    fn from_footprints_merges_range_and_clamps_out_of_range() {
        // three bars, tick_size 1.0
        let fps = vec![
            bar(0, vec![bin(100.0, 5.0, 0.0), bin(101.0, 0.0, 3.0)]),
            bar(1, vec![bin(101.0, 4.0, 0.0), bin(102.0, 0.0, 2.0)]),
            bar(2, vec![bin(100.0, 0.0, 1.0)]),
        ];
        // merge ALL three bars: 100 = buy5 sell1, 101 = buy4 sell3, 102 = sell2. total 15, 70% =
        // 10.5. POC 101 (7). 102 (2) < 100 (6), so the area grows down: 13 ≥ 10.5, VA = [100,101].
        let merged = VolumeProfile::from_footprints(&fps, 1.0, 0, 2);
        assert_eq!(
            merged.bins,
            vec![bin(100.0, 5.0, 1.0), bin(101.0, 4.0, 3.0), bin(102.0, 0.0, 2.0)]
        );
        assert_eq!(merged.poc, 101.0);
        assert_eq!(merged.value_area, (100.0, 101.0));
        // sub-range [1,2] excludes bar 0's 100@5
        let sub = VolumeProfile::from_footprints(&fps, 1.0, 1, 2);
        let sub_total: f64 = sub.bins.iter().map(|b| b.buy_vol + b.sell_vol).sum();
        assert_eq!(sub_total, 7.0); // 4+2+1
        // `hi` past the end clamps to the last bar
        assert_eq!(VolumeProfile::from_footprints(&fps, 1.0, 1, 9), sub);
        // empty / lo past the end
        let empty = VolumeProfile::from_footprints(&fps, 1.0, 5, 9);
        assert_eq!(empty.bins.len(), 0);
        assert_eq!(empty.poc, 0.0);
        assert_eq!(empty.value_area, (0.0, 0.0));
    }
}
