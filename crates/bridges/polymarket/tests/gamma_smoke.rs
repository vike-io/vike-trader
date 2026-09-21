//! LIVE Polymarket Gamma catalog smoke — read-only, KEYLESS (Gamma needs no credentials, only
//! network reachability). `#[ignore]`d and gated behind an explicit `POLY_GAMMA_SMOKE=1` (unlike
//! the credential-gated smokes such as `redeem_smoke.rs`/`polymarket_user_smoke.rs`, there are no
//! credentials to gate on here — gating on `POLY_GAMMA_SMOKE=1` keeps CI's `--ignored` run, which
//! has no arbdub proxy, a clean self-skip). The Gamma host is geo-blocked exactly like the other
//! Polymarket reads, so run WITH the Dublin SOCKS proxy (arbdub):
//!
//! ```sh
//! POLY_SOCKS_PROXY=socks5://127.0.0.1:1080 POLY_GAMMA_SMOKE=1 \
//!   cargo test -p vike-polymarket --features polymarket --test gamma_smoke \
//!   -- --ignored --nocapture
//! ```
//!
//! This is the live confirmation of the `clobTokenIds` field name + `gamma_query`'s query params
//! against the real `GAMMA_BASE/markets` response shape (`gamma.rs`'s Step 0 confirm, exercised
//! end to end): fetches `GammaClient::list(true, 5, 0)` and asserts at least one returned market
//! has a non-empty `question` AND a non-empty `token_ids` (i.e. `clobTokenIds` actually decoded).

#![cfg(feature = "polymarket")]

use vike_polymarket::GammaClient;

#[test]
#[ignore = "LIVE network (Gamma, keyless) — opt-in POLY_GAMMA_SMOKE=1; run manually via arbdub"]
fn gamma_list_returns_live_markets_with_token_ids() {
    vike_log::test_init();

    if std::env::var("POLY_GAMMA_SMOKE").ok().as_deref() != Some("1") {
        tracing::warn!(
            target: "vike_polymarket",
            "SKIP: POLY_GAMMA_SMOKE != 1 — Gamma is keyless, but this still hits the live network via arbdub, so it requires an explicit opt-in"
        );
        return;
    }

    tracing::warn!(target: "vike_polymarket", "LIVE Gamma catalog smoke: GammaClient::list(active_only=true, limit=5, offset=0)");

    let markets = GammaClient::list(true, 5, 0).expect("GammaClient::list should succeed");

    if markets.is_empty() {
        // Not a failure: an empty active+open page is possible in principle, just unlikely.
        tracing::info!(target: "vike_polymarket", "GammaClient::list returned Ok with an empty page — nothing to assert further");
        return;
    }

    let confirmed = markets.iter().any(|m| !m.question.is_empty() && !m.token_ids.is_empty());
    assert!(
        confirmed,
        "expected at least one market with a non-empty question AND non-empty token_ids (clobTokenIds decode) among {} markets: {:?}",
        markets.len(),
        markets.iter().map(|m| (&m.question, &m.token_ids)).collect::<Vec<_>>()
    );

    tracing::info!(
        target: "vike_polymarket",
        count = markets.len(),
        first_question = %markets[0].question,
        first_token_ids = ?markets[0].token_ids,
        "Gamma catalog smoke confirmed: clobTokenIds decoded on live data"
    );
}
