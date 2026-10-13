//! **The tool-feeds lane: the news, the economic calendar, and the equity calendars (earnings,
//! dividends, IPOs) this process HOLDS and polls — what
//! [`Request::NewsFeed`](vike_datahub_client::proto::Request::NewsFeed),
//! [`Request::EconomicCalendar`](vike_datahub_client::proto::Request::EconomicCalendar) and
//! [`Request::EquityCalendar`](vike_datahub_client::proto::Request::EquityCalendar) answer from.**
//!
//! The desktop's News, Economic Calendar, Earnings, Dividends and IPO windows used to call sixteen
//! RSS providers, ForexFactory, Finnhub, FMP and Nasdaq with `ureq` from the UI process, each
//! desktop on its own, with the provider keys on the desktop. They now ask this daemon, which does
//! the provider work ONCE for every client, with the keys on THIS box. The answer SHAPE and the
//! week rule both ends need are `crates/vike-datahub-client/src/tool_feeds.rs`'s; what lives here is
//! everything only the server can have. [`crate::options`] is the model, and its rule is the rule:
//! the TYPE is feature-free, the transport that names the providers is not.
//!
//! # What is feature-free and what is not
//!
//! The verbs must DECODE and ANSWER on every build — a build without the lane answers `NotArmed` —
//! so [`ToolFeedsLane`], the parsers (`news`, `econ`, `equity`), the poller cache (`worker`), the
//! once-per-transition failure log (`health`) and the
//! two seams ([`FeedHttp`], [`KeyReader`]) are compiled everywhere, and the parsers' tests run in the
//! default `test` job over scripted seams. Only `http` — the one module that links `ureq` and names
//! the providers' real URLs in a transport — sits behind `tool-feeds-serve`.
//!
//! # ⚠ It polls NOTHING until somebody asks
//!
//! Each feed's poll thread is started by the first request for that feed (`worker`'s module doc), so
//! a datahub whose clients never open the tool windows spends no provider budget, and an entry
//! nobody asks for within two hours stops being polled.
//!
//! # The bounds, every one a server constant
//!
//! | bound | where |
//! |---|---|
//! | which providers | [`Endpoints`] (the production table: [`news::FEEDS`], one calendar URL, three bases) |
//! | news cadence / calendar cadence | [`NEWS_CADENCE`] / [`CALENDAR_CADENCE`], the equity feeds' [`EQUITY_CADENCE`] |
//! | retry after a failure | [`MIN_RETRY`], doubling to the cadence; an absent key is re-read every [`KEY_RECHECK`] |
//! | how many items / how long a summary | `vike_datahub_client::tool_feeds::{MAX_NEWS_ITEMS, NEWS_SUMMARY_CHARS}` |
//! | which equity weeks | `vike_datahub_client::tool_feeds::validate_equity_week`, `EQUITY_WEEKS_AHEAD` either side |
//!
//! A request SELECTS from what is held and never moves a provider call: no request names a URL, a key
//! or a cadence, and the equity week the poller fetches is a validated Monday — digits and dashes,
//! so it cannot shape a URL. That is why the three verbs are `Observe` and why the keyed providers'
//! budget is the operator's to spend on the server's schedule (decision 0097's reopener, argued in
//! the decision record for this lane).

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use vike_datahub_client::tool_feeds::{
    CalEvent, DividendRow, EarningsRow, EquityCalendarReply, IpoRow, NewsItem, ToolFeedOutcome,
};

pub mod econ;
pub mod equity;
pub mod health;
#[cfg(feature = "tool-feeds-serve")]
pub mod http;
pub mod news;
pub mod worker;

use self::worker::{FeedWorker, GenerationBoard, Schedule};

/// The credential-store key of the Finnhub earnings key — the NAME only; a value is never held in a
/// `const`. ⚠ The settings registry reads this literal as THIS crate reading the credential
/// (`crates/vike-config/src/settings/rows/daemons.rs`).
pub const FINNHUB_API_KEY: &str = "FINNHUB_API_KEY";

/// The credential-store key of the FMP dividends key (see [`FINNHUB_API_KEY`]).
pub const FMP_API_KEY: &str = "FMP_API_KEY";

/// How long after a good news poll the next one is due (the desktop's).
pub const NEWS_CADENCE: Duration = Duration::from_secs(300);

/// How long after a good ForexFactory poll the next one is due (the desktop's).
pub const CALENDAR_CADENCE: Duration = Duration::from_secs(1800);

