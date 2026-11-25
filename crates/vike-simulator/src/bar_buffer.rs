//! BarSeriesBuffer — shared MTF (multi-timeframe) bar buffer.
//! Exact port of `core/bar_buffer.py` (bisect → `partition_point`).
//!
//! Rust adaptation: Python shares `self.bars` by list reference between engine and buffer;
//! here the buffer OWNS the base-bar Vec and the engine reads through it (one owner, same
//! observable behavior).

use vike_marketdata::{Bar, resample};

use crate::timeframe::parse_timeframe;

pub struct BarSeriesBuffer {
    pub bars: Vec<Bar>,
    /// tf string -> (window ms, resampled coarse bars)
    tf: Vec<(String, i64, Vec<Bar>)>,
}

impl BarSeriesBuffer {
    pub fn new(bars: Vec<Bar>, timeframes: &[String]) -> Self {
        let mut tf = Vec::new();
        for t in timeframes {
            let ms = parse_timeframe(t).expect("valid timeframe");
            let coarse = resample(&bars, ms);
            tf.push((t.clone(), ms, coarse));
        }
        BarSeriesBuffer { bars, tf }
    }

    /// Append a live base bar and refresh higher-TF aggregates (forward mode).
    pub fn add_live_bar(&mut self, bar: Bar) {
        self.bars.push(bar);
        for i in 0..self.tf.len() {
            let ms = self.tf[i].1;
            self.tf[i].2 = resample(&self.bars, ms);
        }
    }

    fn entry(&self, tf: &str) -> &(String, i64, Vec<Bar>) {
        self.tf
            .iter()
            .find(|(name, _, _)| name == tf)
            .unwrap_or_else(|| panic!("timeframe {tf:?} not registered"))
    }

    /// Completed higher-TF bars visible at `now` (deliver-on-complete, no look-ahead):
    /// the coarse list up to (but not including) the window that contains `now`.
    pub fn bars_for(&self, tf: &str, now: i64) -> &[Bar] {
        let (_, ms, coarse) = self.entry(tf);
        let window_start = now - now.rem_euclid(*ms);
        // bisect_left on ts-ascending list == partition_point(ts < window_start)
        let index = coarse.partition_point(|b| b.timestamp < window_start);
        &coarse[..index]
    }
}
