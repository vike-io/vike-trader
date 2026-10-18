//! Link-loss disclosure: what the lease tells the core when the trade link goes down and comes back.
//!
//! The core is transport-agnostic (decision 0091), so a dead link is never an `Err` a strategy sees; it
//! is a FEED STATUS. When the lease is lost the strategies mounted on the account must see
//! `FeedStatus::Disconnected` (they pull their resting quotes), the link dead-man's trade-link latch
//! must open, and on re-establishment `Live` closes it. `Stale` stays inert (the dead-man ignores it).
//! Resting orders are NOT cancelled by the client (owner ruling D6): they sit at the venue and reconcile
//! picks them up after recovery; if the outage outlasts the dead-man's grace on an armed venue, the
//! core trips `CancelAllAndHalt` and those cancels queue NEVER-SENT and flush at reconnect
//! (`crate::trade::ledger`).
//!
//! The seam is [`LinkSignal`]: the link thread calls `link_lost` once per loss and `link_restored` once
//! per recovery, AFTER the held frames have been flushed. [`CoreLinkSignal`] is the production impl: an
//! `Ingest::ExecLink` for the account (the stuck-order watchdog defers while it is down and counts again
//! from link-up) and a `StreamStatus` on the reserved `TRADE_LINK_STREAM` word per mounted key (the
//! strategies' `Disconnected`/`Live` and the dead-man's trade-link latch).

use std::sync::{Arc, Mutex, PoisonError};

use vike_exec::{EventSender, ExecLinkUpdate, Ingest, StreamStatusUpdate};
use vike_model::FeedStatus;
use vike_venues::link_deadman::TRADE_LINK_STREAM;

use super::shared::TradeAccount;

/// What the lease link announces about itself.
pub trait LinkSignal: Send + Sync {
    /// The link was lost (`why` is for the log, never a credential). Called once per loss, from the link
    /// thread, as soon as the loss is known.
    fn link_lost(&self, why: &str);
    /// The link was re-established and the held frames flushed. Called once per recovery.
    fn link_restored(&self);
    /// The gatewayhub's epoch changed across a reconnect (it restarted): every in-memory state it held
    /// for this account is gone. `old` is `None` on the first connection (not called then).
    /// DEFAULT no-op; the tradehub's re-teaching and immediate reconcile (W13) hang here.
    fn epoch_changed(&self, old: u64, new: u64) {
        let _ = (old, new);
    }
}

/// A signal that says nothing: for a caller with no mounted strategies (a tool, a smoke).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSignal;

impl LinkSignal for NoSignal {
    fn link_lost(&self, _why: &str) {}
    fn link_restored(&self) {}
}

/// The `(venue, symbol)` keys the account's strategies are mounted on, shared between the composition
/// root (which adds a key as it mounts one) and the signal (which discloses to each).
#[derive(Debug, Clone, Default)]
pub struct MountedKeys(Arc<Mutex<Vec<(String, String)>>>);

impl MountedKeys {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a mounted key (idempotent).
    pub fn add(&self, venue: &str, symbol: &str) {
        let mut keys = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (venue.to_string(), symbol.to_string());
        if !keys.contains(&key) {
            keys.push(key);
        }
    }

    /// The keys, in the order they were added.
    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

/// The production [`LinkSignal`]: one `Ingest::ExecLink` for the account and a `StreamStatus` on the core's
/// lane for each mounted key, stamped with the reserved trade-link word
/// (`vike_venues::link_deadman::TRADE_LINK_STREAM`). Every `Disconnected` is closed by a `Live` with the SAME
/// word, or by process exit.
#[derive(Clone)]
pub struct CoreLinkSignal {
    events: EventSender,
    route_key: String,
    venue: String,
    keys: MountedKeys,
}

impl std::fmt::Debug for CoreLinkSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreLinkSignal")
            .field("route_key", &self.route_key)
            .field("keys", &self.keys)
            .finish()
    }
}

impl CoreLinkSignal {
    /// A signal over the core's lane for `account` (its route key and venue name the `ExecLink`);
    /// `keys` is the account's mounted `(venue, symbol)` set, extended as strategies mount.
    pub fn new(events: &EventSender, account: &TradeAccount, keys: MountedKeys) -> Self {
        Self {
            events: events.clone(),
            route_key: account.route_key.clone(),
            venue: account.venue.clone(),
            keys,
        }
    }

    fn exec_link(&self, up: bool) {
        let update = ExecLinkUpdate {
            route_key: self.route_key.as_str().into(),
            venue: self.venue.as_str().into(),
            up,
            at_ts_ms: vike_model::now_ms(),
        };
        // `CoreGone` = the core has exited = shutting down; there is nobody left to tell.
        let _ = self.events.exec_link(update);
    }

    fn disclose(&self, status: FeedStatus) {
        for (venue, symbol) in self.keys.snapshot() {
            let update =
                StreamStatusUpdate { venue, symbol, stream: TRADE_LINK_STREAM.to_string(), status };
            // `CoreGone` = the core has exited = shutting down; there is nobody left to tell.
            let _ = self.events.ingest.blocking_send(Ingest::StreamStatus(Box::new(update)));
        }
    }
}

impl LinkSignal for CoreLinkSignal {
    fn link_lost(&self, why: &str) {
        tracing::warn!(why, "gatewayhub trade link lost: ExecLink down and Disconnected disclosed");
        self.exec_link(false);
        self.disclose(FeedStatus::Disconnected);
    }

    fn link_restored(&self) {
        tracing::info!("gatewayhub trade link restored: ExecLink up and Live disclosed");
        self.exec_link(true);
        self.disclose(FeedStatus::Live);
    }
}
