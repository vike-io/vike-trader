//! **The datahub poll**: the desktop's News, Calendar and Options windows' CLIENT half — what they
//! ask the backend's datahub for, how often, and how an answer lands in [`DatahubPollSnapshot`].
//! ONE poll serves all three (decision D57 of the top-layers plan): one poller type
//! ([`DatahubPoller`]), one held connection (`connection`'s `DatahubPollConnection`), one link and
//! streak ([`crate::link`]), one dial ([`crate::dial::DatahubDial`]) and one snapshot, run on two
//! threads ([`spawn::spawn_tool_fetchers`]): the News and Calendar half (this module) and the
//! Options half ([`options`]), each on its own cadence.
//!
//! # Where the data comes from
//!
//! The desktop fetches NO provider (owner ruling, 2026-10-12; the Options tool's decision, 0128,
//! extended to the tool feeds). The datahub polls the news RSS feeds, the ForexFactory week, Finnhub
//! (earnings), FMP (dividends) and Nasdaq (IPOs) on ITS cadence, with the provider keys held on ITS
//! box, and this side asks `Request::NewsFeed`, `Request::EconomicCalendar` and
//! `Request::EquityCalendar` and merges the answers (`connection` is the connection).
//! What stays in the desktop is the DISPLAY layer, in the two windows' crates: the topic classifier
//! (`vike-news`), the country and flag of a currency and every date label in the viewer's LOCAL
//! timezone (`vike-calendar`), derived from the raw instant the server sends.
//!
//! ⚠ **There is NO fallback.** An old server, a lane its operator left unarmed, a key the server
//! does not hold, a refusal, no backend and a dead link each put ONE sentence in the feed's status
//! field and keep the last good rows; nothing ever fetches a provider instead (owner ruling).
//!
//! # What is asked: the interest
//!
//! [`DatahubPollLink`] is the UI-to-poll-thread half. Every frame the News window draws it records
//! [`DatahubPollInterest::News`], and every frame the Calendar window draws (any of its four pages:
//! the day-strip counts need all four feeds) it records [`DatahubPollInterest::Calendar`] and the
//! Monday it shows ([`DatahubPollLink::show_week`]). An interest, and a shown week, lives
//! [`INTEREST_TTL`] without being recorded again: a closed window stops being asked for within that,
//! and with no window open the thread dials nothing at all (the datahub, symmetrically, polls nothing until the first
//! request). A NEW interest wakes the thread at once; the Refresh pill ([`DatahubPollLink::refresh`])
//! forces every live feed due now.
//!
//! # Cadence
//!
//! [`POLL_EVERY`]: one ask per feed a minute while its window is open. The server answers from its
//! own cache, so a pass reads that box's memory and never a provider; the cache moves on the
//! server's cadence (by default news 5 min and calendars 30 min, and the operator's to change: each
//! `Snapshot` reports it), and `if_generation` makes an unmoved feed one small frame. A first poll still in flight on the server (`Warming`) is re-asked after
//! [`WARMING_RETRY`]. The thread itself wakes every [`FEEDS_TICK`] to see what is due.
//!
//! # Last good
//!
//! A `Snapshot` REPLACES the feed's rows; every other outcome (and every failed request) keeps the
//! rows the tool holds and replaces only the status sentence, so a refusal never blanks a window.
//! A failed LINK (a dropped tunnel, a reset socket, a restarted datahub) is transient: for the first
//! [`crate::link::QUIET_FAILURES`] passes in a row the sentence is only the quiet
//! reconnecting line, and after that it names the cause ([`crate::link`]). A refusal the
//! datahub answered is its own sentence at once.
//! The status of a held snapshot says how old it is and flags it STALE past [`stale_after`] — three
//! of the server's reported cycles, never less than [`NEWS_STALE_FLOOR`] / [`CALENDAR_STALE_FLOOR`] —
//! or when the server's latest poll reached none of its sources.
//!
//! ⚠ Last good is a rule about ONE server's answers. When the dial names another datahub (a backend
//! switch, a changed key name, no datahub at all), [`DatahubPollState::observe_dial`] clears every row,
//! count and status at once.
//!
//! # One equity calendar per shown week
//!
//! The earnings, dividends and IPOs are asked, held, scheduled and stored PER WEEK, keyed by its ISO
//! Monday (`DatahubPollSnapshot::calendar_weeks`): two Calendar windows on two weeks each draw their own
//! week's rows and statuses. Every week a window shows is asked (the viewer's local week while none
//! has drawn); a week no window shows any more is forgotten with its rows
//! ([`DatahubPollState::keep_weeks`]).
//!
//! # The equity generation
//!
//! `Request::EquityCalendar` carries ONE `if_generation` for its three feeds, so one is sent only
//! when all three feeds are held at the SAME generation; any feed that is not (a `Warming` or
//! `NotConfigured` one) makes the next ask a full one. A non-snapshot answer also forgets the
//! feed's generation, so a restarted server whose counter lands on the same number can never
//! make the tool keep rows the server no longer holds.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
use vike_utility::mutex::lock;

