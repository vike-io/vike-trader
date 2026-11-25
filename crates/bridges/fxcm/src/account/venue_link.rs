//! **An account's ForexConnect session, in the SDK's words**: what the exec thread writes into the account's
//! `vike_bridge_core::venue_link::VenueLink`, the reading behind `trade health`'s `gateway-link` row for an fxcm account,
//! and the gatewayhub's probe of that same cell ([`FxcmSessionProbe`]).
//!
//! The lease stays up through a session that never opened, is reconnecting or was lost, and the engine stays `RUNNING`,
//! so nothing else can say so. The exec thread (`crate::execution`) says a failed login while it retries ([`login_failed`]),
//! then the SDK's own session status (`forex_connect_session_status`, the listener's last `O2GSessionStatus`, [`from_status`]) on
//! every pass of its loop, each through [`say`]. It is a reading: it changes no order's fate, no login and no retry.
//! A CHANGE of that status is also said once, at `info` ([`StatusChanges`]: `session status change`, the old status, the
//! new one and how long the old one held), the evidence an idle limit for this session is measured from.
//! The cell, its registry (by the account's route key) and its "nothing said yet is `Unknown`" rule are bridge-core's;
//! the words, the effects and the freshness bound are this venue's.
//!
//! **A session the SDK gave up on is REBUILT by the gatewayhub** (decision 0164 verdict 7(b), amended 2026-11-21): the exec
//! thread logs in only until its first session opens, so after a FINAL state (`Disconnected`, `Disconnecting`,
//! `SessionLost`: the three that carry [`LOST_EFFECT`]) nothing in this bridge logs in again. [`FxcmSessionProbe`] hands
//! that word to the daemon's one link watch (`crate::adapter::VenueAdapterFxcm`'s `link_probe`), which tears the adapter
//! down and starts it again, at the FX open while the market is closed. Every other state is the SDK's own business (a
//! `Reconnecting` heals itself, a failed first login is the exec thread's to retry), so the rebuild never races it.

use std::time::{Duration, Instant};

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkProbe, VenueLinkState};
use vike_venues::venue_timings;

use crate::reconcile::redial::pause_after;

/// How old the exec thread's last "up" may be and still vouch for the session: the loop passes every 100 ms when idle,
/// and one command plus a confirm pass blocks it for at most `COMMAND_WORST_MS` + `CONFIRM_DRAIN_BUDGET` (about 45 s,
/// `crate::execution` asserts the bound), so a minute without a pass is a thread that is stuck, not busy.
pub(crate) const FRESH: Duration = Duration::from_secs(60);

/// `O2GSessionStatus::Connected` (`IO2GSession.h`).
const CONNECTED: i32 = 3;
/// `O2GSessionStatus::ChartSessionReconnecting`: the chart session carries no order, and the trading session is up.
const CHART_SESSION_RECONNECTING: i32 = 9;

/// What a session the SDK gave up on costs, and the mark of a FINAL state: exactly `Disconnected`, `Disconnecting` and
/// `SessionLost` carry it ([`from_status`]), and it is what [`lost_words`] reads. The exec thread logs in only until its FIRST
/// session opens, so the gatewayhub's supervised rebuild is what logs in again.
const LOST_EFFECT: &str = "every order is refused: the ForexConnect SDK reconnects no more, and the gatewayhub rebuilds \
                           the session (it logs in again, at the FX open while the market is closed)";

/// How often the gatewayhub reads the cell while the session is up, and re-reads the account rows while it is down: the
/// supervisor's cadence (the one IBKR's gateway port is probed at), here a local read of the cell.
pub const PROBE_EVERY: Duration = Duration::from_secs(2);

/// How long a rebuilt session must be seen `Connected` on every probe, without a break, before the rebuild is healthy
/// and the gatewayhub's backoff starts over (the owner's ruling of 2026-11-22, "ok 30s"): a loss before that is one more
/// failed rebuild and climbs the ladder. The exec lane's `probation` in `vike_venues::venue_timings::FXCM`.
pub(crate) const PROBATION: Duration = venue_timings::FXCM.exec().probation();

