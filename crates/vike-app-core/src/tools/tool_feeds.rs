//! The News and Calendar tools' CLIENT half: what they ask the backend's datahub for, how often,
//! and how an answer lands in [`ToolData`].
//!
//! # Where the data comes from
//!
//! The desktop fetches NO provider (owner ruling, 2026-10-12; the Options tool's decision, 0128,
//! extended to the tool feeds). The datahub polls the news RSS feeds, the ForexFactory week, Finnhub
//! (earnings), FMP (dividends) and Nasdaq (IPOs) on ITS cadence, with the provider keys held on ITS
//! box, and this side asks `Request::NewsFeed`, `Request::EconomicCalendar` and
//! `Request::EquityCalendar` and merges the answers ([`super::tool_feeds_wire`] is the connection).
//! What stays here is the DISPLAY layer: the topic classifier, the country and flag of a currency
//! and every date label in the viewer's LOCAL timezone, derived from the raw instant the server
//! sends.
//!
//! ⚠ **There is NO fallback.** An old server, a lane its operator left unarmed, a key the server
//! does not hold, a refusal, no backend and a dead link each put ONE sentence in the feed's status
//! field and keep the last good rows; nothing ever fetches a provider instead (owner ruling).
//!
//! # What is asked: the interest
//!
//! [`ToolFeedsLink`] is the UI-to-poll-thread half. Every frame the News window draws it records
//! [`FeedInterest::News`], and every frame the Calendar window draws (any of its four pages: the
//! day-strip counts need all four feeds) it records [`FeedInterest::Calendar`]. An interest lives
//! [`INTEREST_TTL`] without being recorded again: a closed window stops being asked for within that,
//! and with no window open the thread dials nothing at all (the datahub, symmetrically, polls nothing until the first
//! request). A NEW interest wakes the thread at once; the Refresh pill ([`ToolFeedsLink::refresh`])
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
//! The status of a held snapshot says how old it is and flags it STALE past [`stale_after`] — three
//! of the server's reported cycles, never less than [`NEWS_STALE_FLOOR`] / [`CALENDAR_STALE_FLOOR`] —
//! or when the server's latest poll reached none of its sources.
//!
//! ⚠ Last good is a rule about ONE server's answers. When the dial names another datahub (a backend
//! switch, a changed key name, no datahub at all), [`FeedsState::observe_dial`] clears every row,
//! count and status at once. The equity rows are also one WEEK's: when the viewer's local Monday
//! moves, they are cleared the same way.
//!
//! # The equity generation
//!
//! `Request::EquityCalendar` carries ONE `if_generation` for its three feeds, so one is sent only
//! when all three feeds are held at the SAME generation; any feed that is not (a `Warming` or
//! `NotConfigured` one) makes the next ask a full one. A non-snapshot answer also forgets the
//! feed's generation, so a restarted server whose counter lands on the same number can never
//! make the tool keep rows the server no longer holds.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate};
use vike_datahub_client::tool_feeds::ToolFeedOutcome;

use super::calendar::{CalEvent, week_range_label};
use super::tool_feeds_wire::{FeedAsk, FeedReply, FeedsPass, ToolFeedsWire, failed_line};
use super::view::{FeedHealth, ToolData};
use crate::data::catalog_wire::CatalogDial;

/// How often an open window's feeds are asked for again.
///
/// ⚠ The wire HOLDS one datahub connection between these polls (`tool_feeds_wire`), so this cadence is
/// a floor under the server's idle read timeout: a connection reaped between two polls costs the
/// next poll an error. The assert below holds it to half of
/// `vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT_MIN`, the shortest value a
/// `config.node_idle_read_timeout_ms` row may name — so lengthening this poll, or lowering that
/// floor, fails to compile here instead of racing every poll against the reap.
pub(crate) const POLL_EVERY: Duration = Duration::from_secs(60);

const _: () = assert!(
    2 * POLL_EVERY.as_millis() <= vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT_MIN.as_millis(),
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

/// What a window wants kept fresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FeedInterest {
    /// The News window: the headline feed.
    News,
    /// The Calendar window: the economic calendar AND the week's earnings, dividends and IPOs.
    Calendar,
}

/// **The UI's half of the feeds poll thread**: the interests the windows recorded, the Refresh
/// request and the channel that wakes the thread. Cheap to clone, and every clone is the same link,
/// which is why it rides inside [`ToolData`] and reaches every tool body through the frame's copy
/// of it.
#[derive(Clone)]
pub struct ToolFeedsLink {
    wants: Arc<Mutex<HashMap<FeedInterest, Instant>>>,
    force: Arc<AtomicBool>,
    wake: Sender<()>,
}