use crate::dial::DatahubDial;
use crate::link::{DatahubLinkStreak, RECONNECTING_LINE};
use crate::tool_feeds::{
    CalendarEconomicEvent, CalendarEquityDividendRow, CalendarEquityEarningsRow,
    CalendarEquityIpoRow, DatahubPollOutcome, NewsItem,
};
use connection::{
    DatahubPollAsk, DatahubPollConnection, DatahubPollPass, DatahubPollReply, FEEDS_REPLY_TIMEOUT,
    failed_line,
};

mod connection;
pub mod options;
pub mod spawn;

/// How often an open window's feeds are asked for again.
///
/// ⚠ The connection HOLDS one datahub connection between these polls (`connection`), so this
/// cadence is a floor under the server's idle read timeout: a connection reaped between two polls
/// costs the next poll an error. The assert below holds it to half of
/// `vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT_MIN`, the shortest value a
/// `config.node_idle_read_timeout_ms` row may name — so lengthening this poll, or lowering that
/// floor, fails to compile here instead of racing every poll against the reap.
pub(crate) const POLL_EVERY: Duration = Duration::from_secs(60);

const _: () = assert!(
    2 * POLL_EVERY.as_millis()
        <= vike_hub_protocol::limits::SERVER_IDLE_READ_TIMEOUT_MIN.as_millis(),
    "POLL_EVERY must stay at most half the shortest idle read timeout a datahub may be configured \
     with, or the held connection of the News and Calendar tools is reaped between its own polls"
);

/// How soon a feed whose first server-side poll has not landed is asked again.
const WARMING_RETRY: Duration = Duration::from_secs(5);

/// How often the poll thread wakes to see what is due (a wake from the link comes sooner).
pub(crate) const FEEDS_TICK: Duration = Duration::from_secs(5);

/// How long a recorded interest stays live without being recorded again: two and a half polls,
/// because the pass's own UI wake is what renews an idle window's interest once per poll.
const INTEREST_TTL: Duration = Duration::from_secs(150);

/// How many of the server's own poll cycles a snapshot may age before it is flagged STALE.
const STALE_AFTER_CYCLES: u64 = 3;

/// A news snapshot is never flagged STALE sooner than this: three of the server's DEFAULT
/// five-minute cycles, which is the limit this tool has always used. A slower server cadence
/// raises it ([`stale_after`]); a faster one does not lower it.
const NEWS_STALE_FLOOR: Duration = Duration::from_secs(15 * 60);

/// A calendar snapshot is never flagged STALE sooner than this: three of the server's DEFAULT
/// half-hour cycles. Raised by a slower server cadence, never lowered ([`stale_after`]).
const CALENDAR_STALE_FLOOR: Duration = Duration::from_secs(90 * 60);

/// **How old a snapshot may be before it is flagged STALE**: [`STALE_AFTER_CYCLES`] of the cadence
/// the server reported on it (`cadence_secs`), but never less than the feed's `floor`.
///
/// The cadence is the operator's to set (`config.datahub_news_poll_secs` and its siblings), and this
/// side polls the server's CACHE every [`POLL_EVERY`]: against a fixed limit, a server polling its
/// providers every hour would have every feed flagged STALE between its own polls. `0` is a server
/// that predates the field and says nothing, which leaves the floor — the limit this tool had before
/// the cadence was reported. At the default cadences the result IS the floor (three cycles), so a
/// box that sets no row is flagged exactly as it always was.
fn stale_after(floor: Duration, cadence_secs: u32) -> Duration {
    floor.max(Duration::from_secs(u64::from(cadence_secs) * STALE_AFTER_CYCLES))
}

/// How a polled feed stands, decided where its status sentence is (never recovered from the
/// sentence): what a view colours the sentence by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DatahubPollHealth {
    /// Not asked yet, or nothing CAN be asked (no backend and no `config.datahub_addr`: not
    /// configured, which is not a fault), or the datahub changed and everything was cleared: no
    /// state to colour.
    #[default]
    Unasked,
    /// A held snapshot, fresh, from sources that answered.
    Live,
    /// A held snapshot that is old, or that the server's latest poll reached no source for.
    Stale,
    /// The server's first poll is still in flight; asked again shortly.
    Warming,
    /// The link to the datahub failed for a pass or two ([`crate::link`]): the last good rows
    /// stay, the poll asks again on its cadence, and the sentence is the quiet reconnecting line.
    Reconnecting,
    /// Nothing to show and a reason why: a refusal, an unarmed lane, a missing key, a dead link, an
    /// old server.
    Unavailable,
}

