use super::*;

pub(super) fn live(pairs: &[(&str, u64, i64)]) -> HashMap<String, Liveness> {
    pairs
        .iter()
        .map(|(k, rows, last_ms)| (k.to_string(), Liveness { rows: *rows, last_ms: *last_ms }))
        .collect()
}
pub(super) fn expect(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_flowing_series_is_not_reported() {
    let got = silent_series(
        &expect(&["trade/binance/BTC"]),
        &live(&[("trade/binance/BTC", 9, 990)]),
        1_000,
        60_000,
    );
    assert!(got.is_empty(), "{got:?}");
}

/// **The 95-minute case.** It received rows, then the venue stream went quiet — the loss
/// counters stay zero and nothing errors, so this is the only place it shows.
#[test]
fn a_series_that_stopped_is_reported_with_its_age() {
    let got = silent_series(
        &expect(&["trade/binance/BTC"]),
        &live(&[("trade/binance/BTC", 6_000, 1_000)]),
        1_000 + 95 * 60_000,
        60_000,
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].silent_for_ms, Some(95 * 60_000));
    assert_eq!(got[0].rows, 6_000, "it DID receive rows once — that is the diagnosis");
}

/// **The worse case**, and the one the handle alone can never report: a series with no entry at
/// all. `RecorderHandle::liveness` only knows series that received something, so "never
/// started" is only visible by diffing against what was subscribed.
#[test]
fn a_series_that_never_started_is_reported_distinctly() {
    let got = silent_series(&expect(&["depth/binance/BTC"]), &live(&[]), 5_000, 60_000);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].silent_for_ms, None, "never-started is not 'stale for 5s'");
    assert_eq!(got[0].rows, 0);
}

/// A series still inside the threshold is quiet, not silent — a 5-minute-window family can go
/// a minute between rows without being a fault.
#[test]
fn a_pause_shorter_than_the_threshold_is_not_a_fault() {
    let got =
        silent_series(&expect(&["trade/poly/T"]), &live(&[("trade/poly/T", 3, 0)]), 59_000, 60_000);
    assert!(got.is_empty());
}

/// A series the recorder is no longer subscribed to is IGNORED — a rotated-out Polymarket
/// token stops receiving rows by design, and reporting it would bury the real faults.
#[test]
fn an_unsubscribed_series_is_not_reported() {
    let got = silent_series(
        &expect(&["trade/poly/NEW"]),
        &live(&[("trade/poly/OLD", 500, 0), ("trade/poly/NEW", 1, 9_000)]),
        10_000,
        60_000,
    );
    assert!(got.is_empty(), "only OLD is stale, and OLD is no longer expected: {got:?}");
}

#[test]
fn output_is_sorted_so_a_log_line_is_stable() {
    let got = silent_series(&expect(&["b/v/s", "a/v/s", "c/v/s"]), &live(&[]), 0, 0);
    let names: Vec<&str> = got.iter().map(|s| s.series.as_str()).collect();
    assert_eq!(names, vec!["a/v/s", "b/v/s", "c/v/s"]);
}
