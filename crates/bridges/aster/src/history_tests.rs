use super::*;
use crate::transport::urls::seeded::{mainnet, testnet};
use std::assert_matches;
use vike_binance::family::history::klines::klines_url;

/// The backfill's two hosts as the seed's `live` `history` rows resolve them.
fn seed_klines() -> KlineSourceAster {
    KlineSourceAster::from_urls(&VenueUrls::seed(VENUE))
        .expect("the seed answers aster's history rows")
}

/// Every REST host this module can page, two networks by two books: the seed's mainnet rows and
/// its testnet ones (the hosts the four measurements in [`rest_exchange_info`] were taken on).
fn every_host() -> Vec<(String, &'static str, bool)> {
    let mut out = Vec::new();
    for (net, u) in [("mainnet", mainnet()), ("testnet", testnet())] {
        for perp in [true, false] {
            out.push((format!("{net}/{}", if perp { "perp" } else { "spot" }), u.rest(perp), perp));
        }
    }
    out
}

/// The ceiling is asserted at COMPILE time (now in `vike_venues::venue_rate_limits`, one
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
    // Every spec this module can build carries its OWN book's fallback and the shared default
    // utilization; the host changes the discovery URL, the book the budget with it.
    for (label, rest, perp) in every_host() {
        let s = aster_kline_spec(rest, perp);
        let want = if perp { ASTER_RATE_LIMIT_PERP } else { ASTER_RATE_LIMIT_SPOT };
        assert_eq!(s.rate_limit, want, "{label}");
        assert_eq!(s.venue, VENUE);
        assert_eq!(s.utilization.to_bits(), vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits());
    }
}

/// **Aster discovers.** The limitation this replaces was a limitation of the spec's TYPE, never
/// of the venue: all four hosts serve a `REQUEST_WEIGHT` row (MEASURED 2026-08-05, see
/// [`rest_exchange_info`]). With the field a `Cow`, the URL is composed on the same host the
/// `/klines` base is, and the rung accepts it.
#[test]
fn every_host_and_market_names_a_discovery_url() {
    for (label, rest, perp) in every_host() {
        let s = aster_kline_spec(rest, perp);
        let url = s
            .exchange_info_url
            .as_deref()
            .unwrap_or_else(|| panic!("{label} must name a discovery URL"));
        let path = if perp {
            crate::transport::urls::PERP_PATH_EXCHANGE_INFO
        } else {
            crate::transport::urls::SPOT_PATH_EXCHANGE_INFO
        };
        assert!(url.ends_with(path), "{url} must end with {path}");
        // OWNED, which is the whole capability: there is no `&'static str` to borrow when the
        // host is a resolved row.
        assert_matches!(s.exchange_info_url, Some(Cow::Owned(_)), "{url}");
    }
}

/// **The per-host pairing.** A spec must discover against the host it will ACTUALLY page, asserted
/// through the SHARED [`klines::discovery_url`] gate — the same one the runtime consults and
/// binance's twin test drives — against the base [`range_target`] itself resolves, so the fetcher
/// and its budget cannot drift apart. Run over the backfill's own hosts and over a table holding
/// the testnet ones.
///
/// Not a hypothetical pairing: mainnet fapi publishes 2400, mainnet sapi 6000, and testnet fapi
/// a nonsense `-2`. Every crossing of those axes is a different number.
#[test]
fn each_spec_discovers_the_budget_of_the_host_it_pages() {
    let testnet_table = KlineSourceAster {
        fapi_rest: testnet().fapi_rest.to_string(),
        sapi_rest: testnet().sapi_rest.to_string(),
    };
    for source in [seed_klines(), testnet_table] {
        for symbol in ["BTCUSDT", "BTCUSDT.P"] {
            let (base, _, spec) = range_target(&source, symbol);
            let info = klines::discovery_url(&spec, &base)
                .unwrap_or_else(|| panic!("{symbol}: must discover against {base}"));
            assert_eq!(
                klines::url_host(info),
                klines::url_host(&base),
                "{info} must be the budget of {base}'s host"
            );
        }
    }
}

