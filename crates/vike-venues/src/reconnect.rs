//! **How long a connection waits before it dials again**: [`ReconnectBackoff`], the ONE "wait, then double, up to a
//! cap" of every reconnect loop in the workspace (owner ruling B-13: one rule for every venue, each venue's values
//! passed in; connection plan B3). Its users: the private-stream and market pumps of `vike-bridge-core`, the
//! gatewayhub's session supervisor, the own loops of the bridges (cTrader, dukascopy, IG, oanda, IBKR cpapi), and the
//! hub links (`vike-market-stream`'s reader and its refused-replay retry, the gatewayhub client's trade-lease redial
//! and options feed). Each loop's waits are pinned verbatim beside it.
//!
//! **Why it lives here** (it moved down from `vike_bridge_core::websocket_session::user::reconnect` on 2026-11-22):
//! the layer rule (`crates/vike-ops/tests/architecture/layer_gate.rs`) kept the hub links off it, since
//! `vike-gatewayhub-client` is layer 25 like `vike-bridge-core` and `vike-market-stream` is 22, so each waited by hand.
//! This crate (layer 12) is below every user, every user already links it, and it owns the row
//! [`ReconnectBackoff::from_row`] reads (`crate::venue_timings`' `VenueTimingsReconnect`). Not `vike-math`: that crate
//! holds pure FORMULAS that name no venue concept and is published, and this is a connection's state machine whose
//! verbs are a venue's (a refused open, a session that ended, an announced close). Not `vike-utility`: its helpers
//! carry no domain meaning and it names no `vike-*` crate, so `from_row` could not live there. ⚠ This crate is in
//! `vike-core`'s normal closure, so an edit here runs the latency gate (`crates/vike-venues/CLAUDE.md`).
//!
//! It is pure state: the caller owns the clock and the sleep, so an own-loop venue (SSE, chunked HTTP, Lightstreamer,
//! protobuf over TCP, a sidecar process) and a hub link take it as it is. The log pace of a failure that holds is
//! `vike_bridge_core::websocket_session::user::reconnect`'s `ReconnectLog`, not this.
//!
//! **A venue row's ladder spreads its waits** ([`ReconnectBackoff::from_row`]): every account and lane on one venue
//! loses its socket in the same instant when the venue does, and on one exact ladder they would all dial again in
//! the same instant, every rung. So each wait the doubling hands out grows by a share of up to half of itself, drawn
//! from the ladder's own seeded source (each row ladder its own seed), never past `max`: the schedule's bounds hold (never sooner than the rung, never later than the ceiling), a fixed row
//! and a ladder at its ceiling are not spread, and neither is a venue's declared flat wait (the refusal wait, the end
//! wait, the refusal budget's flat first step). A ladder built with [`ReconnectBackoff::new`] (a hub link, a poll of
//! our own) hands out its schedule exactly. A test that pins a row's waits takes
//! [`ReconnectBackoff::without_jitter`], and one that pins the spread injects its source,
//! [`ReconnectBackoff::with_jitter_seed`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use vike_math::random::SplitMix64;

use crate::venue_timings::VenueTimingsReconnect;

