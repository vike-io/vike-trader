//! **`CredentialedKlinesLane`: the ONE credentialed kline lane** — the engine half of
//! `crates/vike-bridge-core/src/credentialed_klines.rs`'s contract. Every venue whose history needs a
//! credential this daemon holds (OANDA's practice token, IG's demo account and its login, cTrader's demo
//! grant and its session) is the same lane over that venue's `CredentialedKlinesSpec`, which lives in
//! the venue's bridge; this file holds no venue fact.
//!
//! # What the engine does
//!
//! `FetchCredentialed { Klines }` for the lane's venue, in this order:
//!
//! 1. **The cancel flag**, before anything is built or read: a fetch cancelled while it waited never
//!    reaches the venue, and no connection slot is made.
//! 2. **The spec's admission** (`CredentialedKlinesSpec::admit`): a request the venue's lane will not
//!    serve is refused in its words, before the slot, the source or the store.
//! 3. **The connection's source**, built ONCE per (connection, venue) on the connection's first fetch
//!    and kept in the connection's [`ConnectionMemos`] slot, so it is dropped with the connection and
//!    the credential it read with it (the venue's source reads its credential at most once for its own
//!    life: decision 0097's verdict 2, a connection being a request).
//! 4. **The fetch**, handed a stop probe that reads the request's cancel flag; a
//!    `vike_data::source::SourceError` keeps its variant and its words.
//!
//! The lane holds no value between connections: the spec holds a store HANDLE (a location and a
//! scope) and a constructor, and the slot holds the sources.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use vike_bridge_core::credentialed_klines::{CredentialedKlinesSource, CredentialedKlinesSpec};
use vike_data::source::SourceError;
use vike_marketdata::Bar;
use vike_model::CredentialPresence;

use super::{FetchContext, cancelled};
use crate::session::ConnectionMemos;

/// What one connection holds for the credentialed lanes: each venue's source, built on that venue's first
/// fetch of the connection.
#[derive(Default)]
struct ConnectionSources {
    by_venue: Mutex<HashMap<&'static str, Arc<dyn CredentialedKlinesSource>>>,
}

/// **The credentialed kline lane of one venue**: the engine over that venue's spec. See the module doc.
pub struct CredentialedKlinesLane {
    spec: Box<dyn CredentialedKlinesSpec>,
}

impl CredentialedKlinesLane {
    /// The lane over `spec`.
    pub fn new(spec: Box<dyn CredentialedKlinesSpec>) -> Self {
        Self { spec }
    }

    /// The venue this lane answers for.
    pub fn venue(&self) -> &'static str {
        self.spec.venue()
    }

    /// The presence word of the lane's credential, read afresh.
    pub fn presence(&self) -> CredentialPresence {
        self.spec.presence()
    }

    /// `FetchCredentialed { Klines }` for this lane's venue: the module doc's four steps.
    pub fn fetch(
        &self,
        scope: &ConnectionMemos,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        ctx: &FetchContext,
    ) -> Result<Vec<Bar>, SourceError> {
        if ctx.is_cancelled() {
            return Err(cancelled("before the venue was asked"));
        }
        self.spec.admit(interval)?;
        let source = {
            let slot = scope.slot::<ConnectionSources>();
            let mut by_venue = slot.by_venue.lock().unwrap_or_else(PoisonError::into_inner);
            let source = by_venue
                .entry(self.spec.venue())
                .or_insert_with(|| Arc::from(self.spec.connection_source()));
            Arc::clone(source)
        };
        source.fetch_until(symbol, interval, start_ms, end_ms, &|| ctx.is_cancelled())
    }
}

impl std::fmt::Debug for CredentialedKlinesLane {
    /// The venue only: the spec holds a store handle and a constructor, nothing printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialedKlinesLane")
            .field("venue", &self.venue())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "credentialed_klines_tests.rs"]
mod credentialed_klines_tests;
