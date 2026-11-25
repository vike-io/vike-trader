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
        bars: Arc::new(vec![
            bar(0, 150.0),
            bar(HOUR, 151.0),
            bar(2 * HOUR, 152.0),
            bar(4 * HOUR, 154.0),
        ]),
        symbols: Vec::new(),
        par_where_missing: false,
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
        bars: Arc::new(vec![Bar::ohlcv(0, 1.0, 1.0, 1.0, 0.9995, 1.0)]),
        symbols: Vec::new(),
        par_where_missing: false,
    };
    assert_eq!(usdt.rate_at(24 * HOUR).map(|r| r.rate.get()), Some(0.9995));
    assert_eq!(ConversionSeries { bars: Arc::default(), ..usdt }.rate_at(i64::MAX), None);
}

/// The span `advance_quote_rates` passes a cursor by is SOUND: at every moment inside it, earlier or later than
/// the step that found it, the cursor's loop closes no further bar and the rate keeps every field a booking reads
/// (the rate, its kind, a bar's close time), on a series with a hole, marked par and not, around every turn. And
/// it holds the step's own moment, or it would save nothing.
#[test]
fn a_steady_span_keeps_every_answer_a_step_books() {
    let kept = |r: Option<ConversionRate>| {
        r.map(|r| (r.rate.get().to_bits(), r.par, (!r.par).then_some(r.as_of_ms)))
    };
    let grid: Vec<i64> =
        (-4..=32).flat_map(|q| [q * HOUR / 4 - 1, q * HOUR / 4, q * HOUR / 4 + 1]).collect();
    for s in [yen(), ConversionSeries { par_where_missing: true, ..yen() }] {
        for closed in 0..=s.bars.len() {
            for &t in &grid {
                let span = s.steady_span(closed, t);
                if s.bars.get(closed).is_none_or(|b| !b.closed_at(s.interval_ms, t)) {
                    assert!(span.contains(&t), "{closed} at {t}: {span:?}");
                }
                for &m in grid.iter().filter(|m| span.contains(m)) {
                    let more = s.bars.get(closed).is_some_and(|b| b.closed_at(s.interval_ms, m));
                    assert!(!more, "{closed} at {t}: a bar closes by {m} inside {span:?}");
                    let (now, then) = (s.rate_of_closed(closed, t), s.rate_of_closed(closed, m));
                    assert_eq!(kept(then), kept(now), "{closed} at {t}, {m} inside {span:?}");
                }
            }
        }
    }
}

/// The one exception (decision 0243, amended 2026-11-16): a `par_where_missing` series is one-to-one and never
/// stale wherever it has no rate (before its first close, where the missing 3h bar should have closed, past
/// its last bar), and its closes everywhere else; without the mark it fails closed.
#[test]
fn a_par_series_is_one_to_one_wherever_it_has_no_rate_and_no_other_is() {
    let par = ConversionSeries { par_where_missing: true, ..yen() };
    let at = |t| par.rate_at(t).expect("never none");
    assert_eq!((at(HOUR - 1).rate.get(), at(HOUR - 1).par, at(HOUR - 1).stale), (1.0, true, false));
    assert!(!at(i64::MIN).stale, "however long before");
    assert_eq!((at(HOUR).rate.get(), at(HOUR).par), (1.0 / 150.0, false), "the first close");
    assert_eq!(
        at(4 * HOUR - 1).rate.get(),
        1.0 / 152.0,
        "the 2h bar's close, in force one interval"
    );
    assert!(at(4 * HOUR).par, "the 3h bar should have closed at 4h: a hole");
    assert_eq!(
        (at(5 * HOUR).rate.get(), at(5 * HOUR).par),
        (1.0 / 154.0, false),
        "the 4h bar's close"
    );
    assert!(at(6 * HOUR).par && !at(6 * HOUR).stale, "past the last bar");
    assert_eq!(yen().rate_at(HOUR - 1), None, "unmarked: no rate, never par");
    assert!(
        yen().rate_at(7 * HOUR + 1).expect("a rate").stale,
        "unmarked: a stale close stays stale"
    );
}
