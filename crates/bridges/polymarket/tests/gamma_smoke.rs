//! LIVE Polymarket Gamma catalog smoke — read-only, KEYLESS (Gamma needs no credentials, only
//! network reachability). `#[ignore]`d and gated behind an explicit `POLY_GAMMA_SMOKE=1` (unlike
//! the credential-gated smokes such as `redeem_smoke.rs`/`polymarket_user_smoke.rs`, there are no
//! credentials to gate on here — gating on `POLY_GAMMA_SMOKE=1` keeps CI's `--ignored` run, which
//! has no arbdub proxy, a clean self-skip). The Gamma host is geo-blocked exactly like the other
//! Polymarket reads, so run WITH the Dublin SOCKS proxy (arbdub):
//!
//! ```sh
//! POLY_GAMMA_SMOKE=1 \
//!   cargo test -p vike-polymarket --features polymarket --test gamma_smoke \
//!   -- --ignored --nocapture
//! ```
//! The tunnel is `venue.polymarket.*` in the settings database (see `tests/support/egress.rs`).
//!
//! This is the live confirmation of the `clobTokenIds` field name + `gamma_query`'s query params
//! against the real `GAMMA_BASE/markets` response shape (`gamma.rs`'s Step 0 confirm, exercised
//! end to end): fetches `GammaClient::list(true, 5, 0)` and asserts at least one returned market
//! has a non-empty `question` AND a non-empty `token_ids` (i.e. `clobTokenIds` actually decoded).

#![cfg(feature = "polymarket")]

use vike_polymarket::GammaClient;

#[path = "support/egress.rs"]
mod egress_support;

#[test]
#[ignore = "LIVE network (Gamma, keyless) — opt-in POLY_GAMMA_SMOKE=1; run manually via arbdub"]
fn gamma_list_returns_live_markets_with_token_ids() {
    vike_log::test_init();
    egress_support::declare_from_the_settings_database();

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

/// Declare the egress the way this smoke's mode asks: `POLY_GAMMA_SMOKE=1` reads the tunnel from the
/// settings database like every other live smoke, `POLY_GAMMA_SMOKE=direct` dials Gamma with no
/// tunnel (a lane in a region that reaches the keyless reads directly). `false` = not opted in.
fn declare_egress_for(mode: Option<&str>) -> bool {
    match mode {
        Some("1") => {
            egress_support::declare_from_the_settings_database();
            true
        }
        Some("direct") => {
            let _ = vike_polymarket::declare_egress(vike_polymarket::EgressSettings::from_lookup(
                |field| (field == "socks_proxy").then(|| "none".to_string()),
            ));
            true
        }
        _ => false,
    }
}

/// The slug route against the REAL service: take a live market from the volume browse, type its slug
/// and an outcome, and require `canonical_polymarket_symbol` to land on the very token id the browse
/// reported — two independent Gamma routes agreeing — for BOTH outcomes; then the refusals a
/// real operator would meet (a market page that names no outcome, an unlisted slug, a pasted token
/// id that must cost no read) against the same service.
///
/// KEYLESS and READ-ONLY: no credential is read and no wallet is used.
#[test]
#[ignore = "LIVE network (Gamma, keyless) — opt-in POLY_GAMMA_SMOKE=1 (tunnel from the settings database) or =direct"]
fn a_market_slug_resolves_to_its_outcome_token_ids_live() {
    use vike_polymarket::market_ref::{MarketRef, canonical_polymarket_symbol, parse_market_ref};

    vike_log::test_init();
    if !declare_egress_for(std::env::var("POLY_GAMMA_SMOKE").ok().as_deref()) {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY_GAMMA_SMOKE is not 1 or direct");
        return;
    }

    let markets = GammaClient::list(true, 20, 0).expect("GammaClient::list should succeed");
    let m = markets
        .iter()
        .find(|m| {
            !m.slug.is_empty()
                && !m.event_slug.is_empty()
                && m.token_ids.len() == 2
                && m.outcomes.len() == 2
                && m.active
                && !m.closed
        })
        .expect("an active two-outcome market with a slug among the 20 most traded");
    tracing::warn!(target: "vike_polymarket", slug = %m.slug, event = %m.event_slug, "resolving a live market by slug");

    for (i, outcome) in m.outcomes.iter().enumerate() {
        let typed = format!("{}:{outcome}", m.slug);
        let token = canonical_polymarket_symbol(&typed)
            .unwrap_or_else(|e| panic!("`{typed}` must resolve against the live service: {e}"));
        assert_eq!(token, m.token_ids[i], "`{typed}` resolved to the wrong token id");
        // the label is case-insensitive
        assert_eq!(
            canonical_polymarket_symbol(&format!("{}:{}", m.slug, outcome.to_uppercase())),
            Ok(token)
        );
    }

    // the exact market page: its last path segment is the slug, and it names no outcome
    let page = format!("https://polymarket.com/event/{}/{}", m.event_slug, m.slug);
    assert_eq!(
        parse_market_ref(&page),
        Ok(MarketRef::Market { slug: m.slug.to_lowercase(), outcome: None })
    );
    let why = canonical_polymarket_symbol(&page).expect_err("a page names no outcome");
    assert!(why.contains(&m.token_ids[0]) && why.contains(&m.token_ids[1]), "{why}");

    // the event page: the real `/events?slug=` route lists the event's markets, and either names the
    // one market's outcomes or the markets to choose from — never a silent pick
    let event_page = format!("https://polymarket.com/event/{}", m.event_slug);
    let why = canonical_polymarket_symbol(&event_page).expect_err("an event page names no outcome");
    assert!(why.contains("name which") || why.contains("name one market"), "{event_page}: {why}");

    // a RESOLVED market is found (the lookup does not filter on `closed`) and refused as closed, not
    // reported as unlisted: `closed=true` is Gamma's own filter for one
    let closed = vike_polymarket::get_json(
        vike_polymarket::GAMMA_BASE,
        "/markets",
        "closed=true&limit=20&order=volumeNum&ascending=false",
    )
    .expect("the closed-market browse");
    let done = vike_polymarket::parse_gamma_markets(&closed)
        .into_iter()
        .find(|d| d.closed && !d.slug.is_empty() && !d.outcomes.is_empty())
        .expect("a closed market among the 20 most traded resolved ones");
    let typed = format!("{}:{}", done.slug, done.outcomes[0]);
    let why = canonical_polymarket_symbol(&typed).expect_err("a closed market is not tradeable");
    assert!(why.contains("CLOSED"), "`{typed}`: {why}");
    tracing::warn!(target: "vike_polymarket", slug = %done.slug, "a resolved market is refused as CLOSED, not as unlisted");

    // an unlisted slug is refused in words, not mistaken for an outage
    let why = canonical_polymarket_symbol("this-market-does-not-exist-vike-smoke:Yes")
        .expect_err("not listed");
    assert!(why.contains("Gamma lists no market with the slug"), "{why}");

    // a pasted token id keeps working unchanged
    assert_eq!(canonical_polymarket_symbol(&m.token_ids[0]), Ok(m.token_ids[0].clone()));
    tracing::info!(target: "vike_polymarket", "slug resolution confirmed against the live service");
}
