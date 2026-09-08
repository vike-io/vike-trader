//! Volume-at-price profile with POC + 70% value area.
use std::collections::BTreeMap;

use crate::classify::signed;
use crate::footprint::FootprintBar;
use vike_model::TradeTick;

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

/// Price → tick-sized bucket index. The one rounding site for all price bucketing in this
/// crate; `footprint` reuses this rather than re-deriving it.
pub(crate) fn bucket(price: f64, tick_size: f64) -> i64 {
    (price / tick_size).round() as i64
}

/// Shared grid→bins transform: (sorted bucket->(buy,sell)) → ascending `PriceBin`s.
/// Reused by `footprint` for its per-bar cell grid (same shape, different name at the call
/// site — `cells` there, `bins` here).
pub(crate) fn grid_to_cells(map: &BTreeMap<i64, (f64, f64)>, tick_size: f64) -> Vec<PriceBin> {
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

pub struct VolumeProfileBuilder {
    tick_size: f64,
    map: BTreeMap<i64, (f64, f64)>,
}

impl VolumeProfileBuilder {
    pub fn new(tick_size: f64) -> Self {
        assert!(tick_size > 0.0, "tick_size must be > 0");
        VolumeProfileBuilder { tick_size, map: BTreeMap::new() }
    }

    pub fn push(&mut self, t: &TradeTick) {
        if t.size <= 0.0 {
            return;
        }
        let (b, s) = signed(t);
        let e = self.map.entry(bucket(t.price, self.tick_size)).or_insert((0.0, 0.0));
        e.0 += b;
        e.1 += s;
    }

    pub fn finish(&self) -> VolumeProfile {
        build(self.map.clone(), self.tick_size)
    }

    pub fn from_trades(trades: &[TradeTick], tick_size: f64) -> VolumeProfile {
        assert!(tick_size > 0.0);
        // separate path: collect then reduce
        let mut map: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
        for t in trades.iter().filter(|t| t.size > 0.0) {
            let (b, s) = signed(t);
            let e = map.entry(bucket(t.price, tick_size)).or_insert((0.0, 0.0));
            e.0 += b;
            e.1 += s;
        }
        build(map, tick_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::footprint::FootprintBuilder;

    fn tk(price: f64, size: f64, ibm: bool) -> TradeTick {
        TradeTick { ts: 0, local_ts: 0, price, size, is_buyer_maker: ibm, symbol: String::new() }
    }

    // tick 1.0. Prices: 100(buy5), 101(sell10), 102(buy3), 99(buy1). total=19, POC=101.
    fn seq() -> Vec<TradeTick> {
        vec![
            tk(100.0, 5.0, false),
            tk(101.0, 10.0, true),
            tk(102.0, 3.0, false),
            tk(99.0, 1.0, false),
        ]
    }

    #[test]
    fn profile_bins_and_poc() {
        let p = VolumeProfileBuilder::from_trades(&seq(), 1.0);
        assert_eq!(p.bins.len(), 4);
        assert_eq!(p.bins[0], PriceBin { price: 99.0, buy_vol: 1.0, sell_vol: 0.0 }); // ascending
        assert_eq!(p.bins[2], PriceBin { price: 101.0, buy_vol: 0.0, sell_vol: 10.0 });
        assert_eq!(p.poc, 101.0);
    }

    #[test]
    fn profile_value_area_covers_70pct_and_contains_poc() {
        let p = VolumeProfileBuilder::from_trades(&seq(), 1.0);
        // total 19, 70% = 13.3. POC 101(10). Add 100(5) → 15 ≥ 13.3. VA = [100,101].
        assert_eq!(p.value_area, (100.0, 101.0));
        assert!(p.value_area.0 <= p.poc && p.poc <= p.value_area.1);
    }

    #[test]
    fn profile_invariants_and_zero_skip() {
        let mut trades = seq();
        trades.push(tk(100.0, 0.0, false)); // skipped
        let p = VolumeProfileBuilder::from_trades(&trades, 1.0);
        let sum: f64 = p.bins.iter().map(|b| b.buy_vol + b.sell_vol).sum();
        assert_eq!(sum, 19.0);
    }

    #[test]
    fn profile_streaming_equals_batch() {
        let batch = VolumeProfileBuilder::from_trades(&seq(), 1.0);
        let mut b = VolumeProfileBuilder::new(1.0);
        for t in &seq() {
            b.push(t);
        }
        assert_eq!(b.finish(), batch);
    }

    #[test]
    fn from_footprints_merges_range_and_matches_direct_builder() {
        let tk = |ts: i64, p: f64, s: f64, ibm: bool| TradeTick {
            ts,
            local_ts: 0,
            price: p,
            size: s,
            is_buyer_maker: ibm,
            symbol: String::new(),
        };
        // three bars of trades; tick_size 1.0
        let bars = vec![
            vec![tk(1, 100.0, 5.0, false), tk(2, 101.0, 3.0, true)],
            vec![tk(3, 101.0, 4.0, false), tk(4, 102.0, 2.0, true)],
            vec![tk(5, 100.0, 1.0, true)],
        ];
        let fps = FootprintBuilder::from_bars(&bars, 1.0);
        // merge ALL three bars == VolumeProfileBuilder over ALL trades
        let merged = VolumeProfile::from_footprints(&fps, 1.0, 0, 2);
        let all: Vec<TradeTick> = bars.iter().flatten().cloned().collect();
        let direct = VolumeProfileBuilder::from_trades(&all, 1.0);
        assert_eq!(merged.bins, direct.bins);
        assert_eq!(merged.poc, direct.poc);
        assert_eq!(merged.value_area, direct.value_area);
        // sub-range [1,2] excludes bar 0's 100@5
        let sub = VolumeProfile::from_footprints(&fps, 1.0, 1, 2);
        let sub_total: f64 = sub.bins.iter().map(|b| b.buy_vol + b.sell_vol).sum();
        assert_eq!(sub_total, 7.0); // 4+2+1
        // empty / out of range
        let empty = VolumeProfile::from_footprints(&fps, 1.0, 5, 9);
        assert_eq!(empty.bins.len(), 0);
        assert_eq!(empty.poc, 0.0);
    }
}
