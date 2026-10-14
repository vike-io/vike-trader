//! The DATA SEAM: what the server asks of the venue half of the daemon for a live stream.
//!
//! `crate::server` owns the socket, the handshake, the scopes, the caps, the per-session queue and the
//! back-pressure; a `DataLane` owns the VENUE LOGIC and nothing else. The split is one function wide:
//! the server hands the lane an `Arc<dyn LiveDataSink>` (its wire sink, `vike_market_stream::sink`) and gets
//! back the venue's own `vike_data::DataClient`, which it then drives with exactly the calls an
//! in-process broker makes (`subscribe_*`, `unsubscribe`, `begin_shutdown`, `shutdown`). Every
//! `LiveDataSink` verb the client emits becomes one `vike_market_stream::wire::StreamDown` frame
//! with a per-session key id `k` and a per-`(k, verb)` `seq`, assigned BEFORE any drop decision.
//!
//! # What a lane promises
//!
//! * **`venues` is a BUILD fact.** It is the ordered set of keyless venues this build can open a stream
//!   for, and the `md_venue=` entries of `Welcome.features` are computed from it ONCE at start. It
//!   never reads a socket, a venue's health or a credential, so the advertisement cannot change
//!   because a venue is down (drift register R5): a venue that is down is `open` or `subscribe_*`
//!   failing, never a missing string.
//! * **`open` returns a client that has dialled nothing.** The in-process `DataClient` contract holds
//!   unchanged across the hop: `subscribe_*` is fire-and-forget, an `Err` means the stream was not
//!   STARTED, `unsubscribe` of an unknown id is a no-op, `begin_shutdown` returns at once and
//!   `shutdown` joins. The server calls them from ONE thread per session, so the client needs no lock
//!   of its own; the SINK is called from the client's feed threads, from the thread inside
//!   `subscribe_*` (a seed may be emitted before it returns) and from nowhere else.
//! * **A sink call is cheap and never blocks**, which is `LiveDataSink`'s own contract and the reason
//!   the wire sink is a bounded queue and not a socket write. A lane never sees a slow consumer.
//! * **One session is one isolation unit.** Nothing a session does (an unsubscribe, a reconnect storm,
//!   a slow reader) may gap another session's stream; the datahub opens one session per broker client
//!   key. The lane may multiplex TRANSPORT across sessions, never subscription lifecycle.
//!
//! Wave 2's credentialed streams arrive as a second verb with its own seam; nothing here is a
//! placeholder for them.
//!
//! # The lane over the bridges
//!
//! `venues` (behind the `data` feature, which links every bridge) is the real lane: `BridgeData`
//! answers [`DataLane::venues`] with the stream roster of `crate::registry` and [`DataLane::open`]
//! with the venue table `venues::build_client`, moved from the datahub. A build without `data` links
//! no bridge and has no real lane; a root that wants one there passes a lane of its own.

#[cfg(feature = "data")]
pub mod venues;

use std::net::SocketAddr;
use std::sync::Arc;

use vike_data::{DataClient, LiveDataSink};
use vike_hub_proto::auth::Scope;

/// Who asked for a session: the connection that opened it, for the lane's logs and its budget class.
///
/// It is NOT a venue-side concept. The datahub's feed broker has "holders" (the market-data hub, the
/// recorder) that share or split venue clients by its own sharing table; on the wire each broker
/// client key is simply one session, and the daemon sees only the connection that carries it.
#[derive(Debug, Clone)]
pub struct Holder {
    /// The daemon's name for this session (unique per process), the one `Response::DataOpened`
    /// reports.
    pub session: u64,
    /// The peer's address, when the socket still knows it.
    pub peer: Option<SocketAddr>,
    /// The scope the connection authenticated under.
    pub scope: Scope,
    /// The non-secret fingerprint (`nk-...`) of the key that authenticated, for the audit trail.
    pub key_id: Option<String>,
}

/// The venue half of a live stream. See the module doc.
pub trait DataLane: Send + Sync {
    /// The keyless venues this build can stream, in advertisement order (a `vike_model::VENUES` slug
    /// each). A build fact, independent of any venue's liveness.
    fn venues(&self) -> Vec<String>;

    /// A venue client for `venue` that delivers into `sink`, built for the session `holder` opened.
    ///
    /// `Err` is the lane's own words (the `build_client` error text of the in-process table: a venue
    /// with no client in this build, a settings row that does not parse) and reaches the peer as a
    /// positional `Response::Error`, so the connection stays usable.
    fn open(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String>;
}
