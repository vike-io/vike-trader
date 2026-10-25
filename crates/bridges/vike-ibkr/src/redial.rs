//! **The supervised re-dial of the price stream's data connection** (any-symbol price lane, decision 0174): the
//! POLICY, pure. [`crate::quote_handle::LazyQuotes`] holds the symbols and does the I/O; this module decides WHEN,
//! over a clock it is handed (every method takes `now`), so the tests drive a fake one.
//!
//! # What a dead connection is
//!
//! The price stream's pumps end when the connection under them goes: a reset or closed socket, a reconnect `ibapi`
//! gave up, a connectivity-lost notice (`crate::market_feed::connection_gone`). `IbkrFeeds::is_dead` reports it and
//! the supervisor asks it of every held symbol each second. A request IB REFUSED (no such contract) is not a dead
//! connection and is not supervised: that stream stays quiet and its price ages out, as it always did.
//!
//! # The schedule of one outage
//!
//! * **At once** the outage is disclosed: `GapStart` on the `quotes` lane of every held symbol, ONCE per outage
//!   (the dead-man opens its grace window on it, decision 0038). A symbol subscribed during the outage is told the
//!   same way on the next look.
//! * **[`SELF_HEAL`] (20 s) of grace** for `ibapi`'s own reconnect (Fibonacci retries: 1, 1, 2, 3, 5, 8 s). A
//!   connection that came back by itself is JOINED by the re-dial (`crate::data_session`: no dial, no budget).
//! * **Then one dial per backoff step**, never faster than the data lane allows: a dial that was ATTEMPTED and failed
//!   (the gateway answered with a refusal or did not answer, the new connection could not be subscribed, or it died
//!   again inside [`LIVE_AFTER`]) waits [`backoff`] before the next: 60 s, 2 min, then 5 min, the table the gate's
//!   rebuild schedule and its data backoff use. The outage never resets the ladder until it ENDS.
//! * **A refusal that dialled nothing costs nothing.** Before every dial the supervisor asks whether the gateway is
//!   ready (`crate::data_door::redial_ready`: the port answers, and `GatewayGate::data_ready`: no order-lane dial
//!   holds or waits for the turn, the rebuild schedule is not armed, the data lane's backoff and its hourly budget
//!   allow it). Not ready is [`Redial::not_ready`]: asked again no sooner than the answer says (at least
//!   [`PROBE_EVERY`], 3 s), and neither a spent attempt nor a backoff step. So a gateway whose port is closed (its
//!   daily logoff, an operator restart) is probed every 3 s for nothing at all, and the first dial is made when the
//!   port is back.
//! * **`Live` only after [`LIVE_AFTER`] (5 s)** of a re-dialled connection that holds every held symbol subscribed:
//!   the dead-man reads the CONNECTION, so `Live` is not held back for a first quote (a stock market at night sends
//!   none), but a connection that is accepted and dropped at once does not close the window or open a new one. A
//!   flapping gateway is therefore ONE outage with ONE `GapStart` and one `Live` at its end, however often it
//!   drops, and the ladder keeps climbing.
//!
//! # The hourly budget is shared with the lookups
//!
//! The gate gives the data lane six dials an hour per gateway for everyone. Two callers spend it: the dials that ASK
//! for a connection (the stream's first dial, an any-symbol lookup, the backfill: `DataLane::Lookup`) and this
//! re-dial (`DataLane::Redial`). **Each lane may hold at most four of the six** (`LANE_RESERVE`, the gate's
//! module doc has the argument), so a flood of lookups on a dead gateway cannot starve the re-dial that would heal
//! them, and a flapping stream cannot starve a lookup. They also HEAL each other: a lookup that finds the
//! connection down dials it for the stream too (one connection per gateway session), and the stream's next attempt
//! JOINS that connection with no dial at all. When the lane's share is spent the supervisor stays down: `GapStart`
//! stays open, nothing is dialled, and the journal says how long until the gate allows the next dial.
//!
//! # IB's daily logoff
//!
//! IBKR logs the gateway off every day at **23:45:00 UTC** and `deploy/vike-ibkr-gateway.service` starts it again
//! (`Restart=always`; the IBC settings leave `AutoRestartTime=` empty and `IbAutoClosedown=no`, so nothing else
//! restarts it, `deploy/ibkr-gateway/write-ibc-config.sh`). The port is closed until the gateway is back, **about
//! 85 s later** on the box (23:45:00 to 23:46:25 on 2026-10-13, `docs/ops/ibkr-socket-gateway.md`'s "After the
//! daily logoff"). An outage that began in that window carries a `quiet_until` hint: no dial before
//! [`LOGOFF_BACK_AFTER`] (80 s) past the logoff, so the first dial is the one that can succeed. With the port probe
//! it costs the budget one dial, not a loop. ⚠ The time and the 85 s are observed (one night), not configured; a
//! gateway that logs off at another time gets the plain schedule (a probe of a closed port is free).
//!
//! ⚠ **What this does to the link dead-man.** `GapStart` on a held symbol opens the dead-man's grace window for
//! ibkr's price-only venue (`crate::quote_handle` has the sentence), and the daily logoff opens that window every
//! night: the outage here lasts about the gateway's own 85 s plus the probe (<= 3 s), the dial (the gate's 2 s gap)
//! and [`LIVE_AFTER`], i.e. about 100 s, against the default grace of 120 s. The trade link opens its own window for
//! the same logoff (the gatewayhub's rebuild plus the bridge's learning, which this crate cannot bound), and any
//! source past the grace trips the venue.

