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
//!
//! ⚠ **One stream per process, one request per [`REQUEST_PACE`], never logged in** (the owner's ruling of
//! 2026-11-16). Every download of the process goes through [`STREAM`]: one keep-alive connection, opened
//! again only after a 5xx, a drop or a failed connect, and the pace every request keeps, retries
//! included. A 429, or a connect that does not complete (the ban that follows one), ends the download at
//! once with [`DukascopyHistoryRefusal::Throttled`]: no fast retry and no later timer. The stream remembers when it saw that ban,
//! and for [`BAN_MEMORY`] after it every download is refused at once with no request, in the same words
//! plus the time it was seen (a stored instant checked when a download starts). A 5xx or a drop is retried on
//! [`OVERLOAD_RETRY_DELAYS`] and then ends it with [`DukascopyHistoryRefusal::Overloaded`]; a stated `Retry-After` above
//! [`RETRY_AFTER_CAP`] (120 s) is the ban too, never slept on the stream. A failed hour fails the whole fetch, so
//! no unfinished day is stored (`crate::ticks`).

use std::io::Read;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use vike_bi5::{Tick, decode_ticks, decompress, tick_to_quote};
use vike_marketdata::{Bar, TickQuote};
use vike_model::MS_PER_HOUR;
use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlRefusal, UrlType, VenueUrls};

use crate::refusals::DukascopyHistoryRefusal;

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

/// The least time between the STARTS of two datafeed requests from this process, retries included:
/// ONE request per 10 s.
///
/// ⚠ **The only rate MEASURED safe, and the owner's ruling of 2026-11-16** (one thread, not logged in,
/// this pace). Measured that day from two boxes (wave-6 unit DUKA-DATAFEED-LIMITS): 80 requests at one
/// per 10 s on one keep-alive connection drew no 429, under this client's User-Agent and the old one
/// alike. Nothing faster could be measured (the feed was overloaded all day, a 200 took 7–51 s), and a
/// 429 is followed by a ban of the box's IP for 17–24 min at the TCP level, so a faster pace is a guess
/// paid for with a ban. Raise it only on a new measurement.
const REQUEST_PACE: Duration = Duration::from_secs(10);

/// How long a TCP connect may take. Past it the box's IP is taken as fenced off: the ban that follows a
/// 429 drops the SYN (measured 2026-11-16), and the download ends with [`DukascopyHistoryRefusal::Throttled`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take in all. Under overload a 200 took up to 51 s (median 12.5 s, measured
/// 2026-11-16); the old 30 s cut 13 % of the answers that were on their way.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// The waits before each retry of an hour that answered a 5xx (`503 No server is available` is the
/// overloaded feed's answer) or whose connection dropped: four waits, five tries, 220 s (at most 480 s when
/// the feed states a `Retry-After`, [`RETRY_AFTER_CAP`]); then the download ends with [`DukascopyHistoryRefusal::Overloaded`].
/// Under overload a 503 and a 200 alternate request by request (measured 2026-11-16), so a wait outlasts
/// the 503 rather than the minutes-long overload.
const OVERLOAD_RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

/// The longest server-stated `Retry-After` the stream waits out, lengthening one of those waits. ⚠ A wait
/// holds the box's ONLY stream, so every queued download waits with it: a longer stated wait is taken as the
/// ban, and the download ends at once with [`DukascopyHistoryRefusal::Throttled`] (the ban is remembered) instead of sleeping on the
/// stream. The cap is the longest of [`OVERLOAD_RETRY_DELAYS`], so one hour's waits stay at most 4 × 120 s
/// (1d review, finding 6: at 1800 s one hour's file could hold every queued download for about 2 h).
const RETRY_AFTER_CAP: Duration = Duration::from_secs(120);

/// How long this process remembers a ban it saw (a 429, or a connect that did not complete): every
/// download that starts within it is refused at once with no request, because a 429 during the ban re-arms
/// it (measured 2026-11-16: the ban lasted 17–24 min). A stored instant checked when a download starts,
/// never a timer: nothing runs when it lapses.
const BAN_MEMORY: Duration = Duration::from_secs(20 * 60);

