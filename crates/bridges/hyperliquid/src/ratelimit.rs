//! Per-IP REST **weight** gate for the Hyperliquid transport (net-hardening spec §A).
//!
//! Hyperliquid meters ONE budget: `1200 weight per rolling minute per IP`, and **all** REST traffic
//! draws it — `/info` reads and signed `/exchange` actions alike. There is no separate order
//! counter, so this single gate is what paces order flow here; a burst of `userRole` reads (weight
//! 60 each) genuinely competes with order submits inside the same minute.
//!
//! **The numbers live in [`vike_model::venue_rate_limits`], not here** — see that module for why a
//! venue's published cap is a fact rather than a setting. This module is only the WIRING: table row
//! -> `RateGate`, the same shape as `vike_binance::ratelimit` / `vike_okx::ratelimit`.
//!
//! What the `HYPERLIQUID` row records (transcribed from HL's published docs in the 2026-07-16
//! adapter research pass, §9):
//! - **`rest_ip_weight`** — 1200 weight / 60 s, per IP. The table's `admitted` column sits AT the
//!   cap (zero margin, the roster's second such meter after deribit's matching-engine gate) — that
//!   is the behaviour this gate has always had, and adding headroom would be a retune, not a move.
//! - `orders` / `ws_sends` are `Meter::Ungated` and that is the finding, not an omission: HL meters
//!   neither separately, so the pool above is the only budget there is to gate.
//!
//! The per-endpoint weight SCHEDULE stays in [`crate::transport`] (`info_weight` /
//! `exchange_weight`): it is a cost map over this venue's endpoints, not a budget, and the table
//! deliberately carries budgets only. Callers charge it through `RateGate::proceed_cost` — for a
//! weight pool the gate's slots ARE weight units, where a count meter charges one slot per call.
//!
//! **Sharing the window is the MOUNT's job, and the set is exec + funding poll + recon.** The
//! budget is per-IP rather than per-client, so a live mount resolves [`ip_weight_gate`] ONCE
//! (`vike_mount::hyperliquid`'s `hl_ip_gate_and_transport`) and clones that one handle into the
//! exec thread's signed `/exchange` transport, the funding poller's keyless `/info` transport and
//! the reconcile transport, through
//! [`crate::transport::HyperliquidTransport::with_rate_gate`]. `HyperliquidTransport::new` mints a
//! FRESH window on every call, so a consumer that skips that seam does not run a cautious gate —
//! it runs a second full budget the first one cannot see, against the zero-margin row above.
//!
//! ⚠ **"feeds" is not in that set and never was.** [`crate::market_feed`], [`crate::market_data`]
//! and [`crate::user_data`] hold no [`crate::transport::HyperliquidTransport`] at all — they are WS
//! pumps and draw no REST weight — so naming feeds here describes traffic that does not exist while
//! omitting the funding poller, which does. The REST callers OUTSIDE a mount ([`crate::catalog`]'s
//! instrument listing, [`crate::transport::HyperliquidTransport::with_agent`]'s clock canary,
//! `vike-app`'s feed-symbology load, and the backfill collectors, which are separate processes) each
//! open a one-shot window of their own: a startup or batch burst rather than a sustained lane, and
//! not wired through the mount's handle today.

use vike_bridge_core::ratelimit::RateGate;
use vike_model::venue_rate_limits::HYPERLIQUID;

/// A per-IP REST weight gate, sized from [`HYPERLIQUID`]'s `rest_ip_weight` row.
///
/// ⚠ **Every call builds a NEW window, and the budget behind it is per-IP** — so calling this twice
/// in one process is not twice as careful, it spends twice the venue's cap. A unit with more than
/// one REST path calls it ONCE and clones the handle into each transport with
/// [`HyperliquidTransport::with_rate_gate`](crate::transport::HyperliquidTransport::with_rate_gate);
/// `vike_mount::hyperliquid`'s `hl_ip_gate_and_transport` is that call for a live mount, covering
/// the exec thread, the funding poller and the reconcile client.
/// [`crate::transport::HyperliquidTransport::new`] builds a fresh one from here, which is the right
/// answer only for a caller that is its process's sole Hyperliquid REST consumer.
pub fn ip_weight_gate() -> RateGate {
    let m = HYPERLIQUID.rest_ip_weight;
    RateGate::new(m.admitted(), m.window())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// The gate is built from the table, so this pins the WIRING: the literals are the budget this
    /// gate carried as `transport.rs`'s local `IP_WEIGHT_PER_MIN`/`RATE_WINDOW` consts before the
    /// per-venue table existed. A drift in the table's hyperliquid row would change live pacing on
    /// every HL REST call — reads and orders both — and fail here.
    #[test]
    fn the_gate_is_built_from_the_venue_tables_hyperliquid_row() {
        assert_eq!(HYPERLIQUID.rest_ip_weight.admitted(), 1200);
        assert_eq!(HYPERLIQUID.rest_ip_weight.window(), Duration::from_secs(60));
        // ...and the admitted rate IS the published cap: this meter has no safety margin today.
        assert_eq!(HYPERLIQUID.rest_ip_weight.published(), 1200);
    }

    /// HL meters no separate order counter — the pool above is what paces order flow. Pinned so a
    /// future edit that "fills in" an orders row has to justify it against the venue.
    #[test]
    fn hyperliquid_declares_no_separate_order_or_ws_meter() {
        assert_eq!(HYPERLIQUID.orders, vike_model::Meter::Ungated);
        assert_eq!(HYPERLIQUID.ws_sends, vike_model::Meter::Ungated);
    }

    #[test]
    fn gate_admits_its_budget_then_throttles() {
        let g = ip_weight_gate();
        // Charged in WEIGHT, not requests: 600 weight-2 `/info` reads exhaust the minute.
        for _ in 0..600 {
            assert!(g.try_proceed_cost(2));
        }
        assert!(!g.try_proceed(), "the 1201st weight unit in the 60s window is throttled");
    }
}
