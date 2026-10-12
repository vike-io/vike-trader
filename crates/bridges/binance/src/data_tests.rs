use super::*;
use crate::family::klines::klines_url;
use crate::instruments::CoinMContract;
use std::assert_matches;
use std::cell::Cell;

/// The COIN-M listing binance actually publishes, as a lookup [`route_target`] can be driven
/// over with no socket. Keyed on the venue's OWN `symbol` values, which is the whole point.
fn listing(wire: &str) -> Result<Option<CoinMContract>, String> {
    Ok(match wire {
        "BTCUSD_PERP" | "ETHUSD_PERP" => Some(CoinMContract::Perpetual),
        "BTCUSD_260925" => Some(CoinMContract::Delivery("CURRENT_QUARTER")),
        "BTCUSD_261225" => Some(CoinMContract::Delivery("NEXT_QUARTER")),
        _ => None,
    })
}

/// A lookup that FAILS — the venue's list unreadable. Not the same answer as "not COIN-M".
fn unreadable(_: &str) -> Result<Option<CoinMContract>, String> {
    Err("dapi.binance.com/dapi/v1/exchangeInfo GET: connection refused".into())
}

/// [`route_target`] over the planted listing above — the impure half's twin, with the network
/// replaced rather than mocked away.
fn routed(
    symbol: &str,
    claimed: Option<vike_model::AssetClass>,
) -> Result<(&'static str, &str, &'static KlineSpec), String> {
    route_target(symbol, claimed, &listing)
}

/// The FETCHER's own routing decision — the gate the URL-shaping tests below cannot provide,
/// because they build a URL from `rest_klines_base` directly and that helper was already
/// perp-capable before this fix. Re-pinning `fetch_klines_range` to spot fails HERE.
#[test]
fn range_target_routes_a_perp_symbol_to_fapi_with_the_suffix_stripped() {
    let (base, wire, spec) = routed("BTCUSDT.P", None).expect("a USDⓈ-M perp routes");
    assert_eq!((base, wire), (REST_KLINES_FAPI, "BTCUSDT"));
    assert_eq!(spec, &BINANCE_KLINE_PERP, "the fapi host must carry the FAPI budget");
}

/// A bare symbol still resolves the spot host and is passed through untouched.
#[test]
fn range_target_routes_a_bare_symbol_to_spot_unchanged() {
    let (base, wire, spec) = routed("BTCUSDT", None).expect("a bare symbol routes");
    assert_eq!((base, wire), (REST_KLINES, "BTCUSDT"));
    assert_eq!(spec, &BINANCE_KLINE_SPOT, "the spot host must carry the SPOT budget");
}

/// The persisted-pace key must be the SAME decision the fetcher routes on, or a record files
/// itself under one market and paces the other. Asserted against `range_target` itself rather
/// than against a `.P` literal, which is what makes the two unable to drift.
#[test]
fn kline_market_agrees_with_the_fetchers_own_routing() {
    for symbol in ["BTCUSDT", "BTCUSDT.P", "ETHUSDT", "ETHUSDT.P", "1000PEPEUSDT.P"] {
        let (base, _, spec) = routed(symbol, None).expect("routes");
        let expected = if base == REST_KLINES_FAPI { "perp" } else { "spot" };
        assert_eq!(kline_market(symbol), expected, "{symbol} routes to {base}");
        // ...and the market therefore names the budget that record was measured against.
        let want = if expected == "perp" { BINANCE_KLINE_PERP } else { BINANCE_KLINE_SPOT };
        assert_eq!(spec, &want);
    }
    // The two markets are genuinely distinct keys, so a perp record can never overwrite a spot
    // one on the same venue.
    assert_ne!(kline_market("BTCUSDT"), kline_market("BTCUSDT.P"));
}