/// The exec thread's word on `link`: an "up" vouches for the session for [`FRESH`]; a "down" stands until the next word.
pub(crate) fn say(link: &VenueLink, state: VenueLinkState) {
    link.set(state, Some(FRESH));
}

/// A path that is not working, in the bridge's words. No restart count and no wait: the exec thread states none. A
/// session the SDK is still opening or reconnecting is a reopen (`Connecting`); one that FAILED (a final state, a failed
/// login) is a failure (`Error`), said at once.
fn not_working(why: String, effect: &'static str, failed: bool) -> VenueLinkState {
    let down = if failed {
        VenueLinkDown::failed(why, effect, 0, Duration::ZERO)
    } else {
        VenueLinkDown { why, effect, ..VenueLinkDown::default() }
    };
    VenueLinkState::Down(down)
}

/// One status read off the shim, in this cell's words. `None` (a shim that predates `forex_connect_session_status`) and a number
/// the SDK header does not name are not known, never up.
pub fn from_status(status: Option<i32>) -> VenueLinkState {
    let Some(status) = status else { return VenueLinkState::Unknown };
    // A FINAL state (it carries `LOST_EFFECT`) is a session lost; every other is the SDK's own opening or reconnect.
    let down = |name: &str, effect: &'static str| {
        not_working(format!("the ForexConnect session is {name}"), effect, effect == LOST_EFFECT)
    };
    match status {
        CONNECTED | CHART_SESSION_RECONNECTING => VenueLinkState::Up,
        0 => down("Disconnected", LOST_EFFECT),
        1 => down("Connecting", ""),
        2 => down("asking for a trading session this bridge never selects", ""),
        4 => down("Reconnecting (the SDK reconnects by itself)", ""),
        5 => down("Disconnecting", LOST_EFFECT),
        6 => down("SessionLost", LOST_EFFECT),
        7 => down(
            "reconnecting its PRICE session",
            "the trading session may still take orders, but prices read off the session may be stale until FXCM's \
             price session is back",
        ),
        8 => down(
            "connected, but FXCM requires the login's password to be changed",
            "orders may be refused until the password is changed",
        ),
        _ => VenueLinkState::Unknown,
    }
}

/// The SDK's own name of one status read (`O2GSessionStatus`, `IO2GSession.h`), for [`StatusChanges`]' line.
fn status_name(status: Option<i32>) -> &'static str {
    match status {
        None => "not known (the shim exports no forex_connect_session_status)",
        Some(0) => "Disconnected",
        Some(1) => "Connecting",
        Some(2) => "TradingSessionRequested",
        Some(CONNECTED) => "Connected",
        Some(4) => "Reconnecting",
        Some(5) => "Disconnecting",
        Some(6) => "SessionLost",
        Some(7) => "PriceSessionReconnecting",
        Some(8) => "ConnectedWithNeedToChangePassword",
        Some(CHART_SESSION_RECONNECTING) => "ChartSessionReconnecting",
        Some(_) => "unnamed",
    }
}

/// **One `info` line per change of the SDK's session status**, `session status change`: the old status, the new one (and
/// its number, `code`) and how long the old one held (`held_ms`, absent at the session's first reading, whose `old` is
/// `none`). The exec thread reads the status on every pass ([`from_status`]); this says only the passes where it changed.
#[derive(Debug, Default)]
pub(crate) struct StatusChanges {
    /// The status last read, and when it was first read.
    last: Option<(Option<i32>, Instant)>,
}

impl StatusChanges {
    /// The status read at `now`: a line when it is not the one read last.
    pub(crate) fn note(&mut self, status: Option<i32>, now: Instant) {
        let (old, held_ms) = match self.last {
            Some((old, _)) if old == status => return,
            Some((old, since)) => {
                let held = now.saturating_duration_since(since).as_millis();
                (status_name(old), Some(u64::try_from(held).unwrap_or(u64::MAX)))
            }
            None => ("none", None),
        };
        self.last = Some((status, now));
        tracing::info!(
            target: "vike_fxcm",
            venue = "fxcm",
            old,
            new = status_name(status),
            code = status,
            held_ms,
            "session status change"
        );
    }
}

