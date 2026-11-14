// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/matrix-bool-mask-no-transpose-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when RSI(14) is above 55, at least 6 cells of a 24 by 7 mask of UTC hour and weekday are hot, and this bar's own cell is
//! hot. Close when RSI is below 45. A bar with RSI above 60 turns its own cell hot for good. It never shorts.
//!
//! The mask remembers where it started.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::strategy_kit::IndicatorLine;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

const DAY_MS: i64 = 86_400_000;
const HOUR_MS: i64 = 3_600_000;
/// 1970-01-01 was a Thursday, weekday 4 when Sunday is 0.
const EPOCH_WEEKDAY: i64 = 4;

fn transpose<const R: usize, const C: usize>(cells: &[[bool; C]; R]) -> [[bool; R]; C] {
    std::array::from_fn(|c| std::array::from_fn(|r| cells[r][c]))
}

struct HotMask {
    rsi: IndicatorLine,
    cells: [[bool; 7]; 24],
    min_hot: usize,
    round_trip: bool,
}

/// The rule above with its own minimum of hot cells, reading the mask back through two transposes when `round_trip` is set.
fn hot_mask(min_hot: usize, round_trip: bool) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(HotMask {
        rsi: IndicatorLine::new("rsi", &[14.0]),
        cells: [[false; 7]; 24],
        min_hot,
        round_trip,
    }))
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    hot_mask(6, false)
}

impl SignalSource for HotMask {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let rsi = self.rsi.next(bar)[0];
        let hour = (bar.timestamp.rem_euclid(DAY_MS) / HOUR_MS) as usize;
        let weekday = (bar.timestamp.div_euclid(DAY_MS) + EPOCH_WEEKDAY).rem_euclid(7) as usize;
        if rsi > 60.0 {
            self.cells[hour][weekday] = true;
        }

        let read = if self.round_trip { transpose(&transpose(&self.cells)) } else { self.cells };
        let hot = read.iter().flatten().filter(|cell| **cell).count();
        if hot >= self.min_hot && rsi > 55.0 && read[hour][weekday] {
            Signal::Long
        } else if rsi < 45.0 {
            Signal::Close
        } else {
            Signal::Hold
        }
    }
}
