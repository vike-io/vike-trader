use super::*;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType};

fn page(symbols: &[(&str, &str)], cursor: &str) -> Value {
    let list: Vec<Value> = symbols
        .iter()
        .map(|(s, status)| serde_json::json!({ "symbol": s, "status": status }))
        .collect();
    serde_json::json!({ "result": { "list": list, "nextPageCursor": cursor } })
}

#[test]
fn only_trading_rows_count() {
    let p = page(&[("BTCUSD", "Trading"), ("OLDUSD", "Delisted"), ("", "Trading")], "");
    let got = trading_symbols(&p);
    assert_eq!(got.len(), 1);
    assert!(got.contains("BTCUSD"));
}

#[test]
fn a_missing_envelope_is_an_empty_page_not_a_panic() {
    assert!(trading_symbols(&serde_json::json!({})).is_empty());
    assert!(trading_symbols(&serde_json::json!({ "result": { "list": "nope" } })).is_empty());
}

/// ⚠ Bybit sends an EMPTY STRING rather than omitting the field when a listing is exhausted.
/// Reading "present" as "more pages" would spin the walk to its bound on every single fetch.
#[test]
fn an_empty_cursor_is_the_end_of_the_listing() {
    assert_eq!(next_page_cursor(&page(&[], "")), None);
    assert_eq!(next_page_cursor(&page(&[], "abc")), Some("abc".to_string()));
    assert_eq!(next_page_cursor(&serde_json::json!({ "result": {} })), None);
}

/// **THE AMBIGUITY PREDICATE, stated as a test.** spot ∩ inverse, and NOT spot ∩ linear — the
/// 290-symbol case that must keep routing to spot is the second half of this assertion.
#[test]
fn the_predicate_is_spot_intersect_inverse() {
    let spot: BTreeSet<String> =
        ["BTCUSD", "ETHUSD", "BTCUSDT", "SOLUSDT"].iter().map(|s| s.to_string()).collect();
    let inverse: BTreeSet<String> =
        ["BTCUSD", "ETHUSD", "XRPUSD"].iter().map(|s| s.to_string()).collect();
    let ambiguous = ambiguous_bare_symbols(&spot, &inverse);
    assert_eq!(
        ambiguous,
        ["BTCUSD", "ETHUSD"].iter().map(|s| s.to_string()).collect::<BTreeSet<_>>()
    );
    assert!(
        !ambiguous.contains("BTCUSDT"),
        "a symbol listed under spot and LINEAR must stay unambiguous — 290 of them on the live \
             venue, and bare->spot is correct for every one"
    );
    assert!(
        !ambiguous.contains("XRPUSD"),
        "an inverse-only symbol has no spot listing to be confused with"
    );
}

/// **THE ROUTE PREDICATE, stated as a test.** Every one of the four answers, including the two
/// that exist only so a venue-side change cannot pass silently.
#[test]
fn the_route_predicate_reads_both_derivative_listings() {
    let linear: BTreeSet<String> =
        ["BTCUSDT", "SOLUSDT", "BOTHUSD"].iter().map(|s| s.to_string()).collect();
    let inverse: BTreeSet<String> =
        ["BTCUSD", "XRPUSD", "BOTHUSD"].iter().map(|s| s.to_string()).collect();

    assert_eq!(perp_book("BTCUSDT", &linear, &inverse), PerpListing::Linear);
    assert_eq!(perp_book("BTCUSD", &linear, &inverse), PerpListing::Inverse);
    assert_eq!(
        perp_book("XRPUSD", &linear, &inverse),
        PerpListing::Inverse,
        "an inverse-only symbol with NO spot twin still routes — 26 of the venue's 28 inverse \
             listings are this case, and asking their caller to name a class would be ceremony"
    );
    assert_eq!(
        perp_book("BOTHUSD", &linear, &inverse),
        PerpListing::Both,
        "the arm that makes `linear n inverse = {{}}` a gate rather than a dated claim"
    );
    assert_eq!(perp_book("NOSUCHUSDT", &linear, &inverse), PerpListing::Unlisted);
}

/// ⚠ The predicate is a set MEMBERSHIP, never a reading of the string. `USD`-vs-`USDT` is what
/// bybit's inverse perps happen to carry today and is exactly the implicit encoding 0061 exists
/// to remove — so a `…USD` symbol the venue lists on LINEAR must answer `Linear`, and a
/// `…USDT` one it lists on INVERSE must answer `Inverse`. Both are hypothetical on the live
/// venue; that is the point.
#[test]
fn the_route_predicate_does_not_read_the_quote_asset_off_the_string() {
    let linear: BTreeSet<String> = ["WEIRDUSD".to_string()].into_iter().collect();
    let inverse: BTreeSet<String> = ["WEIRDUSDT".to_string()].into_iter().collect();
    assert_eq!(perp_book("WEIRDUSD", &linear, &inverse), PerpListing::Linear);
    assert_eq!(perp_book("WEIRDUSDT", &linear, &inverse), PerpListing::Inverse);
}

