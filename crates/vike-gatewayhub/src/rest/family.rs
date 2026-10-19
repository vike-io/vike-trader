//! **The polymarket rolling-family resolver**: `ResolveFamily { venue, family, now_ms }` answered by
//! `vike_polymarket::rolling_family::RollingFamily`, the recorder's polymarket row without the
//! bridge in the datahub (decision 0164).
//!
//! A family (`btc-updown-5m`) is a series of 5-minute markets, each a different `condition_id` with
//! different outcome token ids, so a recorder names the FAMILY and a resolver turns it into the live
//! tokens on every tick. The resolver is STATEFUL — its planner caches `slug → token ids` so a re-tick
//! inside one window does zero network I/O, and it clamps `now_ms` so a backwards clock step cannot roll
//! the window set backwards — which is why it lives where a connection's state lives: one
//! `RollingFamily` per `(connection, family)`, in a slot of the [`ConnectionScope`], dropped with the
//! connection. The client holds ONE connection per resolver
//! (`vike_gatewayhub_client::resolve::RemoteRollingFamily`), so the planner's lifetime on the wire is
//! the planner's lifetime in-process.
//!
//! # The two rules this lane keeps
//!
//! * **The offline grammar check runs FIRST** (`vike_catalog::polymarket_family_minutes`): a family
//!   with no trailing `-<N>m` is refused with the words the bridge's own `window_spec_for` returns,
//!   before a planner exists and before any venue is asked. The client runs the same check at its own
//!   startup, so a malformed row is refused with the gatewayhub down.
//! * **A failure is `Err`, never an empty set** (drift register R12). `SymbolResolver`'s contract is
//!   that `Err` means UNKNOWN and the caller leaves its streams exactly as they are; an empty set is a
//!   statement that nothing should be streaming, and it DOES unsubscribe every live book. A directory
//!   outage reaches the peer as `Response::Error` in the resolver's own words. The planner is kept
//!   across a failure: it plans without committing, so a failed pass leaves it as it was.
//!
//! # A client-named dimension, bounded
//!
//! The family name is the client's, and each distinct one costs the connection a planner. The slot
//! holds at most [`MAX_FAMILIES_PER_CONNECTION`]; a recorder names a handful. Past the cap the answer
//! is a refusal, not a silent eviction (an evicted planner would forget its window cache and clock).
//!
//! # Cancellation: what stops and what cannot
//!
//! A resolve is one rpc (not a fetch) with no context; the planner's Gamma reads run to completion
//! (a handful of requests, bounded by `FetchSpec`).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_data::live::SymbolResolver;
use vike_polymarket::rolling_family::RollingFamily;

use crate::session::ConnectionScope;

/// The most distinct families one connection may hold a planner for.
pub const MAX_FAMILIES_PER_CONNECTION: usize = 16;

/// Builds the resolver for a family that passed the grammar check: `RollingFamily::new` over the real
/// Gamma directory in production, a scripted source in a suite.
pub type ResolverFactory =
    Box<dyn Fn(&str) -> Result<Box<dyn SymbolResolver>, String> + Send + Sync>;

/// What one connection holds for the family lane: a planner per family it has asked about.
#[derive(Default)]
struct FamilySlot {
    resolvers: Mutex<HashMap<String, Box<dyn SymbolResolver>>>,
}

/// The family lane. See the module doc.
pub struct FamilyHub {
    make: ResolverFactory,
}

impl FamilyHub {
    /// The production lane: `RollingFamily::new` over the real Gamma directory.
    pub fn gamma() -> Self {
        Self::with_factory(Box::new(|family: &str| {
            Ok(Box::new(RollingFamily::new(family)?) as Box<dyn SymbolResolver>)
        }))
    }

    /// A lane whose resolvers `make` builds — the seam a suite scripts a Gamma source through
    /// (`RollingFamily::with_source`).
    pub fn with_factory(make: ResolverFactory) -> Self {
        Self { make }
    }

    /// The symbols `family` should be streaming at `now_ms`, from this connection's planner for it.
    /// `Err` is UNKNOWN (the module doc); `Ok(empty)` is a statement.
    pub fn resolve(
        &self,
        scope: &ConnectionScope,
        family: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        // First, offline: a malformed family is refused with no planner and no socket.
        vike_catalog::polymarket_family_minutes(family)?;
        let slot = scope.slot::<FamilySlot>();
        let mut resolvers = slot.resolvers.lock().unwrap_or_else(PoisonError::into_inner);
        if !resolvers.contains_key(family) {
            if resolvers.len() >= MAX_FAMILIES_PER_CONNECTION {
                return Err(format!(
                    "gatewayhub: this connection already resolves {MAX_FAMILIES_PER_CONNECTION} \
                     polymarket families; `{family}` is refused (nothing was asked of any venue)"
                ));
            }
            resolvers.insert(family.to_string(), (self.make)(family)?);
        }
        let resolver = resolvers
            .get_mut(family)
            .ok_or_else(|| format!("gatewayhub: no planner for `{family}` after building one"))?;
        resolver.desired(now_ms).map(|set| set.into_iter().collect())
    }
}

impl std::fmt::Debug for FamilyHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FamilyHub").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "family_tests.rs"]
mod family_tests;
