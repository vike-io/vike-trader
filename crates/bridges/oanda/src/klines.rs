//! **OANDA's candle history as a store-free source** — [`OandaKlines`], this bridge's
//! `vike_data::source::KlineSource`: the FETCH half of a backfill and nothing else. It returns rows;
//! the commit key, the still-forming guard and the append are `vike-backfill`'s, and which process
//! dispatches to it is the datahub's (`docs/decisions/0094-backfill-names-no-venue.md`).
//!
//! # One channel, deep and recent at once
//!
//! OANDA has no archive and no tick history. The v20 candles endpoint IS its history, so a fetch of
//! 2005 and a fetch of last week are the same call with a different `from`, and there is nothing to
//! download and convert — this is a paging client. One request:
//!
//! ```text
//! GET /v3/instruments/{I}/candles?granularity=S5&price=M&from=<epoch seconds>&count=5000
//! ```
//!
//! MID candles (`price=M`), the fxPractice host and no other, one [`Bar`] per COMPLETE candle.
//! Timestamps arrive in epoch seconds under `Accept-Datetime-Format: UNIX` and leave as epoch ms;
//! `volume` is the venue's tick count. The boundaries are OANDA's own — the venue's default
//! alignment (17:00 New York for the multi-hour and daily granularities) is not overridden — and
//! rows are stored as served.
//!
//! # The paging protocol, and where it was validated
//!
//! It is the protocol the OANDA S5 probe and downloader use (branch `probe/oanda-s5`, examples
//! `s5_probe` and `s5_download`, written 2026-09-30 for four majors from 2005 to the present):
//!
//! * the next `from` is the last candle's time — of ANY completeness — plus ONE STEP;
//! * the walk ends on an empty page, on a page SHORTER than the `count` asked for (the venue ran out
//!   of data), or on a page whose last candle reaches the window's end;
//! * a candle at or before the last one KEPT is a seam duplicate and is dropped. A cursor that lands
//!   inside a candle therefore costs one repeated row and never a hole — which is the venue's own
//!   contract (its v20 spec's `includeFirst` describes `from` as selecting the candle that COVERS
//!   it) and the reason one fixed step is a safe cursor even where a candle is not exactly one step
//!   wide;
//! * a cursor that does not move FORWARD is an error: never a loop, never a silent truncation.
//!
//! Two rules are this module's own. A last candle that is still forming (`complete: false`) ends the
//! walk — nothing follows the live edge, and a `from` past "now" is a request for nothing — and the
//! candles of one page must be strictly increasing, so a reordered response fails loudly instead of
//! being thinned by the seam filter.
//!
//! # The interval vocabulary is NOT the live feed's
//!
//! `crates/bridges/oanda/src/data.rs`'s `granularity` is the LIVE table, not a candles-fetch table:
//! `crates/bridges/oanda/src/market_feed.rs`'s `subscribe_bars` refuses an interval it does not map,
//! and `crates/vike-tradehub/src/venue_plan.rs`'s `oanda_plan` derives a live mount's accepted
//! intervals from it — mounts whose CLOSED bars drive the margin-call watchdog and the drawdown
//! latch. The four sub-minute granularities (`S5 S10 S15 S30`, the probe's subject) are therefore
//! declared HERE, in `history_granularity`, which delegates every other row to `granularity`.
//! Adding them to the shared table would have widened a live-mount gate as a side effect of a
//! history change; `crates/vike-catalog/src/intervals.rs`'s `intervals_for` row for this venue stays
//! `Unmeasured` and untouched (STEP 1 merges byte-identical), and
//! `crates/bridges/oanda/tests/candles.rs`'s `granularity_unsupported_intervals_are_none` (`1s` and
//! `7s` refused) holds either way.
//!
//! `1w` and `1mo` map to `W` and `M`, but a week and a month are calendar widths with no fixed step to
//! page by, and `vike_model::time::measures_bar_step` already refuses them ahead of any source. This
//! one refuses them too rather than depend on only ever being called through that guard.
//!
//! # The credential, and how it stays out of everything
//!
//! The constructor takes a token PROVIDER, never a token. `fetch` calls it ONCE, holds the value in a
//! `Secret` whose `Debug` redacts, and drops it when the fetch ends; this crate reads no store and no
//! environment. The datahub's provider reads the store through a scoped read of
//! [`crate::oanda_history_token_names`] at request time and hands the map to
//! [`crate::load_oanda_history_token`] — the practice tier's key alone, never a live-tier one.
//!
//! * [`HistoryTokenError`] carries NO payload, so its `Display` cannot hold a token by construction.
//! * Every string that came off the wire — an OANDA error body, a network error — passes
//!   `Secret::redact` in ONE place (`request_with`) before it can become an error, and the
//!   redaction runs BEFORE the body is cut to its 200-character head, so a token straddling the cut
//!   cannot leave a readable prefix behind.
//! * A failure to obtain the token is [`SourceError::Refused`]: the venue was never asked, and the
//!   `venue fetch:` prefix a `Fetch` earns would say the opposite.
//!
//! # Pace: one budget and one connection pool per PROCESS
//!
//! OANDA documents 120 requests a second and about two NEW connections a second (quoted through the
//! probe's report, not read from OANDA's site); the probe measured clean at up to 25 a second, which
//! is also the downloader's own default ceiling.
//! This module takes at most 20, across EVERY fetch in the process: `history_gate` is a `RateGate`,
//! the blocking sliding-window limiter in `crates/vike-bridge-core/src/ratelimit.rs`. (The `Pacer`
//! beside it is per-pager and pure — no clock, no sleeping, no shared state — which is the wrong
//! shape for a budget several concurrent fetches must split.) Every ATTEMPT takes a slot, retries
//! included. All fetches also ride one shared connection pool, `history_agent`, so a fetch per day
//! chunk reuses a connection instead of opening one per chunk against a two-a-second allowance.
//!
//! # Retries
//!
//! A transient failure — 429, any 5xx, a transport failure, a 200 whose body is not JSON — is retried
//! on `RETRY_DELAYS` (eight attempts, doubling from one second to a minute: the schedule the
//! downloader uses, minus its jitter, which one datahub has nothing to desynchronise from). Each wait
//! is lengthened, never shortened, to a `Retry-After` up to `RETRY_AFTER_CAP`. Every OTHER status
//! fails at once, with OANDA's own `errorMessage`. The pattern is
//! `crates/bridges/dukascopy/src/data.rs`'s `fetch_hour_with`: an injected attempt and an injected
//! sleep, so the whole schedule is testable with neither a network nor a clock.
//!
//! `crates/bridges/oanda/src/rest.rs`'s `OandaRest` cannot serve this: it returns the parsed JSON or
//! an `OandaApiError`, so a `Retry-After` header never reaches its caller. The source therefore takes
//! the shared `crates/vike-bridge-core/src/http.rs`'s `get_raw`, which captures the header before it
//! drains the body, and sends the two headers `OandaRest` would have sent itself.
//!
//! # What it does not do
//!
//! * **It does not bound the window.** `fetch` returns the WHOLE window as one `Vec`, so a caller
//!   holding a wide sub-minute window holds every bar of it — a year of S5 is millions of rows.
//!   Chunking is the ingest's job; nothing here caps it.
//! * **Mid only.** No bid or ask candles.
//! * **A `from` before the data is harmless — but for S5 the early data is not 5-second data.** The
//!   venue clamps an early `from` to the first candle (the downloader relies on it, starting its
//!   earliest file at 2000-01-01). The probe's early-years profile and the downloader's header also
//!   report, for the four majors probed, ONE END-OF-DAY candle per trading day from 2002-05 until the
//!   Sunday-evening open of 2005-01-02, and a real 5-second series only from then. A sub-minute
//!   window that starts earlier receives those sparse candles under the same label. They are returned
//!   as served: a floor found on four instruments is not a floor for the rest, so that policy belongs
//!   to the caller that knows the instrument.

