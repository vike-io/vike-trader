//! What a CONNECTION holds: a value read once per connection and dropped when the connection closes.
//!
//! In wave 1 this was the whole of the daemon's session state (logins, token refresh and reconnect were a
//! later wave's; wave 2 adds one supervised reconnect, [`reconnect`], whose only user is IBKR). What THIS
//! file holds exists for the credentialed history lane: the OANDA practice
//! token is read from the store when a request's first fetch needs it, kept for the rest of that
//! request, and dropped with it. A client's history SOURCE is built per request and holds ONE
//! connection for its life (`crates/vike-gatewayhub-client/src/history.rs`'s `RemoteKlines`), so a
//! per-CONNECTION memo is a per-REQUEST memo, which is what
//! `docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md` rules
//! (verdict 2): a key stored, rotated or removed between two requests is what the next one sees, and a
//! chunked ingest of thousands of day fetches reads the store once.
//!
//! # The two types
//!
//! * [`ConnectionScope`] is created by the server for each authenticated connection and handed to every
//!   [`crate::rest::RestLane`] call that connection makes. It is a typed bag: a lane asks for its own
//!   slot (`slot::<T>()`, created `Default` on first use) and keeps what it wants per connection in it.
//!   It is dropped when the connection ends, and everything in it with it.
//! * [`ConnectionMemo`] is the read-once cell a slot holds. The READ is injected as a closure, so this
//!   module names no store, no bridge and no credential: [`ConnectionMemo::get_or_read`] runs it at
//!   most once and keeps the ANSWER, an error included, so an unreadable store is reported once per
//!   request rather than once per day chunk.
//!
//! # The credential discipline
//!
//! ⚠ **A credential VALUE never reaches `Debug`, `Display` or a log line**
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md` verdict 6, and
//! `crates/vike-secrets/CLAUDE.md`, which says reads never log). Neither type here prints what it holds: a
//! memo's `Debug` says `unread`, `ok` or `err` and nothing else, a scope's says how many slots it has,
//! and [`Redacted`] is the wrapper a lane puts a value in so that holding it in a struct that derives
//! `Debug` cannot leak it. Its accessor is [`Redacted::expose`], named so the call site reads as the
//! deliberate end of redaction (the `SecretMap::into_map` precedent). The ERROR half of a memo should
//! carry no payload either, `vike_oanda::HistoryTokenError`'s way: the type makes "it cannot hold a
//! token" true rather than a matter of care.

pub mod reconnect;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// A value that must not be printed: `Debug` shows `Redacted(***)`, there is no `Display`, and the only
/// way to the value is [`Redacted::expose`].
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Wrap a value.
    pub fn new(value: T) -> Self {
        Redacted(value)
    }

    /// The value. Named so the call site reads as the deliberate end of redaction: the value goes to
    /// the one place that needs it (an `Authorization` header) and nowhere else.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> Clone for Redacted<T> {
    fn clone(&self) -> Self {
        Redacted(self.0.clone())
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Redacted(***)")
    }
}

/// A cell read at most ONCE per connection: the answer of its first [`ConnectionMemo::get_or_read`],
/// kept for the life of the connection it lives in. The error is kept too.
///
/// `Debug` never prints the value or the error (the module doc's discipline).
pub struct ConnectionMemo<T, E> {
    cell: OnceLock<Result<T, E>>,
}

impl<T, E> Default for ConnectionMemo<T, E> {
    fn default() -> Self {
        Self { cell: OnceLock::new() }
    }
}

impl<T, E> ConnectionMemo<T, E> {
    /// An unread memo.
    pub fn new() -> Self {
        Self::default()
    }

    /// The memoised answer, running `read` first if nothing has been read yet. `read` runs AT MOST
    /// ONCE for the life of the memo, however many threads ask: a second caller waits for the first's
    /// answer rather than reading again.
    pub fn get_or_read(&self, read: impl FnOnce() -> Result<T, E>) -> &Result<T, E> {
        self.cell.get_or_init(read)
    }

    /// Whether a read has happened (and its answer is kept).
    pub fn is_read(&self) -> bool {
        self.cell.get().is_some()
    }
}

impl<T, E> fmt::Debug for ConnectionMemo<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = match self.cell.get() {
            None => "unread",
            Some(Ok(_)) => "ok",
            Some(Err(_)) => "err",
        };
        write!(f, "ConnectionMemo({state})")
    }
}

/// The per-connection typed bag. See the module doc.
#[derive(Default)]
pub struct ConnectionScope {
    slots: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl ConnectionScope {
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

impl fmt::Debug for ConnectionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConnectionScope({} slots)", self.len())
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;
