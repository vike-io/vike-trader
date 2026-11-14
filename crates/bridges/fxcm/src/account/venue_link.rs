//! **An account's ForexConnect session, in the SDK's words**: what the exec thread writes into the account's
//! `vike_bridge_core::venue_link::VenueLink`, the reading behind `trade health`'s `gateway-link` row for an fxcm account.
//!
//! The lease stays up through a session that never opened, is reconnecting or was lost, and the engine stays `RUNNING`,
//! so nothing else can say so. The exec thread (`crate::execution`) says a failed login while it retries ([`login_failed`]),
//! then the SDK's own session status (`forex_connect_session_status`, the listener's last `O2GSessionStatus`, [`from_status`]) on
//! every pass of its loop, each through [`say`]. It is a reading: it changes no order's fate, no login and no retry.
//! The cell, its registry (by the account's route key) and its "nothing said yet is `Unknown`" rule are bridge-core's;
//! the words, the effects and the freshness bound are this venue's.

use std::time::Duration;

use vike_bridge_core::venue_link::{VenueLink, VenueLinkDown, VenueLinkState};

/// How old the exec thread's last "up" may be and still vouch for the session: the loop passes every 100 ms when idle,
/// and one command plus a confirm pass blocks it for at most `COMMAND_WORST_MS` + `CONFIRM_DRAIN_BUDGET` (about 45 s,
/// `crate::execution` asserts the bound), so a minute without a pass is a thread that is stuck, not busy.
pub(crate) const FRESH: Duration = Duration::from_secs(60);

/// `O2GSessionStatus::Connected` (`IO2GSession.h`).
const CONNECTED: i32 = 3;
/// `O2GSessionStatus::ChartSessionReconnecting`: the chart session carries no order, and the trading session is up.
const CHART_SESSION_RECONNECTING: i32 = 9;

/// What a session the SDK gave up on costs: the exec thread logs in only until its FIRST session opens.
const LOST_EFFECT: &str = "every order is refused, and this bridge does not log in again once its session has opened: \
                           set the account's `account.active` off and on";

/// The exec thread's word on `link`: an "up" vouches for the session for [`FRESH`]; a "down" stands until the next word.
pub(crate) fn say(link: &VenueLink, state: VenueLinkState) {
    link.set(state, Some(FRESH));
}

/// A path that is not working, in the bridge's words. No restart count and no wait: the exec thread states none.
fn not_working(why: String, effect: &'static str) -> VenueLinkState {
    VenueLinkState::Down(VenueLinkDown { why, effect, ..VenueLinkDown::default() })
}

/// One status read off the shim, in this cell's words. `None` (a shim that predates `forex_connect_session_status`) and a number
/// the SDK header does not name are not known, never up.
pub(crate) fn from_status(status: Option<i32>) -> VenueLinkState {
    let Some(status) = status else { return VenueLinkState::Unknown };
    let down = |name: &str, effect: &'static str| {
        not_working(format!("the ForexConnect session is {name}"), effect)
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
    )
}

#[path = "venue_link_tests.rs"]
#[cfg(test)]
mod venue_link_tests;
