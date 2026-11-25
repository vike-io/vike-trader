use super::*;

#[test]
fn a_typed_split_is_written_canonically_and_takes_effect_at_midnight_utc() {
    let split = StockSplit::new("dukascopy", " AAPLUSUSD ", "2020-8-31", 4.0).expect("a split");
    assert_eq!(split.symbol, "AAPLUSUSD");
    assert_eq!(split.date, "2020-08-31");
    // 2020-08-31T00:00:00Z
    assert_eq!(split.effective_ms(), Ok(1_598_832_000_000));
}

#[test]
fn a_reverse_split_is_a_split() {
    assert!(StockSplit::new("alpaca", "XYZ", "2024-01-02", 0.1).is_ok());
}

#[test]
fn what_is_no_split_is_refused_in_words() {
    for (venue, symbol, date, ratio) in [
        ("dukascopy", "AAPLUSUSD", "2020-08-31", 1.0),
        ("dukascopy", "AAPLUSUSD", "2020-08-31", 0.0),
        ("dukascopy", "AAPLUSUSD", "2020-08-31", -4.0),
        ("dukascopy", "AAPLUSUSD", "2020-08-31", f64::NAN),
        ("dukascopy", "AAPLUSUSD", "2020-08-31", f64::INFINITY),
        ("dukascopy", "AAPLUSUSD", "2020-02-30", 4.0),
        ("dukascopy", "AAPLUSUSD", "1598832000000", 4.0),
        ("dukascopy", "", "2020-08-31", 4.0),
        ("", "AAPLUSUSD", "2020-08-31", 4.0),
    ] {
        assert!(
            StockSplit::new(venue, symbol, date, ratio).is_err(),
            "{venue} {symbol} {date} {ratio}"
        );
    }
}

#[test]
fn a_row_read_back_is_judged_as_it_was_typed() {
    let stored = StockSplit {
        venue: "dukascopy".into(),
        symbol: "AAPLUSUSD".into(),
        date: "2020-13-01".into(),
        ratio: 4.0,
    };
    assert!(stored.effective_ms().is_err());
}
