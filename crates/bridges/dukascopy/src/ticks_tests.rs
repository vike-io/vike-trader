use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::refusals::DukascopyHistoryRefusalId;

/// The span bound the gatewayhub hands the production lane: `vike_gatewayhub_client::proto`'s
/// `GATEWAY_TICKS_MAX_SPAN_MS`, which this crate cannot name (`crates/vike-gatewayhub/src/rest/ticks_tests.rs`
/// holds the composition to it).
const MAX_SPAN_MS: i64 = 31 * 24 * MS_PER_HOUR;

fn quote(ts: i64, symbol: &str) -> TickQuote {
    TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid: 1.0 + (ts as f64) / 3.0e9,
        ask: 1.0 + (ts as f64) / 3.0e9 + 0.0001,
        bid_size: 1.5,
        ask_size: 2.0,
        symbol: symbol.into(),
    }
}

/// The hour walk at the production span bound, asking `stop` (a cancel flag, as the engine's probe reads
/// the request's).
fn fetch_quotes(
    fetch: &QuoteFn<'_>,
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
    stop: &AtomicBool,
) -> Result<Vec<TickQuote>, TicksError> {
    walk(fetch, symbol, start_ms, end_ms, MAX_SPAN_MS, &|| stop.load(Ordering::SeqCst))
}

/// The datafeed's `fetch_ticks_range` as a model: it walks the window's hour files from the hour
/// containing `start_ms` while `hour <= end_ms`, and keeps a tick of an hour file only when
/// `start_ms <= ts <= end_ms`. Six ticks per hour file, ten minutes apart.
fn whole_window(symbol: &str, start_ms: i64, end_ms: i64) -> Result<Vec<TickQuote>, String> {
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(MS_PER_HOUR) * MS_PER_HOUR;
    while hour <= end_ms {
        for k in 0..6 {
            let ts = hour + k * 600_000 + 7;
            if start_ms <= ts && ts <= end_ms {
                out.push(quote(ts, symbol));
            }
        }
        hour += MS_PER_HOUR;
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
        (MS_PER_HOUR, MS_PER_HOUR),
        (MS_PER_HOUR - 1, MS_PER_HOUR),
        (-5_000_000, 5_000_000),
        (1_234_567, 99_999_999),
        (3 * MS_PER_HOUR, 5 * MS_PER_HOUR - 1),
    ];
    for (start, end) in windows {
        let stop = AtomicBool::new(false);
        let sliced = fetch_quotes(&whole_window, "EURUSD", start, end, &stop).expect("sliced");
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
    let stop = AtomicBool::new(false);
    fetch_quotes(&fetch, "EURUSD", 1_000, 3 * MS_PER_HOUR + 50, &stop).expect("a 4-hour window");
    let calls = calls.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        calls,
        vec![
            (1_000, MS_PER_HOUR - 1),
            (MS_PER_HOUR, 2 * MS_PER_HOUR - 1),
            (2 * MS_PER_HOUR, 3 * MS_PER_HOUR - 1),
            (3 * MS_PER_HOUR, 3 * MS_PER_HOUR + 50),
        ]
    );
    for (from, to) in calls {
        assert_eq!(
            from.div_euclid(MS_PER_HOUR),
            to.div_euclid(MS_PER_HOUR),
            "[{from}, {to}] spans hours"
        );
    }
}

/// An inverted window is the datafeed's to answer, whole, exactly as it was in-process.
#[test]
fn an_inverted_window_goes_to_the_datafeed_whole() {
    let calls: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());
    let fetch = |symbol: &str, from: i64, to: i64| {
        calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((from, to));
        whole_window(symbol, from, to)
    };
    let stop = AtomicBool::new(false);
    let got = fetch_quotes(&fetch, "EURUSD", 10, 5, &stop).expect("the datafeed's answer");
    assert!(got.is_empty());
    assert_eq!(*calls.lock().unwrap_or_else(std::sync::PoisonError::into_inner), vec![(10, 5)]);
}

