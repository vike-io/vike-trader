//! Multi-leg option **basket** analytics — expiry payoff, break-evens, max profit/loss, net
//! greeks, and probability-of-profit / expected-value under a lognormal terminal law.
//!
//! NOT an oracle port: the Python twin (`vike-trader-app data/options/`) has no basket module,
//! so this is an independent ADDITIVE implementation of the standard textbook mechanics
//! (piecewise-linear expiry payoff; risk-neutral lognormal terminal distribution). No parity
//! fixture governs it and none was touched. It reuses the crate's existing primitives rather
//! than re-deriving them: [`crate::greeks::black_scholes_greeks`] for first-order greeks,
//! [`crate::second_order::second_order_greeks`] for vanna/vomma/charm/veta/color, and the
//! crate's own normal CDF/PDF for the probability integrals.
//!
//! ⚠ **That last clause used to read "(`libm`-backed erf)", and the parenthesis was doing damage
//! rather than information.** It was true — `crate::greeks::norm_cdf` does call `libm::erf` — and
//! it read as though the portability question had been answered for this file, while FOUR
//! platform-libm calls sat in the code below it: `TerminalDist::log_params`' `spot.ln()`,
//! `TerminalDist::cdf`'s `price.ln()`, and [`Basket::expected_value`]'s `k.ln()` panel split plus
//! its `u.exp()` inverse substitution. **A half-converted file is worse than an unconverted one**,
//! because a nearby `libm::` spelling is exactly the evidence a reader uses to decide not to look.
//! All four were converted 2026-08-26 under
//! `docs/decisions/0032-transcendentals-come-from-the-libm-crate-not-the-platform.md`: IEEE 754
//! requires `+ - * /` and `sqrt` correctly rounded and requires NOTHING of `ln`/`exp`, so MSVC's
//! CRT and glibc are each free to pick a different last bit. `tau.sqrt()` in
//! `TerminalDist::log_params` deliberately stays on `f64::sqrt`, which IEEE 754 already pins.
//!
//! ⚠ **What that conversion did and did not cost, stated rather than implied.** Nothing was
//! re-recorded and nothing needed to be: this module pins no absolute literal. Every numeric
//! assertion in its `mod tests` is either pure payoff arithmetic (no transcendental in the path at
//! all) or a SELF-CONSISTENCY comparison whose two sides move together — `net_greeks` against the
//! per-leg calls it folds, `expected_value` against [`crate::greeks::black_scholes_price`],
//! `probability_of_profit` against a direct `TerminalDist::cdf` interval, the coarse/fine grid
//! convergence pair. What is NOT known is whether the divergence was moving numbers here before
//! the conversion: `vike-options` carries no `libm_platform_probe`, and this change did not add
//! one, so the sibling crates' measurements are the whole of the evidence.
//!
//! CONTRACT / conventions:
//! - A [`Leg`] carries a **signed** `qty` (positive = long, negative = short), a strike, a
//!   call/put flag, and a **per-unit premium** — always the quoted (positive) option price;
//!   the sign of the cash flow comes from `qty`. [`Basket::net_premium`] is therefore
//!   `Σ qty·premium`: **positive = net debit paid**, negative = net credit received.
//! - [`Basket::payoff_at`] is the **P&L at expiry** (intrinsic value minus net premium), per
//!   1.0 of underlying, in the underlying's quote currency. Multipliers/contract sizes are the
//!   caller's business — scale `qty` accordingly.
//! - **Domain is `S_T ∈ [0, ∞)`.** A price cannot go below zero, so the downside is always
//!   bounded (a naked short put's worst case is realized at `S_T = 0`, not at `-∞`). Only the
//!   `S → +∞` ray can be [`PayoffBound::Unbounded`], decided by the payoff slope beyond the
//!   highest strike. See [`Basket::max_profit`] / [`Basket::max_loss`].
//! - Summation is a **naive left fold in leg order** everywhere (payoff and greeks alike), so
//!   [`Basket::net_greeks`] is bit-identical to summing the per-leg single-option calls in the
//!   same order. Do not reorder legs expecting identical bits.
//! - Everything here is pure `f64` and **deterministic**: no RNG, no I/O, no clock. The EV
//!   integral is a fixed-grid composite Simpson, so the same inputs always give the same bits.

use crate::greeks::{black_scholes_greeks, norm_cdf, norm_pdf};
use crate::model::OptionKind;
use crate::second_order::second_order_greeks;