/// Both predicates are case-insensitive on the way in — the listings are upper-cased on parse,
/// and a caller's lower-case symbol must not read as "not listed".
#[test]
fn a_lower_case_symbol_still_finds_its_listing() {
    let linear: BTreeSet<String> = ["BTCUSDT".to_string()].into_iter().collect();
    let inverse: BTreeSet<String> = ["BTCUSD".to_string()].into_iter().collect();
    assert_eq!(perp_book("btcusd", &linear, &inverse), PerpListing::Inverse);
    assert_eq!(perp_book("btcusdt", &linear, &inverse), PerpListing::Linear);
}

/// The walk follows the cursor and unions the pages. A single-page test cannot see a short read,
/// which is the failure these predicates must not have.
#[test]
fn the_walk_follows_the_cursor_to_exhaustion() {
    let fetch = |url: &str| -> Result<Value, String> {
        if url.contains("cursor=p2") {
            Ok(page(&[("CUSD", "Trading")], ""))
        } else {
            Ok(page(&[("AUSD", "Trading"), ("BUSD", "Trading")], "p2"))
        }
    };
    let got = fetch_trading_symbols("https://example.invalid/x?category=spot", &fetch)
        .expect("walk completes");
    assert_eq!(got.len(), 3);
    assert!(got.contains("CUSD"), "the second page must be unioned in, not dropped");
}

/// A venue that keeps offering pages is a REFUSAL, not a partial answer: an under-read set makes
/// these predicates answer "unambiguous"/"linear" for a symbol they never saw. Hitting the page
/// limit also writes a warn line (owner ruling C-D2).
#[test]
fn an_endless_listing_refuses_rather_than_answering_short() {
    let calls = std::cell::Cell::new(0usize);
    let fetch = |_: &str| -> Result<Value, String> {
        calls.set(calls.get() + 1);
        Ok(page(&[("AUSD", "Trading")], &format!("c{}", calls.get())))
    };
    let (got, lines) =
        vike_log::capture::captured(|| fetch_trading_symbols("https://example.invalid/x", &fetch));
    let err = got.expect_err("an endless listing must not answer");
    assert_eq!(
        err,
        crate::refusals::BybitHistoryRefusal::ListingNeverEnds {
            base_url: "https://example.invalid/x".into(),
            max_pages: MAX_PAGES
        }
    );
    assert_eq!(calls.get(), MAX_PAGES);
    assert!(
        lines.iter().any(|l| l.level == tracing::Level::WARN && l.message == err.to_string()),
        "the refusal is a warn line too: {lines:?}"
    );
}

/// A venue handing back the cursor just spent ends the strict walk at once, refused in words and a
/// warn line (owner ruling C-D2): it used to ask the same cursor again until [`MAX_PAGES`].
#[test]
fn a_repeated_cursor_refuses_at_once() {
    let calls = std::cell::Cell::new(0usize);
    let fetch = |_: &str| -> Result<Value, String> {
        calls.set(calls.get() + 1);
        Ok(page(&[("AUSD", "Trading")], "stuck"))
    };
    let (got, lines) =
        vike_log::capture::captured(|| fetch_trading_symbols("https://example.invalid/x", &fetch));
    let err = got.expect_err("a repeating listing must not answer");
    assert_eq!(
        err,
        crate::refusals::BybitHistoryRefusal::ListingRepeatedCursor {
            base_url: "https://example.invalid/x".into(),
            page: 1
        }
    );
    assert_eq!(calls.get(), 2, "page 0 spends the cursor, page 1 hands it back");
    assert!(
        lines.iter().any(|l| l.level == tracing::Level::WARN && l.message == err.to_string()),
        "the refusal is a warn line too: {lines:?}"
    );
}

/// A fetch failure propagates rather than becoming an empty set — an empty set would read as
/// "nothing is ambiguous" and "no symbol is inverse", which is the fail-permissive shape this
/// whole module exists to avoid.
#[test]
fn a_fetch_failure_is_an_error_not_an_empty_set() {
    let fetch = |_: &str| -> Result<Value, String> { Err("HTTP 503".into()) };
    assert!(derive_listings(&base(), &fetch).is_err());
}

/// The seed's `live` `data` `rest` base: what every production caller reads the listings at.
fn base() -> String {
    let urls = vike_venues::venue_urls::VenueUrls::seed("bybit");
    urls.url(UrlType::Live, UrlChannel::Data, UrlProtocol::Rest, "").expect("seeded").to_string()
}