/// The ceilings are asserted at COMPILE time (now in `vike_model::venues::venue_rate_limits`, one
/// `const _: ()` per row), which is strictly stronger than this test could be — a violation
/// fails the build rather than a test run.
///
/// What this test does gate is the WIRING: the literals below are the numbers these two specs
/// carried as local consts before the per-venue table existed, so if the table's binance rows
/// ever drift, this pager's pacing changes and this test says so. That is the byte-identical
/// claim of the move, written down.
#[test]
fn the_specs_pace_on_the_venue_tables_binance_rows() {
    assert_eq!(BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit, 4800);
    assert_eq!(BINANCE_KLINE_SPOT.rate_limit.page_delay, Duration::from_millis(50));
    assert_eq!(BINANCE_KLINE_PERP.rate_limit.weight_soft_limit, 2000);
    assert_eq!(BINANCE_KLINE_PERP.rate_limit.page_delay, Duration::from_millis(150));
    // ...and each soft limit is genuinely under ITS OWN host's published ceiling (6000 spot,
    // 2400 fapi) — the pairing whose absence let one shared spec sit above fapi's ban threshold.
    assert_eq!(BINANCE_SPOT.history.ceiling_per_min(), 6000);
    assert_eq!(BINANCE_PERP.history.ceiling_per_min(), 2400);
    assert!(
        BINANCE_KLINE_SPOT.rate_limit.weight_soft_limit < BINANCE_SPOT.history.ceiling_per_min()
    );
    assert!(
        BINANCE_KLINE_PERP.rate_limit.weight_soft_limit < BINANCE_PERP.history.ceiling_per_min()
    );
}

/// A discovered budget belongs to the HOST that published it, so each spec's `exchangeInfo` URL
/// must live on the same host as the `/klines` URL it paces. Crossing them would pace the
/// 2400-weight fapi host against spot's 6000 — the same class of defect as the shared
/// `weight_soft_limit: 5000`, only now derived from the venue rather than hand-typed.
///
/// Two things changed when the spec's URL became env-resolvable. The pairing is now the SHARED
/// [`klines::discovery_url`] — the same gate the runtime consults and the same one aster's tests
/// drive over its four (`Environment` x market) hosts — instead of a `host()` helper private to
/// this file. And it is asserted against the base [`range_target`] ACTUALLY RESOLVES rather than
/// against a literal paired by hand here, so re-pointing the fetcher fails this test too.
#[test]
fn each_spec_discovers_the_budget_of_the_host_it_pages() {
    for symbol in ["BTCUSDT", "BTCUSDT.P"] {
        let (base, _, spec) = routed(symbol, None).expect("routes");
        let info = klines::discovery_url(spec, base)
            .unwrap_or_else(|| panic!("{symbol}: binance publishes a budget on both hosts"));
        assert_eq!(
            klines::url_host(info),
            klines::url_host(base),
            "{info} must be the budget of {base}'s host"
        );
    }
    // ...and the two hosts are genuinely different, so the pairing above is not vacuous.
    assert_ne!(BINANCE_KLINE_SPOT.exchange_info_url, BINANCE_KLINE_PERP.exchange_info_url);
    // CROSSED, which is the defect itself: each spec against the OTHER host's base discovers
    // nothing at all, so a mispairing degrades to the fixed `page_delay` instead of pacing fapi
    // against spot's 6000.
    assert_eq!(klines::discovery_url(&BINANCE_KLINE_SPOT, REST_KLINES_FAPI), None);
    assert_eq!(klines::discovery_url(&BINANCE_KLINE_PERP, REST_KLINES), None);
}

