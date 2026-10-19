//! `IgFactsSource::facts_quoted` over the SCRIPTED LOOPBACK IG double (`fake_ig`): the REAL session, the REAL
//! reads (`GET /accounts`, `GET /markets/{epic}` and, for a cross, the conversion pair's) and the REAL fences,
//! against hand-written bodies in the shape IG's reference documents
//! (<https://labs.ig.com/reference/markets-epic.html>). DOCS-SHAPED: nothing here touches IG's network and
//! nothing proves IG's own numbers; the reference states neither the direction nor the meaning of
//! `currencies[].exchangeRate`, which is exactly why a rate is only admitted beside a price that agrees with it.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vike_ig::{IgFactsError, IgFactsSource, MAX_READS_PER_MINUTE};

use crate::fake_ig::{Route, login_route, serve};

const USDJPY: &str = "CS.D.USDJPY.MINI.IP";
const EURGBP: &str = "CS.D.EURGBP.MINI.IP";
const GBPUSD: &str = "CS.D.GBPUSD.MINI.IP";

/// A market body: `rows` are `(code, exchangeRate)`, `(bid, offer)` the snapshot.
fn market(epic: &str, pip: &str, per_pip: &str, rows: &[(&str, f64)], price: (f64, f64)) -> Value {
    let currencies: Vec<Value> = rows
        .iter()
        .map(|(code, rate)| json!({ "code": code, "exchangeRate": rate, "baseExchangeRate": rate }))
        .collect();
    json!({
        "instrument": {
            "epic": epic, "expiry": "-", "type": "CURRENCIES", "unit": "CONTRACTS", "lotSize": 1.0,
            "currencies": currencies, "onePipMeans": pip, "contractSize": "10000",
            "valueOfOnePip": per_pip
        },
        "dealingRules": { "minDealSize": { "unit": "POINTS", "value": 0.1 } },
        "snapshot": { "marketStatus": "TRADEABLE", "bid": price.0, "offer": price.1 }
    })
}

fn usdjpy() -> Route {
    Route::ok(
        "GET /markets/CS.D.USDJPY.MINI.IP",
        market(USDJPY, "0.01 JPY/USD", "100", &[("JPY", 0.00665), ("USD", 1.0)], (149.99, 150.01)),
    )
}

fn eurgbp() -> Route {
    Route::ok(
        "GET /markets/CS.D.EURGBP.MINI.IP",
        market(EURGBP, "0.0001 GBP/EUR", "1", &[("GBP", 1.35), ("USD", 1.0)], (0.8499, 0.8501)),
    )
}

fn gbpusd() -> Route {
    Route::ok(
        "GET /markets/CS.D.GBPUSD.MINI.IP",
        market(GBPUSD, "0.0001 USD/GBP", "1", &[("USD", 1.0)], (1.3499, 1.3501)),
    )
}

/// An accounts answer in which `ACC1` is of `kind` (`accountType`) and holds `currency`.
fn account_of(kind: Option<&str>, currency: Option<&str>) -> Route {
    let mut row = json!({ "accountId": "ACC1", "balance": { "balance": 1000.0 } });
    if let Some(kind) = kind {
        row["accountType"] = json!(kind);
    }
    if let Some(currency) = currency {
        row["currency"] = json!(currency);
    }
    Route::ok("GET /accounts", json!({ "accounts": [row] }))
}

/// **A yen epic is converted with ONE market read**: the epic's own price witnesses a rate for a pair whose
/// base is the account currency, so no other pair is read; one login, one accounts read. The rate is a fraction
/// of a dollar per yen, stamped inside the call.
#[test]
fn a_yen_epic_is_converted_with_one_market_read_and_one_login() {
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), usdjpy()]);
    let source = IgFactsSource::new(ig.config());
    let before = vike_model::now_ms();
    let admitted = source.facts_quoted(USDJPY).expect("converted");
    let after = vike_model::now_ms();
    let rate = admitted.quote.expect("a yen quote carries its rate");
    assert_eq!((rate.quote_ccy.as_str(), rate.account_per_quote), ("JPY", 0.00665));
    assert!(rate.account_per_quote < 0.01, "a fraction of a dollar per yen, never 150");
    assert!((before..=after).contains(&rate.as_of_ms), "stamped inside the call: {rate:?}");
    assert_eq!(admitted.facts.properties.tick_size, 0.01);
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /accounts"), 1);
    assert_eq!(ig.count("GET /markets/"), 1, "{:?}", ig.served());
}

