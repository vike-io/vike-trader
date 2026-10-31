//! Tests of `crate::home_conversion`: the account's currency, an instrument's quote currency, the rate and
//! the facts. DOCS-SHAPED, NOT MEASURED: every row below is what the ForexConnect documentation says an Offers
//! row or the system properties hold (`IO2GSystemPropertiesReader`'s `BASE_CRNCY`, the Offers table page:
//! `ContractCurrency` is the BASE currency, a forex price is "the counter currency per one unit of base
//! currency", `InstrumentType` 1 is forex) and nothing here has been read off a live FXCM session. The
//! numbers are goldens in the sense of decision 0021: never widen a tolerance to make one pass.

use super::*;
use serde_json::json;

/// A forex offer `name` quoted `bid`/`ask`, subscribed and open, 5 digits, a 1000 lot.
fn offer(name: &str, bid: f64, ask: f64) -> Offer {
    Offer {
        instrument: name.to_string(),
        bid,
        ask,
        digits: 5,
        instrument_type: INSTRUMENT_FOREX,
        contract_currency: name.split('/').next().unwrap_or_default().to_string(),
        contract_multiplier: 1.0,
        subscription: "T".to_string(),
        trading_status: "O".to_string(),
        bid_tradable: "T".to_string(),
        ask_tradable: "T".to_string(),
        base_unit: 1000,
        quote_age_ms: Some(0),
        terms: None,
    }
}

fn venue(code: &str) -> AccountCcy {
    AccountCcy { code: code.to_string(), source: CcySource::Venue }
}

fn rate(offers: &[Offer], quote: &str, account: &str) -> f64 {
    quote_rate(offers, quote, account, 7).expect("a rate").account_per_quote.get()
}

// ---- the account's currency ----------------------------------------------------------------------