use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use vike_bridge_core::credentials::Environment;
use vike_bridge_core::http::{blocking_agent, body_head, get_raw};
use vike_bridge_core::ratelimit::RateGate;
use vike_data::source::{KlineSource, SourceError};
use vike_model::Bar;

use crate::config::{oanda_history_token_names, oanda_hosts};
use crate::data::{bar_from_candle, granularity, to_oanda_instrument};
use crate::recon_client::VENUE;
use crate::rest::OandaApiError;

/// The candles asked for per request: OANDA's ceiling (documented, and measured by the probe). It is
/// also what a page is judged against — fewer candles than were asked for means the venue ran out of
/// data — so it is the request's `count` and the walk's "short page" threshold at once.
const PAGE_COUNT: usize = 5000;

/// The most requests this process sends the candles endpoint in any one second, across EVERY fetch.
/// Under OANDA's documented 120 and under the 25 the probe measured clean (see the module doc).
const MAX_REQUESTS_PER_SECOND: usize = 20;

/// The wait before each RETRY of one request — seven waits, so eight attempts, about two minutes of
/// patience in all. The schedule the downloader uses (doubling from one second, capped at a
/// minute), without its jitter.
const RETRY_DELAYS: [Duration; 7] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(32),
    Duration::from_secs(60),
];

