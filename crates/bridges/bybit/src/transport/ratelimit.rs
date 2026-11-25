//! REST **order-rate** gate for the Bybit exec transport (net-hardening spec §A).
//!
//! Bybit meters HTTP requests (no Binance-style weight system): a per-IP ceiling of **600 / 5s**
//! (breach → HTTP 403 + a 10-minute IP ban) and per-UID order-op limits. The public kline/history
//! path draws only the IP pool and self-throttles in `history.rs` (200ms page delay ≈ 5 req/s), so
//! only the order path needs a static gate.
//!
//! **The numbers live in [`vike_venues::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table
//! row -> `RateGate`.
//!
//! What the `BYBIT` row records, and the two shapes it distinguishes:
//! - **orders** — verified 2026-11-19 from the
//!   [v5 rate-limit docs](https://bybit-exchange.github.io/docs/v5/rate-limit): per-UID **linear
//!   create = 10/s, amend = 10/s, cancel = 10/s** (demo: the default, not upgradable). The one window this
//!   gate keeps covers all three, so it sits ~10% under one bucket, at 9/s: a mass-cancel or requote loop trips
//!   the gate (visible `warn`) before the venue refuses it. The `BYBIT` row's doc has why one number.
//! - **websocket_sends** — Bybit documents **NO** WebSocket message-rate limit at all (only a
//!   500-connections/5-min cap and a structural args-per-subscribe cap, both enforced elsewhere), so
//!   the table records `Meter::Unpublished`: the rate below is vike's OWN runaway brake and asking
//!   the table for a "published" WS cap is a panic rather than a plausible integer. That distinction
//!   is the whole reason the table stores `published` and `admitted` separately instead of one
//!   number nobody could tell the origin of.

use vike_bridge_core::ratelimit::{RateGate, RateGatePerKey};
use vike_venues::venue_rate_limits::BYBIT;

/// Order-rate gate for the Bybit exec transport. Attach via
/// `rest_transport().with_rate_gate(rest_rate_gate())`.
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
pub fn websocket_rate_gate() -> RateGatePerKey {
    let m = BYBIT.websocket_sends;
    RateGatePerKey::new("bybit", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_bybit_row() {
        assert_eq!(BYBIT.orders.admitted(), 9); // 10/s per-UID linear create/amend/cancel, ~10% under
        assert_eq!(BYBIT.orders.window(), Duration::from_secs(1));
        assert!(BYBIT.orders.admitted() < BYBIT.orders.published());
        // The WS brake is OURS: the venue publishes no cap, and the table says so rather than
        // fabricating one.
        assert_eq!(BYBIT.websocket_sends.admitted(), 30);
        assert_eq!(BYBIT.websocket_sends.window(), Duration::from_secs(10));
        assert_eq!(
            BYBIT.websocket_sends.published_cap(),
            None,
            "bybit documents no WS message rate"
        );
    }

    #[test]
    fn gate_admits_its_budget_then_throttles() {
        let g = rest_rate_gate();
        for _ in 0..BYBIT.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 10th op in the 1s window is throttled");
    }
}
