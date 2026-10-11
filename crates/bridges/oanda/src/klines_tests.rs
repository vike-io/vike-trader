use super::*;
use serde_json::{Value, json};
use std::assert_matches;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An obviously fake bearer token: long enough to look like one, never a real credential. Every
/// scrub test plants THIS value in the text it expects to come back masked.
const TOKEN: &str = "tok-SECRET-0123456789abcdef-fedcba9876543210";

/// The four sub-minute intervals this source adds, with the code each maps to.
const SUB_MINUTE: [(&str, &str); 4] =
    [("5s", "S5"), ("10s", "S10"), ("15s", "S15"), ("30s", "S30")];

/// The live table's intervals (the rows `crates/bridges/oanda/tests/candles.rs` pins verbatim).
const LIVE_INTERVALS: [&str; 17] = [
    "1m", "2m", "4m", "5m", "10m", "15m", "30m", "1h", "2h", "3h", "4h", "6h", "8h", "12h", "1d",
    "1w", "1mo",
];

const S5: Grain = Grain { granularity: "S5", step_s: 5 };

const URL: &str = "https://example.invalid/v3/instruments/EUR_USD/candles";

fn secret() -> Secret {
    Secret::new(TOKEN.to_string()).expect("a non-blank token")
}

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

// ── hand-made fixtures ──────────────────────────────────────────────────────────────────────────

/// One candle object exactly as the endpoint sends it under `price=M` and `UNIX` times.
fn candle(time_s: i64, complete: bool) -> Value {
    json!({
        "complete": complete,
        "volume": 7,
        "time": format!("{time_s}.000000000"),
        "mid": {"o": "1.10000", "h": "1.10100", "l": "1.09900", "c": "1.10050"},
    })
}

fn page_with(candles: Vec<Value>) -> Value {
    json!({"instrument": "EUR_USD", "granularity": "S5", "candles": candles})
}

/// A page of COMPLETE candles at `times` (seconds).
fn page_of(times: &[i64]) -> Value {
    page_with(times.iter().map(|t| candle(*t, true)).collect())
}

/// A page of exactly [`PAGE_COUNT`] complete candles, five seconds apart from `first_s`.
fn full_page(first_s: i64) -> Value {
    let times: Vec<i64> = (0..PAGE_COUNT as i64).map(|i| first_s + i * 5).collect();
    page_of(&times)
}

fn times(bars: &[Bar]) -> Vec<i64> {
    bars.iter().map(|b| b.ts / 1000).collect()
}

/// The `from=` of every request path, in order.
fn froms(paths: &[String]) -> Vec<i64> {
    paths
        .iter()
        .map(|p| {
            p.split("from=")
                .nth(1)
                .and_then(|rest| rest.split('&').next())
                .and_then(|n| n.parse().ok())
                .expect("every request carries a numeric from=")
        })
        .collect()
}

/// Drive `walk_pages` over canned pages. Returns its result and every request path it made. A
/// request the script has no page for panics, and so does a runaway walk — a loop the code under
/// test failed to stop must fail fast rather than hang the suite.
fn walk(
    grain: &Grain,
    start_ms: i64,
    end_ms: i64,
    page_count: usize,
    pages: Vec<Value>,
) -> (Result<Vec<Bar>, SourceError>, Vec<String>) {
    let mut pages: VecDeque<Value> = pages.into();
    let mut paths: Vec<String> = Vec::new();
    let out = walk_pages(grain, "EUR_USD", start_ms, end_ms, page_count, |path| {
        paths.push(path.to_string());
        assert!(paths.len() <= 12, "runaway paging: {} requests and counting", paths.len());
        Ok(pages.pop_front().expect("the walk asked for a page the script does not have"))
    });
    (out, paths)
}

// ── a scripted wire ─────────────────────────────────────────────────────────────────────────────

/// The network, the clock and the budget, replaced by a script that records what it was asked.
#[derive(Default)]
struct Scripted {
    answers: VecDeque<Attempt>,
    urls: Vec<String>,
    /// The token exposed to each request — the value the real wire would put in the header.
    tokens: Vec<String>,
    /// `pace`, `get` and `sleep`, in the order the code under test called them.
    events: Vec<&'static str>,
    sleeps: Vec<Duration>,
}

impl Scripted {
    fn answering(answers: Vec<Attempt>) -> Self {
        Self { answers: answers.into(), ..Self::default() }
    }

    fn paces(&self) -> usize {
        self.events.iter().filter(|e| **e == "pace").count()
    }
}

impl Wire for Scripted {
    fn pace(&mut self) {
        self.events.push("pace");
    }

    fn get(&mut self, url: &str, token: &Secret) -> Attempt {
        self.events.push("get");
        self.urls.push(url.to_string());
        self.tokens.push(token.expose().to_string());
        self.answers
            .pop_front()
            .expect("the code under test made a request the script has no answer for")
    }

    fn sleep(&mut self, wait: Duration) {
        self.events.push("sleep");
        self.sleeps.push(wait);
    }
}

