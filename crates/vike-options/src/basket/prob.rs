//! Probability of profit and expected value of a [`Basket`] under a lognormal terminal law.

use super::{Basket, gt};
use crate::pricing::greeks::{norm_cdf, norm_pdf};

/// The lognormal terminal-price law used by [`Basket::probability_of_profit`] and
/// [`Basket::expected_value`].
///
/// `ln(S_T) ~ N( ln(spot) + (r - sigma²/2)·tau , sigma²·tau )` — the standard risk-neutral
/// GBM terminal distribution with no dividends, matching this crate's Black–Scholes
/// conventions.
///
/// **Choosing `sigma`:** there is no single "basket vol" — the legs generally have different
/// IVs. The recommended input is the chain's **ATM implied vol** at the basket's dominant
/// expiry, i.e. [`crate::vol::atm_iv`], which is the market's own estimate of terminal
/// dispersion and the convention retail P/L calculators use. A realized-vol estimate — from
/// `vike-indicators`' realized-volatility indicator family, annualized on the same 365-day basis —
/// is the alternative when you want the physical rather than the risk-neutral view, in which case
/// set `r` to your expected drift instead of the rate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalDist {
    /// Underlying price now.
    pub spot: f64,
    /// Terminal-distribution volatility (decimal, annualized). See the type doc — ATM IV.
    pub sigma: f64,
    /// Horizon in years (the basket's expiry).
    pub tau: f64,
    /// Drift (continuous). `r` for the risk-neutral view; an expected return for the physical.
    pub r: f64,
}

impl TerminalDist {
    /// `(mu_ln, s)`: the mean and stdev of `ln(S_T)`, or `None` on invalid inputs.
    pub(super) fn log_params(&self) -> Option<(f64, f64)> {
        if !gt(self.spot, 0.0) || !gt(self.sigma, 0.0) || !gt(self.tau, 0.0) {
            return None;
        }
        let s = self.sigma * self.tau.sqrt();
        // `libm::log`, never `f64::ln` (ADR 0032). `mu_ln` is the centre of the terminal law, so
        // it reaches BOTH consumers — every `cdf` z-score in `probability_of_profit` and every
        // quadrature node in `expected_value`. `tau.sqrt()` above stays on `f64`: IEEE 754
        // requires `sqrt` correctly rounded, so it is portable without help.
        let mu_ln = libm::log(self.spot) + (self.r - 0.5 * self.sigma * self.sigma) * self.tau;
        Some((mu_ln, s))
    }

    /// `P(S_T <= price)`. `price <= 0` is impossible under a lognormal, so returns 0.0.
    pub(super) fn cdf(&self, price: f64, mu_ln: f64, s: f64) -> f64 {
        if price <= 0.0 {
            return 0.0;
        }
        // `libm::log`, never `f64::ln` (ADR 0032). The `norm_cdf` it feeds is already `libm::erf`,
        // one call away — leaving the logarithm on the platform would be the exact mixed state the
        // module doc above is about: portable-looking on the outside, platform-decided inside.
        norm_cdf((libm::log(price) - mu_ln) / s)
    }
}

/// Fixed-grid settings for the [`Basket::expected_value`] integral.
///
/// The integral is taken in **log-price space** (`u = ln S`), where the density is exactly
/// Gaussian, over the truncated window `mu_ln ± n_sigma·s`.
///
/// **`steps` is per smooth panel, not per window.** The payoff has a derivative discontinuity
/// at every strike; a quadrature rule that straddles one loses its convergence order. The
/// window is therefore split at each `ln(strike)` falling inside it, and each resulting panel
/// — on which the integrand is analytic — gets its own uniform **composite Simpson** rule of
/// `steps` intervals (rounded UP to even, as Simpson requires pairs). A basket with `n`
/// distinct in-window strikes costs `(n + 1)·(steps + 1)` payoff evaluations; at the default
/// 4096 the residual quadrature error lands near f64 round-off.
///
/// Plain trapezoid was measured at ~5e-6 absolute on these panels even fully kink-split (its
/// `O(h²)` rate); Simpson's `O(h⁴)` is what buys the ~1e-12 agreement the EV tests pin.
///
/// **Truncation:** mass outside `±n_sigma` standard deviations is discarded. At the default
/// `n_sigma = 10.0` that tail is ~1.5e-23 of the probability; the payoff grows only linearly
/// in `S_T`, so for realistic `sigma·sqrt(tau)` the discarded *value* stays far below f64
/// round-off of the answer. Raise `n_sigma` for very long-dated / very high-vol baskets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EvGrid {
    /// Half-width of the integration window in standard deviations of `ln(S_T)`.
    pub n_sigma: f64,
    /// Number of trapezoid intervals (grid has `steps + 1` nodes).
    pub steps: usize,
}

