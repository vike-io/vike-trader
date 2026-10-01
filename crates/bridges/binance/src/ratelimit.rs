//! Per-IP REST **order-rate** gates for the Binance exec transport (net-hardening spec §A).
//!
//! Sized to the `ORDERS` meter that binds order submit/cancel — a SEPARATE per-IP counter from the
//! `REQUEST_WEIGHT` pool that klines/history draw on. The backfill path (`data.rs`) self-throttles
//! off the live `X-MBX-USED-WEIGHT-1m` header and needs no static gate; only the order path does.
//!
//! **The numbers live in [`vike_model::venue_rate_limits`], not here.** A venue's published cap is a
//! fact about the venue — true for everyone, never ours to raise, and an HTTP 418 IP ban if it is —
//! so it belongs in the workspace's per-venue capability table beside `VenueCaps`/`VenueMarginSupport`/
//! `FeeSchedule`, where a completeness test over `vike_model::VENUES` forces every venue to declare
//! one and a `const _: ()` per row proves at COMPILE time that what vike admits stays under what the
//! venue published. This module is now only the WIRING: table row -> `RateGate`.
//!
//! What those rows record for Binance (both read from the venues' own live `exchangeInfo`
//! `rateLimits`, 2026-07): SPOT `ORDERS` = 100 / 10 s (`api.binance.com`); USDS-M `ORDERS` =
//! 300 / 10 s (`fapi.binance.com`). The table's `admitted` column gates ~10% under each cap — a
//! coarse burst brake, so a mass-cancel-per-order loop or reconnect storm trips the gate with a
//! visible `warn` instead of racing `-1015`/429 -> 418 IP ban, while legitimate order flow still
//! runs up to the venue budget. Spot and USDS-M keep SEPARATE gates because their order caps differ
//! (and they are distinct REST clients) — which is exactly why the table is keyed on
//! `(venue, market)` rather than on venue alone.

use vike_bridge_core::ratelimit::{KeyedRateGate, RateGate};
use vike_model::venue_rate_limits::{BINANCE_PERP, BINANCE_SPOT};

/// Order-rate gate for the Binance **spot** exec transport. Attach via
/// `UreqTransport::with_rate_gate(spot_rest_gate())`.
pub fn spot_rest_gate() -> RateGate {
    let m = BINANCE_SPOT.orders;
    RateGate::new(m.admitted(), m.window())
}

/// Order-rate gate for the Binance **USDS-M futures** exec transport.
pub fn perp_rest_gate() -> RateGate {
    let m = BINANCE_PERP.orders;
    RateGate::new(m.admitted(), m.window())
}

/// Keyed WS-send gate for the Binance spot user-data pump: one shared default window, sized under
/// Binance's documented **5 incoming (client->server) messages per second per connection** (WS-API
/// `General Info`; PING/PONG frames and every request count toward the same per-connection budget).
///
/// The spot user-data handshake sends exactly ONE combined subscribe+auth frame per connection, so
/// the gate is a coarse floor well under 5/s and rarely bites in normal operation. Sized ~20% under
/// as a runaway floor: a reconnect-flap or a pathological resubscribe loop trips the gate with a
/// visible `warn` instead of racing the venue's per-connection cutoff. No keyed sub-buckets — one
/// shared default window covers the single handshake send. Built once per pump and persists across
/// reconnects (more conservative than Binance's per-connection reset — it also brakes a reconnect
/// storm's cumulative subscribe sends).
///
/// The WS meter is per-CONNECTION, not per-market, so the table's spot and perp rows carry the same
/// numbers and either would serve here.
pub fn ws_rate_gate() -> KeyedRateGate {
    let m = BINANCE_SPOT.ws_sends;
    KeyedRateGate::new("binance", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed, and a drift in the
    /// table's binance rows would change live order pacing and fail here.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_binance_rows() {
        assert_eq!(BINANCE_SPOT.orders.admitted(), 90); // 100/10s, ~10% under
        assert_eq!(BINANCE_PERP.orders.admitted(), 270); // 300/10s, ~10% under
        assert_eq!(BINANCE_SPOT.ws_sends.admitted(), 4); // 5/s per connection, ~20% under
        for m in [BINANCE_SPOT.orders, BINANCE_PERP.orders] {
            assert_eq!(m.window(), Duration::from_secs(10), "binance meters ORDERS per 10s");
        }
        assert_eq!(BINANCE_SPOT.ws_sends.window(), Duration::from_secs(1));
        // ...and each admitted rate is genuinely under the cap the venue published.
        assert!(BINANCE_SPOT.orders.admitted() < BINANCE_SPOT.orders.published());
        assert!(BINANCE_PERP.orders.admitted() < BINANCE_PERP.orders.published());
    }

    #[test]
    fn spot_gate_admits_its_budget_then_throttles() {
        let g = spot_rest_gate();
        for _ in 0..BINANCE_SPOT.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 91st request in the window is throttled");
    }

    #[test]
    fn perp_gate_admits_its_budget_then_throttles() {
        let g = perp_rest_gate();
        for _ in 0..BINANCE_PERP.orders.admitted() {
            assert!(g.try_proceed());
        }
        assert!(!g.try_proceed(), "the 271st request in the window is throttled");
    }
}
