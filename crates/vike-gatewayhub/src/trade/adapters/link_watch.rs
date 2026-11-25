//! **The daemon's one watch over a venue session it SUPERVISES** (decision 0164 verdict 7(b), wave 2 W14 and W14b;
//! amended 2026-11-21: a declared adapter verb).
//!
//! A bridge whose session cannot heal itself DECLARES a [`VenueLinkProbe`] (`VenueAdapter::link_probe`: IBKR's socket
//! gateway, which logs off once a day and whose adapter has no reconnect, `vike_ibkr::gateway_watch`; FXCM's ForexConnect
//! session, which its exec thread never logs into again once the SDK gave it up, `vike_fxcm::account::venue_link`).
//! [`VenueLinkWatch`] is the [`VenueLinkWatchSource`] over it: `reachable`, `lost` and `up` are the bridge's probe, `tier_now`
//! the fresh read of the account rows the supervision asks before every attempt, `login_closed` the venue's own calendar
//! (`vike_venues::session::fx_login_closed_until`, on the lane's FX clock: no FX login while the FX week is closed), and
//! the schedule the bridge's cadence. The loop is `crate::session::reconnect`'s and the session half is `AccountSession`'s
//! (`crate::trade::session`): this file names no venue, no socket, no bridge, no client id and no credential.

use std::sync::Arc;

use vike_bridge_core::venue_link::VenueLinkProbe;
use vike_model::{AccountTier, ArmingBlock, Clock};

use crate::session::reconnect::VenueLinkReconnectSchedule;
use crate::trade::lane::{NOT_READY_WORDS, VenueLinkWatchSource};

/// The rows' answer RIGHT NOW: a fresh read of the account table on every call.
pub type TierNow = Box<dyn Fn() -> Result<AccountTier, ArmingBlock> + Send + Sync>;

/// The [`VenueLinkWatchSource`] of one account: its bridge's probe and a fresh read of its rows.
pub struct VenueLinkWatch {
    probe: Box<dyn VenueLinkProbe>,
    tier_now: TierNow,
    schedule: VenueLinkReconnectSchedule,
    /// [`VenueLinkWatchSource::not_ready_words`].
    not_ready: &'static str,
    /// The venue whose calendar holds a rebuild's login, and the clock it is read on ([`Self::with_calendar`]); `None`
    /// holds nothing.
    calendar: Option<(String, Arc<dyn Clock + Send + Sync>)>,
}

impl VenueLinkWatch {
    /// A watch over `probe`, asking `tier_now` for the rows, on the bridge's own cadence
    /// ([`Self::production_schedule`]), saying the bridge's words while a start waits behind it.
    #[must_use]
    pub fn new(probe: Box<dyn VenueLinkProbe>, tier_now: TierNow) -> Self {
        let schedule = Self::production_schedule(probe.as_ref());
        let not_ready = probe.not_ready_words().unwrap_or(NOT_READY_WORDS);
        Self { probe, tier_now, schedule, not_ready, calendar: None }
    }

    /// The same watch on another schedule (a test's, in milliseconds).
    #[must_use]
    pub fn with_schedule(mut self, schedule: VenueLinkReconnectSchedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The same watch holding a rebuild to `venue`'s calendar on `clock` (the lane's FX clock: the wall clock in
    /// production, a fixed instant in a test). `None` holds nothing, as a lane with no clock holds no start.
    #[must_use]
    pub fn with_calendar(
        mut self,
        venue: &str,
        clock: Option<Arc<dyn Clock + Send + Sync>>,
    ) -> Self {
        self.calendar = clock.map(|clock| (venue.to_string(), clock));
        self
    }

    /// The production schedule of `probe`: probe every `probe_every`, the first failed restore waits `first_wait`,
    /// growing to `max_wait`, a restore healthy once seen up for `probation`, each the bridge's.
    #[must_use]
    pub fn production_schedule(probe: &dyn VenueLinkProbe) -> VenueLinkReconnectSchedule {
        VenueLinkReconnectSchedule {
            probe_every: probe.probe_every(),
            first_wait: probe.first_wait(),
            max_wait: probe.max_wait(),
            probation: probe.probation(),
        }
    }
}

impl std::fmt::Debug for VenueLinkWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenueLinkWatch")
            .field("probe", &self.probe)
            .field("schedule", &self.schedule)
            .field("calendar", &self.calendar.as_ref().map(|(venue, _)| venue))
            .finish_non_exhaustive()
    }
}

impl VenueLinkWatchSource for VenueLinkWatch {
    fn reachable(&self) -> Result<(), String> {
        self.probe.reachable()
    }

    fn lost(&self) -> Option<String> {
        self.probe.lost()
    }

    fn up(&self) -> Option<bool> {
        self.probe.up()
    }

    fn login_closed(&self) -> Option<String> {
        let (venue, clock) = self.calendar.as_ref()?;
        vike_venues::session::fx_login_closed_until(venue, clock.now_ms())
            .map(vike_venues::session::fx_login_closed_words)
    }

    fn tier_now(&self) -> Result<AccountTier, ArmingBlock> {
        (self.tier_now)()
    }

    fn schedule(&self) -> VenueLinkReconnectSchedule {
        self.schedule
    }

    fn not_ready_words(&self) -> String {
        self.not_ready.to_string()
    }
}