/// **API-reported EUR account: EUR is used, as the venue states it, and nothing is assumed.**
#[test]
fn a_currency_the_venue_states_is_used_and_never_assumed() {
    let body = r#"{"BASE_CRNCY":"EUR","BASE_UNIT_SIZE":"1000","BASE_TIME_ZONE":"UTC"}"#;
    let got = account_ccy(PropertiesRead::Json(body.to_string())).expect("a currency");
    assert_eq!(got, venue("EUR"));
    assert!(!got.is_assumed());

    // Spelled the way a human or an older server might: padded, lower case.
    let padded =
        account_ccy(PropertiesRead::Json(r#"{"BASE_CRNCY":" gbp "}"#.to_string())).unwrap();
    assert_eq!(padded, venue("GBP"));
}

/// **The API cannot give it: USD, ASSUMED, flagged, and the reason travels with it.** Three ways the API can
/// stay silent: the property is absent or blank, and the read itself was not possible (a shim installed before
/// `fc_system_properties`, no session).
#[test]
fn a_currency_the_api_cannot_give_is_usd_assumed_and_flagged() {
    for body in
        [r#"{}"#, r#"{"BASE_UNIT_SIZE":"1000"}"#, r#"{"BASE_CRNCY":""}"#, r#"{"BASE_CRNCY":"  "}"#]
    {
        let got =
            account_ccy(PropertiesRead::Json(body.to_string())).expect("assumed, not refused");
        assert_eq!(got.code, "USD", "{body}");
        assert!(got.is_assumed(), "{body}");
        let CcySource::Assumed { why } = &got.source else { panic!("{body}: not assumed") };
        assert!(why.contains(BASE_CURRENCY_PROPERTY), "the reason names the property: {why}");
    }
    let unreadable = "the shim exports no `fc_system_properties`; reinstall it";
    let got = account_ccy(PropertiesRead::Unreadable(unreadable.to_string())).unwrap();
    assert_eq!(got.code, ASSUMED_ACCOUNT_CURRENCY);
    assert_eq!(got.source, CcySource::Assumed { why: unreadable.to_string() });
}

/// A statement that is not a currency is NOT silence: the fallback is for an API that cannot say, not for
/// one that said something wrong.
#[test]
fn a_malformed_statement_is_refused_and_not_assumed_over() {
    for body in [
        r#"{"BASE_CRNCY":"US"}"#,
        r#"{"BASE_CRNCY":"USDT"}"#,
        r#"{"BASE_CRNCY":"1$X"}"#,
        r#"["BASE_CRNCY","USD"]"#,
        "not json",
    ] {
        let why = account_ccy(PropertiesRead::Json(body.to_string()))
            .expect_err(&format!("{body} must be refused"));
        assert!(why.contains("not"), "{body}: {why}");
    }
}

// ---- the Offers table ------------------------------------------------------------------------------

/// The shim's own row shape (`fc_offers`) parses into the typed row, and a damaged row costs only itself.
#[test]
fn the_shims_offers_array_parses_and_a_damaged_row_costs_only_itself() {
    let row = |name: &str| {
        json!({
            "instrument": name, "bid": 150.08, "ask": 150.12, "digits": 3, "instrument_type": 1,
            "contract_currency": "USD", "contract_multiplier": 1.0, "subscription": "T",
            "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T", "base_unit": 1000,
            "quote_age_ms": 120
        })
    };
    let mut damaged = row("EUR/JPY");
    damaged.as_object_mut().unwrap().remove("ask");
    let body = serde_json::Value::Array(vec![row("USD/JPY"), damaged, row("EUR/USD")]).to_string();
    let offers = parse_offers(&body).expect("an array");
    let names: Vec<&str> = offers.iter().map(|o| o.instrument.as_str()).collect();
    assert_eq!(names, ["USD/JPY", "EUR/USD"], "the row missing `ask` is skipped, the others kept");
    assert_eq!(offers[0].digits, 3);
    assert_eq!(offers[0].base_unit, 1000);

    assert!(parse_offers("{}").is_err(), "an object is not an Offers table");
    assert!(parse_offers("nope").is_err());
    assert_eq!(
        parse_offers("[]").unwrap(),
        Vec::<Offer>::new(),
        "an empty table is empty, not an error"
    );
    // Every key the parser needs is in the published key list, and the list holds no more.
    let first = row("USD/JPY");
    let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut declared = OFFER_KEYS.to_vec();
    declared.sort_unstable();
    assert_eq!(keys, declared);
}

// ---- an instrument's quote currency -----------------------------------------------------------------

#[test]
fn a_forex_pairs_quote_currency_is_the_second_half_of_its_name() {
    assert_eq!(quote_currency_of(&offer("EUR/USD", 1.0, 1.0)), Ok("USD".to_string()));
    assert_eq!(quote_currency_of(&offer("USD/JPY", 1.0, 1.0)), Ok("JPY".to_string()));
    assert_eq!(quote_currency_of(&offer("EUR/GBP", 1.0, 1.0)), Ok("GBP".to_string()));
}

/// A type the SDK does not name is no class: refused by name, whatever the row looks like.
#[test]
fn a_type_the_sdk_does_not_name_has_no_determinable_quote_currency() {
    for kind in [0, 10, 99, -1] {
        let mut o = offer("XAU/USD", 1.0, 1.0);
        o.instrument_type = kind;
        o.contract_currency = "USD".to_string();
        let why = quote_currency_of(&o).expect_err("an unknown type");
        assert!(why.contains("none of the nine the SDK names"), "type {kind}: {why}");
        assert!(Class::of(kind).is_none());
    }
    for (kind, class) in [
        (1, Class::Forex),
        (2, Class::Index),
        (3, Class::Commodity),
        (4, Class::Treasury),
        (5, Class::Bullion),
        (6, Class::Share),
        (7, Class::FxIndex),
        (8, Class::CfdShare),
        (9, Class::Crypto),
    ] {
        assert_eq!(Class::of(kind), Some(class), "the SDK's O2GInstrumentType {kind}");
    }
}

/// **A CFD-class row is quoted in its own `ContractCurrency`**, cross-checked against a pair-shaped name, and its
/// multiplier must be a positive number.
#[test]
fn a_cfd_class_row_is_quoted_in_its_contract_currency() {
    let cfd = |name: &str, kind: i32, ccy: &str, mult: f64| {
        let mut o = offer(name, 1.0, 1.0);
        o.instrument_type = kind;
        o.contract_currency = ccy.to_string();
        o.contract_multiplier = mult;
        o
    };
    assert_eq!(quote_currency_of(&cfd("XAU/USD", 5, "USD", 1.0)), Ok("USD".to_string()));
    assert_eq!(quote_currency_of(&cfd("GER30", 2, "EUR", 0.1)), Ok("EUR".to_string()));
    assert_eq!(quote_currency_of(&cfd("JPN225", 2, "JPY", 1.0)), Ok("JPY".to_string()));
    assert_eq!(quote_currency_of(&cfd("USOil", 3, "USD", 10.0)), Ok("USD".to_string()));
    assert_eq!(quote_currency_of(&cfd("BTC/EUR", 9, "EUR", 0.01)), Ok("EUR".to_string()));
    assert_eq!(quote_currency_of(&cfd("ETH/BTC", 9, "BTC", 0.1)), Ok("BTC".to_string()));
    assert_eq!(quote_currency_of(&cfd("NVDA.ext", 8, "USD", 0.1)), Ok("USD".to_string()));
    // a pair-shaped name that disagrees with the column is not trusted
    let why = quote_currency_of(&cfd("BTC/EUR", 9, "USD", 0.01)).unwrap_err();
    assert!(why.contains("disagree"), "{why}");
    // the column must be a currency code, the multiplier a positive number
    assert!(quote_currency_of(&cfd("GER30", 2, "", 0.1)).unwrap_err().contains("three-letter"));
    assert!(quote_currency_of(&cfd("GER30", 2, "EURO", 0.1)).is_err());
    for mult in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let why = quote_currency_of(&cfd("GER30", 2, "EUR", mult)).unwrap_err();
        assert!(why.contains("contract multiplier"), "{mult}: {why}");
    }
}

#[test]
fn a_forex_pair_keeps_its_old_rules() {
    let mut copper = offer("EUR/USD", 1.0, 1.0);
    copper.contract_multiplier = 25_000.0;
    assert!(quote_currency_of(&copper).unwrap_err().contains("multiplier"));

    let mut odd = offer("EUR/USD", 1.0, 1.0);
    odd.contract_currency = "GBP".to_string();
    assert!(quote_currency_of(&odd).unwrap_err().contains("disagree"));
    odd.contract_currency = String::new();
    assert!(quote_currency_of(&odd).is_err(), "no ContractCurrency, no cross-check, no trust");

    for name in ["EURUSD", "eur/usd", "EUR/USDT", "EUR/", "/USD", ""] {
        assert!(quote_currency_of(&offer(name, 1.0, 1.0)).is_err(), "{name:?} names no pair");
    }
}

/// The money of the ENGINE's mounted symbol is named by its spelling (no Offers row is read for it).
#[test]
fn a_mounted_symbols_money_is_the_second_half_of_its_spelling() {
    assert_eq!(quote_of_symbol("EURUSD").as_deref(), Some("USD"));
    assert_eq!(quote_of_symbol("eurusd").as_deref(), Some("USD"));
    assert_eq!(quote_of_symbol("USD/JPY").as_deref(), Some("JPY"));
    assert_eq!(quote_of_symbol("EURGBP").as_deref(), Some("GBP"));
    for not_a_pair in ["US30", "GER30EUR", "", "EUR", "EURUSDX", "EUR/USDT"] {
        assert_eq!(quote_of_symbol(not_a_pair), None, "{not_a_pair:?}");
    }
}

// ---- the rate and its orientation ----------------------------------------------------------------------

#[test]
fn account_per_quote_exists_only_for_a_finite_positive_number() {
    assert!(AccountPerQuote::new(0.0066).is_some());
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
    }
}

/// **The 22,500x bug.** USD account, a USD/JPY pair at 150.10: one yen is worth 1/150.10 of a dollar, and
/// 10,000 yen of gain on a closed 100,000-unit position (a 0.10 move) is 66.62 USD, never 10,000 and never
/// 1,501,000.
#[test]
fn a_yen_quoted_close_books_66_dollars_not_ten_thousand() {
    let offers = [offer("USD/JPY", 150.08, 150.12)];
    let r = quote_rate(&offers, "JPY", "USD", 7).expect("a rate");
    assert_eq!(r.quote_ccy, "JPY");
    assert_eq!(r.as_of_ms, 7);
    let per_yen = r.account_per_quote.get();
    assert!((per_yen - 1.0 / 150.10).abs() < 1e-15, "{per_yen}");
    assert!(
        per_yen < 0.01,
        "an account-per-quote-unit of a yen is a fraction of a dollar: {per_yen}"
    );

    let gain_usd = r.account_per_quote.account_amount(10_000.0);
    assert!((gain_usd - 66.622_251_832_111_9).abs() < 1e-9, "{gain_usd}");
    assert!(gain_usd > 66.0 && gain_usd < 67.0);
    // …and the two ways it goes wrong.
    assert!((10_000.0_f64 - gain_usd).abs() > 9_000.0, "par would book 10,000");
    assert!((10_000.0 * 150.10 - gain_usd).abs() > 1_000_000.0, "the flipped rate would book 1.5M");
}

/// A quote currency that IS the pair's base (GBP in GBP/USD on a USD account) takes the mid as it stands.
#[test]
fn a_pair_whose_base_is_the_quote_currency_is_taken_as_it_stands() {
    let offers = [offer("GBP/USD", 1.3498, 1.3502)];
    let r = quote_rate(&offers, "GBP", "USD", 7).expect("a rate");
    assert!((r.account_per_quote.get() - 1.35).abs() < 1e-12);
    // EUR/GBP: 85,000 GBP at 1.35 is 114,750 USD (the engine design's EURGBP golden).
    assert!((r.account_per_quote.account_amount(85_000.0) - 114_750.0).abs() < 1e-6);
}

/// A non-USD account: the same rule, with the account currency on either side of the pair.
#[test]
fn a_non_usd_account_is_converted_in_both_orientations() {
    // EUR account, USD-quoted (EUR/USD): the account currency is the pair's base, so EUR per USD = 1 / mid.
    let eurusd = [offer("EUR/USD", 1.0998, 1.1002)];
    assert!((rate(&eurusd, "USD", "EUR") - 1.0 / 1.10).abs() < 1e-12);
    // EUR account, yen-quoted: EUR/JPY, base = account.
    let eurjpy = [offer("EUR/JPY", 164.98, 165.02)];
    assert!((rate(&eurjpy, "JPY", "EUR") - 1.0 / 165.0).abs() < 1e-12);
    // GBP account, EUR-quoted: EUR/GBP, base = QUOTE currency, taken as it stands.
    let eurgbp = [offer("EUR/GBP", 0.8498, 0.8502)];
    assert!((rate(&eurgbp, "EUR", "GBP") - 0.85).abs() < 1e-12);
}

/// **Both directions of one pair multiply to one**, so neither orientation can drift from the other.
#[test]
fn the_two_directions_of_a_pair_are_reciprocal() {
    let offers = [offer("EUR/GBP", 0.8498, 0.8502)];
    let eur_in_gbp = rate(&offers, "EUR", "GBP");
    let gbp_in_eur = rate(&offers, "GBP", "EUR");
    assert!((eur_in_gbp * gbp_in_eur - 1.0).abs() < 1e-15);
    assert!(eur_in_gbp < 1.0 && gbp_in_eur > 1.0, "{eur_in_gbp} {gbp_in_eur}");
}

/// The rate is a MID (never the ask or bid alone), so a wide spread moves it by half the spread.
#[test]
fn the_rate_is_the_mid_of_a_two_sided_quote() {
    let wide = [offer("GBP/USD", 1.30, 1.40)];
    assert!((rate(&wide, "GBP", "USD") - 1.35).abs() < 1e-12);
    // A locked quote (bid == ask) is a quote.
    let locked = [offer("GBP/USD", 1.35, 1.35)];
    assert!((rate(&locked, "GBP", "USD") - 1.35).abs() < 1e-12);
}

// ---- no usable rate is no rate --------------------------------------------------------------------------

#[test]
fn a_missing_pair_is_refused_in_words() {
    let why = quote_rate(&[offer("EUR/USD", 1.1, 1.1)], "JPY", "USD", 7).unwrap_err();
    assert!(why.contains("lists no JPY/USD or USD/JPY pair"), "{why}");
    let why = quote_rate(&[], "JPY", "USD", 7).unwrap_err();
    assert!(why.contains("lists no"), "{why}");
}

#[test]
fn a_pair_that_is_not_subscribed_is_refused_not_priced() {
    for status in ["V", "D", ""] {
        let mut o = offer("USD/JPY", 150.0, 150.1);
        o.subscription = status.to_string();
        let why = quote_rate(&[o], "JPY", "USD", 7).unwrap_err();
        assert!(why.contains("not subscribed"), "{status:?}: {why}");
    }
}

/// Two candidate pairs (`Q/A` and `A/Q`, both subscribed) are ambiguous and NEITHER is chosen.
#[test]
fn two_candidate_pairs_are_ambiguous() {
    let both = [offer("USD/JPY", 150.0, 150.1), offer("JPY/USD", 0.00666, 0.00667)];
    let why = quote_rate(&both, "JPY", "USD", 7).unwrap_err();
    assert!(why.contains("ambiguous"), "{why}");
    // An UNSUBSCRIBED twin is not a candidate.
    let mut twin = offer("JPY/USD", 0.00666, 0.00667);
    twin.subscription = "V".to_string();
    let one = [offer("USD/JPY", 150.0, 150.1), twin];
    assert!(quote_rate(&one, "JPY", "USD", 7).is_ok());
}

/// A pair whose class is not forex is never a conversion pair, whatever its name.
#[test]
fn only_a_forex_instrument_can_be_a_conversion_pair() {
    let mut index = offer("USD/JPY", 150.0, 150.1);
    index.instrument_type = 2;
    let why = quote_rate(&[index], "JPY", "USD", 7).unwrap_err();
    assert!(why.contains("lists no"), "{why}");
}

#[test]
fn a_quote_that_is_not_live_two_sided_and_uncrossed_is_refused() {
    let case = |tweak: &dyn Fn(&mut Offer)| {
        let mut o = offer("USD/JPY", 150.0, 150.1);
        tweak(&mut o);
        quote_rate(&[o], "JPY", "USD", 7).unwrap_err()
    };
    assert!(case(&|o| o.trading_status = "C".to_string()).contains("not open"));
    assert!(case(&|o| o.bid_tradable = "N".to_string()).contains("not available for trading"));
    assert!(case(&|o| o.ask_tradable = "N".to_string()).contains("not available for trading"));
    // an INDICATIVE side is not tradable either, and neither is a flag nobody set or nobody knows
    for flag in ["I", "", "X", "t"] {
        assert!(
            case(&|o| o.bid_tradable = flag.to_string()).contains("not available for trading"),
            "bid flag {flag:?}"
        );
        assert!(
            case(&|o| o.ask_tradable = flag.to_string()).contains("only `T` is tradable"),
            "ask flag {flag:?}"
        );
    }
    assert!(case(&|o| o.bid = 0.0).contains("two-sided positive"));
    assert!(case(&|o| o.ask = 0.0).contains("two-sided positive"));
    assert!(case(&|o| o.bid = -150.0).contains("two-sided positive"));
    assert!(case(&|o| o.ask = f64::NAN).contains("two-sided positive"));
    assert!(case(&|o| o.bid = f64::INFINITY).contains("two-sided positive"));
    assert!(
        case(&|o| {
            o.bid = 150.2;
            o.ask = 150.1;
        })
        .contains("crossed")
    );
}

// ---- the quote's own time ------------------------------------------------------------------------------

/// **The stamp is the QUOTE's moment, not the ask's** (finding 4 of the second review). A quote 30 s old at the
/// read is stamped 30 s before the ask, so the node's 120 s staleness check measures the real age; one that is
/// frozen past the limit while the status still says `O` is refused, as is one with no time or one that reads
/// hours ahead of the server (the two times are then not in one frame, and no age is trusted).
#[test]
fn a_rate_is_stamped_with_its_quotes_own_time_and_a_stale_or_untimed_quote_is_refused() {
    let aged = |age: Option<i64>| {
        let mut o = offer("USD/JPY", 150.0, 150.1);
        o.quote_age_ms = age;
        quote_rate(&[o], "JPY", "USD", 1_000_000)
    };
    assert_eq!(aged(Some(0)).unwrap().as_of_ms, 1_000_000);
    assert_eq!(aged(Some(30_000)).unwrap().as_of_ms, 970_000, "stamped at the quote, not the ask");
    assert_eq!(aged(Some(MAX_QUOTE_AGE_MS)).unwrap().as_of_ms, 1_000_000 - MAX_QUOTE_AGE_MS);
    assert_eq!(
        aged(Some(-3_000)).unwrap().as_of_ms,
        1_000_000,
        "jitter ahead is no older than now"
    );

    let frozen = aged(Some(MAX_QUOTE_AGE_MS + 1_000)).unwrap_err();
    assert!(frozen.contains("121 s old") && frozen.contains("limit is 120 s"), "{frozen}");
    let friday = aged(Some(2 * 24 * 3_600_000)).unwrap_err();
    assert!(friday.contains("172800 s old"), "the weekend gap is refused: {friday}");
    let untimed = aged(None).unwrap_err();
    assert!(untimed.contains("no time of its own"), "{untimed}");
    let framed = aged(Some(-5 * 3_600_000)).unwrap_err();
    assert!(framed.contains("18000 s ahead") && framed.contains("time frame"), "{framed}");
}

/// The refusal reaches the facts as the RATE's failure (the node forgets it and asks again), while the symbol's
/// own grid needs no time at all: a quote with no time costs the conversion, never the instrument.
#[test]
fn a_stale_conversion_quote_is_a_rate_failure_and_an_untimed_symbol_still_has_its_grid() {
    let mut offers = usd_offers();
    for o in &mut offers {
        o.quote_age_ms = Some(MAX_QUOTE_AGE_MS + 1);
    }
    let err = facts_from_offers(&offers, "EURGBP", &venue("USD"), 1).unwrap_err();
    assert!(err.is_rate(), "{err}");

    for o in &mut offers {
        o.quote_age_ms = None;
    }
    let at_par = facts_from_offers(&offers, "EURUSD", &venue("USD"), 1).expect("quoted in USD");
    assert!(at_par.quote.is_none() && at_par.properties.step_size > 0.0);
}

// ---- a symbol's facts ---------------------------------------------------------------------------------

fn usd_offers() -> Vec<Offer> {
    let mut usdjpy = offer("USD/JPY", 150.08, 150.12);
    usdjpy.digits = 3;
    vec![
        offer("EUR/USD", 1.0998, 1.1002),
        usdjpy,
        offer("GBP/USD", 1.3498, 1.3502),
        offer("EUR/GBP", 0.8148, 0.8152),
        offer("EUR/JPY", 165.0, 165.1),
    ]
}

#[test]
fn a_symbol_quoted_in_the_accounts_currency_converts_nothing() {
    let facts = facts_from_offers(&usd_offers(), "EURUSD", &venue("USD"), 7).expect("facts");
    assert_eq!(facts.quote, None, "quoted in the account's own money: nothing converts");
    assert!((facts.properties.tick_size - 1e-5).abs() < 1e-15);
    assert_eq!(facts.properties.step_size, 1000.0);
    assert_eq!(facts.properties.min_qty, 1000.0, "one lot");
}

/// The answer carries the ledger currency it is expressed in (review 2: the gatewayhub stamps it on the wire so the
/// node can refuse a rate in a currency its engine was not anchored to), converted or at par, assumed or stated.
#[test]
fn the_facts_carry_the_ledger_currency_they_are_expressed_in() {
    for (symbol, code) in [("EURUSD", "USD"), ("USD/JPY", "USD"), ("EURGBP", "USD")] {
        let facts = facts_from_offers(&usd_offers(), symbol, &venue(code), 7).expect("facts");
        assert_eq!(facts.account_ccy, code, "{symbol}");
    }
    let assumed = account_ccy(PropertiesRead::Unreadable("old shim".to_string())).unwrap();
    let facts = facts_from_offers(&usd_offers(), "USDJPY", &assumed, 9).expect("facts");
    assert_eq!(facts.account_ccy, "USD");
}

/// The JPY pair is on its own price grid (three decimals), and is converted through ITS OWN pair.
#[test]
fn a_yen_pair_has_its_own_grid_and_a_rate() {
    let facts = facts_from_offers(&usd_offers(), "USD/JPY", &venue("USD"), 7).expect("facts");
    assert!((facts.properties.tick_size - 0.001).abs() < 1e-15);
    let quote = facts.quote.expect("a rate for a yen quote");
    assert_eq!(quote.quote_ccy, "JPY");
    assert!((quote.account_per_quote.get() - 1.0 / 150.10).abs() < 1e-15);
    assert_eq!(quote.as_of_ms, 7);
}

/// A cross: EUR/GBP on a USD account is converted through GBP/USD, a pair the symbol itself is not.
#[test]
fn a_cross_is_converted_through_the_pair_of_its_quote_currency() {
    let facts = facts_from_offers(&usd_offers(), "EURGBP", &venue("USD"), 7).expect("facts");
    let quote = facts.quote.expect("a rate");
    assert_eq!(quote.quote_ccy, "GBP");
    assert!((quote.account_per_quote.get() - 1.35).abs() < 1e-12);
}

/// **An ASSUMED-USD account still converts a non-USD-quoted pair through the conversion pair**, with the
/// orientation intact: the assumption changes how the currency is KNOWN, not how a rate is derived.
#[test]
fn an_assumed_usd_account_still_converts_through_the_pair_rate() {
    let assumed = account_ccy(PropertiesRead::Unreadable("old shim".to_string())).unwrap();
    assert!(assumed.is_assumed());
    let facts = facts_from_offers(&usd_offers(), "USDJPY", &assumed, 9).expect("facts");
    let quote = facts.quote.expect("a rate");
    assert_eq!(quote.quote_ccy, "JPY");
    assert!((quote.account_per_quote.get() - 1.0 / 150.10).abs() < 1e-15, "never 150.10");
    let gbp = facts_from_offers(&usd_offers(), "EURGBP", &assumed, 9).unwrap().quote.unwrap();
    assert!((gbp.account_per_quote.get() - 1.35).abs() < 1e-12, "never 1/1.35");
}

#[test]
fn a_symbol_the_venue_does_not_offer_or_cannot_describe_is_refused() {
    let account = venue("USD");
    // (the words, whether the failure is the RATE's: a verdict on the symbol is remembered by the node, a
    // rate that is unavailable now must not be)
    let why = |symbol: &str, offers: &[Offer]| {
        let e = facts_from_offers(offers, symbol, &account, 7).expect_err(symbol);
        (e.to_string(), e.is_rate())
    };
    let symbol_verdict = |(words, is_rate): (String, bool), needle: &str| {
        assert!(words.contains(needle), "{words}");
        assert!(!is_rate, "a verdict on the symbol, not on the rate: {words}");
    };
    symbol_verdict(why("EURCHF", &usd_offers()), "not in the venue's Offers table");

    let mut unsubscribed = usd_offers();
    unsubscribed[0].subscription = "V".to_string();
    symbol_verdict(why("EURUSD", &unsubscribed), "not subscribed");

    let mut unnamed = usd_offers();
    unnamed.push({
        let mut odd = offer("ZZZ/USD", 2000.0, 2000.5);
        odd.instrument_type = 42;
        odd
    });
    symbol_verdict(why("ZZZUSD", &unnamed), "not determinable");

    let mut no_lot = usd_offers();
    no_lot[0].base_unit = 0;
    symbol_verdict(why("EURUSD", &no_lot), "no base unit size");

    let mut bad_digits = usd_offers();
    bad_digits[0].digits = 9;
    symbol_verdict(why("EURUSD", &bad_digits), "price precision");

    // The instrument is fine but the conversion pair is not: no rate, no facts, and the failure is the RATE's.
    let mut closed = usd_offers();
    closed[1].trading_status = "C".to_string();
    let (words, is_rate) = why("USDJPY", &closed);
    assert!(words.contains("not open") && is_rate, "{words}");
    // A yen cross with no USD/JPY (or JPY/USD) in the table has nothing to convert through.
    let eurjpy_alone = [usd_offers().remove(4)];
    let (words, is_rate) = why("EURJPY", &eurjpy_alone);
    assert!(words.contains("lists no JPY/USD or USD/JPY pair") && is_rate, "{words}");
}

// ---- a quote currency the ledger has no pair with: composed through USD ---------------------------------------

/// The 48 regular forex rows of the EUR demo login's Offers table as read on 2026-10-26 (measured, not docs-shaped):
/// FXCM lists no pair of EUR with HKD, ZAR, MXN or CNH, which was the door's "lists no HKD/EUR or EUR/HKD pair".
const EUR_DEMO: &str = include_str!("../tests/fixtures/eur_demo_offers.json");

fn eur_demo() -> Vec<Offer> {
    let fixture: serde_json::Value = serde_json::from_str(EUR_DEMO).expect("the fixture is JSON");
    parse_offers(&fixture["offers"].to_string()).expect("the fixture's rows are the shim's shape")
}

fn mid(offers: &[Offer], name: &str) -> f64 {
    let o = offers.iter().find(|o| o.instrument == name).expect(name);
    (o.bid + o.ask) / 2.0
}

fn tweak(offers: &mut [Offer], name: &str, f: impl FnOnce(&mut Offer)) {
    f(offers.iter_mut().find(|o| o.instrument == name).expect(name));
}

/// **The measured defect (v0.1.93, EUR demo): USDHKD, USDZAR, USDMXN and USDCNH were refused.** The rate of one
/// unit of the quote currency in euro is made of the two legs through USD: `1 / mid(USD/Q)` (dollars per quote
/// unit) times `1 / mid(EUR/USD)` (euro per dollar).
#[test]
fn a_quote_currency_with_no_pair_of_the_ledger_is_composed_through_usd() {
    let offers = eur_demo();
    assert_eq!(offers.len(), 48);
    let eur_per_usd = 1.0 / mid(&offers, "EUR/USD");
    for (symbol, usd_pair, quote_ccy) in [
        ("USDHKD", "USD/HKD", "HKD"),
        ("USDZAR", "USD/ZAR", "ZAR"),
        ("USDMXN", "USD/MXN", "MXN"),
        ("USDCNH", "USD/CNH", "CNH"),
    ] {
        let facts = facts_from_offers(&offers, symbol, &venue("EUR"), 1_000_000)
            .unwrap_or_else(|e| panic!("{symbol}: {e}"));
        let quote = facts.quote.expect("quoted in another currency than the ledger's");
        assert_eq!(quote.quote_ccy, quote_ccy);
        assert_eq!(facts.account_ccy, "EUR");
        let expected = (1.0 / mid(&offers, usd_pair)) * eur_per_usd;
        assert!(
            (quote.account_per_quote.get() - expected).abs() < 1e-15,
            "{symbol}: {} against {expected}",
            quote.account_per_quote.get()
        );
        assert_eq!(quote.as_of_ms, 1_000_000 - 150, "stamped at the quote, as for one pair");
        assert!((facts.properties.tick_size - 1e-5).abs() < 1e-15);
        assert_eq!(facts.properties.step_size, 1000.0);
    }
    // In numbers: USD/HKD 7.84425/7.84477 (mid 7.84451) and EUR/USD 1.11799/1.11808 (mid 1.118035) make one Hong Kong
    // dollar worth 0.114019407 euro (1 EUR = 8.7704 HKD): a gain of 10,000 HKD books 1,140.19 EUR.
    let hkd = quote_rate(&offers, "HKD", "EUR", 7).expect("a rate");
    assert!((hkd.account_per_quote.get() - 0.114_019_407_458).abs() < 1e-12);
    assert!((hkd.account_per_quote.account_amount(10_000.0) - 1_140.194_074_58).abs() < 1e-7);
}

/// **The orientation, checked against the venue's own direct pair**: where FXCM lists a direct EUR cross, the same
/// rate composed through USD (that row taken out of the table) agrees with it to the spread, so neither leg is
/// inverted the wrong way (a flipped leg is off by a factor of the dollar's rate, 1.12 squared or 157).
#[test]
fn the_composed_rate_agrees_with_the_listed_direct_cross_to_the_spread() {
    let mut offers = eur_demo();
    tweak(&mut offers, "USD/HUF", |o| o.subscription = "T".to_string());
    for (quote, direct_pair) in
        [("JPY", "EUR/JPY"), ("TRY", "EUR/TRY"), ("HUF", "EUR/HUF"), ("CHF", "EUR/CHF")]
    {
        let direct =
            quote_rate(&offers, quote, "EUR", 7).expect(direct_pair).account_per_quote.get();
        let without: Vec<Offer> =
            offers.iter().filter(|o| o.instrument != direct_pair).cloned().collect();
        let composed = quote_rate(&without, quote, "EUR", 7).expect(quote).account_per_quote.get();
        assert!(
            (composed / direct - 1.0).abs() < 0.002,
            "{quote}: composed {composed} against {direct_pair} {direct}"
        );
    }
}

/// The rate is as old as its OLDEST leg (the engine judges staleness on the stamp), and each leg is held to the 120 s
/// window by itself.
#[test]
fn a_composed_rate_is_stamped_with_its_oldest_leg_and_each_leg_must_be_fresh() {
    let aged = |hkd_leg: i64, eur_leg: i64| {
        let mut offers = eur_demo();
        tweak(&mut offers, "USD/HKD", |o| o.quote_age_ms = Some(hkd_leg));
        tweak(&mut offers, "EUR/USD", |o| o.quote_age_ms = Some(eur_leg));
        quote_rate(&offers, "HKD", "EUR", 1_000_000)
    };
    assert_eq!(aged(0, 0).unwrap().as_of_ms, 1_000_000);
    assert_eq!(aged(30_000, 90_000).unwrap().as_of_ms, 910_000, "the euro leg is older");
    assert_eq!(aged(90_000, 30_000).unwrap().as_of_ms, 910_000, "the dirham leg is older");
    assert_eq!(aged(MAX_QUOTE_AGE_MS, 0).unwrap().as_of_ms, 1_000_000 - MAX_QUOTE_AGE_MS);

    let hkd_stale = aged(MAX_QUOTE_AGE_MS + 1_000, 0).unwrap_err();
    assert!(
        hkd_stale.contains("HKD to USD leg") && hkd_stale.contains("`USD/HKD` is 121 s old"),
        "{hkd_stale}"
    );
    let eur_stale = aged(0, MAX_QUOTE_AGE_MS + 1_000).unwrap_err();
    assert!(
        eur_stale.contains("USD to EUR leg") && eur_stale.contains("`EUR/USD` is 121 s old"),
        "{eur_stale}"
    );
    let untimed = {
        let mut offers = eur_demo();
        tweak(&mut offers, "EUR/USD", |o| o.quote_age_ms = None);
        quote_rate(&offers, "HKD", "EUR", 7).unwrap_err()
    };
    assert!(
        untimed.contains("USD to EUR leg") && untimed.contains("no time of its own"),
        "{untimed}"
    );
}

/// A leg that is not subscribed, not live or not listed refuses the conversion with the LEG named, and the refusal
/// reaches the facts as the RATE's failure (the node forgets it), never as a verdict on the symbol.
#[test]
fn a_leg_that_is_not_a_live_subscribed_listed_quote_is_refused_naming_the_leg() {
    let refused = |name: &str, f: &dyn Fn(&mut Offer)| {
        let mut offers = eur_demo();
        tweak(&mut offers, name, f);
        quote_rate(&offers, "HKD", "EUR", 7).unwrap_err()
    };
    let why = refused("EUR/USD", &|o| o.subscription = "V".to_string());
    assert!(why.contains("USD to EUR leg") && why.contains("not subscribed"), "{why}");
    assert!(why.contains("EUR/USD (subscription `V`)"), "{why}");
    let why = refused("USD/HKD", &|o| o.trading_status = "C".to_string());
    assert!(why.contains("HKD to USD leg") && why.contains("not open"), "{why}");
    let why = refused("USD/HKD", &|o| o.ask_tradable = "I".to_string());
    assert!(why.contains("HKD to USD leg") && why.contains("not available for trading"), "{why}");
    let why = refused("EUR/USD", &|o| o.bid = 0.0);
    assert!(why.contains("USD to EUR leg") && why.contains("two-sided positive"), "{why}");

    let mut no_eurusd = eur_demo();
    no_eurusd.retain(|o| o.instrument != "EUR/USD");
    let why = quote_rate(&no_eurusd, "HKD", "EUR", 7).unwrap_err();
    assert!(
        why.contains("USD to EUR leg") && why.contains("lists no USD/EUR or EUR/USD pair"),
        "{why}"
    );
    let why = facts_from_offers(&no_eurusd, "USDZAR", &venue("EUR"), 7).unwrap_err();
    assert!(why.is_rate() && why.message().contains("USD to EUR leg"), "{why}");

    let mut no_hkd = eur_demo();
    no_hkd.retain(|o| o.instrument != "USD/HKD");
    let why = quote_rate(&no_hkd, "HKD", "EUR", 7).unwrap_err();
    assert!(
        why.contains("HKD to USD leg") && why.contains("lists no HKD/USD or USD/HKD pair"),
        "{why}"
    );

    // A ledger FXCM lists no USD pair for cannot be reached at all, and the words say which leg.
    let why = quote_rate(&eur_demo(), "JPY", "XTS", 7).unwrap_err();
    assert!(
        why.contains("USD to XTS leg") && why.contains("lists no USD/XTS or XTS/USD pair"),
        "{why}"
    );
}

/// **Where a direct pair is listed nothing changes**: the same rate to the last bit and the same stamp, a listed
/// but unsubscribed direct pair stays a refusal (the bridge subscribes it; it is never bypassed through USD), and a
/// USD ledger is never composed.
#[test]
fn a_listed_direct_pair_is_used_as_before_and_never_bypassed_through_usd() {
    let offers = eur_demo();
    for (quote, pair) in [
        ("USD", "EUR/USD"),
        ("JPY", "EUR/JPY"),
        ("GBP", "EUR/GBP"),
        ("CHF", "EUR/CHF"),
        ("AUD", "EUR/AUD"),
        ("CAD", "EUR/CAD"),
        ("TRY", "EUR/TRY"),
        ("HUF", "EUR/HUF"),
    ] {
        let got = quote_rate(&offers, quote, "EUR", 1_000_000).expect(quote);
        assert_eq!(
            got.account_per_quote.get(),
            1.0 / mid(&offers, pair),
            "{quote}: one pair, as before"
        );
        assert_eq!(got.as_of_ms, 1_000_000 - 150, "{quote}");
    }
    // EUR/NZD is listed and disabled: that is a subscription's to fix, not a reason to compose.
    let why = quote_rate(&offers, "NZD", "EUR", 7).unwrap_err();
    assert!(why.contains("not subscribed") && why.contains("EUR/NZD"), "{why}");
    assert!(!why.contains("through USD"), "{why}");
    // Both orientations listed and subscribed stays ambiguous.
    let mut both = eur_demo();
    both.push(offer("GBP/EUR", 1.19, 1.1902));
    assert!(quote_rate(&both, "GBP", "EUR", 7).unwrap_err().contains("ambiguous"));
    // A USD ledger: the one pair of the quote currency and USD, or the old refusal, never a composition.
    assert_eq!(rate(&offers, "JPY", "USD"), 1.0 / mid(&offers, "USD/JPY"));
    assert_eq!(rate(&offers, "GBP", "USD"), mid(&offers, "GBP/USD"));
    let mut no_hkd = eur_demo();
    no_hkd.retain(|o| o.instrument != "USD/HKD");
    let why = quote_rate(&no_hkd, "HKD", "USD", 7).unwrap_err();
    assert!(
        why.contains("lists no HKD/USD or USD/HKD pair") && !why.contains("through USD"),
        "{why}"
    );
}

/// Any ledger, not only the euro: a pound ledger reaches SEK (no GBP/SEK) through USD and JPY directly; a yen ledger
/// reaches HUF (no JPY/HUF) through USD and EUR directly.
#[test]
fn a_ledger_that_is_not_the_euro_composes_the_same_way() {
    let mut offers = eur_demo();
    tweak(&mut offers, "USD/HUF", |o| o.subscription = "T".to_string());
    // GBP: SEK -> USD (1 / mid USD/SEK), USD -> GBP (1 / mid GBP/USD)
    let sek = rate(&offers, "SEK", "GBP");
    assert!(
        (sek - (1.0 / mid(&offers, "USD/SEK")) * (1.0 / mid(&offers, "GBP/USD"))).abs() < 1e-15
    );
    assert_eq!(
        rate(&offers, "JPY", "GBP"),
        1.0 / mid(&offers, "GBP/JPY"),
        "GBP/JPY is listed: direct"
    );
    // JPY: HUF -> USD (1 / mid USD/HUF), USD -> JPY (mid USD/JPY: the ledger is the pair's QUOTE side)
    let huf = rate(&offers, "HUF", "JPY");
    assert!((huf - (1.0 / mid(&offers, "USD/HUF")) * mid(&offers, "USD/JPY")).abs() < 1e-12);
    assert_eq!(rate(&offers, "EUR", "JPY"), mid(&offers, "EUR/JPY"), "EUR/JPY is listed: direct");
    // and the whole facts path, on a pound ledger
    let facts = facts_from_offers(&offers, "USDSEK", &venue("GBP"), 1_000).expect("USDSEK on GBP");
    assert_eq!(facts.quote.expect("SEK").quote_ccy, "SEK");
    assert_eq!(facts.account_ccy, "GBP");
}

// ---- the classes: metals, indices, commodities, treasuries, FX indices, share and crypto CFDs -----------------

/// **EVERY non-forex row** of the EUR demo's Offers table (449 of 497), MEASURED 2026-10-30 19:56 UTC through the live
/// smoke `tests/fxcm_classes_smoke.rs`: the fixture is the shim's `fc_offers` rows with the terms keys.
const CLASSES: &str = include_str!("../tests/fixtures/offers_classes.json");

fn classes() -> Vec<Offer> {
    let fixture: serde_json::Value = serde_json::from_str(CLASSES).expect("the fixture is JSON");
    let rows = fixture["offers"].as_array().expect("an array").len();
    let parsed = parse_offers(&fixture["offers"].to_string()).expect("the shim's shape");
    assert_eq!(parsed.len(), rows, "a captured row was skipped by the parser");
    parsed
}

/// The 48 forex rows (the conversion pairs) beside the 449 class rows: the table a EUR account's facts are judged on.
fn whole_table() -> Vec<Offer> {
    let mut table = eur_demo();
    table.extend(classes());
    table
}

fn tradable(offers: &mut [Offer], name: &str) {
    tweak(offers, name, |o| o.subscription = "T".to_string());
}

#[test]
fn every_measured_class_row_has_a_class_a_quote_currency_and_terms() {
    let rows = classes();
    assert_eq!(rows.len(), 449);
    let mut seen = std::collections::BTreeMap::new();
    for o in &rows {
        let class =
            Class::of(o.instrument_type).unwrap_or_else(|| panic!("{}: type", o.instrument));
        assert_ne!(class, Class::Forex, "{}: a forex row in the class fixture", o.instrument);
        *seen.entry(o.instrument_type).or_insert(0) += 1;
        assert_eq!(
            quote_currency_of(o).unwrap_or_else(|why| panic!("{}: {why}", o.instrument)),
            o.contract_currency,
            "{}: a CFD is quoted in its ContractCurrency",
            o.instrument
        );
        let terms = o.terms.as_ref().unwrap_or_else(|| panic!("{}: no terms", o.instrument));
        assert!(
            terms.min_qty >= 1 && terms.max_qty >= terms.min_qty,
            "{}: {terms:?}",
            o.instrument
        );
        assert_eq!(terms.min_qty % o.base_unit, 0, "{}: the minimum is whole lots", o.instrument);
        assert_eq!(terms.margin_per_lot, None, "{}: the demo states no margin", o.instrument);
        assert!((0..=8).contains(&o.digits) && o.base_unit >= 1, "{}", o.instrument);
    }
    // 14 indices, 19 commodities, 10 treasuries, 2 bullion, 3 FX indices, 374 share CFDs, 27 crypto CFDs
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        [(2, 14), (3, 19), (4, 10), (5, 2), (7, 3), (8, 374), (9, 27)]
    );
    // no two rows share an engine spelling: a symbol names one row
    let mut names: Vec<String> =
        rows.iter().map(|o| crate::event_mapper::from_fxcm_instrument(&o.instrument)).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), rows.len());
}

