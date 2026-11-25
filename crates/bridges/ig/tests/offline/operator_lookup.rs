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
const EURUSD_CFD: &str = include_str!("../fixtures/markets/CS.D.EURUSD.CFD.IP.json");

fn gateway() -> fake_ig::FakeIg {
    let searches: Value = serde_json::from_str(SEARCHES).expect("the fixture parses");
    fake_ig::serve(vec![
        fake_ig::login_route(),
        Route::ok("GET /markets?searchTerm=EURUSD", searches["EURUSD"].clone()),
        Route::ok(
            "GET /markets/CS.D.EURUSD.MINI.IP",
            serde_json::from_str(EURUSD_MINI).expect("the capture parses"),
        ),
        Route::ok(
            "GET /markets/CS.D.EURUSD.CFD.IP",
            serde_json::from_str(EURUSD_CFD).expect("the capture parses"),
        ),
    ])
}

/// **`EURUSD.CFD` adds the standard lot** (decision 0290 item 6, 2026-11-17): IG is searched for the pair alone, the
/// standard lot's details are the one market read, and its row is named `EURUSD.CFD` at ten Minis' unit.
#[test]
fn the_cfd_name_adds_the_standard_lot_named_eurusd_cfd() {
    let ig = gateway();
    let source = IgPropertiesSource::new(ig.config());
    let rows = source.operator_row("EURUSD.CFD").expect("a row");
    assert_eq!(rows[0].raw_symbol, "CS.D.EURUSD.CFD.IP");
    assert_eq!(vike_catalog::user_name_of_row(&rows[0]).as_deref(), Ok("EURUSD.CFD"));
    assert_eq!(rows[0].properties.contract_size, 100_000.0);
    assert_eq!(rows[0].properties.min_qty, 1.0);
    assert_eq!(ig.count("GET /markets?searchTerm=EURUSD"), 1, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets/CS.D.EURUSD.CFD.IP"), 1, "{:?}", ig.served());
    assert_eq!(ig.count("GET /markets/"), 1, "{:?}", ig.served());
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
/// ONE login serves the lookup after the facts' own reads (no second `POST /session`). Once its account no longer
/// runs, the same source is refused in the gatewayhub's words and IG is asked nothing (one test: the slot is
/// process-wide).
#[test]
fn the_held_session_is_the_one_asked_while_its_account_runs() {
    let ig = gateway();
    let source = Arc::new(IgPropertiesSource::new(ig.config()));
    source.search("EURUSD").expect("the account's own earlier read opens the session");
    hold_source(&source, Arc::new(|| Ok(())));
    let rows = operator_lookup("EURUSD").expect("a row over the held session");
    assert_eq!(rows[0].raw_symbol, "CS.D.EURUSD.MINI.IP");
    assert_eq!(ig.count("POST /session"), 1, "no login of its own: {:?}", ig.served());

    let asked = ig.served().len();
    hold_source(&source, Arc::new(|| Err("ig: the account `default` does not run".to_string())));
    let why = operator_lookup("EURUSD").expect_err("its account stopped");
    assert_eq!(why, "ig: the account `default` does not run");
    assert_eq!(ig.served().len(), asked, "nothing asked of IG: {:?}", ig.served());
}
