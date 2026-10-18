//! Dukascopy FX source — keyless deep tick history (bid/ask, back to ~2003), history-only (T+1 lag).
//!
//! Port of the Python `dukascopy_source`. Downloads the public per-UTC-hour `.bi5` tick archives (no
//! key), LZMA-decodes them, and aggregates ticks to [`Bar`] OHLC at any step. The file FORMAT — the
//! 20-byte record, [`Tick`], and the decoder — is `vike-bi5`'s (`crates/vike-bi5/src/tick.rs`); this
//! file keeps the FETCH: the URL, the retrying download and the tick-to-bar aggregation. Prices are
//! integer "points", divided by 10^digits (5 for most pairs, 3 for JPY-quoted: [`point_divisor`]).
//! OHLC uses the bid/ask **mid**; `volume` is the tick count (FX has no true volume). The pure parts
//! are unit-tested; only `fetch_hour` does network I/O.

use std::io::Read;
use std::time::Duration;

use vike_bi5::{HOUR_MS, Tick, decode_ticks, decompress, tick_to_quote};
use vike_marketdata::{Bar, QuoteTick};

const DATAFEED: &str = "https://datafeed.dukascopy.com/datafeed";

/// Price scale for a pair: `1e3` for JPY-quoted (3 digits), else `1e5` (5 digits).
pub fn point_divisor(symbol: &str) -> f64 {
    if symbol.to_ascii_uppercase().ends_with("JPY") { 1e3 } else { 1e5 }
}

/// Aggregate ticks into OHLC bars of `step_ms`, using the bid/ask mid. `volume` = tick count in the
/// bucket. Assumes ticks ascending by `ts`; emits bars ascending.
pub fn ticks_to_bars(ticks: &[Tick], step_ms: i64) -> Vec<Bar> {
    use std::collections::BTreeMap;
    // per bucket: (open, high, low, close, count)
    let mut buckets: BTreeMap<i64, (f64, f64, f64, f64, f64)> = BTreeMap::new();
    for t in ticks {
        let bucket = t.ts.div_euclid(step_ms) * step_ms;
        let mid = (t.bid + t.ask) / 2.0;
        buckets
            .entry(bucket)
            .and_modify(|b| {
                b.1 = b.1.max(mid);
                b.2 = b.2.min(mid);
                b.3 = mid;
                b.4 += 1.0;
            })
            .or_insert((mid, mid, mid, mid, 1.0));
    }
    buckets
        .into_iter()
        .map(|(ts, (open, high, low, close, count))| Bar {
            ts,
            open,
            high,
            low,
            close,
            volume: count,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        })
        .collect()
}

/// Build the `.bi5` URL for a UTC hour. Month is 0-indexed (January = 00), matching Dukascopy.
pub fn hour_url(symbol: &str, hour_start_ms: i64) -> String {
    use chrono::{Datelike, Timelike};
    let dt = chrono::DateTime::from_timestamp_millis(hour_start_ms).unwrap_or_default();
    format!(
        "{DATAFEED}/{}/{:04}/{:02}/{:02}/{:02}h_ticks.bi5",
        symbol.to_ascii_uppercase(),
        dt.year(),
        dt.month0(),
        dt.day(),
        dt.hour(),
    )
}

/// How long to wait before each RETRY of one hour's download — four waits, so five attempts, about
/// 74 s of patience in all.
///
/// ⚠ **Sized to outlast a burst, because that is how this CDN fails.** The schedule was 0.1, 0.2,
/// 0.4 and 0.8 s — 1.5 s in all — and MEASURED on 2026-09-30 it never outlasted anything: the feed
/// answered in runs of instant 503s, connection resets and 30 s timeouts that lasted seconds to
/// minutes, and a day-sized chunk (24 hourly downloads that must all succeed before it is stored)
/// got through on one attempt in six. Retries a second apart land inside the same burst as the
/// failure they retry.
const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(4),
    Duration::from_secs(10),
    Duration::from_secs(20),
    Duration::from_secs(40),
];

