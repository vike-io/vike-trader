//! **The reconnect loop of a venue SESSION the daemon supervises** (decision 0164 verdict 7(b), wave 2 W14):
//! watch a link, notice that it dropped, and bring it back with backoff for exactly as long as the
//! operator's account rows still say the account runs.
//!
//! This module is the policy and nothing else. It names no venue, no socket, no store and no clock: a
//! [`VenueLinkReconnectLink`] answers its questions (is it reachable, may it run, is a login held by the venue's calendar,
//! take it down, bring it back) and a [`VenueLinkReconnectPause`] waits, so the loop is driven by a test with a scripted
//! link and a pause that records instead of sleeping. Which venues use it is their BRIDGE's to declare
//! (`vike_bridge_core::venue_adapter::VenueAdapter::link_probe`, decision 0164 verdict 7(b), amended 2026-11-21): IBKR,
//! whose gateway logs off once a day and whose socket client dies with it for good (`IbkrInbound::StreamDead`), and FXCM,
//! whose exec thread never logs in again once the ForexConnect SDK gave its session up (`SessionLost`).
//!
//! # The loop
//!
//! ```text
//!  UP     every `probe_every`:
//!           the link says it is gone (Link::reported_lost)   --------> Link::lost(why)  -> DOWN
//!           on probation and Link::up() said Some(true) on every pass
//!             for `probation` (a pass that did not starts it over) ---> healthy: backoff reset
//!           [`LOSS_PROBES`] failed probes in a row (reachable?) -----> Link::lost(why)  -> DOWN
//!  DOWN   (entered from probation: the loss is a failed restore, the first attempt waits the backoff)
//!         every `probe_every`:
//!           1. Link::verdict()          the rows are re-read HERE, on every pass
//!                Stop(why)  -> Link::gave_up(why), the loop ENDS (an account that is no longer
//!                              active at its tier is not brought back)
//!                Retry(why) -> the rows could not be READ: nothing is restored this pass and the
//!                              next read waits (1, 2, 4 .. passes, up to `max_wait`); the loop goes on
//!           2. the backoff is paid:
//!                Link::closed() says why (the FX week is closed) -> HELD: no attempt, no failure, the
//!                              backoff starts over, Link::held(why) once per hold
//!                else reachable -> Link::restore()
//!                Ok  -> UP again: Link::up() is None -> backoff reset; else -> on PROBATION
//!                Err -> the next attempt waits twice as long, up to `max_wait` (after a hold of this
//!                       spell, Link::refused(why) first)
//! ```
//!
//! * **One failed probe is not a loss.** A healthy session is torn down only after [`LOSS_PROBES`]
//!   consecutive failures (a localhost connect that fails once is noise; the cost of a false loss is a
//!   lease bounce, a new epoch and an empty id map). A loss the link REPORTS itself (the bridge saying
//!   its stream is dead) needs no second opinion.
//! * **A rows read that failed is neither a yes nor a no.** The store being locked for a moment must not
//!   end the supervision for good (it did, read as "deactivated"), and must not authorize a restore
//!   either: the loop waits and asks again, and a [`VenueLinkReconnectVerdict::Stop`] from a read that worked ends it.
//!
//! * **The predicate is asked before every attempt, not once.** A `verdict` that is `Go` at the loss and
//!   `Stop` ten minutes later (an operator ran `account deactivate`, or a second row created a
//!   venue-scoped tier conflict) ends the loop on its next pass: no attempt is made after the rows said
//!   stop. The pass is every `probe_every`, so "within one attempt" holds with room to spare.
//! * **Attempts back off, probes do not.** The cheap reachability probe runs every `probe_every` so the
//!   restore happens within one probe of the gateway coming back; only a RESTORE that failed (the port
//!   answers and the venue still refuses the login) waits longer next time, so a gateway that is up but
//!   not yet logged in is not hammered. Both waits (a failed restore's, an unreadable store's) are the one reconnect
//!   ladder, `vike_venues::reconnect::ReconnectBackoff`, started over by a restore that
//!   proved healthy (below), a hold and an answer from the rows (`crates/vike-gatewayhub/src/session/reconnect_tests.rs`'s
//!   `the_supervisors_waits_are_pinned_verbatim`).
//! * **The first attempt is not delayed**: the backoff starts paid.
//! * **A restore is healthy only once the session has SAID it is up for the schedule's `probation`**, for a link that
//!   can say it ([`VenueLinkReconnectLink::up`]: FXCM's start returns before its login, so a server that accepts each
//!   login and drops it was rebuilt every 2-4 s for ever). Until then it is on probation, and its loss is one more failed
//!   restore: the next attempt waits the backoff instead of starting over. The span is counted in probe periods from the
//!   first pass that saw `Up`, and a pass that does not see it starts it over, so a server that holds each session for a
//!   probe or two and drops it still climbs. FXCM's is 30 s (the owner's ruling of 2026-11-22, the exec lane's
//!   `probation` of `vike_venues::venue_timings::FXCM`); zero is one sighting. A link that cannot tell (`None`, IBKR) is
//!   healthy at the restore, as before (`crates/vike-gatewayhub/src/session/reconnect_tests.rs`'s
//!   `a_session_that_drops_before_it_says_up_climbs_the_backoff` and `a_session_up_for_less_than_the_probation_climbs_the_backoff`).
//! * **A failure after the open says its own words**: a hold wrote the calendar's words into the state, so a restore that
//!   fails after it is told to the link ([`VenueLinkReconnectLink::refused`]) and replaces them.
//! * **A closed market is not a failure** (the owner's FX-week rule of 2026-11-13: no FX login while the market is
//!   closed). While [`VenueLinkReconnectLink::closed`] answers, no attempt is made and nothing backs off: the hold is said
//!   ONCE ([`VenueLinkReconnectLink::held`]), the rows are still read every pass (a stop still ends it), and the first pass
//!   the market is open makes ONE attempt, after which a failure backs off from the first wait as any other.
//! * **A stop request ends the loop at its next pause**, whatever state it is in.