use std::time::{Duration, Instant};

/// The waits before the 1st, 2nd, ... failed dial's retry; the last repeats (the 5 minute cap). ONE table for the
/// gateway: the rebuild schedule of a lost link (`Heal`), the data lane's backoff (`DataDials`) and this re-dial.
const BACKOFF: [Duration; 3] =
    [Duration::from_secs(60), Duration::from_secs(120), Duration::from_secs(300)];

/// The wait after `failures` failed dials (the first failure is 1).
pub(crate) fn backoff(failures: u32) -> Duration {
    BACKOFF[(failures.max(1) as usize - 1).min(BACKOFF.len() - 1)]
}

/// How long a dead connection is left to `ibapi`'s own reconnect before the first dial.
pub(crate) const SELF_HEAL: Duration = Duration::from_secs(20);

/// How often a gateway that is not ready (its port closed, a turn held) is asked again: the cadence the gate's port
/// watch looks at the port.
pub(crate) const PROBE_EVERY: Duration = Duration::from_secs(3);

/// How long a re-dialled connection must hold every symbol subscribed before the outage is over.
pub(crate) const LIVE_AFTER: Duration = Duration::from_secs(5);

/// IBKR's daily logoff of the gateway, in seconds into the UTC day (23:45:00).
pub(crate) const DAILY_LOGOFF_UTC_SECS: i64 = (23 * 60 + 45) * 60;

/// An outage that began up to this many seconds BEFORE the logoff is read as part of it (a clock a little ahead).
const LOGOFF_EARLY_SECS: i64 = 30;

/// How long after the logoff the gateway can first be back: the 85 s measured, less a margin so a quicker box is not
/// held. Past it the plain schedule and the port probe decide.
pub(crate) const LOGOFF_BACK_AFTER: Duration = Duration::from_secs(80);

/// The time left in the daily logoff's quiet window for an outage that began at `wall_ms` (epoch ms), if it began in it.
fn daily_logoff_quiet(wall_ms: i64) -> Option<Duration> {
    let secs_into_day = wall_ms.div_euclid(1000).rem_euclid(86_400);
    let ends = DAILY_LOGOFF_UTC_SECS + LOGOFF_BACK_AFTER.as_secs() as i64;
    let window = (DAILY_LOGOFF_UTC_SECS - LOGOFF_EARLY_SECS)..ends;
    if window.contains(&secs_into_day) {
        Some(Duration::from_secs((ends - secs_into_day) as u64))
    } else {
        None
    }
}

/// What one look at the held streams found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Every held stream is alive and no outage is open.
    Healthy,
    /// The outage ended on this look: `Live` for every symbol told down.
    Recovered,
    /// An outage is open. `opened`: this look opened it (disclose, once). `attempt`: a dial is due now (ask the
    /// gateway whether it is ready, then dial).
    Down { opened: bool, attempt: bool },
}

/// The outage state of one price stream: none, or the one open outage. See the module doc.
#[derive(Debug, Default)]
pub(crate) struct Redial {
    outage: Option<Outage>,
}