/// One week's earnings, dividends and IPOs, as the datahub answered them for that week. Each
/// `*_health` is its feed's state as DATA beside its `*_status` sentence (`judge` sets both).
#[derive(Clone, Default)]
pub struct CalendarEquityWeek {
    // earnings/dividends/IPO counts per ISO date — drives the day-card rows
    pub counts: std::collections::HashMap<String, (usize, usize, usize)>,
    pub earnings: Vec<CalendarEquityEarningsRow>, // Finnhub earnings calendar rows (Earnings page)
    pub earnings_status: String,
    pub earnings_health: DatahubPollHealth,
    pub dividends: Vec<CalendarEquityDividendRow>, // FMP dividends calendar rows (Dividends page)
    pub dividends_status: String,
    pub dividends_health: DatahubPollHealth,
    pub ipos: Vec<CalendarEquityIpoRow>, // Nasdaq IPO calendar rows (IPO page)
    pub ipo_status: String,
    pub ipo_health: DatahubPollHealth,
}

/// **What the poll has fetched**: the shared state both poll threads write and every window
/// (News, Calendar, Options, Greeks, the Data Manager's providers plane) reads, as of the datahub's
/// last answers.
#[derive(Clone, Default)]
pub struct DatahubPollSnapshot {
    // News, Calendar, Earnings, Dividends and IPO are the backend datahub's snapshots (this module's
    // doc). Each `*_status` is that feed's ONE sentence: LIVE with its age, or the reason it is not
    // (the server's own words for a refusal), with the last good rows kept beside it.
    pub news: Vec<NewsItem>,
    pub news_status: String,
    /// How the news feed stands, as DATA beside its sentence: the footer's colour reads this and
    /// never the words (`judge` sets both).
    pub news_health: DatahubPollHealth,
    /// The economic calendar's RAW rows (an instant and a currency each); the Calendar window
    /// derives its display rows (country, flag, importance, the day in its zone) where it draws.
    pub calendar: Vec<CalendarEconomicEvent>,
    pub calendar_status: String,
    /// How the economic calendar stands, as DATA beside its sentence (`judge` sets both).
    pub calendar_health: DatahubPollHealth,
    /// Each shown week's earnings, dividends and IPOs, by its ISO Monday: every Calendar window
    /// draws its own week's (this module's doc).
    pub calendar_weeks: std::collections::BTreeMap<String, CalendarEquityWeek>,
    /// The UI's half of the News and Calendar poll thread: the interest those windows record and
    /// the Refresh wake. Every clone is the same link.
    pub feeds_link: DatahubPollLink,
    // Per-underlying options books — the source of truth the Options tool reads, as the backend's
    // datahub last answered them ([`options`]' module doc). `options_by_underlying[u]` holds the
    // chains of the expiries that were ASKED for and the strip of every expiry the server lists;
    // `options_underlyings` is the selector order (the server's listing order).
    pub options_by_underlying: std::collections::BTreeMap<String, vike_marketdata::OptionChainSet>,
    pub options_underlyings: Vec<String>,
    pub options_status: String,
    /// Every deribit option and future by name, as its instrument payload defines it — what the
    /// Greeks tool looks a name up in (never parse). Built by the options poll thread from the
    /// datahub's deribit instrument list; empty until the first list lands, and a name it does not
    /// hold prices as `unpriced`.
    pub options_index: vike_options::OptionIndex,
    /// The UI's half of the options poll thread: the picks the Options window's two tabs record
    /// and the Refresh wake. Every clone is the same link, so the frame's copy of this struct
    /// reaches the thread.
    pub options_link: options::OptionsLink,
}

/// What a window wants kept fresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DatahubPollInterest {
    /// The News window: the headline feed.
    News,
    /// The Calendar window: the economic calendar AND the week's earnings, dividends and IPOs.
    Calendar,
}

/// **The UI's half of the feeds poll thread**: the interests the windows recorded, the Refresh
/// request and the channel that wakes the thread. Cheap to clone, and every clone is the same link,
/// which is why it rides inside [`DatahubPollSnapshot`] and reaches every tool body through the
/// frame's copy of it.
#[derive(Clone)]
pub struct DatahubPollLink {
    wants: Arc<Mutex<HashMap<DatahubPollInterest, Instant>>>,
    /// The Mondays the Calendar windows show (their ← and → move them), each with when it was last
    /// drawn: a shown week lives [`INTEREST_TTL`], as an interest does.
    weeks: Arc<Mutex<HashMap<NaiveDate, Instant>>>,
    force: Arc<AtomicBool>,
    wake: Sender<()>,
}

impl Default for DatahubPollLink {
    /// A link no thread listens on: interests are recorded and every wake is dropped. What
    /// `DatahubPollSnapshot::default()` holds until `spawn_tool_fetchers` installs the live one.
    fn default() -> Self {
        Self::new(std::sync::mpsc::channel().0)
    }
}

impl DatahubPollLink {
    /// A link whose wakes go to `wake` — the receiving half is the poll thread's.
    #[must_use]
    pub fn new(wake: Sender<()>) -> Self {
        Self { wants: Arc::default(), weeks: Arc::default(), force: Arc::default(), wake }
    }

    /// Record that a Calendar window shows the week of `monday`, whose equity calendars are then
    /// asked for. Called every frame it draws, BEFORE its interest; only a week that was not
    /// already shown wakes the thread.
    pub fn show_week(&self, monday: NaiveDate) {
        self.show_week_at(monday, Instant::now());
    }