/// The longest a server-stated `Retry-After` is honoured for: a confused header must not park a
/// backfill for an hour. The downloader's own cap.
const RETRY_AFTER_CAP: Duration = Duration::from_secs(120);

/// The longest instrument name accepted. Generous — the longest real names are about a dozen
/// characters — and only a bound on what may be spliced into a request path.
const MAX_INSTRUMENT_LEN: usize = 32;

/// What a caller hands [`OandaKlines::new`] instead of a token: a closure that produces the
/// practice-tier API key WHEN a fetch starts. It is called once per fetch, and the value it returns
/// lives only for that fetch — nothing in this crate stores it.
///
/// The datahub's provider builds `KeyScope::of(oanda_history_token_names())`, reads the credential
/// store through it, and returns the token [`crate::load_oanda_history_token`] finds in the scoped
/// map — or a [`HistoryTokenError`] saying why there is none.
///
/// ⚠ **"Once per fetch" is not "once per request".** An ingest that walks a long window in day chunks
/// calls `fetch` once per chunk, so a provider that reads the store afresh on every call reads it
/// thousands of times over a multi-year request. Holding the answer for the length of the request
/// the source was built for — a source per request, a provider that reads on its first call — is the
/// provider's job; this crate deliberately caches nothing, so a token rotated between requests is
/// picked up by the next one.
pub type HistoryTokenProvider = Arc<dyn Fn() -> Result<String, HistoryTokenError> + Send + Sync>;

/// Why a [`HistoryTokenProvider`] produced no token.
///
/// ⚠ **No variant carries a payload, deliberately.** The `Display` of this type reaches the operator
/// through a `SourceError`, a wire error and a CLI line, so the property "it can never hold a token"
/// is made true by the type rather than by care: a caller cannot put a value into it, whatever it
/// does. The DETAIL of a store failure belongs in the caller's own log, which is where the datahub's
/// root already writes one (its polymarket egress read is the precedent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryTokenError {
    /// The store holds no practice-tier API key (absent or blank).
    NotConfigured,
    /// The credential store could not be read at all.
    StoreUnreadable,
}

impl fmt::Display for HistoryTokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HistoryTokenError::NotConfigured => {
                let names = oanda_history_token_names();
                let key = names.first().map_or("the practice API key", String::as_str);
                write!(
                    f,
                    "oanda history needs the practice account's API token, and this data server \
                     holds none: set {key} on the data server's own box (`vike-cli secrets set \
                     {key}`). It is read when a Backfill arrives, so storing it needs no restart. \
                     Only the practice key is read for history; a live-tier key is never used"
                )
            }
            HistoryTokenError::StoreUnreadable => f.write_str(
                "the credential store on this data server could not be read, so the OANDA history \
                 token is unavailable (the cause is in the data server's log)",
            ),
        }
    }
}

impl std::error::Error for HistoryTokenError {}

/// **OANDA's history source** — the `KlineSource` the datahub dispatches an OANDA `Backfill` to.
///
/// Cheap to build and to clone: it holds the token provider and nothing else. Every fetch through
/// any instance shares one process-wide request budget and one connection pool (module doc).
#[derive(Clone)]
pub struct OandaKlines {
    token: HistoryTokenProvider,
}

impl OandaKlines {
    /// A source that obtains its token from `token` when a fetch starts.
    pub fn new(token: HistoryTokenProvider) -> Self {
        Self { token }
    }
}

impl fmt::Debug for OandaKlines {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The provider is a closure with no `Debug`, and it is the one thing here that leads to a
        // credential — so nothing about it is printed.
        f.debug_struct("OandaKlines").finish_non_exhaustive()
    }
}