/// **Byte-identity of the discovery request across the `&'static str` -> `Cow` change.** The
/// URL each spec is asked for is the SAME literal it named before, so both hosts issue the same
/// one `exchangeInfo` GET they issued before, read the same budget, and pace the same way. The
/// literals are spelled out rather than compared to the consts on purpose: a test that compares
/// a const to itself would pass through a rename of the endpoint.
#[test]
fn the_discovery_urls_are_the_literals_they_have_always_been() {
    let (spot_base, _, spot) = routed("BTCUSDT", None).expect("routes");
    let (perp_base, _, perp) = routed("BTCUSDT.P", None).expect("routes");
    assert_eq!(
        klines::discovery_url(spot, spot_base),
        Some("https://api.binance.com/api/v3/exchangeInfo")
    );
    assert_eq!(
        klines::discovery_url(perp, perp_base),
        Some("https://fapi.binance.com/fapi/v1/exchangeInfo")
    );
    // Both are BORROWED — binance's hosts are compile-time constants, so nothing is allocated
    // and the two specs remain `const`. (Aster's are `Owned`; that is the capability the field's
    // type change exists for.)
    for spec in [&BINANCE_KLINE_SPOT, &BINANCE_KLINE_PERP] {
        assert_matches!(
            spec.exchange_info_url,
            Some(Cow::Borrowed(_)),
            "a static host must not allocate"
        );
    }
}

/// Utilization is vike-model's operator-facing default at every const site — this crate does not
/// carry a second copy of the number, and a venue does not get to pick its own risk appetite.
#[test]
fn both_specs_target_the_shared_default_utilization() {
    for spec in [BINANCE_KLINE_SPOT, BINANCE_KLINE_PERP] {
        assert_eq!(
            spec.utilization.to_bits(),
            vike_model::rate_limits::DEFAULT_UTILIZATION.to_bits()
        );
    }
}

// Binance's OWN host-resolution proof (the per-venue delta): the shared pure `parse_klines`/URL
// shaping is proven once at the rung (`crate::family::klines`); here we prove the api/fapi hosts.
#[test]
fn klines_url_includes_optional_bounds() {
    let latest = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        latest,
        "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let ranged = klines_url(REST_KLINES, "BTCUSDT", "1m", Some(10), Some(20), 1000);
    assert!(ranged.ends_with("&startTime=10&endTime=20"));
}

#[test]
fn klines_url_perp_uses_fapi_host_spot_uses_api_host() {
    let perp = klines_url(REST_KLINES_FAPI, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(
        perp,
        "https://fapi.binance.com/fapi/v1/klines?symbol=BTCUSDT&interval=1m&limit=1000"
    );
    let spot = klines_url(REST_KLINES, "BTCUSDT", "1m", None, None, 1000);
    assert_eq!(spot, "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000");
}

#[test]
fn fetch_klines_latest_base_url_selects_fapi_for_perp() {
    // rest_klines_base is the pure host-selection the perp seed relies on — no network.
    assert_eq!(rest_klines_base(true), REST_KLINES_FAPI);
    assert_eq!(rest_klines_base(false), REST_KLINES);
}

/// The perp suffix selects the fapi host AND is stripped from the wire symbol. This is the
/// site the `.P` convention was skipped at: before this, `BTCUSDT.P` silently fetched SPOT
/// bars and stored them under the perp key.
#[test]
fn perp_suffix_selects_fapi_and_strips_the_wire_symbol() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT.P");
    assert_eq!(wire, "BTCUSDT");
    assert!(perp);
    let url = klines_url(rest_klines_base(perp), wire, "1m", Some(10), Some(20), 1000);
    assert!(url.starts_with(REST_KLINES_FAPI), "perp must use fapi: {url}");
    assert!(url.contains("symbol=BTCUSDT&"), "the .P must not reach the wire: {url}");
    assert!(!url.contains(".P"), "the .P must not reach the wire: {url}");
}

/// A bare symbol is unchanged — spot URLs stay byte-identical.
#[test]
fn a_bare_symbol_still_builds_the_spot_url() {
    let (wire, perp) = vike_catalog::split_perp("BTCUSDT");
    assert!(!perp);
    let url = klines_url(rest_klines_base(perp), wire, "1m", None, None, 1000);
    assert_eq!(url, "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1m&limit=1000");
}

// ---- the THIRD book -----------------------------------------------------------------