/// **The engine's spelling finds the venue's row**: no slash, upper case (`USOIL` is `USOil`, `XAUUSD` is `XAU/USD`,
/// `NVDA.EXT` is `NVDA.ext`, `COPPER` is `Copper` though six letters would be slashed by the pair rule).
#[test]
fn the_engines_spelling_finds_the_venues_row() {
    let table = whole_table();
    let named = |symbol: &str| row_for(&table, symbol).unwrap().map(|o| o.instrument.as_str());
    assert_eq!(named("USOIL"), Some("USOil"));
    assert_eq!(named("usoil"), Some("USOil"));
    assert_eq!(named("XAUUSD"), Some("XAU/USD"));
    assert_eq!(named("xau/usd"), Some("XAU/USD"));
    assert_eq!(named("NVDA.EXT"), Some("NVDA.ext"));
    assert_eq!(named("COPPER"), Some("Copper"));
    assert_eq!(named("COP/PER"), Some("Copper"));
    assert_eq!(named("10USNOTE"), Some("10USNote"));
    assert_eq!(named("EURUSD"), Some("EUR/USD"));
    assert_eq!(named("NOTALISTING"), None);
    // two rows with one engine spelling are never guessed between
    let both = [offer("EUR/USD", 1.0, 1.0), offer("EURUSD", 1.0, 1.0)];
    assert!(row_for(&both, "EURUSD").unwrap_err().contains("more than one row"));
    let why = facts_from_offers(&both, "EURUSD", &venue("USD"), 1).unwrap_err();
    assert!(!why.is_rate() && why.to_string().contains("more than one row"), "{why}");
}

