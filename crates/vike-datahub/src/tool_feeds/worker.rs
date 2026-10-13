//! **The cache-and-poller one tool feed runs on**: a map of keyed entries (one key for the news and
//! the economic calendar, one per WEEK for the three equity feeds), each holding the last good rows,
//! a generation, and when it is next due; and ONE thread, started by the first request, that polls
//! whichever entry is due.
//!
//! # ⚠ Polling is LAZY, and it pauses
//!
//! Nothing is fetched, and no thread exists, until [`FeedWorker::read`] is first called. An entry
//! nobody has asked for within [`PAUSE_AFTER`] stops being polled (its rows stay, and are served
//! stale with their `fetched_at_ms` until the next ask re-arms it) and is dropped after
//! [`DROP_AFTER`], so a desktop closed for the weekend spends no provider budget — the keyed
//! providers (Finnhub, FMP) are rate-limited per key, and the key is the operator's.
//!
//! # ⚠ Keep-last-good is the rule, never "replace with an empty list"
//!
//! The desktop's equity fetch replaced its rows with an EMPTY list when Finnhub or FMP failed, and
//! said nothing. Here a poll that no source answered ([`PollResult::Failed`]) leaves the rows, the
//! `fetched_at_ms` (so the staleness is visible) and the generation of the rows alone, and records
//! `sources_ok = 0` so a client can say so. A feed whose KEY went missing is the one exception, on
//! purpose: [`PollResult::NotConfigured`] clears the rows, because removing a credential disarms
//! its feed (decision 0097's "removing the token disarms the lane").
//!
//! # The generation
//!
//! One counter per lane, SEEDED FROM THE START TIME in milliseconds (`ToolFeedsLane::new`), minted
//! from whenever a key's served state changes (new rows, a different `sources_ok`, a key appearing or
//! vanishing). Seeding makes a number from a previous run of the daemon unequal to any number this
//! run mints, so a client that survived a restart is sent rows rather than `Unchanged`; sharing the
//! counter across keys makes two weeks' generations unequal, so a client that switched weeks is too.
//!
//! The number a key serves lives in a [`GenerationBoard`], not in the entry, so that the THREE equity
//! feeds of one week can share ONE board: `EquityCalendar` carries a single `if_generation`, and
//! with a board per feed no one number could match all three. On a shared board a change to any of
//! the week's feeds moves the number all three report, and an unchanged week answers all three
//! `Unchanged` on one cheap poll.
//! `vike_datahub_client::tool_feeds::is_unchanged` is the comparison.
//!
//! # Retry
//!
//! A cycle that fails retries after [`Schedule::min_retry`], doubling per consecutive failure up to
//! [`Schedule::cadence`]. An absent key is re-read every [`Schedule::key_recheck`] — a local store
//! read, no provider call — so `vike-cli secrets set` takes effect within a minute without a
//! restart. A feed marked [`Schedule::retry_when_empty`] treats an empty success as a soft failure
//! for scheduling (Nasdaq answers empty on a transient miss), without touching its rows.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use vike_datahub_client::tool_feeds::{ToolFeedOutcome, is_unchanged};

/// An entry nobody has asked for within this long is no longer polled (see the module doc).
pub const PAUSE_AFTER: Duration = Duration::from_secs(2 * 60 * 60);

/// An entry nobody has asked for within this long is dropped, rows and all.
pub const DROP_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long the poll thread sleeps when no entry is due and none is scheduled — it is woken early
/// by a new or re-armed entry, so this only bounds the housekeeping (`DROP_AFTER`) latency.
const IDLE_WAIT: Duration = Duration::from_secs(60 * 60);

/// What one poll of one key came to.
#[derive(Debug)]
pub enum PollResult<T> {
    /// At least one source answered: `rows` are the feed's whole current content (a source that
    /// failed on this poll contributes whatever the poller kept of it), `ok` of `total` sources
    /// answered on THIS poll.
    Rows { rows: Vec<T>, ok: u32, total: u32 },
    /// No source answered. The cache keeps what it holds.
    Failed { total: u32 },
    /// The feed needs a credential the store does not hold (the NAME, never a value).
    NotConfigured { key_name: &'static str },
}

/// The cadences of one worker.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// How long after a good poll the next one is due.
    pub cadence: Duration,
    /// The first retry after a failed poll; doubles per consecutive failure up to `cadence`.
    pub min_retry: Duration,
    /// How long after a `NotConfigured` answer the key is read again.
    pub key_recheck: Duration,
    /// Whether an EMPTY successful poll is retried on the failure schedule (the IPO feed).
    pub retry_when_empty: bool,
}