/// **A cross reads its conversion pair once more**, and the account's currency is still read once for both asks.
#[test]
fn a_cross_reads_its_conversion_pair_and_the_session_is_kept() {
    let ig = serve(vec![
        login_route(),
        account_of(Some("CFD"), Some("USD")),
        eurgbp(),
        gbpusd(),
        usdjpy(),
    ]);
    let source = IgFactsSource::new(ig.config());
    let cross = source.facts_quoted(EURGBP).expect("converted").quote.expect("a pound quote");
    assert_eq!((cross.quote_ccy.as_str(), cross.account_per_quote), ("GBP", 1.35));
    assert_eq!(ig.count("GET /markets/"), 2, "the epic and GBPUSD: {:?}", ig.served());
    let yen = source.facts_quoted(USDJPY).expect("converted").quote.expect("a yen quote");
    assert_eq!(yen.quote_ccy, "JPY");
    assert_eq!(ig.count("POST /session"), 1, "one kept login for every ask");
    assert_eq!(ig.count("GET /accounts"), 1, "the account's row is read once");
}

/// An epic quoted in the account's currency comes back with no rate and costs the one market read, exactly as
/// the plain facts; the plain facts still refuse the yen epic over the same gateway.
#[test]
fn a_dollar_quote_carries_no_rate_and_the_plain_facts_still_refuse_a_yen_quote() {
    let eurusd = Route::ok(
        "GET /markets/CS.D.EURUSD.MINI.IP",
        market("CS.D.EURUSD.MINI.IP", "0.0001 USD/EUR", "1", &[("USD", 1.0)], (1.08, 1.0801)),
    );
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), eurusd, usdjpy()]);
    let source = IgFactsSource::new(ig.config());
    let plain = source.facts("CS.D.EURUSD.MINI.IP").expect("plain");
    let quoted = source.facts_quoted("CS.D.EURUSD.MINI.IP").expect("quoted");
    assert_eq!((quoted.facts, quoted.quote), (plain, None));
    let err = source.facts(USDJPY).expect_err("the fence stands for a node that does not convert");
    assert!(matches!(&err, IgFactsError::Refused(why) if why.contains("quoted in JPY")), "{err:?}");
}

/// The rate is only as good as the price that agrees with it. A yen row stated the other way up (150, yen per
/// dollar) is READ inverted and lands on the same dollar fraction, naming the reading; a number near neither the
/// price nor its reciprocal is `NoRate`, a "not now" about the rate that remembers nothing about the epic; and a
/// cross whose conversion pair IG does not list is `NoRate` likewise.
#[test]
fn a_rate_no_price_confirms_and_an_unlisted_conversion_pair_are_no_rate() {
    let upside_down = Route::ok(
        "GET /markets/CS.D.USDJPY.MINI.IP",
        market(USDJPY, "0.01 JPY/USD", "100", &[("JPY", 150.0), ("USD", 1.0)], (149.99, 150.01)),
    );
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), upside_down]);
    let rate = IgFactsSource::new(ig.config())
        .facts_quoted(USDJPY)
        .expect("read inverted")
        .quote
        .expect("converted");
    assert_eq!(rate.account_per_quote, 1.0 / 150.0, "a dollar fraction, never 150");
    assert_eq!(rate.reading, vike_ig::Reading::QuoteRowInverse);

    let nonsense = Route::ok(
        "GET /markets/CS.D.USDJPY.MINI.IP",
        market(USDJPY, "0.01 JPY/USD", "100", &[("JPY", 0.5), ("USD", 1.0)], (149.99, 150.01)),
    );
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), nonsense]);
    let err = IgFactsSource::new(ig.config()).facts_quoted(USDJPY).expect_err("no reading agrees");
    assert!(
        matches!(&err, IgFactsError::NoRate(why) if why.contains("no reading of IG's currencies table")),
        "{err:?}"
    );

    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), eurgbp()]);
    let err = IgFactsSource::new(ig.config()).facts_quoted(EURGBP).expect_err("no GBPUSD");
    assert!(matches!(&err, IgFactsError::NoRate(why) if why.contains("lists neither")), "{err:?}");
    assert_eq!(ig.count("GET /markets/"), 3, "the epic, then both spellings: {:?}", ig.served());
}

