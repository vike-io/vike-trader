// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/session-ny-spring-forward-dst-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long on the first bar of the 09:30 to 16:00 New York session when flat, close on the first bar after it when long. A bar
//! belongs to the session by its open time, 16:00 itself is outside, and every day of the week has a session.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::helpers::us_minute;
use vike_tactics::signal::{Signal, SignalRule, SignalSource};

struct SessionEdges {
    opens: i64,
    closes: i64,
    was_inside: bool,
}

/// Long on entering the New York session and close on leaving it, its bounds in minutes since New York midnight.
fn session_edges(opens: i64, closes: i64) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(SessionEdges { opens, closes, was_inside: false }))
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    session_edges(9 * 60 + 30, 16 * 60)
}

impl SignalSource for SessionEdges {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let inside = (self.opens..self.closes).contains(&us_minute(bar.timestamp, -5));
        let signal = match (inside, self.was_inside) {
            (true, false) => Signal::Long,
            (false, true) => Signal::Close,
            _ => Signal::Hold,
        };
        self.was_inside = inside;
        signal
    }
}