/// **THE FINDING, closed.** A COIN-M perpetual reaches the COIN-M host with the suffix
/// stripped, and carries the COIN-M budget with it.
///
/// ⚠ This is the test the mutation proof reddens: re-point `book_target`'s `Book::CoinM` arm
/// (or `route_target`'s listing arm) anywhere else and this fails on the host it actually
/// resolved. It asserts the RESOLVED triple rather than a hand-paired literal, so a mutation
/// cannot satisfy it by editing a constant beside it.
#[test]
fn a_coin_m_perpetual_routes_to_the_coin_m_host_with_the_suffix_stripped() {
    let (base, wire, spec) = routed("BTCUSD_PERP.P", None).expect("a COIN-M perpetual routes");
    assert_eq!(
        (base, wire),
        (REST_KLINES_DAPI, "BTCUSD_PERP"),
        "a COIN-M symbol must reach the COIN-M host, and the .P must not reach the wire"
    );
    assert_eq!(spec, &BINANCE_KLINE_COIN_M);
    let url = klines_url(base, wire, "1h", Some(10), Some(20), 1000);
    assert_eq!(
        url,
        "https://dapi.binance.com/dapi/v1/klines?symbol=BTCUSD_PERP&interval=1h\
             &limit=1000&startTime=10&endTime=20"
    );
}

/// ⚠ **The unit defect, gated at the route.** Index 5 of a COIN-M row is a CONTRACT COUNT and
/// index 7 is the base asset; every other binance book is the other way round. A COIN-M route
/// that carried `Index5` would fetch correct-looking bars with a ~760x wrong `Bar::volume` —
/// and 0059's self-sealing commit key would make that window unrepairable.
#[test]
fn only_the_coin_m_book_reads_the_base_asset_from_the_other_column() {
    assert_eq!(
        routed("BTCUSD_PERP.P", None).expect("routes").2.volume_column,
        VolumeColumn::Index7
    );
    for usual in ["BTCUSDT", "BTCUSDT.P"] {
        assert_eq!(
            routed(usual, None).expect("routes").2.volume_column,
            VolumeColumn::Index5,
            "{usual} must keep the column every binance bar has always been parsed at"
        );
    }
}

/// A DATED COIN-M contract reached through the perpetual marker is REFUSED, not routed — the
/// suffix says PERPETUAL and the venue says `CURRENT_QUARTER`. The message names the venue's
/// own word, and says outright that no other spelling exists rather than inventing one.
#[test]
fn a_dated_coin_m_contract_is_refused_rather_than_routed() {
    let err = routed("BTCUSD_260925.P", None).expect_err("a dated future is not a perpetual");
    assert!(err.contains("CURRENT_QUARTER"), "the venue's own word must be quoted: {err}");
    assert!(err.contains("CryptoFuture"), "and the class that would name it: {err}");
    assert!(err.contains("catalog.rs"), "and what would have to change first: {err}");
    assert!(
        routed("BTCUSD_261225.P", None)
            .expect_err("both dated arms refuse")
            .contains("NEXT_QUARTER")
    );
}

/// ⚠ **An unreadable listing is an ERROR, never a fall-through to fapi.** The two must not
/// reach the same code path: fapi SERVES the COIN-M tape, so falling through does not fail —
/// it succeeds with the wrong volume column.
#[test]
fn an_unreadable_listing_refuses_instead_of_falling_through_to_fapi() {
    let err = route_target("BTCUSD_PERP.P", None, &unreadable)
        .expect_err("not proven USDⓈ-M must not read as proven USDⓈ-M");
    assert!(err.contains("connection refused"), "the venue's own words survive: {err}");
    // ...and the SAME unreadable listing refuses a perfectly ordinary USDⓈ-M symbol too, which
    // is the accepted residual `crate::instruments` declares: this route cannot tell the two
    // apart without the list, so it refuses both rather than guessing either.
    assert!(route_target("BTCUSDT.P", None, &unreadable).is_err());
}

