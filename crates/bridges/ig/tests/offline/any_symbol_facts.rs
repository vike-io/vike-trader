//! `IgFactsSource` over a SCRIPTED LOOPBACK IG double (`fake_ig`): the REAL session, the REAL two reads
//! (`GET /accounts`, `GET /markets/{epic}`) and the REAL fences, against hand-written bodies in the
//! documented shapes. Nothing here touches IG's network, and nothing proves IG's own numbers: that is the
//! demo proof the plan owes.

use serde_json::{Value, json};
use vike_ig::{IgFactsError, IgFactsSource};
use vike_model::ContractValue;

use crate::fake_ig::{Route, accounts_route, login_route, serve};

fn market(epic: &str, pair: &str, quote: &str, pip: &str, per_pip: &str) -> Value {
    json!({
        "instrument": {
            "epic": epic, "expiry": "-", "type": "CURRENCIES", "unit": "CONTRACTS", "lotSize": 1.0,
            "currencies": [{ "code": "USD" }],
            "onePipMeans": format!("{pip} {quote}/{pair}"), "contractSize": "10000",
            "valueOfOnePip": per_pip
        },
        "dealingRules": { "minDealSize": { "unit": "POINTS", "value": 0.1 } }
    })
}

const EUR: &str = "CS.D.EURUSD.MINI.IP";
const GBP: &str = "CS.D.GBPUSD.MINI.IP";

/// ONE login serves every epic asked (the session is kept), the account's currency is read ONCE, and each
/// epic costs exactly one market read.
#[test]
fn one_kept_session_answers_every_epic_with_one_market_read_each() {
    let ig = serve(vec![
        login_route(),
        accounts_route("USD"),
        Route::ok("GET /markets/CS.D.EURUSD.MINI.IP", market(EUR, "EUR", "USD", "0.0001", "1")),
        Route::ok("GET /markets/CS.D.GBPUSD.MINI.IP", market(GBP, "GBP", "USD", "0.0001", "1")),
    ]);
    let source = IgFactsSource::new(ig.config());
    let eur = source.facts(EUR).expect("EUR/USD");
    let gbp = source.facts(GBP).expect("GBP/USD");
    assert_eq!(eur.properties.tick_size, 0.0001);
    assert_eq!(eur.properties.min_qty, 0.1);
    assert_eq!(gbp.contract_value, ContractValue::linear(10_000.0));
    assert_eq!(ig.count("POST /session"), 1, "one login for two epics");
    assert_eq!(ig.count("GET /accounts"), 1, "the currency cannot differ per epic: read once");
    assert_eq!(ig.count("GET /markets/"), 2);
    assert!(
        ig.served().iter().any(|l| l.starts_with("GET /markets/CS.D.EURUSD.MINI.IP ")),
        "the epic is the path: {:?}",
        ig.served()
    );
}

/// IG's own "no such epic" is a REFUSAL in IG's words; a refused login or a failing read is only
/// "not now", which a node must not remember.
#[test]
fn the_venues_answers_are_named_and_only_a_no_is_a_refusal() {
    let unknown = serve(vec![
        login_route(),
        accounts_route("USD"),
        Route::status(
            "GET /markets/",
            404,
            json!({ "errorCode": "error.service.marketdata.instrument.epic.unavailable" }),
        ),
    ]);
    let IgFactsError::Refused(why) =
        IgFactsSource::new(unknown.config()).facts(EUR).expect_err("IG says no such epic")
    else {
        panic!("a 404 on the epic is a definitive no");
    };
    assert!(why.contains("GET /markets/CS.D.EURUSD.MINI.IP") && why.contains("404"), "{why}");
    assert!(why.contains("epic.unavailable"), "{why}");

    let down = serve(vec![
        login_route(),
        accounts_route("USD"),
        Route::status("GET /markets/", 503, json!({ "errorCode": "error.service.unavailable" })),
    ]);
    assert!(
        matches!(
            IgFactsSource::new(down.config()).facts(EUR),
            Err(IgFactsError::Unavailable(why)) if why.contains("503")
        ),
        "a 503 is not an answer about the epic"
    );

    let refused = serve(vec![Route::status(
        "POST /session",
        401,
        json!({ "errorCode": "error.security.invalid-details" }),
    )]);
    let err = IgFactsSource::new(refused.config()).facts(EUR).expect_err("login refused");
    assert!(
        matches!(&err, IgFactsError::Unavailable(why) if why.starts_with("ig login: ")),
        "{err:?}"
    );
    assert!(err.text().contains("invalid-details"), "{err:?}");
}

/// The account's currency is read from the account the session is scoped to, and a quote that is not it
/// is refused before any grid word (the fence the plan's blocker 1 asks for).
#[test]
fn a_quote_in_another_currency_than_the_accounts_is_refused() {
    let usdjpy = "CS.D.USDJPY.MINI.IP";
    let ig = serve(vec![
        login_route(),
        accounts_route("USD"),
        Route::ok("GET /markets/CS.D.USDJPY.MINI.IP", market(usdjpy, "USD", "JPY", "0.01", "100")),
    ]);
    let err = IgFactsSource::new(ig.config()).facts(usdjpy).expect_err("JPY quote");
    assert!(matches!(&err, IgFactsError::Refused(_)), "{err:?}");
    assert!(err.text().contains("quoted in JPY but this account's currency is USD"), "{err:?}");

    let gbp_account = serve(vec![
        login_route(),
        accounts_route("GBP"),
        Route::ok("GET /markets/CS.D.EURUSD.MINI.IP", market(EUR, "EUR", "USD", "0.0001", "1")),
    ]);
    let err = IgFactsSource::new(gbp_account.config()).facts(EUR).expect_err("GBP account");
    assert!(err.text().contains("quoted in USD but this account's currency is GBP"), "{err:?}");
}

/// A text that cannot stand in a URL path never leaves the process; and a rotated credential is a
/// different login (the caller replaces the source).
#[test]
fn a_bad_epic_dials_nothing_and_a_rotated_credential_is_another_login() {
    let ig = serve(vec![login_route()]);
    let source = IgFactsSource::new(ig.config());
    let err = source.facts("CS.D.EURUSD/../x").expect_err("not an epic");
    assert!(
        matches!(&err, IgFactsError::Refused(why) if why.contains("is not an epic")),
        "{err:?}"
    );
    assert!(ig.served().is_empty(), "no login, no read: {:?}", ig.served());

    assert!(source.serves(&ig.config()));
    let mut rotated = ig.config();
    rotated.password = "new".to_string();
    assert!(!source.serves(&rotated));
}