/// `x > y` as a plain bool.
///
/// A named helper rather than an inline `!(x > y)` at the guard sites below, because the
/// negated comparison form trips `clippy::neg_cmp_op_on_partial_ord`. The semantics are
/// deliberate and load-bearing: every comparison against NaN is false, so `!gt(NaN, 0.0)` is
/// true and each guard rejects a NaN input rather than letting it poison the arithmetic —
/// which a `<=` rewrite would NOT do.
fn gt(x: f64, y: f64) -> bool {
    x > y
}

/// One option leg of a basket.
///
/// `qty` is signed (long > 0, short < 0) and may be fractional. `premium` is the quoted
/// per-unit option price and is expected to be **non-negative**; shorts express their credit
/// through a negative `qty`, not a negative premium.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Leg {
    /// Signed quantity: positive = long, negative = short.
    pub qty: f64,
    /// Strike price.
    pub strike: f64,
    /// `true` = call, `false` = put.
    pub is_call: bool,
    /// Quoted per-unit premium (non-negative; the cash-flow sign comes from `qty`).
    pub premium: f64,
}

impl Leg {
    /// A long (`qty > 0`) leg constructor; pass a negative `qty` for a short.
    pub fn new(qty: f64, strike: f64, is_call: bool, premium: f64) -> Self {
        Self { qty, strike, is_call, premium }
    }

    /// This leg's [`OptionKind`], for the greeks calls.
    pub fn kind(&self) -> OptionKind {
        if self.is_call {
            OptionKind::Call
        } else {
            OptionKind::Put
        }
    }

    /// Per-unit intrinsic value at expiry price `price` (always >= 0, unsigned by `qty`).
    pub fn intrinsic(&self, price: f64) -> f64 {
        if self.is_call {
            (price - self.strike).max(0.0)
        } else {
            (self.strike - price).max(0.0)
        }
    }

    /// `d(qty·intrinsic)/dS` strictly to the RIGHT of `price` (the right-hand slope): a call
    /// contributes `qty` once `price >= strike`, a put contributes `-qty` while
    /// `price < strike`.
    fn right_slope(&self, price: f64) -> f64 {
        if self.is_call {
            if price >= self.strike {
                self.qty
            } else {
                0.0
            }
        } else if price < self.strike {
            -self.qty
        } else {
            0.0
        }
    }
}

/// Whether an extremum of the expiry payoff is finite, and its value.
///
/// `Bounded` carries the P&L at the extremum (so a max loss reads as a **negative** number
/// when the basket can lose money).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PayoffBound {
    /// The extremum is finite, at this P&L.
    Bounded(f64),
    /// The payoff runs off to ±∞ as `S_T → +∞`.
    Unbounded,
}

impl PayoffBound {
    /// The finite value, or `None` when [`PayoffBound::Unbounded`].
    pub fn value(self) -> Option<f64> {
        match self {
            Self::Bounded(v) => Some(v),
            Self::Unbounded => None,
        }
    }

    /// `true` when this extremum is unbounded.
    pub fn is_unbounded(self) -> bool {
        matches!(self, Self::Unbounded)
    }
}

/// Per-leg market inputs for the net-greeks aggregation — one entry per [`Leg`], same order.
///
/// Kept per-leg (rather than one basket-wide tuple) because a calendar/diagonal spread has a
/// different `tau` and IV on every leg. `spot` and `r` are per-leg too so a caller can price a
/// multi-underlying basket, though the usual case repeats the same values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LegMarket {
    /// Implied volatility of this leg, as a decimal (0.65 = 65%).
    pub iv: f64,
    /// Time to expiry in years.
    pub tau: f64,
    /// Underlying spot for this leg.
    pub spot: f64,
    /// Risk-free rate (continuous). A plain parameter — env reads stay in the binary.
    pub r: f64,
}