/// ⚠ **A spot route opens no socket**, proven by a lookup that panics if it is called. This is
/// what keeps every spot backfill in the workspace byte-identical to before the third book
/// existed — the listing is consulted only when it can change the outcome.
#[test]
fn a_spot_route_never_consults_the_coin_m_listing() {
    let calls = Cell::new(0_usize);
    let counting = |wire: &str| {
        calls.set(calls.get() + 1);
        listing(wire)
    };
    for bare in ["BTCUSDT", "ETHUSDT", "BTCUSD_PERP"] {
        let (base, _, _) = route_target(bare, None, &counting).expect("a bare symbol is spot");
        assert_eq!(base, REST_KLINES, "{bare}: a bare symbol is the spot book");
    }
    let (base, _, _) = route_target("BTCUSDT", Some(vike_model::AssetClass::CryptoSpot), &counting)
        .expect("a spot claim is spot");
    assert_eq!(base, REST_KLINES);
    assert_eq!(calls.get(), 0, "a spot route must not reach for the venue's COIN-M listing");
    // ...and the futures route DOES consult it, so the counter above is not vacuous.
    let _ = route_target("BTCUSDT.P", None, &counting).expect("routes");
    assert_eq!(calls.get(), 1);
}

/// A bare `BTCUSD_PERP` still routes to SPOT, which answers `-1121 Invalid symbol` — an ERROR,
/// never another book in silence. That is exactly what `vike_catalog::addressing_for`'s binance
/// row means by `BareSymbol::Unambiguous`, and admitting a third book did not change it: dapi's
/// `symbol` set is DISJOINT from spot's Trading set and from fapi's (MEASURED).
#[test]
fn a_bare_coin_m_symbol_is_still_the_spot_books_problem_not_another_books() {
    let (base, wire, _) = routed("BTCUSD_PERP", None).expect("a bare symbol routes to spot");
    assert_eq!((base, wire), (REST_KLINES, "BTCUSD_PERP"));
    assert!(
        !vike_catalog::addressing_for(VENUE).must_claim(),
        "binance's row stays Unambiguous — the venue REFUSES a futures string on spot rather \
             than answering from another book, which is what that column measures"
    );
}

/// The CLAIM seam: `Some(CryptoPerp)` reaches the futures plane with no suffix, and the listing
/// still decides WHICH futures book — because 0061 refuses to split the variant, so the claim
/// genuinely cannot answer that question.
#[test]
fn a_class_claim_reaches_the_futures_plane_and_the_listing_still_picks_the_book() {
    use vike_model::AssetClass;
    let (base, wire, _) = routed("BTCUSD_PERP", Some(AssetClass::CryptoPerp)).expect("routes");
    assert_eq!((base, wire), (REST_KLINES_DAPI, "BTCUSD_PERP"));
    let (base, _, _) = routed("BTCUSDT", Some(AssetClass::CryptoPerp)).expect("routes");
    assert_eq!(base, REST_KLINES_FAPI, "the same claim, the other book — the listing decided");
}

/// A claim this venue's data path cannot address is refused rather than coerced — and the
/// COIN-M QUARTERLIES are exactly that case, because `CryptoFuture` is not in binance's row.
#[test]
fn a_class_the_row_does_not_name_is_refused() {
    use vike_model::AssetClass;
    let err = routed("BTCUSD_260925", Some(AssetClass::CryptoFuture)).expect_err("refused");
    assert!(err.contains("CryptoFuture"), "{err}");
    assert!(err.contains("BTCUSD_PERP"), "the message must name what IS reachable: {err}");
    assert!(routed("AAPL", Some(AssetClass::Equity)).is_err());
}

/// A suffix and a claim that disagree: neither is obeyed. Bybit's twin, verbatim in shape.
#[test]
fn a_claim_contradicting_the_suffix_is_refused() {
    use vike_model::AssetClass;
    let err = routed("BTCUSDT.P", Some(AssetClass::CryptoSpot)).expect_err("refused");
    assert!(err.contains("two claims that disagree"), "{err}");
    assert!(err.contains("Drop the suffix or drop the claim"), "{err}");
}