impl Default for EvGrid {
    fn default() -> Self {
        Self { n_sigma: 10.0, steps: 4096 }
    }
}

impl Basket {
    /// Probability that the expiry P&L is **strictly positive** under `dist`.
    ///
    /// The break-evens partition `[0, ∞)` into intervals on which the payoff sign is constant;
    /// each interval's probability is a difference of lognormal CDFs, and the profitable ones
    /// are summed (left fold, ascending). Returns `None` on invalid distribution inputs
    /// (non-positive `spot`/`sigma`/`tau`).
    ///
    /// The break-evens themselves are a measure-zero set, so "strictly positive" vs
    /// "non-negative" does not change the answer for a payoff that only touches zero.
    pub fn probability_of_profit(&self, dist: &TerminalDist) -> Option<f64> {
        let (mu_ln, s) = dist.log_params()?;
        let bes = self.break_evens();
        // A point strictly beyond every kink AND every break-even, for the final interval.
        let mut beyond = 1.0;
        for k in self.kinks() {
            if k + 1.0 > beyond {
                beyond = k + 1.0;
            }
        }
        for &b in &bes {
            if b + 1.0 > beyond {
                beyond = b + 1.0;
            }
        }

        let mut acc = 0.0;
        let mut lo = 0.0;
        for (i, &b) in bes.iter().enumerate() {
            let rep = if i == 0 { 0.5 * b } else { 0.5 * (lo + b) };
            if self.payoff_at(rep) > 0.0 {
                acc += dist.cdf(b, mu_ln, s) - dist.cdf(lo, mu_ln, s);
            }
            lo = b;
        }
        if self.payoff_at(beyond) > 0.0 {
            acc += 1.0 - dist.cdf(lo, mu_ln, s);
        }
        Some(acc)
    }

    /// Expected expiry P&L `E[payoff(S_T)]` under `dist`, by fixed-grid composite Simpson in
    /// log-price space, split at the strikes (see [`EvGrid`] for the quadrature and truncation
    /// contract). Deterministic; returns `None` on
    /// invalid distribution inputs or a degenerate grid (`steps == 0`, `n_sigma <= 0`).
    ///
    /// Note this is the **undiscounted** expectation. Sanity anchor: with `r = 0` and a single
    /// long call priced at zero premium, it converges to [`crate::black_scholes_price`].
    pub fn expected_value(&self, dist: &TerminalDist, grid: &EvGrid) -> Option<f64> {
        let (mu_ln, s) = dist.log_params()?;
        if grid.steps == 0 || !gt(grid.n_sigma, 0.0) {
            return None;
        }
        // Simpson needs an even interval count per panel; round up (documented on `EvGrid`).
        let n = grid.steps + (grid.steps % 2);
        let half = grid.n_sigma * s;
        let (u_lo, u_hi) = (mu_ln - half, mu_ln + half);

        // Panel edges: the window bounds plus every in-window strike, ascending. Splitting on
        // the kinks is what keeps the trapezoid at its O(h^2) rate (see `EvGrid`).
        let mut edges: Vec<f64> = Vec::with_capacity(self.legs.len() + 2);
        edges.push(u_lo);
        for k in self.kinks() {
            if k > 0.0 {
                // `libm::log`, never `f64::ln` (ADR 0032). A panel EDGE is where a last-bit
                // difference stops being a last bit: the strike's log-coordinate decides both the
                // `u > u_lo && u < u_hi` admission and every node position inside the two panels
                // it separates, so a divergence here changes the quadrature grid rather than one
                // sample of it.
                let u = libm::log(k);
                if u > u_lo && u < u_hi {
                    edges.push(u);
                }
            }
        }
        edges.push(u_hi);

        let mut acc = 0.0;
        for pair in edges.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if !gt(b, a) {
                continue;
            }
            let h = (b - a) / (n as f64);
            // Composite Simpson: weights 1, 4, 2, 4, ..., 4, 1 scaled by h/3.
            let mut panel = 0.0;
            for i in 0..=n {
                let u = a + (i as f64) * h;
                let z = (u - mu_ln) / s;
                let w = if i == 0 || i == n {
                    1.0
                } else if i % 2 == 1 {
                    4.0
                } else {
                    2.0
                };
                // `libm::exp`, never `f64::exp` (ADR 0032) — the inverse of the `u = ln S`
                // substitution, evaluated `(panels)·(steps + 1)` times per call (4097 per panel at
                // the default grid), so this is the hottest transcendental in the crate and the
                // one whose divergence the summation would accumulate across thousands of nodes.
                panel += w * self.payoff_at(libm::exp(u)) * norm_pdf(z);
            }
            acc += panel * h / 3.0;
        }
        // The `1/s` Jacobian of the u = ln S substitution, factored out of every panel.
        Some(acc / s)
    }
}
