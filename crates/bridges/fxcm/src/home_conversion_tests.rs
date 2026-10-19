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

/// Metals, indices and every other CFD: refused by name, because the venue states no native currency for them.
#[test]
fn anything_but_a_regular_forex_instrument_has_no_determinable_quote_currency() {
    for (name, kind) in
        [("XAU/USD", 5), ("US30", 2), ("GER30/EUR", 2), ("BTC/USD", 9), ("AAPL.us", 6)]
    {
        let mut o = offer(name, 1.0, 1.0);
        o.instrument_type = kind;
        let why = quote_currency_of(&o).expect_err(name);
        assert!(why.contains("not determinable"), "{name}: {why}");
    }
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

    let mut metal = usd_offers();
    metal.push({
        let mut gold = offer("XAU/USD", 2000.0, 2000.5);
        gold.instrument_type = 5;
        gold
    });
    symbol_verdict(why("XAUUSD", &metal), "not determinable");

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