fn ok(body: &Value) -> Attempt {
    Attempt::Response { status: 200, body: body.to_string(), retry_after: None }
}

fn status(code: u16) -> Attempt {
    Attempt::Response { status: code, body: String::new(), retry_after: None }
}

fn status_with(code: u16, body: &str) -> Attempt {
    Attempt::Response { status: code, body: body.to_string(), retry_after: None }
}

fn throttled(code: u16, retry_after_s: u64) -> Attempt {
    Attempt::Response { status: code, body: String::new(), retry_after: Some(secs(retry_after_s)) }
}

fn empty_page() -> Value {
    page_with(Vec::new())
}

fn source_with(token: &'static str) -> OandaKlines {
    OandaKlines::new(Arc::new(move || -> Result<String, HistoryTokenError> {
        Ok(token.to_string())
    }))
}

/// A source whose provider counts its calls and answers `answer`.
fn counting_source(
    calls: &Arc<AtomicUsize>,
    answer: Result<String, HistoryTokenError>,
) -> OandaKlines {
    let calls = Arc::clone(calls);
    OandaKlines::new(Arc::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        answer.clone()
    }))
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the interval vocabulary — and the decision that it is NOT the live feed's
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn history_adds_the_four_sub_minute_granularities() {
    for (interval, code) in SUB_MINUTE {
        assert_eq!(history_granularity(interval), Some(code), "{interval}");
    }
}

#[test]
fn every_other_row_is_the_live_tables_by_delegation() {
    for interval in LIVE_INTERVALS {
        assert!(granularity(interval).is_some(), "{interval} is a live row");
        assert_eq!(history_granularity(interval), granularity(interval), "{interval}");
    }
    for bad in ["1s", "7s", "45s", "3d", "", "M1", "1H"] {
        assert_eq!(history_granularity(bad), None, "{bad} must stay unsupported");
    }
}

/// **THE DECISION, pinned.** The sub-minute rows are NOT in the live table: `subscribe_bars` and the
/// daemon's `oanda_plan` derive what a live mount accepts from `granularity`, and a closed bar of a
/// live mount drives the margin-call watchdog and the drawdown latch. A history change must not widen
/// that gate as a side effect.
#[test]
fn the_live_table_still_refuses_the_sub_minute_intervals() {
    for (interval, _) in SUB_MINUTE {
        assert_eq!(
            granularity(interval),
            None,
            "{interval}: adding it to `granularity` would let a LIVE mount subscribe a bar poll \
             for it — declare history-only rows in `history_granularity`"
        );
    }
}

/// `S5` is 5 seconds, `M15` 15 minutes, `H4` 4 hours, `D` a day — so a copy-paste slip such as
/// `"5s" => "S15"` (a cursor that steps by the wrong width, silently skipping or repeating
/// candles) fails here rather than in a store.
#[test]
fn every_granularity_code_agrees_with_the_width_of_its_interval() {
    fn code_seconds(code: &str) -> Option<i64> {
        let (unit, n) = code.split_at(1);
        let n: Option<i64> = if n.is_empty() { Some(1) } else { n.parse().ok() };
        match unit {
            "S" => n,
            // A bare `M` is the MONTH granularity: no fixed width.
            "M" if code.len() > 1 => n.map(|m| m * 60),
            "H" => n.map(|h| h * 3600),
            "D" => n.map(|d| d * 86_400),
            _ => None,
        }
    }
    let intervals = SUB_MINUTE.iter().map(|(i, _)| *i).chain(LIVE_INTERVALS);
    for interval in intervals {
        let code = history_granularity(interval).expect("a mapped interval");
        match vike_model::time::interval_ms(interval) {
            Some(ms) => {
                assert_eq!(code_seconds(code), Some(ms / 1000), "{interval} -> {code}");
                let grain = grain_for(interval).expect("a fixed-width interval has a grain");
                assert_eq!(grain, Grain { granularity: code, step_s: ms / 1000 }, "{interval}");
            }
            None => assert_matches!(interval, "1w" | "1mo", "{interval} has no width"),
        }
    }
}

#[test]
fn a_week_and_a_month_are_refused_as_calendar_width() {
    for interval in ["1w", "1mo"] {
        match grain_for(interval) {
            Err(SourceError::Refused(m)) => assert!(m.contains("calendar-width"), "{m}"),
            other => panic!("{interval}: expected a refusal, got {other:?}"),
        }
    }
}