/// **The facts of a metal, an index, a commodity, a treasury, a crypto and a share CFD on the EUR account**, read off
/// the measured table: the grid is `10^-digits`, the lot the base unit, the order bounds the venue's, the money the
/// row's ContractCurrency converted to euro by the table's own pairs, and the multiplier the contract value.
#[test]
fn the_facts_of_each_class_are_the_rows_own_numbers() {
    let eur = venue("EUR");
    let eur_per_usd = 1.0 / mid(&eur_demo(), "EUR/USD");
    let mut table = whole_table();
    for name in
        ["USOil", "Copper", "GasolineF", "XRP/USD", "NVDA.ext", "Bund", "BTC/JPY", "ETH/BTC"]
    {
        tradable(&mut table, name);
    }
    let facts = |symbol: &str| facts_from_offers(&table, symbol, &eur, 1_000).expect(symbol);

    // a metal (type 5): a lot of 1, no multiplier, quoted in dollars
    let gold = facts("XAUUSD");
    assert_eq!(
        (gold.properties.tick_size, gold.properties.step_size, gold.properties.min_qty),
        (0.01, 1.0, 1.0)
    );
    assert_eq!(gold.properties.max_qty, 50_000_000.0);
    assert_eq!(gold.contract_value, None, "a multiplier of 1 is the default");
    let usd = gold.quote.expect("dollars on a euro account");
    assert_eq!(usd.quote_ccy, "USD");
    assert!((usd.account_per_quote.get() - eur_per_usd).abs() < 1e-15, "euro per ONE dollar");
    assert_eq!(gold.account_ccy, "EUR");
    assert_eq!(facts("XAGUSD").properties.tick_size, 0.001);

    // an index quoted in the ledger's own money: par, no rate; one in dollars: a rate. Both carry the multiplier.
    let dax = facts("GER30");
    assert_eq!(dax.quote, None, "EUR in a EUR ledger");
    assert_eq!(dax.contract_value, Some(ContractValue::linear(0.1)));
    assert_eq!((dax.properties.step_size, dax.properties.max_qty), (1.0, 1_000.0));
    let dow = facts("US30");
    assert_eq!(dow.quote.expect("USD").quote_ccy, "USD");
    assert_eq!(dow.contract_value, Some(ContractValue::linear(0.1)));
    assert_eq!(dow.properties.max_qty, 4_000.0);
    // a commodity, six letters (`Copper`), a lot of 25 and one of 1
    assert_eq!(facts("COPPER").contract_value, Some(ContractValue::linear(100.0)));
    assert_eq!(facts("COPPER").properties.tick_size, 0.0001);
    let gasoline = facts("GASOLINEF");
    assert_eq!((gasoline.properties.step_size, gasoline.properties.min_qty), (25.0, 25.0));
    assert_eq!(facts("USOIL").contract_value, Some(ContractValue::linear(10.0)));
    // a treasury, a crypto with a lot of 100, a crypto in yen (digits 0: a tick of 1), a share CFD
    assert_eq!(facts("BUND").quote, None, "EUR");
    let xrp = facts("XRPUSD");
    assert_eq!(
        (xrp.properties.step_size, xrp.properties.min_qty, xrp.properties.max_qty),
        (100.0, 100.0, 10_000.0)
    );
    let btcjpy = facts("BTCJPY");
    assert_eq!(btcjpy.properties.tick_size, 1.0);
    assert_eq!(btcjpy.quote.expect("JPY").quote_ccy, "JPY");
    assert_eq!(btcjpy.contract_value, Some(ContractValue::linear(0.01)));
    let nvda = facts("NVDA.EXT");
    assert_eq!(nvda.contract_value, Some(ContractValue::linear(0.1)));
    assert_eq!(nvda.quote.expect("USD").quote_ccy, "USD");
}