#[derive(Debug)]
struct Outage {
    /// When the outage was seen to begin.
    began: Instant,
    /// Dials ATTEMPTED and failed since it began: the index into [`BACKOFF`].
    failed: u32,
    /// No dial before this: the self-heal wait or the daily logoff's quiet, then each failed dial's backoff.
    next_ok: Instant,
    /// The daily logoff's hint, when the outage began in its window.
    quiet_until: Option<Instant>,
    /// The gateway is not asked before this (a closed port, a held turn, the lane's budget).
    probe_at: Instant,
    /// When a re-dial last put every held symbol back on a connection; the outage ends [`LIVE_AFTER`] later if it
    /// stands.
    redialled: Option<Instant>,
}

impl Outage {
    fn dial_failed(&mut self, now: Instant) {
        self.failed += 1;
        self.next_ok = now + backoff(self.failed);
        self.probe_at = self.next_ok;
    }
}

impl Redial {
    /// Whether an outage is open.
    #[cfg(test)]
    pub(crate) fn is_down(&self) -> bool {
        self.outage.is_some()
    }

    /// One look at the held streams: `dead` = at least one held symbol's stream is dead; `wall_ms` is the wall clock
    /// (epoch ms), read only to recognise the daily logoff.
    pub(crate) fn observe(&mut self, now: Instant, wall_ms: i64, dead: bool) -> Step {
        let Some(o) = self.outage.as_mut() else {
            if !dead {
                return Step::Healthy;
            }
            let quiet_until = daily_logoff_quiet(wall_ms).map(|q| now + q);
            let next_ok = quiet_until.map_or(now + SELF_HEAL, |q| q.max(now + SELF_HEAL));
            self.outage = Some(Outage {
                began: now,
                failed: 0,
                next_ok,
                quiet_until,
                probe_at: next_ok,
                redialled: None,
            });
            return Step::Down { opened: true, attempt: false };
        };
        match (o.redialled, dead) {
            // Healed with no dial (the dead symbol went away), or the re-dialled connection stood its probation.
            (None, false) => {}
            (Some(at), false) if now.saturating_duration_since(at) >= LIVE_AFTER => {}
            (Some(_), false) => return Step::Down { opened: false, attempt: false },
            // Dead again inside the probation: that dial FAILED, and the ladder goes on from where it was.
            (Some(_), true) => {
                o.redialled = None;
                o.dial_failed(now);
            }
            (None, true) => {}
        }
        if !dead {
            self.outage = None;
            return Step::Recovered;
        }
        Step::Down { opened: false, attempt: now >= o.next_ok && now >= o.probe_at }
    }

    /// The gateway was not ready for a dial: ask again no sooner than `wait` (and no sooner than [`PROBE_EVERY`]).
    /// Not an attempt: nothing is spent and the ladder does not move.
    pub(crate) fn not_ready(&mut self, now: Instant, wait: Duration) {
        if let Some(o) = self.outage.as_mut() {
            o.probe_at = now + wait.max(PROBE_EVERY);
        }
    }

    /// A dial was ATTEMPTED and failed (or its connection could not be subscribed): the next one waits [`backoff`].
    pub(crate) fn dial_failed(&mut self, now: Instant) {
        if let Some(o) = self.outage.as_mut() {
            o.dial_failed(now);
        }
    }

    /// A dial succeeded and every held symbol is subscribed on the new connection: the outage ends in [`LIVE_AFTER`]
    /// if the connection stands.
    pub(crate) fn redialled(&mut self, now: Instant) {
        if let Some(o) = self.outage.as_mut() {
            o.redialled = Some(now);
        }
    }

    /// Dials attempted and failed in the open outage (0 when none is open).
    pub(crate) fn failed(&self) -> u32 {
        self.outage.as_ref().map_or(0, |o| o.failed)
    }

    /// How long the open outage has lasted (zero when none is open).
    pub(crate) fn down_for(&self, now: Instant) -> Duration {
        self.outage.as_ref().map_or(Duration::ZERO, |o| now.saturating_duration_since(o.began))
    }

    /// The daily logoff's hint of the open outage: no dial before this.
    pub(crate) fn quiet_until(&self) -> Option<Instant> {
        self.outage.as_ref().and_then(|o| o.quiet_until)
    }

    /// The earliest the next dial may be tried, by the schedule (the gateway's readiness is asked beside it).
    pub(crate) fn next_dial(&self) -> Option<Instant> {
        self.outage.as_ref().map(|o| o.next_ok)
    }
}

#[cfg(test)]
#[path = "redial_tests.rs"]
mod redial_tests;
