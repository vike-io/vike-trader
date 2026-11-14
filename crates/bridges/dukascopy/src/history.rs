//! Dukascopy FX source — keyless deep tick history (bid/ask, back to ~2003), history-only (T+1 lag).
//!
//! Port of the Python `dukascopy_source`. Downloads the public per-UTC-hour `.bi5` tick archives (no
//! key), LZMA-decodes them, and aggregates ticks to [`Bar`] OHLC at any step. The file FORMAT — the
//! 20-byte record, [`Tick`], and the decoder — is `vike-bi5`'s (`crates/vike-bi5/src/tick.rs`); this
//! file keeps the FETCH: the URL, the retrying download and the tick-to-bar aggregation. Prices are
//! integer "points", divided by the instrument's point value, which the CALLER hands in: its row of the account's
//! instrument list states it (`crate::instrument::grid::history_point_value`, `10^tick_scale`), and nothing here
//! assumes one (owner 2026-11-13: no per-symbol fact is compiled in).
//! OHLC uses the bid/ask **mid**; `volume` is the tick count (FX has no true volume). The pure parts
//! are unit-tested; only `fetch_hour` does network I/O.
//!
//! The feed's base is the `dukascopy live history rest` address row ([`DukascopyTicks::from_urls`]):
//! a keyless public archive, so a demo box reads the same live feed.

use std::io::Read;
use std::time::Duration;

use vike_bi5::{HOUR_MS, Tick, decode_ticks, decompress, tick_to_quote};
use vike_bridge_core::retry::{RetryAttempt, RetryFailure, RetryGaveUp, retry_scheduled};
use vike_marketdata::{Bar, TickQuote};
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

/// **The keyless `.bi5` tick archive at its base**: every fetch below is a method of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DukascopyTicks {
    base: String,
}

impl DukascopyTicks {
    /// The feed at the `dukascopy live history rest` address row.
    ///
    /// # Errors
    /// The lookup's refusal (it names the missing row).
    pub fn from_urls(urls: &VenueUrls) -> Result<Self, UrlRefusal> {
        let base = urls.url(UrlType::Live, UrlChannel::History, UrlProtocol::Rest, "")?;
        Ok(Self { base: base.to_string() })
    }

    /// Build the `.bi5` URL for a UTC hour. Month is 0-indexed (January = 00), matching Dukascopy.
    pub fn hour_url(&self, symbol: &str, hour_start_ms: i64) -> String {
        use chrono::{Datelike, Timelike};
        let dt = chrono::DateTime::from_timestamp_millis(hour_start_ms).unwrap_or_default();
        format!(
            "{}/{}/{:04}/{:02}/{:02}/{:02}h_ticks.bi5",
            self.base,
            symbol.to_ascii_uppercase(),
            dt.year(),
            dt.month0(),
            dt.day(),
            dt.hour(),
        )
    }
}