/// A stop is seen BETWEEN hour files: the third hour is never asked for, and the answer is an
/// error, not the two hours that were fetched.
#[test]
fn a_stop_ends_the_walk_between_hour_files() {
    let stop = AtomicBool::new(false);
    let asked = Mutex::new(0usize);
    let fetch = |symbol: &str, from: i64, to: i64| {
        let mut asked = asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *asked += 1;
        if *asked == 2 {
            stop.store(true, Ordering::SeqCst);
        }
        whole_window(symbol, from, to)
    };
    let out = fetch_quotes(&fetch, "EURUSD", 0, 6 * MS_PER_HOUR, &stop);
    assert_eq!(out, Err(TicksError::Stopped("between hour files")));
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 2);
}

/// A fetch stopped before the first hour never reaches the CDN.
#[test]
fn a_fetch_stopped_while_queued_asks_for_no_hour_at_all() {
    let stop = AtomicBool::new(true);
    let fetch = |_: &str, _: i64, _: i64| -> Result<Vec<TickQuote>, String> {
        panic!("a stopped fetch reached the CDN")
    };
    assert!(fetch_quotes(&fetch, "EURUSD", 0, MS_PER_HOUR, &stop).is_err());
    assert!(fetch_quotes(&fetch, "EURUSD", 10, 5, &stop).is_err());
}

/// A failed hour fails the fetch with the datafeed's words unchanged, and no hour before it leaks out
/// as a short answer.
#[test]
fn a_failed_hour_fails_the_whole_fetch_in_the_datafeeds_words() {
    let fetch = |symbol: &str, from: i64, to: i64| {
        if from >= 2 * MS_PER_HOUR {
            Err("network: connection reset by peer (hour 02)".to_string())
        } else {
            whole_window(symbol, from, to)
        }
    };
    let stop = AtomicBool::new(false);
    assert_eq!(
        fetch_quotes(&fetch, "EURUSD", 0, 4 * MS_PER_HOUR, &stop),
        Err(TicksError::Fetch("network: connection reset by peer (hour 02)".to_string()))
    );
}

/// **No `dukascopy live history rest` row: every fetch is refused naming the row**, and nothing is
/// dialled (an empty row set has no host to dial).
#[test]
fn with_no_history_row_the_production_lane_refuses_naming_the_row() {
    let lane = TicksSpecDukascopy::from_urls(&VenueUrls::default(), MAX_SPAN_MS, QuoteBus::new());
    let Err(TicksError::Fetch(why)) = lane.fetch_until("EURUSD", 0, MS_PER_HOUR - 1, &|| false)
    else {
        panic!("no row, no feed");
    };
    assert!(why.contains("live history rest"), "{why}");
}

/// **The point value of every record is the account's list's, never assumed**: with no list stated yet, and for a name
/// the list lacks, the production lane fails the fetch in words BEFORE the CDN is asked (both answers come back at once,
/// with no host dialled), and the datahub asks again later.
#[test]
fn with_no_list_or_a_name_off_it_the_production_lane_fails_before_the_cdn_is_asked() {
    let seeded = VenueUrls::seed("dukascopy");
    let unlisted = TicksSpecDukascopy::from_urls(&seeded, MAX_SPAN_MS, QuoteBus::new());
    let Err(TicksError::Fetch(why)) = unlisted.fetch_until("EURUSD", 0, MS_PER_HOUR - 1, &|| false)
    else {
        panic!("no list, no scale");
    };
    assert_eq!(why, crate::instrument::grid::not_listed());
    let bus = QuoteBus::new();
    bus.set_instrument_list(crate::test_support::listed());
    let listed = TicksSpecDukascopy::from_urls(&seeded, MAX_SPAN_MS, bus);
    let Err(TicksError::Fetch(why)) = listed.fetch_until("GBPNOK", 0, MS_PER_HOUR - 1, &|| false)
    else {
        panic!("off the list, no scale");
    };
    let off_the_list = DukascopyHistoryRefusal::NotOnList {
        name: "GBPNOK".to_string(),
        count: crate::test_support::listed().len(),
    };
    assert_eq!(why, off_the_list.to_string());
}

