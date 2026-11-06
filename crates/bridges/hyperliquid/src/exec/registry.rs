//! The shared `cloid → coid` registry: exec REGISTERS at submit, the WS pump RESOLVES.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use vike_model::events::TradeId;

use crate::exec::event_mapper::cloid_from_client_order_id;

#[cfg(doc)]
use super::client::HyperliquidExecutionClient;

/// Deterministic `cloid → framework client-order-id` table (see [`cloid_from_client_order_id`]).
///
/// HL's `cloid` is `keccak(coid)` — one-way — so the WS pump cannot invert it; exec REGISTERS each
/// mapping at submit and the pump RESOLVES it when an `orderUpdates`/`userFills` frame carries the
/// cloid back. Shared (`Arc<Mutex<_>>`) between the exec thread (writer) and the pump thread (reader).
#[derive(Clone, Default)]
pub struct CloidRegistry {
    inner: Arc<Mutex<HashMap<String, String>>>,
    /// oids retired by a native `modify` (cancel-replace), each stamped with the instant it was
    /// retired. A native modify keeps the SAME cloid on a NEW oid, so HL may stream a `canceled`
    /// `orderUpdates` for the OLD oid — which, sharing the cloid, would wrongly cancel the live
    /// re-placed order. The pump consults this to DROP that stale cancel. TTL-pruned so an oid whose
    /// stale cancel never arrives can't leak.
    retired: Arc<Mutex<HashMap<u64, std::time::Instant>>>,
    /// cloids seeded by [`Self::register_restored`] — orders a PREVIOUS session placed. They resolve
    /// like any other, but the pump's first-snapshot history floor must not read them as "this
    /// process minted it" ([`Self::is_restored`]).
    restored: Arc<Mutex<std::collections::HashSet<String>>>,
    /// `cloid → oid` of the latest `open`/`resting` `orderUpdates` row the pump saw. A cancel-replace
    /// keeps the cloid on a NEW oid, and when the `modify` reply was ambiguous exec never learned it:
    /// exec reads this ([`Self::take_live_oid`]) when it next needs the oid. Bounded like `inner`
    /// (only registered cloids are recorded, one slot each).
    live_oids: Arc<Mutex<HashMap<String, u64>>>,
    /// The `userFills` trade ids this process's pump has already decoded, whether it emitted the row
    /// or the first-snapshot floor dropped it ([`Self::first_sight_of_fill`]). HL replays recent
    /// account history on every reconnect; this is what lets the pump leave a row it already saw out
    /// of the replay instead of re-emitting it into a core that would only drop it again (and say
    /// so at WARN when the fill is foreign and several accounts claim its symbol).
    seen_fills: Arc<Mutex<std::collections::HashSet<TradeId>>>,
}

/// The most trade ids [`CloidRegistry::first_sight_of_fill`] remembers before it forgets them all.
/// 50k ids is a few MB; a forgotten id only costs a re-emission the engine's own never-pruned
/// `seen_trade_ids` collapses, i.e. exactly the behaviour before the ledger existed.
const SEEN_FILLS_CAP: usize = 50_000;

/// How long a retired oid suppresses a stale `orderUpdates` cancel. A cancel-replace's stale cancel
/// arrives within a beat; 30 s is a generous ceiling that still bounds the map.
pub(super) const RETIRED_TTL: std::time::Duration = std::time::Duration::from_secs(30);

impl CloidRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Derive `coid`'s cloid, record `cloid → coid`, and return the cloid to put on the wire.
    pub fn register(&self, coid: &str) -> String {
        let cloid = cloid_from_client_order_id(coid);
        self.inner.lock().unwrap().insert(cloid.clone(), coid.to_string());
        cloid
    }

    /// [`Self::register`] for an order a PREVIOUS session placed ([`HyperliquidExecutionClient`]'s
    /// `adopt_restored`): `cloid → coid` resolves from now on, and the cloid is remembered as
    /// restored so it is NOT proof of authorship for the first-snapshot floor. ⚠ That floor drops a
    /// pre-spawn row only when the order is foreign, and an adoption can run BEFORE the pump's first
    /// `userFills` snapshot: reading a restored order as ours would fold the previous session's fills
    /// into a fresh `Account`. The fills of the window the process was down are the reconcile pass's.
    pub fn register_restored(&self, coid: &str) -> String {
        let cloid = self.register(coid);
        self.restored.lock().unwrap().insert(cloid.clone());
        cloid
    }

    /// Was `cloid` seeded by [`Self::register_restored`] (a previous session's order)?
    pub fn is_restored(&self, cloid: &str) -> bool {
        self.restored.lock().unwrap().contains(cloid)
    }

    /// The framework coid a venue `cloid` maps to, if exec registered it this session (else `None`,
    /// so a foreign/unknown order's event keeps the venue id and the core drops it as not-ours).
    pub fn resolve(&self, cloid: &str) -> Option<String> {
        self.inner.lock().unwrap().get(cloid).cloned()
    }

    /// Pump side: record that a `userFills` row with trade id `tid` was decoded, and say whether this
    /// is the FIRST time this process has seen it. `false` = a replay of a row already seen.
    /// ⚠ shortcut: the ledger is cleared at [`SEEN_FILLS_CAP`] entries, so a very old row may count
    /// as new again; the engine's own trade-id dedup is still behind it.
    pub fn first_sight_of_fill(&self, tid: &TradeId) -> bool {
        let mut seen = self.seen_fills.lock().unwrap();
        if seen.len() >= SEEN_FILLS_CAP {
            seen.clear();
        }
        seen.insert(tid.clone())
    }

    /// Pump side: `cloid`'s order rests under `oid` now (only a cloid this process registered).
    pub fn learn_live_oid(&self, cloid: &str, oid: u64) {
        if self.inner.lock().unwrap().contains_key(cloid) {
            self.live_oids.lock().unwrap().insert(cloid.to_string(), oid);
        }
    }

    /// Exec side: drop whatever was learned for `cloid` — called just before a modify is sent, so
    /// only rows streamed AFTER the send can be read as its outcome.
    pub fn forget_live_oid(&self, cloid: &str) {
        self.live_oids.lock().unwrap().remove(cloid);
    }

    /// Exec side: CONSUME the oid the pump learned for `cloid`, if any.
    pub fn take_live_oid(&self, cloid: &str) -> Option<u64> {
        self.live_oids.lock().unwrap().remove(cloid)
    }

    /// Mark `oid` retired by a native modify — the pump then suppresses a subsequent stale
    /// `orderUpdates` cancel of it. Opportunistically prunes expired entries.
    pub fn retire_oid(&self, oid: u64) {
        let mut m = self.retired.lock().unwrap();
        let now = std::time::Instant::now();
        m.retain(|_, t| now.duration_since(*t) < RETIRED_TTL);
        m.insert(oid, now);
    }

    /// If `oid` is a retired oid still inside the suppression window, CONSUME it (remove) and return
    /// `true` so the pump drops that one stale cancel; expired/unknown → `false`. Prunes as it goes.
    pub fn take_retired(&self, oid: u64) -> bool {
        let mut m = self.retired.lock().unwrap();
        let now = std::time::Instant::now();
        m.retain(|_, t| now.duration_since(*t) < RETIRED_TTL);
        m.remove(&oid).is_some()
    }
}
