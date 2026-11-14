// SPDX-License-Identifier: MPL-2.0
// This Source Code Form is subject to the terms of the Mozilla Public License, v. 2.0. If a copy of the MPL
// was not distributed with this file, You can obtain one at https://mozilla.org/MPL/2.0/.
// Derived from "Delta-RSI Strategy with Filters" by tbtkg, https://www.tradingview.com/script/0OdCms2T/
// The original's notice: © tbiktag
// Changed: rewritten by hand in Rust for vike.

//! Delta-RSI: the slope at the newest point of a cubic least-squares fit to the last 50 values of RSI(21), and its signal line,
//! EMA(9) of it. Long 1 when the slope crosses up through its signal line, short 1 when it crosses down, each reversing the
//! other. A crossing down also closes a long, and closes a short: the rule's close of a short reads the long's exit
//! condition. No costs; starts with 100,000.
//!
//! The fit is the rule's own arithmetic: the 50-by-4 matrix of `z^j` for `z = 0..49`, its pseudoinverse through a modified
//! Gram-Schmidt QR and the inverse of `R`, then the coefficients, each sum taken in the rule's order so the slope is its
//! number to the bit. Its stop and target are off and its filters pass everything.
//!
//! ```toml
//! [strategy]
//! runs_on = "paper"
//!
//! [run]
//! bound_exits = true
//! ```

use std::collections::VecDeque;
use vike_tactics::signal::{Cross, Signal};
use vike_tactics::{Rule, RuleDefaults, Tactics};

use vike_indicators::indicators::{Ema, Rsi};
use vike_marketdata::Bar;
use vike_model::OrderSide::{Long, Short};
use vike_model::params::Params;

const WINDOW: usize = 50;
const DEGREE: usize = 3;
const COLUMNS: usize = DEGREE + 1;

/// `a[i + rows * j]`, the rule's column-major matrix element.
fn at(a: &[f64], i: usize, j: usize, rows: usize) -> f64 {
    a[i + rows * j]
}

/// The square norm of `x` as the rule sums it.
fn norm(x: &[f64]) -> f64 {
    x.iter().fold(0.0, |s, v| s + v * v).sqrt()
}

/// `C = A * B`, `A` being `rows` by `inner` and `B` `inner` by `cols`, each element summed from 0 in order.
fn multiply(a: &[f64], b: &[f64], rows: usize, inner: usize, cols: usize) -> Vec<f64> {
    let mut c = vec![0.0; rows * cols];
    for i in 0..rows {
        for j in 0..cols {
            let mut e = 0.0;
            for k in 0..inner {
                e += at(a, i, k, rows) * at(b, k, j, inner);
            }
            c[i + rows * j] = e;
        }
    }
    c
}

/// The pseudoinverse of the `WINDOW` by `COLUMNS` matrix of `z^j`, `COLUMNS` by `WINDOW`, as the rule computes it.
fn fit_rows() -> Vec<f64> {
    let (n, m) = (WINDOW, COLUMNS);
    let mut a = vec![0.0; n * m];
    for i in 0..n {
        for j in 0..m {
            a[i + n * j] = libm::pow(i as f64, j as f64);
        }
    }
    let mut q = vec![0.0; n * m];
    let mut r = vec![0.0; m * m];
    let mut col: Vec<f64> = (0..n).map(|i| at(&a, i, 0, n)).collect();
    let mut d = norm(&col);
    r[0] = d;
    for i in 0..n {
        q[i] = col[i] / d;
    }
    for k in 1..m {
        for (i, c) in col.iter_mut().enumerate() {
            *c = at(&a, i, k, n);
        }
        for j in 0..k {
            d = 0.0;
            for (i, c) in col.iter().enumerate() {
                d += at(&q, i, j, n) * c;
            }
            r[j + m * k] = d;
            for (i, c) in col.iter_mut().enumerate() {
                *c -= d * at(&q, i, j, n);
            }
        }
        d = norm(&col);
        r[k + m * k] = d;
        for i in 0..n {
            q[i + n * k] = col[i] / d;
        }
    }
    let mut qt = vec![0.0; n * m];
    for i in 0..n {
        for j in 0..m {
            qt[j + m * i] = at(&q, i, j, n);
        }
    }
    let mut inverse = vec![0.0; m * m];
    inverse[0] = 1.0 / r[0];
    for j in 1..m {
        for i in 0..j {
            let mut s = 0.0;
            for k in i..j {
                s += at(&inverse, i, k, m) * at(&r, k, j, m);
            }
            inverse[i + m * j] = s;
        }
        for k in 0..j {
            inverse[k + m * j] = -at(&inverse, k, j, m) / at(&r, j, j, m);
        }
        inverse[j + m * j] = 1.0 / at(&r, j, j, m);
    }
    multiply(&inverse, &qt, m, m, n)
}

struct S {
    rsi: Rsi,
    signal: Ema,
    fit: Vec<f64>,
    /// The last `WINDOW` values of the RSI, oldest first.
    values: VecDeque<f64>,
    cross: Cross,
}

pub fn rule(_params: &Params) -> Box<dyn Rule + Send> {
    Box::new(S {
        rsi: Rsi::new(21.0).expect("a valid length"),
        signal: Ema::new(9.0).expect("a valid length").with_skip_nan(true),
        fit: fit_rows(),
        values: VecDeque::with_capacity(WINDOW + 1),
        cross: Cross::new(),
    })
}

impl S {
    /// The slope at the newest point, NaN until the window holds `WINDOW` values.
    fn slope(&self) -> f64 {
        if self.values.len() < WINDOW {
            return f64::NAN;
        }
        let y: Vec<f64> = self.values.iter().copied().collect();
        let coef = multiply(&self.fit, &y, COLUMNS, WINDOW, 1);
        let mut slope = 0.0;
        for (i, c) in coef.iter().enumerate().skip(1) {
            slope += i as f64 * c * libm::pow((WINDOW - 1) as f64, (i - 1) as f64);
        }
        slope
    }
}

impl Rule for S {
    fn defaults(&self) -> RuleDefaults {
        RuleDefaults::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.values.push_back(self.rsi.next(bar));
        if self.values.len() > WINDOW {
            self.values.pop_front();
        }
        let slope = self.slope();
        let signal = self.signal.next_value(slope);
        match self.cross.step(slope, signal) {
            Signal::Long => {
                strategy.entry("long", Long).size(1.0);
            }
            Signal::Short => {
                strategy.entry("short", Short).size(1.0);
                strategy.close("long");
                strategy.close("short");
            }
            _ => {}
        }
    }
}
