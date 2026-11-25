//! **The credentialed kline lane's ONE contract: the venue's half.** The gatewayhub serves
//! `FetchCredentialed { Klines }` (a `credentialed_data_venue=` klines fetch that spends a credential the daemon holds)
//! through ONE engine, `crates/vike-gatewayhub/src/rest/credentialed_klines.rs`'s
//! `CredentialedKlinesLane`, and every venue's difference is a [`CredentialedKlinesSpec`] in that venue's
//! bridge: which account's fields it reads, how it builds a connection's source around them, the words it
//! refuses and announces with. The engine + spec split of `crate::data_client::ws`'s `DataClientWebsocket`, for
//! the history lane.
//!
//! # What the engine does, so a spec does not
//!
//! The engine checks the request's cancel flag, asks [`CredentialedKlinesSpec::admit`], builds ONE
//! connection source per (connection, venue) through [`CredentialedKlinesSpec::connection_source`] on
//! the connection's first fetch, keeps it for the rest of the connection and drops it with the
//! connection, hands every fetch a stop probe that reads the cancel flag, and turns a
//! `vike_data::source::SourceError` into the wire's failure with its variant and its words unchanged.
//!
//! # What a spec promises
//!
//! * **A connection source reads its credential at most ONCE for its own life.** A connection is a
//!   request (`docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md`,
//!   verdict 2), so the memo a source reads through is born with the source and dies with it
//!   (`crate::connection_memo`): a credential stored, rotated or removed between two requests is what
//!   the next one sees. ⚠ The memo must not be reachable from the thing it is stored in, or dropping the
//!   connection would not drop the credential.
//! * **Nothing is read before it is needed.** Building a source reads no store; a refusal the source
//!   gives before it needs a credential (an interval the venue does not serve) reads none.
//! * **Unreadable is not absent**, in the read and in [`CredentialedKlinesSpec::presence`]: a store that
//!   exists and will not open is its own answer, never the not-configured one.
//! * **No credential value leaves the spec** but through the source it builds: `presence` is a word, and
//!   the startup line carries names and words only.

use std::path::Path;

use vike_data::source::{KlineSource, SourceError};
use vike_marketdata::Bar;
use vike_model::CredentialPresence;

/// **A venue's credentialed kline lane**, as the gatewayhub's engine asks it. See the module doc.
pub trait CredentialedKlinesSpec: Send + Sync {
    /// The venue this lane answers for: a `vike_model::VENUES` slug.
    fn venue(&self) -> &'static str;

    /// Refuse a request this lane will not serve, before anything is built or read: no connection slot,
    /// no source, no store read. The default admits every request.
    fn admit(&self, interval: &str) -> Result<(), SourceError> {
        let _ = interval;
        Ok(())
    }

    /// One connection's source, built on the connection's first fetch and dropped with the connection.
    /// It reads its credential at most once for its own life (the module doc).
    fn connection_source(&self) -> Box<dyn CredentialedKlinesSource>;

    /// Whether the credential is stored: a word, never a value, read afresh.
    fn presence(&self) -> CredentialPresence;

    /// The line the daemon logs once at its start, and the presence word it was read with: the composition
    /// root logs it at `warn` when the word is `Unreadable` and at `info` otherwise. `settings_dir` is the
    /// directory the boot resolved (`None`: no store to hold the credential).
    fn startup_line(&self, settings_dir: Option<&Path>) -> (CredentialPresence, String);
}

/// **What the engine asks of a connection's source**: the closed bars of `[start_ms, end_ms]`, asking
/// `stop` wherever the source can stop (a wait, a page). A source with no such point ignores it, and
/// the engine's check before the call is the only one.
pub trait CredentialedKlinesSource: Send + Sync {
    /// The bars of `[start_ms, end_ms]` of `(symbol, interval)`, asking `stop` at every point the source
    /// can stop at.
    fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError>;
}

/// A plain `KlineSource` (one call, the venue's paging inside it, no stop point): asked once, its probe
/// unread.
impl CredentialedKlinesSource for Box<dyn KlineSource> {
    fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        _stop: &dyn Fn() -> bool,
    ) -> Result<Vec<Bar>, SourceError> {
        self.fetch(symbol, interval, start_ms, end_ms)
    }
}
