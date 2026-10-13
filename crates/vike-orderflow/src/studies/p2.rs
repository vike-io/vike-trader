//! P² (Jain & Chlamtac 1985) — streaming single-quantile estimator in O(1) space and
//! O(1) per-observation update; the textbook five-marker algorithm with the parabolic
//! (piecewise-parabolic, "P²") marker-height adjustment and the linear fallback when the
//! parabola would break marker monotonicity. Generic utility for future adaptive
//! thresholds (e.g. a VPIN alarm level tracking its own p95) — this crate keeps it pure
//! and venue-free like everything else here.
//!
//! Mechanism: five markers track (min, ~p/2, ~p, ~(1+p)/2, max) of the stream. Each
//! observation bumps the positions of the markers above its cell, drifts the DESIRED
//! positions by fixed increments, and nudges any interior marker whose actual position
//! strayed ≥ 1 from desired — parabolic height interpolation when the move preserves
//! `q[i−1] < q' < q[i+1]`, linear otherwise. The p-quantile estimate is the middle
//! marker's height. Until 5 observations exist the estimate is the exact interpolated
//! quantile of the buffered sample (the algorithm is undefined below 5).
use std::cmp::Ordering;

/// Streaming estimator of one fixed quantile `p` ∈ (0, 1).
pub struct P2Quantile {
    p: f64,
    /// marker heights q0..q4 (q0 = running min, q4 = running max once initialized)
    q: [f64; 5],
    /// actual marker positions (1-based counts; integer-valued, kept as f64 — every
    /// mutation is ±1.0, exact)
    n: [f64; 5],
    /// desired marker positions
    np: [f64; 5],
    /// per-observation desired-position increments
    dnp: [f64; 5],
    count: u64,
    /// the first ≤ 5 observations (init buffer, then frozen)
    init: Vec<f64>,
}

impl P2Quantile {
    pub fn new(p: f64) -> Self {
        assert!(p > 0.0 && p < 1.0 && p.is_finite(), "P2Quantile p must be in (0, 1)");
        P2Quantile {
            p,
            q: [0.0; 5],
            n: [0.0; 5],
            np: [0.0; 5],
            dnp: [0.0; 5],
            count: 0,
            init: Vec::with_capacity(5),
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// Fold one observation. NaN is ignored (a poisoned marker never recovers).
    pub fn push(&mut self, x: f64) {
        if x.is_nan() {
            return;
        }
        self.count += 1;
        if self.count <= 5 {
            self.init.push(x);
            if self.count == 5 {
                self.init.sort_by(f64::total_cmp);
                for i in 0..5 {
                    self.q[i] = self.init[i];
                    self.n[i] = (i + 1) as f64;
                }
                let p = self.p;
                self.np = [1.0, 1.0 + 2.0 * p, 1.0 + 4.0 * p, 3.0 + 2.0 * p, 5.0];
                self.dnp = [0.0, p / 2.0, p, (1.0 + p) / 2.0, 1.0];
            }
            return;
        }
        // B1: find the cell k such that q[k] <= x < q[k+1], extending the extremes.
        let k = if x < self.q[0] {
            self.q[0] = x;
            0
        } else if x < self.q[1] {
            0
        } else if x < self.q[2] {
            1
        } else if x < self.q[3] {
            2
        } else if x <= self.q[4] {
            3
        } else {
            self.q[4] = x;
            3
        };
        // B2: bump actual positions above the cell; drift all desired positions.
        for i in (k + 1)..5 {
            self.n[i] += 1.0;
        }
        for i in 0..5 {
            self.np[i] += self.dnp[i];
        }
        // B3: adjust interior markers whose position strayed >= 1 from desired.
        for i in 1..4 {
            let d = self.np[i] - self.n[i];
            if (d >= 1.0 && self.n[i + 1] - self.n[i] > 1.0)
                || (d <= -1.0 && self.n[i - 1] - self.n[i] < -1.0)
            {
                let ds = if d >= 0.0 { 1.0 } else { -1.0 };
                let qp = self.parabolic(i, ds);
                self.q[i] =
                    if self.q[i - 1] < qp && qp < self.q[i + 1] { qp } else { self.linear(i, ds) };
                self.n[i] += ds;
            }
        }
    }

    /// The piecewise-parabolic height for moving marker `i` by `d` (±1).
    fn parabolic(&self, i: usize, d: f64) -> f64 {
        let (q, n) = (&self.q, &self.n);
        q[i] + d / (n[i + 1] - n[i - 1])
            * ((n[i] - n[i - 1] + d) * (q[i + 1] - q[i]) / (n[i + 1] - n[i])
                + (n[i + 1] - n[i] - d) * (q[i] - q[i - 1]) / (n[i] - n[i - 1]))
    }

    /// Linear fallback toward the neighbor in the direction of `d`.
    fn linear(&self, i: usize, d: f64) -> f64 {
        let j = if d > 0.0 { i + 1 } else { i - 1 };
        self.q[i] + d * (self.q[j] - self.q[i]) / (self.n[j] - self.n[i])
    }

    /// Current estimate: the middle marker once ≥ 5 observations exist; the exact
    /// interpolated sample quantile of the buffer below that; None when empty.
    pub fn value(&self) -> Option<f64> {
        match self.count.cmp(&5) {
            Ordering::Less if self.count == 0 => None,
            Ordering::Less => {
                let mut v = self.init.clone();
                v.sort_by(f64::total_cmp);
                Some(interp_sorted(&v, self.p))
            }
            _ => Some(self.q[2]),
        }
    }
}

/// Exact linear-interpolated quantile of an ascending-sorted non-empty slice:
/// rank r = p·(m−1), value = v[⌊r⌋] + frac·(v[⌊r⌋+1] − v[⌊r⌋]).
fn interp_sorted(v: &[f64], p: f64) -> f64 {
    let r = p * (v.len() - 1) as f64;
    let lo = r.floor() as usize;
    let frac = r - lo as f64;
    if lo + 1 >= v.len() { v[v.len() - 1] } else { v[lo] + frac * (v[lo + 1] - v[lo]) }
}

#[path = "p2_tests.rs"]
#[cfg(test)]
mod p2_tests;
