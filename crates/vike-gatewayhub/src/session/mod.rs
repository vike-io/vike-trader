//! What a CONNECTION holds: a value read once per connection and dropped when the connection closes.
//!
//! In wave 1 this was the whole of the daemon's session state (logins, token refresh and reconnect were a
//! later wave's; wave 2 adds one supervised reconnect, [`reconnect`], whose only user is IBKR). What THIS
//! file holds exists for the credentialed history lane: a venue's credential is read from the store
//! when a request's first fetch needs it, kept for the rest of that request, and dropped with it. A
//! client's history SOURCE is built per request and holds ONE connection for its life
//! (`crates/vike-gatewayhub-client/src/history.rs`'s `KlineSourceGateway`), so a per-CONNECTION memo is a
//! per-REQUEST memo, which is what
//! `docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md` rules
//! (verdict 2): a key stored, rotated or removed between two requests is what the next one sees, and a
//! chunked ingest of thousands of day fetches reads the store once.
//!
//! # The one type
//!
//! [`ConnectionMemos`] is created by the server for each authenticated connection and handed to every
//! [`crate::rest::RestLane`] call that connection makes. It is a typed bag: a lane asks for its own slot
//! (`slot::<T>()`, created `Default` on first use) and keeps what it wants per connection in it. It is
//! dropped when the connection ends, and everything in it with it. The read-once cell and the
//! never-printed wrapper a credential lives in inside a slot are
//! `crates/vike-bridge-core/src/connection_memo.rs`'s `ConnectionMemo` and `Redacted`: the venue's bridge
//! reads its credential through them (`crate::rest::credentialed_klines`).
//!
//! ⚠ **A credential VALUE never reaches `Debug`, `Display` or a log line**
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md` verdict 6): a scope's
//! `Debug` says how many slots it has and nothing of what they hold.

pub mod reconnect;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

/// The per-connection typed bag. See the module doc.
#[derive(Default)]
pub struct ConnectionMemos {
    slots: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ConnectionMemos {
    /// An empty scope.
    pub fn new() -> Self {
        Self::default()
    }

    /// This connection's slot of type `T`, created with `T::default()` the first time it is asked for.
    /// Two calls on one scope return the same slot; two scopes never share one.
    pub fn slot<T: Default + Send + Sync + 'static>(&self) -> Arc<T> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = slots
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Arc::new(T::default()) as Arc<dyn Any + Send + Sync>);
        Arc::clone(entry)
            .downcast::<T>()
            .unwrap_or_else(|_| unreachable!("a slot is keyed by the TypeId of the type it holds"))
    }

    /// How many slots this connection holds.
    pub fn len(&self) -> usize {
        self.slots.lock().unwrap_or_else(PoisonError::into_inner).len()
    }

    /// Whether no slot has been asked for.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl fmt::Debug for ConnectionMemos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConnectionMemos({} slots)", self.len())
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
