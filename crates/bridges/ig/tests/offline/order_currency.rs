//! The `currencyCode` of an IG order is the EPIC's, read once from its `GET /markets/{epic}` table, not a
//! constant: an epic that lists USD keeps the bytes IG has always been sent, an epic that lists another
//! currency ALONE (an FX Mini lists its QUOTE currency only: EURGBP lists GBP, measured on the demo
//! gateway) is written in it, and an epic whose table lists none is refused in words before anything is
//! POSTed. Driven through the real exec client over `fake_ig` (a hand-written loopback gateway, never IG).

use std::time::{Duration, Instant};

use serde_json::json;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_ig::IgExecutionClient;
use vike_model::events::Event;
use vike_model::{OrderRequest, OrderType};

use crate::fake_ig::{self, FakeIg, Route};

const EURUSD: &str = "CS.D.EURUSD.MINI.IP";
const EURGBP: &str = "CS.D.EURGBP.MINI.IP";

fn market_route(key: &'static str, epic: &str, currencies: &[&str]) -> Route {
    let rows: Vec<_> =
        currencies.iter().map(|c| json!({ "code": c, "isDefault": false })).collect();
    Route::ok(key, json!({ "instrument": { "epic": epic, "currencies": rows } }))
}

/// `POST /session` for an account in `currency` (`currencyIsoCode`, which the reply may also omit).
fn login(currency: Option<&str>) -> Route {
    Route {
        body: json!({ "currentAccountId": "ACC1", "lightstreamerEndpoint": "",
                      "currencyIsoCode": currency })
        .to_string(),
        ..fake_ig::login_route()
    }
}

/// Submit one MARKET buy of `epic` through the real exec client against a gateway that serves `market`
/// (or, with `None`, answers its market-details read 404). Returns every event up to the order's end
/// and the gateway, to read what was POSTed.
fn submit(epic: &'static str, market: Option<Route>) -> (Vec<Event>, FakeIg) {
    submit_as(epic, market, None)
}

/// [`submit`] as an account whose login states `account_currency`.
fn submit_as(
    epic: &'static str,
    market: Option<Route>,
    account_currency: Option<&str>,
) -> (Vec<Event>, FakeIg) {
    submit_at(epic, market, account_currency, 42)
}

/// [`submit_as`] for a request stamped `ts` (0 = an order from the command line, which states none).
pub(crate) fn submit_at(
    epic: &'static str,
    market: Option<Route>,
    account_currency: Option<&str>,
    ts: i64,
) -> (Vec<Event>, FakeIg) {
    let mut routes = vec![
        login(account_currency),
        Route::ok("POST /positions/otc", json!({ "dealReference": "DREF1" })),
        Route::ok(
            "GET /confirms/",
            json!({ "dealStatus": "ACCEPTED", "dealReference": "DREF1", "dealId": "DIAAA1",
                    "epic": epic, "direction": "BUY", "size": 1.0, "level": 0.84288 }),
        ),
    ];
    routes.extend(market);
    let ig = fake_ig::serve(routes);
    let (events, mut rx) = event_channel(64);
    let mut client = IgExecutionClient::spawn(ig.config(), events);
    client.submit(&OrderRequest {
        client_order_id: "c-1".into(),
        venue: "ig".into(),
        symbol: epic.into(),
        side: 1,
        qty: 1.0,
        order_type: OrderType::Market,
        ts,
        ..Default::default()
    });
    let (mut seen, deadline) = (Vec::new(), Instant::now() + Duration::from_secs(10));
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(Ingest::Event(ev)) => {
                let end = matches!(ev, Event::OrderRejected(_) | Event::OrderFilled(_));
                seen.push(ev);
                if end {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    client.detach();
    (seen, ig)
}

/// The golden that must NOT move: an epic that lists USD is written `currencyCode` USD, as always.
#[test]
fn an_epic_that_lists_usd_is_still_written_usd() {
    let (events, ig) =
        submit(EURUSD, Some(market_route("GET /markets/CS.D.EURUSD", EURUSD, &["USD"])));
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
    let posted = ig.bodies_of("POST /positions/otc");
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0]["currencyCode"], "USD", "{}", posted[0]);
}

/// The defect: EURGBP lists GBP ALONE, and every order was written USD (IG: "Restricted to available
/// instrument currencies"), so IG was expected to refuse it as not tradeable in that currency.
#[test]
fn a_gbp_only_epic_is_written_gbp() {
    let (events, ig) =
        submit(EURGBP, Some(market_route("GET /markets/CS.D.EURGBP", EURGBP, &["GBP"])));
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
    let posted = ig.bodies_of("POST /positions/otc");
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert_eq!(posted[0]["currencyCode"], "GBP", "{}", posted[0]);
    assert_eq!(ig.count("GET /markets/CS.D.EURGBP.MINI.IP"), 1, "one table read for the epic");
}