/// **A client cannot make the daemon walk the CDN without end, whatever its scope.** The window is the
/// client's, one hour file is one real request, so the widest window one fetch may name is the span
/// bound; a wider one (down to `0..i64::MAX`, about 2.5e12 hour files) is refused and the venue is never
/// asked. A window exactly at the bound still runs.
#[test]
fn a_window_over_the_span_bound_is_refused_before_the_cdn_is_asked() {
    let asked = Mutex::new(0usize);
    let fetch = |symbol: &str, from: i64, to: i64| {
        *asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        whole_window(symbol, from, to)
    };
    let stop = AtomicBool::new(false);
    for (start, end) in [
        (0, MAX_SPAN_MS),
        (0, i64::MAX),
        (i64::MIN, i64::MAX),
        (i64::MIN, 0),
        (1_000, 1_000 + MAX_SPAN_MS + 5),
    ] {
        let out = fetch_quotes(&fetch, "EURUSD", start, end, &stop);
        match out {
            Err(TicksError::Refused(ref text)) => {
                let why = refusal("EURUSD", start, end, MAX_SPAN_MS).expect("refused");
                assert_eq!(why.id(), DukascopyHistoryRefusalId::SpanTooWide, "{text}");
                assert_eq!(*text, why.to_string());
            }
            other => panic!("[{start}, {end}] was not refused: {other:?}"),
        }
    }
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 0);

    // Exactly the bound runs: [0, SPAN - 1] covers SPAN milliseconds, 744 hour files.
    let got = fetch_quotes(&fetch, "EURUSD", 0, MAX_SPAN_MS - 1, &stop).expect("at bound");
    assert_eq!(got.len(), 744 * 6);
    assert_eq!(*asked.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 744);
}

/// The span bound is the one the lane was BUILT with: a lane bounded at one hour refuses two.
#[test]
fn the_span_bound_is_the_one_the_lane_was_built_with() {
    let lane = TicksSpecDukascopy::with_fetch(Arc::new(whole_window), MS_PER_HOUR);
    assert!(lane.fetch_until("EURUSD", 0, MS_PER_HOUR - 1, &|| false).is_ok());
    assert!(matches!(
        lane.fetch_until("EURUSD", 0, MS_PER_HOUR, &|| false),
        Err(TicksError::Refused(ref t))
            if *t == DukascopyHistoryRefusal::SpanTooWide { hours: 1, max_hours: 1 }.to_string()
    ));
}

/// The symbol is spliced into the CDN path by the datafeed, so only a plain code gets there.
#[test]
fn a_symbol_that_could_choose_the_cdn_path_is_refused() {
    let fetch = |_: &str, _: i64, _: i64| -> Result<Vec<TickQuote>, String> {
        panic!("a refused symbol reached the CDN")
    };
    let stop = AtomicBool::new(false);
    let long = "A".repeat(MAX_SYMBOL_BYTES + 1);
    for symbol in
        ["", "..", "EUR/USD", "EURUSD/../x", "EURUSD?x=1", "EUR USD", "EURUSD\n", long.as_str()]
    {
        let out = fetch_quotes(&fetch, symbol, 0, MS_PER_HOUR, &stop);
        let not_plain =
            DukascopyHistoryRefusal::SymbolNotPlain { max: MAX_SYMBOL_BYTES, bytes: symbol.len() };
        assert_eq!(out, Err(TicksError::Refused(not_plain.to_string())), "{symbol:?}");
    }
    // The inverted-window path is refused for a bad symbol too.
    assert!(matches!(fetch_quotes(&fetch, "a/b", 10, 5, &stop), Err(TicksError::Refused(_))));
    // Plain codes pass.
    let ok = |symbol: &str, from: i64, to: i64| whole_window(symbol, from, to);
    for symbol in ["EURUSD", "eurusd", "USA500IDXUSD", "E_Gold", "BRENT-CMD"] {
        assert!(fetch_quotes(&ok, symbol, 0, MS_PER_HOUR - 1, &stop).is_ok(), "{symbol}");
    }
}