impl KlineSource for OandaKlines {
    fn venue(&self) -> &str {
        VENUE
    }

    /// Page `[start_ms, end_ms]` of `(symbol, interval)` mid candles, oldest first.
    ///
    /// `Refused` — before the token is read or anything is sent — for an interval OANDA cannot
    /// express or one this source cannot page, and for a symbol that is not an instrument name.
    /// A missing token is `Refused` too. Every other failure is `Fetch`, and none of them can carry
    /// the token.
    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        self.fetch_over(&mut LiveWire::shared(), symbol, interval, start_ms, end_ms)
    }
}

impl OandaKlines {
    /// [`KlineSource::fetch`] over an injected [`Wire`]: the whole fetch — refusals, the token, the
    /// URL, paging, retries, the scrub — with the network, the clock and the budget replaced.
    fn fetch_over(
        &self,
        wire: &mut impl Wire,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        // Everything that can be refused is refused BEFORE the token is read and before anything is
        // sent: a refusal is a statement that the venue was never asked.
        let grain = grain_for(interval)?;
        let instrument = instrument_for(symbol)?;
        if end_ms < start_ms {
            return Ok(Vec::new());
        }
        // A provider that fails says why in its OWN words; a provider that answers with a blank token
        // has said "none", which is `NotConfigured`'s meaning too.
        let token = (self.token)().map_err(|e| SourceError::Refused(e.to_string()))?;
        let secret = Secret::new(token)
            .ok_or_else(|| SourceError::Refused(HistoryTokenError::NotConfigured.to_string()))?;
        let base = history_base();
        walk_pages(&grain, &instrument, start_ms, end_ms, PAGE_COUNT, |path| {
            request_with(&secret, &format!("{base}{path}"), wire)
        })
    }
}

// ------------------------------------------------------------------------------------------------
// the interval vocabulary
// ------------------------------------------------------------------------------------------------

/// What one interval means on the wire: OANDA's granularity code and the cursor step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grain {
    /// OANDA's code (`S5`, `M1`, `H4`, `D`).
    granularity: &'static str,
    /// The width of one candle in seconds — the step the paging cursor advances by.
    step_s: i64,
}

/// A vike interval → OANDA's granularity code, for HISTORY.
///
/// The four sub-minute rows are the probe's subject and live here alone; every other row is the live
/// table's, by delegation, so a change to `granularity` reaches history without a second edit (and a
/// sub-minute interval never reaches the LIVE feed — module doc).
fn history_granularity(interval: &str) -> Option<&'static str> {
    match interval {
        "5s" => Some("S5"),
        "10s" => Some("S10"),
        "15s" => Some("S15"),
        "30s" => Some("S30"),
        other => granularity(other),
    }
}

/// The [`Grain`] of `interval`, or the reason this source cannot express it.
fn grain_for(interval: &str) -> Result<Grain, SourceError> {
    let Some(code) = history_granularity(interval) else {
        return Err(SourceError::Refused(format!(
            "oanda history: {interval:?} is not an interval OANDA serves candles for"
        )));
    };
    // The step is the interval's own width, from the one parser every collector's forming-bar guard
    // shares. `1w` and `1mo` have none — a week and a month are calendar widths.
    match vike_model::time::interval_ms(interval) {
        Some(ms) if ms >= 1000 && ms % 1000 == 0 => {
            Ok(Grain { granularity: code, step_s: ms / 1000 })
        }
        _ => Err(SourceError::Refused(format!(
            "oanda history: {interval:?} candles are calendar-width, and this source pages by a \
             fixed step. Fetch 1d and resample"
        ))),
    }
}

/// The OANDA instrument name for a store symbol — `EUR_USD` for `EUR_USD` or `eurusd` — or a refusal.
///
/// The name is spliced into a request PATH, so anything outside letters, digits and `_` is refused:
/// a `/`, `?` or `#` would let a symbol steer a request carrying a bearer token to some other
/// endpoint of the same host.
fn instrument_for(symbol: &str) -> Result<String, SourceError> {
    let instrument = to_oanda_instrument(symbol);
    let well_formed = !instrument.is_empty()
        && instrument.len() <= MAX_INSTRUMENT_LEN
        && instrument.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if well_formed {
        return Ok(instrument);
    }
    let shown: String = symbol.chars().take(40).collect();
    Err(SourceError::Refused(format!(
        "oanda history: {shown:?} is not an OANDA instrument name (letters, digits and `_`, like \
         EUR_USD)"
    )))
}

