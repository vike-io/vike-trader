//! **The one funding poll thread of a perp exec start, and the one window it reads the venue's log through.** A venue
//! whose funding payments are not on its private stream reads them from a REST log (binance's income history, bybit's
//! and deribit's transaction log, okx's bills, hyperliquid's `userFunding`) on a thread of its own, attached to the
//! actor through [`super::ExecutionClientActor::with_poller`]. The thread, its cadence, its spawn floor and its
//! warn-once streak are this module's ([`spawn_funding_poll`], [`FundingPollThread::run`]); the venue keeps only its
//! read, its decode and its own warn line, so its log target stays its own.
//!
//! **The restart law.** The thread samples its spawn instant ONCE, before its first read
//! ([`FundingPollThread::floor_ms`]): a payment stamped before it is already inside the wallet the first reconcile pass
//! adopts, so folding it would count it twice (`Account::apply_funding` is not idempotent).
//!
//! **The window** ([`FundingWindow`]). Each poll reads `[since, min(now, since + 7 d)]`: `since` is the floor, then
//! [`FUNDING_LOOKBACK_MS`] before the end of the last GOOD window, so a failed poll loses nothing (the next one reads
//! the same window again), a row the venue lists a moment late is still read, and a long outage is read back a week at
//! a time ([`FUNDING_MAX_WINDOW_MS`], the widest window bybit answers). A row stamped before the window's start is
//! never emitted (an earlier window held it), and inside the window a row id is emitted once. The ids emitted are
//! forgotten BY TIME, once their stamp is older than the next window's start, so the memory is bounded by the lookback
//! and never by a count.
//!
//! A venue whose log takes a start alone and pages on by its own cursor (okx's bills) keeps its own read and its
//! `vike_utility::recent_set::RecentSet` of ids; it still runs on this thread.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use vike_execution::EventSender;
use vike_model::events::Event;
use vike_utility::poller::{STOP_POLL_SLICE, StopHandle, sleep_stop_aware, spawn_poller};

use super::forward_poll;

/// The funding poll's cadence, every polled venue's: one log read a minute keeps live equity current (a venue settles
/// funding at most every hour, most every eight) without spending the account's request budget.
pub const FUNDING_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How far before the end of the last good window the next one starts.
pub const FUNDING_LOOKBACK_MS: i64 = 5 * 60_000;

/// The widest window one poll asks: seven days, bybit's `endTime - startTime` limit and binance's default week.
pub const FUNDING_MAX_WINDOW_MS: i64 = 7 * vike_model::MS_PER_DAY;

/// **The poll thread, as its venue's body is handed it** ([`spawn_funding_poll`]): the spawn floor, the stop flag and
/// the account's event lane. The body builds its venue's read (a client of its own, never the command loop's), then
/// hands it to [`Self::run`].
pub struct FundingPollThread {
    floor_ms: i64,
    stop: Arc<AtomicBool>,
    events: EventSender,
    cadence: Duration,
}

impl FundingPollThread {
    /// The emission floor: this thread's start, in epoch milliseconds (the module doc's restart law).
    #[must_use]
    pub fn floor_ms(&self) -> i64 {
        self.floor_ms
    }

    /// Has the start been told to stop?
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Wait one cadence before a setup step is tried again (deribit's instrument read); `true` when the stop fired
    /// meanwhile and the body should return.
    pub fn wait(&self) -> bool {
        sleep_stop_aware(&self.stop, self.cadence, STOP_POLL_SLICE)
    }

    /// **The poll loop**: `poll(now_ms)` once a cadence until the stop fires or the core is gone, its events forwarded
    /// in order (`forward_poll`). A failed read is read again next cadence and handed to `warn` only on the FIRST
    /// failure of a streak, so the venue's line, under its own target, says an outage once.
    pub fn run<E>(
        self,
        mut poll: impl FnMut(i64) -> Result<Vec<Event>, E>,
        mut warn: impl FnMut(&E),
    ) {
        let mut fail_streak = 0u32;
        while !self.stopped() {
            match forward_poll(poll(vike_model::now_ms()), &self.events, &mut fail_streak) {
                Err(_core_gone) => return,
                Ok(Some(error)) => warn(&error),
                Ok(None) => {}
            }
            if self.wait() {
                return;
            }
        }
    }
}

/// **Spawn a start's funding poll thread** named `name`, through `vike_utility::poller::spawn_poller`: `body` is handed
/// the [`FundingPollThread`] (its floor sampled on the new thread, before anything is read) and runs the venue's setup
/// and then [`FundingPollThread::run`]. Attach the handle with [`super::ExecutionClientActor::with_poller`].
pub fn spawn_funding_poll<F>(name: &str, events: EventSender, body: F) -> StopHandle
where
    F: FnOnce(FundingPollThread) + Send + 'static,
{
    spawn_poller(name, move |stop| {
        let floor_ms = vike_model::now_ms();
        body(FundingPollThread { floor_ms, stop, events, cadence: FUNDING_POLL_INTERVAL });
    })
}

/// **One funding poller's window over its venue's log, and the row ids it has emitted** (the module doc's window), keyed
/// by the venue's row id `K`. A floor of `0` is no floor (a test or a probe): every poll reads from `0` to now and the
/// window never moves, the request a floorless probe always sent.
#[derive(Debug)]
pub struct FundingWindow<K> {
    floor_ms: i64,
    since_ms: i64,
    /// Row id -> its stamp, for the rows emitted that a later window can still list.
    seen: HashMap<K, i64>,
}

impl<K: Eq + Hash> FundingWindow<K> {
    /// A window floored at `floor_ms` (the spawn instant), starting there.
    #[must_use]
    pub fn new(floor_ms: i64) -> Self {
        FundingWindow { floor_ms, since_ms: floor_ms, seen: HashMap::new() }
    }