/// A login that failed, in this cell's words: `why` is the exec thread's refusal reason, and a refused credential is
/// never tried again.
pub(crate) fn login_failed(why: &str, credentials_refused: bool) -> VenueLinkState {
    not_working(
        why.to_string(),
        if credentials_refused {
            "every order is refused and nothing logs in again: change the key, then set the account's \
             `account.active` off and on"
        } else {
            "every order is refused while the bridge logs in again by itself, at growing pauses"
        },
        true,
    )
}

/// **What the gatewayhub's probe reads off one state of the cell**: the SDK's words when the state is FINAL (it carries
/// [`LOST_EFFECT`]: the SDK gave up and nothing in this bridge logs in again), else `None`: up, connecting, the SDK's own
/// reconnect, a failed first login the exec thread retries itself, a price session reconnecting, not known.
pub(crate) fn lost_words(state: &VenueLinkState) -> Option<String> {
    match state {
        VenueLinkState::Down(VenueLinkDown { why, effect, .. }) if *effect == LOST_EFFECT => {
            Some(why.clone())
        }
        _ => None,
    }
}

/// **The gatewayhub's probe of one account's ForexConnect session** (`vike_bridge_core::venue_link::VenueLinkProbe`,
/// declared by `crate::adapter::VenueAdapterFxcm`'s `link_probe`): the cell the exec thread writes, found by the account's
/// route key, and never a session of its own.
///
/// * [`VenueLinkProbe::lost`] is the SDK's FINAL state ([`lost_words`]): the daemon tears the dead adapter down at once
///   and rebuilds it. The cell is CLAIMED by each exec thread (`crate::execution`'s `spawn`), so a thread that outlived
///   its teardown cannot write its last `SessionLost` over the rebuilt one.
/// * [`VenueLinkProbe::up`] is a fresh `Connected`: until the rebuilt session said it on every probe for [`PROBATION`]
///   ([`VenueLinkProbe::probation`]), the rebuild is not healthy and its loss climbs the daemon's backoff.
/// * [`VenueLinkProbe::reachable`] is always `Ok`: there is no endpoint to probe beside the session itself (the login
///   finds FXCM's host). While the FX market is closed the daemon holds the rebuild by the venue's calendar
///   (`vike_venues::session::fx_login_closed_until`), not this probe.
/// * A refused rebuild (a start that did not come up) waits as a failed login does: the login ladder's first pause and
///   its ceiling (`crate::reconcile::redial`'s `PAUSES`), each attempt a login that may count towards a lockout.
#[derive(Debug, Clone)]
pub struct FxcmSessionProbe {
    link: VenueLink,
}

impl FxcmSessionProbe {
    /// The probe of the account whose cell is `link` (`vike_bridge_core::venue_link::venue_link` of its route key).
    #[must_use]
    pub fn new(link: VenueLink) -> Self {
        Self { link }
    }
}

impl VenueLinkProbe for FxcmSessionProbe {
    fn reachable(&self) -> Result<(), String> {
        Ok(())
    }

    fn lost(&self) -> Option<String> {
        lost_words(&self.link.state())
    }

    /// The SDK's `Connected`, fresh, off the cell: a rebuild's start returns before its login, so only this word makes
    /// the rebuild healthy (a server that accepts each login and drops it is backed off, never re-logged-in at once).
    fn up(&self) -> Option<bool> {
        Some(self.link.state() == VenueLinkState::Up)
    }

    fn probation(&self) -> Duration {
        PROBATION
    }

    fn probe_every(&self) -> Duration {
        PROBE_EVERY
    }

    fn first_wait(&self) -> Duration {
        pause_after(1)
    }

    fn max_wait(&self) -> Duration {
        pause_after(u32::MAX)
    }
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
