//! **The rolling-family lane's engine**: `ResolveFamily { venue, family, now_ms }` answered by a venue's
//! `FamilySpec` — the engine half of `crates/vike-bridge-core/src/family.rs`'s contract. The venue's
//! grammar check and its resolver are the spec's, in the venue's bridge (polymarket's is
//! `crates/bridges/polymarket/src/catalog/rolling_family.rs`'s `FamilySpecPolymarket`, a `RollingFamily` over the
//! Gamma directory: the recorder's polymarket row without the bridge in the datahub, decision 0164); this
//! file holds no venue fact.
//!
//! A family (`btc-updown-5m`) is a series of 5-minute markets, each a different `condition_id` with
//! different outcome token ids, so a recorder names the FAMILY and a resolver turns it into the live
//! tokens on every tick. The resolver is STATEFUL — its planner caches `slug → token ids` so a re-tick
//! inside one window does zero network I/O, and it clamps `now_ms` so a backwards clock step cannot roll
//! the window set backwards — which is why it lives where a connection's state lives: one resolver per
//! `(connection, family)`, in a slot of the [`ConnectionMemos`], dropped with the connection. The client
//! holds ONE connection per resolver (`vike_gatewayhub_client::resolve::SymbolResolverGateway`), so the
//! planner's lifetime on the wire is the planner's lifetime in-process.
//!
//! # The two rules this lane keeps
//!
//! * **The spec's offline grammar check runs FIRST** (`FamilySpec::admit`; polymarket's is
//!   `vike_catalog::polymarket_family_minutes`): a malformed family is refused in the spec's words, before
//!   a resolver exists, before the connection's slot exists and before any venue is asked.
//! * **A failure is `Err`, never an empty set** (drift register R12). `SymbolResolver`'s contract is
//!   that `Err` means UNKNOWN and the caller leaves its streams exactly as they are; an empty set is a
//!   statement that nothing should be streaming, and it DOES unsubscribe every live book. A directory
//!   outage reaches the peer as `Response::Error` in the resolver's own words. The resolver is kept
//!   across a failure: it plans without committing, so a failed pass leaves it as it was.
//!
//! # A client-named dimension, bounded
//!
//! The family name is the client's, and each distinct one costs the connection a resolver. The slot
//! holds at most [`MAX_FAMILIES_PER_CONNECTION`]; a recorder names a handful. Past the cap the answer
//! is a refusal naming the spec's venue, not a silent eviction (an evicted planner would forget its
//! window cache and clock).
//!
//! # Cancellation: what stops and what cannot
//!
//! A resolve is one rpc (not a fetch) with no context; the resolver's directory reads run to completion
//! (a handful of requests, bounded by the venue's own fetch spec).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use vike_bridge_core::family::FamilySpec;
use vike_data::live::SymbolResolver;

use crate::session::ConnectionMemos;

/// The most distinct families one connection may hold a resolver for.
pub const MAX_FAMILIES_PER_CONNECTION: usize = 16;

/// What one connection holds for the family lane: a resolver per family it has asked about.
#[derive(Default)]
struct FamilySlot {
    resolvers: Mutex<HashMap<String, Box<dyn SymbolResolver>>>,
}

/// The family lane: the engine over a venue's spec. See the module doc.
pub struct FamilyHub {
    spec: Box<dyn FamilySpec>,
}

impl FamilyHub {
    /// The engine over `spec`.
    pub fn new(spec: Box<dyn FamilySpec>) -> Self {
        Self { spec }
    }

    /// The symbols `family` should be streaming at `now_ms`, from this connection's resolver for it.
    /// `Err` is UNKNOWN (the module doc); `Ok(empty)` is a statement.
    pub fn resolve(
        &self,
        scope: &ConnectionMemos,
        family: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        // First, offline: a malformed family is refused with no resolver and no socket.
        self.spec.admit(family)?;
        let slot = scope.slot::<FamilySlot>();
        let mut resolvers = slot.resolvers.lock().unwrap_or_else(PoisonError::into_inner);
        if !resolvers.contains_key(family) {
            if resolvers.len() >= MAX_FAMILIES_PER_CONNECTION {
                return Err(format!(
                    "gatewayhub: this connection already resolves {MAX_FAMILIES_PER_CONNECTION} \
                     {} families; `{family}` is refused (nothing was asked of any venue)",
                    self.spec.venue()
                ));
            }
            resolvers.insert(family.to_string(), self.spec.resolver(family)?);
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
