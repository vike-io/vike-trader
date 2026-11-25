//! **A value read at most once for the life of the thing that holds it, and never printed**: the
//! [`ConnectionMemo`] cell and the [`Redacted`] wrapper a credentialed history lane keeps a read
//! credential in. Moved here from the gatewayhub's `session` module with the lanes that use them
//! (`crate::credentialed_klines`): the gatewayhub keeps the per-connection bag, and each venue's
//! bridge keeps the memo its connection's source reads through.
//!
//! The READ is injected as a closure, so this module names no store, no bridge and no credential:
//! [`ConnectionMemo::get_or_read`] runs it at most once and keeps the ANSWER, an error included, so an
//! unreadable store is reported once per request rather than once per day chunk.
//! `docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md`
//! (verdict 2) is why the unit is a request: a key stored, rotated or removed between two requests is
//! what the next one sees, and a chunked ingest of thousands of day fetches reads the store once.
//!
//! # The credential discipline
//!
//! ⚠ **A credential VALUE never reaches `Debug`, `Display` or a log line**
//! (`docs/decisions/0164-one-daemon-owns-every-venue-connection-vike-gatewayhub.md` verdict 6, and
//! `crates/vike-secrets/CLAUDE.md`, which says reads never log). Neither type here prints what it holds: a
//! memo's `Debug` says `unread`, `ok` or `err` and nothing else, and [`Redacted`] is the wrapper a lane
//! puts a value in so that holding it in a struct that derives `Debug` cannot leak it. Its accessor is
//! [`Redacted::expose`], named so the call site reads as the deliberate end of redaction (the
//! `CredentialMap::into_map` precedent). The ERROR half of a memo should carry no payload either,
//! `vike_oanda::HistoryTokenError`'s way: the type makes "it cannot hold a token" true rather than a
//! matter of care.

use std::fmt;
use std::sync::OnceLock;

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

#[cfg(test)]
#[path = "connection_memo_tests.rs"]
mod connection_memo_tests;