    /// The emission floor.
    #[must_use]
    pub fn floor_ms(&self) -> i64 {
        self.floor_ms
    }

    /// `(start, end)` of the window the next poll reads at `now_ms`, both ends sent: from `since` to `now`, at most
    /// [`FUNDING_MAX_WINDOW_MS`] past `since`, never ending before it starts.
    #[must_use]
    pub fn span(&self, now_ms: i64) -> (i64, i64) {
        let start = self.since_ms;
        let end =
            if self.floor_ms == 0 { now_ms } else { now_ms.min(start + FUNDING_MAX_WINDOW_MS) };
        (start, end.max(start))
    }

    /// Is row `id`, stamped `stamp_ms`, one to emit now: stamped at or after the window's start and never emitted? A
    /// row it admits is marked emitted at once, so a row two pages of one poll list is one row. ⚠ Ask it only once
    /// every read of the poll succeeded, just before the events are handed over: a row admitted by a poll that then
    /// fails is marked and never emitted.
    pub fn admit(&mut self, id: K, stamp_ms: i64) -> bool {
        if stamp_ms < self.since_ms || self.seen.contains_key(&id) {
            return false;
        }
        self.seen.insert(id, stamp_ms);
        true
    }

    /// The poll that read through `end_ms` succeeded: the next window starts [`FUNDING_LOOKBACK_MS`] before it (never
    /// before the floor, never backwards), and an emitted id stamped before that start is forgotten, since no later
    /// window can list its row. A floorless window does not move.
    pub fn read_through(&mut self, end_ms: i64) {
        if self.floor_ms == 0 {
            return;
        }
        self.since_ms = self.since_ms.max(end_ms - FUNDING_LOOKBACK_MS).max(self.floor_ms);
        let since = self.since_ms;
        self.seen.retain(|_, stamp| *stamp >= since);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::cell::RefCell;

    use vike_execution::event_channel;
    use vike_model::events::OrderAccepted;

    use crate::execution_drive::drain_ready;

    const FLOOR: i64 = 1_700_000_000_000;
    const MINUTE: i64 = 60_000;

    fn accepted(order_client_id: &str) -> Event {
        Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: None,
            timestamp: 0,
        })
    }

    /// **The window starts at the floor, is at most a week wide, and moves to five minutes before the end of the last
    /// good read**: never back, never below the floor.
    #[test]
    fn the_window_starts_at_the_floor_caps_at_a_week_and_moves_by_the_lookback() {
        let mut window = FundingWindow::<i64>::new(FLOOR);
        assert_eq!(window.span(FLOOR + MINUTE), (FLOOR, FLOOR + MINUTE));
        let ten_days = FLOOR + 10 * 86_400_000;
        assert_eq!(
            window.span(ten_days),
            (FLOOR, FLOOR + FUNDING_MAX_WINDOW_MS),
            "a week at a time"
        );
        assert_eq!(window.span(FLOOR - MINUTE), (FLOOR, FLOOR), "never ends before it starts");

        window.read_through(FLOOR + MINUTE);
        assert_eq!(
            window.span(FLOOR + 2 * MINUTE).0,
            FLOOR,
            "the lookback never reaches below the floor"
        );
        window.read_through(FLOOR + 60 * MINUTE);
        assert_eq!(window.span(FLOOR + 61 * MINUTE).0, FLOOR + 55 * MINUTE);
        window.read_through(FLOOR + 30 * MINUTE);
        assert_eq!(
            window.span(FLOOR + 61 * MINUTE).0,
            FLOOR + 55 * MINUTE,
            "the window never moves back"
        );
    }

