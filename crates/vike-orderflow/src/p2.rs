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

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact oracle: full-sort interpolated quantile (same rank convention).
    fn exact(sample: &[f64], p: f64) -> f64 {
        let mut v = sample.to_vec();
        v.sort_by(f64::total_cmp);
        interp_sorted(&v, p)
    }

    /// Deterministic LCG uniforms in [0, 1) — no rand dep.
    fn uniforms(n: usize, mut seed: u64) -> Vec<f64> {
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            out.push((seed >> 11) as f64 / (1u64 << 53) as f64);
        }
        out
    }

    // The worked example from Jain & Chlamtac (1985), Table I: 20 observations, p = 0.5;
    // the paper's final middle-marker height is 4.44063.
    #[test]
    fn p2_matches_the_paper_example() {
        let obs = [
            0.02, 0.15, 0.74, 3.39, 0.83, 22.37, 10.15, 15.43, 38.62, 15.92, 34.60, 10.28, 1.47,
            0.40, 0.05, 11.39, 0.27, 0.42, 0.09, 11.37,
        ];
        let mut est = P2Quantile::new(0.5);
        for &x in &obs {
            est.push(x);
        }
        let v = est.value().unwrap();
        assert!((v - 4.44063).abs() < 1e-4, "paper example: got {v}");
        assert_eq!(est.count(), 20);
    }

    #[test]
    fn small_samples_are_exact() {
        let mut est = P2Quantile::new(0.5);
        assert_eq!(est.value(), None);
        est.push(4.0);
        assert_eq!(est.value(), Some(4.0));
        est.push(2.0);
        assert_eq!(est.value(), Some(3.0)); // median of {2,4}
        est.push(6.0);
        assert_eq!(est.value(), Some(4.0)); // median of {2,4,6}
        est.push(0.0);
        assert_eq!(est.value(), Some(3.0)); // {0,2,4,6} → (2+4)/2
    }

    #[test]
    fn p2_close_to_exact_on_uniform_sample() {
        let xs = uniforms(1000, 42);
        for &p in &[0.1, 0.5, 0.9, 0.95] {
            let mut est = P2Quantile::new(p);
            for &x in &xs {
                est.push(x);
            }
            let got = est.value().unwrap();
            let want = exact(&xs, p);
            assert!(
                (got - want).abs() < 0.02,
                "p={p}: P² {got} vs exact {want} (|Δ| = {})",
                (got - want).abs()
            );
        }
    }

    // Skewed sample (x³ of uniforms): the estimator must still land near the exact
    // quantile — a distribution-shape robustness check, looser gate.
    #[test]
    fn p2_close_to_exact_on_skewed_sample() {
        let xs: Vec<f64> = uniforms(2000, 7).into_iter().map(|u| u * u * u).collect();
        for &p in &[0.5, 0.9] {
            let mut est = P2Quantile::new(p);
            for &x in &xs {
                est.push(x);
            }
            let got = est.value().unwrap();
            let want = exact(&xs, p);
            assert!((got - want).abs() < 0.03, "p={p}: P² {got} vs exact {want}");
        }
    }

    // Invariants under a mixed stream: markers stay sorted, estimate within [min, max].
    #[test]
    fn markers_stay_monotone_and_bounded() {
        let xs = uniforms(500, 99);
        let mut est = P2Quantile::new(0.75);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for &x in &xs {
            est.push(x);
            lo = lo.min(x);
            hi = hi.max(x);
            if est.count() >= 5 {
                for i in 0..4 {
                    assert!(est.q[i] <= est.q[i + 1], "markers out of order at {i}");
                }
                let v = est.value().unwrap();
                assert!((lo..=hi).contains(&v));
            }
        }
    }

    #[test]
    fn nan_is_ignored() {
        let mut est = P2Quantile::new(0.5);
        for x in [1.0, f64::NAN, 2.0, 3.0, f64::NAN, 4.0, 5.0, 6.0, 7.0] {
            est.push(x);
        }
        assert_eq!(est.count(), 7);
        let v = est.value().unwrap();
        assert!(v.is_finite());
    }
}