/// A quote currency FXCM lists no forex pair for (the BTC of `ETH/BTC`) has no conversion: refused by name as a verdict
/// on the symbol; and a closed market is not a refusal of the FACTS (the row keeps its terms, the order is the venue's).
#[test]
fn a_crypto_quote_has_no_conversion_and_a_closed_market_still_has_facts() {
    let mut table = whole_table();
    tradable(&mut table, "ETH/BTC");
    let why = facts_from_offers(&table, "ETHBTC", &venue("EUR"), 1_000).unwrap_err();
    assert!(!why.is_rate(), "a verdict, the node remembers it: {why}");
    assert!(
        why.to_string().contains("quoted in BTC") && why.to_string().contains("no forex pair"),
        "{why}"
    );

    tradable(&mut table, "HKG33");
    let hkg = table.iter().find(|o| o.instrument == "HKG33").unwrap();
    assert_eq!(hkg.trading_status, "C", "closed at the measurement");
    let facts =
        facts_from_offers(&table, "HKG33", &venue("EUR"), 1_000).expect("closed, still admitted");
    assert_eq!(facts.quote.as_ref().expect("HKD").quote_ccy, "HKD");
    assert_eq!(facts.properties.max_qty, 300.0);
}

/// A shim older than the terms sends none: a metal, an index or a CFD is not admitted without its minimum and maximum
/// (a failure of the SHIM, not a verdict on the symbol: the node forgets it), while a forex pair is as it always was.
#[test]
fn a_cfd_class_row_without_terms_is_not_admitted_and_a_forex_pair_is() {
    let mut table = whole_table();
    tradable(&mut table, "US30");
    tweak(&mut table, "US30", |o| o.terms = None);
    let why = facts_from_offers(&table, "US30", &venue("EUR"), 1_000).unwrap_err();
    assert!(why.is_rate(), "{why}");
    assert!(
        why.to_string().contains("sends no trading terms")
            && why.to_string().contains("an index CFD"),
        "{why}"
    );
    let eurusd =
        facts_from_offers(&table, "EURUSD", &venue("EUR"), 1_000).expect("forex needs none");
    assert_eq!((eurusd.properties.min_qty, eurusd.properties.max_qty), (1000.0, 0.0));
    assert_eq!((eurusd.contract_value, eurusd.margin_rate), (None, None));
}