/// The wait before a connection dials again: `initial`, doubling per wait handed out, capped at `max`.
///
/// **When it starts over** is the loop's own rule, each kept as it was when the loop moved onto this type:
///
/// - **[`Self::ended`]** (the private-stream pump): a successful open does NOT start the ladder over, because a server
///   may accept and close at once for ever; a session that then LIVED twice the ceiling does, because a socket that
///   was fine for minutes has no reason to stay dark for the ceiling after it closes. A shorter session keeps
///   doubling, and a longer one reopens at most once per `2 x max`, against the `max` a failing open retries at.
/// - **[`Self::with_reset_after`]** (below).
/// - **[`Self::reset`]**, called by a loop on an event of its own: the market pump's connect that worked, the
///   gatewayhub supervisor's restore that worked (and its calendar hold), cTrader's renewed grant, dukascopy's
///   sidecar that lived the ceiling, the trade lease that opened, and a hub stream's or options feed's healthy (or
///   the stream's parked) session.
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
///
/// And [`Self::with_end_wait`]: a session's end waits a flat wait of its own and leaves the ladder where it was (the
/// IBKR cpapi pump's nap after a drop, while its failed dials wait the row's).
///
/// And [`Self::with_silence_spared`], which moves no wait: a session the lane's idle bound ended is a reopen, not a
/// failure ([`Self::silence_spared`], what the shared pump reads a silent session by), for a lane whose venue documents
/// no heartbeat cadence, so that an idle account's quiet stream is no outage (alpaca's SSE lane).
///
/// **The spread** (the module doc): each doubling step of a [`Self::from_row`] ladder, `wait`, is handed out as
/// `wait + u`, `u` uniform in `0 ..= min(wait / 2, max - wait)` from the ladder's own seeded source. The ladder's
/// arithmetic (what the next step is) reads the unspread `wait`, so the spread never accumulates. ⚠ A clone draws the
/// same spread as its original: build one ladder per connection.
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
    /// [`Self::with_end_wait`]'s `wait`.
    end_wait: Option<Duration>,
    /// [`Self::with_silence_spared`].
    silence_spared: bool,
    /// The source of the spread (the type's doc); `None` hands out the schedule exactly.
    jitter: Option<SplitMix64>,
}

