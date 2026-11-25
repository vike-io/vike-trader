// SPDX-License-Identifier: Apache-2.0
// © PineForge contributors 2026
// Source: https://github.com/pineforge-4pass/pineforge-corpus/blob/adef1c63e650163a57408bcf3c97e7d5ecd4f697/validation/matrix-covariance-eigen-pca-01/strategy.pine
// Changed: rewritten by hand in Rust for vike. See NOTICE in the strategies folder.

//! Long when the largest eigenvalue of a 2 by 2 covariance crosses up through its own SMA(14), short when it crosses down. The
//! two series are close minus open and high minus low, and each covariance is the SMA(14) of the product of their distances from
//! their own SMA(14).
//!
//! The original reads the first element of an eigenvalue list it does not sort. This rule takes the larger eigenvalue, from
//! the closed form of a symmetric 2 by 2, with no iterative routine.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//! ```

use vike_indicators::indicators::Sma;
use vike_marketdata::Bar;
use vike_model::params::Params;
use vike_tactics::Rule;
use vike_tactics::signal::{Cross, Signal, SignalRule, SignalSource};

const LENGTH: f64 = 14.0;

struct S {
    body_mean: Sma,
    range_mean: Sma,
    body_variance: Sma,
    covariance: Sma,
    range_variance: Sma,
    eigenvalue_mean: Sma,
    cross: Cross,
}

fn average() -> Sma {
    Sma::new(LENGTH).expect("a valid period")
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(SignalRule(S {
        body_mean: average(),
        range_mean: average(),
        body_variance: average(),
        covariance: average(),
        range_variance: average(),
        eigenvalue_mean: average(),
        cross: Cross::new(),
    }))
}

/// The larger eigenvalue of the symmetric matrix `[[a, b], [b, c]]`.
fn largest_eigenvalue(a: f64, b: f64, c: f64) -> f64 {
    (a + c) / 2.0 + libm::hypot((a - c) / 2.0, b)
}

impl SignalSource for S {
    fn signal(&mut self, bar: &Bar) -> Signal {
        let body = bar.close - bar.open;
        let range = bar.high - bar.low;
        let body_mean = self.body_mean.next_value(body);
        let range_mean = self.range_mean.next_value(range);
        // Each average takes only real values, so its window fills from the first one it is given.
        if body_mean.is_nan() {
            return Signal::Hold;
        }
        let (d_body, d_range) = (body - body_mean, range - range_mean);
        let a = self.body_variance.next_value(d_body * d_body);
        let b = self.covariance.next_value(d_body * d_range);
        let c = self.range_variance.next_value(d_range * d_range);
        if a.is_nan() {
            return Signal::Hold;
        }
        let eigenvalue = largest_eigenvalue(a, b, c);
        let mean = self.eigenvalue_mean.next_value(eigenvalue);
        self.cross.step(eigenvalue, mean)
    }
}
