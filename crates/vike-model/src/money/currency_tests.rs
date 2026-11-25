use super::*;

/// The engine's reading of a code (vike-execution's former `ccy_code` / `same_ccy`): trimmed and
/// upper-cased, any non-empty word, a settlement coin included. Red on a mutant that skips the trim,
/// the case fold or the empty check.
#[test]
fn a_currency_code_is_any_word_trimmed_and_upper_cased() {
    assert_eq!(currency_code(" usd ").as_deref(), Some("USD"));
    assert_eq!(currency_code("JPY").as_deref(), Some("JPY"));
    assert_eq!(currency_code("usdt").as_deref(), Some("USDT"));
    for empty in ["", "   ", "\t"] {
        assert_eq!(currency_code(empty), None, "{empty:?}");
    }
    assert!(same_currency(" usd", "USD "));
    assert!(same_currency("Usdt", "USDT"));
    assert!(!same_currency("USD", "USDT"));
    assert!(!same_currency("USD", ""));
}

/// The pin of cTrader's, Dukascopy's, OANDA's and IBKR's former copies (each crate's
/// `a_rate_cannot_be_built_from_a_non_rate`), and FXCM's
/// (`account_per_quote_exists_only_for_a_finite_positive_number`, with its own type, until 2026-11-11). Red on a mutant that drops `is_finite` (inf
/// passes) or turns `> 0.0` into `>= 0.0` (zero passes).
#[test]
fn a_rate_cannot_be_built_from_a_non_rate() {
    for bad in [0.0, -1.0, -0.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(AccountPerQuote::new(bad), None, "{bad}");
    }
    assert_eq!(AccountPerQuote::new(0.0066).map(AccountPerQuote::get), Some(0.0066));
    assert_eq!(AccountPerQuote::new(0.5).map(|r| r.to_account(10_000.0)), Some(5_000.0));
    assert_eq!(AccountPerQuote::IDENTITY.get(), 1.0);
}

/// The yen golden, the classic bug: USDJPY at 150 on a dollar account is `1/150` dollar per yen,
/// so ten thousand yen are 66.67 dollars, and the flipped reading (150 dollars per yen) would be
/// 22,500 times that. GBPUSD at 1.35 for a pound quote is 1.35 dollars per pound as it stands.
#[test]
fn a_yen_is_a_fraction_of_a_dollar_never_150_dollars() {
    let yen = AccountPerQuote::of_mid(AccountPerQuoteLeg::AccountIsBase, 150.0).expect("a rate");
    assert_eq!(yen.get(), 1.0 / 150.0);
    assert!((yen.to_account(10_000.0) - 66.666_666_666_666_67).abs() < 1e-9);
    let flipped = AccountPerQuote::of_mid(AccountPerQuoteLeg::QuoteIsBase, 150.0).expect("a rate");
    assert!((flipped.to_account(10_000.0) / yen.to_account(10_000.0) - 22_500.0).abs() < 1e-6);
    assert_eq!(
        AccountPerQuote::of_mid(AccountPerQuoteLeg::QuoteIsBase, 1.35).map(AccountPerQuote::get),
        Ok(1.35)
    );
}

/// A mid that is no price gives no rate either way round, and the refusal carries the oriented figure.
#[test]
fn a_mid_that_is_no_price_is_no_rate() {
    for leg in [AccountPerQuoteLeg::QuoteIsBase, AccountPerQuoteLeg::AccountIsBase] {
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(AccountPerQuote::of_mid(leg, bad).is_err(), "{leg:?} {bad}");
        }
    }
    assert_eq!(AccountPerQuote::of_mid(AccountPerQuoteLeg::AccountIsBase, 0.0), Err(f64::INFINITY));
    assert_eq!(AccountPerQuote::of_mid(AccountPerQuoteLeg::QuoteIsBase, -1.0), Err(-1.0));
}

/// Every kind writes the source's sentence as it stands, so a bridge moving onto the one error keeps
/// its text byte for byte (cTrader's prefix, OANDA's four sentences, Dukascopy's bare words).
#[test]
fn a_rate_error_says_the_venues_words_and_nothing_else() {
    let ctrader = "ctrader: no two-sided quote for USDJPY";
    let oanda = "oanda sent no home conversion for CHF, so it has no rate (never 1.0)";
    let dukascopy = "dukascopy: JPY is the account's own currency; there is nothing to convert";
    assert_eq!(QuoteRateError::Unavailable(ctrader.into()).to_string(), ctrader);
    assert_eq!(QuoteRateError::BadQuote(oanda.into()).to_string(), oanda);
    assert_eq!(QuoteRateError::NoPair(dukascopy.into()).to_string(), dukascopy);
}

