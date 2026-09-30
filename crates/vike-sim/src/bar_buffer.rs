//! BarSeriesBuffer — shared MTF (multi-timeframe) bar buffer.
//! Exact port of `core/bar_buffer.py` (bisect → `partition_point`).
//!
//! Rust adaptation: Python shares `self.bars` by list reference between engine and buffer;
//! here the buffer OWNS the base-bar Vec and the engine reads through it (one owner, same
//! observable behavior).

use vike_model::Bar;

use crate::timeframe::{parse_timeframe, resample};

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
        let idx = coarse.partition_point(|b| b.ts < window_start);
        &coarse[..idx]
    }

    /// The still-forming coarse bar for `tf` up to `now`, or None if no base bars have
    /// started the current window yet.
    pub fn forming_for(&self, tf: &str, now: i64) -> Option<Bar> {
        let (_, ms, _) = self.entry(tf);
        let window_start = now - now.rem_euclid(*ms);
        let lo = self.bars.partition_point(|b| b.ts < window_start); // bisect_left
        let hi = self.bars.partition_point(|b| b.ts <= now); // bisect_right
        let window = &self.bars[lo..hi];
        if window.is_empty() {
            return None;
        }
        // Python max()/min() fold in order; volume is builtin sum() → Neumaier (py_sum)
        let mut high = f64::NEG_INFINITY;
        let mut low = f64::INFINITY;
        for b in window {
            high = high.max(b.high);
            low = low.min(b.low);
        }
        let vol = vike_model::py_sum(window.iter().map(|b| b.volume));
        Some(Bar {
            ts: window_start,
            open: window[0].open,
            high,
            low,
            close: window[window.len() - 1].close,
            volume: vol,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
    }
}