/// A forex pair that carries terms takes the venue's own maximum (GBP/NZD reads 200000, not the 50 million of the majors).
#[test]
fn a_forex_pair_with_terms_takes_the_venues_maximum() {
    let mut table = eur_demo();
    tradable(&mut table, "EUR/NZD"); // the pair NZD converts through
    tweak(&mut table, "GBP/NZD", |o| {
        o.subscription = "T".to_string();
        o.terms = Some(Terms { min_qty: 1000, max_qty: 200_000, margin_per_lot: Some(5.0) });
    });
    let facts = facts_from_offers(&table, "GBPNZD", &venue("EUR"), 1_000).expect("GBPNZD");
    assert_eq!((facts.properties.min_qty, facts.properties.max_qty), (1000.0, 200_000.0));
    assert_eq!(facts.margin_rate, None, "a forex pair keeps the engine's margin rate");
}

/// **The venue's margin becomes the symbol's margin rate**, in the account's currency, over the lot's value: US30 at the
/// measured quote is worth `mid * 1 * 0.1 * (euro per dollar)` euro a lot, and a margin of a twentieth of that is 5 percent.
#[test]
fn the_margin_the_venue_states_becomes_a_rate_of_the_lots_value() {
    let mut table = whole_table();
    tradable(&mut table, "US30");
    let us30 = table.iter().find(|o| o.instrument == "US30").unwrap().clone();
    let mid_px = (us30.bid + us30.ask) / 2.0;
    let eur_per_usd = 1.0 / mid(&table, "EUR/USD");
    let lot_value = mid_px * 1.0 * 0.1 * eur_per_usd;
    let terms = |margin: Option<f64>| Terms { min_qty: 1, max_qty: 4000, margin_per_lot: margin };
    let mut rate = |margin: Option<f64>| {
        tweak(&mut table, "US30", |o| o.terms = Some(terms(margin)));
        facts_from_offers(&table, "US30", &venue("EUR"), 1_000).unwrap().margin_rate
    };
    let five = rate(Some(lot_value / 20.0)).expect("a rate");
    assert!((five - 0.05).abs() < 1e-12, "{five}");
    assert_eq!(rate(None), None, "the demo states none: the engine keeps its own");
    assert_eq!(rate(Some(lot_value * 2.0)), None, "more than the whole value is no rate");
    assert_eq!(rate(Some(0.0)), None);
}