#[test]
fn an_interval_oanda_has_no_granularity_for_is_refused() {
    for interval in ["7s", "1s", "3d", "", "5S"] {
        assert_matches!(grain_for(interval), Err(SourceError::Refused(_)), "{interval:?}");
    }
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the symbol, the request and the host
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn a_symbol_may_be_either_spelling_and_nothing_that_could_steer_a_path() {
    for symbol in ["EUR_USD", "eurusd", "eur_usd", "XAU_USD", "xauusd", "SPX500_USD"] {
        assert!(instrument_for(symbol).is_ok(), "{symbol}");
    }
    assert_eq!(instrument_for("eurusd").expect("a pair"), "EUR_USD");
    for bad in [
        "",
        "..",
        "EUR_USD/../accounts",
        "EUR_USD?granularity=D",
        "EUR_USD#frag",
        "EUR USD",
        "EUR-USD",
        "EUR_USD\n",
        "%45UR_USD",
    ] {
        assert_matches!(instrument_for(bad), Err(SourceError::Refused(_)), "{bad:?}");
    }
    // Each path- or query-significant character is enough ON ITS OWN: a symbol that differs from a
    // legal one by that single character must still be refused.
    for c in ['/', '?', '#', '.', '%', '&', '=', ':', ';', '\\', ' ', '\n', '\r', '-', '@'] {
        let bad = format!("EUR_USD{c}");
        assert_matches!(instrument_for(&bad), Err(SourceError::Refused(_)), "{bad:?}");
    }
    let too_long = "A".repeat(MAX_INSTRUMENT_LEN + 1);
    assert_matches!(instrument_for(&too_long), Err(SourceError::Refused(_)));
}

/// The validated request, character for character: mid candles, an integer-second `from`, the
/// documented ceiling as `count`.
#[test]
fn the_request_is_the_validated_one() {
    assert_eq!(
        candles_path("EUR_USD", "S5", 1_104_710_400, PAGE_COUNT),
        "/v3/instruments/EUR_USD/candles?granularity=S5&price=M&from=1104710400&count=5000"
    );
}

/// The bearer credential and the UNIX datetime format. Without the second a request would get RFC
/// 3339 times, which `epoch_ms` refuses page after page — so the pair is pinned, off the network.
#[test]
fn every_request_carries_the_bearer_token_and_asks_for_unix_times() {
    let headers = request_headers(&secret());
    assert_eq!(headers.len(), 2);
    assert!(headers.iter().any(|(k, v)| *k == "Authorization" && *v == format!("Bearer {TOKEN}")));
    assert!(headers.iter().any(|(k, v)| *k == "Accept-Datetime-Format" && v == "UNIX"));
}

#[test]
fn the_host_is_the_practice_host_and_never_the_live_one() {
    assert_eq!(history_base(), "https://api-fxpractice.oanda.com");
    assert!(!history_base().contains("fxtrade"));
}

#[test]
fn a_fetch_sends_the_validated_request_to_the_practice_host_with_the_token() {
    let mut wire = Scripted::answering(vec![ok(&empty_page())]);
    let out = source_with(TOKEN)
        .fetch_over(&mut wire, "eurusd", "5s", 1_104_710_400_000, 1_104_800_000_000)
        .expect("an empty page is an empty window");
    assert!(out.is_empty());
    assert_eq!(
        wire.urls,
        ["https://api-fxpractice.oanda.com/v3/instruments/EUR_USD/candles\
             ?granularity=S5&price=M&from=1104710400&count=5000"]
    );
    assert_eq!(wire.tokens, [TOKEN]);
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// decoding one page
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn a_time_is_parsed_to_exact_milliseconds_by_integer_arithmetic() {
    assert_eq!(epoch_ms("1478012400.000000000"), Some(1_478_012_400_000));
    assert_eq!(epoch_ms("1478012400.123456789"), Some(1_478_012_400_123));
    assert_eq!(epoch_ms("1478012400.5"), Some(1_478_012_400_500));
    assert_eq!(epoch_ms("1478012400"), Some(1_478_012_400_000));
    assert_eq!(epoch_ms("1478012400.007"), Some(1_478_012_400_007));
    // Just UNDER a millisecond boundary the digits floor to 6 ms. (`secs * 1000.0` on the nearest
    // double rounds this same input UP to 7 — see the bar test below.)
    assert_eq!(epoch_ms("1478012400.006999999"), Some(1_478_012_400_006));
    for bad in ["", ".5", "-1.0", "2016-11-01T00:00:00.000000000Z", "12a.0", "1.2x", "1e9"] {
        assert_eq!(epoch_ms(bad), None, "{bad:?}");
    }
}

#[test]
fn a_page_keeps_the_complete_candles_and_reports_a_forming_last_one() {
    let page =
        decode_page(&page_with(vec![candle(100, true), candle(105, true), candle(110, false)]))
            .expect("a well-formed page");
    assert_eq!(times(&page.bars), [100, 105]);
    assert_eq!((page.last_ms, page.last_forming, page.total), (Some(110_000), true, 3));
}

/// The time sits just under a millisecond boundary on purpose. `bar_from_candle` parses it as a float
/// and its `secs * 1000.0` rounds it UP to 7 ms; the integer parse this module substitutes floors it
/// to 6 — so this is the input on which the two routes differ, and it pins which one supplies
/// `bar.ts`. (A whole-second or millisecond-precision time cannot tell them apart at this epoch.)
#[test]
fn a_bar_carries_the_mid_prices_the_tick_count_and_the_exact_time() {
    let v = page_with(vec![json!({
        "complete": true, "volume": 42, "time": "1478012400.006999999",
        "mid": {"o": "1.09000", "h": "1.09500", "l": "1.08900", "c": "1.09300"},
    })]);
    let page = decode_page(&v).expect("a well-formed page");
    let bar = &page.bars[0];
    assert_eq!(bar.ts, 1_478_012_400_006);
    assert_eq!((bar.open, bar.high, bar.low, bar.close), (1.09, 1.095, 1.089, 1.093));
    assert_eq!(bar.volume, 42.0);
    assert_eq!((bar.funding, bar.bid, bar.ask, bar.symbol.as_deref()), (None, None, None, None));
}

/// A candle that does not SAY it is complete is not trusted as complete: it is dropped, and as the
/// last of its page it is the live edge.
#[test]
fn a_candle_without_a_complete_flag_is_not_trusted_as_complete() {
    let unflagged = json!({
        "volume": 1, "time": "105.000000000",
        "mid": {"o": "1.1", "h": "1.1", "l": "1.1", "c": "1.1"},
    });
    let page = decode_page(&page_with(vec![candle(100, true), unflagged])).expect("decodes");
    assert_eq!(times(&page.bars), [100]);
    assert!(page.last_forming, "an unflagged candle is treated as still forming");
}

#[test]
fn an_empty_page_decodes_to_nothing_and_no_last_time() {
    let page = decode_page(&empty_page()).expect("an empty page is legal");
    assert!(page.bars.is_empty());
    assert_eq!((page.last_ms, page.last_forming, page.total), (None, false, 0));
}

/// A forming candle is dropped without being decoded, so a missing `mid` on one is not an error —
/// only a COMPLETE candle has to be usable.
#[test]
fn a_forming_candle_need_not_carry_prices() {
    let forming = json!({"complete": false, "volume": 1, "time": "105.000000000"});
    let page = decode_page(&page_with(vec![candle(100, true), forming])).expect("decodes");
    assert_eq!(times(&page.bars), [100]);
    assert!(page.last_forming);
}

/// Strict where a shortcut would store something wrong. Each of these used to be a silent zero,
/// a skipped row or a NaN in the store.
#[test]
fn a_malformed_page_fails_loudly_instead_of_storing_something_wrong() {
    let no_mid = json!({"complete": true, "volume": 1, "time": "100.000000000"});
    let bad_price = json!({
        "complete": true, "volume": 1, "time": "100.000000000",
        "mid": {"o": "abc", "h": "1", "l": "1", "c": "1"},
    });
    let nan_price = json!({
        "complete": true, "volume": 1, "time": "100.000000000",
        "mid": {"o": "NaN", "h": "1", "l": "1", "c": "1"},
    });
    let rfc3339 = json!({
        "complete": true, "volume": 1, "time": "1970-01-01T00:01:40.000000000Z",
        "mid": {"o": "1", "h": "1", "l": "1", "c": "1"},
    });
    let cases = [
        ("no candles array", json!({"instrument": "EUR_USD"})),
        ("candles not an array", json!({"candles": "none"})),
        ("a candle with no time", page_with(vec![json!({"complete": true})])),
        ("an RFC 3339 time", page_with(vec![rfc3339])),
        ("a complete candle with no mid", page_with(vec![no_mid])),
        ("an unparseable price", page_with(vec![bad_price])),
        ("a NaN price", page_with(vec![nan_price])),
        ("candles out of order", page_of(&[105, 100])),
        ("a repeated candle inside one page", page_of(&[100, 100])),
    ];
    for (what, body) in cases {
        assert!(decode_page(&body).is_err(), "{what} must fail the page, not pass it");
    }
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the walk over pages
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn three_pages_are_walked_from_each_last_candle_plus_one_step() {
    let pages =
        vec![page_of(&[1000, 1005, 1010]), page_of(&[1015, 1020, 1025]), page_of(&[1030, 1035])];
    let (out, paths) = walk(&S5, 1_000_000, 2_000_000_000, 3, pages);
    assert_eq!(times(&out.expect("three pages")), [1000, 1005, 1010, 1015, 1020, 1025, 1030, 1035]);
    assert_eq!(froms(&paths), [1000, 1015, 1030], "each `from` is the last candle plus one step");
    assert!(paths.iter().all(|p| p.contains("count=3")), "{paths:?}");
}

/// The venue's `from` COVERS a candle, so a cursor that lands inside one repeats it: here every page
/// after the first opens on the previous page's last candle. Each candle must still appear once.
#[test]
fn a_seam_duplicate_is_dropped_once_and_never_thins_the_rest() {
    let pages =
        vec![page_of(&[1000, 1005, 1010]), page_of(&[1010, 1015, 1020]), page_of(&[1020, 1025])];
    let (out, paths) = walk(&S5, 1_000_000, 2_000_000_000, 3, pages);
    assert_eq!(times(&out.expect("three pages")), [1000, 1005, 1010, 1015, 1020, 1025]);
    assert_eq!(paths.len(), 3);
}

/// A full page whose last candle is still forming is the live edge: nothing follows it, so the walk
/// must not ask again (a `from` past "now" is a request for nothing).
#[test]
fn a_forming_last_candle_is_dropped_and_ends_the_walk_even_on_a_full_page() {
    let page = page_with(vec![candle(1000, true), candle(1005, true), candle(1010, false)]);
    let (out, paths) = walk(&S5, 1_000_000, 2_000_000_000, 3, vec![page]);
    assert_eq!(times(&out.expect("one page")), [1000, 1005]);
    assert_eq!(paths.len(), 1, "nothing follows the live edge");
}

#[test]
fn a_short_page_ending_in_a_forming_candle_keeps_the_complete_ones() {
    let page = page_with(vec![candle(1000, true), candle(1005, false)]);
    let (out, paths) = walk(&S5, 1_000_000, 2_000_000_000, 5, vec![page]);
    assert_eq!(times(&out.expect("one page")), [1000]);
    assert_eq!(paths.len(), 1);
}

#[test]
fn an_empty_first_page_is_an_empty_window() {
    let (out, paths) = walk(&S5, 1_000_000, 9_000_000, 3, vec![empty_page()]);
    assert!(out.expect("an empty page ends the walk").is_empty());
    assert_eq!(paths.len(), 1);
}

#[test]
fn an_empty_page_after_a_full_one_ends_the_walk_with_what_it_has() {
    let pages = vec![page_of(&[1000, 1005, 1010]), empty_page()];
    let (out, paths) = walk(&S5, 1_000_000, 2_000_000_000, 3, pages);
    assert_eq!(times(&out.expect("two pages")), [1000, 1005, 1010]);
    assert_eq!(froms(&paths), [1000, 1015]);
}

/// Inclusive at both ends, and the candle that COVERS `from` — which precedes the window — is
/// dropped along with the one past its end.
#[test]
fn the_window_is_inclusive_at_both_ends_and_drops_what_lies_outside() {
    let pages = vec![page_of(&[1000, 1005, 1010, 1015, 1020])];
    let (out, paths) = walk(&S5, 1_005_000, 1_015_000, 5, pages);
    assert_eq!(times(&out.expect("one page")), [1005, 1010, 1015]);
    assert_eq!(froms(&paths), [1005]);
    assert_eq!(paths.len(), 1, "a last candle past the window's end stops the walk");
}

#[test]
fn a_last_candle_exactly_at_the_end_stops_the_walk() {
    let (out, paths) = walk(&S5, 1_000_000, 1_010_000, 3, vec![page_of(&[1000, 1005, 1010])]);
    assert_eq!(times(&out.expect("one page")), [1000, 1005, 1010]);
    assert_eq!(paths.len(), 1);
}

#[test]
fn a_start_inside_a_second_rounds_down_and_a_negative_start_clamps_to_the_epoch() {
    let (_, paths) = walk(&S5, 1_999, 5_000, 3, vec![empty_page()]);
    assert_eq!(froms(&paths), [1]);
    let (_, paths) = walk(&S5, -5_000, 5_000, 3, vec![empty_page()]);
    assert_eq!(froms(&paths), [0]);
}

#[test]
fn the_cursor_steps_by_the_intervals_own_width() {
    for (interval, step) in [("1m", 60), ("15m", 900), ("4h", 14_400), ("1d", 86_400)] {
        let grain = grain_for(interval).expect("a fixed-width interval");
        let pages = vec![page_of(&[step, 2 * step, 3 * step]), empty_page()];
        let (_, paths) = walk(&grain, step * 1000, 9_000_000_000, 3, pages);
        assert_eq!(froms(&paths), [step, 4 * step], "{interval}");
    }
}

/// A venue that ignores `from` and serves the same full page forever must FAIL the fetch — not
/// loop, and not return the thinned result as if it were the window.
#[test]
fn a_cursor_that_does_not_advance_is_an_error_not_a_loop() {
    let pages = (0..12).map(|_| page_of(&[1000, 1005, 1010])).collect();
    let (out, paths) = walk(&S5, 1_000_000, 9_000_000_000, 3, pages);
    match out {
        Err(SourceError::Fetch(m)) => assert!(m.contains("no progress"), "{m}"),
        other => panic!("expected a no-progress error, got {other:?}"),
    }
    assert_eq!(paths.len(), 2);
}

#[test]
fn a_failed_request_is_a_fetch_error_naming_the_page() {
    let out = walk_pages(&S5, "EUR_USD", 1_000_000, 2_000_000, 3, |_| {
        Err(OandaApiError { status: 401, message: "no".to_string() })
    });
    match out {
        Err(SourceError::Fetch(m)) => assert!(m.contains("401") && m.contains("from=1000"), "{m}"),
        other => panic!("expected a fetch error, got {other:?}"),
    }
}

#[test]
fn a_malformed_page_is_a_fetch_error_naming_the_page() {
    let bad = json!({"candles": "none"});
    let (out, _) = walk(&S5, 1_000_000, 2_000_000, 3, vec![bad]);
    match out {
        Err(SourceError::Fetch(m)) => assert!(m.contains("from=1000"), "{m}"),
        other => panic!("expected a fetch error, got {other:?}"),
    }
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// one request: the retry policy
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn a_429_then_a_success_waits_the_first_slot_once_and_no_more() {
    let mut wire = Scripted::answering(vec![status(429), ok(&empty_page())]);
    let value = request_with(&secret(), URL, &mut wire).expect("the retry succeeds");
    assert_eq!(value["candles"], json!([]));
    assert_eq!(wire.sleeps, [secs(1)]);
    assert_eq!(
        wire.events,
        ["pace", "get", "sleep", "pace", "get"],
        "a slot before every attempt, a wait only between attempts, none after the success"
    );
}

/// The schedule the downloader ran: doubling from a second to a minute, eight attempts. Pinned
/// verbatim — a "tidied" schedule that no longer outlasts a throttle is exactly the regression.
#[test]
fn the_whole_schedule_is_waited_out_and_the_last_failure_is_reported() {
    let mut wire = Scripted::answering((0..8).map(|_| status(503)).collect());
    let err = request_with(&secret(), URL, &mut wire).expect_err("eight failures exhaust it");
    assert_eq!(err.status, 503);
    assert!(
        err.message.contains("HTTP 503") && err.message.contains("after 8 attempts"),
        "{}",
        err.message
    );
    assert_eq!(
        wire.sleeps,
        [secs(1), secs(2), secs(4), secs(8), secs(16), secs(32), secs(60)],
        "seven waits — and none after the last failure, which has nothing left to wait for"
    );
    assert_eq!(wire.urls.len(), 8);
    assert_eq!(wire.paces(), 8, "every attempt takes a budget slot, retries included");
}

/// A `Retry-After` lengthens a slot, is capped, and never shortens one.
#[test]
fn a_retry_after_lengthens_a_wait_but_is_capped_and_never_shortens_one() {
    let mut wire = Scripted::answering(vec![
        throttled(429, 25),
        throttled(429, 3600),
        throttled(429, 1),
        ok(&empty_page()),
    ]);
    request_with(&secret(), URL, &mut wire).expect("the fourth attempt succeeds");
    // 25 s beats the 1 s slot; an hour is capped at two minutes, which beats the 2 s slot; 1 s
    // cannot shorten the 4 s slot.
    assert_eq!(wire.sleeps, [secs(25), RETRY_AFTER_CAP, secs(4)]);
}

#[test]
fn every_server_side_status_is_transient_and_is_retried() {
    for code in [429u16, 500, 501, 502, 503, 504, 599] {
        let mut wire = Scripted::answering(vec![status(code), ok(&empty_page())]);
        request_with(&secret(), URL, &mut wire).unwrap_or_else(|e| panic!("{code}: {e}"));
        assert_eq!(wire.sleeps, [secs(1)], "{code}");
    }
}

#[test]
fn a_hard_client_error_fails_at_once_with_oandas_own_message() {
    let body = json!({"errorMessage": "Insufficient authorization to perform request."});
    for code in [400u16, 401, 403, 404, 405, 409] {
        let mut wire = Scripted::answering(vec![status_with(code, &body.to_string())]);
        let err = request_with(&secret(), URL, &mut wire).expect_err("a hard failure");
        assert_eq!(err.status, code);
        assert_eq!(err.message, "Insufficient authorization to perform request.");
        assert_eq!(wire.urls.len(), 1, "{code}: retried a hard failure");
        assert!(wire.sleeps.is_empty(), "{code}: waited on a hard failure");
    }
}

#[test]
fn a_transport_failure_is_retried_like_a_server_error() {
    let mut wire =
        Scripted::answering(vec![Attempt::Failed("connection reset".into()), ok(&empty_page())]);
    request_with(&secret(), URL, &mut wire).expect("the retry succeeds");
    assert_eq!(wire.sleeps, [secs(1)]);

    let mut wire =
        Scripted::answering((0..8).map(|_| Attempt::Failed("timed out".into())).collect());
    let err = request_with(&secret(), URL, &mut wire).expect_err("eight failures exhaust it");
    assert_eq!(err.status, 0, "a transport failure carries no HTTP status");
    assert!(err.message.contains("timed out") && err.message.contains("after 8 attempts"));
}

#[test]
fn a_200_that_is_not_json_is_retried_as_a_garbled_body() {
    let mut wire = Scripted::answering(vec![status_with(200, "<html>upstream"), ok(&empty_page())]);
    request_with(&secret(), URL, &mut wire).expect("the retry succeeds");
    assert_eq!(wire.sleeps, [secs(1)]);
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the token never reaches an error
// ══════════════════════════════════════════════════════════════════════════════════════════════

/// A gateway that echoes the request's `Authorization` header into its 401 body is the realistic
/// leak: the bearer text is in the wire response.
#[test]
fn a_401_that_echoes_the_bearer_header_is_masked() {
    let body = json!({"errorMessage": format!("Invalid Authorization header: Bearer {TOKEN}")});
    let mut wire = Scripted::answering(vec![status_with(401, &body.to_string())]);
    let err = request_with(&secret(), URL, &mut wire).expect_err("a hard failure");
    assert_eq!(err.status, 401);
    assert!(!err.message.contains(TOKEN), "the token leaked: {}", err.message);
    assert!(!err.to_string().contains(TOKEN), "Display leaked the token");
    assert!(!format!("{err:?}").contains(TOKEN), "Debug leaked the token");
    assert!(err.message.contains("***"), "{}", err.message);
}

/// Masking runs BEFORE the 200-character cut. Cut first and a token straddling the cut leaves a
/// readable prefix that no later masking can recognise.
#[test]
fn a_token_straddling_the_truncation_point_leaves_no_prefix() {
    let body = format!("{}{TOKEN}{}", "a".repeat(190), "b".repeat(50));
    let mut wire = Scripted::answering(vec![status_with(400, &body)]);
    let err = request_with(&secret(), URL, &mut wire).expect_err("a hard failure");
    assert!(!err.message.contains(&TOKEN[..10]), "a token prefix survived: {}", err.message);
    assert!(err.message.contains("***"), "{}", err.message);
}

#[test]
fn a_transport_error_that_names_the_token_is_masked() {
    let leak = format!("tls: rejected header value {TOKEN}");
    let mut wire = Scripted::answering((0..8).map(|_| Attempt::Failed(leak.clone())).collect());
    let err = request_with(&secret(), URL, &mut wire).expect_err("eight failures exhaust it");
    assert!(!err.message.contains(TOKEN), "the token leaked: {}", err.message);
    assert!(err.message.contains("***"), "{}", err.message);
}

#[test]
fn a_throttle_body_that_names_the_token_is_masked_in_the_exhaustion_message() {
    let body = format!("rate limited for key {TOKEN}");
    let mut wire = Scripted::answering((0..8).map(|_| status_with(429, &body)).collect());
    let err = request_with(&secret(), URL, &mut wire).expect_err("eight throttles exhaust it");
    assert_eq!(err.status, 429);
    assert!(!err.message.contains(TOKEN), "the token leaked: {}", err.message);
}

/// The whole fetch, end to end: the token reaches the wire and reaches nothing the caller sees.
#[test]
fn a_fetch_that_fails_cannot_carry_the_token_into_its_source_error() {
    let body = json!({"errorMessage": format!("bad key {TOKEN}")});
    let mut wire = Scripted::answering(vec![status_with(401, &body.to_string())]);
    let out = source_with(TOKEN).fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 2_000_000);
    match out {
        Err(SourceError::Fetch(m)) => {
            assert!(m.contains("401"), "{m}");
            assert!(!m.contains(TOKEN), "the token leaked into a SourceError: {m}");
        }
        other => panic!("expected a fetch error, got {other:?}"),
    }
    assert_eq!(wire.tokens, [TOKEN], "the token still reached the wire");
    assert_eq!(wire.urls.len(), 1, "a 401 is not retried");
}

#[test]
fn a_secret_prints_a_mask_and_a_blank_one_does_not_exist() {
    assert_eq!(format!("{:?}", secret()), "Secret(***)");
    assert!(Secret::new(String::new()).is_none());
    assert!(Secret::new("  \t ".to_string()).is_none());
    assert_eq!(Secret::new("  tok  ".to_string()).expect("trimmed").expose(), "tok");
}

#[test]
fn a_source_prints_nothing_that_leads_to_a_credential() {
    assert_eq!(format!("{:?}", source_with(TOKEN)), "OandaKlines { .. }");
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the token provider
// ══════════════════════════════════════════════════════════════════════════════════════════════

#[test]
fn the_provider_is_called_once_per_fetch_not_once_per_page() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = counting_source(&calls, Ok(TOKEN.to_string()));
    let mut wire = Scripted::answering(vec![
        ok(&full_page(1000)),
        ok(&full_page(26_000)),
        ok(&page_of(&[51_000, 51_005])),
    ]);
    let out = source
        .fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 9_000_000_000)
        .expect("three pages");
    assert_eq!(out.len(), 2 * PAGE_COUNT + 2);
    assert_eq!(froms(&wire.urls), [1000, 26_000, 51_000]);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the token is read once for the whole fetch");
    assert_eq!(wire.paces(), 3);
}

#[test]
fn a_missing_token_is_refused_before_anything_is_asked() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = counting_source(&calls, Err(HistoryTokenError::NotConfigured));
    let mut wire = Scripted::default();
    let out = source.fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 2_000_000);
    assert_eq!(out, Err(SourceError::Refused(HistoryTokenError::NotConfigured.to_string())));
    assert!(wire.events.is_empty(), "nothing may reach the wire without a token");
}

#[test]
fn an_unreadable_store_is_refused_in_its_own_words() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = counting_source(&calls, Err(HistoryTokenError::StoreUnreadable));
    let mut wire = Scripted::default();
    let out = source.fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 2_000_000);
    assert_eq!(out, Err(SourceError::Refused(HistoryTokenError::StoreUnreadable.to_string())));
    assert!(wire.events.is_empty());
}

