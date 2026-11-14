//! **No FX venue logs in while the FX market is closed** (owner ruling 2026-11-13, "dont try to login on
//! weekends, only on midnight"): no login, no reconnect and no login retry of a venue `vike_venues::session::session_for`
//! puts on the FX week (fxcm, dukascopy, oanda, ctrader, ig) between Friday 22:00 and Sunday 21:00 UTC; the first
//! attempt is at the open. The calendar is `vike_venues::session::fx_login_closed_until`'s; this module adds the one
//! thing that crate may not hold, the process's clock.
//!
//! ⚠ **OFF until a process arms it.** The gatewayhub, the one process that opens venue sessions (decision 0164), arms
//! it at its start with the wall clock (`crates/vike-gatewayhub/src/gatewayhub_cli/wire.rs`'s `serve_wire`). A test,
//! a tool and a smoke that never arm it log in as they always did, whatever the day, so no test depends on the day it
//! runs; a test that holds the gate arms it with a fixed clock ([`arm`]).
//!
//! A bridge asks it two ways:
//! - **a login** asks [`closed_words`] first and is refused in those words, sending nothing;
//! - **a retry ladder** waits [`held_pause`] instead of its own pause: the same pause on an open market, and ONE wait to
//!   the open when its next attempt would fall while the market is closed, so it neither spins nor logs in.
//!
//! A session already logged in when the market closes is left alone: nothing here logs one out.

use std::sync::{PoisonError, RwLock};
use std::time::Duration;

use vike_venues::session::{fx_login_closed_until, fx_login_closed_words};

/// The clock the gate reads, epoch ms; `None` = the gate is off.
static CLOCK: RwLock<Option<fn() -> i64>> = RwLock::new(None);

/// Arm the gate for this process on `clock` (epoch ms): the gatewayhub passes the wall clock, a test a fixed instant. A
/// later call replaces the clock.
pub fn arm(clock: fn() -> i64) {
    *CLOCK.write().unwrap_or_else(PoisonError::into_inner) = Some(clock);
}

/// The gate's clock now, or `None` while it is off.
fn now() -> Option<i64> {
    let clock = *CLOCK.read().unwrap_or_else(PoisonError::into_inner);
    clock.map(|clock| clock())
}

/// The next FX open when `venue` may not log in now; `None` when it may (always, while the gate is off).
pub fn closed_until(venue: &str) -> Option<i64> {
    fx_login_closed_until(venue, now()?)
}

/// [`closed_until`] in the words a refused login is told in (`FX market closed: login at ...`).
pub fn closed_words(venue: &str) -> Option<String> {
    closed_until(venue).map(fx_login_closed_words)
}

/// What a retry ladder of `venue` waits instead of its own `pause`: [`held_pause_at`] on the gate's clock, `pause`
/// itself while the gate is off.
pub fn held_pause(venue: &str, pause: Duration) -> Duration {
    now().map_or(pause, |now_ms| held_pause_at(venue, pause, now_ms))
}

/// `pause`, or the wait from `now_ms` to the FX open when the attempt it leads to (at `now_ms + pause`) would fall
/// while the market is closed: one wait to the open, never a pause that lands a login inside the closed window. Pure.
pub fn held_pause_at(venue: &str, pause: Duration, now_ms: i64) -> Duration {
    let due = now_ms.saturating_add(i64::try_from(pause.as_millis()).unwrap_or(i64::MAX));
    fx_login_closed_until(venue, due).map_or(pause, |open| {
        Duration::from_millis(u64::try_from(open.saturating_sub(now_ms)).unwrap_or(0))
    })
}

#[path = "fx_login_tests.rs"]
#[cfg(test)]
mod fx_login_tests;
