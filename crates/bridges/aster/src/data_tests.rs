use super::*;
use vike_binance::family::klines::klines_url;

/// The ceiling is asserted at COMPILE time (now in `vike_model::venues::venue_rate_limits`, one
/// `const _: ()` per row), which is strictly stronger than this test — a violation fails the
/// build, not a test run.
///
/// What this gates is the WIRING: a drift in the table's aster rows changes this pager's
/// FALLBACK pacing and fails here.
///
/// ⚠ It also gates the thing that made the sapi retune worth doing: the two hosts must resolve
/// to DIFFERENT budgets. They were one shared const built from `ASTER_PERP`, so a bare (spot)
/// symbol braked on the fapi soft limit of 2000 — 33 % of the 6000 sapi publishes and that
/// discovery already finds. Asserting `perp != spot` here is what stops that collapsing back.
#[test]
fn the_spec_paces_on_the_venue_tables_aster_row() {
    // perp — unchanged, the byte-identical half
    assert_eq!(ASTER_RATE_LIMIT_PERP.weight_soft_limit, 2000);
    assert_eq!(ASTER_RATE_LIMIT_PERP.page_delay, Duration::from_millis(150));
    assert_eq!(ASTER_PERP.history.ceiling_per_min(), 2400);
    // spot — sapi's own published budget, measured 2026-08-05
    assert_eq!(ASTER_RATE_LIMIT_SPOT.weight_soft_limit, 4800);
    assert_eq!(ASTER_RATE_LIMIT_SPOT.page_delay, Duration::from_millis(125));
    assert_eq!(ASTER_SPOT.history.ceiling_per_min(), 6000);
    // a soft limit at or above its own hard ceiling can never fire
    assert!(ASTER_RATE_LIMIT_PERP.weight_soft_limit < ASTER_PERP.history.ceiling_per_min());
    assert!(ASTER_RATE_LIMIT_SPOT.weight_soft_limit < ASTER_SPOT.history.ceiling_per_min());
    // THE property: the hosts are paced apart, not off one shared number.
    assert_ne!(ASTER_RATE_LIMIT_PERP, ASTER_RATE_LIMIT_SPOT);
    // Every spec this module can build carries its OWN host's fallback and the shared default
    // utilization; `(env, perp)` changes the discovery URL and now the budget with it.
    for env in [Environment::Live, Environment::Demo, Environment::Sim] {
        for perp in [true, false] {
            let s = aster_kline_spec(env, perp);
            let want = if perp { ASTER_RATE_LIMIT_PERP } else { ASTER_RATE_LIMIT_SPOT };
            assert_eq!(s.rate_limit, want, "{env:?}/{perp}");
            assert_eq!(s.venue, VENUE);
            assert_eq!(
                s.utilization.to_bits(),
                vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits()
            );
        }
    }
}

/// **Aster discovers.** The limitation this replaces was a limitation of the spec's TYPE, never
/// of the venue: all four hosts serve a `REQUEST_WEIGHT` row (MEASURED 2026-08-05, see
/// [`rest_exchange_info`]). With the field a `Cow`, the URL is composed from the same
/// `urls::urls_for(env)` table the `/klines` base is, and the rung accepts it.
#[test]
fn every_env_and_market_now_names_a_discovery_url() {
    for env in [Environment::Live, Environment::Demo, Environment::Sim] {
        for perp in [true, false] {
            let s = aster_kline_spec(env, perp);
            let url = s
                .exchange_info_url
                .as_deref()
                .unwrap_or_else(|| panic!("{env:?}/{perp} must name a discovery URL"));
            let path = if perp {
                crate::urls::PERP_PATH_EXCHANGE_INFO
            } else {
                crate::urls::SPOT_PATH_EXCHANGE_INFO
            };
            assert!(url.ends_with(path), "{url} must end with {path}");
            // OWNED, which is the whole capability: there is no `&'static str` to borrow when
            // the host comes out of an `Environment`.
            assert!(matches!(s.exchange_info_url, Some(Cow::Owned(_))), "{url}");
        }
    }
}