#[test]
fn a_blank_token_is_no_token() {
    let mut wire = Scripted::default();
    let out = source_with("   ").fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 2_000_000);
    assert_eq!(out, Err(SourceError::Refused(HistoryTokenError::NotConfigured.to_string())));
    assert!(wire.events.is_empty());
}

#[test]
fn the_token_is_trimmed_before_it_is_sent() {
    let mut wire = Scripted::answering(vec![ok(&empty_page())]);
    source_with("  tok-abc  ")
        .fetch_over(&mut wire, "EUR_USD", "5s", 1_000_000, 2_000_000)
        .expect("an empty window");
    assert_eq!(wire.tokens, ["tok-abc"]);
}

/// A refusal is a statement that the venue was never asked — so it comes before the token is read,
/// and before anything reaches the wire.
#[test]
fn every_refusal_comes_before_the_token_is_read() {
    let cases = [
        ("EUR_USD", "7s"),
        ("EUR_USD", "1w"),
        ("EUR_USD", "1mo"),
        ("EUR_USD/../accounts", "5s"),
        ("", "5s"),
    ];
    for (symbol, interval) in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let source = counting_source(&calls, Ok(TOKEN.to_string()));
        let mut wire = Scripted::default();
        let out = source.fetch_over(&mut wire, symbol, interval, 1_000_000, 2_000_000);
        assert_matches!(out, Err(SourceError::Refused(_)), "{symbol:?}@{interval}: {out:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "{symbol:?}@{interval} read the token");
        assert!(wire.events.is_empty(), "{symbol:?}@{interval} touched the wire");
    }
}