/// The longest a server-stated `Retry-After` is honoured for: a confused header must not park a
/// backfill for an hour.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// What one download attempt came back with — the seam [`fetch_hour_with`] retries over, so the
/// retry policy is testable with neither a network nor a clock.
enum Attempt {
    /// The server answered: its status, the body (read only for a 2xx) and the wait its
    /// `Retry-After` header stated in seconds, if it stated one.
    Response { status: u16, body: Vec<u8>, retry_after: Option<Duration> },
    /// No usable answer at all — DNS, connect, TLS, a reset, a timeout, a body cut off mid-read.
    Failed(String),
}

/// [`fetch_hour`]'s retry policy over an injected attempt and sleep. A 404 is an absent hour and a
/// non-transient status fails at once; a 429/5xx or a failed attempt waits out the next of
/// [`RETRY_DELAYS`] — lengthened, never shortened, to a `Retry-After` up to [`RETRY_AFTER_CAP`] —
/// and tries again. The last failure is not slept after: there is nothing left to wait for.
fn fetch_hour_with(
    mut attempt: impl FnMut() -> Attempt,
    mut sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, String> {
    let mut last_err = String::new();
    for delay in RETRY_DELAYS.iter().map(Some).chain(std::iter::once(None)) {
        let mut stated = Duration::ZERO;
        match attempt() {
            Attempt::Response { status: 404, .. } => return Ok(None),
            Attempt::Response { status, body, .. } if (200..300).contains(&status) => {
                return Ok(Some(body));
            }
            Attempt::Response { status, retry_after, .. } => {
                if !matches!(status, 429 | 500 | 502 | 503 | 504) {
                    return Err(format!("dukascopy {status}"));
                }
                last_err = format!("dukascopy {status}");
                stated = retry_after.unwrap_or(Duration::ZERO).min(RETRY_AFTER_CAP);
            }
            Attempt::Failed(e) => last_err = e,
        }
        if let Some(delay) = delay {
            sleep((*delay).max(stated));
        }
    }
    Err(format!("dukascopy: {last_err} (after retries)"))
}

/// Download one hour's `.bi5` — `Ok(None)` when the hour is absent (404 = weekend / pre-listing /
/// not-yet-published). Retries transient CDN failures (429/5xx/network) on the [`RETRY_DELAYS`]
/// schedule — spaced seconds apart, not the fraction of a second it once was, because the CDN's
/// failures arrive in bursts (see that constant).
pub fn fetch_hour(symbol: &str, hour_start_ms: i64) -> Result<Option<Vec<u8>>, String> {
    let url = hour_url(symbol, hour_start_ms);
    let agent = vike_bridge_core::http::blocking_agent();
    // One real attempt. A closure rather than a fn because the agent's type is `ureq`'s and this
    // crate names no `ureq` — it takes its agent from `vike_bridge_core::http`.
    let attempt = || match agent
        .get(&url)
        // Dukascopy's CDN prefers a browser-ish UA
        .header("User-Agent", "Mozilla/5.0 (vike-trader-rust forex source)")
        .call()
    {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut body = Vec::new();
            if (200..300).contains(&status)
                && let Err(e) = resp.body_mut().as_reader().read_to_end(&mut body)
            {
                return Attempt::Failed(format!("read: {e}"));
            }
            Attempt::Response { status, body, retry_after }
        }
        Err(e) => Attempt::Failed(format!("network: {e}")),
    };
    fetch_hour_with(attempt, std::thread::sleep)
}

/// Fetch tick history for `[start_ms, end_ms]` (hour by hour, skipping absent/empty hours).
pub fn fetch_ticks_range(symbol: &str, start_ms: i64, end_ms: i64) -> Result<Vec<Tick>, String> {
    let divisor = point_divisor(symbol);
    let mut out = Vec::new();
    let mut hour = start_ms.div_euclid(HOUR_MS) * HOUR_MS;
    while hour <= end_ms {
        if let Some(blob) = fetch_hour(symbol, hour)?
            && !blob.is_empty()
        {
            for t in decode_ticks(&decompress(&blob)?, hour, divisor) {
                if start_ms <= t.ts && t.ts <= end_ms {
                    out.push(t);
                }
            }
        }
        hour += HOUR_MS;
    }
    Ok(out)
}