    /// [`Self::show_week`] at a given instant — the clock is a parameter so the TTL is testable.
    pub(crate) fn show_week_at(&self, monday: NaiveDate, now: Instant) {
        if record(&self.weeks, monday, now) {
            let _ = self.wake.send(());
        }
    }

    /// The Mondays still shown at `now`, earliest first; the lapsed ones are forgotten.
    pub(crate) fn live_weeks(&self, now: Instant) -> Vec<NaiveDate> {
        let mut weeks = still_live(&self.weeks, now);
        weeks.sort_unstable();
        weeks
    }

    /// The Refresh pill: ask every live feed NOW instead of at its next cadence. A closed channel
    /// (the thread gone at shutdown) is ignored, so the UI can never panic on it.
    pub fn refresh(&self) {
        self.force.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
    }

    /// Record that a window shows `interest`. Called every frame the window draws; only an
    /// interest that was not already live wakes the thread.
    pub fn want(&self, interest: DatahubPollInterest) {
        self.want_at(interest, Instant::now());
    }

    /// [`Self::want`] at a given instant — the clock is a parameter so the TTL is testable.
    pub(crate) fn want_at(&self, interest: DatahubPollInterest, now: Instant) {
        if record(&self.wants, interest, now) {
            let _ = self.wake.send(());
        }
    }

    /// The interests still live at `now`; the lapsed ones are forgotten.
    pub(crate) fn live(&self, now: Instant) -> Vec<DatahubPollInterest> {
        still_live(&self.wants, now)
    }

    /// Whether a Refresh was pressed since the last call (and forget it).
    pub(crate) fn take_refresh(&self) -> bool {
        self.force.swap(false, Ordering::SeqCst)
    }
}

/// Record `key` as drawn at `now`; whether it was not already live (the thread is then woken).
fn record<K: Eq + std::hash::Hash>(
    drawn: &Mutex<HashMap<K, Instant>>,
    key: K,
    now: Instant,
) -> bool {
    let mut drawn = lock(drawn);
    let fresh = drawn.get(&key).is_none_or(|at| now.saturating_duration_since(*at) >= INTEREST_TTL);
    drawn.insert(key, now);
    fresh
}

/// The keys drawn within [`INTEREST_TTL`] of `now`; the lapsed ones are forgotten.
fn still_live<K: Copy + Eq + std::hash::Hash>(
    drawn: &Mutex<HashMap<K, Instant>>,
    now: Instant,
) -> Vec<K> {
    let mut drawn = lock(drawn);
    drawn.retain(|_, at| now.saturating_duration_since(*at) < INTEREST_TTL);
    drawn.keys().copied().collect()
}

/// The clock one pass runs on: a monotonic instant for the schedule, the wall clock for a
/// snapshot's age, and the viewer's LOCAL date for the week the equity calendars are asked for.
#[derive(Clone, Copy, Debug)]
pub struct DatahubPollClock {
    /// Schedules the next ask.
    pub now: Instant,
    /// Epoch milliseconds UTC: compared with a snapshot's `fetched_at_ms`.
    pub now_ms: i64,
    /// The viewer's local calendar day: the Monday of its week is the equity week while no
    /// Calendar window names one ([`DatahubPollLink::show_week`]).
    pub today: NaiveDate,
}

impl DatahubPollClock {
    /// The clock right now.
    #[must_use]
    pub fn system() -> Self {
        Self {
            now: Instant::now(),
            now_ms: chrono::Utc::now().timestamp_millis(),
            today: chrono::Local::now().date_naive(),
        }
    }
}

/// What the tool holds of ONE feed from the current server.
#[derive(Debug, Default)]
struct DatahubPollHeld {
    /// The generation of the rows held; sent back as `if_generation`.
    generation: Option<u64>,
    /// What the last snapshot said about itself, to rebuild the status line when a later pass
    /// answers `Unchanged` (and to age it).
    stamp: Option<DatahubPollStamp>,
}

#[derive(Debug)]
struct DatahubPollStamp {
    rows: usize,
    fetched_at_ms: i64,
    sources_ok: u32,
    sources_total: u32,
    /// The server's poll cadence for this feed, as the snapshot reported it (`0`: not said).
    cadence_secs: u32,
}

/// One feed's schedule: what is held and when it is next due (`None`: now).
#[derive(Debug, Default)]
struct DatahubPollSlot {
    held: DatahubPollHeld,
    due: Option<Instant>,
}

/// How a feed is named in a sentence.
struct DatahubPollLabel {
    /// The feed's human name (`DatahubPollOutcome::describe`'s `feed`).
    name: &'static str,
    /// What its rows are called.
    noun: &'static str,
    /// The shortest age that flags a snapshot of this feed STALE ([`stale_after`]'s `floor`).
    stale_floor: Duration,
}

const NEWS: DatahubPollLabel =
    DatahubPollLabel { name: "news", noun: "headlines", stale_floor: NEWS_STALE_FLOOR };