/// ⚠ **The claim [`kline_market`] rests on.** That key names a BUDGET, and it stays two-valued
/// across three hosts only because binance's two FUTURES hosts publish ONE budget — MEASURED
/// 2400/min and weight 5 per page on both. Give dapi its own budget and this fails, which is
/// the signal to split the pace key (and to mint the `vike_model::venues::venue_rate_limits::Market`
/// variant this deliberately did not).
#[test]
fn the_coin_m_host_is_paced_on_the_measured_twin_of_fapis_budget() {
    assert_eq!(
        BINANCE_KLINE_COIN_M.rate_limit, BINANCE_KLINE_PERP.rate_limit,
        "the COIN-M spec takes fapi's budget BY CONSTRUCTION; if that stops being true, \
             `kline_market` must grow a third key before this route ships"
    );
    assert_eq!(kline_market("BTCUSD_PERP.P"), "perp");
    // ...and the DISCOVERY url is NOT shared, because a published budget belongs to a host.
    let (base, _, spec) = routed("BTCUSD_PERP.P", None).expect("routes");
    assert_eq!(
        klines::discovery_url(spec, base),
        Some("https://dapi.binance.com/dapi/v1/exchangeInfo")
    );
    // Crossed, the pairing degrades to the fallback pace rather than pacing against another
    // machine's counter — the same guard the two older specs already carry.
    assert_eq!(klines::discovery_url(&BINANCE_KLINE_COIN_M, REST_KLINES_FAPI), None);
    assert_eq!(klines::discovery_url(&BINANCE_KLINE_PERP, REST_KLINES_DAPI), None);
}

/// The three books resolve three DISTINCT hosts — so `book_target` is a real fan-out and not
/// two arms with a third alias.
#[test]
fn the_three_books_are_three_hosts() {
    let hosts = [Book::Spot, Book::UsdM, Book::CoinM].map(|b| book_target(b).0);
    assert_eq!(hosts, [REST_KLINES, REST_KLINES_FAPI, REST_KLINES_DAPI]);
    let unique: std::collections::BTreeSet<&str> = hosts.into_iter().collect();
    assert_eq!(unique.len(), 3, "three books must not share a host");
}

// --- funding-rate parser (moved here from vike-backfill's funding_rate_tests.rs, 0094) ---------

/// The real `/fapi/v1/fundingRate` shape: `fundingTime` is a NUMBER; `fundingRate`/`markPrice`
/// are STRINGS. Both the string→f64 decode and a negative rate must survive.
#[test]
fn parse_binance_decodes_ts_and_string_rate() {
    let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1700000000000,"fundingRate":"0.00010000","markPrice":"37000.0"},
            {"symbol":"BTCUSDT","fundingTime":1700028800000,"fundingRate":"-0.00005500","markPrice":"37100.0"}
        ]"#;
    let pts = parse_funding_rates(body).unwrap();
    assert_eq!(pts.len(), 2);
    assert_eq!(pts[0].ts_ms, 1_700_000_000_000);
    assert!((pts[0].rate - 0.0001).abs() < 1e-12, "decimal string → f64");
    assert_eq!(pts[1].ts_ms, 1_700_028_800_000);
    assert!((pts[1].rate + 0.000_055).abs() < 1e-12, "negative rate survives");
}

#[test]
fn parse_binance_empty_array_is_ok_empty() {
    assert_eq!(parse_funding_rates("[]").unwrap(), Vec::new());
}

/// Per-row tolerance: a row missing `fundingRate`, and a row whose rate won't parse, are both
/// skipped; the one well-formed row survives.
#[test]
fn parse_binance_skips_bad_rows_keeps_good() {
    let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1},
            {"symbol":"BTCUSDT","fundingTime":2,"fundingRate":"not-a-number"},
            {"symbol":"BTCUSDT","fundingTime":3,"fundingRate":"0.0002"}
        ]"#;
    let pts = parse_funding_rates(body).unwrap();
    assert_eq!(pts.len(), 1, "only the well-formed row survives");
    assert_eq!(pts[0].ts_ms, 3);
    assert!((pts[0].rate - 0.0002).abs() < 1e-12);
}

