//! REST **order-rate** gate for the OKX exec transport (net-hardening spec §A).
//!
//! OKX meters request COUNT (no Binance-style weight). The public candles/history path draws the IP
//! pool and self-throttles in `data.rs` (200ms page delay ≈ 10 req/2s); only the order path needs a
//! static gate.
//!
//! **The numbers live in [`vike_model::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table
//! row -> `RateGate`.
//!
//! What the `OKX` row records, verified 2026-07 from OKX v5 docs:
//! - **orders** — place order = **60 / 2s** keyed to *User ID + Instrument ID* (i.e. per-instrument),
//!   with a **1000 / 2s** sub-account aggregate ceiling above it. Because each `OkxPerpRest` (and its
//!   transport) is per-symbol, a per-transport gate maps onto the per-instrument bucket, and the
//!   table gates ~17% under the 60/2s budget (50/2s) — the WIDEST margin on the roster, which is why
//!   the table stores each venue's `admitted` verbatim instead of deriving every gate from one
//!   shared fraction. N symbols run N such gates (≈ N × 50/2s), which stays under the 1000/2s
//!   sub-account ceiling for up to ~20 symbols, so multi-symbol trading isn't throttled while a
//!   runaway loop on any single instId trips its gate.
//! - **ws_sends** — **480 subscribe/unsubscribe/login requests per connection per hour** (v5 WS
//!   docs). OKX counts all three toward ONE per-connection budget, so a single shared default gate
//!   is the right model. Sized ~8% under 480/hr as a coarse floor: normal operation (a login + a
//!   subscribe per connect, reconnecting occasionally) is far under it, while a subscription-churn
//!   loop or a pathological reconnect flap trips the gate with a visible warn.

use vike_bridge_core::ratelimit::{KeyedRateGate, RateGate};
use vike_model::venue_rate_limits::OKX;

/// Order-rate gate for one OKX per-symbol exec transport. Attach via
/// `UreqOkxTransport::with_rate_gate(rest_rate_gate())`.
pub fn rest_rate_gate() -> RateGate {
    let m = OKX.orders;
    RateGate::new(m.admitted(), m.window())
}

/// Keyed WS-send gate for the OKX user-data pump: one shared window covering `login` + `subscribe`
/// (OKX's combined 480/hr per-connection budget). Built ONCE per pump and persists across reconnects
/// (more conservative than OKX's per-connection reset — it also brakes a reconnect storm's
/// cumulative subscribe sends).
pub fn ws_rate_gate() -> KeyedRateGate {
    let m = OKX.ws_sends;
    KeyedRateGate::new("okx", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_okx_row() {
        assert_eq!(OKX.orders.admitted(), 50); // 60/2s per instrument, ~17% under
        assert_eq!(OKX.orders.window(), Duration::from_secs(2));
        assert_eq!(OKX.ws_sends.admitted(), 440); // 480/hr per connection, ~8% under
        assert_eq!(OKX.ws_sends.window(), Duration::from_secs(3600));
        assert!(OKX.orders.admitted() < OKX.orders.published());
        assert!(OKX.ws_sends.admitted() < OKX.ws_sends.published());
    }

    #[test]
    fn gate_admits_its_budget_then_throttles() {
        let g = rest_rate_gate();
        for _ in 0..OKX.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 51st op in the 2s window is throttled");
    }
}