const ECONOMIC: DatahubPollLabel = DatahubPollLabel {
    name: "economic calendar",
    noun: "events",
    stale_floor: CALENDAR_STALE_FLOOR,
};
const EARNINGS: DatahubPollLabel =
    DatahubPollLabel { name: "earnings", noun: "earnings rows", stale_floor: CALENDAR_STALE_FLOOR };
const DIVIDENDS: DatahubPollLabel = DatahubPollLabel {
    name: "dividends",
    noun: "dividend rows",
    stale_floor: CALENDAR_STALE_FLOOR,
};
const IPOS: DatahubPollLabel =
    DatahubPollLabel { name: "IPOs", noun: "IPO rows", stale_floor: CALENDAR_STALE_FLOOR };

/// What the News and Calendar poll thread has learned between passes. The state of the News and
/// Calendar half of [`DatahubPoller`].
#[derive(Debug, Default)]
pub struct DatahubPollState {
    /// The dial (address, device-key epoch) the last pass ran under; `None` before the first pass.
    dial_seen: Option<Option<(String, u64)>>,
    news: DatahubPollSlot,
    economic: DatahubPollSlot,
    /// Each shown week's equity calendars, by its ISO Monday.
    equity: BTreeMap<String, DatahubPollEquitySlot>,
    /// The held connection's run of link failures ([`crate::link`]).
    streak: DatahubLinkStreak,
}

/// One week's equity calendars: when they are next due (`None`: now) and what is held of each.
#[derive(Debug, Default)]
struct DatahubPollEquitySlot {
    due: Option<Instant>,
    earnings: DatahubPollHeld,
    dividends: DatahubPollHeld,
    ipos: DatahubPollHeld,
}

impl DatahubPollEquitySlot {
    /// The one `if_generation` the week's equity request may carry: set only when all three feeds
    /// are held at the SAME generation (see the module doc).
    fn generation(&self) -> Option<u64> {
        let g = self.earnings.generation?;
        (self.dividends.generation == Some(g) && self.ipos.generation == Some(g)).then_some(g)
    }
}

/// The poll's name in its log lines.
const POLL: &str = "news and calendar feeds";

/// Whether a feed due at `due` is asked this pass.
fn is_due(due: Option<Instant>, force: bool, now: Instant) -> bool {
    force || due.is_none_or(|at| now >= at)
}

impl DatahubPollState {
    /// **A different datahub is a different world.** When the dial names another address or device-key
    /// epoch than the last pass saw (a backend switch, a new `config.datahub_addr`, a new device key, or no
    /// datahub at all), everything the old server answered is CLEARED at once, with everything learned from it.
    /// Returns whether it cleared. The first dial a poller ever sees clears nothing.
    pub(crate) fn observe_dial(
        &mut self,
        td: &mut DatahubPollSnapshot,
        dial: Option<&DatahubDial>,
    ) -> bool {
        let identity = dial.map(|d| (d.address.clone(), d.epoch));
        let changed = self.dial_seen.as_ref().is_some_and(|seen| *seen != identity);
        self.dial_seen = Some(identity);
        if !changed {
            return false;
        }
        td.news.clear();
        td.news_status.clear();
        td.news_health = DatahubPollHealth::default();
        td.calendar.clear();
        td.calendar_status.clear();
        td.calendar_health = DatahubPollHealth::default();
        td.calendar_weeks.clear();
        let dial_seen = self.dial_seen.take();
        *self = Self { dial_seen, ..Self::default() };
        true
    }

    /// The equity calendars are per WEEK: keep what is held for each of `weeks` (ISO Mondays, the
    /// ones a Calendar window shows) and forget every other week's schedule, rows, counts and
    /// statuses.
    pub(crate) fn keep_weeks(&mut self, td: &mut DatahubPollSnapshot, weeks: &[&str]) {
        self.equity.retain(|week, _| weeks.contains(&week.as_str()));
        td.calendar_weeks.retain(|week, _| weeks.contains(&week.as_str()));
    }

    /// **This pass's requests**, from the live `interests` and the shown `weeks` (ISO Mondays):
    /// each feed that is due (or all of them, on a forced refresh), the equity calendars once per
    /// week.
    pub(crate) fn plan(
        &self,
        interests: &[DatahubPollInterest],
        force: bool,
        weeks: &[&str],
        now: Instant,
    ) -> Vec<DatahubPollAsk> {
        let mut plan = Vec::new();
        if interests.contains(&DatahubPollInterest::News) && is_due(self.news.due, force, now) {
            plan.push(DatahubPollAsk::News { if_generation: self.news.held.generation });
        }
        if interests.contains(&DatahubPollInterest::Calendar) {
            if is_due(self.economic.due, force, now) {
                plan.push(DatahubPollAsk::Economic {
                    if_generation: self.economic.held.generation,
                });
            }
            for week in weeks {
                let slot = self.equity.get(*week);
                if is_due(slot.and_then(|s| s.due), force, now) {
                    plan.push(DatahubPollAsk::Equity {
                        week_start: (*week).to_string(),
                        if_generation: slot.and_then(DatahubPollEquitySlot::generation),
                    });
                }
            }
        }
        plan
    }