// ------------------------------------------------------------------------------------------------
// the wire: one request, its retry policy, and everything they touch outside the process
// ------------------------------------------------------------------------------------------------

/// The bearer token, from the moment the provider hands it over until the fetch ends.
///
/// Never empty (see [`Secret::new`]), so [`Secret::redact`] can replace it without the empty-pattern
/// case that would splice a mask between every character. `Debug` prints a mask; there is no
/// `Display`.
struct Secret(String);

impl Secret {
    /// `None` for a blank token: nothing to send and nothing to redact.
    fn new(raw: String) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        Some(Self(trimmed.to_string()))
    }

    /// The token, for the one place that puts it in a header.
    fn expose(&self) -> &str {
        &self.0
    }

    /// `text` with every occurrence of the token masked — the ONE place wire text is cleaned.
    fn redact(&self, text: &str) -> String {
        text.replace(self.0.as_str(), "***")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// What one HTTP attempt came back with — the seam [`request_with`] retries over.
enum Attempt {
    /// The server answered: its status, the body, and the wait its `Retry-After` stated in seconds.
    Response { status: u16, body: String, retry_after: Option<Duration> },
    /// No usable answer at all — DNS, connect, TLS, a reset, a timeout, a body cut off mid-read.
    Failed(String),
}

/// Everything a fetch does to the outside world, so the paging and the retry policy above it run
/// with neither a network nor a clock.
trait Wire {
    /// Take one slot of the process-wide request budget; blocks while it is spent.
    fn pace(&mut self);
    /// One GET of `url` with `token` as the bearer credential. No retry.
    fn get(&mut self, url: &str, token: &Secret) -> Attempt;
    /// Wait `wait`.
    fn sleep(&mut self, wait: Duration);
}

/// The real wire: the process-wide gate, the shared connection pool and the thread clock.
struct LiveWire {
    agent: &'static ureq::Agent,
    gate: &'static RateGate,
}

impl LiveWire {
    fn shared() -> Self {
        Self { agent: history_agent(), gate: history_gate() }
    }
}

impl Wire for LiveWire {
    fn pace(&mut self) {
        self.gate.proceed_logged(VENUE, "history candles");
    }

    fn get(&mut self, url: &str, token: &Secret) -> Attempt {
        let owned = request_headers(token);
        let headers: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
        match get_raw(self.agent, url, "oanda candles", &headers) {
            Ok(raw) => Attempt::Response {
                status: raw.status,
                body: raw.body,
                retry_after: raw.retry_after,
            },
            Err(e) => Attempt::Failed(e),
        }
    }

    fn sleep(&mut self, wait: Duration) {
        std::thread::sleep(wait);
    }
}

/// The two headers every candles request carries: the bearer credential, and the UNIX datetime
/// format that makes a candle's `time` arrive as epoch seconds — `epoch_ms` refuses the RFC 3339
/// spelling, so a request without it fails every page rather than storing a wrong time. A function of
/// its own so the pair is testable without a network.
fn request_headers(token: &Secret) -> [(&'static str, String); 2] {
    [
        ("Authorization", format!("Bearer {}", token.expose())),
        ("Accept-Datetime-Format", "UNIX".to_string()),
    ]
}

/// The practice host — the only host this module knows. `Environment::Demo` is spelled out so that
/// no other value can reach `oanda_hosts` from here.
fn history_base() -> &'static str {
    oanda_hosts(Environment::Demo).0
}

/// The one place the request budget is spelled: [`MAX_REQUESTS_PER_SECOND`] per second.
fn new_history_gate() -> RateGate {
    RateGate::new(MAX_REQUESTS_PER_SECOND, Duration::from_secs(1))
}

/// The process-wide request budget every fetch shares.
fn history_gate() -> &'static RateGate {
    static GATE: OnceLock<RateGate> = OnceLock::new();
    GATE.get_or_init(new_history_gate)
}

/// The process-wide connection pool every fetch shares.
fn history_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(blocking_agent)
}

