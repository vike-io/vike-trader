//! The operator's lookup of one pair (`vike_ig::operator_row`), over `fake_ig` (a hand-written loopback gateway,
//! never IG): ONE search and ONE market read, over the kept session of a facts source the process already holds, and
//! no login of its own.

use std::sync::Arc;

use serde_json::Value;
use vike_ig::IgPropertiesSource;
use vike_ig::operator_row::{hold_source, operator_lookup};

use crate::fake_ig::{self, Route};

const SEARCHES: &str = include_str!("../fixtures/markets_search_pairs.json");
const EURUSD_MINI: &str = include_str!("../fixtures/markets/CS.D.EURUSD.MINI.IP.json");

fn gateway() -> fake_ig::FakeIg {
    let searches: Value = serde_json::from_str(SEARCHES).expect("the fixture parses");
    fake_ig::serve(vec![
        fake_ig::login_route(),
        Route::ok("GET /markets?searchTerm=EURUSD", searches["EURUSD"].clone()),
        Route::ok(
            "GET /markets/CS.D.EURUSD.MINI.IP",
            serde_json::from_str(EURUSD_MINI).expect("the capture parses"),
        ),
    ])
}

/// The pick first with its details, the passed-over forms after it; one login, one search, one market read.
///
/// MUTATION PROOF: read the details of every candidate (not only the pick) and the market count is 2.
#[test]
fn one_pair_costs_one_search_and_one_market_read() {
    let ig = gateway();
    let source = IgPropertiesSource::new(ig.config());
    let rows = source.operator_row("EURUSD").expect("a row");
    let names: Vec<&str> = rows.iter().map(|r| r.raw_symbol.as_str()).collect();
    assert_eq!(
        names,
        [
            "CS.D.EURUSD.MINI.IP",
            "CS.D.EURUSD.CFD.IP",
            "CS.D.EURUSD.TODAY.IP",
            "CS.D.EURUSD.FWM2.IP"
        ]
    );
    assert_eq!(rows[0].properties.tick_size, 0.0001);
    assert_eq!(rows[0].quote, "USD");
    assert_eq!(
        rows[1].properties.tick_size, 0.0,
        "a passed-over form is the search row, no details read"
    );
    assert_eq!(ig.count("POST /session"), 1, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets?searchTerm=EURUSD"), 1, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets/"), 1, "{:?}", ig.served());
}

/// Through the process's held session: a source the gatewayhub keeps for an account's facts is the one asked, and its
/// ONE login serves the lookup after the facts' own reads (no second `POST /session`).
#[test]
fn the_held_session_is_the_one_asked() {
    let ig = gateway();
    let source = Arc::new(IgPropertiesSource::new(ig.config()));
    source.search("EURUSD").expect("the account's own earlier read opens the session");
    hold_source(&source);
    let rows = operator_lookup("EURUSD").expect("a row over the held session");
    assert_eq!(rows[0].raw_symbol, "CS.D.EURUSD.MINI.IP");
    assert_eq!(ig.count("POST /session"), 1, "no login of its own: {:?}", ig.served());
}