    /// Schedule `ask`'s feed `after` from now.
    fn schedule(&mut self, ask: &DatahubPollAsk, after: Duration, now: Instant) {
        let due = Some(now + after);
        match ask {
            DatahubPollAsk::News { .. } => self.news.due = due,
            DatahubPollAsk::Economic { .. } => self.economic.due = due,
            DatahubPollAsk::Equity { week_start, .. } => {
                self.equity.entry(week_start.clone()).or_default().due = due;
            }
        }
    }

    /// Fold one pass into the tool's data: each answered feed's outcome is applied (a snapshot
    /// replaces its rows, anything else only its status), a request that failed or never left says
    /// so in the status of the feeds it covers, and every asked feed is scheduled.
    pub(crate) fn merge(
        &mut self,
        td: &mut DatahubPollSnapshot,
        plan: &[DatahubPollAsk],
        pass: DatahubPollPass,
        clock: &DatahubPollClock,
    ) {
        let (addr, answers) = match pass {
            DatahubPollPass::Asked { addr, answers } => (addr, answers),
            DatahubPollPass::ConnectFailed { addr, error } if error.transient => {
                self.link_failed(td, plan, &addr, &error.cause, clock.now);
                return;
            }
            not_asked => {
                // A refused dial and an old server both ANSWERED: the link itself works.
                if let DatahubPollPass::ConnectFailed { addr, .. }
                | DatahubPollPass::ServerUnsupported { addr, .. } = &not_asked
                {
                    self.streak.reached(POLL, addr);
                }
                let line = not_asked.not_asked_line().unwrap_or_default();
                // No backend is NOT CONFIGURED, not a fault: grey, as the market-data session shows
                // its own "no datahub configured". A refused dial and an old server are red.
                let health = match not_asked {
                    DatahubPollPass::NoBackend => DatahubPollHealth::Unasked,
                    _ => DatahubPollHealth::Unavailable,
                };
                for ask in plan {
                    set_status(td, ask, &line, health);
                    self.schedule(ask, POLL_EVERY, clock.now);
                }
                return;
            }
        };
        for (i, (ask, answer)) in answers.into_iter().enumerate() {
            match answer {
                Ok(DatahubPollReply::News(reply)) => {
                    let (rows, status, health, next) =
                        judge(reply.outcome, &mut self.news.held, &NEWS, &addr, clock);
                    if let Some(rows) = rows {
                        td.news = rows;
                    }
                    td.news_status = status;
                    td.news_health = health;
                    self.schedule(&ask, next, clock.now);
                }
                Ok(DatahubPollReply::Economic(reply)) => {
                    let (rows, status, health, next) =
                        judge(reply.outcome, &mut self.economic.held, &ECONOMIC, &addr, clock);
                    if let Some(rows) = rows {
                        td.calendar = rows;
                    }
                    td.calendar_status = status;
                    td.calendar_health = health;
                    self.schedule(&ask, next, clock.now);
                }
                Ok(DatahubPollReply::Equity(reply)) => {
                    // An equity reply only ever answers an equity ask: the week is the ask's.
                    let DatahubPollAsk::Equity { week_start, .. } = &ask else { continue };
                    let slot = self.equity.entry(week_start.clone()).or_default();
                    let week = td.calendar_weeks.entry(week_start.clone()).or_default();
                    let (rows, status, health, n1) =
                        judge(reply.earnings, &mut slot.earnings, &EARNINGS, &addr, clock);
                    if let Some(rows) = rows {
                        week.earnings = rows;
                    }
                    week.earnings_status = status;
                    week.earnings_health = health;
                    let (rows, status, health, n2) =
                        judge(reply.dividends, &mut slot.dividends, &DIVIDENDS, &addr, clock);
                    if let Some(rows) = rows {
                        week.dividends = rows;
                    }
                    week.dividends_status = status;
                    week.dividends_health = health;
                    let (rows, status, health, n3) =
                        judge(reply.ipos, &mut slot.ipos, &IPOS, &addr, clock);
                    if let Some(rows) = rows {
                        week.ipos = rows;
                    }
                    week.ipo_status = status;
                    week.ipo_health = health;
                    recount(week);
                    slot.due = Some(clock.now + n1.min(n2).min(n3));
                }
                Err(error) if error.transient => {
                    // The link failed mid-pass: this ask and every one after it went unanswered.
                    let rest = plan.get(i..).unwrap_or_default();
                    self.link_failed(td, rest, &addr, &error.cause, clock.now);
                    return;
                }
                Err(error) => {
                    let line = failed_line(&addr, ask.feed_name(), &error.cause);
                    set_status(td, &ask, &line, DatahubPollHealth::Unavailable);
                    self.schedule(&ask, POLL_EVERY, clock.now);
                }
            }
        }
        self.streak.reached(POLL, &addr);
    }