/// **The refusal words and the stop stages, byte for byte**: a refusal reaches the gatewayhub's peer in
/// these words, and a stage is what the engine's cancel words name.
#[test]
fn the_refusal_words_and_the_stop_stages_are_these() {
    let fetch = |symbol: &str, from: i64, to: i64| whole_window(symbol, from, to);
    let stop = AtomicBool::new(false);
    assert_eq!(
        fetch_quotes(&fetch, "EUR/USD", 0, MS_PER_HOUR, &stop),
        Err(TicksError::Refused(
            "gatewayhub: a tick symbol is 1 to 32 ASCII letters, digits, `_` or `-`; 7 bytes were \
             sent; nothing was asked of the venue"
                .to_string()
        ))
    );
    assert_eq!(
        fetch_quotes(&fetch, "EURUSD", 0, 800 * MS_PER_HOUR - 1, &stop),
        Err(TicksError::Refused(
            "gatewayhub: a tick fetch spans 800 hours and one request may name at most 744 (one \
             hour file is one CDN request); ask in chunks; nothing was asked of the venue"
                .to_string()
        ))
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(
        fetch_quotes(&fetch, "EURUSD", 0, MS_PER_HOUR, &stop),
        Err(TicksError::Stopped("between hour files"))
    );
    assert_eq!(
        fetch_quotes(&fetch, "EURUSD", 10, 5, &stop),
        Err(TicksError::Stopped("before the venue was asked"))
    );
}

// ── one download at a time: the lane's lock (the owner's ruling of 2026-11-16) ──────────────────

/// **The lane takes one download at a time**: two backfills started together never have two requests in
/// flight and never interleave their hours; the second waits for its turn and then runs whole.
#[test]
fn the_lane_lock_queues_a_second_backfill_behind_the_first() {
    use std::sync::atomic::AtomicUsize;

    let (in_flight, most) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (now, peak, log) = (Arc::clone(&in_flight), Arc::clone(&most), Arc::clone(&order));
    let fetch: QuoteFetch = Arc::new(move |symbol: &str, start_ms: i64, end_ms: i64| {
        peak.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        log.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(symbol.to_string());
        std::thread::sleep(std::time::Duration::from_millis(20));
        now.fetch_sub(1, Ordering::SeqCst);
        whole_window(symbol, start_ms, end_ms)
    });
    let lane = Arc::new(TicksSpecDukascopy::with_fetch(fetch, MAX_SPAN_MS));
    let backfill = |symbol: &'static str| {
        let lane = Arc::clone(&lane);
        std::thread::spawn(move || lane.fetch_until(symbol, 0, 3 * MS_PER_HOUR - 1, &|| false))
    };
    let (first, second) = (backfill("AAAUSD"), backfill("BBBUSD"));
    assert!(first.join().expect("first").is_ok() && second.join().expect("second").is_ok());

    assert_eq!(most.load(Ordering::SeqCst), 1, "never two requests in flight");
    let order = order.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert_eq!(order.len(), 6, "three hour files each");
    assert!(
        order[..3].iter().all(|s| *s == order[0])
            && order[3..].iter().all(|s| *s == order[3])
            && order[0] != order[3],
        "the second backfill ran whole after the first: {order:?}"
    );
}

/// A backfill queued behind another stops when asked, before its turn and without a request, and a window
/// the lane refuses is answered at once instead of queueing.
#[test]
fn a_queued_backfill_stops_before_its_turn_and_a_refused_one_never_queues() {
    use std::sync::mpsc;

    let (entered, inside) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let (entered, released) = (Mutex::new(entered), Mutex::new(released));
    let fetch: QuoteFetch = Arc::new(move |symbol: &str, start_ms: i64, end_ms: i64| {
        assert_eq!(symbol, "AAAUSD", "a queued or refused fetch reached the feed");
        entered.lock().unwrap_or_else(std::sync::PoisonError::into_inner).send(()).expect("test");
        released.lock().unwrap_or_else(std::sync::PoisonError::into_inner).recv().expect("test");
        whole_window(symbol, start_ms, end_ms)
    });
    let lane = Arc::new(TicksSpecDukascopy::with_fetch(fetch, MAX_SPAN_MS));
    let holder = {
        let lane = Arc::clone(&lane);
        std::thread::spawn(move || lane.fetch_until("AAAUSD", 0, MS_PER_HOUR - 1, &|| false))
    };
    inside.recv().expect("the first backfill holds the lane");

    assert_eq!(
        lane.fetch_until("BBBUSD", 0, MS_PER_HOUR - 1, &|| true),
        Err(TicksError::Stopped("while queued behind another download"))
    );
    assert!(matches!(
        lane.fetch_until("a/b", 0, MS_PER_HOUR - 1, &|| false),
        Err(TicksError::Refused(_))
    ));

    release.send(()).expect("test");
    assert!(holder.join().expect("holder").is_ok());
}