/// Fetch tick history and aggregate to `step_ms` bars. History-only (T+1 publish lag).
pub fn fetch_bars_range(
    symbol: &str,
    step_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<Bar>, String> {
    Ok(ticks_to_bars(&fetch_ticks_range(symbol, start_ms, end_ms)?, step_ms))
}

/// [`fetch_ticks_range`], mapped to the store's quote row by [`tick_to_quote`]. Network I/O (the
/// public `.bi5` CDN); no store — the append is `vike-backfill`'s.
pub fn fetch_quotes_range(
    symbol: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<QuoteTick>, String> {
    Ok(fetch_ticks_range(symbol, start_ms, end_ms)?
        .iter()
        .map(|t| tick_to_quote(t, symbol))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divisor_jpy_vs_others() {
        assert_eq!(point_divisor("USDJPY"), 1e3);
        assert_eq!(point_divisor("eurjpy"), 1e3);
        assert_eq!(point_divisor("EURUSD"), 1e5);
        assert_eq!(point_divisor("gbpusd"), 1e5);
    }

    #[test]
    fn hour_url_is_zero_indexed_month() {
        // 2020-01-02 03:00 UTC → epoch ms. January is month 00.
        let ms = 1_577_934_000_000; // 2020-01-02T03:00:00Z
        let u = hour_url("eurusd", ms);
        assert_eq!(u, "https://datafeed.dukascopy.com/datafeed/EURUSD/2020/00/02/03h_ticks.bi5");
    }

    #[test]
    fn ticks_aggregate_to_ohlc_on_mid() {
        // three ticks in the same 60s bucket, one in the next.
        let mk = |ts, bid, ask| Tick { ts, bid, ask, bid_vol: 0.0, ask_vol: 0.0 };
        let ticks = vec![
            mk(0, 1.0, 1.2),      // mid 1.1 (open)
            mk(1_000, 1.2, 1.4),  // mid 1.3 (high)
            mk(2_000, 0.9, 1.1),  // mid 1.0 (low, close of bucket 0)
            mk(60_000, 2.0, 2.0), // mid 2.0 (next bucket)
        ];
        let bars = ticks_to_bars(&ticks, 60_000);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].ts, 0);
        assert!((bars[0].open - 1.1).abs() < 1e-9);
        assert!((bars[0].high - 1.3).abs() < 1e-9);
        assert!((bars[0].low - 1.0).abs() < 1e-9);
        assert!((bars[0].close - 1.0).abs() < 1e-9);
        assert_eq!(bars[0].volume, 3.0); // tick count
        assert_eq!(bars[1].ts, 60_000);
        assert_eq!(bars[1].volume, 1.0);
    }

    // ── the retry policy, over a scripted network: no test sleeps or dials ──────────────────────

    /// Runs the policy against `script` — one answer per attempt, in order — and returns its result,
    /// how many attempts it made and every wait it asked for.
    fn run_scripted(
        script: Vec<Attempt>,
    ) -> (Result<Option<Vec<u8>>, String>, usize, Vec<Duration>) {
        let mut script = script.into_iter();
        let (mut attempts, mut waits) = (0usize, Vec::new());
        let out = fetch_hour_with(
            || {
                attempts += 1;
                script.next().expect("the policy attempted more often than the script answers")
            },
            |wait| waits.push(wait),
        );
        (out, attempts, waits)
    }

    fn status(status: u16) -> Attempt {
        Attempt::Response { status, body: Vec::new(), retry_after: None }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn a_transient_failure_is_retried_on_the_spaced_schedule_until_it_succeeds() {
        let ok = Attempt::Response { status: 200, body: vec![1, 2, 3], retry_after: None };
        let script = vec![status(503), Attempt::Failed("network: reset".into()), ok];

        let (out, attempts, waits) = run_scripted(script);

        assert_eq!(out, Ok(Some(vec![1, 2, 3])));
        assert_eq!(attempts, 3);
        assert_eq!(
            waits,
            vec![secs(4), secs(10)],
            "the first two waits, and none after the success"
        );
    }

    #[test]
    fn an_hour_that_keeps_failing_waits_out_the_whole_schedule_and_reports_the_last_failure() {
        let (out, attempts, waits) = run_scripted((0..5).map(|_| status(503)).collect());

        assert_eq!(out, Err("dukascopy: dukascopy 503 (after retries)".to_string()));
        assert_eq!(attempts, 5, "one attempt, then one more after each wait");
        assert_eq!(
            waits,
            vec![secs(4), secs(10), secs(20), secs(40)],
            "four waits — and none after the last failure, which has nothing left to wait for"
        );
        // The property the schedule exists for: the old one waited 1.5 s in all and never outlasted
        // a burst of failures that lasts seconds to minutes.
        assert!(waits.iter().sum::<Duration>() >= secs(60));
    }

    #[test]
    fn a_404_is_an_absent_hour_and_is_never_retried() {
        let (out, attempts, waits) = run_scripted(vec![status(404)]);

        assert_eq!(out, Ok(None));
        assert_eq!((attempts, waits.len()), (1, 0));
    }

    #[test]
    fn a_status_that_is_not_transient_fails_at_once() {
        let (out, attempts, waits) = run_scripted(vec![status(403)]);

        assert_eq!(out, Err("dukascopy 403".to_string()));
        assert_eq!((attempts, waits.len()), (1, 0));
    }

    #[test]
    fn a_retry_after_lengthens_a_wait_but_is_capped_and_never_shortens_one() {
        let after =
            |s| Attempt::Response { status: 429, body: Vec::new(), retry_after: Some(secs(s)) };
        let ok = Attempt::Response { status: 200, body: vec![9], retry_after: None };

        // 25 s beats the 4 s slot; an hour is capped at 60 s, which beats the 10 s slot; 1 s cannot
        // shorten the 20 s slot.
        let (out, _, waits) = run_scripted(vec![after(25), after(3600), after(1), ok]);

        assert_eq!(out, Ok(Some(vec![9])));
        assert_eq!(waits, vec![secs(25), secs(60), secs(20)]);
    }

    /// LIVE: fetch one real EURUSD hour from Dukascopy and aggregate to 1-minute bars. A weekday
    /// mid-session hour has ticks; run explicitly:
    /// `cargo test -p vike-dukascopy --lib data::tests::live_fetch -- --ignored --nocapture`
    #[test]
    #[ignore = "live: hits Dukascopy's public datafeed CDN"]
    fn live_fetch_eurusd_hour() {
        vike_log::test_init();
        // 2024-01-03 (Wed) 14:00–15:00 UTC — a liquid London/NY-overlap hour.
        let start = 1_704_290_400_000; // 2024-01-03T14:00:00Z
        let end = start + HOUR_MS - 1;
        let bars = fetch_bars_range("EURUSD", 60_000, start, end).expect("fetch");
        tracing::info!(target: "vike_dukascopy::data", "EURUSD 2024-01-03 14:00Z → {} one-minute bars", bars.len());
        assert!(!bars.is_empty(), "a liquid hour should yield bars");
        assert!(bars.len() <= 60, "≤ 60 one-minute bars in an hour");
        for b in &bars {
            assert!(b.low <= b.open && b.open <= b.high);
            assert!(b.low <= b.close && b.close <= b.high);
            assert!((1.0..1.2).contains(&b.close), "EURUSD ~1.09 in Jan-2024, got {}", b.close);
        }
        tracing::debug!(target: "vike_dukascopy::data", "first bar: {:?}", bars.first());
    }
}