/// ANY one list failing must not answer from the others. Each category gets its own round of
/// this: an answer assembled from two of three is a different kind of wrong for each hole.
#[test]
fn any_single_list_failing_refuses_the_whole_derivation() {
    for missing in ["category=spot", "category=linear", "category=inverse"] {
        let fetch = |url: &str| -> Result<Value, String> {
            if url.contains(missing) {
                Err("HTTP 503".into())
            } else {
                Ok(page(&[("BTCUSD", "Trading")], ""))
            }
        };
        assert!(
            derive_listings(&base(), &fetch).is_err(),
            "{missing} failing must refuse the whole derivation"
        );
    }
}

/// All three categories are consulted, and each exactly once on a single-page venue. The
/// `limit=1000` half is the fail-permissive guard, asserted on every URL rather than on one.
#[test]
fn all_three_categories_are_read() {
    let seen = std::sync::Mutex::new(Vec::new());
    let fetch = |url: &str| -> Result<Value, String> {
        seen.lock().expect("lock").push(url.to_string());
        Ok(page(&[("BTCUSD", "Trading")], ""))
    };
    let got = derive_listings(&base(), &fetch).expect("derives");
    let urls = seen.lock().expect("lock").clone();
    assert_eq!(urls.len(), 3, "one page per category: {urls:?}");
    for category in ["category=spot", "category=linear", "category=inverse"] {
        assert!(urls.iter().any(|u| u.contains(category)), "{category} unread: {urls:?}");
    }
    assert!(urls.iter().all(|u| u.contains("limit=1000")), "a short page fails permissive");
    // Byte for byte the URLs the constants this replaced held, built from the seed's row.
    assert_eq!(
        urls,
        [
            "https://api.bybit.com/v5/market/instruments-info?category=spot&limit=1000",
            "https://api.bybit.com/v5/market/instruments-info?category=linear&limit=1000",
            "https://api.bybit.com/v5/market/instruments-info?category=inverse&limit=1000",
        ]
    );
    assert!(got.ambiguous.contains("BTCUSD"));
    assert_eq!(
        perp_book("BTCUSD", &got.linear, &got.inverse),
        PerpListing::Both,
        "this fixture plants BTCUSD in every category, so the collision arm is what answers"
    );
}

/// **The live re-derivation** — `#[ignore]`d, network, run by hand. It prints every number this
/// module's doc is sized by, from this module's own code rather than from a shell one-liner, so
/// "if it does not get these, one of us is wrong" is answerable at any time.
///
/// ⚠ The `linear ∩ inverse` line is the one that decides the ROUTE, and it is asserted rather
/// than merely printed: the production guard for a non-empty answer is [`PerpListing::Both`], which
/// no live symbol can reach while this holds.
///
/// ```sh
/// cargo test -p vike-bybit --lib instrument::listing::tests::report_the_counts -- --ignored --nocapture
/// ```
#[test]
#[ignore = "network: keyless public GETs against api.bybit.com"]
fn report_the_counts_that_size_this_module() {
    let list = |category| {
        fetch_trading_symbols(&crate::catalog::listing_url(&base(), category), &fetch_json)
            .unwrap_or_else(|e| panic!("{category} list: {e}"))
    };
    let (spot, linear, inverse) = (list("spot"), list("linear"), list("inverse"));
    let refused = ambiguous_bare_symbols(&spot, &inverse);
    let not_refused: BTreeSet<String> = spot.intersection(&linear).cloned().collect();
    let both_books: BTreeSet<String> = linear.intersection(&inverse).cloned().collect();
    println!(
        "bybit trading symbols: spot={} inverse={} linear={}",
        spot.len(),
        inverse.len(),
        linear.len()
    );
    println!(
        "REFUSED   spot n inverse = {} {:?}",
        refused.len(),
        refused.iter().collect::<Vec<_>>()
    );
    println!("ALLOWED   spot n linear  = {} (deliberately not refused)", not_refused.len());
    println!(
        "ROUTABLE  linear n inverse = {} {:?} (must be 0 — see PerpListing::Both)",
        both_books.len(),
        both_books.iter().collect::<Vec<_>>()
    );
    assert!(!refused.is_empty(), "the predicate selected nothing — re-read this module's doc");
    assert!(
        both_books.is_empty(),
        "linear n inverse is NO LONGER EMPTY: {both_books:?}. A perpetual claim on those \
             symbols now names two books and `route_target` refuses them — which is the designed \
             behaviour, but this module's doc claims the set is empty and must be re-measured"
    );
}
