use super::*;

const HOUR: i64 = 3_600_000;

/// USDJPY hourly bars opened at 0, 1h, 2h, 4h (3h missing), each closing at `150 + n`.
fn yen() -> ConversionSeries {
    let bar = |ts: i64, close: f64| Bar::ohlcv(ts, close, close, close, close, 1.0);
    ConversionSeries {
        quote_currency: "JPY".into(),
        pair: "dukascopy:USDJPY:1h".into(),
        leg: AccountPerQuoteLeg::AccountIsBase,
        interval_ms: HOUR,
        bars: vec![bar(0, 150.0), bar(HOUR, 151.0), bar(2 * HOUR, 152.0), bar(4 * HOUR, 154.0)],
        symbols: Vec::new(),
    }
}

/// The as-of rule: nothing before the first close, the bar closing exactly at `t` is seen, the bar
/// `t` falls in never is. Red on a mutant that reads the bar in progress or the next close.
#[test]
fn the_rate_is_the_close_of_the_last_bar_closed_by_then() {
    let s = yen();
    assert_eq!(s.rate_at(HOUR - 1), None, "the first bar closes at 1h");
    let at = |t| s.rate_at(t).expect("a rate");
    assert_eq!(at(HOUR).rate.get(), 1.0 / 150.0);
    assert_eq!(at(HOUR).as_of_ms, HOUR);
    assert_eq!(
        at(2 * HOUR - 1).rate.get(),
        1.0 / 150.0,
        "inside the second bar: the first's close"
    );
    assert_eq!(at(2 * HOUR).rate.get(), 1.0 / 151.0);
    assert_eq!(at(5 * HOUR).rate.get(), 1.0 / 154.0);
}

/// Older than twice the interval is stale, exactly twice is not: the missing 3h bar leaves the 2h
/// bar's close (closed at 3h) standing until the 4h bar closes at 5h.
#[test]
fn a_rate_older_than_two_intervals_is_stale() {
    let s = yen();
    let stale = |t| s.rate_at(t).expect("a rate").stale;
    assert!(!stale(3 * HOUR));
    assert!(!stale(5 * HOUR - 1), "the 2h bar closed at 3h: 2h old minus a ms");
    assert!(!stale(5 * HOUR), "the 4h bar closed at 5h");
    assert!(!stale(7 * HOUR), "exactly twice the interval");
    assert!(stale(7 * HOUR + 1));
    assert_eq!(s.rate_at(7 * HOUR + 1).expect("a rate").as_of_ms, 5 * HOUR);
}

/// The orientation is the leg's: the same close reads as it stands on a `QUOTE/ACCOUNT` pair.
#[test]
fn a_quote_first_pair_reads_its_close_as_it_stands() {
    let usdt = ConversionSeries {
        quote_currency: "USDT".into(),
        pair: "user-coinbase:coinbase:USDT-USD:1d".into(),
        leg: AccountPerQuoteLeg::QuoteIsBase,
        interval_ms: 24 * HOUR,
        bars: vec![Bar::ohlcv(0, 1.0, 1.0, 1.0, 0.9995, 1.0)],
        symbols: Vec::new(),
    };
    assert_eq!(usdt.rate_at(24 * HOUR).map(|r| r.rate.get()), Some(0.9995));
    assert_eq!(ConversionSeries { bars: Vec::new(), ..usdt }.rate_at(i64::MAX), None);
}