impl Default for ToolFeedsLink {
    /// A link no thread listens on: interests are recorded and every wake is dropped. What
    /// `ToolData::default()` holds until `spawn_tool_fetchers` installs the live one.
    fn default() -> Self {
        Self::new(std::sync::mpsc::channel().0)
    }
}

impl ToolFeedsLink {
    /// A link whose wakes go to `wake` — the receiving half is the poll thread's.
    #[must_use]
    pub fn new(wake: Sender<()>) -> Self {
        Self { wants: Arc::default(), force: Arc::default(), wake }
    }

    /// The Refresh pill: ask every live feed NOW instead of at its next cadence. A closed channel
    /// (the thread gone at shutdown) is ignored, so the UI can never panic on it.
    pub fn refresh(&self) {
        self.force.store(true, Ordering::SeqCst);
        let _ = self.wake.send(());
    }

    /// Record that a window shows `interest`. Called every frame the window draws; only an
    /// interest that was not already live wakes the thread.
    pub fn want(&self, interest: FeedInterest) {
        self.want_at(interest, Instant::now());
    }

    /// [`Self::want`] at a given instant — the clock is a parameter so the TTL is testable.
    pub(crate) fn want_at(&self, interest: FeedInterest, now: Instant) {
        let fresh = {
            let mut wants = lock(&self.wants);
            let fresh = wants
                .get(&interest)
                .is_none_or(|at| now.saturating_duration_since(*at) >= INTEREST_TTL);
            wants.insert(interest, now);
            fresh
        };
        if fresh {
            let _ = self.wake.send(());
        }
    }

    /// The interests still live at `now`; the lapsed ones are forgotten.
    pub(crate) fn live(&self, now: Instant) -> Vec<FeedInterest> {
        let mut wants = lock(&self.wants);
        wants.retain(|_, at| now.saturating_duration_since(*at) < INTEREST_TTL);
        wants.keys().copied().collect()
    }

    /// Whether a Refresh was pressed since the last call (and forget it).
    pub(crate) fn take_refresh(&self) -> bool {
        self.force.swap(false, Ordering::SeqCst)
    }
}

/// The clock one pass runs on: a monotonic instant for the schedule, the wall clock for a
/// snapshot's age, and the viewer's LOCAL date for the week the equity calendars are asked for.
#[derive(Clone, Copy, Debug)]
pub struct FeedsClock {
    /// Schedules the next ask.
    pub now: Instant,
    /// Epoch milliseconds UTC: compared with a snapshot's `fetched_at_ms`.
    pub now_ms: i64,
    /// The viewer's local calendar day: the Monday of its week is the equity week.
    pub today: NaiveDate,
}

impl FeedsClock {
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
struct Held {
    /// The generation of the rows held; sent back as `if_generation`.
    generation: Option<u64>,
    /// What the last snapshot said about itself, to rebuild the status line when a later pass
    /// answers `Unchanged` (and to age it).
    stamp: Option<Stamp>,
}

#[derive(Debug)]
struct Stamp {
    rows: usize,
    fetched_at_ms: i64,
    sources_ok: u32,
    sources_total: u32,
    /// The server's poll cadence for this feed, as the snapshot reported it (`0`: not said).
    cadence_secs: u32,
}

/// One feed's schedule: what is held and when it is next due (`None`: now).
#[derive(Debug, Default)]
struct Slot {
    held: Held,
    due: Option<Instant>,
}

/// How a feed is named in a sentence.
struct Label {
    /// The feed's human name (`ToolFeedOutcome::describe`'s `feed`).
    name: &'static str,
    /// What its rows are called.
    noun: &'static str,
    /// The shortest age that flags a snapshot of this feed STALE ([`stale_after`]'s `floor`).
    stale_floor: Duration,
}

const NEWS: Label = Label { name: "news", noun: "headlines", stale_floor: NEWS_STALE_FLOOR };
const ECONOMIC: Label =
    Label { name: "economic calendar", noun: "events", stale_floor: CALENDAR_STALE_FLOOR };
const EARNINGS: Label =
    Label { name: "earnings", noun: "earnings rows", stale_floor: CALENDAR_STALE_FLOOR };
const DIVIDENDS: Label =
    Label { name: "dividends", noun: "dividend rows", stale_floor: CALENDAR_STALE_FLOOR };
const IPOS: Label = Label { name: "IPOs", noun: "IPO rows", stale_floor: CALENDAR_STALE_FLOOR };

/// What the poll thread has learned between passes.
#[derive(Debug, Default)]
pub(crate) struct FeedsState {
    /// The dial (address, key name) the last pass ran under; `None` before the first pass.
    dial_seen: Option<Option<(String, String)>>,
    news: Slot,
    economic: Slot,
    /// The Monday the equity rows held are for.
    equity_week: Option<String>,
    equity_due: Option<Instant>,
    earnings: Held,
    dividends: Held,
    ipos: Held,
}

/// Whether a feed due at `due` is asked this pass.
fn is_due(due: Option<Instant>, force: bool, now: Instant) -> bool {
    force || due.is_none_or(|at| now >= at)
}

impl FeedsState {
    /// **A different datahub is a different world.** When the dial names another address or key
    /// name than the last pass saw (a backend switch, a new `config.datahub_addr`, or no datahub at
    /// all), everything the old server answered is CLEARED at once, with everything learned from it.
    /// Returns whether it cleared. The first dial a poller ever sees clears nothing.
    pub(crate) fn observe_dial(&mut self, td: &mut ToolData, dial: Option<&CatalogDial>) -> bool {
        let identity = dial.map(|d| (d.addr.clone(), d.key_name.clone()));
        let changed = self.dial_seen.as_ref().is_some_and(|seen| *seen != identity);
        self.dial_seen = Some(identity);
        if !changed {
            return false;
        }
        td.news.clear();
        td.news_status.clear();
        td.news_health = FeedHealth::default();
        td.calendar.clear();
        td.cal_status.clear();
        td.cal_range.clear();
        clear_equity(td);
        let dial_seen = self.dial_seen.take();
        *self = Self { dial_seen, ..Self::default() };
        true
    }