/// How long after a good Finnhub / FMP / Nasdaq poll the next one is due (the desktop's).
pub const EQUITY_CADENCE: Duration = Duration::from_secs(1800);

/// The first retry after a failed poll (and after an empty IPO answer, the desktop's 60 s); doubles
/// per consecutive failure up to the feed's cadence.
pub const MIN_RETRY: Duration = Duration::from_secs(60);

/// How often an ABSENT key is looked for again — a local store read, no provider call — so storing
/// a key takes effect within the minute without a restart.
pub const KEY_RECHECK: Duration = Duration::from_secs(60);

/// A credential VALUE, held only long enough to put it on the wire. No `Display`, and a `Debug`
/// that prints no value: a key must never reach a log line, an error string or a panic message.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for the one place that builds the request carrying it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// What reading one credential name came to — three states, never folded (a store that exists and
/// cannot be read is not "no key": that is a permissions bug wearing the unconfigured answer).
#[derive(Debug)]
pub enum KeyRead {
    /// The store holds a non-blank value.
    Present(Secret),
    /// The store answered and holds none (or there is no store at all).
    Absent,
    /// The store exists and would not read; the reader has logged why.
    Unreadable,
}

/// **The scoped credential read, as the lane sees it**: ask for ONE name, get one answer. A trait so
/// the lane's key handling is tested over a scripted reader, and so the lane holds no store.
pub trait KeyReader: Send + Sync {
    fn read(&self, name: &str) -> KeyRead;
}

/// **The real reader: a scoped read of the settings database, one name at a time.**
///
/// Decision 0097's pattern, narrowed to this lane: the read declares exactly the name it is asked
/// for (`vike_secrets::KeyScope::of([name])`), so the database query binds that one name and selects
/// no other row, and nothing is held between calls — the poller asks again each cycle, so a key
/// stored, rotated or removed after start is what the next poll sees. A directory-less process has
/// no store, which is `Absent` (the live gate's answer). The value reaches no log line: the lines
/// this logs carry the store's error text (paths and OS reasons) and the NAME.
pub struct StoreKeys {
    store: vike_secrets::CredentialStore,
}

impl StoreKeys {
    /// Over the boot's credential store handle (`vike_boot::Booted::store`); a handle with no
    /// location is the directory-less process.
    pub fn new(store: vike_secrets::CredentialStore) -> Self {
        Self { store }
    }
}

impl KeyReader for StoreKeys {
    fn read(&self, name: &str) -> KeyRead {
        if self.store.location().is_none() {
            return KeyRead::Absent;
        }
        let scope = vike_secrets::KeyScope::of([name]);
        match self.store.read(&scope) {
            Ok(scoped) => match scoped.get(name).declared() {
                Ok(Some(value)) if !value.trim().is_empty() => {
                    KeyRead::Present(Secret::new(value.trim()))
                }
                Ok(_) => KeyRead::Absent,
                Err(e) => {
                    // Unreachable (the scope declares `name`), but a defect must not read as absent.
                    tracing::error!(error = %e, "vike-datahub tool feeds: a credential read left its own scope");
                    KeyRead::Unreadable
                }
            },
            Err(e) => {
                tracing::error!(
                    error = %e,
                    key = name,
                    "the credential store could not be read for a tool feed's API key; the feed keeps \
                     its last good rows, and this is NOT the same as the key being absent"
                );
                KeyRead::Unreadable
            }
        }
    }
}

/// **One GET** the lane asks its transport to make. No `Debug`: a request may carry a credential
/// (a header value or a query parameter) and none must ever be formatted into a log line.
pub struct HttpGet {
    url: String,
    headers: Vec<(&'static str, String)>,
    timeout: Duration,
}

/// The browser `User-Agent` ForexFactory's CDN and Nasdaq need (the desktop's): both stall or refuse
/// a bare client.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0 Safari/537.36";

impl HttpGet {
    pub fn new(url: impl Into<String>, timeout: Duration) -> Self {
        Self { url: url.into(), headers: Vec::new(), timeout }
    }

    /// The browser `User-Agent` and a JSON `Accept` (ForexFactory, Nasdaq).
    #[must_use]
    pub fn with_browser_headers(self) -> Self {
        self.with_header("User-Agent", BROWSER_USER_AGENT).accepting_json()
    }