/// A conversion pair that cannot be read is "not now", not a verdict.
#[test]
fn a_conversion_pair_that_fails_to_read_is_unavailable() {
    let down = Route::status("GET /markets/CS.D.GBPUSD.MINI.IP", 503, json!({ "errorCode": "x" }));
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), eurgbp(), down]);
    let err = IgFactsSource::new(ig.config()).facts_quoted(EURGBP).expect_err("503");
    assert!(matches!(&err, IgFactsError::Unavailable(why) if why.contains("503")), "{err:?}");
}

/// **The account's currency is the account's own, and an unknown one is an error, never a default**: no
/// readable currency means no rate for anything, and `account_currency` says so.
#[test]
fn an_account_with_no_readable_currency_converts_nothing() {
    for currency in [None, Some("dollars"), Some("")] {
        let ig = serve(vec![login_route(), account_of(Some("CFD"), currency), usdjpy()]);
        let source = IgFactsSource::new(ig.config());
        let err = source.account_currency().expect_err("no currency");
        assert!(
            matches!(&err, IgFactsError::Account(why) if why.contains("no readable currency")),
            "{currency:?}: {err:?}"
        );
        let err = source.facts_quoted(USDJPY).expect_err("no currency");
        assert!(matches!(&err, IgFactsError::Account(_)), "{currency:?}: {err:?}");
        assert_eq!(
            ig.count("GET /accounts"),
            1,
            "{currency:?}: the account row that cannot be used is remembered, not read again"
        );
        assert_eq!(
            ig.count("GET /markets/"),
            0,
            "no market is read without the account's currency"
        );
    }
}

/// `account_currency` is the venue's statement, upper-cased, read once with the same kept login the facts use.
#[test]
fn the_accounts_currency_is_read_once_and_shared_with_the_facts() {
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("usd")), usdjpy()]);
    let source = IgFactsSource::new(ig.config());
    assert_eq!(source.account_currency().expect("read"), "USD");
    assert_eq!(source.account_currency().expect("kept"), "USD");
    source.facts_quoted(USDJPY).expect("converted");
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /accounts"), 1);
}

/// A spread-bet or physical account is not converted for (a stake per point and a share count are other
/// products); a CFD account is. **An account row that states NO type is refused too**: IG always states it, so
/// its absence means the row is not the shape this was read against, and a missing field never opens a fence.
#[test]
fn only_a_cfd_account_is_converted_for() {
    for kind in ["SPREADBET", "PHYSICAL"] {
        let ig = serve(vec![login_route(), account_of(Some(kind), Some("USD")), usdjpy()]);
        let err = IgFactsSource::new(ig.config()).facts_quoted(USDJPY).expect_err(kind);
        assert!(
            matches!(&err, IgFactsError::Refused(why) if why.contains(kind) && why.contains("only a CFD account")),
            "{kind}: {err:?}"
        );
        assert_eq!(ig.count("GET /markets/"), 0, "{kind}: refused before the epic is read");
    }
    let ig = serve(vec![login_route(), account_of(None, Some("USD")), usdjpy()]);
    let err = IgFactsSource::new(ig.config()).facts_quoted(USDJPY).expect_err("no type stated");
    assert!(
        matches!(&err, IgFactsError::Refused(why)
            if why.contains("states no accountType") && why.contains("only a CFD account")),
        "{err:?}"
    );
    assert_eq!(ig.count("GET /markets/"), 0, "refused before the epic is read");
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), usdjpy()]);
    IgFactsSource::new(ig.config()).facts_quoted(USDJPY).expect("a CFD account is converted for");
}

const EURCHF: &str = "CS.D.EURCHF.MINI.IP";
const GBPCHF: &str = "CS.D.GBPCHF.MINI.IP";

fn eurchf() -> Route {
    Route::ok(
        "GET /markets/CS.D.EURCHF.MINI.IP",
        market(EURCHF, "0.0001 CHF/EUR", "1", &[("CHF", 1.11), ("USD", 1.0)], (0.9399, 0.9401)),
    )
}