/// Is `status` worth retrying: a rate limit or any server-side failure?
fn is_transient(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

/// The operator-facing text of a failed response: OANDA's `errorMessage` when the body has one,
/// else the body itself — masked of the token FIRST, then cut to its head, so a token that straddles
/// the cut cannot leave a readable prefix.
fn failure_text(secret: &Secret, body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("errorMessage").and_then(serde_json::Value::as_str).map(str::to_string))
        .unwrap_or_else(|| body.to_string());
    body_head(&secret.redact(&message))
}

/// One request with the retry policy: pace, attempt, and on a transient failure wait out the next of
/// [`RETRY_DELAYS`] — lengthened, never shortened, to a `Retry-After` up to [`RETRY_AFTER_CAP`] — and
/// try again. The last failure is not slept after: there is nothing left to wait for.
///
/// Returns the parsed JSON of the first 2xx answer. A non-transient status is an immediate
/// [`OandaApiError`]; exhaustion is one too, carrying the LAST failure. Every string that came off
/// the wire is masked of `secret` here, and nowhere else touches wire text.
fn request_with(
    secret: &Secret,
    url: &str,
    wire: &mut impl Wire,
) -> Result<serde_json::Value, OandaApiError> {
    let mut last = String::new();
    let mut last_status = 0u16;
    let delays = RETRY_DELAYS.iter().map(Some).chain(std::iter::once(None));
    for (n, delay) in delays.enumerate() {
        wire.pace();
        let mut stated = Duration::ZERO;
        match wire.get(url, secret) {
            Attempt::Response { status, body, .. } if (200..300).contains(&status) => {
                match serde_json::from_str(&body) {
                    Ok(value) => return Ok(value),
                    Err(e) => {
                        last_status = status;
                        last = format!("HTTP {status} with a body that is not JSON ({e})");
                    }
                }
            }
            Attempt::Response { status, body, retry_after } => {
                let text = failure_text(secret, &body);
                if !is_transient(status) {
                    return Err(OandaApiError { status, message: text });
                }
                last_status = status;
                last = if text.is_empty() {
                    format!("HTTP {status}")
                } else {
                    format!("HTTP {status}: {text}")
                };
                stated = retry_after.unwrap_or(Duration::ZERO).min(RETRY_AFTER_CAP);
            }
            Attempt::Failed(e) => {
                last_status = 0;
                last = secret.redact(&e);
            }
        }
        if let Some(delay) = delay {
            let wait = (*delay).max(stated);
            // Numbers only — nothing from the wire reaches this line.
            tracing::warn!(
                target: "vike_oanda::klines",
                status = last_status,
                attempt = n + 1,
                wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                "OANDA candles request failed transiently; retrying"
            );
            wire.sleep(wait);
        }
    }
    Err(OandaApiError {
        status: last_status,
        message: format!("{last} (after {} attempts)", RETRY_DELAYS.len() + 1),
    })
}

// ------------------------------------------------------------------------------------------------
// one page of candles, and the walk over pages
// ------------------------------------------------------------------------------------------------

/// The request path and query for one page.
fn candles_path(instrument: &str, code: &str, from_s: i64, count: usize) -> String {
    format!(
        "/v3/instruments/{instrument}/candles?granularity={code}\
         &price=M&from={from_s}&count={count}"
    )
}