/// The terms half of a shim row: sizes are base units, and the margin is the entry margin of a three-level account, else
/// the minimum margin requirement; a figure the SDK could not give (-1) is none; a row without sizes has no terms.
#[test]
fn the_terms_half_of_a_shim_row_parses_into_terms_or_none() {
    let row = |extra: serde_json::Value| {
        let mut r = json!({
            "instrument": "US30", "bid": 100.0, "ask": 101.0, "digits": 2, "instrument_type": 2,
            "contract_currency": "USD", "contract_multiplier": 0.1, "subscription": "T",
            "trading_status": "O", "bid_tradable": "T", "ask_tradable": "T", "base_unit": 1,
            "quote_age_ms": 120
        });
        r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        parse_offers(&serde_json::Value::Array(vec![r]).to_string()).unwrap().remove(0)
    };
    assert_eq!(row(json!({})).terms, None, "an older shim");
    assert_eq!(row(json!({"min_qty": 0, "max_qty": 10})).terms, None, "a minimum below one unit");
    assert_eq!(row(json!({"min_qty": 5, "max_qty": 4})).terms, None, "a maximum below the minimum");
    assert_eq!(
        row(json!({"min_qty": 1, "max_qty": 4000, "mmr": -1.0})).terms,
        Some(Terms { min_qty: 1, max_qty: 4000, margin_per_lot: None })
    );
    assert_eq!(
        row(json!({"min_qty": 1, "max_qty": 4000, "mmr": 12.5, "three_level": 0})).terms,
        Some(Terms { min_qty: 1, max_qty: 4000, margin_per_lot: Some(12.5) })
    );
    assert_eq!(
        row(json!({"min_qty": 1, "max_qty": 4000, "mmr": 12.5, "three_level": 1, "emr": 30.0}))
            .terms,
        Some(Terms { min_qty: 1, max_qty: 4000, margin_per_lot: Some(30.0) }),
        "a three-level account opens on its ENTRY margin"
    );
    assert_eq!(
        row(json!({"min_qty": 1, "max_qty": 4000, "mmr": 12.5, "three_level": 1, "emr": -1.0}))
            .terms
            .unwrap()
            .margin_per_lot,
        None
    );
}