    /// A JSON `Accept` only.
    #[must_use]
    pub fn accepting_json(self) -> Self {
        self.with_header("Accept", "application/json, text/plain, */*")
    }

    #[must_use]
    pub fn with_header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn headers(&self) -> &[(&'static str, String)] {
        &self.headers
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// **The source seam: how the lane reaches a provider.** `Ok` is a 2xx body as text; `Err` is a
/// short reason that names no URL and no credential. The real implementation is `http::UreqHttp`
/// (`tool-feeds-serve`); tests script it, or stand it in front of a loopback server.
pub trait FeedHttp: Send + Sync {
    fn get(&self, request: &HttpGet) -> Result<String, String>;
}

/// **The provider addresses**, injectable so a test points the whole lane at a loopback stand-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// The news RSS providers.
    pub news: Vec<news::NewsFeedSpec>,
    /// ForexFactory's weekly calendar JSON, the whole URL.
    pub calendar_url: String,
    /// Finnhub's origin (`https://finnhub.io`); the lane appends `/api/v1/calendar/earnings?..`.
    pub finnhub_base: String,
    /// FMP's origin; the lane appends `/stable/dividends-calendar?..`.
    pub fmp_base: String,
    /// Nasdaq's origin; the lane appends `/api/ipo/calendar?date=..`.
    pub nasdaq_base: String,
}

impl Endpoints {
    /// The real providers.
    pub fn production() -> Self {
        Self {
            news: news::production_feeds(),
            calendar_url: "https://nfs.faireconomy.media/ff_calendar_thisweek.json".to_string(),
            finnhub_base: "https://finnhub.io".to_string(),
            fmp_base: "https://financialmodelingprep.com".to_string(),
            nasdaq_base: "https://api.nasdaq.com".to_string(),
        }
    }
}

/// **The lane the three verbs read**: one [`FeedWorker`] per feed. Its mere EXISTENCE is the arming —
/// a build without `tool-feeds-serve` mounts none and the verbs answer `NotArmed`.
pub struct ToolFeedsLane {
    news: Arc<FeedWorker<(), NewsItem>>,
    economic: Arc<FeedWorker<(), CalEvent>>,
    earnings: Arc<FeedWorker<String, EarningsRow>>,
    dividends: Arc<FeedWorker<String, DividendRow>>,
    ipos: Arc<FeedWorker<String, IpoRow>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl ToolFeedsLane {
    /// A lane over `http` and `keys` against `endpoints`. `now_ms` is the clock (the lane's only
    /// source of "today" and of every `fetched_at_ms`); `autostart: false` is the test seam that
    /// spawns no thread (see [`FeedWorker::new`]). Nothing is polled by constructing it.
    pub fn new(
        http: Arc<dyn FeedHttp>,
        keys: Arc<dyn KeyReader>,
        endpoints: Endpoints,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        autostart: bool,
    ) -> Self {
        // Seeded from the start time: a generation from a previous run of this daemon equals no
        // number this run mints (`worker`'s module doc).
        let counter = Arc::new(AtomicU64::new(u64::try_from(now_ms()).unwrap_or(0)));
        let single = |cadence: Duration| Schedule {
            cadence,
            min_retry: MIN_RETRY,
            key_recheck: KEY_RECHECK,
            retry_when_empty: false,
        };

        let news_poller = news::NewsPoller::new(endpoints.news.clone(), Arc::clone(&http));
        let news = FeedWorker::new(
            "news",
            single(NEWS_CADENCE),
            GenerationBoard::new(Arc::clone(&counter)),
            Arc::clone(&now_ms),
            autostart,
            move |_: &()| news_poller.poll(),
        );

        let (calendar_http, calendar_url) = (Arc::clone(&http), endpoints.calendar_url.clone());
        let calendar_health = health::ProviderHealth::new();
        let economic = FeedWorker::new(
            "economic",
            single(CALENDAR_CADENCE),
            GenerationBoard::new(Arc::clone(&counter)),
            Arc::clone(&now_ms),
            autostart,
            move |_: &()| econ::poll(calendar_http.as_ref(), &calendar_health, &calendar_url),
        );

        // The three equity feeds of one week serve ONE generation (`worker`'s module doc).
        let week_board = GenerationBoard::new(Arc::clone(&counter));
        let (earn_http, earn_keys, finnhub) =
            (Arc::clone(&http), Arc::clone(&keys), endpoints.finnhub_base.clone());
        let earn_health = health::ProviderHealth::new();
        let earnings = FeedWorker::new(
            "earnings",
            single(EQUITY_CADENCE),
            Arc::clone(&week_board),
            Arc::clone(&now_ms),
            autostart,
            move |week: &String| {
                equity::poll_earnings(
                    earn_http.as_ref(),
                    earn_keys.as_ref(),
                    &earn_health,
                    &finnhub,
                    week,
                )
            },
        );
        let (div_http, div_keys, fmp) =
            (Arc::clone(&http), Arc::clone(&keys), endpoints.fmp_base.clone());
        let div_health = health::ProviderHealth::new();
        let dividends = FeedWorker::new(
            "dividends",
            single(EQUITY_CADENCE),
            Arc::clone(&week_board),
            Arc::clone(&now_ms),
            autostart,
            move |week: &String| {
                equity::poll_dividends(
                    div_http.as_ref(),
                    div_keys.as_ref(),
                    &div_health,
                    &fmp,
                    week,
                )
            },
        );
        let (ipo_http, ipo_poller) =
            (Arc::clone(&http), equity::IpoPoller::new(endpoints.nasdaq_base));
        let ipos = FeedWorker::new(
            "ipos",
            Schedule { retry_when_empty: true, ..single(EQUITY_CADENCE) },
            week_board,
            Arc::clone(&now_ms),
            autostart,
            move |week: &String| ipo_poller.poll(ipo_http.as_ref(), week),
        );

        Self { news, economic, earnings, dividends, ipos, now_ms }
    }

    /// The news feed's current state, for a request carrying `if_generation`.
    pub fn news(&self, if_generation: Option<u64>) -> ToolFeedOutcome<NewsItem> {
        self.news.read(&(), if_generation)
    }

    /// The economic calendar's current state.
    pub fn economic_calendar(&self, if_generation: Option<u64>) -> ToolFeedOutcome<CalEvent> {
        self.economic.read(&(), if_generation)
    }

    /// The three equity feeds' current state for `week_start`, a Monday the caller has already
    /// validated against [`Self::today_iso`] (`validate_equity_week`): nothing else may reach the
    /// pollers, whose URLs it is part of.
    pub fn equity_calendar(
        &self,
        week_start: &str,
        if_generation: Option<u64>,
    ) -> EquityCalendarReply {
        // ⚠ THREE reads, no common lock: a poll landing between them would leave one feed answered
        // at the old generation and another at the new, and a client holding the newer number would
        // then be told "unchanged" about rows it never received. The week's feeds share one board, so
        // a consistent reply is one whose generations all agree; a change is rare (a poll per half
        // hour), so one more pass settles it.
        let mut reply = self.read_equity(week_start, if_generation);
        for _ in 0..EQUITY_CONSISTENCY_RETRIES {
            if equity_generations_agree(&reply) {
                break;
            }
            reply = self.read_equity(week_start, if_generation);
        }
        reply
    }

    fn read_equity(&self, week_start: &str, if_generation: Option<u64>) -> EquityCalendarReply {
        let week = week_start.to_string();
        EquityCalendarReply {
            earnings: self.earnings.read(&week, if_generation),
            dividends: self.dividends.read(&week, if_generation),
            ipos: self.ipos.read(&week, if_generation),
        }
    }

    /// The server's current UTC date, ISO — what the equity week rule measures from.
    pub fn today_iso(&self) -> String {
        match chrono::DateTime::from_timestamp_millis((self.now_ms)()) {
            Some(now) => now.format("%Y-%m-%d").to_string(),
            None => String::new(),
        }
    }

    /// Whether any feed's poll thread has started — false until the first request (a test probe of
    /// the lazy-start rule).
    pub fn any_started(&self) -> bool {
        self.news.started()
            || self.economic.started()
            || self.earnings.started()
            || self.dividends.started()
            || self.ipos.started()
    }
}

/// How many times [`ToolFeedsLane::equity_calendar`] re-reads a reply whose feeds disagree about the
/// week's generation before giving up and sending it.
const EQUITY_CONSISTENCY_RETRIES: usize = 4;

/// Whether every outcome of `reply` that carries a generation carries the same one.
fn equity_generations_agree(reply: &EquityCalendarReply) -> bool {
    let generations =
        [reply.earnings.generation(), reply.dividends.generation(), reply.ipos.generation()];
    let mut present = generations.iter().flatten();
    match present.next() {
        Some(first) => present.all(|g| g == first),
        None => true,
    }
}