fn gbpchf() -> Route {
    Route::ok(
        "GET /markets/CS.D.GBPCHF.MINI.IP",
        market(GBPCHF, "0.0001 CHF/GBP", "1", &[("CHF", 1.11), ("USD", 1.0)], (1.1599, 1.1601)),
    )
}

/// `USD/CHF`, the only spelling of the franc's conversion pair that exists at this IG (`CHFUSD` is a 404).
fn usdchf() -> Route {
    Route::ok(
        "GET /markets/CS.D.USDCHF.MINI.IP",
        market("CS.D.USDCHF.MINI.IP", "0.0001 CHF/USD", "1", &[("USD", 1.0)], (0.8999, 0.9001)),
    )
}

/// **A refresh of several crosses costs IG the fewest reads**: the franc's conversion pair is spelled
/// `CHFUSD` first (a 404 here) and `USDCHF` second. The first cross pays for finding that out (the epic, the 404,
/// the pair); a second cross quoted in francs, asked a moment later, reads only its own epic: the missing spelling
/// is remembered and the pair's market details are shared. (The node refreshes every held rate about once a
/// minute against a budget of 12 fetches, so a wasted call per cross per minute was the load.)
#[test]
fn a_second_cross_in_the_same_quote_currency_reads_only_its_own_epic() {
    let ig = serve(vec![
        login_route(),
        account_of(Some("CFD"), Some("USD")),
        eurchf(),
        gbpchf(),
        usdchf(),
    ]);
    let source = IgFactsSource::new(ig.config());
    let first = source.facts_quoted(EURCHF).expect("converted").quote.expect("a franc quote");
    assert_eq!(first.quote_ccy, "CHF");
    assert_eq!(ig.count("GET /markets/"), 3, "the epic, CHFUSD (404), USDCHF: {:?}", ig.served());
    let second = source.facts_quoted(GBPCHF).expect("converted").quote.expect("a franc quote");
    assert_eq!(second.account_per_quote, 1.11);
    assert_eq!(ig.count("GET /markets/"), 4, "only the second epic: {:?}", ig.served());
    assert_eq!(ig.count("GET /markets/CS.D.CHFUSD"), 1, "the missing spelling is not asked again");
    assert_eq!(ig.count("GET /markets/CS.D.USDCHF"), 1, "the pair's details are shared");
    // a third ask of the first cross re-reads its own epic (a rate is never served from here), nothing else
    source.facts_quoted(EURCHF).expect("again");
    assert_eq!(ig.count("GET /markets/"), 5, "{:?}", ig.served());
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /accounts"), 1);
}

/// **The source holds its own per-minute cap on the reads it makes of IG** ([`MAX_READS_PER_MINUTE`], a figure
/// of this adapter's, unmeasured, used when IG's own allowance was not read): the read past it is "not now" and
/// never reaches IG, the login and the account's row counted among them. The plain (non-converting) ask has no
/// refreshes to leave a reserve for and keeps the whole cap.
#[test]
fn the_source_stops_reading_at_its_own_per_minute_cap() {
    let eurusd = Route::ok(
        "GET /markets/CS.D.EURUSD.MINI.IP",
        market("CS.D.EURUSD.MINI.IP", "0.0001 USD/EUR", "1", &[("USD", 1.0)], (1.08, 1.0801)),
    );
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), eurusd]);
    let source = IgFactsSource::new(ig.config());
    // the login and the account's row are two of the cap's reads
    for _ in 0..MAX_READS_PER_MINUTE - 2 {
        source.facts("CS.D.EURUSD.MINI.IP").expect("under the cap");
    }
    assert_eq!(ig.count("POST /session"), 1);
    assert_eq!(ig.count("GET /accounts"), 1);
    assert_eq!(ig.count("GET /markets/"), MAX_READS_PER_MINUTE - 2);
    let err = source.facts("CS.D.EURUSD.MINI.IP").expect_err("the cap is spent");
    assert!(
        matches!(&err, IgFactsError::Unavailable(why) if why.contains("at most 20 times a minute")),
        "{err:?}"
    );
    assert_eq!(ig.count("GET /markets/"), MAX_READS_PER_MINUTE - 2, "the capped read never left");
}

/// A first look at an epic nobody has a route for: IG answers 404 (`{}`), which is still a read.
fn first_look(source: &IgFactsSource, n: usize) -> Result<(), IgFactsError> {
    let epic = format!("CS.D.X{n}USD.MINI.IP");
    match source.facts_quoted(&epic) {
        Err(IgFactsError::Refused(_)) => Ok(()),
        other => other.map(|_| ()),
    }
}