#[test]
fn an_inverted_window_is_empty_and_asks_nothing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = counting_source(&calls, Ok(TOKEN.to_string()));
    let mut wire = Scripted::default();
    let out = source.fetch_over(&mut wire, "EUR_USD", "5s", 2_000_000, 1_000_000);
    assert_eq!(out, Ok(Vec::new()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(wire.events.is_empty());
}

#[test]
fn the_missing_token_message_names_the_practice_key_and_no_other() {
    let names = oanda_history_token_names();
    assert_eq!(names.len(), 1, "one practice key: {names:?}");
    let text = HistoryTokenError::NotConfigured.to_string();
    assert!(text.contains(&names[0]), "{text}");
    assert!(text.contains("vike-cli secrets set"), "{text}");
    // The names that must NOT appear are composed from the crate's own naming site, never spelled:
    // an environment-variable-shaped fragment in a string literal reads as an environment read to
    // `settings_registry`'s harvest, and it has already reddened this file once.
    let (live_key, live_account) = crate::oanda_env_var_names(Environment::Live);
    let (_, practice_account) = crate::oanda_env_var_names(Environment::Demo);
    for wrong in [live_key.as_str(), live_account.as_str(), practice_account.as_str()] {
        assert!(!text.contains(wrong), "{wrong} in: {text}");
    }
    assert!(HistoryTokenError::StoreUnreadable.to_string().contains("could not be read"));
}

#[test]
fn the_trait_method_refuses_before_it_reaches_the_network() {
    let calls = Arc::new(AtomicUsize::new(0));
    let source = counting_source(&calls, Ok(TOKEN.to_string()));
    let out = source.fetch("EUR_USD", "7s", 0, 1);
    assert_matches!(out, Err(SourceError::Refused(_)), "{out:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn the_source_is_a_send_sync_kline_source_under_the_oanda_venue_id() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<OandaKlines>();
    let source: Box<dyn KlineSource> = Box::new(source_with(TOKEN));
    assert_eq!(
        source.venue(),
        "oanda",
        "the store partition — a rename orphans every stored series"
    );
}

// ══════════════════════════════════════════════════════════════════════════════════════════════
// the budget
// ══════════════════════════════════════════════════════════════════════════════════════════════

/// At most twenty requests in any one second: a fresh gate built by the one function that spells the
/// budget admits exactly twenty in a burst and refuses the rest.
#[test]
fn the_budget_is_twenty_requests_a_second() {
    let gate = new_history_gate();
    let admitted = (0..100).filter(|_| gate.try_proceed()).count();
    assert_eq!(admitted, 20);
}

/// The real wire paces on the ONE process-wide gate and rides the ONE shared pool — so concurrent
/// fetches split a single budget instead of each spending its own.
#[test]
fn the_real_wire_shares_one_gate_and_one_pool_per_process() {
    let wire = LiveWire::shared();
    assert!(std::ptr::eq(wire.gate, history_gate()));
    assert!(std::ptr::eq(wire.agent, history_agent()));
    assert!(std::ptr::eq(LiveWire::shared().gate, wire.gate));
}