    /// The equity rows are one week's: move to `week` (an ISO Monday), forgetting what was held for
    /// the old one. Returns whether it moved from a week it had held.
    pub(crate) fn enter_week(&mut self, week: &str) -> bool {
        if self.equity_week.as_deref() == Some(week) {
            return false;
        }
        let had_one = self.equity_week.is_some();
        self.equity_week = Some(week.to_string());
        self.equity_due = None;
        self.earnings = Held::default();
        self.dividends = Held::default();
        self.ipos = Held::default();
        had_one
    }

    /// The one `if_generation` the equity request may carry: set only when all three feeds are
    /// held at the SAME generation (see the module doc).
    fn equity_generation(&self) -> Option<u64> {
        let g = self.earnings.generation?;
        (self.dividends.generation == Some(g) && self.ipos.generation == Some(g)).then_some(g)
    }

    /// **This pass's requests**, from the live `interests`: each feed that is due (or all of them,
    /// on a forced refresh).
    pub(crate) fn plan(
        &self,
        interests: &[FeedInterest],
        force: bool,
        week: &str,
        now: Instant,
    ) -> Vec<FeedAsk> {
        let mut plan = Vec::new();
        if interests.contains(&FeedInterest::News) && is_due(self.news.due, force, now) {
            plan.push(FeedAsk::News { if_generation: self.news.held.generation });
        }
        if interests.contains(&FeedInterest::Calendar) {
            if is_due(self.economic.due, force, now) {
                plan.push(FeedAsk::Economic { if_generation: self.economic.held.generation });
            }
            if is_due(self.equity_due, force, now) {
                plan.push(FeedAsk::Equity {
                    week_start: week.to_string(),
                    if_generation: self.equity_generation(),
                });
            }
        }
        plan
    }

    /// Schedule `ask`'s feed `after` from now.
    fn schedule(&mut self, ask: &FeedAsk, after: Duration, now: Instant) {
        let due = Some(now + after);
        match ask {
            FeedAsk::News { .. } => self.news.due = due,
            FeedAsk::Economic { .. } => self.economic.due = due,
            FeedAsk::Equity { .. } => self.equity_due = due,
        }
    }

