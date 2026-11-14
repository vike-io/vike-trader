//! **The daemon's one watch over a venue session it SUPERVISES** (decision 0164 verdict 7(b), wave 2 W14 and W14b).
//!
//! A bridge whose session cannot heal itself hands the daemon a [`VenueLinkProbe`] (IBKR's socket gateway, which logs
//! off once a day and whose adapter has no reconnect: `vike_ibkr::gateway_watch`, whose module doc says what the probe
//! can and cannot see). [`VenueLinkWatch`] is the [`VenueLinkWatchSource`] over it: `reachable` is the bridge's probe,
//! `tier_now` the fresh read of the account rows the supervision asks before every attempt, and the schedule the
//! bridge's cadence. The loop is `crate::session::reconnect`'s and the session half is `AccountSession`'s
//! (`crate::trade::session`): this file names no venue, no socket, no bridge, no client id and no credential.

use vike_bridge_core::venue_link::VenueLinkProbe;
use vike_model::{AccountTier, ArmingBlock};

use crate::session::reconnect::VenueLinkReconnectSchedule;
use crate::trade::lane::VenueLinkWatchSource;

/// The rows' answer RIGHT NOW: a fresh read of the account table on every call.
pub type TierNow = Box<dyn Fn() -> Result<AccountTier, ArmingBlock> + Send + Sync>;

/// The [`VenueLinkWatchSource`] of one account: its bridge's probe and a fresh read of its rows.
pub struct VenueLinkWatch {
    probe: Box<dyn VenueLinkProbe>,
    tier_now: TierNow,
    schedule: VenueLinkReconnectSchedule,
}

impl VenueLinkWatch {
    /// A watch over `probe`, asking `tier_now` for the rows, on the bridge's own cadence
    /// ([`Self::production_schedule`]).
    #[must_use]
    pub fn new(probe: Box<dyn VenueLinkProbe>, tier_now: TierNow) -> Self {
        let schedule = Self::production_schedule(probe.as_ref());
        Self { probe, tier_now, schedule }
    }

    /// The same watch on another schedule (a test's, in milliseconds).
    #[must_use]
    pub fn with_schedule(mut self, schedule: VenueLinkReconnectSchedule) -> Self {
        self.schedule = schedule;
        self
    }

    /// The production schedule of `probe`: probe every `probe_every`, the first failed restore waits `first_wait`,
    /// growing to `max_wait`, each the bridge's.
    #[must_use]
    pub fn production_schedule(probe: &dyn VenueLinkProbe) -> VenueLinkReconnectSchedule {
        VenueLinkReconnectSchedule {
            probe_every: probe.probe_every(),
            first_wait: probe.first_wait(),
            max_wait: probe.max_wait(),
        }
    }
}

impl std::fmt::Debug for VenueLinkWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VenueLinkWatch")
            .field("probe", &self.probe)
            .field("schedule", &self.schedule)
            .finish_non_exhaustive()
    }
}

impl VenueLinkWatchSource for VenueLinkWatch {
    fn reachable(&self) -> Result<(), String> {
        self.probe.reachable()
    }

    fn tier_now(&self) -> Result<AccountTier, ArmingBlock> {
        (self.tier_now)()
    }

    fn schedule(&self) -> VenueLinkReconnectSchedule {
        self.schedule
    }
}