/// **The reserve for refreshes is real**: a flood of FIRST LOOKS at new epics is held to half of the cap, so the
/// re-read of an epic that has already answered with a rate (a held position's, kept inside the engine's 120 s)
/// always has the other half. Before, the daemon reserved 6 of its 12 fetches for refreshes but a fetch is two or
/// three reads and the cap was one bucket: a minute of first looks left ~4 reads for the 6 reserved refreshes.
/// A refresh still may spend the WHOLE cap, and the first looks that were turned away never reached IG.
#[test]
fn a_flood_of_first_looks_cannot_drain_the_reads_a_refresh_needs() {
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), usdjpy()]);
    let source = IgFactsSource::new(ig.config());
    // the epic that holds a position: its first look answers with a rate, so every ask after is a refresh
    source.facts_quoted(USDJPY).expect("converted").quote.expect("a yen quote");
    // login, accounts and IG's allowance (an unanswered 404 here: the fallback cap stands) are 3 reads, the
    // epic's first look the 4th; the first looks are held to 10 (half of 20)
    let mut admitted = 1;
    let turned_away = loop {
        match first_look(&source, admitted) {
            Ok(()) => admitted += 1,
            Err(err) => break err,
        }
        assert!(admitted < 40, "the flood is never stopped");
    };
    assert_eq!(admitted, MAX_READS_PER_MINUTE / 2, "10 first looks, the epic's own included");
    assert!(
        matches!(&turned_away, IgFactsError::Unavailable(why)
            if why.contains("first looks") && why.contains("at most 20 times a minute")),
        "{turned_away:?}"
    );
    let before = ig.count("GET /markets/");
    assert_eq!(before, MAX_READS_PER_MINUTE / 2, "the 11th first look never left");
    // ...and the refreshes of the held epic go on, up to the whole cap (3 + 10 spent, 7 left)
    for round in 0..MAX_READS_PER_MINUTE - 13 {
        source.facts_quoted(USDJPY).unwrap_or_else(|e| panic!("refresh {round}: {e:?}"));
    }
    let err = source.facts_quoted(USDJPY).expect_err("the whole cap is spent");
    assert!(
        matches!(&err, IgFactsError::Unavailable(why) if why.contains("at most 20")),
        "{err:?}"
    );
    assert_eq!(ig.count("GET /markets/"), before + MAX_READS_PER_MINUTE - 13);
}

/// **The cap is two thirds of the key's own allowance when IG states it** (`GET /operations/application`, read
/// once per source and counted in the budget); a read that fails, or that states nothing usable, leaves the
/// 20 a minute this adapter chose (its origin is unmeasured). A plain ask never makes the extra request.
#[test]
fn the_cap_follows_the_keys_own_allowance_and_falls_back_to_twenty() {
    let app = |body: Value| Route::ok("GET /operations/application", body);
    // 60 a minute per account: 40 reads
    let ig = serve(vec![
        login_route(),
        account_of(Some("CFD"), Some("USD")),
        app(json!([
            { "apiKey": "other", "allowanceAccountOverall": 5 },
            { "apiKey": "KEY", "allowanceAccountOverall": 60, "allowanceAccountTrading": 100 }
        ])),
        usdjpy(),
    ]);
    let source = IgFactsSource::new(ig.config());
    let mut done = 0;
    while source.facts_quoted(USDJPY).is_ok() {
        done += 1;
        assert!(done < 100, "never capped");
    }
    // login, accounts and the allowance are 3 of the 40 reads
    assert_eq!((ig.count("GET /operations/application"), ig.count("GET /markets/")), (1, 37));
    assert_eq!(done, 37);
    let err = source.facts_quoted(USDJPY).expect_err("capped");
    assert!(
        matches!(&err, IgFactsError::Unavailable(why) if why.contains("at most 40")),
        "{err:?}"
    );

    // the same key described as one object; a plain ask asks nothing
    let ig = serve(vec![
        login_route(),
        account_of(Some("CFD"), Some("USD")),
        app(json!({ "allowanceAccountOverall": 30 })),
        usdjpy(),
    ]);
    let source = IgFactsSource::new(ig.config());
    source.facts_quoted(USDJPY).expect("converted");
    assert_eq!(ig.count("GET /operations/application"), 1);
    source.facts_quoted(USDJPY).expect("again");
    assert_eq!(ig.count("GET /operations/application"), 1, "once per source");
    let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), usdjpy()]);
    IgFactsSource::new(ig.config()).facts(USDJPY).expect_err("the plain fence");
    assert_eq!(ig.count("GET /operations/application"), 0, "a plain ask makes no extra request");

    // a failed read, and bodies that state nothing usable: the fallback of 20
    for (what, route) in [
        ("500", Route::status("GET /operations/application", 500, json!({}))),
        ("no figure", app(json!({ "status": "ENABLED" }))),
        ("zero", app(json!({ "allowanceAccountOverall": 0 }))),
        ("not our key", app(json!([{ "apiKey": "other", "allowanceAccountOverall": 90 }]))),
        ("a string", app(json!("busy"))),
    ] {
        let ig = serve(vec![login_route(), account_of(Some("CFD"), Some("USD")), route, usdjpy()]);
        let source = IgFactsSource::new(ig.config());
        let mut done = 0;
        while source.facts_quoted(USDJPY).is_ok() {
            done += 1;
            assert!(done < 100, "{what}: never capped");
        }
        assert_eq!(
            done,
            MAX_READS_PER_MINUTE - 3,
            "{what}: login, accounts and the allowance are 3"
        );
    }
}