    /// Fold one pass into the tool's data: each answered feed's outcome is applied (a snapshot
    /// replaces its rows, anything else only its status), a request that failed or never left says
    /// so in the status of the feeds it covers, and every asked feed is scheduled.
    pub(crate) fn merge(
        &mut self,
        td: &mut ToolData,
        plan: &[FeedAsk],
        pass: FeedsPass,
        clock: &FeedsClock,
    ) {
        let (addr, answers) = match pass {
            FeedsPass::Asked { addr, answers } => (addr, answers),
            not_asked => {
                let line = not_asked.not_asked_line().unwrap_or_default();
                for ask in plan {
                    set_status(td, ask, &line);
                    self.schedule(ask, POLL_EVERY, clock.now);
                }
                return;
            }
        };
        for (ask, answer) in answers {
            match answer {
                Ok(FeedReply::News(reply)) => {
                    let (rows, status, health, next) =
                        judge(reply.outcome, &mut self.news.held, &NEWS, &addr, clock);
                    if let Some(rows) = rows {
                        td.news = rows;
                    }
                    td.news_status = status;
                    td.news_health = health;
                    self.schedule(&ask, next, clock.now);
                }
                Ok(FeedReply::Economic(reply)) => {
                    let (rows, status, _, next) =
                        judge(reply.outcome, &mut self.economic.held, &ECONOMIC, &addr, clock);
                    if let Some(rows) = rows {
                        td.calendar =
                            rows.iter().map(|e| CalEvent::from_wire(e, &chrono::Local)).collect();
                    }
                    td.cal_status = status;
                    self.schedule(&ask, next, clock.now);
                }
                Ok(FeedReply::Equity(reply)) => {
                    let (rows, status, _, n1) =
                        judge(reply.earnings, &mut self.earnings, &EARNINGS, &addr, clock);
                    if let Some(rows) = rows {
                        td.cal_earnings = rows;
                    }
                    td.cal_earnings_status = status;
                    let (rows, status, _, n2) =
                        judge(reply.dividends, &mut self.dividends, &DIVIDENDS, &addr, clock);
                    if let Some(rows) = rows {
                        td.cal_dividends = rows;
                    }
                    td.cal_dividends_status = status;
                    let (rows, status, _, n3) =
                        judge(reply.ipos, &mut self.ipos, &IPOS, &addr, clock);
                    if let Some(rows) = rows {
                        td.cal_ipos = rows;
                    }
                    td.cal_ipo_status = status;
                    recount(td);
                    self.schedule(&ask, n1.min(n2).min(n3), clock.now);
                }
                Err(error) => {
                    set_status(td, &ask, &failed_line(&addr, ask.feed_name(), &error));
                    self.schedule(&ask, POLL_EVERY, clock.now);
                }
            }
        }
    }
}

/// Put `line` in the status field of every feed `ask` covers.
fn set_status(td: &mut ToolData, ask: &FeedAsk, line: &str) {
    match ask {
        FeedAsk::News { .. } => {
            td.news_status = line.to_string();
            td.news_health = FeedHealth::Unavailable;
        }
        FeedAsk::Economic { .. } => td.cal_status = line.to_string(),
        FeedAsk::Equity { .. } => {
            td.cal_earnings_status = line.to_string();
            td.cal_dividends_status = line.to_string();
            td.cal_ipo_status = line.to_string();
        }
    }
}

/// Forget every equity row, count and status.
fn clear_equity(td: &mut ToolData) {
    td.cal_equity.clear();
    td.cal_earnings.clear();
    td.cal_dividends.clear();
    td.cal_ipos.clear();
    td.cal_earnings_status.clear();
    td.cal_dividends_status.clear();
    td.cal_ipo_status.clear();
}

/// Rebuild the per-day counts the day-strip cards show from the rows held.
fn recount(td: &mut ToolData) {
    td.cal_equity.clear();
    for r in &td.cal_earnings {
        td.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).0 += 1;
    }
    for r in &td.cal_dividends {
        td.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).1 += 1;
    }
    for r in td.cal_ipos.iter().filter(|r| !r.date.is_empty()) {
        td.cal_equity.entry(r.date.clone()).or_insert((0, 0, 0)).2 += 1;
    }
}

/// **One outcome, judged**: the rows to adopt (`Some` only for a snapshot), the status sentence,
/// how the feed stands as DATA ([`FeedHealth`], so a view colours by it and never by reading the
/// sentence), and when to ask again. The generation is kept for a snapshot and an `Unchanged`, and
/// FORGOTTEN for everything else (the module doc's last paragraph).
fn judge<T>(
    outcome: ToolFeedOutcome<T>,
    held: &mut Held,
    label: &Label,
    addr: &str,
    clock: &FeedsClock,
) -> (Option<Vec<T>>, String, FeedHealth, Duration) {
    match outcome {
        ToolFeedOutcome::Snapshot {
            rows,
            fetched_at_ms,
            generation,
            sources_ok,
            sources_total,
            cadence_secs,
        } => {
            held.generation = Some(generation);
            let stamp =
                Stamp { rows: rows.len(), fetched_at_ms, sources_ok, sources_total, cadence_secs };
            let line = live_line(label, &stamp, addr, clock);
            let health = stamp_state(label, &stamp, clock).0;
            held.stamp = Some(stamp);
            (Some(rows), line, health, POLL_EVERY)
        }
        ToolFeedOutcome::Unchanged { generation } => {
            held.generation = Some(generation);
            let (line, health) = match &held.stamp {
                Some(stamp) => {
                    (live_line(label, stamp, addr, clock), stamp_state(label, stamp, clock).0)
                }
                None => (
                    ToolFeedOutcome::<T>::Unchanged { generation }.describe(label.name),
                    FeedHealth::Unavailable,
                ),
            };
            (None, line, health, POLL_EVERY)
        }
        other => {
            held.generation = None;
            let warming = matches!(other, ToolFeedOutcome::Warming);
            let next = if warming { WARMING_RETRY } else { POLL_EVERY };
            let health = if warming { FeedHealth::Warming } else { FeedHealth::Unavailable };
            (None, other.describe(label.name), health, next)
        }
    }
}

