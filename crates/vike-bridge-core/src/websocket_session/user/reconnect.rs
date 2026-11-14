//! **How a private stream dials again, and how loud it is while it cannot** (owner ruling B-13: one rule
//! for every venue, each venue's values passed in).
//!
//! - [`ReconnectBackoff`]: the wait before the next dial, `initial` doubling to `max` (a venue's exec-lane
//!   `vike_venues::venue_timings::VenueTimingsReconnect`), the ONE rule for when it starts over, and the wait after a
//!   run of refused opens (a 401: a hot loop on a persistent refusal is what B-13 retired).
//! - [`ReconnectLog`]: a persistent failure speaks when it starts, then on a quiet interval that grows to a
//!   ceiling, and starts over once it clears (the cure for 53,160 lines a day on one lane, measured on
//!   the CI box on 2026-08-23; [`RECONNECT_LOG`] carries that lane's numbers).
//!
//! Both are pure state: the caller owns the clock and the sleep, so an own-loop venue (SSE, chunked HTTP,
//! Lightstreamer, protobuf over TCP) takes them as they are. The shared WS pump
//! (`super::run_user`) runs on [`ReconnectBackoff`]; its spec-built spawns
//! (`super::spawn_user`, `super::spawn_user_with_resync`) also pace its failed-open line with [`RECONNECT_LOG`].

use std::time::Duration;

/// The wait before a private stream dials again: `initial`, doubling per wait handed out, capped at `max`.
///
/// **The one reset rule.** A successful open does NOT start the ladder over, because a server may accept
/// and close at once for ever; a session that then LIVED twice the ceiling does ([`Self::ended`]), because
/// a socket that was fine for minutes has no reason to stay dark for the ceiling after it closes. A shorter
/// session keeps doubling, and a longer one reopens at most once per `2 x max`, against the `max` a
/// failing open retries at.
///
/// **A refused open** ([`Self::refused`], a venue saying 401 to the key) waits like a failed one until
/// `after` refusals in a row, then `wait` each time ([`Self::with_refusal_wait`]); only an open that worked
/// ([`Self::opened`]) ends the run, so a failure of another kind in between does not let a refusal loop
/// past the bound. Without it a refusal is a failed open.
///
/// **Two modes a venue opts into** (IG's trade stream takes both):
///
/// - [`Self::with_refusal_budget`]: the first `n` refusals in a row wait `initial` FLAT (a re-login may still cure
///   them, so each is worth a prompt retry), and past the budget the ladder climbs from `initial`. The two kinds
///   of failure are separate runs: a failed open ends a run of refusals, and either kind starts the ladder over
///   when it follows the other.
/// - [`Self::with_reset_after`]: a session that lived `lived` or longer starts the ladder over, in place of the
///   twice-the-ceiling rule, and its end waits `initial` without taking the new ladder's first step (with
///   `Duration::ZERO`, every session that opened).
#[derive(Debug, Clone)]
pub struct ReconnectBackoff {
    initial: Duration,
    max: Duration,
    next: Duration,
    refusal: Option<(u32, Duration)>,
    refused: u32,
    /// [`Self::with_refusal_budget`]'s `n`.
    budget: Option<u32>,
    /// [`Self::with_reset_after`]'s `lived`.
    reset_after: Option<Duration>,
}

impl ReconnectBackoff {
    /// The ladder from `initial` to `max`. The first wait is `initial` even above `max`; a reset lands on
    /// the smaller of the two.
    #[must_use]
    pub fn new(initial: Duration, max: Duration) -> Self {
        Self {
            initial,
            max,
            next: initial,
            refusal: None,
            refused: 0,
            budget: None,
            reset_after: None,
        }
    }

    /// From the `after`-th refused open in a row on, wait `wait` (the venue's value).
    #[must_use]
    pub fn with_refusal_wait(mut self, after: u32, wait: Duration) -> Self {
        self.refusal = Some((after, wait));
        self
    }

    /// The flat-inside-the-refusal-budget mode (the type's doc): `budget` refusals in a row wait `initial`.
    #[must_use]
    pub fn with_refusal_budget(mut self, budget: u32) -> Self {
        self.budget = Some(budget);
        self
    }

    /// The session-lifetime reset (the type's doc): a session that lived `lived` starts the ladder over.
    #[must_use]
    pub fn with_reset_after(mut self, lived: Duration) -> Self {
        self.reset_after = Some(lived);
        self
    }

    /// The next wait of the ladder, which then doubles toward `max`.
    fn step(&mut self) -> Duration {
        let wait = self.next;
        self.next = (wait * 2).min(self.max);
        wait
    }

    /// An open failed: the wait before the next one.
    pub fn failed(&mut self) -> Duration {
        if self.budget.is_some() && std::mem::take(&mut self.refused) > 0 {
            // A failed open ends a run of refusals, and its own ladder starts over.
            self.next = self.initial;
        }
        self.step()
    }