/// The retry delay after `failures` consecutive failed (or soft-failed) polls: `min_retry`, doubled
/// per further failure, never above `cadence`. `failures == 0` is the cadence itself.
pub fn retry_delay(schedule: &Schedule, failures: u32) -> Duration {
    if failures == 0 {
        return schedule.cadence;
    }
    let doubled = schedule
        .min_retry
        .checked_mul(1u32.checked_shl(failures - 1).unwrap_or(u32::MAX))
        .unwrap_or(schedule.cadence);
    doubled.min(schedule.cadence)
}

/// The numbers a set of keys serve as their generation, minted from ONE counter. See the module doc.
pub struct GenerationBoard<K> {
    counter: Arc<AtomicU64>,
    per_key: Mutex<BTreeMap<K, u64>>,
}

impl<K: Ord + Clone> GenerationBoard<K> {
    /// A board minting from `counter` (the lane's, shared with every other board so no two keys of
    /// the lane ever serve the same number).
    pub fn new(counter: Arc<AtomicU64>) -> Arc<Self> {
        Arc::new(Self { counter, per_key: Mutex::new(BTreeMap::new()) })
    }

    /// The generation `key` serves; 0 for a key that never changed.
    pub fn get(&self, key: &K) -> u64 {
        self.per_key.lock().unwrap_or_else(PoisonError::into_inner).get(key).copied().unwrap_or(0)
    }

    /// Give `key` a new generation, larger than any this lane has minted.
    pub fn bump(&self, key: &K) -> u64 {
        let next = self.counter.fetch_add(1, Ordering::AcqRel) + 1;
        self.per_key.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone(), next);
        next
    }
}

/// What one stored poll needs to know besides the entry and the answer.
struct PollCtx<'a, K> {
    now_ms: i64,
    now: Instant,
    schedule: &'a Schedule,
    key: &'a K,
    board: &'a GenerationBoard<K>,
    feed: &'static str,
}

struct Entry<T> {
    rows: Option<Vec<T>>,
    fetched_at_ms: i64,
    ok: u32,
    total: u32,
    not_configured: Option<&'static str>,
    failures: u32,
    next_due: Instant,
    last_asked: Instant,
}

impl<T> Entry<T> {
    fn new(now: Instant) -> Self {
        Self {
            rows: None,
            fetched_at_ms: 0,
            ok: 0,
            total: 0,
            not_configured: None,
            failures: 0,
            next_due: now,
            last_asked: now,
        }
    }
}

struct State<K, T> {
    entries: BTreeMap<K, Entry<T>>,
}

type FetchFn<K, T> = dyn Fn(&K) -> PollResult<T> + Send + Sync;

/// One feed's keyed caches and the thread that fills them. See the module doc.
pub struct FeedWorker<K, T> {
    name: &'static str,
    schedule: Schedule,
    board: Arc<GenerationBoard<K>>,
    fetch: Box<FetchFn<K, T>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    autostart: bool,
    started: AtomicBool,
    state: Mutex<State<K, T>>,
    wake: Condvar,
}

fn locked<K, T>(m: &Mutex<State<K, T>>) -> MutexGuard<'_, State<K, T>> {
    // A panic in a fetch never holds this lock (the fetch runs outside it), so a poisoned guard
    // still holds a consistent map; serving it beats poisoning every later request.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl<K, T> FeedWorker<K, T>
