//! REST **order-rate** gate for the Bybit exec transport (net-hardening spec §A).
//!
//! Bybit meters HTTP requests (no Binance-style weight system): a per-IP ceiling of **600 / 5s**
//! (breach → HTTP 403 + a 10-minute IP ban) and per-UID order-op limits. The public kline/history
//! path draws only the IP pool and self-throttles in `data.rs` (200ms page delay ≈ 5 req/s), so
//! only the order path needs a static gate.
//!
//! **The numbers live in [`vike_model::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table
//! row -> `RateGate`.
//!
//! What the `BYBIT` row records, and the two shapes it distinguishes:
//! - **orders** — verified 2026-07 from the
//!   [v5 rate-limit docs](https://bybit-exchange.github.io/docs/v5/rate-limit): per-UID **linear
//!   create = 20/s, cancel = 20/s** (amend is a tighter 10/s bucket). The table gates ~10% under the
//!   20/s create/cancel budget as a coarse burst brake — 1/6.6 of the 600/5s IP ban, so a mass-cancel
//!   loop trips the gate (visible `warn`) far below the ban threshold. Amend's separate 10/s bucket
//!   is not distinctly gated (amend-heavy bursts are rare and rejections are recoverable).
//! - **ws_sends** — Bybit documents **NO** WebSocket message-rate limit at all (only a
//!   500-connections/5-min cap and a structural args-per-subscribe cap, both enforced elsewhere), so
//!   the table records `Meter::Unpublished`: the rate below is vike's OWN runaway brake and asking
//!   the table for a "published" WS cap is a panic rather than a plausible integer. That distinction
//!   is the whole reason the table stores `published` and `admitted` separately instead of one
//!   number nobody could tell the origin of.

use vike_bridge_core::ratelimit::{KeyedRateGate, RateGate};
use vike_model::venue_rate_limits::BYBIT;

/// Order-rate gate for the Bybit exec transport. Attach via
/// `UreqBybitTransport::with_rate_gate(rest_rate_gate())`.
pub fn rest_rate_gate() -> RateGate {
    let m = BYBIT.orders;
    RateGate::new(m.admitted(), m.window())
}

/// Keyed WS-send gate for the Bybit user-data pump — purely a RUNAWAY-CATCHER for a pathological
/// subscribe/reconnect loop, since Bybit publishes no WS message-rate limit to size it against.
/// Sized generously (30/10s ≈ 3/s) so it never bites legitimate use (a login + a subscribe per
/// connect, reconnecting occasionally) yet still brakes a flapping loop with a visible warn. Built
/// once per pump and cloned across reconnects (a single shared default window covers both `login`
/// and `subscribe`).
pub fn ws_rate_gate() -> KeyedRateGate {
    let m = BYBIT.ws_sends;
    KeyedRateGate::new("bybit", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_bybit_row() {
        assert_eq!(BYBIT.orders.admitted(), 18); // 20/s per-UID create/cancel, ~10% under
        assert_eq!(BYBIT.orders.window(), Duration::from_secs(1));
        assert!(BYBIT.orders.admitted() < BYBIT.orders.published());
        // The WS brake is OURS: the venue publishes no cap, and the table says so rather than
        // fabricating one.
        assert_eq!(BYBIT.ws_sends.admitted(), 30);
        assert_eq!(BYBIT.ws_sends.window(), Duration::from_secs(10));
        assert_eq!(BYBIT.ws_sends.published_cap(), None, "bybit documents no WS message rate");
    }

    #[test]
    fn gate_admits_its_budget_then_throttles() {
        let g = rest_rate_gate();
        for _ in 0..BYBIT.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 19th op in the 1s window is throttled");
    }
}
