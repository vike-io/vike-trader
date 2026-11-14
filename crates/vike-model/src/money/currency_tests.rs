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
