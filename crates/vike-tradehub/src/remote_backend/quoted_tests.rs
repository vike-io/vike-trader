//! [`judge`]: every way a facts answer's conversion is usable, and each way it is not. The goldens are the
//! decision's own: EURGBP on a USD account is GBP at `1.35` (a pound is worth MORE than a dollar), USDJPY is
//! JPY at `1/150` (a yen is a fraction of one), and the same answers with the rate flipped are 22,500x apart,
//! which is why the orientation is asserted by value here and not only by type.

use super::*;

const NOW: i64 = 1_700_000_000_000;

fn facts(ccy: Option<&str>, rate: Option<f64>, as_of: Option<i64>) -> WireSymbolFacts {
    WireSymbolFacts {
        quote_ccy: ccy.map(str::to_string),
        quote_rate: rate,
        rate_as_of_ms: as_of,
        ..WireSymbolFacts::default()
    }
}

fn refused(v: Result<Option<Quote>, Verdict>) -> String {
    match v {
        Err(Verdict::Refused(why)) => why,
        other => panic!("expected a definitive refusal, got {other:?}"),
    }
}

fn unavailable(v: Result<Option<Quote>, Verdict>) -> String {
    match v {
        Err(Verdict::Unavailable(why)) => why,
        other => panic!("expected a not-now refusal, got {other:?}"),
    }
}

/// An answer that names no currency is the account's own, and a fresh foreign one carries its quote as given.
#[test]
fn an_account_quoted_symbol_has_no_quote_and_a_foreign_one_carries_its_rate_as_stated() {
    assert_eq!(judge("oanda", "GBPUSD", &facts(None, None, None), NOW), Ok(None));

    let gbp = judge("oanda", "EURGBP", &facts(Some("GBP"), Some(1.35), Some(NOW - 1_000)), NOW);
    assert_eq!(gbp, Ok(Some(Quote { ccy: "GBP".into(), rate: 1.35, as_of_ms: NOW - 1_000 })));
    // A yen is a FRACTION of a dollar: the rate the node forwards is the one stated, never inverted.
    let jpy = judge("oanda", "USDJPY", &facts(Some("JPY"), Some(1.0 / 150.0), Some(NOW)), NOW);
    let quote = jpy.expect("fresh").expect("foreign");
    assert_eq!(quote.rate.to_bits(), (1.0_f64 / 150.0).to_bits());
    assert!(quote.rate < 1.0, "JPY on a USD account is worth less than one dollar");
}

/// The edges of freshness: exactly the engine's window is fine, a millisecond past it is not; a rate from the
/// near future (clock jitter) is fine, one from far ahead is a daemon on another clock.
#[test]
fn freshness_is_the_engines_window_and_a_future_rate_is_a_clock_disagreement() {
    let at =
        |age: i64| judge("oanda", "EURGBP", &facts(Some("GBP"), Some(1.35), Some(NOW - age)), NOW);
    assert!(at(0).is_ok() && at(RATE_MAX_AGE_MS).is_ok());
    let old = unavailable(at(RATE_MAX_AGE_MS + 1));
    assert!(old.contains("NOT admitted") && old.contains("120 s"), "{old}");
    assert!(at(-RATE_MAX_AHEAD_MS).is_ok(), "jitter inside the allowance");
    let future = unavailable(at(-RATE_MAX_AHEAD_MS - 1));
    assert!(future.contains("clocks disagree") && future.contains("NOT admitted"), "{future}");
}

/// **No rate, no admission**: a missing, zero, negative, infinite or not-a-number rate, a missing moment, a
/// rate beside no currency and a code that is not a currency are each refused in words that say which, and
/// none of them is read as par.
#[test]
fn a_missing_or_unusable_rate_is_never_read_as_par() {
    let none = refused(judge("oanda", "EURGBP", &facts(Some("GBP"), None, Some(NOW)), NOW));
    assert!(none.contains("gave none") && none.contains("never valued at par"), "{none}");
    for bad in [0.0, -1.35, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let why = refused(judge("oanda", "EURGBP", &facts(Some("GBP"), Some(bad), Some(NOW)), NOW));
        assert!(
            why.contains("not a positive number") && why.contains("NOT admitted"),
            "{bad}: {why}"
        );
    }
    let undated = refused(judge("oanda", "EURGBP", &facts(Some("GBP"), Some(1.35), None), NOW));
    assert!(undated.contains("did not say when"), "{undated}");
    let orphan = refused(judge("oanda", "EURUSD", &facts(None, Some(1.35), Some(NOW)), NOW));
    assert!(orphan.contains("no currency to apply it to"), "{orphan}");
    let orphan_time = refused(judge("oanda", "EURUSD", &facts(None, None, Some(NOW)), NOW));
    assert!(orphan_time.contains("no currency to apply it to"), "{orphan_time}");
    for code in ["", "gbp", "GB", "GBPP", "G1P", "G P"] {
        let why = refused(judge("oanda", "EURGBP", &facts(Some(code), Some(1.35), Some(NOW)), NOW));
        assert!(why.contains("not a currency code"), "{code:?}: {why}");
    }
}
