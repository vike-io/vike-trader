//! **The reconnect loop of a venue SESSION the daemon supervises** (decision 0164 verdict 7(b), wave 2 W14):
//! watch a link, notice that it dropped, and bring it back with backoff for exactly as long as the
//! operator's account rows still say the account runs.
//!
//! This module is the policy and nothing else. It names no venue, no socket, no store and no clock: a
//! [`Link`] answers four questions (is it reachable, may it run, take it down, bring it back) and a
//! [`Pause`] waits, so the loop is driven by a test with a scripted link and a pause that records instead
//! of sleeping. The one venue that uses it today is IBKR (`crate::trade::ibkr`): IB logs every gateway off
//! once a day and the socket client the bridge built dies with it for good (`IbInbound::StreamDead`), so
//! the daemon, the one API client of the gateway, rebuilds the adapter.
//!
//! # The loop
//!
//! ```text
//!  UP     every `probe_every`:
//!           the link says it is gone (Link::reported_lost)   --------> Link::lost(why)  -> DOWN
//!           [`LOSS_PROBES`] failed probes in a row (reachable?) -----> Link::lost(why)  -> DOWN
//!  DOWN   every `probe_every`:
//!           1. Link::verdict()          the rows are re-read HERE, on every pass
//!                Stop(why)  -> Link::gave_up(why), the loop ENDS (an account that is no longer
//!                              active at its tier is not brought back)
//!                Retry(why) -> the rows could not be READ: nothing is restored this pass and the
//!                              next read waits (1, 2, 4 .. passes, up to `max_wait`); the loop goes on
//!           2. reachable AND the backoff is paid -> Link::restore()
//!                Ok  -> UP again (backoff reset)
//!                Err -> the next attempt waits twice as long, up to `max_wait`
//! ```
//!
//! * **One failed probe is not a loss.** A healthy session is torn down only after [`LOSS_PROBES`]
//!   consecutive failures (a localhost connect that fails once is noise; the cost of a false loss is a
//!   lease bounce, a new epoch and an empty id map). A loss the link REPORTS itself (the bridge saying
//!   its stream is dead) needs no second opinion.
//! * **A rows read that failed is neither a yes nor a no.** The store being locked for a moment must not
//!   end the supervision for good (it did, read as "deactivated"), and must not authorize a restore
//!   either: the loop waits and asks again, and a [`Verdict::Stop`] from a read that worked ends it.
//!
//! * **The predicate is asked before every attempt, not once.** A `verdict` that is `Go` at the loss and
//!   `Stop` ten minutes later (an operator ran `account deactivate`, or a second row created a
//!   venue-scoped tier conflict) ends the loop on its next pass: no attempt is made after the rows said
//!   stop. The pass is every `probe_every`, so "within one attempt" holds with room to spare.
//! * **Attempts back off, probes do not.** The cheap reachability probe runs every `probe_every` so the
//!   restore happens within one probe of the gateway coming back; only a RESTORE that failed (the port
//!   answers and the venue still refuses the login) waits longer next time, so a gateway that is up but
//!   not yet logged in is not hammered.
//! * **The first attempt is not delayed**: the backoff starts paid.
//! * **A stop request ends the loop at its next pause**, whatever state it is in.

use std::time::Duration;

/// How many probes in a row must fail before a session that was up is called lost.
pub const LOSS_PROBES: u32 = 2;

/// The cadence and the backoff of one supervised link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    /// How often the link is probed (UP) and the rows re-read (DOWN).
    pub probe_every: Duration,
    /// The wait after the first failed restore.
    pub first_wait: Duration,
    /// The ceiling the wait grows to (doubling).
    pub max_wait: Duration,
}

impl Schedule {
    /// Whether the schedule can run: a probe cadence and a first wait above zero (a zero wait is a hot
    /// loop against the venue) and a ceiling that is not below the first wait.
    #[must_use]
    pub fn is_sane(&self) -> bool {
        !self.probe_every.is_zero()
            && !self.first_wait.is_zero()
            && self.first_wait <= self.max_wait
    }
}

/// What the rows say about bringing the account back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The account is still active at its tier: go on.
    Go,
    /// The account is not to be brought back, and why (words for the operator, never a credential).
    Stop(String),
    /// The rows could not be READ (the store was locked or would not open): neither a yes nor a no.
    Retry(String),
}

