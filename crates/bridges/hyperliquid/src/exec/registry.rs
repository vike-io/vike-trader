//! The shared `cloid → coid` registry: exec REGISTERS at submit, the WS pump RESOLVES.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
}

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