/// Aggregate ticks into OHLC bars of `step_ms`, using the bid/ask mid. `volume` = tick count in the
/// bucket. Assumes ticks ascending by `ts`; emits bars ascending.
pub fn ticks_to_bars(ticks: &[Tick], step_ms: i64) -> Vec<Bar> {
    use std::collections::BTreeMap;
    // per bucket: (open, high, low, close, count)
    let mut buckets: BTreeMap<i64, (f64, f64, f64, f64, f64)> = BTreeMap::new();
    for t in ticks {
        let bucket = t.timestamp.div_euclid(step_ms) * step_ms;
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
            timestamp: ts,
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

/// [`fetch_hour`]'s retry policy over an injected attempt and sleep: `retry_scheduled` on
/// [`RETRY_DELAYS`] and [`RETRY_AFTER_CAP`] (a 404 is an absent hour), in this venue's words.
fn fetch_hour_with(
    attempt: impl FnMut() -> RetryAttempt,
    sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, String> {
    retry_scheduled(&RETRY_DELAYS, RETRY_AFTER_CAP, attempt, sleep).map_err(|gave_up| match gave_up
    {
        RetryGaveUp::Refused(status) => format!("dukascopy {status}"),
        RetryGaveUp::Exhausted(RetryFailure::Status(status)) => {
            format!("dukascopy: dukascopy {status} (after retries)")
        }
        RetryGaveUp::Exhausted(RetryFailure::Transport(e)) => {
            format!("dukascopy: {e} (after retries)")
        }
    })
}

impl DukascopyTicks {
    /// Download one hour's `.bi5` — `Ok(None)` when the hour is absent (404 = weekend / pre-listing /
    /// not-yet-published). Retries transient CDN failures (429/5xx/network) on the [`RETRY_DELAYS`]
    /// schedule — spaced seconds apart, not the fraction of a second it once was, because the CDN's
    /// failures arrive in bursts (see that constant).
    pub fn fetch_hour(&self, symbol: &str, hour_start_ms: i64) -> Result<Option<Vec<u8>>, String> {
        let url = self.hour_url(symbol, hour_start_ms);
        let agent = vike_bridge_core::http::blocking_agent();
        // One real attempt. A closure rather than a fn because the agent's type is `ureq`'s and this
        // crate names no `ureq` — it takes its agent from `vike_bridge_core::http`.
        let attempt = || match agent
            .get(&url)
            // Dukascopy's CDN prefers a browser-ish UA
            .header("User-Agent", "Mozilla/5.0 (vike-trader-rust forex source)")
            .call()
        {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let mut body = Vec::new();
                if (200..300).contains(&status)
                    && let Err(e) = response.body_mut().as_reader().read_to_end(&mut body)
                {
                    return RetryAttempt::Failed(format!("read: {e}"));
                }
                RetryAttempt::Response { status, body, retry_after }
            }
            Err(e) => RetryAttempt::Failed(format!("network: {e}")),
        };
        fetch_hour_with(attempt, std::thread::sleep)
    }

    /// Fetch tick history for `[start_ms, end_ms]` (hour by hour, skipping absent/empty hours), each record's
    /// integer prices divided by `point_value` (the instrument's, from its list row; see the module doc).
    pub fn fetch_ticks_range(
        &self,
        symbol: &str,
        point_value: f64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Tick>, String> {
        let mut out = Vec::new();
        let mut hour = start_ms.div_euclid(HOUR_MS) * HOUR_MS;
        while hour <= end_ms {
            if let Some(blob) = self.fetch_hour(symbol, hour)?
                && !blob.is_empty()
            {
                for t in decode_ticks(&decompress(&blob)?, hour, point_value) {
                    if start_ms <= t.timestamp && t.timestamp <= end_ms {
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
        &self,
        symbol: &str,
        point_value: f64,
        step_ms: i64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, String> {
        Ok(ticks_to_bars(&self.fetch_ticks_range(symbol, point_value, start_ms, end_ms)?, step_ms))
    }

    /// [`Self::fetch_ticks_range`], mapped to the store's quote row by [`tick_to_quote`]. Network
    /// I/O (the public `.bi5` CDN); no store — the append is `vike-backfill`'s.
    pub fn fetch_quotes_range(
        &self,
        symbol: &str,
        point_value: f64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<TickQuote>, String> {
        Ok(self
            .fetch_ticks_range(symbol, point_value, start_ms, end_ms)?
            .iter()
            .map(|t| tick_to_quote(t, symbol))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release's feed: the seed's `dukascopy live history rest` row.
    fn seeded() -> DukascopyTicks {
        DukascopyTicks::from_urls(&VenueUrls::seed("dukascopy")).expect("the seed's history row")
    }

    #[test]
    fn hour_url_is_zero_indexed_month() {
        // 2020-01-02 03:00 UTC → epoch ms. January is month 00.
        let ms = 1_577_934_000_000; // 2020-01-02T03:00:00Z
        let u = seeded().hour_url("eurusd", ms);
        assert_eq!(u, "https://datafeed.dukascopy.com/datafeed/EURUSD/2020/00/02/03h_ticks.bi5");
    }

    /// A demo box reads the same keyless feed (history falls back to live), and no row is refused
    /// naming the row, never a built-in host.
    #[test]
    fn the_feed_is_the_live_history_row() {
        let demo_only = VenueUrls::from_rows(
            "dukascopy",
            [("dukascopy", UrlType::Demo, UrlChannel::History, UrlProtocol::Rest, "", "https://x")],
        );
        let why = DukascopyTicks::from_urls(&demo_only).expect_err("asks the LIVE row");
        assert!(why.to_string().contains("live history rest"), "{why}");
        assert!(DukascopyTicks::from_urls(&VenueUrls::default()).is_err());
    }

    #[test]
    fn ticks_aggregate_to_ohlc_on_mid() {
        // three ticks in the same 60s bucket, one in the next.
        let mk = |ts, bid, ask| Tick { timestamp: ts, bid, ask, bid_vol: 0.0, ask_vol: 0.0 };
        let ticks = vec![
            mk(0, 1.0, 1.2),      // mid 1.1 (open)
            mk(1_000, 1.2, 1.4),  // mid 1.3 (high)
            mk(2_000, 0.9, 1.1),  // mid 1.0 (low, close of bucket 0)
            mk(60_000, 2.0, 2.0), // mid 2.0 (next bucket)
        ];
        let bars = ticks_to_bars(&ticks, 60_000);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].timestamp, 0);
        assert!((bars[0].open - 1.1).abs() < 1e-9);
        assert!((bars[0].high - 1.3).abs() < 1e-9);
        assert!((bars[0].low - 1.0).abs() < 1e-9);
        assert!((bars[0].close - 1.0).abs() < 1e-9);
        assert_eq!(bars[0].volume, 3.0); // tick count
        assert_eq!(bars[1].timestamp, 60_000);
        assert_eq!(bars[1].volume, 1.0);
    }

    // ── the retry policy, over a scripted network: no test sleeps or dials ──────────────────────

    /// Runs the policy against `script` — one answer per attempt, in order — and returns its result,
    /// how many attempts it made and every wait it asked for.
    fn run_scripted(
        script: Vec<RetryAttempt>,
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

    fn status(status: u16) -> RetryAttempt {
        RetryAttempt::Response { status, body: Vec::new(), retry_after: None }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn a_transient_failure_is_retried_on_the_spaced_schedule_until_it_succeeds() {
        let ok = RetryAttempt::Response { status: 200, body: vec![1, 2, 3], retry_after: None };
        let script = vec![status(503), RetryAttempt::Failed("network: reset".into()), ok];

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
        let after = |s| RetryAttempt::Response {
            status: 429,
            body: Vec::new(),
            retry_after: Some(secs(s)),
        };
        let ok = RetryAttempt::Response { status: 200, body: vec![9], retry_after: None };

        // 25 s beats the 4 s slot; an hour is capped at 60 s, which beats the 10 s slot; 1 s cannot
        // shorten the 20 s slot.
        let (out, _, waits) = run_scripted(vec![after(25), after(3600), after(1), ok]);

        assert_eq!(out, Ok(Some(vec![9])));
        assert_eq!(waits, vec![secs(25), secs(60), secs(20)]);
    }

    /// LIVE: fetch one real EURUSD hour from Dukascopy and aggregate to 1-minute bars. A weekday
    /// mid-session hour has ticks; run explicitly:
    /// `cargo test -p vike-dukascopy --lib history::tests::live_fetch -- --ignored --nocapture`
    #[test]
    #[ignore = "live: hits Dukascopy's public datafeed CDN"]
    fn live_fetch_eurusd_hour() {
        vike_log::test_init();
        // 2024-01-03 (Wed) 14:00–15:00 UTC — a liquid London/NY-overlap hour.
        let start = 1_704_290_400_000; // 2024-01-03T14:00:00Z
        let end = start + HOUR_MS - 1;
        // EUR/USD's point value, as its list row states it (tick scale 5)
        let bars = seeded().fetch_bars_range("EURUSD", 1e5, 60_000, start, end).expect("fetch");
        tracing::info!(target: "vike_dukascopy::history", "EURUSD 2024-01-03 14:00Z → {} one-minute bars", bars.len());
        assert!(!bars.is_empty(), "a liquid hour should yield bars");
        assert!(bars.len() <= 60, "≤ 60 one-minute bars in an hour");
        for b in &bars {
            assert!(b.low <= b.open && b.open <= b.high);
            assert!(b.low <= b.close && b.close <= b.high);
            assert!((1.0..1.2).contains(&b.close), "EURUSD ~1.09 in Jan-2024, got {}", b.close);
        }
        tracing::debug!(target: "vike_dukascopy::history", "first bar: {:?}", bars.first());
    }
}