    /// A pass whose LINK failed ([`crate::link`]): every feed in `asks` keeps its rows and is
    /// asked again on the cadence, and its status is the quiet reconnecting line while the streak
    /// is short, the cause once it is not.
    fn link_failed(
        &mut self,
        td: &mut DatahubPollSnapshot,
        asks: &[DatahubPollAsk],
        addr: &str,
        cause: &str,
        now: Instant,
    ) {
        let (line, health) = match self.streak.failed(POLL, addr, cause) {
            None => (RECONNECTING_LINE.to_string(), DatahubPollHealth::Reconnecting),
            Some(line) => (line, DatahubPollHealth::Unavailable),
        };
        for ask in asks {
            set_status(td, ask, &line, health);
            self.schedule(ask, POLL_EVERY, now);
        }
    }
}

/// Put `line` in the status field, and `health` in the health field, of every feed `ask` covers.
fn set_status(
    td: &mut DatahubPollSnapshot,
    ask: &DatahubPollAsk,
    line: &str,
    health: DatahubPollHealth,
) {
    match ask {
        DatahubPollAsk::News { .. } => {
            td.news_status = line.to_string();
            td.news_health = health;
        }
        DatahubPollAsk::Economic { .. } => {
            td.calendar_status = line.to_string();
            td.calendar_health = health;
        }
        DatahubPollAsk::Equity { week_start, .. } => {
            let week = td.calendar_weeks.entry(week_start.clone()).or_default();
            week.earnings_status = line.to_string();
            week.earnings_health = health;
            week.dividends_status = line.to_string();
            week.dividends_health = health;
            week.ipo_status = line.to_string();
            week.ipo_health = health;
        }
    }
}

/// Rebuild the per-day counts the day-strip cards show from the week's rows.
fn recount(week: &mut CalendarEquityWeek) {
    week.counts.clear();
    for r in &week.earnings {
        week.counts.entry(r.date.clone()).or_insert((0, 0, 0)).0 += 1;
    }
    for r in &week.dividends {
        week.counts.entry(r.date.clone()).or_insert((0, 0, 0)).1 += 1;
    }
    for r in week.ipos.iter().filter(|r| !r.date.is_empty()) {
        week.counts.entry(r.date.clone()).or_insert((0, 0, 0)).2 += 1;
    }
}

/// **One outcome, judged**: the rows to adopt (`Some` only for a snapshot), the status sentence,
/// how the feed stands as DATA ([`DatahubPollHealth`], so a view colours by it and never by reading
/// the sentence), and when to ask again. The generation is kept for a snapshot and an `Unchanged`,
/// and FORGOTTEN for everything else (the module doc's last paragraph).
fn judge<T>(
    outcome: DatahubPollOutcome<T>,
    held: &mut DatahubPollHeld,
    label: &DatahubPollLabel,
    addr: &str,
    clock: &DatahubPollClock,
) -> (Option<Vec<T>>, String, DatahubPollHealth, Duration) {
    match outcome {
        DatahubPollOutcome::Snapshot {
            rows,
            fetched_at_ms,
            generation,
            sources_ok,
            sources_total,
            cadence_secs,
        } => {
            held.generation = Some(generation);
            let stamp = DatahubPollStamp {
                rows: rows.len(),
                fetched_at_ms,
                sources_ok,
                sources_total,
                cadence_secs,
            };
            let line = live_line(label, &stamp, addr, clock);
            let health = stamp_state(label, &stamp, clock).0;
            held.stamp = Some(stamp);
            (Some(rows), line, health, POLL_EVERY)
        }
        DatahubPollOutcome::Unchanged { generation } => {
            held.generation = Some(generation);
            let (line, health) = match &held.stamp {
                Some(stamp) => {
                    (live_line(label, stamp, addr, clock), stamp_state(label, stamp, clock).0)
                }
                None => (
                    DatahubPollOutcome::<T>::Unchanged { generation }.describe(label.name),
                    DatahubPollHealth::Unavailable,
                ),
            };
            (None, line, health, POLL_EVERY)
        }
        other => {
            held.generation = None;
            let warming = matches!(other, DatahubPollOutcome::Warming);
            let next = if warming { WARMING_RETRY } else { POLL_EVERY };
            let health =
                if warming { DatahubPollHealth::Warming } else { DatahubPollHealth::Unavailable };
            (None, other.describe(label.name), health, next)
        }
    }
}

/// How a held snapshot stands, with the word its status sentence leads with:
/// [`DatahubPollHealth::Stale`] when the server's latest poll reached none of its sources or the
/// snapshot is older than the feed's limit, [`DatahubPollHealth::Live`] otherwise. The ONE place the
/// two are decided, so the sentence and the colour cannot disagree.
fn stamp_state(
    label: &DatahubPollLabel,
    stamp: &DatahubPollStamp,
    clock: &DatahubPollClock,
) -> (DatahubPollHealth, &'static str) {
    let age_ms = (clock.now_ms - stamp.fetched_at_ms).max(0);
    let age = Duration::from_millis(age_ms.unsigned_abs());
    if stamp.sources_total > 0 && stamp.sources_ok == 0 {
        (DatahubPollHealth::Stale, "STALE — the datahub's latest poll reached none of its sources")
    } else if age > stale_after(label.stale_floor, stamp.cadence_secs) {
        (DatahubPollHealth::Stale, "STALE")
    } else {
        (DatahubPollHealth::Live, "LIVE")
    }
}

