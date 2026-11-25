use super::*;

fn bars(n: i64) -> Vec<Bar> {
    (0..n).map(|ts| Bar::ohlcv(ts, 1.0, 1.0, 1.0, 1.0, 1.0)).collect()
}

/// A view is the range it names of the one series, a view of a view included, and shares it.
#[test]
fn a_slice_is_a_range_of_the_one_series() {
    let all = BarSeriesShared::from(bars(10));
    let part = all.slice(2..8).slice(1..4);
    assert_eq!(part.iter().map(|b| b.timestamp).collect::<Vec<_>>(), [3, 4, 5]);
    assert!(Arc::ptr_eq(&all.series, &part.series), "no bar copied");
}

/// The three ways a view is tagged: a series of its own in place, a view already tagged as it is, and a
/// view of a shared series copied once; each answers every bar tagged `SYMBOL.VENUE`.
#[test]
fn tagging_copies_only_a_shared_view_it_must_change() {
    let own = BarSeriesShared::from(bars(4));
    let before = own.as_ptr();
    let own = own.tagged(Some("bybit"), "BTCUSDT");
    assert_eq!(own.as_ptr(), before, "a series of its own is tagged in place");
    assert!(own.iter().all(|b| b.symbol.as_deref() == Some("BTCUSDT.BYBIT")));

    let view = own.slice(1..3);
    let kept = view.clone().tagged(Some("bybit"), "BTCUSDT");
    assert!(Arc::ptr_eq(&kept.series, &own.series), "a tagged view is kept as it is");

    let retagged = view.tagged(None, "BTCUSDT");
    assert!(!Arc::ptr_eq(&retagged.series, &own.series), "a shared view is copied to be tagged");
    assert_eq!(retagged.len(), 2);
    assert!(retagged.iter().all(|b| b.symbol.as_deref() == Some("BTCUSDT")));
    assert!(
        own.iter().all(|b| b.symbol.as_deref() == Some("BTCUSDT.BYBIT")),
        "the shared one kept"
    );
}