/// The link a loop supervises. See the module doc.
pub trait Link {
    /// Whether the account may still run. Re-reads the rows each call.
    fn verdict(&self) -> Verdict;
    /// Whether the link is reachable now (one bounded probe); `Err` is why not.
    fn reachable(&self) -> Result<(), String>;
    /// Why the link is gone when the thing behind it said so itself (the bridge's own disclosure), or
    /// `None`. DEFAULT `None`.
    fn reported_lost(&self) -> Option<String> {
        None
    }
    /// The link was seen to drop: take what depended on it down and say so. Called once per loss.
    fn lost(&self, why: &str);
    /// One attempt to bring the link back. `Err` is why it did not come up.
    fn restore(&self) -> Result<(), String>;
    /// The loop is ending because the rows said stop. Called once, last.
    fn gave_up(&self, why: &str);
}

/// A wait the loop can be asked to abandon.
pub trait Pause {
    /// Wait `for_`; `false` = stop was requested, the loop ends.
    fn pause(&self, for_: Duration) -> bool;
}

/// The doubling wait between failed restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    first: Duration,
    max: Duration,
    next: Duration,
}

impl Backoff {
    /// A backoff from `first` up to `max`.
    #[must_use]
    pub fn new(first: Duration, max: Duration) -> Self {
        Self { first, max, next: first }
    }

    /// The wait to serve now; the one after it is twice as long, up to the ceiling.
    pub fn next_wait(&mut self) -> Duration {
        let now = self.next;
        self.next = (self.next * 2).min(self.max);
        now
    }

    /// Back to the first wait (the link came back).
    pub fn reset(&mut self) {
        self.next = self.first;
    }
}

/// Run the loop until the link's rows say stop or the pause says stop. See the module doc.
pub fn supervise(link: &dyn Link, schedule: &Schedule, pause: &dyn Pause) {
    let mut backoff = Backoff::new(schedule.first_wait, schedule.max_wait);
    loop {
        // UP: probe until the link drops (it says so itself, or LOSS_PROBES probes in a row fail).
        let mut failed = 0u32;
        loop {
            if !pause.pause(schedule.probe_every) {
                return;
            }
            if let Some(why) = link.reported_lost() {
                link.lost(&why);
                break;
            }
            match link.reachable() {
                Ok(()) => failed = 0,
                Err(why) => {
                    failed += 1;
                    if failed >= LOSS_PROBES {
                        link.lost(&why);
                        break;
                    }
                }
            }
        }
        // DOWN: ask the rows, then (reachable and the backoff paid) bring it back.
        let mut owed = Duration::ZERO;
        let mut unreadable = Backoff::new(schedule.probe_every, schedule.max_wait);
        let mut unreadable_owed = Duration::ZERO;
        let mut unreadable_reads = 0u32;
        loop {
            if unreadable_owed.is_zero() {
                match link.verdict() {
                    Verdict::Stop(why) => {
                        link.gave_up(&why);
                        return;
                    }
                    Verdict::Retry(why) => {
                        unreadable_reads += 1;
                        unreadable_owed = unreadable.next_wait();
                        // The first and every tenth: a store that stays broken must not flood the log.
                        if unreadable_reads == 1 || unreadable_reads.is_multiple_of(10) {
                            tracing::warn!(
                                "TRADE-LINK: the account rows could not be read ({why}); the venue session is not \
                                 restored without an answer and the rows are asked again in {}s (read failure {unreadable_reads})",
                                unreadable_owed.as_secs()
                            );
                        }
                    }
                    Verdict::Go => {
                        unreadable.reset();
                        unreadable_reads = 0;
                    }
                }
            }
            if owed.is_zero() && unreadable_owed.is_zero() && link.reachable().is_ok() {
                match link.restore() {
                    Ok(()) => {
                        backoff.reset();
                        break;
                    }
                    Err(why) => {
                        owed = backoff.next_wait();
                        tracing::warn!(
                            "TRADE-LINK: the venue session was not re-established ({why}); the next attempt is in {}s",
                            owed.as_secs()
                        );
                    }
                }
            }
            if !pause.pause(schedule.probe_every) {
                return;
            }
            owed = owed.saturating_sub(schedule.probe_every);
            unreadable_owed = unreadable_owed.saturating_sub(schedule.probe_every);
        }
    }
}

#[cfg(test)]
#[path = "reconnect_tests.rs"]
mod reconnect_tests;