/// A pair's name against the ledger: `QUOTE`+`ACCOUNT` as it stands, `ACCOUNT`+`QUOTE` inverted, any
/// separator and case; anything else (a suffix, a third code, the wrong currency, one currency twice)
/// is no pair. Red on a mutant that swaps the two legs, keeps a separator or reads a prefix.
#[test]
fn a_pair_name_points_one_way_or_is_no_pair() {
    use AccountPerQuoteLeg::{AccountIsBase, QuoteIsBase};
    let leg = AccountPerQuoteLeg::of_pair_name;
    assert_eq!(leg("USDJPY", "JPY", "USD"), Some(AccountIsBase));
    assert_eq!(leg("usd/jpy", "jpy", " usd "), Some(AccountIsBase));
    assert_eq!(leg("GBPUSD", "GBP", "USD"), Some(QuoteIsBase));
    assert_eq!(leg("USDT-USD", "USDT", "USD"), Some(QuoteIsBase));
    assert_eq!(leg("USD_USDT", "USDT", "USD"), Some(AccountIsBase));
    assert_eq!(leg("EUR.USD", "USD", "EUR"), Some(AccountIsBase));
    for (pair, quote, account) in [
        ("USDJPYm", "JPY", "USD"),
        ("EURJPY", "JPY", "USD"),
        ("USDJPY", "GBP", "USD"),
        ("USDUSD", "USD", "USD"),
        ("USDT-USD", "USD", "USD"),
        ("", "JPY", "USD"),
        ("USDJPY", "", "USD"),
    ] {
        assert_eq!(leg(pair, quote, account), None, "{pair} {quote} {account}");
    }
    // The rate the leg orients is `of_mid`'s: USDJPY at 150 is 1/150 dollar per yen.
    let rate = AccountPerQuote::of_mid(leg("USDJPY", "JPY", "USD").expect("a leg"), 150.0);
    assert_eq!(rate.map(AccountPerQuote::get), Ok(1.0 / 150.0));
}

proptest::proptest! {
    /// Both directions, for any usable mid (a yen's 150 and a pound's 1.35 included): a
    /// `QUOTE/ACCOUNT` mid is the rate bit for bit, an `ACCOUNT/QUOTE` one is `1 / mid` bit for
    /// bit, so a quote amount converts by MULTIPLYING on the first and by DIVIDING by the mid on the
    /// second, and one account unit's worth of quote converts back to one account unit.
    #[test]
    fn the_orientation_reads_quote_base_as_is_and_account_base_inverted(
        mid in 1e-4f64..1e5,
        amount in 1e-2f64..1e9,
    ) {
        let as_is = AccountPerQuote::of_mid(AccountPerQuoteLeg::QuoteIsBase, mid).expect("a rate");
        let inverted = AccountPerQuote::of_mid(AccountPerQuoteLeg::AccountIsBase, mid).expect("a rate");
        proptest::prop_assert_eq!(as_is.get().to_bits(), mid.to_bits());
        proptest::prop_assert_eq!(inverted.get().to_bits(), (1.0 / mid).to_bits());
        proptest::prop_assert_eq!(as_is.to_account(amount), amount * mid);
        let by_division = amount / mid;
        proptest::prop_assert!((inverted.to_account(amount) - by_division).abs() <= by_division * 1e-15);
        proptest::prop_assert!((inverted.to_account(mid) - 1.0).abs() <= 1e-15);
    }
}

/// The dollar words: the empty group key reads USD; the dollar GROUP is no code, USD or USDT in any
/// case (USDC keeps its own); the three LABELS are compared as written. Red on a mutant that drops
/// the trim, the case fold, USDT from the group or adds USDC to it.
#[test]
fn the_dollar_words_say_which_codes_are_dollars() {
    assert_eq!(DEFAULT_CURRENCY, "USD");
    assert_eq!(DOLLAR_LABELS, ["USD", "USDC", "USDT"]);
    assert_eq!(ledger_label(""), "USD");
    assert_eq!(ledger_label("JPY"), "JPY");
    assert_eq!(ledger_label("USDT"), "USDT");
    for dollar in ["", "  ", "USD", "usd", " USDT ", "usdt"] {
        assert!(is_dollar_group(dollar), "{dollar:?}");
    }
    for own in ["USDC", "JPY", "BTC", "USDX"] {
        assert!(!is_dollar_group(own), "{own:?}");
    }
}

