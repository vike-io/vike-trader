//! The symbol picker's per-query seam on the IG side (`CatalogProviderIg::search_remote`), over `fake_ig` (a hand-written
//! loopback gateway, never IG): every search rides ONE kept login and is counted against the source's per-minute
//! read cap. The catalog used to log in afresh for each search, and a picker asks on every debounced keystroke.

use serde_json::json;
use vike_catalog::{CatalogProvider, CatalogSearchFilter};
use vike_ig::refusals::IgSymbolRefusal;
use vike_ig::{CatalogProviderIg, IgPropertiesSource};

use crate::fake_ig::{self, Route};

fn gateway() -> fake_ig::FakeIg {
    fake_ig::serve(vec![
        fake_ig::login_route(),
        Route::ok(
            "GET /markets?searchTerm=",
            json!({ "markets": [{
                "epic": "CS.D.EURUSD.MINI.IP", "instrumentName": "EUR/USD Mini",
                "instrumentType": "CURRENCIES", "expiry": "-"
            }] }),
        ),
    ])
}

/// Three searches, one `POST /session`; the term is percent-encoded onto the query.
///
/// MUTATION PROOF: log in inside `IgPropertiesSource::search` per call (the old `fetch_markets` shape) - three logins.
#[test]
fn searches_share_one_login() {
    let ig = gateway();
    let catalog = CatalogProviderIg::over(IgPropertiesSource::new(ig.config()));
    for term in ["eur", "eur u", "eur us"] {
        let hits =
            catalog.search_remote(term, &CatalogSearchFilter::default()).expect("a search answers");
        assert_eq!(hits.len(), 1, "{term}");
        assert_eq!(hits[0].raw_symbol, "CS.D.EURUSD.MINI.IP");
    }
    assert_eq!(ig.count("POST /session"), 1, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets?searchTerm="), 3, "{:?}", ig.served());
    assert!(ig.served().iter().any(|l| l.contains("searchTerm=eur%20u")), "{:?}", ig.served());
}

/// A typist who outruns the source's per-minute read cap is told "not now" (retry shortly), and the searches that
/// were refused cost IG nothing: the login and the first nineteen searches fill the cap of twenty.
#[test]
fn searches_are_counted_against_the_read_cap() {
    let ig = gateway();
    let catalog = CatalogProviderIg::over(IgPropertiesSource::new(ig.config()));
    let answers: Vec<_> = (0..25)
        .map(|i| catalog.search_remote(&format!("t{i}"), &CatalogSearchFilter::default()))
        .collect();
    let ok = answers.iter().filter(|a| a.is_ok()).count();
    assert_eq!(ok, 19, "the login spent one of the twenty reads: {answers:?}");
    let why = &answers.last().unwrap().as_ref().unwrap_err().0;
    assert_eq!(why, &IgSymbolRefusal::ReadCapSpent { cap: 20 }.carried(), "the cap is \"not now\"");
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /markets?searchTerm="), 19, "a refused search sends nothing");
}

/// No credentials, no search and no error: the live gate of every credentialed provider.
#[test]
fn a_catalog_without_credentials_answers_empty() {
    let catalog = CatalogProviderIg::new(
        &vike_secrets::account_credentials::AccountCredentials::none(),
        &vike_venues::venue_urls::VenueUrls::seed("ig"),
    )
    .expect("absent creds look nothing up");
    assert!(
        catalog.search_remote("eur", &CatalogSearchFilter::default()).expect("no error").is_empty()
    );
}