/// **The start waits a bounded time for the account's currency**, and the read it gave up on is not wasted:
/// the login and the account row it finishes are kept, so the next ask (the next start) costs nothing.
#[test]
fn the_start_waits_a_bounded_time_for_the_accounts_currency_and_the_read_is_kept() {
    let ig = serve(vec![
        login_route().slow(Duration::from_millis(600)),
        account_of(Some("CFD"), Some("USD")),
    ]);
    let source = Arc::new(IgFactsSource::new(ig.config()));
    let began = Instant::now();
    let err = source.account_currency_within(Duration::from_millis(100)).expect_err("too slow");
    assert!(
        began.elapsed() < Duration::from_millis(500),
        "the wait is the bound: {:?}",
        began.elapsed()
    );
    assert!(
        matches!(&err, IgFactsError::Unavailable(why) if why.contains("not read within")),
        "{err:?}"
    );
    // the abandoned read finishes in the background; asking again shares its login and its row
    let currency = source.account_currency_within(Duration::from_secs(10)).expect("kept");
    assert_eq!(currency, "USD");
    assert_eq!(ig.count("POST /session"), 1, "one login for both asks");
    assert_eq!(ig.count("GET /accounts"), 1);
    let began = Instant::now();
    source.account_currency_within(Duration::from_millis(100)).expect("kept, instant");
    assert!(began.elapsed() < Duration::from_millis(100), "{:?}", began.elapsed());
}

/// A text that cannot stand in a URL path never leaves the process, on this door too.
#[test]
fn a_bad_epic_dials_nothing_on_the_quoted_door_either() {
    let ig = serve(vec![login_route()]);
    let err = IgFactsSource::new(ig.config()).facts_quoted("CS.D.USDJPY/../x").expect_err("bad");
    assert!(
        matches!(&err, IgFactsError::Refused(why) if why.contains("is not an epic")),
        "{err:?}"
    );
    assert!(ig.served().is_empty(), "{:?}", ig.served());
}

