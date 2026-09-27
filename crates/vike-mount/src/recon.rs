//! Venue → `ReconClient` construction, extracted verbatim from `vike-app/src/main.rs`.
//!
//! ReconFactory seam (wave-2 task 6): each construction now delegates to that venue's OWN
//! `pub fn recon_client(...)` factory (`vike_bybit::recon_client`/`vike_okx::recon_client`/
//! `vike_binance::recon_client`) — the actual signer/transport/URL wiring lives in the bridge crate
//! next to the `ReconClient` impl it builds, not here. [`build_recon_client`] is now a thin venue
//! dispatch over `make_engine`'s bybit/okx/binance arms (the `mainnet` argument was added by the
//! STEP-2 `{VENUE}_MAINNET` convergence — see that function's doc).
//! Deribit/Aster/Hyperliquid/Polymarket are NOT dispatched here (they need non-REST or
//! bespoke-signer handshakes) — those arms call their own bridge's `recon_client` factory INLINE in
//! `make_engine` / [`crate::hyperliquid`] instead; see the doc on [`build_recon_client`] for why.

/// Venue -> `ReconClient` dispatch (audit A1 item 4; ReconFactory seam wave-2 task 6): a thin match
/// over each bridge's OWN `recon_client` factory, reusing the SAME `Credentials` each venue's
/// `ExecutionClient`/`spawn_with_recorder` already builds from (see `make_engine`'s live arms,
/// below) — no new transport is invented, and no wiring detail lives in this crate anymore.
///
/// `okx_ct_val` is the caller's already-fetched (or fallback) OKX contracts->base scale factor,
/// threaded straight through to `vike_okx::recon_client` so this function stays network-free/pure —
/// the one `fetch_okx_instrument` call stays in `make_engine`, alongside the existing RiskLimits
/// pre-fetch that already needs the same response. Unused for every other venue.
///
/// `mainnet` is the venue's already-resolved `{VENUE}_MAINNET` verdict (`make_engine` resolves it
/// ONCE per mount via `cex_mainnet_enabled`, which reads BOTH the process env and the workspace
/// `.env` map — the STEP-2 converged rule, `vike_bridge_core::mainnet`). It is threaded straight
/// through to each bridge's factory so a mainnet mount reconciles against the MAINNET account: the
/// same verdict the exec client binds its hosts to, never a second independent read. Unused for
/// every venue with no mainnet switch (they ignore it by never taking it).
///
/// Deribit is NOT dispatched here (falls through to `None`) — but it IS reconciled: its
/// `ReconClient` (`vike_deribit::recon_client`, over `DeribitReconClient::connect`) needs a
/// BLOCKING authed-WS handshake, unlike the pure stateless-REST clients the bybit/okx/binance
/// factories build, so it is called INLINE in `make_engine`'s `("deribit", Some(c))` arm instead (a
/// dedicated recon socket, isolated from the exec order path — see that arm's comment). Paper
/// venues (absent credentials) also return `None` via the wildcard arm — the same
/// absent-credentials-is-the-live-gate rule `make_engine`'s exec arms already follow.
pub fn build_recon_client(
    venue: &str,
    symbol: &str,
    creds: &vike_bridge_core::Credentials,
    okx_ct_val: f64,
    mainnet: bool,
) -> Option<Box<dyn vike_exec::recon::ReconClient>> {
    match venue {
        "bybit" => vike_bybit::recon_client(creds, symbol, mainnet),
        "okx" => vike_okx::recon_client(creds, symbol, okx_ct_val, mainnet),
        "binance" => vike_binance::recon_client(creds, symbol, mainnet),
        // Deribit (not yet wired) + paper/unknown venues: see the doc comment above.
        _ => None,
    }
}

#[cfg(test)]
mod recon_client_tests {
    use super::*;

    fn demo_creds() -> vike_bridge_core::Credentials {
        vike_bridge_core::Credentials {
            api_key: "test-key".to_string(),
            api_secret: "test-secret".to_string(),
            passphrase: Some("test-pass".to_string()),
        }
    }

    #[test]
    fn credentialed_bybit_yields_a_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("bybit", "BTCUSDT", &c, 0.0, false).is_some());
    }

    #[test]
    fn credentialed_okx_yields_a_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("okx", "BTC-USDT-SWAP", &c, 0.01, false).is_some());
    }

    #[test]
    fn credentialed_binance_spot_yields_a_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("binance", "BTCUSDT", &c, 0.0, false).is_some());
    }

    #[test]
    fn credentialed_binance_perp_yields_a_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("binance", "BTCUSDT.P", &c, 0.0, false).is_some());
    }

    /// The dispatch is network-free and infallible either way, so an ARMED mainnet verdict still
    /// yields a client for all three CEX venues — the flag selects hosts/headers inside each
    /// bridge's factory, it never gates construction.
    #[test]
    fn an_armed_mainnet_verdict_still_yields_a_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("bybit", "BTCUSDT", &c, 0.0, true).is_some());
        assert!(build_recon_client("okx", "BTC-USDT-SWAP", &c, 0.01, true).is_some());
        assert!(build_recon_client("binance", "BTCUSDT.P", &c, 0.0, true).is_some());
    }

    /// `build_recon_client` returns `None` for deribit BY DESIGN — deribit IS reconciled, but via its
    /// `make_engine` arm (a dedicated authed order-WS), not this REST-creds factory (see the
    /// function's doc comment). Asserted here so the factory's deliberate `None` can't silently drift.
    #[test]
    fn build_recon_client_returns_none_for_deribit_by_design() {
        let c = demo_creds();
        assert!(build_recon_client("deribit", "BTC-PERPETUAL", &c, 0.0, false).is_none());
    }

    /// `build_recon_client` returns `None` for polymarket BY DESIGN — polymarket IS reconciled now,
    /// but via its `make_engine` arm (behind vike-mount's `polymarket` feature), which calls
    /// `vike_polymarket::recon_client_from_vars`: that needs the workspace `.env` map, an L1→EOA
    /// derivation and a blocking L2 `/auth/derive-api-key` round-trip, none of which this generic
    /// `Credentials`-shaped REST factory can produce. Same deliberate-`None` shape as deribit above.
    #[test]
    fn build_recon_client_returns_none_for_polymarket_by_design() {
        let c = demo_creds();
        assert!(build_recon_client("polymarket", "SOME-MARKET", &c, 0.0, false).is_none());
    }

    #[test]
    fn unknown_or_paper_venue_yields_no_recon_client() {
        let c = demo_creds();
        assert!(build_recon_client("dukascopy", "EURUSD", &c, 0.0, false).is_none());
        assert!(build_recon_client("not-a-venue", "X", &c, 0.0, false).is_none());
    }
}