/// A table that lists no currency leaves nothing to write: the order is refused IN WORDS, as a terminal
/// reject, and no request reaches the deal endpoint.
#[test]
fn an_epic_that_lists_no_currency_is_refused_before_anything_is_sent() {
    let (events, ig) = submit(EURGBP, Some(market_route("GET /markets/CS.D.EURGBP", EURGBP, &[])));
    let reasons: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{events:?}");
    assert!(
        reasons[0].contains(EURGBP) && reasons[0].contains("currenc"),
        "the refusal names the epic and the missing currency: {}",
        reasons[0]
    );
    assert_eq!(ig.count("POST /positions/otc"), 0, "nothing may be POSTed for an unwritable order");
}

/// Several currencies listed and no USD among them: the account's own, as its login states it, is written;
/// an account that states none gets the first listed.
#[test]
fn the_accounts_own_currency_breaks_a_tie_between_listed_currencies() {
    let two = || market_route("GET /markets/CS.D.EURGBP", EURGBP, &["EUR", "GBP"]);
    let (events, ig) = submit_as(EURGBP, Some(two()), Some("GBP"));
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
    assert_eq!(ig.bodies_of("POST /positions/otc")[0]["currencyCode"], "GBP");
    let (_, ig) = submit_as(EURGBP, Some(two()), None);
    assert_eq!(ig.bodies_of("POST /positions/otc")[0]["currencyCode"], "EUR");
}

/// FAIL CLOSED: an epic that lists GBP alone, whose table cannot be read, is refused in words (retryable)
/// and nothing is POSTed: the old `USD` guess would have been a terminal reject at best.
#[test]
fn an_unreadable_table_for_a_gbp_only_epic_refuses_and_posts_nothing() {
    let (events, ig) = submit(EURGBP, None);
    let reasons: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            Event::OrderRejected(r) => Some(r.reason.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(reasons.len(), 1, "{events:?}");
    assert!(
        reasons[0].contains(EURGBP)
            && reasons[0].contains("dealing currencies")
            && reasons[0].contains("retry"),
        "{}",
        reasons[0]
    );
    assert_eq!(ig.count("POST /positions/otc"), 0, "nothing may be POSTed for a guessed currency");
}

/// The wiring-pinned epic (EUR/USD mini, which lists USD) is the exception: a table that cannot be READ
/// is not a verdict on the epic that has always traded, so the order goes out exactly as it always did
/// (USD), and an IG hiccup on a market-details read cannot stop it.
#[test]
fn an_unreadable_table_keeps_the_old_wire_currency_for_the_wiring_pinned_epic() {
    let (events, ig) = submit(EURUSD, None);
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
    assert_eq!(ig.bodies_of("POST /positions/otc")[0]["currencyCode"], "USD");
}

/// **A NON-USD account is written in the EPIC's currency, never in the account's** (decision 0183's mounted-symbol
/// amendment; the proof the reopener "IG non-USD accounts are wanted" asked for, OFFLINE: no pound or euro IG demo
/// exists, so nothing here is IG's own answer). A pound account's mounted EUR/USD mini lists USD alone and is
/// written `USD`; its EURGBP mini lists GBP alone and is written `GBP`; a yen epic is written `JPY`; a euro account
/// is no different. The account only breaks a tie between several listed currencies
/// (`the_accounts_own_currency_breaks_a_tie_between_listed_currencies`), and the wired EUR/USD epic keeps trading
/// `USD` through an unreadable table whatever the account's money is.
#[test]
fn a_non_usd_account_is_written_in_the_epics_currency_not_its_own() {
    const USDJPY: &str = "CS.D.USDJPY.MINI.IP";
    for (account, epic, listed, written) in [
        ("GBP", EURUSD, &["USD"][..], "USD"),
        ("GBP", EURGBP, &["GBP"][..], "GBP"),
        ("GBP", USDJPY, &["JPY"][..], "JPY"),
        ("EUR", EURUSD, &["USD"][..], "USD"),
        ("EUR", EURGBP, &["GBP"][..], "GBP"),
        ("JPY", EURUSD, &["USD"][..], "USD"),
    ] {
        let key = match epic {
            EURUSD => "GET /markets/CS.D.EURUSD",
            EURGBP => "GET /markets/CS.D.EURGBP",
            _ => "GET /markets/CS.D.USDJPY",
        };
        let (events, ig) = submit_as(epic, Some(market_route(key, epic, listed)), Some(account));
        assert!(
            events.iter().any(|e| matches!(e, Event::OrderFilled(_))),
            "{account} {epic}: {events:?}"
        );
        let posted = ig.bodies_of("POST /positions/otc");
        assert_eq!(posted.len(), 1, "{account} {epic}: {posted:?}");
        assert_eq!(posted[0]["currencyCode"], written, "{account} {epic}: {}", posted[0]);
    }
    // the table of the wired epic cannot be read on a pound account: still USD, as it always was
    let (events, ig) = submit_as(EURUSD, None, Some("GBP"));
    assert!(events.iter().any(|e| matches!(e, Event::OrderFilled(_))), "{events:?}");
    assert_eq!(ig.bodies_of("POST /positions/otc")[0]["currencyCode"], "USD");
}