use std::time::Duration;

use vike_venues::reconnect::ReconnectBackoff;

/// How many probes in a row must fail before a session that was up is called lost.
pub const LOSS_PROBES: u32 = 2;

/// The cadence and the backoff of one supervised link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueLinkReconnectSchedule {
    /// How often the link is probed (UP) and the rows re-read (DOWN).
    pub probe_every: Duration,
    /// The wait after the first failed restore.
    pub first_wait: Duration,
    /// The ceiling the wait grows to (doubling).
    pub max_wait: Duration,
    /// How long a restored session must be SEEN up ([`VenueLinkReconnectLink::up`] on every UP pass, without a break)
    /// before the restore is healthy; zero = one sighting. Unused for a link that cannot say it is up.
    pub probation: Duration,
}

impl VenueLinkReconnectSchedule {
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
pub enum VenueLinkReconnectVerdict {
    /// The account is still active at its tier: go on.
    Go,
    /// The account is not to be brought back, and why (words for the operator, never a credential).
    Stop(String),
    /// The rows could not be READ (the store was locked or would not open): neither a yes nor a no.
    Retry(String),
}

/// The link a loop supervises. See the module doc.
pub trait VenueLinkReconnectLink {
    /// Whether the account may still run. Re-reads the rows each call.
    fn verdict(&self) -> VenueLinkReconnectVerdict;
    /// Whether the link is reachable now (one bounded probe); `Err` is why not.
    fn reachable(&self) -> Result<(), String>;
    /// Why the link is gone when the thing behind it said so itself (the bridge's own disclosure), or
    /// `None`. DEFAULT `None`.
    fn reported_lost(&self) -> Option<String> {
        None
    }
    /// Why no login may be ATTEMPTED now by the venue's own calendar (an FX venue while the FX week is closed:
    /// `vike_venues::session::fx_login_closed_until`'s words), or `None`. Asked before every attempt. DEFAULT `None`.
    fn closed(&self) -> Option<String> {
        None
    }
    /// The loop holds the restore for `why` ([`Self::closed`]'s words): say so. Called once per hold. DEFAULT nothing.
    fn held(&self, _why: &str) {}
    /// A restore failed AFTER a hold of this DOWN spell, for `why` (the restore's error): the held words no longer
    /// stand (the market is open), so say `why` in their place. Called on each such failure. DEFAULT nothing.
    fn refused(&self, _why: &str) {}
    /// The session's own word that it is up now (an SDK's `Connected`), asked after a restore on every UP pass until it
    /// says `Some(true)`: only then is the restore healthy. `None` = the link cannot tell, and a restore that returned
    /// `Ok` is healthy at once. DEFAULT `None`.
    fn up(&self) -> Option<bool> {
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
pub trait VenueLinkReconnectPause {
    /// Wait `for_`; `false` = stop was requested, the loop ends.
    fn pause(&self, for_: Duration) -> bool;
}

/// Run the loop until the link's rows say stop or the pause says stop. See the module doc.
pub fn supervise(
    link: &dyn VenueLinkReconnectLink,
    schedule: &VenueLinkReconnectSchedule,
    pause: &dyn VenueLinkReconnectPause,
) {
    let mut backoff = ReconnectBackoff::new(schedule.first_wait, schedule.max_wait);
    // A restore that returned `Ok` from a link that can say it is up, and has not been up for the probation yet: its loss
    // is a failed restore.
    let mut probation = false;
    // How long a session on probation has been seen up without a break, counted from its first sighting: `None` until a
    // pass sees it up, and again after a pass that does not.
    let mut up_for: Option<Duration> = None;
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
            if probation {
                up_for = (link.up() == Some(true))
                    .then(|| up_for.map_or(Duration::ZERO, |d| d + schedule.probe_every));
                if up_for.is_some_and(|d| d >= schedule.probation) {
                    probation = false;
                    backoff.reset();
                }
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
        // DOWN: ask the rows, then (the backoff paid, the calendar open and reachable) bring it back.
        let mut owed = Duration::ZERO;
        if std::mem::take(&mut probation) {
            owed = backoff.failed();
            tracing::warn!(
                "TRADE-LINK: the re-established venue session was lost before it had been up for {}s; the next attempt is \
                 in {}s",
                schedule.probation.as_secs(),
                owed.as_secs()
            );
        }
        let mut unreadable = ReconnectBackoff::new(schedule.probe_every, schedule.max_wait);
        let mut unreadable_owed = Duration::ZERO;
        let mut unreadable_reads = 0u32;
        // Whether this DOWN spell is held by the venue's calendar right now (said once per hold), and whether it was
        // held at all (a failure after the open then says its own words in place of the held ones).
        let mut holding = false;
        let mut was_held = false;
        loop {
            if unreadable_owed.is_zero() {
                match link.verdict() {
                    VenueLinkReconnectVerdict::Stop(why) => {
                        link.gave_up(&why);
                        return;
                    }
                    VenueLinkReconnectVerdict::Retry(why) => {
                        unreadable_reads += 1;
                        unreadable_owed = unreadable.failed();
                        // The first and every tenth: a store that stays broken must not flood the log.
                        if unreadable_reads == 1 || unreadable_reads.is_multiple_of(10) {
                            tracing::warn!(
                                "TRADE-LINK: the account rows could not be read ({why}); the venue session is not \
                                 restored without an answer and the rows are asked again in {}s (read failure {unreadable_reads})",
                                unreadable_owed.as_secs()
                            );
                        }
                    }
                    VenueLinkReconnectVerdict::Go => {
                        unreadable.reset();
                        unreadable_reads = 0;
                    }
                }
            }
            if owed.is_zero() && unreadable_owed.is_zero() {
                match link.closed() {
                    // The venue's calendar holds every login: not a failure. No attempt, said once, and the first
                    // attempt after the open waits nothing.
                    Some(why) => {
                        if !holding {
                            holding = true;
                            was_held = true;
                            backoff.reset();
                            link.held(&why);
                        }
                    }
                    None => {
                        holding = false;
                        if link.reachable().is_ok() {
                            match link.restore() {
                                Ok(()) => {
                                    // Healthy now unless the link can say it is up: then once it has (module doc).
                                    probation = link.up().is_some();
                                    up_for = None;
                                    if !probation {
                                        backoff.reset();
                                    }
                                    break;
                                }
                                Err(why) => {
                                    if was_held {
                                        link.refused(&why);
                                    }
                                    owed = backoff.failed();
                                    tracing::warn!(
                                        "TRADE-LINK: the venue session was not re-established ({why}); the next attempt is in {}s",
                                        owed.as_secs()
                                    );
                                }
                            }
                        }
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