/// When this process saw the feed ban it: the instant [`BAN_MEMORY`] runs from, and the wall clock its
/// words name.
#[derive(Debug, Clone, Copy)]
struct Ban {
    at: Instant,
    at_ms: i64,
}

impl Ban {
    /// [`DukascopyHistoryRefusal::ThrottledSince`] the time the ban was seen, `HH:MM` UTC.
    fn refusal(self) -> DukascopyHistoryRefusal {
        let at = chrono::DateTime::from_timestamp_millis(self.at_ms)
            .map_or_else(|| "?".to_string(), |t| t.format("%H:%M").to_string());
        DukascopyHistoryRefusal::ThrottledSince { at }
    }
}

/// What one request came back with: the seam [`fetch_hour_with`] drives.
enum Answer {
    /// The server answered: its status, the body (kept for a 2xx only) and its `Retry-After` in seconds.
    Got { status: u16, body: Vec<u8>, retry_after: Option<Duration> },
    /// The TCP connect did not complete within [`CONNECT_TIMEOUT`].
    NoConnect,
    /// No answer after the connect: a reset, a read timeout, a body cut off, TLS.
    Dropped(String),
}

/// The request starts a stream books: one per `every`, retries included.
#[derive(Debug)]
struct Pace {
    every: Duration,
    last_start: Option<Instant>,
}

impl Pace {
    const fn new(every: Duration) -> Self {
        Self { every, last_start: None }
    }

    /// How long a request that wants to start at `now` waits for its turn; the start is booked.
    fn book(&mut self, now: Instant) -> Duration {
        let start = self.last_start.map_or(now, |last| (last + self.every).max(now));
        self.last_start = Some(start);
        start - now
    }
}

/// One connection to the feed: one agent, whose pool keeps the one keep-alive connection between requests.
type Connection = Box<dyn FnMut(&str) -> Answer + Send>;

/// **This process's one datafeed stream**: its connection (opened on first use, and again only after a
/// 5xx, a drop or a failed connect), the pace every request keeps, and the last ban it saw. Held for the
/// whole of one hour's download, retries included, so two downloads never interleave their requests.
struct Stream {
    connection: Option<Connection>,
    pace: Pace,
    ban: Option<Ban>,
}

static STREAM: Mutex<Stream> =
    Mutex::new(Stream { connection: None, pace: Pace::new(REQUEST_PACE), ban: None });

/// A fresh connection: [`CONNECT_TIMEOUT`], [`ANSWER_TIMEOUT`] and this client's name (the agent's
/// `vike_transport::http::USER_AGENT`). Every body is read to its
/// end so the connection goes back to the pool (a closed hour's 404 included). A closure rather than a
/// struct because the agent's type is `ureq`'s and this crate names no `ureq`: it takes its agent from
/// `vike_transport::http`.
fn connect() -> Connection {
    let agent =
        vike_transport::http::blocking_agent_with_connect_timeout(CONNECT_TIMEOUT, ANSWER_TIMEOUT);
    Box::new(move |url| match agent.get(url).call() {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let mut body = Vec::new();
            if let Err(e) = response.body_mut().as_reader().read_to_end(&mut body) {
                return Answer::Dropped(format!("read: {e}"));
            }
            if !(200..300).contains(&status) {
                body.clear();
            }
            Answer::Got { status, body, retry_after }
        }
        Err(e) if vike_transport::http::is_connect_failure(&e) => Answer::NoConnect,
        Err(e) => Answer::Dropped(format!("network: {e}")),
    })
}