#[test]
fn parse_binance_malformed_or_non_array_is_error() {
    assert!(parse_funding_rates("not json").is_err());
    assert!(parse_funding_rates("{}").is_err(), "an object is not the expected array");
    assert!(parse_funding_rates("null").is_err());
}

/// Binance's `/fapi/v1/fundingRate` carries no premium at all. The row must still parse and
/// keep its rate: a venue that sends no premium must not lose its funding history to one.
#[test]
fn parse_binance_has_no_premium_but_keeps_the_rate() {
    let body = r#"[
            {"symbol":"BTCUSDT","fundingTime":1700000000000,"fundingRate":"0.0001","markPrice":"37000.0"}
        ]"#;
    let pts = parse_funding_rates(body).unwrap();
    assert_eq!(pts.len(), 1, "the row survives");
    assert_eq!(pts[0].premium, None, "no premium field on this venue");
    assert!((pts[0].rate - 0.0001).abs() < 1e-12);
}

/// Funding is a PERPETUAL's series, and the store spells a Binance perpetual `SYMBOL.P`: a bare
/// symbol names SPOT and is REFUSED before any request (so this runs offline), rather than filing a
/// perp's funding under the spot key where no perp backtest would look for it.
#[test]
fn a_spot_symbol_is_refused_before_the_funding_endpoint_is_asked() {
    use vike_data::source::{FundingRateSource, SourceError};
    let err = BinanceFunding.fetch("BTCUSDT", 0, 1).expect_err("spot has no funding");
    match err {
        SourceError::Refused(msg) => {
            assert!(msg.contains("BTCUSDT.P"), "names the perpetual spelling: {msg}")
        }
        other => panic!("a spot symbol must be a REFUSAL, not a fetch failure: {other:?}"),
    }
}

// --- COIN-M funding routing (0094 follow-ups, D4) -----------------------------------------

/// [`funding_url_for`]'s three outcomes, driven on the WIRE symbol alone (no `.P`, no network) —
/// the routing [`BinanceFunding::fetch`] applies AFTER stripping the store's perpetual suffix.
/// MEASURED 2026-09-29: fapi answers HTTP 200 with no rows for a COIN-M symbol, so before this
/// routed, `BTCUSD_PERP` silently reached fapi and came back empty rather than reaching dapi.
#[test]
fn funding_url_for_routes_on_the_wire_symbols_own_shape() {
    assert_eq!(
        funding_url_for("BTCUSDT"),
        Ok(FUNDING_URL),
        "a bare symbol is USDⓈ-M, unchanged from before this routed"
    );
    assert_eq!(
        funding_url_for("BTCUSD_PERP"),
        Ok(COIN_M_FUNDING_URL),
        "the venue's own PERPETUAL marker routes to dapi"
    );
    let err = funding_url_for("BTCUSD_260925").expect_err("a dated contract has no funding");
    assert!(
        err.contains("BTCUSD_260925") && err.contains("no funding rate"),
        "names the symbol and says why: {err}"
    );
}

/// A dated COIN-M contract reached through the store's perpetual suffix is refused BEFORE any
/// request — the funding-routing twin of `a_spot_symbol_is_refused_before_the_funding_endpoint_is_asked`
/// above, proving `fetch` actually calls [`funding_url_for`] rather than only the unit test above
/// exercising it in isolation.
#[test]
fn a_dated_coin_m_contract_is_refused_before_the_funding_endpoint_is_asked() {
    use vike_data::source::{FundingRateSource, SourceError};
    let err = BinanceFunding
        .fetch("BTCUSD_260925.P", 0, 1)
        .expect_err("a dated delivery contract has no funding rate to ask for");
    match err {
        SourceError::Refused(msg) => {
            assert!(msg.contains("no funding rate"), "{msg}")
        }
        other => panic!("a dated contract must be a REFUSAL, not a fetch failure: {other:?}"),
    }
}