    /// An open the venue refused: [`Self::failed`]'s wait, or the refusal wait once the run reaches it; with a
    /// refusal budget, `initial` inside it and the ladder past it.
    pub fn refused(&mut self) -> Duration {
        self.refused = self.refused.saturating_add(1);
        if let Some(budget) = self.budget {
            if self.refused <= budget {
                // Inside the budget: flat, and the first refusal past it is the ladder's first step.
                self.next = self.initial;
                return self.initial;
            }
            return self.step();
        }
        match self.refusal {
            Some((after, wait)) if self.refused >= after => wait,
            _ => self.step(),
        }
    }

    /// An open worked: a run of refusals is over (the ladder is not, see the type's doc).
    pub fn opened(&mut self) {
        self.refused = 0;
    }

    /// A session that had opened ended after `lived`: the wait before the next open, from the start of the
    /// ladder when it lived at least twice the ceiling (or `lived` past [`Self::with_reset_after`]'s).
    pub fn ended(&mut self, lived: Duration) -> Duration {
        if let Some(after) = self.reset_after {
            if lived < after {
                return self.step();
            }
            self.refused = 0;
            self.next = self.initial;
            return self.initial;
        }
        if lived >= self.max * 2 {
            self.next = self.initial.min(self.max);
        }
        self.step()
    }
}

/// What a [`ReconnectLog`] says on one turn of a failure that holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectLogLine {
    /// The failure has just started: say so. `next_in` is how long the lane stays quiet now.
    Onset { next_in: Duration },
    /// Still failing and its interval is up. `down_for` counts from the onset; `next_in` is the grown
    /// interval that follows this line, so a reader can tell a longer silence from a recovery.
    Heartbeat { down_for: Duration, next_in: Duration },
    /// Still failing, interval not up: say nothing.
    Quiet,
}

/// The log pace of ONE persistent failure: a line at the onset, then one per interval, the interval
/// growing by `decay` per line up to `ceiling`, and everything forgotten on [`Self::clear`] so the next
/// failure starts responsive. Past the ceiling the lane still speaks once per ceiling: an outage that stops
/// being mentioned reads as one that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectLog {
    first: Duration,
    decay: u32,
    ceiling: Duration,
    /// When the failure began; `None` while it is not on.
    since: Option<Duration>,
    /// When it last spoke: the interval is measured from here, never from `since` (that would speak on
    /// every turn once the outage outlived one interval).
    last_said: Option<Duration>,
    /// Heartbeats spoken in this failure (the onset is not one).
    steps: u32,
}

/// The pace every private stream logs a failure it cannot open past at: 5 minutes after the onset, then
/// 15, 45, then hourly. A DISK-SAFETY bound with no venue in it: one lane that spoke on every turn of a
/// one-second loop wrote 53,160 lines (23 MB) in a day on the CI box (2026-08-23), and a flat 300 s still left
/// 288 lines a day about a failure already known.
pub const RECONNECT_LOG: ReconnectLog =
    ReconnectLog::new(Duration::from_secs(300), 3, Duration::from_secs(3600));

impl ReconnectLog {
    /// A pace starting at `first`, growing by `decay` per line, capped at `ceiling`.
    #[must_use]
    pub const fn new(first: Duration, decay: u32, ceiling: Duration) -> Self {
        Self { first, decay, ceiling, since: None, last_said: None, steps: 0 }
    }

    /// One turn on which the failure holds, at `now` (any monotonic reading the caller keeps).
    pub fn tick(&mut self, now: Duration) -> ReconnectLogLine {
        let (Some(since), Some(said)) = (self.since, self.last_said) else {
            self.since = Some(now);
            self.last_said = Some(now);
            self.steps = 0;
            return ReconnectLogLine::Onset { next_in: self.interval(0) };
        };
        if now.saturating_sub(said) < self.interval(self.steps) {
            return ReconnectLogLine::Quiet;
        }
        self.last_said = Some(now);
        self.steps = self.steps.saturating_add(1);
        ReconnectLogLine::Heartbeat {
            down_for: now.saturating_sub(since),
            next_in: self.interval(self.steps),
        }
    }

    /// The failure cleared: the next one is a new onset with the first interval.
    pub fn clear(&mut self) {
        (self.since, self.last_said, self.steps) = (None, None, 0);
    }

    /// The quiet period after the `steps`-th heartbeat. Saturating: an overflow returns the ceiling, never
    /// a short interval.
    fn interval(&self, steps: u32) -> Duration {
        let factor = self.decay.checked_pow(steps).unwrap_or(u32::MAX);
        self.first.checked_mul(factor).unwrap_or(self.ceiling).min(self.ceiling)
    }
}

#[path = "reconnect_tests.rs"]
#[cfg(test)]
mod reconnect_tests;
