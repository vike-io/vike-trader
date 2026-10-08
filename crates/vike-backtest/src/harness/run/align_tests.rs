//! `refuse_ragged_series`: the store-free guard against ragged multi-symbol series.

use super::*;
use vike_model::Bar;

// `Bar` derives no `Default` — a full struct literal stands in for the brief's
// `Bar::default()` sketch; only `ts` varies across callers here.
fn bar(ts: i64) -> Bar {
    Bar {
        ts,
        open: 1.0,
        high: 1.0,
        low: 1.0,
        close: 1.0,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[test]
fn a_ragged_universe_is_named_not_panicked() {
    let loaded = vec![
        ("BTCUSDT".to_string(), vec![bar(0), bar(60_000), bar(120_000)]),
        ("PEPEUSDT".to_string(), vec![bar(60_000), bar(120_000)]),
    ];
    let err = refuse_ragged_series(&loaded).unwrap_err();
    match err {
        HarnessError::Data(m) => {
            assert!(m.contains("BTCUSDT"), "names the long series: {m}");
            assert!(m.contains("PEPEUSDT"), "names the short series: {m}");
            assert!(m.contains('3') && m.contains('2'), "names both lengths: {m}");
        }
        other => panic!("expected a data error, got {other:?}"),
    }
}

#[test]
fn an_aligned_universe_passes() {
    let loaded = vec![
        ("BTCUSDT".to_string(), vec![bar(0), bar(60_000)]),
        ("ETHUSDT".to_string(), vec![bar(0), bar(60_000)]),
    ];
    assert!(refuse_ragged_series(&loaded).is_ok());
}

/// A single-symbol run can never be ragged, and must not pay for the check's message.
#[test]
fn one_series_is_always_aligned() {
    let loaded = vec![("BTCUSDT".to_string(), vec![bar(0)])];
    assert!(refuse_ragged_series(&loaded).is_ok());
}

/// Zero series is the empty-slice case someone else owns; this guard must not claim it.
#[test]
fn no_series_is_not_this_guards_failure() {
    let loaded: Vec<(String, Vec<Bar>)> = Vec::new();
    assert!(refuse_ragged_series(&loaded).is_ok());
}