    /// **A row is emitted once**: one stamped before the window's start never, a new one once (a second page's copy and
    /// the next window's overlap are refused), and its id is forgotten only once its stamp is older than the window.
    #[test]
    fn a_row_is_emitted_once_and_its_id_is_forgotten_by_time() {
        let mut window = FundingWindow::<i64>::new(FLOOR);
        assert!(!window.admit(1, FLOOR - 1), "a row from before the floor is the wallet's");
        assert!(window.admit(2, FLOOR + 10 * MINUTE));
        assert!(!window.admit(2, FLOOR + 10 * MINUTE), "the same row on a second page");
        assert!(window.admit(3, FLOOR + 58 * MINUTE));

        window.read_through(FLOOR + 60 * MINUTE);
        assert!(
            !window.admit(3, FLOOR + 58 * MINUTE),
            "the lookback lists it again, and it is not emitted twice"
        );
        assert!(
            !window.admit(4, FLOOR + 50 * MINUTE),
            "stamped before the new start: an earlier window held it"
        );
        assert_eq!(
            window.seen.len(),
            1,
            "row 2 is older than the window and forgotten; row 3 is kept"
        );
    }

    /// A floorless window (a test or a probe) reads from zero to now and never moves.
    #[test]
    fn a_floorless_window_reads_everything_and_never_moves() {
        let mut window = FundingWindow::<String>::new(0);
        assert_eq!(window.span(FLOOR), (0, FLOOR));
        window.read_through(FLOOR);
        assert_eq!(window.span(FLOOR + MINUTE), (0, FLOOR + MINUTE));
        assert!(window.admit("a".to_string(), 5));
        assert!(!window.admit("a".to_string(), 5));
    }

    /// **The loop**: the events go out in order, a failure is warned only at the start of its streak, a stop ends it,
    /// and so does a closed lane.
    #[test]
    fn the_loop_forwards_in_order_warns_once_a_streak_and_ends_on_a_stop_or_a_closed_lane() {
        let (events, mut ingest) = event_channel(64);
        let stop = Arc::new(AtomicBool::new(false));
        let thread = FundingPollThread {
            floor_ms: FLOOR,
            stop: Arc::clone(&stop),
            events,
            cadence: Duration::from_millis(1),
        };
        let mut answers: Vec<Result<Vec<Event>, &str>> = vec![
            Err("down"),
            Err("still down"),
            Ok(vec![accepted("a"), accepted("b")]),
            Err("down again"),
            Ok(vec![accepted("c")]),
        ];
        answers.reverse();
        let warned = RefCell::new(Vec::new());
        thread.run(
            |_now| {
                let answer = answers.pop().expect("the loop stops after the last answer");
                if answers.is_empty() {
                    stop.store(true, Ordering::Relaxed);
                }
                answer
            },
            |error| warned.borrow_mut().push(*error),
        );
        assert_eq!(*warned.borrow(), ["down", "down again"], "one warn per streak");
        let order_client_ids: Vec<String> = drain_ready(|| ingest.try_recv().ok())
            .iter()
            .map(|event| match event {
                Event::OrderAccepted(a) => a.order_client_id.clone(),
                other => panic!("only the polled events reach the lane: {other:?}"),
            })
            .collect();
        assert_eq!(order_client_ids, ["a", "b", "c"]);

        let (events, ingest) = event_channel(64);
        drop(ingest);
        let thread = FundingPollThread {
            floor_ms: FLOOR,
            stop: Arc::new(AtomicBool::new(false)),
            events,
            cadence: Duration::from_millis(1),
        };
        let mut polls = 0;
        thread.run(
            |_now| {
                polls += 1;
                Ok::<_, &str>(vec![accepted("gone")])
            },
            |_| {},
        );
        assert_eq!(polls, 1, "a closed lane ends the thread");
    }

    /// The spawned thread samples its floor on start, reads at once, and a shutdown joins it without waiting a cadence.
    #[test]
    fn the_spawned_poll_reads_at_once_from_its_own_floor_and_a_shutdown_joins_it() {
        let (events, mut ingest) = event_channel(64);
        let before = vike_model::now_ms();
        let handle = spawn_funding_poll("vike-funding-poll-test", events, move |thread| {
            let floor = thread.floor_ms();
            thread.run(
                move |_now| {
                    Ok::<_, &str>(vec![Event::OrderAccepted(OrderAccepted {
                        order_client_id: floor.to_string(),
                        venue_order_id: None,
                        timestamp: 0,
                    })])
                },
                |_| {},
            );
        });
        let Some(vike_execution::Ingest::Event(Event::OrderAccepted(first))) =
            ingest.blocking_recv()
        else {
            panic!("the first poll's event");
        };
        let floor: i64 = first.order_client_id.parse().expect("the floor");
        assert!(floor >= before, "the floor is the thread's start");
        handle.shutdown();
    }
}