/// How a held snapshot stands, with the word its status sentence leads with: [`FeedHealth::Stale`]
/// when the server's latest poll reached none of its sources or the snapshot is older than the
/// feed's limit, [`FeedHealth::Live`] otherwise. The ONE place the two are decided, so the sentence
/// and the colour cannot disagree.
fn stamp_state(label: &Label, stamp: &Stamp, clock: &FeedsClock) -> (FeedHealth, &'static str) {
    let age_ms = (clock.now_ms - stamp.fetched_at_ms).max(0);
    let age = Duration::from_millis(age_ms.unsigned_abs());
    if stamp.sources_total > 0 && stamp.sources_ok == 0 {
        (FeedHealth::Stale, "STALE — the datahub's latest poll reached none of its sources")
    } else if age > stale_after(label.stale_floor, stamp.cadence_secs) {
        (FeedHealth::Stale, "STALE")
    } else {
        (FeedHealth::Live, "LIVE")
    }
}

/// The status of a held snapshot: LIVE, or STALE with the reason, with its age and its sources.
fn live_line(label: &Label, stamp: &Stamp, addr: &str, clock: &FeedsClock) -> String {
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
    let s = ms.max(0) / 1000;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// The Monday of the week containing `day`.
pub(crate) fn monday_of(day: NaiveDate) -> NaiveDate {
    day - chrono::Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// **The poll thread's whole state, one pass at a time** — public so the loopback suite
/// (`crates/vike-app-core/tests/tool_feeds_wire.rs`) drives the exact pass the thread runs.
#[derive(Default)]
pub struct ToolFeedsPoller {
    state: FeedsState,
    wire: ToolFeedsWire,
}

impl ToolFeedsPoller {
    /// A poller whose held connection bounds each reply by `reply_timeout` instead of
    /// `FEEDS_REPLY_TIMEOUT` — the seam the loopback suite drives a silent datahub through in
    /// milliseconds. The poll thread uses [`Default`].
    #[must_use]
    pub fn with_reply_timeout(reply_timeout: Duration) -> Self {
        Self {
            state: FeedsState::default(),
            wire: ToolFeedsWire::with_reply_timeout(reply_timeout),
        }
    }

    /// **One pass**: read the live interests off `shared`'s link; with none, ask nothing and return
    /// `false`. Otherwise ask the datahub, over the held connection, for each feed that is due and
    /// merge the answers, then return `true` when something was asked (the caller wakes the UI,
    /// which is what renews an open window's interest). Every network call is made OUTSIDE the
    /// lock, so a slow server never stalls the frame.
    pub fn run_once(
        &mut self,
        shared: &Mutex<ToolData>,
        dial: Option<&CatalogDial>,
        clock: &FeedsClock,
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
        let monday = monday_of(clock.today);
        let week = monday.format("%Y-%m-%d").to_string();
        if interests.contains(&FeedInterest::Calendar) {
            let mut td = lock(shared);
            if self.state.enter_week(&week) {
                clear_equity(&mut td);
            }
            td.cal_range = week_range_label(monday);
        }
        let plan = self.state.plan(&interests, force, &week, clock.now);
        if plan.is_empty() {
            return false;
        }
        let pass = self.wire.pass(dial, &plan);
        self.state.merge(&mut lock(shared), &plan, pass, clock);
        true
    }
}

/// Lock, recovering a poisoned guard: a panic elsewhere must not take the tools with it.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The pure halves of the poll: the link's TTL, wake and refresh, the schedule, the merge and its
/// sentences, the dial and week resets and the generation rule.
#[path = "tool_feeds_tests.rs"]
#[cfg(test)]
mod tool_feeds_tests;