/// The signed, quantity-weighted sum of every leg's greeks.
///
/// Units follow the per-leg sources exactly: `delta` per 1.0 of underlying, `gamma` per 1.0²,
/// `theta` **per calendar day**, `vega` **per vol-point** (`black_scholes_greeks`' /100
/// scaling); `vanna`/`vomma` are RAW per-1.0-sigma and `charm`/`veta`/`color` are per calendar
/// day (`second_order_greeks`' conventions). Mixing scales is inherited from those two
/// sources deliberately — this type does no rescaling.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NetGreeks {
    /// Σ qty·delta.
    pub delta: f64,
    /// Σ qty·gamma.
    pub gamma: f64,
    /// Σ qty·theta (per calendar day).
    pub theta: f64,
    /// Σ qty·vega (per vol-point).
    pub vega: f64,
    /// Σ qty·vanna (raw, per 1.0 sigma).
    pub vanna: f64,
    /// Σ qty·vomma (raw, per 1.0 sigma).
    pub vomma: f64,
    /// Σ qty·charm (per calendar day).
    pub charm: f64,
    /// Σ qty·veta (per calendar day).
    pub veta: f64,
    /// Σ qty·color (per calendar day).
    pub color: f64,
}

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
/// dispersion and the convention retail P/L calculators use. A realized-vol estimate
/// ([`crate::vol::EwmaVol`]) is the alternative when you want the physical rather than the
/// risk-neutral view — in which case set `r` to your expected drift instead of the rate.
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
    fn log_params(&self) -> Option<(f64, f64)> {
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
    fn cdf(&self, price: f64, mu_ln: f64, s: f64) -> f64 {
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

/// A multi-leg option position on one underlying.
#[derive(Debug, Clone, PartialEq)]
pub struct Basket {
    /// The legs, in the order they were built. Summation order follows this vector.
    pub legs: Vec<Leg>,
    /// Underlying spot at construction time — the reference price for curve sampling.
    pub underlying_spot: f64,
}

impl Basket {
    /// Build a basket from legs and the current underlying spot.
    pub fn new(legs: Vec<Leg>, underlying_spot: f64) -> Self {
        Self { legs, underlying_spot }
    }

    /// `Σ qty·premium` — **positive = net debit paid**, negative = net credit received.
    /// Naive left fold in leg order.
    pub fn net_premium(&self) -> f64 {
        let mut acc = 0.0;
        for leg in &self.legs {
            acc += leg.qty * leg.premium;
        }
        acc
    }

    /// P&L at expiry for terminal price `price`: `Σ qty·intrinsic(price) − net_premium`.
    /// Piecewise-linear with kinks at the strikes. Naive left fold in leg order.
    pub fn payoff_at(&self, price: f64) -> f64 {
        let mut acc = 0.0;
        for leg in &self.legs {
            acc += leg.qty * leg.intrinsic(price);
        }
        acc - self.net_premium()
    }

    /// The sorted, de-duplicated strikes — the kinks of the piecewise-linear payoff.
    pub fn kinks(&self) -> Vec<f64> {
        let mut ks: Vec<f64> = self.legs.iter().map(|l| l.strike).collect();
        ks.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ks.dedup();
        ks
    }

    /// The payoff's **right-hand** slope at `price` (`dP&L/dS` just above it): the fold of
    /// every leg's contribution — a call turns on at its strike, a put turns off.
    ///
    /// Constant between kinks; AT a kink this reports the segment to the right, so
    /// `slope_at(x)` for any `x` at or beyond the highest strike equals
    /// [`Basket::upper_slope`]. Naive left fold in leg order.
    pub fn slope_at(&self, price: f64) -> f64 {
        let mut acc = 0.0;
        for leg in &self.legs {
            acc += leg.right_slope(price);
        }
        acc
    }

    /// The payoff slope beyond the highest strike (`S → +∞`): every call is ITM (contributing
    /// `qty` each), every put is worthless (contributing 0).
    pub fn upper_slope(&self) -> f64 {
        let mut acc = 0.0;
        for leg in &self.legs {
            if leg.is_call {
                acc += leg.qty;
            }
        }
        acc
    }

    /// Break-even terminal prices — every `S_T` in `[0, ∞)` where the expiry P&L crosses (or
    /// touches) zero, ascending and de-duplicated.
    ///
    /// Exact: the payoff is linear between kinks, so each crossing is found by linear
    /// interpolation on the bracketing kink interval; the final ray beyond the highest strike
    /// is solved against [`Basket::upper_slope`]. A node whose payoff is exactly `0.0` is
    /// reported as a break-even in its own right (a payoff that merely touches zero without
    /// crossing still counts).
    pub fn break_evens(&self) -> Vec<f64> {
        let kinks = self.kinks();
        // Nodes bounding each linear segment: 0.0 (the left edge of the domain) then the kinks.
        let mut nodes: Vec<f64> = Vec::with_capacity(kinks.len() + 1);
        nodes.push(0.0);
        for &k in &kinks {
            if k > 0.0 {
                nodes.push(k);
            }
        }
        let mut roots: Vec<f64> = Vec::new();
        let vals: Vec<f64> = nodes.iter().map(|&x| self.payoff_at(x)).collect();

        for (i, (&node, &val)) in nodes.iter().zip(vals.iter()).enumerate() {
            if val == 0.0 {
                roots.push(node);
            }
            if i + 1 < nodes.len() {
                let (a, b) = (node, nodes[i + 1]);
                let (pa, pb) = (val, vals[i + 1]);
                // Strict sign change ⇒ exactly one interior crossing on this segment.
                if (pa < 0.0 && pb > 0.0) || (pa > 0.0 && pb < 0.0) {
                    roots.push(a + (b - a) * (-pa) / (pb - pa));
                }
            }
        }

        // The unbounded ray beyond the last node.
        if let (Some(&last), Some(&p_last)) = (nodes.last(), vals.last()) {
            let slope = self.upper_slope();
            if p_last != 0.0 && slope != 0.0 {
                let x = last + (-p_last) / slope;
                if x > last {
                    roots.push(x);
                }
            }
        }

        roots.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        roots.dedup();
        roots
    }

    /// The best achievable expiry P&L over `S_T ∈ [0, ∞)`.
    ///
    /// [`PayoffBound::Unbounded`] iff the slope beyond the highest strike is strictly positive
    /// (e.g. a long call). Otherwise the maximum of a piecewise-linear function is attained at
    /// a node — `0.0` or a strike — so the finite value is the max over those.
    pub fn max_profit(&self) -> PayoffBound {
        if self.upper_slope() > 0.0 {
            return PayoffBound::Unbounded;
        }
        PayoffBound::Bounded(self.node_extremum(true))
    }

    /// The worst expiry P&L over `S_T ∈ [0, ∞)` (a loss reads negative).
    ///
    /// [`PayoffBound::Unbounded`] iff the slope beyond the highest strike is strictly negative
    /// (e.g. a naked short call). The downside is never unbounded — the domain stops at
    /// `S_T = 0` — so a naked short put reports the finite loss realized there.
    pub fn max_loss(&self) -> PayoffBound {
        if self.upper_slope() < 0.0 {
            return PayoffBound::Unbounded;
        }
        PayoffBound::Bounded(self.node_extremum(false))
    }

    /// Max (`want_max`) or min payoff over the nodes `{0.0} ∪ strikes`.
    fn node_extremum(&self, want_max: bool) -> f64 {
        let mut best = self.payoff_at(0.0);
        for k in self.kinks() {
            if k <= 0.0 {
                continue;
            }
            let v = self.payoff_at(k);
            if (want_max && v > best) || (!want_max && v < best) {
                best = v;
            }
        }
        best
    }

    /// Signed, quantity-weighted net greeks across every leg.
    ///
    /// `markets[i]` supplies leg `i`'s `(iv, tau, spot, r)`. Returns `None` when the slice
    /// length differs from the leg count, or when ANY leg's greeks are not computable (the
    /// `s/k/t/sigma <= 0` guard in [`black_scholes_greeks`]) — a partial sum would be
    /// silently wrong, so it is never returned.
    ///
    /// Accumulated as a naive left fold in leg order, so the result is bit-identical to
    /// summing the individual `black_scholes_greeks` / `second_order_greeks` calls in that
    /// same order.
    pub fn net_greeks(&self, markets: &[LegMarket]) -> Option<NetGreeks> {
        if markets.len() != self.legs.len() {
            return None;
        }
        let mut net = NetGreeks::default();
        for (leg, m) in self.legs.iter().zip(markets.iter()) {
            let (delta, gamma, theta, vega) =
                black_scholes_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r)?;
            let so = second_order_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r)?;
            net.delta += leg.qty * delta;
            net.gamma += leg.qty * gamma;
            net.theta += leg.qty * theta;
            net.vega += leg.qty * vega;
            net.vanna += leg.qty * so.vanna;
            net.vomma += leg.qty * so.vomma;
            net.charm += leg.qty * so.charm;
            net.veta += leg.qty * so.veta;
            net.color += leg.qty * so.color;
        }
        Some(net)
    }

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

    /// Sample the expiry payoff curve as `steps + 1` evenly spaced `(price, pnl)` points over
    /// `[lo, hi]` — the chart-ready form of [`Basket::payoff_at`].
    ///
    /// Empty when `steps == 0`, `hi <= lo`, or `lo < 0.0`.
    pub fn payoff_curve(&self, lo: f64, hi: f64, steps: usize) -> Vec<(f64, f64)> {
        if steps == 0 || !gt(hi, lo) || lo < 0.0 {
            return Vec::new();
        }
        let h = (hi - lo) / (steps as f64);
        (0..=steps)
            .map(|i| {
                let p = lo + (i as f64) * h;
                (p, self.payoff_at(p))
            })
            .collect()
    }

    /// [`Basket::payoff_curve`] over a spot-centred window of `±pct` (0.30 = ±30%), a
    /// convenience for the default chart view. Empty if `underlying_spot <= 0.0`.
    pub fn payoff_curve_around_spot(&self, pct: f64, steps: usize) -> Vec<(f64, f64)> {
        if self.underlying_spot <= 0.0 || !gt(pct, 0.0) {
            return Vec::new();
        }
        let lo = (self.underlying_spot * (1.0 - pct)).max(0.0);
        let hi = self.underlying_spot * (1.0 + pct);
        self.payoff_curve(lo, hi, steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long 1x 100-strike call at 5.00.
    fn long_call() -> Basket {
        Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0)], 100.0)
    }

    /// Bull call spread: long 100C @5, short 110C @2. Debit 3.
    fn vertical() -> Basket {
        Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0), Leg::new(-1.0, 110.0, true, 2.0)], 100.0)
    }

    /// Long straddle: 100C @5 + 100P @4. Debit 9.
    fn straddle() -> Basket {
        Basket::new(vec![Leg::new(1.0, 100.0, true, 5.0), Leg::new(1.0, 100.0, false, 4.0)], 100.0)
    }

    /// Iron condor: -90P@2 +85P@1 -110C@2 +115C@1. Net credit 2, wings 5 wide.
    fn condor() -> Basket {
        Basket::new(
            vec![
                Leg::new(-1.0, 90.0, false, 2.0),
                Leg::new(1.0, 85.0, false, 1.0),
                Leg::new(-1.0, 110.0, true, 2.0),
                Leg::new(1.0, 115.0, true, 1.0),
            ],
            100.0,
        )
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    // ---- payoff / premium goldens (hand-computed) ----

    #[test]
    fn long_call_payoff_and_bounds() {
        let b = long_call();
        assert_eq!(b.net_premium(), 5.0);
        assert_eq!(b.payoff_at(0.0), -5.0);
        assert_eq!(b.payoff_at(90.0), -5.0);
        assert_eq!(b.payoff_at(100.0), -5.0);
        assert_eq!(b.payoff_at(110.0), 5.0);
        assert_eq!(b.break_evens(), vec![105.0]);
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-5.0));
        assert_eq!(b.max_profit(), PayoffBound::Unbounded);
    }

    #[test]
    fn vertical_spread_payoff_and_bounds() {
        let b = vertical();
        assert_eq!(b.net_premium(), 3.0);
        assert_eq!(b.payoff_at(90.0), -3.0);
        assert_eq!(b.payoff_at(110.0), 7.0);
        assert_eq!(b.payoff_at(200.0), 7.0); // capped by the short wing
        assert_eq!(b.break_evens(), vec![103.0]);
        assert_eq!(b.max_profit(), PayoffBound::Bounded(7.0));
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-3.0));
        assert_eq!(b.upper_slope(), 0.0);
    }

    #[test]
    fn straddle_payoff_and_bounds() {
        let b = straddle();
        assert_eq!(b.net_premium(), 9.0);
        assert_eq!(b.payoff_at(100.0), -9.0);
        assert_eq!(b.payoff_at(120.0), 11.0);
        assert_eq!(b.payoff_at(80.0), 11.0);
        assert_eq!(b.break_evens(), vec![91.0, 109.0]);
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-9.0));
        assert_eq!(b.max_profit(), PayoffBound::Unbounded);
        // Downside is bounded by S = 0: put pays 100, less the 9 debit.
        assert_eq!(b.payoff_at(0.0), 91.0);
    }

    #[test]
    fn iron_condor_payoff_and_bounds() {
        let b = condor();
        assert_eq!(b.net_premium(), -2.0); // net credit
        assert_eq!(b.payoff_at(100.0), 2.0);
        assert_eq!(b.payoff_at(80.0), -3.0);
        assert_eq!(b.payoff_at(200.0), -3.0);
        assert_eq!(b.break_evens(), vec![88.0, 112.0]);
        assert_eq!(b.max_profit(), PayoffBound::Bounded(2.0));
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-3.0));
        assert_eq!(b.payoff_at(88.0), 0.0);
        assert_eq!(b.payoff_at(112.0), 0.0);
    }

    // ---- unbounded detection ----

    #[test]
    fn naked_short_call_loss_is_unbounded_profit_is_not() {
        let b = Basket::new(vec![Leg::new(-1.0, 100.0, true, 5.0)], 100.0);
        assert_eq!(b.max_loss(), PayoffBound::Unbounded);
        assert_eq!(b.max_profit(), PayoffBound::Bounded(5.0));
        assert_eq!(b.break_evens(), vec![105.0]);
    }

    #[test]
    fn naked_short_put_downside_is_bounded_at_zero() {
        let b = Basket::new(vec![Leg::new(-1.0, 100.0, false, 5.0)], 100.0);
        assert_eq!(b.upper_slope(), 0.0);
        assert_eq!(b.max_profit(), PayoffBound::Bounded(5.0));
        // Worst case is S_T = 0: -(100) + 5 credit.
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-95.0));
        assert_eq!(b.break_evens(), vec![95.0]);
    }

    #[test]
    fn ratio_backspread_is_unbounded_up_and_bounded_down() {
        // -1x 100C @6, +2x 110C @2  ⇒ upper slope = +1.
        let b = Basket::new(
            vec![Leg::new(-1.0, 100.0, true, 6.0), Leg::new(2.0, 110.0, true, 2.0)],
            100.0,
        );
        assert_eq!(b.upper_slope(), 1.0);
        assert_eq!(b.max_profit(), PayoffBound::Unbounded);
        assert_eq!(b.net_premium(), -2.0);
        // Worst point is the 110 kink: -(10) + 0 + 2 credit.
        assert_eq!(b.max_loss(), PayoffBound::Bounded(-8.0));
    }

    #[test]
    fn empty_basket_is_flat_and_bounded() {
        let b = Basket::new(vec![], 100.0);
        assert_eq!(b.net_premium(), 0.0);
        assert_eq!(b.payoff_at(123.0), 0.0);
        assert_eq!(b.upper_slope(), 0.0);
        assert_eq!(b.max_profit(), PayoffBound::Bounded(0.0));
        assert_eq!(b.max_loss(), PayoffBound::Bounded(0.0));
        // Flat at exactly zero: the single node touches zero.
        assert_eq!(b.break_evens(), vec![0.0]);
    }

    #[test]
    fn payoff_bound_accessors() {
        assert_eq!(PayoffBound::Bounded(2.5).value(), Some(2.5));
        assert_eq!(PayoffBound::Unbounded.value(), None);
        assert!(PayoffBound::Unbounded.is_unbounded());
        assert!(!PayoffBound::Bounded(0.0).is_unbounded());
    }

    #[test]
    fn right_slope_matches_numeric_derivative() {
        let b = condor();
        for &p in &[70.0, 87.0, 95.0, 100.0, 112.0, 130.0] {
            let analytic = b.slope_at(p);
            let eps = 1e-4;
            let fd = (b.payoff_at(p + eps) - b.payoff_at(p)) / eps;
            assert!(close(analytic, fd, 1e-6), "p={p} analytic={analytic} fd={fd}");
        }
        // At/beyond the highest strike the right-hand slope IS the upper slope.
        assert_eq!(b.slope_at(115.0), b.upper_slope());
        assert_eq!(b.slope_at(1e9), b.upper_slope());
    }

    // ---- net greeks ----

    #[test]
    fn net_greeks_are_bit_identical_to_summed_single_leg_calls() {
        let b = condor();
        let mk = |iv: f64| LegMarket { iv, tau: 0.25, spot: 100.0, r: 0.03 };
        let markets = [mk(0.55), mk(0.60), mk(0.50), mk(0.58)];
        let net = b.net_greeks(&markets).expect("computable");

        let (mut d, mut g, mut th, mut v) = (0.0, 0.0, 0.0, 0.0);
        let (mut vn, mut vm, mut ch, mut vt, mut co) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for (leg, m) in b.legs.iter().zip(markets.iter()) {
            let (ld, lg, lt, lv) =
                black_scholes_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r).unwrap();
            let so = second_order_greeks(m.spot, leg.strike, m.tau, m.iv, leg.kind(), m.r).unwrap();
            d += leg.qty * ld;
            g += leg.qty * lg;
            th += leg.qty * lt;
            v += leg.qty * lv;
            vn += leg.qty * so.vanna;
            vm += leg.qty * so.vomma;
            ch += leg.qty * so.charm;
            vt += leg.qty * so.veta;
            co += leg.qty * so.color;
        }
        // Bit-identical, not merely close.
        assert_eq!(net.delta.to_bits(), d.to_bits());
        assert_eq!(net.gamma.to_bits(), g.to_bits());
        assert_eq!(net.theta.to_bits(), th.to_bits());
        assert_eq!(net.vega.to_bits(), v.to_bits());
        assert_eq!(net.vanna.to_bits(), vn.to_bits());
        assert_eq!(net.vomma.to_bits(), vm.to_bits());
        assert_eq!(net.charm.to_bits(), ch.to_bits());
        assert_eq!(net.veta.to_bits(), vt.to_bits());
        assert_eq!(net.color.to_bits(), co.to_bits());
    }

    #[test]
    fn net_greeks_of_a_straddle_has_near_zero_delta_and_double_gamma() {
        let b = straddle();
        let m = LegMarket { iv: 0.5, tau: 0.25, spot: 100.0, r: 0.0 };
        let net = b.net_greeks(&[m, m]).unwrap();
        // At r = 0 a same-strike call+put straddle: delta = N(d1) + N(d1) - 1, small near ATM.
        assert!(net.delta.abs() < 0.2, "delta={}", net.delta);
        let single = black_scholes_greeks(100.0, 100.0, 0.25, 0.5, OptionKind::Call, 0.0).unwrap();
        assert!(close(net.gamma, 2.0 * single.1, 1e-15));
        assert!(net.vega > 0.0 && net.theta < 0.0);
    }

    #[test]
    fn net_greeks_rejects_length_mismatch_and_invalid_legs() {
        let b = straddle();
        let m = LegMarket { iv: 0.5, tau: 0.25, spot: 100.0, r: 0.0 };
        assert_eq!(b.net_greeks(&[m]), None); // wrong length
        let expired = LegMarket { tau: 0.0, ..m };
        assert_eq!(b.net_greeks(&[m, expired]), None); // one leg not computable
    }

    // ---- POP ----

    fn dist(sigma: f64, tau: f64) -> TerminalDist {
        TerminalDist { spot: 100.0, sigma, tau, r: 0.0 }
    }

    #[test]
    fn pop_of_a_far_otm_short_put_is_near_one() {
        // Short 50P for 5: break-even 45, spot 100, modest vol ⇒ almost surely profitable.
        let b = Basket::new(vec![Leg::new(-1.0, 50.0, false, 5.0)], 100.0);
        let p = b.probability_of_profit(&dist(0.2, 0.08)).unwrap();
        assert!(p > 0.999_999, "pop={p}");
        assert!(p <= 1.0);
    }

    #[test]
    fn pop_of_a_deep_itm_long_call_is_near_one() {
        // Long 10C bought BELOW intrinsic-at-spot: break-even 60 with spot 100 and low vol.
        let b = Basket::new(vec![Leg::new(1.0, 10.0, true, 50.0)], 100.0);
        let p = b.probability_of_profit(&dist(0.2, 0.08)).unwrap();
        assert!(p > 0.999_99, "pop={p}");
    }

    #[test]
    fn pop_of_a_basket_and_its_negation_sum_to_one() {
        // Profit regions are exact complements (break-evens are measure zero).
        let b = straddle();
        let neg = Basket::new(
            b.legs.iter().map(|l| Leg::new(-l.qty, l.strike, l.is_call, l.premium)).collect(),
            b.underlying_spot,
        );
        let d = dist(0.15, 0.25);
        let a = b.probability_of_profit(&d).unwrap();
        let c = neg.probability_of_profit(&d).unwrap();
        assert!(close(a + c, 1.0, 1e-12), "long={a} short={c}");
        // Sanity: at 15% vol the 91/109 break-evens are a stretch, so the long side is the
        // less likely one (raise the vol and this flips -- the complement law above does not).
        assert!(a < 0.5 && c > 0.5, "long={a} short={c}");
    }

    #[test]
    fn pop_of_a_condor_matches_the_direct_interval_probability() {
        let b = condor();
        let d = dist(0.35, 0.25);
        let (mu_ln, s) = d.log_params().unwrap();
        // Profitable exactly on (88, 112) — the two break-evens.
        let direct = d.cdf(112.0, mu_ln, s) - d.cdf(88.0, mu_ln, s);
        let p = b.probability_of_profit(&d).unwrap();
        assert!(close(p, direct, 1e-15), "p={p} direct={direct}");
        assert!(p > 0.0 && p < 1.0);
    }

    #[test]
    fn pop_rejects_degenerate_distributions() {
        let b = straddle();
        assert_eq!(b.probability_of_profit(&dist(0.0, 0.25)), None);
        assert_eq!(b.probability_of_profit(&dist(0.4, 0.0)), None);
        let bad_spot = TerminalDist { spot: 0.0, ..dist(0.4, 0.25) };
        assert_eq!(b.probability_of_profit(&bad_spot), None);
    }

    // ---- EV ----

    #[test]
    fn ev_of_a_zero_premium_long_call_matches_black_scholes_at_zero_rate() {
        // With r = 0 the discount factor is 1, so E[(S_T - K)+] == the BS call price.
        let b = Basket::new(vec![Leg::new(1.0, 105.0, true, 0.0)], 100.0);
        let d = dist(0.4, 0.5);
        let ev = b.expected_value(&d, &EvGrid::default()).unwrap();
        let bs = crate::greeks::black_scholes_price(100.0, 105.0, 0.5, 0.4, OptionKind::Call, 0.0)
            .unwrap();
        assert!(close(ev, bs, 1e-11), "ev={ev} bs={bs}");
    }

    #[test]
    fn ev_of_a_zero_premium_long_put_matches_black_scholes_at_zero_rate() {
        let b = Basket::new(vec![Leg::new(1.0, 95.0, false, 0.0)], 100.0);
        let d = dist(0.4, 0.5);
        let ev = b.expected_value(&d, &EvGrid::default()).unwrap();
        let bs = crate::greeks::black_scholes_price(100.0, 95.0, 0.5, 0.4, OptionKind::Put, 0.0)
            .unwrap();
        assert!(close(ev, bs, 1e-11), "ev={ev} bs={bs}");
    }

    #[test]
    fn ev_converges_as_the_grid_refines() {
        let b = condor();
        let d = dist(0.35, 0.25);
        let coarse = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 1024 }).unwrap();
        let fine = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 16384 }).unwrap();
        assert!(close(coarse, fine, 1e-9), "coarse={coarse} fine={fine}");
        // The trapezoid error should shrink with the step count, not wander.
        let mid = b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 4096 }).unwrap();
        assert!((mid - fine).abs() <= (coarse - fine).abs() + 1e-15);
    }

    #[test]
    fn ev_is_deterministic() {
        let b = straddle();
        let d = dist(0.5, 0.25);
        let a = b.expected_value(&d, &EvGrid::default()).unwrap();
        let c = b.expected_value(&d, &EvGrid::default()).unwrap();
        assert_eq!(a.to_bits(), c.to_bits());
    }

    #[test]
    fn ev_of_a_fairly_priced_straddle_is_near_zero() {
        // Price both legs at their BS value under the same law ⇒ zero-EV bet at r = 0.
        let (s, k, tau, sigma) = (100.0, 100.0, 0.25, 0.5);
        let c =
            crate::greeks::black_scholes_price(s, k, tau, sigma, OptionKind::Call, 0.0).unwrap();
        let p = crate::greeks::black_scholes_price(s, k, tau, sigma, OptionKind::Put, 0.0).unwrap();
        let b = Basket::new(vec![Leg::new(1.0, k, true, c), Leg::new(1.0, k, false, p)], s);
        let ev = b
            .expected_value(&TerminalDist { spot: s, sigma, tau, r: 0.0 }, &EvGrid::default())
            .unwrap();
        // Two legs, so ~2x the single-leg quadrature budget the tests above pin at 1e-11.
        assert!(ev.abs() < 1e-10, "ev={ev}");
    }

    #[test]
    fn ev_rejects_degenerate_grids() {
        let b = straddle();
        let d = dist(0.4, 0.25);
        assert_eq!(b.expected_value(&d, &EvGrid { n_sigma: 10.0, steps: 0 }), None);
        assert_eq!(b.expected_value(&d, &EvGrid { n_sigma: 0.0, steps: 64 }), None);
    }

    // ---- curve sampler ----

    #[test]
    fn payoff_curve_samples_endpoints_and_matches_payoff_at() {
        let b = vertical();
        let pts = b.payoff_curve(80.0, 120.0, 4);
        assert_eq!(pts.len(), 5);
        assert_eq!(pts[0].0, 80.0);
        assert_eq!(pts[4].0, 120.0);
        for (price, pnl) in pts {
            assert_eq!(pnl, b.payoff_at(price));
        }
    }

    #[test]
    fn payoff_curve_around_spot_is_centred_and_guarded() {
        let b = vertical();
        let pts = b.payoff_curve_around_spot(0.2, 10);
        assert_eq!(pts.len(), 11);
        assert!(close(pts[0].0, 80.0, 1e-12));
        assert!(close(pts[10].0, 120.0, 1e-12));
        assert!(Basket::new(vec![], 0.0).payoff_curve_around_spot(0.2, 10).is_empty());
        assert!(b.payoff_curve_around_spot(0.0, 10).is_empty());
    }

    #[test]
    fn payoff_curve_rejects_degenerate_ranges() {
        let b = vertical();
        assert!(b.payoff_curve(80.0, 120.0, 0).is_empty());
        assert!(b.payoff_curve(120.0, 80.0, 4).is_empty());
        assert!(b.payoff_curve(-1.0, 80.0, 4).is_empty());
    }
}
