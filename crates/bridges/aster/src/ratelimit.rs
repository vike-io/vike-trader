//! Per-IP REST **order-rate** gates for the Aster exec transport (net-hardening spec §A;
//! Task 12). Ported from `vike-binance`'s `ratelimit.rs` — the same brake shape, re-keyed and
//! re-budgeted for Aster's own limits.
//!
//! Sized to the `ORDERS` meter that binds order submit/cancel — a SEPARATE per-IP counter from
//! any request-weight pool that klines/history draw on.
//!
//! **The numbers live in [`vike_venues::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table
//! row -> `RateGate`.
//!
//! What the rows record for Aster (per the Aster API docs, transcribed when this bridge landed
//! 2026-07): SPOT `ORDERS` = 100/min; USDⓈ-M futures `ORDERS` = 1200/min — BOTH metered per MINUTE,
//! unlike Binance's 10-second `ORDERS` window, so the gate window is 60s here. ⚠ That window is the
//! divergence that bites: the identical `admitted: 90` is a 6x tighter gate on Aster than on
//! Binance, which is exactly the sort of thing a bare integer in a bridge crate hides and a
//! `(published, admitted, window)` row in a shared table does not.
//!
//! The table's `admitted` column gates ~10% under each cap: a coarse burst brake (a
//! mass-cancel-per-order loop or reconnect storm trips the gate with a visible `warn` instead of
//! racing a 429/ban), while legitimate order flow still runs up to the venue budget. Spot and USDⓈ-M
//! keep SEPARATE gates because their order caps differ by 12x (and they are distinct REST clients).

use vike_bridge_core::ratelimit::RateGate;
use vike_venues::venue_rate_limits::{ASTER_PERP, ASTER_SPOT};

/// Order-rate gate for the Aster **spot** exec transport. Attach via
/// `UreqTransport::with_rate_gate(spot_rest_gate())`.
pub fn spot_rest_gate() -> RateGate {
    let m = ASTER_SPOT.orders;
    RateGate::new(m.admitted(), m.window())
}

/// Order-rate gate for the Aster **USDⓈ-M futures** exec transport.
pub fn perp_rest_gate() -> RateGate {
    let m = ASTER_PERP.orders;
    RateGate::new(m.admitted(), m.window())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_aster_rows() {
        assert_eq!(ASTER_SPOT.orders.admitted(), 90); // 100/min, ~10% under
        assert_eq!(ASTER_PERP.orders.admitted(), 1080); // 1200/min, ~10% under
        assert_eq!(ASTER_SPOT.ws_sends.admitted(), 8); // 10/s per connection, ~20% under

        // ⚠ the window Aster meters ORDERS over is a MINUTE, not Binance's 10 seconds.
        for m in [ASTER_SPOT.orders, ASTER_PERP.orders] {
            assert_eq!(m.window(), Duration::from_secs(60), "aster meters ORDERS per minute");
        }
        assert_eq!(ASTER_SPOT.ws_sends.window(), Duration::from_secs(1));
        assert!(ASTER_SPOT.orders.admitted() < ASTER_SPOT.orders.published());
        assert!(ASTER_PERP.orders.admitted() < ASTER_PERP.orders.published());
    }

    #[test]
    fn spot_gate_admits_its_budget_then_throttles() {
        let g = spot_rest_gate();
        for _ in 0..ASTER_SPOT.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 91st request in the window is throttled");
    }

    #[test]
    fn perp_gate_admits_its_budget_then_throttles() {
        let g = perp_rest_gate();
        for _ in 0..ASTER_PERP.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 1081st request in the window is throttled");
    }
}