/// The fiat list stays whole (29 codes, SEK, HKD and MXN among them) and is read trimmed in any
/// case; a stablecoin, a minor unit and an empty word are no fiat code.
#[test]
fn a_fiat_code_is_one_of_the_whole_list() {
    assert_eq!(FIAT_CODES.len(), 29);
    for code in FIAT_CODES {
        assert_eq!(fiat_code(&format!(" {} ", code.to_ascii_lowercase())), Some(*code));
    }
    for code in ["SEK", "HKD", "MXN", "CLP", "COP", "TWD"] {
        assert_eq!(fiat_code(code), Some(code));
    }
    for none in ["USDT", "GBX", "ILA", "ZAC", "BTC", "", "  "] {
        assert_eq!(fiat_code(none), None, "{none:?}");
    }
}

/// The minor-unit table: each minor code finds its major and back, trimmed and in any case (`ZAc`);
/// a major is never a minor code and the reverse (so `GBp` folds to the pound, not to pence); every
/// major is a fiat code and no minor code is.
#[test]
fn a_minor_unit_is_found_by_either_of_its_codes() {
    let pence = MinorUnit { major: "GBP", minor: "GBX" };
    assert_eq!(MinorUnit::of_minor("GBX"), Some(pence));
    assert_eq!(MinorUnit::of_minor(" gbx "), Some(pence));
    assert_eq!(MinorUnit::of_major("gbp"), Some(pence));
    assert_eq!(MinorUnit::of_minor("GBp"), None);
    assert_eq!(MinorUnit::of_major("GBp"), Some(pence));
    assert_eq!(MinorUnit::of_minor("ILA").map(|unit| unit.major), Some("ILS"));
    assert_eq!(MinorUnit::of_minor("ZAc").map(|unit| unit.major), Some("ZAR"));
    assert_eq!(MinorUnit::of_major("ZAR").map(|unit| unit.minor), Some("ZAC"));
    for not_minor in ["GBP", "ILS", "ZAR", "USD", ""] {
        assert_eq!(MinorUnit::of_minor(not_minor), None, "{not_minor:?}");
    }
    for not_major in ["GBX", "ILA", "ZAC", "USD", ""] {
        assert_eq!(MinorUnit::of_major(not_major), None, "{not_major:?}");
    }
    for unit in MINOR_UNITS {
        assert_eq!(fiat_code(unit.major), Some(unit.major));
        assert_eq!(fiat_code(unit.minor), None);
    }
    assert_ne!(FiatQuote::Minor(pence), FiatQuote::Code("GBP"));
}

/// A quote rate's code enters in the engine's spelling, and a code with nothing in it is no record.
#[test]
fn a_quote_rate_normalises_its_code() {
    let rate = AccountPerQuote::new(0.0066).expect("a rate");
    let made = QuoteRate::new(" jpy ", rate, 42).expect("a code");
    assert_eq!(
        made,
        QuoteRate { quote_currency: "JPY".into(), account_per_quote: rate, as_of_ms: 42 }
    );
    assert_eq!(QuoteRate::new("usdt", rate, 0).map(|r| r.quote_currency).as_deref(), Some("USDT"));
    for empty in ["", "  "] {
        assert_eq!(QuoteRate::new(empty, rate, 42), None, "{empty:?}");
    }
}

/// The ruling: the venue's code when it states one (normalised), else USD, ASSUMED, with the
/// reason; the reason is not asked for a stated code.
#[test]
fn an_account_currency_is_the_venues_word_else_an_assumed_dollar() {
    let stated =
        AccountCurrency::of_venue(Some(" eur "), || unreachable!("a stated code asks no reason"));
    assert_eq!(stated, AccountCurrency::Stated("EUR".into()));
    assert_eq!((stated.code(), stated.is_assumed()), ("EUR", false));
    for silent in [None, Some(""), Some("  ")] {
        let assumed = AccountCurrency::of_venue(silent, || "no BASE_CRNCY".into());
        assert_eq!(assumed, AccountCurrency::Assumed { why: "no BASE_CRNCY".into() });
        assert_eq!((assumed.code(), assumed.is_assumed()), ("USD", true));
    }
}

/// A venue's list as the pick's lookup: its pair written `BASE/COUNTER`, upper case.
fn venue_list(pairs: &'static [&'static str]) -> impl FnMut(&str, &str) -> Option<&'static str> {
    move |base: &str, counter: &str| {
        pairs.iter().copied().find(|pair| *pair == format!("{base}/{counter}"))
    }
}

fn leg(
    pair: &'static str,
    from: &str,
    to: &str,
    orientation: AccountPerQuoteLeg,
) -> ConversionLeg<&'static str> {
    ConversionLeg { pair, from: from.into(), to: to.into(), orientation }
}

