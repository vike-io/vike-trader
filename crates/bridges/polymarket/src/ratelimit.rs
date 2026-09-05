//! Keyed WS-send gate for the Polymarket CLOB user channel (net-hardening spec §A).
//!
//! Polymarket's CLOB WebSocket documents no explicit subscribe/message RATE limit (the user channel
//! takes one auth-bearing subscribe per connection, then a 10s `PING` keepalive). So this gate is a
//! pure RUNAWAY-CATCHER — sized generously so it never bites legitimate operation but clamps a
//! pathological subscribe/reconnect loop. Built once per pump; persists across reconnects. Orders
//! are REST and carry no rate gate today; this covers only the WS subscribe path.
//!
//! **The number lives in [`vike_model::venue_rate_limits`], not here.** The `POLYMARKET` row records
//! it as `Meter::Unpublished` and the row's provenance as `Provenance::Unpublished`, which is the
//! honest encoding: there is no venue cap to be under, so the rate below is vike's OWN choice and
//! asking the table for a "published" Polymarket WS cap is a panic rather than a plausible integer.
//! Keeping it in the shared table anyway is the point — a reader comparing venues sees which brakes
//! are the venue's and which are ours, which a bare local `const` could never tell them.

use vike_bridge_core::ratelimit::{KeyedRateGate, RateGate};
use vike_model::venue_rate_limits::POLYMARKET;

/// Keyed WS-send gate for the Polymarket user-data pump (shared default; the `subscribe` key falls
/// to it). Built once per pump, cloned across reconnects. ~3/s — a generous runaway floor, with no
/// documented Polymarket WS subscribe rate to match.
pub fn ws_rate_gate() -> KeyedRateGate {
    let m = POLYMARKET.ws_sends;
    KeyedRateGate::new("polymarket", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate is built from the table, so this pins the WIRING: the literal is the budget this
    /// gate carried as a local const before the per-venue table existed.
    #[test]
    fn the_gate_is_built_from_the_venue_tables_polymarket_row() {
        assert_eq!(POLYMARKET.ws_sends.admitted(), 30);
        assert_eq!(POLYMARKET.ws_sends.window(), Duration::from_secs(10));
        // The brake is OURS: the venue publishes no WS rate, and the table says so.
        assert_eq!(POLYMARKET.ws_sends.published_cap(), None);
    }
}