where
    K: Ord + Clone + Send + Sync + 'static,
    T: Clone + PartialEq + Send + Sync + 'static,
{
    /// A worker named `name` (its thread's name and its log prefix). `autostart: false` is the test
    /// seam: no thread is ever spawned, and a test drives [`Self::poll_now`] by hand.
    pub fn new(
        name: &'static str,
        schedule: Schedule,
        board: Arc<GenerationBoard<K>>,
        now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
        autostart: bool,
        fetch: impl Fn(&K) -> PollResult<T> + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            schedule,
            board,
            fetch: Box::new(fetch),
            now_ms,
            autostart,
            started: AtomicBool::new(false),
            state: Mutex::new(State { entries: BTreeMap::new() }),
            wake: Condvar::new(),
        })
    }

    /// Whether the poll thread has been started — false until the first [`Self::read`].
    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    /// **Answer one request for `key`**: register the interest (which starts the thread on the very
    /// first call, and re-arms a paused or new entry), then say what the cache holds.
    pub fn read(self: &Arc<Self>, key: &K, if_generation: Option<u64>) -> ToolFeedOutcome<T> {
        self.ensure_started();
        let now = Instant::now();
        let mut st = locked(&self.state);
        let is_new = !st.entries.contains_key(key);
        let entry = st.entries.entry(key.clone()).or_insert_with(|| Entry::new(now));
        let was_paused = now.saturating_duration_since(entry.last_asked) >= PAUSE_AFTER;
        entry.last_asked = now;
        let outcome = outcome_of(entry, self.board.get(key), if_generation);
        drop(st);
        if is_new || was_paused {
            self.wake.notify_one();
        }
        outcome
    }

    /// **Poll `key` once, now, and store the answer** — what the thread does for a due entry, and
    /// what a test calls in place of the thread.
    pub fn poll_now(&self, key: &K) {
        // A fetch that panics (a parser bug on a hostile body) must not end the thread: it is a
        // failed poll like any other.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.fetch)(key)))
            .unwrap_or_else(|_| {
                tracing::error!(
                    feed = self.name,
                    "vike-datahub tool feed: a poll PANICKED; treated as a failed poll"
                );
                PollResult::Failed { total: 0 }
            });
        let now_ms = (self.now_ms)();
        let now = Instant::now();
        let mut st = locked(&self.state);
        let entry = st.entries.entry(key.clone()).or_insert_with(|| Entry::new(now));
        let ctx = PollCtx {
            now_ms,
            now,
            schedule: &self.schedule,
            key,
            board: &self.board,
            feed: self.name,
        };
        apply(entry, result, &ctx);
    }

    /// The generation `key` serves (0 before its first change) — a test probe.
    pub fn generation_of(&self, key: &K) -> u64 {
        self.board.get(key)
    }

    fn ensure_started(self: &Arc<Self>) {
        if !self.autostart {
            self.started.store(true, Ordering::Release);
            return;
        }
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("dh-toolfeeds-{}", self.name))
            .spawn(move || me.run());
        if let Err(e) = spawned {
            // The next request retries the spawn.
            self.started.store(false, Ordering::Release);
            tracing::error!(feed = self.name, error = %e, "vike-datahub tool feed: could not spawn the poll thread");
        }
    }

    /// The poll thread's body: forever, poll whichever entry is due, else sleep until the soonest
    /// one (or until a new entry wakes it).
    fn run(self: Arc<Self>) {
        loop {
            let mut st = locked(&self.state);
            let now = Instant::now();
            st.entries.retain(|_, e| now.saturating_duration_since(e.last_asked) < DROP_AFTER);
            let mut due: Option<K> = None;
            let mut soonest: Option<Instant> = None;
            for (key, e) in &st.entries {
                if now.saturating_duration_since(e.last_asked) >= PAUSE_AFTER {
                    continue; // paused until somebody asks again
                }
                if e.next_due <= now {
                    due = Some(key.clone());
                    break;
                }
                soonest = Some(soonest.map_or(e.next_due, |s| s.min(e.next_due)));
            }
            match due {
                Some(key) => {
                    drop(st);
                    self.poll_now(&key);
                }
                None => {
                    let wait = soonest
                        .map_or(IDLE_WAIT, |t| t.saturating_duration_since(now))
                        .min(IDLE_WAIT);
                    // The returned guard is dropped at once: the loop re-reads the state.
                    drop(self.wake.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner));
                }
            }
        }
    }
}

