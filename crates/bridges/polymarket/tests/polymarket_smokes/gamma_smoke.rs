//! LIVE Polymarket Gamma catalog smoke — read-only, KEYLESS (Gamma needs no credentials, only
//! network reachability). `#[ignore]`d and gated behind an explicit `POLY_GAMMA_SMOKE=1` (unlike
//! the credential-gated smokes such as `redeem_smoke.rs`/`polymarket_user_smoke.rs`, there are no
//! credentials to gate on here — gating on `POLY_GAMMA_SMOKE=1` keeps CI's `--ignored` run, which
//! has no arbdub proxy, a clean self-skip). The Gamma host is geo-blocked exactly like the other
//! Polymarket reads, so run WITH the Dublin SOCKS proxy (arbdub):
//!
//! ```sh
//! POLY_GAMMA_SMOKE=1 \
//!   cargo test -p vike-polymarket --features polymarket --test polymarket_smokes \
//!   -- --ignored --nocapture gamma_smoke::
//! ```
//! The tunnel is `venue.polymarket.*` in the settings database (see `tests/polymarket_smokes/egress_support.rs`).
//!
//! This is the live confirmation of the `clobTokenIds` field name + `gamma_keyset_query`'s query
//! params against the real `{gamma}/markets/keyset` response shape (`catalog/gamma.rs`'s Step 0
//! confirm, exercised end to end): fetches the first page of `GammaSource::list(true, 5, "")` and
//! asserts at least one returned market has a non-empty `question` AND a non-empty `token_ids`
//! (i.e. `clobTokenIds` actually decoded). The Gamma host is the release's seed row (a smoke reads
//! no store).

use vike_polymarket::{GammaClient, GammaSource};

use crate::egress_support;

/// The Gamma client over the release's seed row.
fn gamma() -> GammaClient {
    GammaClient::from_urls(&egress_support::seed()).expect("the seed's gamma row")
}

/// The door's resolution over the release's seed rows.
fn canonical(input: &str) -> Result<String, String> {
    vike_polymarket::catalog::market_ref::canonical_polymarket_symbol(
        input,
        &egress_support::seed(),
    )
    .map_err(|refusal| refusal.to_string())
}

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

    tracing::warn!(target: "vike_polymarket", "LIVE Gamma catalog smoke: GammaSource::list(active_only=true, limit=5, first page)");

    let (markets, _) = gamma().list(true, 5, "").expect("GammaSource::list should succeed");

    if markets.is_empty() {
        // Not a failure: an empty active+open page is possible in principle, just unlikely.
        tracing::info!(target: "vike_polymarket", "GammaSource::list returned Ok with an empty page — nothing to assert further");
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
                |field| {
                    (*field == vike_venues::venue_fields::SOCKS_PROXY).then(|| "none".to_string())
                },
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
    use vike_polymarket::catalog::market_ref::{MarketRef, parse_market_ref};
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol};

    vike_log::test_init();
    if !declare_egress_for(std::env::var("POLY_GAMMA_SMOKE").ok().as_deref()) {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY_GAMMA_SMOKE is not 1 or direct");
        return;
    }

    let (markets, _) = gamma().list(true, 20, "").expect("GammaSource::list should succeed");
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
        let token = canonical(&typed)
            .unwrap_or_else(|e| panic!("`{typed}` must resolve against the live service: {e}"));
        assert_eq!(token, m.token_ids[i], "`{typed}` resolved to the wrong token id");
        // the label is case-insensitive
        assert_eq!(canonical(&format!("{}:{}", m.slug, outcome.to_uppercase())), Ok(token));
    }

    // the exact market page: its last path segment is the slug, and it names no outcome
    let page = format!("https://polymarket.com/event/{}/{}", m.event_slug, m.slug);
    assert_eq!(
        parse_market_ref(&page),
        Ok(MarketRef::Market { slug: m.slug.to_lowercase(), outcome: None })
    );
    let why = canonical(&page).expect_err("a page names no outcome");
    assert!(why.contains(&m.token_ids[0]) && why.contains(&m.token_ids[1]), "{why}");

    // the event page: the real `/events/keyset?slug=` route lists the event's markets, and either
    // names the one market's outcomes or the markets to choose from — never a silent pick
    let event_page = format!("https://polymarket.com/event/{}", m.event_slug);
    let why = canonical(&event_page).expect_err("an event page names no outcome");
    assert!(why.contains("name which") || why.contains("name one market"), "{event_page}: {why}");

    // a RESOLVED market is found (the lookup does not filter on `closed`) and refused as closed, not
    // reported as unlisted: `closed=true` is Gamma's own filter for one
    let closed = vike_polymarket::get_json(
        &egress_support::seed_row(UrlChannel::Data, UrlProtocol::Rest, "gamma"),
        "/markets/keyset",
        "closed=true&limit=20&order=volumeNum&ascending=false",
    )
    .expect("the closed-market browse");
    let done = vike_polymarket::parse_gamma_markets(&closed["markets"])
        .into_iter()
        .find(|d| d.closed && !d.slug.is_empty() && !d.outcomes.is_empty())
        .expect("a closed market among the 20 most traded resolved ones");
    let typed = format!("{}:{}", done.slug, done.outcomes[0]);
    let why = canonical(&typed).expect_err("a closed market is not tradeable");
    assert!(why.contains("CLOSED"), "`{typed}`: {why}");
    tracing::warn!(target: "vike_polymarket", slug = %done.slug, "a resolved market is refused as CLOSED, not as unlisted");

    // an unlisted slug is refused in words, not mistaken for an outage
    let why = canonical("this-market-does-not-exist-vike-smoke:Yes").expect_err("not listed");
    assert!(why.contains("Gamma lists no market with the slug"), "{why}");

    // a pasted token id keeps working unchanged
    assert_eq!(canonical(&m.token_ids[0]), Ok(m.token_ids[0].clone()));
    tracing::info!(target: "vike_polymarket", "slug resolution confirmed against the live service");
}