/// The status of a held snapshot: LIVE, or STALE with the reason, with its age and its sources.
fn live_line(
    label: &DatahubPollLabel,
    stamp: &DatahubPollStamp,
    addr: &str,
    clock: &DatahubPollClock,
) -> String {
    let age_ms = (clock.now_ms - stamp.fetched_at_ms).max(0);
    let state = stamp_state(label, stamp, clock).1;
    format!(
        "{} {} · {state} · {} of {} sources · fetched {} ago · via the datahub at {addr}",
        stamp.rows,
        label.noun,
        stamp.sources_ok,
        stamp.sources_total,
        age_label(age_ms),
    )
}

/// A coarse age: `45s`, `12m`, `3h`, `2d`.
fn age_label(ms: i64) -> String {
    use vike_model::{MS_PER_DAY, MS_PER_HOUR, MS_PER_MINUTE, MS_PER_SECOND};
    let ms = ms.max(0);
    if ms < MS_PER_MINUTE {
        format!("{}s", ms / MS_PER_SECOND)
    } else if ms < MS_PER_HOUR {
        format!("{}m", ms / MS_PER_MINUTE)
    } else if ms < MS_PER_DAY {
        format!("{}h", ms / MS_PER_HOUR)
    } else {
        format!("{}d", ms / MS_PER_DAY)
    }
}

/// The Monday of the week containing `day`.
#[must_use]
pub fn monday_of(day: NaiveDate) -> NaiveDate {
    day - chrono::Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// **One poll of the datahub, one pass at a time**: what it has learned (`state`: the News and
/// Calendar half's [`DatahubPollState`] or the Options half's [`options::OptionsPollState`]) and the
/// connection it holds across passes. Public so the loopback suites
/// (`crates/vike-datahub-client/tests/tool_feeds_poll.rs`, `tests/options_poll.rs`) drive the exact
/// pass each thread runs.
pub struct DatahubPoller<S> {
    state: S,
    connection: DatahubPollConnection,
}

/// The News and Calendar half's poller: its replies bounded by [`FEEDS_REPLY_TIMEOUT`].
impl Default for DatahubPoller<DatahubPollState> {
    fn default() -> Self {
        Self::with_reply_timeout(FEEDS_REPLY_TIMEOUT)
    }
}

impl DatahubPoller<DatahubPollState> {
    /// A poller whose held connection bounds each reply by `reply_timeout` instead of
    /// [`FEEDS_REPLY_TIMEOUT`] — the seam the loopback suite drives a silent datahub through in
    /// milliseconds. The poll thread uses [`Default`].
    #[must_use]
    pub fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self {
            state: DatahubPollState::default(),
            connection: DatahubPollConnection::with_reply_timeout(reply_timeout),
        }
    }

    /// **One pass**: read the live interests off `shared`'s link; with none, ask nothing and return
    /// `false`. Otherwise ask the datahub, over the held connection, for each feed that is due and
    /// merge the answers, then return `true` when something was asked (the caller wakes the UI,
    /// which is what renews an open window's interest). Every network call is made OUTSIDE the
    /// lock, so a slow server never stalls the frame.
    pub fn run_once(
        &mut self,
        shared: &Mutex<DatahubPollSnapshot>,
        dial: Option<&DatahubDial>,
        clock: &DatahubPollClock,
    ) -> bool {
        // A switched datahub clears the old one's data before anything else, window open or not.
        let link = {
            let mut td = lock(shared);
            self.state.observe_dial(&mut td, dial);
            td.feeds_link.clone()
        };
        let force = link.take_refresh();
        let interests = link.live(clock.now);
        if interests.is_empty() {
            return false;
        }
        // Every week a Calendar window shows; the viewer's own while none has drawn.
        let mut mondays = link.live_weeks(clock.now);
        if mondays.is_empty() {
            mondays.push(monday_of(clock.today));
        }
        let weeks: Vec<String> = mondays.iter().map(|m| m.format("%Y-%m-%d").to_string()).collect();
        let weeks: Vec<&str> = weeks.iter().map(String::as_str).collect();
        if interests.contains(&DatahubPollInterest::Calendar) {
            self.state.keep_weeks(&mut lock(shared), &weeks);
        }
        let plan = self.state.plan(&interests, force, &weeks, clock.now);
        if plan.is_empty() {
            return false;
        }
        let pass = self.connection.pass(dial, &plan);
        self.state.merge(&mut lock(shared), &plan, pass, clock);
        true
    }
}

/// The pure halves of the poll: the link's TTL, wake and refresh, the schedule, the merge and its
/// sentences, the dial and week resets and the generation rule.
#[cfg(test)]
mod tests;

/// ONE poll serves the News, Calendar and Options windows (D57): its two halves still ask the
/// verbs they asked before the collapse, at the cadence they asked them at.
#[cfg(test)]
mod collapse_tests;