/// **The per-environment pairing.** A spec must discover against the host it will ACTUALLY page,
/// and on this venue "the host" has two independent axes: market (sapi vs fapi) and network
/// (testnet vs mainnet). Asserted through the SHARED [`klines::discovery_url`] gate — the same
/// one the runtime consults and binance's twin test drives — against the base
/// [`range_target`] itself resolves, so the fetcher and its budget cannot drift apart.
///
/// Not a hypothetical pairing: mainnet fapi publishes 2400, mainnet sapi 6000, and testnet fapi
/// a nonsense `-2`. Every crossing of those axes is a different number.
#[test]
fn each_spec_discovers_the_budget_of_the_host_it_pages() {
    for env in [Environment::Live, Environment::Demo, Environment::Sim] {
        for symbol in ["BTCUSDT", "BTCUSDT.P"] {
            let (base, _, spec) = range_target(symbol, env);
            let info = klines::discovery_url(&spec, &base)
                .unwrap_or_else(|| panic!("{env:?}/{symbol}: must discover against {base}"));
            assert_eq!(
                klines::url_host(info),
                klines::url_host(&base),
                "{info} must be the budget of {base}'s host"
            );
        }
    }
}

/// The hazard the `&'static str` could not avoid, stated as a REFUSAL: a mainnet spec against a
/// testnet base (and every other crossing of the env x market grid) discovers NOTHING, so it
/// falls back to `page_delay` rather than pacing one network against the other's budget.
///
/// The four hosts are also pairwise distinct, which is what stops this from being vacuous.
#[test]
fn a_spec_never_discovers_against_another_network_or_market() {
    let targets: Vec<(String, String, KlineSpec)> = [
        (Environment::Live, true),
        (Environment::Live, false),
        (Environment::Demo, true),
        (Environment::Demo, false),
    ]
    .into_iter()
    .map(|(env, perp)| {
        (
            format!("{env:?}/{}", if perp { "perp" } else { "spot" }),
            rest_klines_base(env, perp),
            aster_kline_spec(env, perp),
        )
    })
    .collect();

    // Four genuinely different hosts — otherwise the crossings below prove nothing.
    let hosts: std::collections::BTreeSet<&str> =
        targets.iter().filter_map(|(_, base, _)| klines::url_host(base)).collect();
    assert_eq!(hosts.len(), 4, "the env x market grid must be four distinct hosts: {hosts:?}");

    for (i_label, _, spec) in &targets {
        for (j_label, base, _) in &targets {
            let got = klines::discovery_url(spec, base);
            if i_label == j_label {
                assert!(got.is_some(), "{i_label} must discover against its own host");
            } else {
                assert_eq!(got, None, "{i_label}'s budget must not pace {j_label}");
            }
        }
    }
}

// Aster's OWN host-resolution proof (the per-venue delta): the shared pure `parse_klines`/URL
// shaping is proven once at the rung (`vike_binance::family::klines`); here we prove the
// testnet/mainnet + sapi/fapi hosts.
#[test]
fn klines_url_includes_optional_bounds() {
    let base = rest_klines_base(Environment::Demo, false);
    let latest = klines_url(&base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        latest,
        "https://sapi.asterdex-testnet.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let ranged = klines_url(&base, "BTCUSDT", "1m", Some(10), Some(20), 1000);
    assert!(ranged.ends_with("&startTime=10&endTime=20"));
}

#[test]
fn klines_url_perp_uses_fapi_host_spot_uses_sapi_host() {
    let perp_base = rest_klines_base(Environment::Demo, true);
    let perp = klines_url(&perp_base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        perp,
        "https://fapi.asterdex-testnet.com/fapi/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let spot_base = rest_klines_base(Environment::Demo, false);
    let spot = klines_url(&spot_base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        spot,
        "https://sapi.asterdex-testnet.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
}

/// The perp suffix selects the fapi host AND is stripped from the wire symbol.
#[test]
fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
    assert_eq!(wire, "BTCUSDT");
    assert!(perp);
    let base = rest_klines_base(Environment::Live, perp);
    assert!(base.ends_with("/fapi/v3/klines"), "perp must use fapi: {base}");
    let url = klines_url(&base, wire, "1m", Some(10), Some(20), 1000);
    assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
    assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
}

/// A bare symbol still resolves the SPOT (sapi) host.
#[test]
fn a_bare_symbol_still_resolves_the_spot_host() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
    assert!(!perp);
    assert_eq!(wire, "BTCUSDT");
    let base = rest_klines_base(Environment::Live, perp);
    assert!(base.ends_with("/api/v3/klines"), "spot must use sapi: {base}");
}

// The two tests above shape a URL out of `rest_klines_base` DIRECTLY, which has supported perp
// since the live warmup seed was written — they would pass even with `fetch_klines_range` still
// pinned to spot. The bug is the FETCHER not calling the helper, so gate that decision itself:
// `range_target` is exactly what `fetch_klines_range` routes on, and it is pure (no network).
#[test]
fn fetch_klines_range_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
    let (base, wire, spec) = range_target("BTCUSDT.P", Environment::Live);
    assert_eq!(base, "https://fapi.asterdex.com/fapi/v3/klines");
    assert_eq!(wire, "BTCUSDT");
    assert_eq!(
        spec.exchange_info_url.as_deref(),
        Some("https://fapi.asterdex.com/fapi/v3/exchangeInfo"),
        "the fapi host must carry the FAPI budget"
    );
}

