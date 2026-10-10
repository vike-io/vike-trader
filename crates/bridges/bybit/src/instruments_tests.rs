use super::*;

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

    assert_eq!(perp_book("BTCUSDT", &linear, &inverse), PerpBook::Linear);
    assert_eq!(perp_book("BTCUSD", &linear, &inverse), PerpBook::Inverse);
    assert_eq!(
        perp_book("XRPUSD", &linear, &inverse),
        PerpBook::Inverse,
        "an inverse-only symbol with NO spot twin still routes — 26 of the venue's 28 inverse \
             listings are this case, and asking their caller to name a class would be ceremony"
    );
    assert_eq!(
        perp_book("BOTHUSD", &linear, &inverse),
        PerpBook::Both,
        "the arm that makes `linear n inverse = {{}}` a gate rather than a dated claim"
    );
    assert_eq!(perp_book("NOSUCHUSDT", &linear, &inverse), PerpBook::Unlisted);
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
    assert_eq!(perp_book("WEIRDUSD", &linear, &inverse), PerpBook::Linear);
    assert_eq!(perp_book("WEIRDUSDT", &linear, &inverse), PerpBook::Inverse);
}

/// Both predicates are case-insensitive on the way in — the listings are upper-cased on parse,
/// and a caller's lower-case symbol must not read as "not listed".
#[test]
fn a_lower_case_symbol_still_finds_its_listing() {
    let linear: BTreeSet<String> = ["BTCUSDT".to_string()].into_iter().collect();
    let inverse: BTreeSet<String> = ["BTCUSD".to_string()].into_iter().collect();
    assert_eq!(perp_book("btcusd", &linear, &inverse), PerpBook::Inverse);
    assert_eq!(perp_book("btcusdt", &linear, &inverse), PerpBook::Linear);
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
/// these predicates answer "unambiguous"/"linear" for a symbol they never saw.
#[test]
fn an_endless_listing_refuses_rather_than_answering_short() {
    let fetch = |_: &str| -> Result<Value, String> { Ok(page(&[("AUSD", "Trading")], "more")) };
    let err = fetch_trading_symbols("https://example.invalid/x", &fetch)
        .expect_err("an endless listing must not answer");
    assert!(err.contains("may be short"), "{err}");
}

/// A fetch failure propagates rather than becoming an empty set — an empty set would read as
/// "nothing is ambiguous" and "no symbol is inverse", which is the fail-permissive shape this
/// whole module exists to avoid.
#[test]
fn a_fetch_failure_is_an_error_not_an_empty_set() {
    let fetch = |_: &str| -> Result<Value, String> { Err("HTTP 503".into()) };
    assert!(derive_listings(&fetch).is_err());
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
            derive_listings(&fetch).is_err(),
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
    let got = derive_listings(&fetch).expect("derives");
    let urls = seen.lock().expect("lock").clone();
    assert_eq!(urls.len(), 3, "one page per category: {urls:?}");
    for category in ["category=spot", "category=linear", "category=inverse"] {
        assert!(urls.iter().any(|u| u.contains(category)), "{category} unread: {urls:?}");
    }
    assert!(urls.iter().all(|u| u.contains("limit=1000")), "a short page fails permissive");
    assert!(got.ambiguous.contains("BTCUSD"));
    assert_eq!(
        perp_book("BTCUSD", &got.linear, &got.inverse),
        PerpBook::Both,
        "this fixture plants BTCUSD in every category, so the collision arm is what answers"
    );
}

/// **The live re-derivation** — `#[ignore]`d, network, run by hand. It prints every number this
/// module's doc is sized by, from this module's own code rather than from a shell one-liner, so
/// "if it does not get these, one of us is wrong" is answerable at any time.
///
/// ⚠ The `linear ∩ inverse` line is the one that decides the ROUTE, and it is asserted rather
/// than merely printed: the production guard for a non-empty answer is [`PerpBook::Both`], which
/// no live symbol can reach while this holds.
///
/// ```sh
/// cargo test -p vike-bybit --lib instruments::tests::report_the_counts -- --ignored --nocapture
/// ```
#[test]
#[ignore = "network: keyless public GETs against api.bybit.com"]
fn report_the_counts_that_size_this_module() {
    let spot = fetch_trading_symbols(SPOT_INSTRUMENTS_URL, &fetch_json).expect("spot list");
    let linear = fetch_trading_symbols(LINEAR_INSTRUMENTS_URL, &fetch_json).expect("linear list");
    let inverse =
        fetch_trading_symbols(INVERSE_INSTRUMENTS_URL, &fetch_json).expect("inverse list");
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
        "ROUTABLE  linear n inverse = {} {:?} (must be 0 — see PerpBook::Both)",
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
