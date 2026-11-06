use std::sync::Mutex;

use super::*;

fn quote(ts: i64, symbol: &str) -> QuoteTick {
    QuoteTick {
        ts,
        local_ts: 0,
        bid: 1.0 + (ts as f64) / 3.0e9,
        ask: 1.0 + (ts as f64) / 3.0e9 + 0.0001,
        bid_size: 1.5,
        ask_size: 2.0,
        symbol: symbol.to_string(),
    }
}

/// The bridge's `fetch_ticks_range` as a model: it walks the window's hour files from the hour
/// containing `start_ms` while `hour <= end_ms`, and keeps a tick of an hour file only when
/// `start_ms <= ts <= end_ms`. Six ticks per hour file, ten minutes apart.
fn whole_window(symbol: &str, start_ms: i64, end_ms: i64) -> Result<Vec<QuoteTick>, String> {
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(HOUR_MS) * HOUR_MS;
    while hour <= end_ms {
        for k in 0..6 {
            let ts = hour + k * 600_000 + 7;
            if start_ms <= ts && ts <= end_ms {
                out.push(quote(ts, symbol));
            }
        }
        hour += HOUR_MS;
    }
    Ok(out)
}

/// **The hour-sliced fetch IS the whole-window fetch**: same ticks, same order, for windows that are
/// aligned, unaligned, inside one hour, on a boundary, before the epoch and several hours wide.
#[test]
fn the_slices_concatenate_to_the_whole_window_answer() {
    let windows = [
        (0, 0),
        (5, 3_599_999),
        (100, 7_200_050),
        (HOUR_MS, HOUR_MS),
        (HOUR_MS - 1, HOUR_MS),
        (-5_000_000, 5_000_000),
        (1_234_567, 99_999_999),
        (3 * HOUR_MS, 5 * HOUR_MS - 1),
    ];
    for (start, end) in windows {
        let ctx = FetchCtx::new();
        let sliced = fetch_quotes(&whole_window, "EURUSD", start, end, &ctx).expect("sliced");
        let whole = whole_window("EURUSD", start, end).expect("whole");
        assert_eq!(sliced, whole, "window [{start}, {end}] differs once sliced by the hour");
    }
}

/// Every call the lane makes names ONE hour file, and the hours are consecutive and fetched once.
#[test]
fn each_call_names_one_hour_file_and_each_hour_is_fetched_once() {
    let calls: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());
    let fetch = |symbol: &str, from: i64, to: i64| {
        calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((from, to));
        whole_window(symbol, from, to)
    };
    let ctx = FetchCtx::new();
    fetch_quotes(&fetch, "EURUSD", 1_000, 3 * HOUR_MS + 50, &ctx).expect("a 4-hour window");
    let calls = calls.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        calls,
        vec![
            (1_000, HOUR_MS - 1),
            (HOUR_MS, 2 * HOUR_MS - 1),
            (2 * HOUR_MS, 3 * HOUR_MS - 1),
            (3 * HOUR_MS, 3 * HOUR_MS + 50),
        ]
    );
    for (from, to) in calls {
        assert_eq!(from.div_euclid(HOUR_MS), to.div_euclid(HOUR_MS), "[{from}, {to}] spans hours");
    }
}

/// An inverted window is the bridge's to answer, whole, exactly as it was in-process.
#[test]
fn an_inverted_window_goes_to_the_bridge_whole() {
    let calls: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());
    let fetch = |symbol: &str, from: i64, to: i64| {
        calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((from, to));
        whole_window(symbol, from, to)
    };
    let ctx = FetchCtx::new();
    let got = fetch_quotes(&fetch, "EURUSD", 10, 5, &ctx).expect("the bridge's answer");
    assert!(got.is_empty());
    assert_eq!(*calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner), vec![(10, 5)]);
}

/// A cancel is seen BETWEEN hour files: the third hour is never asked for, and the answer is an
/// error, not the two hours that were fetched.
#[test]
fn a_cancel_stops_the_walk_between_hour_files() {
    let ctx = FetchCtx::new();
    let asked = Mutex::new(0usize);
    let for_fetch = ctx.clone();
    let fetch = |symbol: &str, from: i64, to: i64| {
        let mut asked = asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *asked += 1;
        if *asked == 2 {
            for_fetch.cancel();
        }
        whole_window(symbol, from, to)
    };
    let out = fetch_quotes(&fetch, "EURUSD", 0, 6 * HOUR_MS, &ctx);
    assert!(matches!(out, Err(FetchFailure::Fetch(ref t)) if t.contains("between hour files")));
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 2);
}