/// [`DukascopyTicks::fetch_hour`]'s rule over an injected request, clock and sleep, so it is testable with
/// neither a network nor a clock. Every request first waits its turn on `pace`. A 404 is an absent hour.
/// A 429, a connect that did not complete, or a 5xx whose stated `Retry-After` is above [`RETRY_AFTER_CAP`],
/// ends the download at once with [`DukascopyHistoryRefusal::Throttled`], with no sleep. A 5xx or a drop is retried after each of
/// [`OVERLOAD_RETRY_DELAYS`] (lengthened, never shortened, to a stated `Retry-After` of at most
/// [`RETRY_AFTER_CAP`]) and then ends it with [`DukascopyHistoryRefusal::Overloaded`]. Any other status fails at once in the feed's
/// number.
fn fetch_hour_with(
    pace: &mut Pace,
    mut attempt: impl FnMut() -> Answer,
    mut now: impl FnMut() -> Instant,
    mut sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, DukascopyHistoryRefusal> {
    let mut waits = OVERLOAD_RETRY_DELAYS.iter();
    let mut tries: u32 = 0;
    loop {
        let turn = pace.book(now());
        if !turn.is_zero() {
            sleep(turn);
        }
        tries += 1;
        let (failure, stated) = match attempt() {
            Answer::Got { status: 404, .. } => return Ok(None),
            Answer::Got { status, body, .. } if (200..300).contains(&status) => {
                return Ok(Some(body));
            }
            Answer::Got { status: 429, .. } | Answer::NoConnect => {
                return Err(DukascopyHistoryRefusal::Throttled);
            }
            Answer::Got { status: status @ 500..=599, retry_after, .. } => {
                if retry_after.is_some_and(|stated| stated > RETRY_AFTER_CAP) {
                    // a wait that long would hold the box's only stream: it is the ban, not a wait
                    return Err(DukascopyHistoryRefusal::Throttled);
                }
                (format!("HTTP {status}"), retry_after)
            }
            Answer::Got { status, .. } => return Err(DukascopyHistoryRefusal::Status { status }),
            Answer::Dropped(why) => (why, None),
        };
        let Some(wait) = waits.next() else {
            return Err(DukascopyHistoryRefusal::Overloaded { tries, failure });
        };
        sleep((*wait).max(stated.unwrap_or_default()));
    }
}

/// [`fetch_hour_with`] behind the ban this process remembers in `ban`. While a ban seen less than
/// [`BAN_MEMORY`] ago stands, the hour is refused in its words with no request (a download queued behind
/// the one that was throttled, or a new one). A download the feed throttles records the ban (`now`,
/// `wall_ms`) and ends in the same words.
fn fetch_hour_remembering(
    ban: &mut Option<Ban>,
    pace: &mut Pace,
    attempt: impl FnMut() -> Answer,
    mut now: impl FnMut() -> Instant,
    wall_ms: impl FnOnce() -> i64,
    sleep: impl FnMut(Duration),
) -> Result<Option<Vec<u8>>, DukascopyHistoryRefusal> {
    if let Some(seen) = *ban
        && now().saturating_duration_since(seen.at) < BAN_MEMORY
    {
        return Err(seen.refusal());
    }
    match fetch_hour_with(pace, attempt, &mut now, sleep) {
        Err(DukascopyHistoryRefusal::Throttled) => {
            let seen = Ban { at: now(), at_ms: wall_ms() };
            *ban = Some(seen);
            Err(seen.refusal())
        }
        out => out,
    }
}

impl DukascopyTicks {
    /// Download one hour's `.bi5` — `Ok(None)` when the hour is absent (404 = weekend / pre-listing /
    /// not-yet-published) — on this process's one stream (`STREAM`), at `REQUEST_PACE` (one request per
    /// 10 s), by the rule of `fetch_hour_with` behind the ban the stream remembers (`fetch_hour_remembering`).
    /// The stream is held for the whole hour, retries included; the ways a download ends are this module's doc.
    pub fn fetch_hour(
        &self,
        symbol: &str,
        hour_start_ms: i64,
    ) -> Result<Option<Vec<u8>>, DukascopyHistoryRefusal> {
        let url = self.hour_url(symbol, hour_start_ms);
        let mut stream = STREAM.lock().unwrap_or_else(PoisonError::into_inner);
        let Stream { connection, pace, ban } = &mut *stream;
        let attempt = || {
            let answer = connection.get_or_insert_with(connect)(&url);
            if !matches!(answer, Answer::Got { status: 0..=499, .. }) {
                // a 5xx, a drop or a failed connect: the next request opens a new connection
                *connection = None;
            }
            answer
        };
        fetch_hour_remembering(
            ban,
            pace,
            attempt,
            Instant::now,
            vike_model::now_ms,
            std::thread::sleep,
        )
    }

    /// Fetch tick history for `[start_ms, end_ms]` (hour by hour, skipping absent/empty hours), each record's
    /// integer prices divided by `point_value` (the instrument's, from its list row; see the module doc).
    pub fn fetch_ticks_range(
        &self,
        symbol: &str,
        point_value: f64,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Tick>, DukascopyHistoryRefusal> {
        let mut out = Vec::new();
        let mut hour = start_ms.div_euclid(MS_PER_HOUR) * MS_PER_HOUR;
        while hour <= end_ms {
            if let Some(blob) = self.fetch_hour(symbol, hour)?
                && !blob.is_empty()
            {
                let raw = decompress(&blob)
                    .map_err(|why| DukascopyHistoryRefusal::Undecodable { why })?;
                for t in decode_ticks(&raw, hour, point_value) {
                    if start_ms <= t.timestamp && t.timestamp <= end_ms {
                        out.push(t);
                    }
                }
            }
            hour += MS_PER_HOUR;
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
    ) -> Result<Vec<Bar>, DukascopyHistoryRefusal> {
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
    ) -> Result<Vec<TickQuote>, DukascopyHistoryRefusal> {
        Ok(self
            .fetch_ticks_range(symbol, point_value, start_ms, end_ms)?
            .iter()
            .map(|t| tick_to_quote(t, symbol))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

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

    // ── the download rule, over a scripted network and a scripted clock: no test sleeps or dials ──

    /// What one scripted hour did: its result, the start of every request (from the clock's origin) and
    /// every sleep it asked for.
    struct Run {
        out: Result<Option<Vec<u8>>, DukascopyHistoryRefusal>,
        starts: Vec<Duration>,
        sleeps: Vec<Duration>,
    }

    /// One hour's download against `script` (one answer per request, in order) on `pace`, over a clock
    /// that moves only when the rule sleeps or a request takes `took`.
    fn hour_on(
        pace: &mut Pace,
        origin: Instant,
        clock: &Cell<Instant>,
        script: Vec<Answer>,
        took: Duration,
    ) -> Run {
        let mut script = script.into_iter();
        let (mut starts, mut sleeps) = (Vec::new(), Vec::new());
        let out = fetch_hour_with(
            pace,
            || {
                starts.push(clock.get() - origin);
                clock.set(clock.get() + took);
                script.next().expect("the rule asked more often than the script answers")
            },
            || clock.get(),
            |wait| {
                sleeps.push(wait);
                clock.set(clock.get() + wait);
            },
        );
        Run { out, starts, sleeps }
    }

    /// One hour's download on a fresh stream whose requests take no time.
    fn hour(script: Vec<Answer>) -> Run {
        let origin = Instant::now();
        hour_on(&mut Pace::new(REQUEST_PACE), origin, &Cell::new(origin), script, Duration::ZERO)
    }

    fn got(status: u16) -> Answer {
        Answer::Got { status, body: Vec::new(), retry_after: None }
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn a_429_ends_the_download_at_once_in_the_owners_words_with_no_retry_and_no_wait() {
        let stated = Answer::Got { status: 429, body: Vec::new(), retry_after: Some(secs(5)) };
        for script in [vec![got(429)], vec![stated]] {
            let run = hour(script);
            assert_eq!(run.out, Err(DukascopyHistoryRefusal::Throttled));
            assert_eq!((run.starts.len(), run.sleeps.len()), (1, 0), "one request, no wait");
        }
    }

    /// The ban after a 429 drops the SYN: a connect that never completes is the same stop.
    #[test]
    fn a_connect_that_never_completes_is_the_ban_and_ends_the_download_at_once() {
        let run = hour(vec![Answer::NoConnect]);
        assert_eq!(run.out, Err(DukascopyHistoryRefusal::Throttled));
        assert_eq!((run.starts.len(), run.sleeps.len()), (1, 0));
    }

    #[test]
    fn a_503_is_retried_after_10_30_60_and_120_s_and_then_the_download_ends_overloaded() {
        let run = hour((0..5).map(|_| got(503)).collect());
        assert_eq!(
            run.out,
            Err(DukascopyHistoryRefusal::Overloaded { tries: 5, failure: "HTTP 503".to_string() })
        );
        assert_eq!(run.starts.len(), 5, "one request, then one after each wait");
        assert_eq!(run.sleeps, vec![secs(10), secs(30), secs(60), secs(120)]);
    }

    #[test]
    fn a_503_or_a_drop_is_retried_and_the_hour_comes_on_a_later_try() {
        let ok = Answer::Got { status: 200, body: vec![1, 2, 3], retry_after: None };
        let run = hour(vec![got(503), Answer::Dropped("network: reset".into()), ok]);
        assert_eq!(run.out, Ok(Some(vec![1, 2, 3])));
        assert_eq!(run.sleeps, vec![secs(10), secs(30)], "no wait after the success");
    }

    #[test]
    fn a_404_is_an_absent_hour_and_is_never_retried() {
        let run = hour(vec![got(404)]);
        assert_eq!(run.out, Ok(None));
        assert_eq!((run.starts.len(), run.sleeps.len()), (1, 0));
    }

    #[test]
    fn a_status_that_is_not_transient_fails_at_once() {
        let run = hour(vec![got(403)]);
        assert_eq!(run.out, Err(DukascopyHistoryRefusal::Status { status: 403 }));
        assert_eq!((run.starts.len(), run.sleeps.len()), (1, 0));
    }

    #[test]
    fn a_retry_after_up_to_120_s_lengthens_a_wait_and_never_shortens_one() {
        let after = |s| Answer::Got { status: 503, body: Vec::new(), retry_after: Some(secs(s)) };
        let ok = Answer::Got { status: 200, body: vec![9], retry_after: None };
        // 25 s beats the 10 s slot; 120 s (the cap itself) beats the 30 s slot; 1 s cannot shorten the
        // 60 s slot.
        let run = hour(vec![after(25), after(120), after(1), ok]);
        assert_eq!(run.out, Ok(Some(vec![9])));
        assert_eq!(run.sleeps, vec![secs(25), secs(120), secs(60)]);
        assert_eq!(RETRY_AFTER_CAP, secs(120));
    }

    /// **A stated wait above 120 s is the ban, never slept on the stream** (1d review, finding 6): the
    /// download ends at once in the ban's words with no sleep, the ban is remembered, and the next download
    /// is refused with no request.
    #[test]
    fn a_retry_after_over_120_s_is_the_ban_ends_at_once_with_no_sleep_and_is_remembered() {
        let at_ms = chrono::DateTime::parse_from_rfc3339("2026-11-16T18:41:00Z")
            .expect("test")
            .timestamp_millis();
        let words = DukascopyHistoryRefusal::ThrottledSince { at: "18:41".to_string() };
        for stated in [121, 1800, 3600] {
            let (origin, mut ban, mut pace) = (Instant::now(), None, Pace::new(REQUEST_PACE));
            let clock = Cell::new(origin);
            let long =
                Answer::Got { status: 503, body: Vec::new(), retry_after: Some(secs(stated)) };

            let (out, asked) = remembered(&mut ban, &mut pace, &clock, vec![long], at_ms);
            assert_eq!((out, asked), (Err(words.clone()), 1), "Retry-After {stated} s");
            assert_eq!(clock.get(), origin, "Retry-After {stated} s: nothing slept on the stream");

            let (out, asked) = remembered(&mut ban, &mut pace, &clock, vec![], at_ms);
            assert_eq!((out, asked), (Err(words.clone()), 0), "the ban is remembered");
        }
    }

    /// One hour behind the stream's remembered ban, over the scripted clock (requests take no time):
    /// its result and how many requests it made.
    fn remembered(
        ban: &mut Option<Ban>,
        pace: &mut Pace,
        clock: &Cell<Instant>,
        script: Vec<Answer>,
        wall_ms: i64,
    ) -> (Result<Option<Vec<u8>>, DukascopyHistoryRefusal>, usize) {
        let mut script = script.into_iter();
        let mut asked = 0;
        let out = fetch_hour_remembering(
            ban,
            pace,
            || {
                asked += 1;
                script.next().expect("a request the ban should have refused was made")
            },
            || clock.get(),
            || wall_ms,
            |wait| clock.set(clock.get() + wait),
        );
        (out, asked)
    }

    /// **The ban is remembered for 20 minutes**: after a 429, every download that starts within
    /// `BAN_MEMORY` is refused at once with no request, in the owner's words plus the time the ban was
    /// seen; one that starts after it asks the feed again.
    #[test]
    fn a_ban_is_remembered_for_20_minutes_and_every_download_in_it_is_refused_with_no_request() {
        let at_ms = chrono::DateTime::parse_from_rfc3339("2026-11-16T14:07:30Z")
            .expect("test")
            .timestamp_millis();
        let words = DukascopyHistoryRefusal::ThrottledSince { at: "14:07".to_string() };
        let (origin, mut ban, mut pace) = (Instant::now(), None, Pace::new(REQUEST_PACE));
        let clock = Cell::new(origin);

        let (out, asked) = remembered(&mut ban, &mut pace, &clock, vec![got(429)], at_ms);
        assert_eq!((out, asked), (Err(words.clone()), 1), "the throttled download");

        for later in [secs(1), secs(5 * 60), BAN_MEMORY - secs(1)] {
            clock.set(origin + later);
            let (out, asked) = remembered(&mut ban, &mut pace, &clock, vec![], at_ms + 999_999);
            assert_eq!((out, asked), (Err(words.clone()), 0), "{later:?} after the ban");
        }

        clock.set(origin + BAN_MEMORY);
        let ok = Answer::Got { status: 200, body: vec![7], retry_after: None };
        let (out, asked) = remembered(&mut ban, &mut pace, &clock, vec![ok], at_ms);
        assert_eq!(
            (out, asked),
            (Ok(Some(vec![7])), 1),
            "the memory has lapsed: the feed is asked"
        );
        assert_eq!(BAN_MEMORY, secs(20 * 60));
    }

    /// A connect that never completes is the ban too and is remembered; an overloaded feed is not a ban.
    #[test]
    fn a_failed_connect_is_remembered_as_the_ban_and_an_overload_is_not() {
        let at_ms = chrono::DateTime::parse_from_rfc3339("2026-11-16T09:05:00Z")
            .expect("test")
            .timestamp_millis();
        let origin = Instant::now();
        let clock = Cell::new(origin);

        let (mut ban, mut pace) = (None, Pace::new(REQUEST_PACE));
        let (out, _) = remembered(&mut ban, &mut pace, &clock, vec![Answer::NoConnect], at_ms);
        let words = DukascopyHistoryRefusal::ThrottledSince { at: "09:05".to_string() };
        assert_eq!(out, Err(words.clone()));
        assert_eq!(remembered(&mut ban, &mut pace, &clock, vec![], at_ms), (Err(words), 0));

        let (mut ban, mut pace) = (None, Pace::new(REQUEST_PACE));
        let (out, asked) =
            remembered(&mut ban, &mut pace, &clock, (0..5).map(|_| got(503)).collect(), at_ms);
        assert_eq!(
            out.expect_err("overloaded").id(),
            crate::refusals::DukascopyHistoryRefusalId::Overloaded
        );
        assert_eq!((asked, ban.is_none()), (5, true), "an overload records no ban");
    }

    /// **The pace holds**: across hours and retries, two requests of one stream never start less than
    /// `REQUEST_PACE` apart, and the stream's first request does not wait.
    #[test]
    fn the_pace_holds_one_request_start_per_10_s_across_hours_and_retries() {
        let ok = || Answer::Got { status: 200, body: vec![1], retry_after: None };
        let (origin, mut pace) = (Instant::now(), Pace::new(REQUEST_PACE));
        let clock = Cell::new(origin);
        let took = Duration::from_millis(300);
        let mut starts = Vec::new();
        for script in [vec![ok()], vec![got(503), ok()], vec![ok()], vec![got(404)]] {
            let run = hour_on(&mut pace, origin, &clock, script, took);
            assert!(run.out.is_ok(), "{:?}", run.out);
            starts.extend(run.starts);
        }
        assert_eq!(starts.len(), 5);
        assert_eq!(starts[0], Duration::ZERO, "the stream's first request does not wait");
        for pair in starts.windows(2) {
            assert!(pair[1] - pair[0] >= REQUEST_PACE, "starts {starts:?}");
        }
        assert_eq!(REQUEST_PACE, secs(10));
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
        let end = start + MS_PER_HOUR - 1;
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