/// The direct pair in either orientation, the codes upper-cased before the lookup sees them; the
/// direct pair wins over a route through USD that is also listed.
#[test]
fn the_direct_pair_is_picked_either_way_round() {
    use AccountPerQuoteLeg::{AccountIsBase, QuoteIsBase};
    let pound = ConversionRoute::pick("gbp", " usd", venue_list(&["GBP/USD"]));
    assert_eq!(pound, Ok(ConversionRoute::Direct(leg("GBP/USD", "GBP", "USD", QuoteIsBase))));
    let yen = ConversionRoute::pick("JPY", "USD", venue_list(&["EUR/USD", "USD/JPY"]));
    assert_eq!(yen, Ok(ConversionRoute::Direct(leg("USD/JPY", "JPY", "USD", AccountIsBase))));
    let cross = ConversionRoute::pick("GBP", "EUR", venue_list(&["EUR/USD", "GBP/USD", "EUR/GBP"]));
    assert_eq!(cross, Ok(ConversionRoute::Direct(leg("EUR/GBP", "GBP", "EUR", AccountIsBase))));
    assert_eq!(cross.map(|route| route.legs().len()), Ok(1));
}

/// No pair of the two: two legs through USD, each oriented by its own listing, and the rate is the
/// product of the legs' rates (fxcm's golden: HKD on a euro ledger with USD/HKD at 7.84451 and
/// EUR/USD at 1.118035 is 0.1140194 euro per Hong Kong dollar).
#[test]
fn a_cross_is_two_legs_through_the_dollar() {
    use AccountPerQuoteLeg::AccountIsBase;
    let route =
        ConversionRoute::pick("HKD", "EUR", venue_list(&["USD/HKD", "EUR/USD"])).expect("a route");
    assert_eq!(
        route,
        ConversionRoute::ThroughHub([
            leg("USD/HKD", "HKD", "USD", AccountIsBase),
            leg("EUR/USD", "USD", "EUR", AccountIsBase),
        ])
    );
    let mid = |pair: &str| if pair == "USD/HKD" { 7.84451 } else { 1.118035 };
    let rate: f64 = route
        .legs()
        .iter()
        .map(|leg| AccountPerQuote::of_mid(leg.orientation, mid(leg.pair)).expect("a rate").get())
        .product();
    assert!((rate - 0.114_019_4).abs() < 1e-7, "{rate}");
    let sterling = ConversionRoute::pick("SEK", "GBP", venue_list(&["USD/SEK", "GBP/USD"]));
    assert_eq!(
        sterling.map(|route| route.legs().iter().map(|leg| leg.pair).collect::<Vec<_>>()),
        Ok(vec!["USD/SEK", "GBP/USD"])
    );
}

/// A pair listed BOTH ways round is refused, on the direct pair and on a leg through USD alike,
/// never chosen between; no route, one currency twice and an empty code are refusals too.
#[test]
fn a_pair_listed_both_ways_or_no_route_is_refused() {
    let both = |from: &str, to: &str| ConversionRefusal::BothOrientations {
        from: from.into(),
        to: to.into(),
    };
    assert_eq!(
        ConversionRoute::pick("JPY", "USD", venue_list(&["USD/JPY", "JPY/USD"])),
        Err(both("JPY", "USD"))
    );
    assert_eq!(
        ConversionRoute::pick("HKD", "EUR", venue_list(&["USD/HKD", "HKD/USD", "EUR/USD"])),
        Err(both("HKD", "USD"))
    );
    assert_eq!(
        ConversionRoute::pick("HKD", "EUR", venue_list(&["USD/HKD", "EUR/USD", "USD/EUR"])),
        Err(both("USD", "EUR"))
    );
    let nothing = Err::<ConversionRoute<&str>, _>(ConversionRefusal::NoPair);
    assert_eq!(ConversionRoute::pick("HKD", "EUR", venue_list(&["EUR/USD"])), nothing);
    assert_eq!(ConversionRoute::pick("HKD", "EUR", venue_list(&["USD/HKD"])), nothing);
    // A dollar side has no route through the dollar: the direct pair or nothing.
    assert_eq!(ConversionRoute::pick("JPY", "USD", venue_list(&["EUR/USD", "EUR/JPY"])), nothing);
    assert_eq!(
        ConversionRoute::pick("usd", "USD", venue_list(&[])),
        Err(ConversionRefusal::SameCurrency)
    );
    for (quote, account) in [("", "USD"), ("JPY", "  ")] {
        assert_eq!(
            ConversionRoute::pick(quote, account, venue_list(&[])),
            Err(ConversionRefusal::NotACurrency)
        );
    }
}