/// **A failure only a change can cure is remembered by the source, and a blip is not**: a refused login (wrong
/// details, a disabled key), and an account row IG does not list, answer `Account` and are NOT asked of IG again
/// by a caller that keeps asking (one login per 5 s per key was the cost, against an account IG may lock on
/// repeated failed logins); a timeout-class failure (a 503) is "not now" and is asked again, every attempt counted
/// in the cap like any other read.
#[test]
fn a_refused_login_is_not_dialled_again_and_a_blip_is_but_both_count_in_the_cap() {
    let refused = serve(vec![Route::status(
        "POST /session",
        401,
        json!({ "errorCode": "error.security.invalid-details" }),
    )]);
    let source = IgFactsSource::new(refused.config());
    for epic in [USDJPY, EURGBP, USDJPY] {
        let err = source.facts_quoted(epic).expect_err("refused");
        assert!(
            matches!(&err, IgFactsError::Account(why)
                if why.starts_with("ig login: ") && why.contains("invalid-details")),
            "{err:?}"
        );
    }
    assert_eq!(
        refused.count("POST /session"),
        1,
        "the refusal is remembered: {:?}",
        refused.served()
    );
    // a rotated credential is another source (the caller replaces it), which dials again
    let mut rotated = refused.config();
    rotated.password = "new".to_string();
    assert!(!source.serves(&rotated));

    // a 403 that is an allowance is "not now", not a refusal
    let allowance = serve(vec![Route::status(
        "POST /session",
        403,
        json!({ "errorCode": "error.public-api.exceeded-api-key-allowance" }),
    )]);
    let source = IgFactsSource::new(allowance.config());
    for _ in 0..3 {
        let err = source.facts_quoted(USDJPY).expect_err("allowance");
        assert!(matches!(&err, IgFactsError::Unavailable(_)), "{err:?}");
    }
    assert_eq!(allowance.count("POST /session"), 3, "asked again each time");

    // a blip is asked again, and every attempt is a read of the cap: the dials stop at it
    let down = serve(vec![Route::status("POST /session", 503, json!({ "errorCode": "x" }))]);
    let source = IgFactsSource::new(down.config());
    for _ in 0..MAX_READS_PER_MINUTE + 5 {
        let err = source.facts_quoted(USDJPY).expect_err("down");
        assert!(matches!(&err, IgFactsError::Unavailable(_)), "{err:?}");
    }
    assert_eq!(down.count("POST /session"), MAX_READS_PER_MINUTE, "logins are counted in the cap");

    // an account row that is not listed: remembered, not read again
    let unlisted = serve(vec![
        login_route(),
        Route::ok(
            "GET /accounts",
            json!({ "accounts": [{ "accountId": "OTHER", "currency": "USD" }] }),
        ),
        usdjpy(),
    ]);
    let source = IgFactsSource::new(unlisted.config());
    for _ in 0..3 {
        let err = source.facts_quoted(USDJPY).expect_err("not listed");
        assert!(
            matches!(&err, IgFactsError::Account(why) if why.contains("lists no account")),
            "{err:?}"
        );
    }
    assert_eq!(unlisted.count("GET /accounts"), 1);
    assert_eq!(unlisted.count("GET /markets/"), 0);
}

/// **One lookup has one overall deadline**: a cross is up to four serial HTTP calls at 30 s each on the shared
/// agent, and the daemon admits four lookups at once for ALL venues; four hung IG lookups held every slot for ~120 s
/// and every other venue's lookup was answered `busy`. Past the deadline the lookup is "not now" at once; the read
/// it gave up on finishes in the background and its login and rows serve the next ask.
#[test]
fn a_lookup_over_its_deadline_is_not_now_and_the_read_it_gave_up_on_is_kept() {
    // a mute-enough gateway: its login answers only after 800 ms
    for plain in [false, true] {
        let ig = serve(vec![
            login_route().slow(Duration::from_millis(800)),
            account_of(Some("CFD"), Some("USD")),
            usdjpy(),
        ]);
        let source = Arc::new(IgFactsSource::new(ig.config()));
        let began = Instant::now();
        let err = if plain {
            source
                .facts_within(USDJPY, Duration::from_millis(100))
                .map(|_| ())
                .expect_err("too slow")
        } else {
            source
                .facts_quoted_within(USDJPY, Duration::from_millis(100))
                .map(|_| ())
                .expect_err("too slow")
        };
        assert!(
            matches!(&err, IgFactsError::Unavailable(why) if why.contains("did not finish within")),
            "plain {plain}: {err:?}"
        );
        assert!(
            began.elapsed() < Duration::from_millis(600),
            "plain {plain}: the wait is the bound: {:?}",
            began.elapsed()
        );
        // the abandoned lookup finishes in the background; the next ask finds its login and row kept
        let admitted =
            source.facts_quoted_within(USDJPY, Duration::from_secs(10)).expect("converted");
        assert_eq!(admitted.quote.expect("a yen quote").quote_ccy, "JPY", "plain {plain}");
        assert_eq!(ig.count("POST /session"), 1, "plain {plain}: one login for every ask");
        assert_eq!(ig.count("GET /accounts"), 1, "plain {plain}");
    }
}