/// A fetch cancelled before the first hour never reaches the CDN.
#[test]
fn a_fetch_cancelled_while_queued_asks_for_no_hour_at_all() {
    let ctx = FetchCtx::new();
    ctx.cancel();
    let fetch = |_: &str, _: i64, _: i64| -> Result<Vec<QuoteTick>, String> {
        panic!("a cancelled fetch reached the CDN")
    };
    assert!(fetch_quotes(&fetch, "EURUSD", 0, HOUR_MS, &ctx).is_err());
    assert!(fetch_quotes(&fetch, "EURUSD", 10, 5, &ctx).is_err());
}

/// A failed hour fails the fetch with the bridge's words unchanged, and no hour before it leaks out
/// as a short answer.
#[test]
fn a_failed_hour_fails_the_whole_fetch_in_the_bridges_words() {
    let fetch = |symbol: &str, from: i64, to: i64| {
        if from >= 2 * HOUR_MS {
            Err("network: connection reset by peer (hour 02)".to_string())
        } else {
            whole_window(symbol, from, to)
        }
    };
    let ctx = FetchCtx::new();
    assert_eq!(
        fetch_quotes(&fetch, "EURUSD", 0, 4 * HOUR_MS, &ctx),
        Err(FetchFailure::Fetch("network: connection reset by peer (hour 02)".to_string()))
    );
}

/// **A client cannot make the daemon walk the CDN without end, whatever its scope.** The window is the
/// client's, one hour file is one real request, so the widest window one fetch may name is
/// `GW_TICKS_MAX_SPAN_MS`; a wider one (down to `0..i64::MAX`, about 2.5e12 hour files) is `Refused`
/// and the venue is never asked. A window exactly at the bound still runs.
#[test]
fn a_window_over_the_span_bound_is_refused_before_the_cdn_is_asked() {
    let asked = Mutex::new(0usize);
    let fetch = |symbol: &str, from: i64, to: i64| {
        *asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        whole_window(symbol, from, to)
    };
    let ctx = FetchCtx::new();
    for (start, end) in [
        (0, GW_TICKS_MAX_SPAN_MS),
        (0, i64::MAX),
        (i64::MIN, i64::MAX),
        (i64::MIN, 0),
        (1_000, 1_000 + GW_TICKS_MAX_SPAN_MS + 5),
    ] {
        let out = fetch_quotes(&fetch, "EURUSD", start, end, &ctx);
        match out {
            Err(FetchFailure::Refused(ref text)) => {
                assert!(text.contains("nothing was asked of the venue"), "{text}");
                assert!(text.contains("hours"), "the refusal names the span: {text}");
            }
            other => panic!("[{start}, {end}] was not refused: {other:?}"),
        }
    }
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 0);

    // Exactly the bound runs: [0, SPAN - 1] covers SPAN milliseconds, 744 hour files.
    let got = fetch_quotes(&fetch, "EURUSD", 0, GW_TICKS_MAX_SPAN_MS - 1, &ctx).expect("at bound");
    assert_eq!(got.len(), 744 * 6);
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 744);
}

/// The symbol is spliced into the CDN path by the bridge, so only a plain code gets there.
#[test]
fn a_symbol_that_could_choose_the_cdn_path_is_refused() {
    let fetch = |_: &str, _: i64, _: i64| -> Result<Vec<QuoteTick>, String> {
        panic!("a refused symbol reached the CDN")
    };
    let ctx = FetchCtx::new();
    let long = "A".repeat(MAX_SYMBOL_BYTES + 1);
    for symbol in
        ["", "..", "EUR/USD", "EURUSD/../x", "EURUSD?x=1", "EUR USD", "EURUSD\n", long.as_str()]
    {
        let out = fetch_quotes(&fetch, symbol, 0, HOUR_MS, &ctx);
        assert!(
            matches!(out, Err(FetchFailure::Refused(ref t)) if t.contains("nothing was asked")),
            "{symbol:?} was not refused: {out:?}"
        );
    }
    // The inverted-window path is refused for a bad symbol too.
    assert!(matches!(fetch_quotes(&fetch, "a/b", 10, 5, &ctx), Err(FetchFailure::Refused(_))));
    // Plain codes pass.
    let ok = |symbol: &str, from: i64, to: i64| whole_window(symbol, from, to);
    for symbol in ["EURUSD", "eurusd", "USA500IDXUSD", "E_Gold", "BRENT-CMD"] {
        assert!(fetch_quotes(&ok, symbol, 0, HOUR_MS - 1, &ctx).is_ok(), "{symbol}");
    }
}