/// The hazard the `&'static str` could not avoid, stated as a REFUSAL: a mainnet spec against a
/// testnet base (and every other crossing of the network x market grid) discovers NOTHING, so it
/// falls back to `page_delay` rather than pacing one host against the other's budget.
///
/// The four hosts are also pairwise distinct, which is what stops this from being vacuous.
#[test]
fn a_spec_never_discovers_against_another_network_or_market() {
    let targets: Vec<(String, String, KlineSpec)> = every_host()
        .into_iter()
        .map(|(label, rest, perp)| {
            (label, rest_klines_base(rest, perp), aster_kline_spec(rest, perp))
        })
        .collect();

    // Four genuinely different hosts — otherwise the crossings below prove nothing.
    let hosts: std::collections::BTreeSet<&str> =
        targets.iter().filter_map(|(_, base, _)| klines::url_host(base)).collect();
    assert_eq!(hosts.len(), 4, "the network x market grid must be four distinct hosts: {hosts:?}");

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
// shaping is proven once at the rung (`vike_binance::family::history::klines`); here we prove the
// sapi/fapi hosts the rows give.
#[test]
fn klines_url_includes_optional_bounds() {
    let base = rest_klines_base(mainnet().rest(false), false);
    let latest = klines_url(&base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        latest,
        "https://sapi.asterdex.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let ranged = klines_url(&base, "BTCUSDT", "1m", Some(10), Some(20), 1000);
    assert!(ranged.ends_with("&startTime=10&endTime=20"));
}

#[test]
fn klines_url_perp_uses_fapi_host_spot_uses_sapi_host() {
    let perp_base = rest_klines_base(mainnet().rest(true), true);
    let perp = klines_url(&perp_base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        perp,
        "https://fapi.asterdex.com/fapi/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let spot_base = rest_klines_base(mainnet().rest(false), false);
    let spot = klines_url(&spot_base, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        spot,
        "https://sapi.asterdex.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
}

/// The perp suffix selects the fapi host AND is stripped from the wire symbol.
#[test]
fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
    let (wire, perp) = vike_venues::split_perp("BTCUSDT.P");
    assert_eq!(wire, "BTCUSDT");
    assert!(perp);
    let base = rest_klines_base(mainnet().rest(perp), perp);
    assert!(base.ends_with("/fapi/v3/klines"), "perp must use fapi: {base}");
    let url = klines_url(&base, wire, "1m", Some(10), Some(20), 1000);
    assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
    assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
}

/// A bare symbol still resolves the SPOT (sapi) host.
#[test]
fn a_bare_symbol_still_resolves_the_spot_host() {
    let (wire, perp) = vike_venues::split_perp("BTCUSDT");
    assert!(!perp);
    assert_eq!(wire, "BTCUSDT");
    let base = rest_klines_base(mainnet().rest(perp), perp);
    assert!(base.ends_with("/api/v3/klines"), "spot must use sapi: {base}");
}

// The two tests above shape a URL out of `rest_klines_base` DIRECTLY, which has supported perp
// since the live warmup seed was written — they would pass even with `fetch_klines_range` still
// pinned to spot. The bug is the FETCHER not calling the helper, so gate that decision itself:
// `range_target` is exactly what `fetch_klines_range` routes on, and it is pure (no network).
#[test]
fn fetch_klines_range_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
    let (base, wire, spec) = range_target(&seed_klines(), "BTCUSDT.P");
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
    let (base, wire, spec) = range_target(&seed_klines(), "BTCUSDT");
    assert_eq!(base, "https://sapi.asterdex.com/api/v3/klines");
    assert_eq!(wire, "BTCUSDT");
    assert_eq!(
        spec.exchange_info_url.as_deref(),
        Some("https://sapi.asterdex.com/api/v3/exchangeInfo"),
        "the sapi host must carry the SAPI budget"
    );
}

/// The backfill reads the `live` `history` rows — the mainnet hosts the code dialled before the
/// rows existed — and a table that cannot answer builds no source.
#[test]
fn the_backfill_holds_the_live_history_rows() {
    let k = seed_klines();
    assert_eq!(k.rest(true), "https://fapi.asterdex.com");
    assert_eq!(k.rest(false), "https://sapi.asterdex.com");
    assert!(KlineSourceAster::from_urls(&VenueUrls::default()).is_err());
}

/// The discovery URL is composed on the host the pages are, never from a second copy of the host
/// — so a moved row moves the pages and the budget together. Asserted as a prefix relation rather
/// than by re-typing the literals, which is what a duplicate would pass.
#[test]
fn the_discovery_host_is_the_paged_host_not_a_second_literal() {
    for (label, rest, perp) in every_host() {
        let path = if perp {
            crate::transport::urls::PERP_PATH_EXCHANGE_INFO
        } else {
            crate::transport::urls::SPOT_PATH_EXCHANGE_INFO
        };
        assert_eq!(rest_exchange_info(rest, perp), format!("{rest}{path}"), "{label}");
        // ...and the klines base it pairs with is on the same host.
        assert!(rest_klines_base(rest, perp).starts_with(rest), "{label}");
    }
}

#[test]
fn rest_klines_base_selects_fapi_for_perp_and_spot_for_sapi() {
    // rest_klines_base is the pure path selection the perp seed relies on — no network.
    assert_eq!(
        rest_klines_base(testnet().rest(true), true),
        "https://fapi.asterdex-testnet.com/fapi/v3/klines"
    );
    assert_eq!(
        rest_klines_base(testnet().rest(false), false),
        "https://sapi.asterdex-testnet.com/api/v3/klines"
    );
    assert_eq!(
        rest_klines_base(mainnet().rest(false), false),
        "https://sapi.asterdex.com/api/v3/klines"
    );
    assert_eq!(
        rest_klines_base(mainnet().rest(true), true),
        "https://fapi.asterdex.com/fapi/v3/klines"
    );
}

/// The committed venue answer (`fapi.asterdex.com/fapi/v1/fundingRate`, BTCUSDT, 2025-10-01, four
/// rows): binance's wire, read by binance's parser, the jittered stamp kept raw (the ingest floors it).
#[test]
fn the_funding_fixture_reads_through_binances_parser() {
    let body = include_str!("../tests/fixtures/aster_funding_btcusdt.json");
    let got = vike_binance::history::parse_funding_rates(body).unwrap();
    let ts: Vec<i64> = got.iter().map(|p| p.timestamp_ms).collect();
    assert_eq!(
        ts,
        vec![1_759_276_800_000, 1_759_305_600_000, 1_759_334_400_002, 1_759_363_200_000]
    );
    assert_eq!(
        got[1].rate.to_bits(),
        (-0.000_011_22_f64).to_bits(),
        "a negative rate keeps its sign"
    );
    assert!(got.iter().all(|p| p.mark.is_none() && p.premium.is_none()), "aster sends neither");
}

/// The funding page asks the MAINNET futures host (the `live` `data` row: public data; the testnet
/// has no history), with the `.P` stripped from the wire symbol.
#[test]
fn the_funding_url_is_the_mainnet_futures_host() {
    let funding = FundingRateSourceAster::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    assert_eq!(
        funding_url(&funding.fapi_rest, "BTCUSDT", 5, 9),
        "https://fapi.asterdex.com/fapi/v1/fundingRate?symbol=BTCUSDT&startTime=5&endTime=9&limit=1000"
    );
    assert!(FundingRateSourceAster::from_urls(&VenueUrls::default()).is_err());
}

/// A bare name is the spot pair: refused BEFORE any request.
#[test]
fn a_spot_symbol_is_refused_before_the_venue_is_asked() {
    use vike_data::source::{FundingRateSource, SourceError};
    let funding = FundingRateSourceAster::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    let err = funding.fetch("BTCUSDT", 0, 1).unwrap_err();
    assert!(matches!(&err, SourceError::Refused(t) if t.contains("BTCUSDT.P")), "{err:?}");
}

/// F-8: aster's funding walk is the family's (`vike_binance::history::funding_walk_forward`): a
/// rate-limited page is waited out and asked again. Red before the fix: one 429 failed the backfill.
#[test]
fn a_rate_limited_funding_page_is_asked_again_not_failed() {
    use vike_data::source::FundingRateSource;
    let row = r#"[{"symbol":"BTCUSDT","fundingTime":1000,"fundingRate":"0.0001"}]"#;
    let replies = vec![(429, String::new()), (200, row.to_string())];
    let (base, seen) = vike_log::loopback::serve_replies("", replies);
    let points = FundingRateSourceAster { fapi_rest: base }
        .fetch("BTCUSDT.P", 0, 2_000)
        .expect("waited out");
    assert_eq!(points.len(), 1, "the page asked again is read");
    assert_eq!(seen.lock().unwrap().len(), 2, "one 429, one answer");
}