impl ReconnectBackoff {
    /// The ladder from `initial` to `max`, handed out exactly. The first wait is `initial` even above `max`, and so is
    /// the first after [`Self::reset`]; [`Self::ended`]'s reset lands on the smaller of the two.
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
            end_wait: None,
            silence_spared: false,
            jitter: None,
        }
    }

    /// The ladder of a declared row (`crate::venue_timings`): its `initial` doubling to its `max`, a fixed wait
    /// when the two are equal, each step spread (the module doc) from a source seeded with the count of row
    /// ladders this process built before it, so no two draw alike. A count, not ambient randomness: this crate is
    /// determinism-critical (`crates/vike-ops/tests/architecture/clock_pin.rs`'s `DETERMINISM_CRITICAL_CRATES`).
    #[must_use]
    pub fn from_row(row: &VenueTimingsReconnect) -> Self {
        static BUILT: AtomicU64 = AtomicU64::new(0);
        Self::new(row.initial, row.max).with_jitter_seed(BUILT.fetch_add(1, Ordering::Relaxed))
    }

    /// Spread the steps from a source seeded with `seed`: the same seed draws the same spread, so a test pins it.
    #[must_use]
    pub fn with_jitter_seed(mut self, seed: u64) -> Self {
        self.jitter = Some(SplitMix64::new(seed));
        self
    }

    /// Hand out the schedule exactly: what a test that pins a row's waits asks for.
    #[must_use]
    pub fn without_jitter(mut self) -> Self {
        self.jitter = None;
        self
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

    /// The flat end wait (the type's doc): a session that opened ends into `wait`, however long it lived, in place of
    /// [`Self::ended`]'s start-over rules.
    #[must_use]
    pub fn with_end_wait(mut self, wait: Duration) -> Self {
        self.end_wait = Some(wait);
        self
    }

    /// The quiet-session mode (the type's doc): a session its idle bound ended is a reopen. It moves no wait.
    #[must_use]
    pub fn with_silence_spared(mut self) -> Self {
        self.silence_spared = true;
        self
    }

    /// Whether a session the lane's idle bound ended is a reopen ([`Self::with_silence_spared`]), not a failure. The
    /// shared pump reads it (`vike_bridge_core::websocket_session::user`'s `run_user`).
    #[must_use]
    pub fn silence_spared(&self) -> bool {
        self.silence_spared
    }

    /// The next wait of the ladder, which then doubles toward `max`, unspread.
    fn step(&mut self) -> Duration {
        let wait = self.next;
        self.next = (wait * 2).min(self.max);
        wait
    }

    /// The next step as handed out: [`Self::step`] spread (the type's doc).
    fn stepped(&mut self) -> Duration {
        let wait = self.step();
        self.spread(wait)
    }

    /// `wait` plus a draw in `0 ..= min(wait / 2, max - wait)`; `wait` itself with no source or no room under `max`.
    fn spread(&mut self, wait: Duration) -> Duration {
        let Some(source) = self.jitter.as_mut() else { return wait };
        let room = (wait / 2).min(self.max.saturating_sub(wait));
        let room_nanos = u64::try_from(room.as_nanos()).unwrap_or(u64::MAX);
        if room_nanos == 0 {
            return wait;
        }
        wait + Duration::from_nanos(source.next_u64() % room_nanos.saturating_add(1))
    }

    /// An open failed: the wait before the next one.
    pub fn failed(&mut self) -> Duration {
        if self.budget.is_some() && std::mem::take(&mut self.refused) > 0 {
            // A failed open ends a run of refusals, and its own ladder starts over.
            self.next = self.initial;
        }
        self.stepped()
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
            return self.stepped();
        }
        match self.refusal {
            Some((after, wait)) if self.refused >= after => wait,
            _ => self.stepped(),
        }
    }

    /// Whether the run of refusals [`Self::refused`] last counted is still inside the venue's refusal mode: within
    /// [`Self::with_refusal_budget`]'s budget, or short of [`Self::with_refusal_wait`]'s `after`-th refusal. `false`
    /// once the run reaches that bound, and with neither mode (a refusal is then a failed open). The shared pump reads
    /// a refusal this spares as a reopen, not a failure (`vike_bridge_core::websocket_session::user`'s `run_user`).
    #[must_use]
    pub fn refusal_spared(&self) -> bool {
        match (self.budget, self.refusal) {
            (Some(budget), _) => self.refused <= budget,
            (None, Some((after, _))) => self.refused < after,
            (None, None) => false,
        }
    }

    /// An open worked: a run of refusals is over (the ladder is not, see the type's doc).
    pub fn opened(&mut self) {
        self.refused = 0;
    }

    /// The loop's own start-over event (the type's doc): the next wait handed out is `initial` again. A run of
    /// refusals is [`Self::opened`]'s to end, not this.
    pub fn reset(&mut self) {
        self.next = self.initial;
    }

    /// A session that had opened ended after `lived`: the wait before the next open, from the start of the
    /// ladder when it lived at least twice the ceiling (or `lived` past [`Self::with_reset_after`]'s); with
    /// [`Self::with_end_wait`], that wait and the ladder untouched.
    pub fn ended(&mut self, lived: Duration) -> Duration {
        let end = self.end(lived);
        self.hand_out(end)
    }

    /// [`Self::ended`]'s wait unspread, and whether it is a step of the ladder (a flat wait is not spread).
    fn end(&mut self, lived: Duration) -> (Duration, bool) {
        if let Some(wait) = self.end_wait {
            return (wait, false);
        }
        if let Some(after) = self.reset_after {
            if lived < after {
                return (self.step(), true);
            }
            self.refused = 0;
            self.next = self.initial;
            return (self.initial, false);
        }
        if lived >= self.max * 2 {
            self.next = self.initial.min(self.max);
        }
        (self.step(), true)
    }

    /// [`Self::end`]'s wait as handed out: a step spread, a flat wait as it is.
    fn hand_out(&mut self, (wait, step): (Duration, bool)) -> Duration {
        if step { self.spread(wait) } else { wait }
    }

    /// A session the venue ANNOUNCED it would end (a notice frame ahead of the cut), ended after `lived`: no wait
    /// when [`Self::ended`] would start the ladder over, and that fresh ladder's first step stays unspent; a shorter
    /// session waits [`Self::ended`]'s wait. So a venue that announces the end of every new session (a rolling restart
    /// landing each reopen on another server about to go) climbs the ladder, never a hot loop.
    pub fn announced(&mut self, lived: Duration) -> Duration {
        let fresh = lived >= self.reset_after.unwrap_or(self.max * 2);
        let end = self.end(lived);
        if !fresh {
            return self.hand_out(end);
        }
        // A flat end wait took no step, so there is none to give back: the ladder stays where it was.
        if self.end_wait.is_none() {
            self.next = end.0;
        }
        Duration::ZERO
    }
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod reconnect_tests;
