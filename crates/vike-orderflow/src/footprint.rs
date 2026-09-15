//! Footprint: per-bar, per-price-bucket buy/sell volume grid (cells reuse
//! profile::{PriceBin, bucket, grid_to_cells} — the bucketing and grid→bins transform are
//! owned by `profile`, not re-derived here).
use crate::classify::signed;
use crate::profile::{PriceBin, bucket, grid_to_cells};
use std::collections::BTreeMap;
use vike_model::TradeTick;

#[derive(Clone, PartialEq, Debug)]
pub struct FootprintBar {
    pub bar_index: u64,
    pub cells: Vec<PriceBin>,
}

pub struct FootprintBuilder {
    tick_size: f64,
    cur: BTreeMap<i64, (f64, f64)>,
    next_index: u64,
}

impl FootprintBuilder {
    pub fn new(tick_size: f64) -> Self {
        assert!(tick_size > 0.0, "tick_size must be > 0");
        FootprintBuilder { tick_size, cur: BTreeMap::new(), next_index: 0 }
    }

    pub fn push(&mut self, t: &TradeTick) {
        if t.size <= 0.0 {
            return;
        }
        let (b, s) = signed(t);
        let e = self.cur.entry(bucket(t.price, self.tick_size)).or_insert((0.0, 0.0));
        e.0 += b;
        e.1 += s;
    }

    pub fn close_bar(&mut self) -> FootprintBar {
        let cells = grid_to_cells(&self.cur, self.tick_size);
        self.cur.clear();
        let idx = self.next_index;
        self.next_index += 1;
        FootprintBar { bar_index: idx, cells }
    }

    pub fn from_bars(bars: &[Vec<TradeTick>], tick_size: f64) -> Vec<FootprintBar> {
        assert!(tick_size > 0.0);
        bars.iter()
            .enumerate()
            .map(|(i, trades)| {
                let mut map: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
                for t in trades.iter().filter(|t| t.size > 0.0) {
                    let (b, s) = signed(t);
                    let e = map.entry(bucket(t.price, tick_size)).or_insert((0.0, 0.0));
                    e.0 += b;
                    e.1 += s;
                }
                FootprintBar { bar_index: i as u64, cells: grid_to_cells(&map, tick_size) }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(price: f64, size: f64, ibm: bool) -> TradeTick {
        TradeTick { ts: 0, local_ts: 0, price, size, is_buyer_maker: ibm, symbol: String::new() }
    }

    #[test]
    fn footprint_expected_and_streaming_equals_batch() {
        // bar0: buy 2@100, sell 3@100, buy1@101 ; bar1: sell 4@99
        let bars = vec![
            vec![tk(100.0, 2.0, false), tk(100.0, 3.0, true), tk(101.0, 1.0, false)],
            vec![tk(99.0, 4.0, true)],
        ];
        let batch = FootprintBuilder::from_bars(&bars, 1.0);
        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch[0],
            FootprintBar {
                bar_index: 0,
                cells: vec![
                    PriceBin { price: 100.0, buy_vol: 2.0, sell_vol: 3.0 },
                    PriceBin { price: 101.0, buy_vol: 1.0, sell_vol: 0.0 },
                ]
            }
        );
        assert_eq!(
            batch[1],
            FootprintBar {
                bar_index: 1,
                cells: vec![PriceBin { price: 99.0, buy_vol: 0.0, sell_vol: 4.0 }]
            }
        );
        // streaming drive:
        let mut fb = FootprintBuilder::new(1.0);
        let mut stream = Vec::new();
        for bar in &bars {
            for t in bar {
                fb.push(t);
            }
            stream.push(fb.close_bar());
        }
        assert_eq!(stream, batch);
    }

    #[test]
    fn footprint_cell_sum_matches_bar_and_zero_skip() {
        let bars = vec![vec![tk(100.0, 2.0, false), tk(100.0, 0.0, true), tk(100.0, 3.0, true)]];
        let fp = FootprintBuilder::from_bars(&bars, 1.0);
        let (b, s): (f64, f64) =
            fp[0].cells.iter().fold((0.0, 0.0), |(b, s), c| (b + c.buy_vol, s + c.sell_vol));
        assert_eq!((b, s), (2.0, 3.0)); // zero-size trade skipped
    }
}