/// Both paged walks against the REAL `/markets/keyset` (`vike_polymarket::catalog::gamma`'s
/// `keyset_page`): the catalog follows Gamma's cursor past its 100-market page to the 40-page ceiling
/// with no market read twice, and the archive reads a whole closed day (29 markets ending 2025-01-01,
/// measured 2026-11-17, one page whose `next_cursor` is null) and stops where Gamma names no cursor.
///
/// KEYLESS and READ-ONLY: about 41 requests, no credential is read and no wallet is used.
#[test]
#[ignore = "LIVE network (Gamma, keyless) — opt-in POLY_GAMMA_SMOKE=1 (tunnel from the settings database) or =direct"]
fn the_keyset_walks_follow_gammas_cursor_live() {
    use vike_catalog::CatalogProvider;
    use vike_data::source::MarketRulesSource;

    vike_log::test_init();
    if !declare_egress_for(std::env::var("POLY_GAMMA_SMOKE").ok().as_deref()) {
        tracing::warn!(target: "vike_polymarket", "SKIP: POLY_GAMMA_SMOKE is not 1 or direct");
        return;
    }

    let rows = vike_polymarket::CatalogProviderPolymarket::new(&egress_support::seed())
        .list_instruments()
        .expect("the keyset browse");
    let tokens: std::collections::BTreeSet<&str> =
        rows.iter().map(|r| r.raw_symbol.as_str()).collect();
    assert!(rows.len() > 2 * 100, "more than Gamma's first page of markets: {}", rows.len());
    assert_eq!(tokens.len(), rows.len(), "no outcome token is read twice");

    let rules = vike_polymarket::catalog::rules::MarketRulesSourcePolymarket::from_urls(
        &egress_support::seed(),
    )
    .expect("the seed's gamma row");
    let day = vike_model::time::parse_utc_timestamp_ms("2025-01-01T00:00:00Z").expect("a day");
    let texts = rules.closed_ending(day, day + 86_400_000 - 1).expect("the keyset archive");
    let markets: std::collections::BTreeSet<&str> =
        texts.iter().map(|t| t.source_id.as_str()).collect();
    assert!(markets.len() >= 29, "the day's measured 29 markets: {}", markets.len());
    tracing::info!(
        target: "vike_polymarket",
        instruments = rows.len(),
        archived_markets = markets.len(),
        "the keyset walks confirmed against the live service"
    );
}