#[test]
fn fetch_klines_range_routes_a_bare_symbol_to_sapi_unchanged() {
    let (base, wire, spec) = range_target("BTCUSDT", Environment::Live);
    assert_eq!(base, "https://sapi.asterdex.com/api/v3/klines");
    assert_eq!(wire, "BTCUSDT");
    assert_eq!(
        spec.exchange_info_url.as_deref(),
        Some("https://sapi.asterdex.com/api/v3/exchangeInfo"),
        "the sapi host must carry the SAPI budget"
    );
    // testnet is unchanged too — `env` still picks the tier, orthogonally to the perp split,
    // and it picks it for the discovery URL as well as for the pages.
    let (base, wire, spec) = range_target("BTCUSDT", Environment::Demo);
    assert_eq!(base, "https://sapi.asterdex-testnet.com/api/v3/klines");
    assert_eq!(wire, "BTCUSDT");
    assert_eq!(
        spec.exchange_info_url.as_deref(),
        Some("https://sapi.asterdex-testnet.com/api/v3/exchangeInfo")
    );
}

/// The discovery URL is composed from [`urls::urls_for`], never from a second copy of the host
/// — so a host change in that ONE table moves the pages and the budget together. Asserted as a
/// prefix relation rather than by re-typing the literals, which is what a duplicate would pass.
#[test]
fn the_discovery_host_comes_from_the_urls_table_not_a_second_literal() {
    for env in [Environment::Live, Environment::Demo] {
        let u = urls::urls_for(env);
        assert_eq!(
            rest_exchange_info(env, true),
            format!("{}{}", u.fapi_rest, crate::urls::PERP_PATH_EXCHANGE_INFO)
        );
        assert_eq!(
            rest_exchange_info(env, false),
            format!("{}{}", u.sapi_rest, crate::urls::SPOT_PATH_EXCHANGE_INFO)
        );
        // ...and the klines base it pairs with comes out of the same two fields.
        assert!(rest_klines_base(env, true).starts_with(u.fapi_rest));
        assert!(rest_klines_base(env, false).starts_with(u.sapi_rest));
    }
}

#[test]
fn rest_klines_base_selects_fapi_for_perp_and_testnet_vs_mainnet() {
    // rest_klines_base is the pure host-selection the perp seed relies on — no network.
    assert_eq!(
        rest_klines_base(Environment::Demo, true),
        "https://fapi.asterdex-testnet.com/fapi/v3/klines"
    );
    assert_eq!(
        rest_klines_base(Environment::Demo, false),
        "https://sapi.asterdex-testnet.com/api/v3/klines"
    );
    assert_eq!(
        rest_klines_base(Environment::Live, false),
        "https://sapi.asterdex.com/api/v3/klines"
    );
    assert_eq!(
        rest_klines_base(Environment::Live, true),
        "https://fapi.asterdex.com/fapi/v3/klines"
    );
}
