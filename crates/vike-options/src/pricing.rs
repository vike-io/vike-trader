//! Black–Scholes pricing: first-order greeks, implied volatility and the second-order greeks built
//! on the SAME `d1`/`d2`/`erf` internals. Every transcendental reaches the platform through the
//! `libm` crate (decision 0032), which is what `tests/libm_platform_probe.rs` pins.
//!
//! - [`greeks`] — price, Δ/Γ/Θ/V, [`implied_vol`](greeks::implied_vol), `norm_cdf`/`norm_pdf`.
//! - [`second_order`] — vanna/vomma/charm/veta/color.

pub mod greeks;
pub mod second_order;
