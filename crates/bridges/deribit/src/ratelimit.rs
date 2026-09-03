//! Keyed WS-send gates for Deribit (net-hardening spec §A). Deribit runs TWO independent sockets,
//! metered by separate credit pools, so it gets two gates:
//!
//! - the **order** socket ([`DeribitOrderTransport`](crate::transport::DeribitOrderTransport)) sends
//!   `private/buy|sell|cancel|edit` JSON-RPC — **matching-engine** requests, limited per docs to
//!   ~5 req/s sustained on the default tier (Tier4, <$1M 7-day volume; higher tiers allow more). Its
//!   auth + reconcile reads are non-matching-engine. → [`order_ws_gate`].
//! - the **fill stream** ([`crate::user_data::open_deribit_user_data_ws`]) sends auth + `subscribe` —
//!   **non-matching-engine** (~20 req/s sustained). → [`ws_rate_gate`].
//!
//! The order gate is the load-bearing one: it paces a market-maker's cancel/replace burst to the
//! sustainable ~5/s instead of racing into `too_many_requests` rejections (stale quotes → adverse
//! fills). Built once per transport/pump; persists across reconnects.
//!
//! **The numbers live in [`vike_model::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table
//! row -> `RateGate`. The `DERIBIT` row maps the two credit pools onto its two meters —
//! `orders` = matching-engine, `ws_sends` = non-matching-engine — and records one thing worth
//! knowing about this venue: its matching-engine gate sits at **exactly** the published 5/s, the
//! only zero-margin meter on the roster. That is why the table's compile-time invariant is
//! `admitted <= published` and not a strict inequality, and it is checkable now only because the
//! published cap is stored beside the admitted rate instead of being divided out into one integer.

use vike_bridge_core::ratelimit::{KeyedRateGate, RateGate};
use vike_model::venue_rate_limits::DERIBIT;

/// Gate for the Deribit **order** socket: key `"order"` → matching-engine 5/s; everything else
/// (`"login"`, reconcile reads) → the non-matching-engine default.
pub fn order_ws_gate() -> KeyedRateGate {
    let me = DERIBIT.orders;
    let non_me = DERIBIT.ws_sends;
    KeyedRateGate::new(
        "deribit",
        vec![("order", RateGate::new(me.admitted(), me.window()))],
        Some(RateGate::new(non_me.admitted(), non_me.window())),
    )
}

/// Gate for the Deribit **fill-stream** pump: auth + subscribe are non-matching-engine (~20/s),
/// so a single shared default suffices.
pub fn ws_rate_gate() -> KeyedRateGate {
    let m = DERIBIT.ws_sends;
    KeyedRateGate::new("deribit", vec![], Some(RateGate::new(m.admitted(), m.window())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gates are built from the table, so this pins the WIRING: the literals are the budgets
    /// these gates carried as local consts before the per-venue table existed.
    #[test]
    fn the_gates_are_built_from_the_venue_tables_deribit_row() {
        // Matching engine: gated at EXACTLY the published rate — the roster's only zero-margin gate.
        assert_eq!(DERIBIT.orders.admitted(), 5);
        assert_eq!(DERIBIT.orders.published(), 5);
        assert_eq!(DERIBIT.orders.window(), Duration::from_secs(1));
        // Non-matching engine: 20/s published, gated ~10% under.
        assert_eq!(DERIBIT.ws_sends.admitted(), 18);
        assert_eq!(DERIBIT.ws_sends.published(), 20);
        assert_eq!(DERIBIT.ws_sends.window(), Duration::from_secs(1));
    }
}