/// What `entry` says to a request carrying `if_generation`.
fn outcome_of<T: Clone>(
    entry: &Entry<T>,
    generation: u64,
    if_generation: Option<u64>,
) -> ToolFeedOutcome<T> {
    if let Some(key_name) = entry.not_configured {
        return ToolFeedOutcome::NotConfigured { key_name: key_name.to_string() };
    }
    match &entry.rows {
        None => ToolFeedOutcome::Warming,
        Some(_) if is_unchanged(if_generation, generation) => {
            ToolFeedOutcome::Unchanged { generation }
        }
        Some(rows) => ToolFeedOutcome::Snapshot {
            rows: rows.clone(),
            fetched_at_ms: entry.fetched_at_ms,
            generation,
            sources_ok: entry.ok,
            sources_total: entry.total,
        },
    }
}

/// Fold one poll's answer into `entry` and schedule the next poll. The module doc's rules, in one
/// place.
fn apply<K: Ord + Clone, T: PartialEq>(
    entry: &mut Entry<T>,
    result: PollResult<T>,
    ctx: &PollCtx<'_, K>,
) {
    let (now_ms, now, schedule, key, board, feed) =
        (ctx.now_ms, ctx.now, ctx.schedule, ctx.key, ctx.board, ctx.feed);
    match result {
        PollResult::Rows { rows, ok, total } => {
            let changed = entry.rows.as_ref() != Some(&rows)
                || entry.ok != ok
                || entry.total != total
                || entry.not_configured.is_some();
            let empty = rows.is_empty();
            entry.not_configured = None;
            entry.rows = Some(rows);
            entry.ok = ok;
            entry.total = total;
            entry.fetched_at_ms = now_ms;
            if changed {
                board.bump(key);
            }
            if empty && schedule.retry_when_empty {
                entry.failures = entry.failures.saturating_add(1);
                entry.next_due = now + retry_delay(schedule, entry.failures);
            } else {
                entry.failures = 0;
                entry.next_due = now + schedule.cadence;
            }
        }
        PollResult::Failed { total } => {
            let changed = entry.ok != 0;
            // The rows, `fetched_at_ms` and generation-of-the-rows stay; only the success count
            // says the latest poll found nothing.
            entry.ok = 0;
            entry.total = total.max(entry.total);
            entry.failures = entry.failures.saturating_add(1);
            entry.next_due = now + retry_delay(schedule, entry.failures);
            if changed && entry.rows.is_some() {
                board.bump(key);
            }
            tracing::warn!(
                feed,
                failures = entry.failures,
                next_in_secs = retry_delay(schedule, entry.failures).as_secs(),
                kept_rows = entry.rows.as_ref().map_or(0, Vec::len),
                "vike-datahub tool feed: no source answered; the last good rows are kept"
            );
        }
        PollResult::NotConfigured { key_name } => {
            let changed = entry.not_configured != Some(key_name);
            entry.not_configured = Some(key_name);
            entry.rows = None;
            entry.ok = 0;
            if changed {
                board.bump(key);
                tracing::info!(
                    feed,
                    key_name,
                    "vike-datahub tool feed: its API key is not in this box's credential store; \
                     the feed answers NotConfigured and the store is re-read every minute"
                );
            }
            entry.next_due = now + schedule.key_recheck;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEDULE: Schedule = Schedule {
        cadence: Duration::from_secs(1800),
        min_retry: Duration::from_secs(60),
        key_recheck: Duration::from_secs(60),
        retry_when_empty: false,
    };

    /// A worker over a scripted fetch: each call pops the next answer; `autostart` off.
    fn scripted(
        schedule: Schedule,
        script: Vec<PollResult<u32>>,
    ) -> (Arc<FeedWorker<(), u32>>, Arc<GenerationBoard<()>>) {
        let board = GenerationBoard::new(Arc::new(AtomicU64::new(100)));
        let script = Mutex::new(script.into_iter().collect::<std::collections::VecDeque<_>>());
        let w = FeedWorker::new(
            "test",
            schedule,
            Arc::clone(&board),
            Arc::new(|| 1_700_000_000_000),
            false,
            move |_: &()| script.lock().unwrap().pop_front().expect("a scripted answer"),
        );
        (w, board)
    }

    fn snapshot(o: &ToolFeedOutcome<u32>) -> (Vec<u32>, u64, u32, u32, i64) {
        match o {
            ToolFeedOutcome::Snapshot {
                rows,
                generation,
                sources_ok,
                sources_total,
                fetched_at_ms,
            } => (rows.clone(), *generation, *sources_ok, *sources_total, *fetched_at_ms),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn nothing_is_polled_and_no_thread_exists_before_the_first_request() {
        let polls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&polls);
        let w = FeedWorker::<(), u32>::new(
            "lazy",
            SCHEDULE,
            GenerationBoard::new(Arc::new(AtomicU64::new(1))),
            Arc::new(|| 0),
            true,
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                PollResult::Rows { rows: vec![1], ok: 1, total: 1 }
            },
        );
        assert!(!w.started(), "constructing a worker starts nothing");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(polls.load(Ordering::SeqCst), 0, "no request, no poll");
        // The first request answers Warming (the entry did not exist), starts the thread, and the
        // poll lands behind it.
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Warming));
        assert!(w.started());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let ToolFeedOutcome::Snapshot { rows, .. } = w.read(&(), None) {
                assert_eq!(rows, vec![1]);
                break;
            }
            assert!(Instant::now() < deadline, "the first poll never landed");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            polls.load(Ordering::SeqCst),
            1,
            "one request burst is one poll, not one per ask"
        );
    }

    #[test]
    fn a_failed_poll_keeps_the_last_good_rows_and_stamps_the_failure() {
        let (w, _) = scripted(
            SCHEDULE,
            vec![
                PollResult::Rows { rows: vec![1, 2, 3], ok: 2, total: 2 },
                PollResult::Failed { total: 2 },
            ],
        );
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Warming));
        w.poll_now(&());
        let first = snapshot(&w.read(&(), None));
        assert_eq!(first.0, vec![1, 2, 3]);
        w.poll_now(&());
        let second = snapshot(&w.read(&(), None));
        assert_eq!(second.0, vec![1, 2, 3], "the failure did not replace the rows with nothing");
        assert_eq!((second.2, second.3), (0, 2), "sources_ok says the latest poll found nothing");
        assert_eq!(second.4, first.4, "fetched_at_ms is the last GOOD fetch, so staleness shows");
        assert_ne!(second.1, first.1, "the changed sources_ok is a change a client must see");
    }

    #[test]
    fn a_failed_first_poll_stays_warming_never_an_empty_snapshot() {
        let (w, _) = scripted(SCHEDULE, vec![PollResult::Failed { total: 1 }]);
        let _ = w.read(&(), None);
        w.poll_now(&());
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Warming));
    }

    #[test]
    fn an_equal_generation_is_unchanged_and_an_identical_poll_does_not_move_it() {
        let (w, _) = scripted(
            SCHEDULE,
            vec![
                PollResult::Rows { rows: vec![7], ok: 1, total: 1 },
                PollResult::Rows { rows: vec![7], ok: 1, total: 1 },
                PollResult::Rows { rows: vec![8], ok: 1, total: 1 },
            ],
        );
        let _ = w.read(&(), None);
        w.poll_now(&());
        let g = snapshot(&w.read(&(), None)).1;
        assert!(
            matches!(w.read(&(), Some(g)), ToolFeedOutcome::Unchanged { generation } if generation == g)
        );
        w.poll_now(&());
        assert!(
            matches!(w.read(&(), Some(g)), ToolFeedOutcome::Unchanged { .. }),
            "the same rows twice is the same generation: no churn for a cheap poller"
        );
        w.poll_now(&());
        let after = snapshot(&w.read(&(), Some(g)));
        assert_eq!(after.0, vec![8]);
        assert!(after.1 > g, "new rows mint a larger generation");
        assert!(
            matches!(w.read(&(), Some(g + 1_000_000)), ToolFeedOutcome::Snapshot { .. }),
            "a generation the server never minted (a previous run's) is answered with rows"
        );
    }

    #[test]
    fn a_missing_key_clears_the_rows_and_a_stored_key_brings_them_back() {
        let key = "SOME_KEY_NAME";
        let (w, _) = scripted(
            SCHEDULE,
            vec![
                PollResult::Rows { rows: vec![5], ok: 1, total: 1 },
                PollResult::NotConfigured { key_name: key },
                PollResult::NotConfigured { key_name: key },
                PollResult::Rows { rows: vec![5], ok: 1, total: 1 },
            ],
        );
        let _ = w.read(&(), None);
        w.poll_now(&());
        let g = snapshot(&w.read(&(), None)).1;
        w.poll_now(&());
        assert!(
            matches!(w.read(&(), Some(g)), ToolFeedOutcome::NotConfigured { key_name } if key_name == key),
            "removing the key disarms the feed, even for a client holding the old generation"
        );
        let g_missing = w.generation_of(&());
        w.poll_now(&());
        assert_eq!(w.generation_of(&()), g_missing, "re-reading an absent key churns nothing");
        w.poll_now(&());
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Snapshot { .. }));
    }

    #[test]
    fn two_keys_hold_two_independent_entries_with_distinct_generations() {
        let board = GenerationBoard::new(Arc::new(AtomicU64::new(0)));
        let w = FeedWorker::<String, u32>::new(
            "weeks",
            SCHEDULE,
            Arc::clone(&board),
            Arc::new(|| 0),
            false,
            |k: &String| PollResult::Rows { rows: vec![k.len() as u32], ok: 1, total: 1 },
        );
        let (a, b) = ("2026-10-05".to_string(), "2026-10-12".to_string());
        let _ = (w.read(&a, None), w.read(&b, None));
        w.poll_now(&a);
        w.poll_now(&b);
        let (ga, gb) = (w.generation_of(&a), w.generation_of(&b));
        assert_ne!(ga, gb);
        assert!(
            matches!(w.read(&b, Some(ga)), ToolFeedOutcome::Snapshot { .. }),
            "a client that switched weeks is sent the new week's rows, not Unchanged"
        );
    }

    #[test]
    fn the_retry_schedule_doubles_to_the_cadence_and_never_past_it() {
        let secs = |f| retry_delay(&SCHEDULE, f).as_secs();
        assert_eq!(secs(0), 1800);
        assert_eq!([secs(1), secs(2), secs(3), secs(4)], [60, 120, 240, 480]);
        assert_eq!([secs(5), secs(6), secs(30), secs(u32::MAX)], [960, 1800, 1800, 1800]);
    }

    #[test]
    fn an_empty_success_is_retried_soon_only_where_the_feed_asks_for_it() {
        let retrying = Schedule { retry_when_empty: true, ..SCHEDULE };
        for (schedule, expect_secs) in [(retrying, 60), (SCHEDULE, 1800)] {
            let (w, _) =
                scripted(schedule, vec![PollResult::Rows { rows: vec![], ok: 1, total: 1 }]);
            let _ = w.read(&(), None);
            let before = Instant::now();
            w.poll_now(&());
            let st = locked(&w.state);
            let due = st.entries.get(&()).unwrap().next_due.duration_since(before);
            assert!(
                due.as_secs() >= expect_secs - 1 && due.as_secs() <= expect_secs + 1,
                "{expect_secs}: {due:?}"
            );
        }
    }

    #[test]
    fn a_panicking_fetch_is_a_failed_poll_not_a_dead_worker() {
        let n = Arc::new(AtomicU64::new(0));
        let c = Arc::clone(&n);
        let w = FeedWorker::<(), u32>::new(
            "panics",
            SCHEDULE,
            GenerationBoard::new(Arc::new(AtomicU64::new(0))),
            Arc::new(|| 0),
            false,
            move |_| {
                if c.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("a parser bug");
                }
                PollResult::Rows { rows: vec![1], ok: 1, total: 1 }
            },
        );
        let _ = w.read(&(), None);
        w.poll_now(&());
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Warming));
        w.poll_now(&());
        assert!(matches!(w.read(&(), None), ToolFeedOutcome::Snapshot { .. }));
    }
}