/// An OANDA `time` under `Accept-Datetime-Format: UNIX` (`"1478012400.000000000"`) → epoch ms, by
/// INTEGER arithmetic: the whole seconds and the first three fractional digits, the rest dropped. The
/// result is exact by construction and FLOORS sub-millisecond digits, where the float parse
/// `bar_from_candle` uses can round a time just under a millisecond boundary UP to the next one.
///
/// `None` for anything that is not unsigned digits with an optional fractional part — including the
/// RFC 3339 spelling, so a venue that ignored the header FAILS the page rather than stamping zeros.
fn epoch_ms(time: &str) -> Option<i64> {
    let (secs, frac) = time.split_once('.').unwrap_or((time, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if secs.is_empty() || !digits(secs) || !digits(frac) {
        return None;
    }
    let secs: i64 = secs.parse().ok()?;
    let millis = frac
        .bytes()
        .chain(std::iter::repeat(b'0'))
        .take(3)
        .fold(0i64, |acc, d| acc * 10 + i64::from(d - b'0'));
    secs.checked_mul(1000)?.checked_add(millis)
}

/// One decoded page.
struct Page {
    /// Every COMPLETE candle, in the venue's order (strictly increasing — [`decode_page`] checks).
    bars: Vec<Bar>,
    /// The time of the last candle of ANY completeness, in ms — where the cursor moves from.
    last_ms: Option<i64>,
    /// Whether that last candle is still forming: the live edge, with nothing after it.
    last_forming: bool,
    /// Candles received, complete or not — what "a short page" is measured on.
    total: usize,
}

/// Decode a candles response. Strict where a shortcut would store something wrong: a candle without a
/// parseable time, a COMPLETE candle without usable mid prices, a non-finite price, or candles out of
/// order fail the page — the retry layer has already ruled out a garbled body, so this is a
/// response the venue really sent.
fn decode_page(v: &serde_json::Value) -> Result<Page, String> {
    let candles = v
        .get("candles")
        .and_then(serde_json::Value::as_array)
        .ok_or("the response has no `candles` array")?;
    let mut page = Page {
        bars: Vec::with_capacity(candles.len()),
        last_ms: None,
        last_forming: false,
        total: 0,
    };
    for candle in candles {
        let ts = candle
            .get("time")
            .and_then(serde_json::Value::as_str)
            .and_then(epoch_ms)
            .ok_or("a candle has no parseable `time`")?;
        if page.last_ms.is_some_and(|prev| ts <= prev) {
            return Err(format!("candle {ts} is not after the one before it in the same page"));
        }
        let complete = candle.get("complete").and_then(serde_json::Value::as_bool) == Some(true);
        page.total += 1;
        page.last_ms = Some(ts);
        page.last_forming = !complete;
        if !complete {
            continue;
        }
        // The one candle -> `Bar` decode this crate has (the live feed's too), with the time replaced:
        // its own is a float parse, and its "no time" is a silent zero.
        let mut bar = bar_from_candle(candle)
            .ok_or_else(|| format!("complete candle {ts} has no usable `mid` prices"))?;
        if ![bar.open, bar.high, bar.low, bar.close].iter().all(|p| p.is_finite()) {
            return Err(format!("complete candle {ts} carries a non-finite price"));
        }
        bar.ts = ts;
        page.bars.push(bar);
    }
    Ok(page)
}

/// Page `[start_ms, end_ms]` (inclusive) of `instrument` candles through `get`, which is handed one
/// request path and query at a time and answers with the parsed response.
///
/// `page_count` is the `count` of every request and the yardstick for a short page; production
/// passes [`PAGE_COUNT`]. The bars come back oldest first, each candle once, inside the window.
fn walk_pages(
    grain: &Grain,
    instrument: &str,
    start_ms: i64,
    end_ms: i64,
    page_count: usize,
    mut get: impl FnMut(&str) -> Result<serde_json::Value, OandaApiError>,
) -> Result<Vec<Bar>, SourceError> {
    let mut out: Vec<Bar> = Vec::new();
    let mut last_kept = i64::MIN;
    // Whole seconds, rounded DOWN so the window's first partial second is covered (the window filter
    // below drops what precedes it); no candle predates the epoch, so a negative start clamps.
    let mut from_s = start_ms.div_euclid(1000).max(0);
    loop {
        let path = candles_path(instrument, grain.granularity, from_s, page_count);
        let body = get(&path)
            .map_err(|e| SourceError::Fetch(format!("{e} (candles page from={from_s})")))?;
        let page = decode_page(&body).map_err(|e| {
            SourceError::Fetch(format!("oanda history: {e} (candles page from={from_s})"))
        })?;
        for bar in page.bars {
            // Outside the window, or at or before the last kept candle: a seam duplicate.
            if bar.ts < start_ms || bar.ts > end_ms || bar.ts <= last_kept {
                continue;
            }
            last_kept = bar.ts;
            out.push(bar);
        }
        let Some(last_ms) = page.last_ms else { break };
        if page.last_forming || last_ms >= end_ms || page.total < page_count {
            break;
        }
        let next = last_ms.div_euclid(1000) + grain.step_s;
        if next <= from_s {
            return Err(SourceError::Fetch(format!(
                "oanda history: the candles endpoint made no progress past from={from_s} (its last \
                 candle was at {last_ms} ms)"
            )));
        }
        from_s = next;
    }
    Ok(out)
}

#[path = "klines_tests.rs"]
#[cfg(test)]
mod klines_tests;

#[path = "klines_props.rs"]
#[cfg(test)]
mod klines_props;
