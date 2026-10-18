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
//! # The strategy lane's three additions (wave 2, W10)
//!
//! All three ride the SAME `DataOpen` session and the same wire sink; each is a default method of
//! [`DataLane`] that answers "not hosted", so a lane that hosts none (the scripted ones) compiles and
//! behaves as before.
//!
//! * **The tick pump** ([`DataLane::open_pump`], `crate::data::pump`): the CEX `spawn_*_market_data`
//!   feeds, which push `Ingest::Quote/Trade/Book/StreamStatus` onto a tick channel rather than into a
//!   `LiveDataSink`. The lane hosts the pump against a drained channel and turns each message into the
//!   sink verb of the same name (the book as an anchor plus deltas). One pump is SHARED by every
//!   session that names the same `(venue, symbol, tick_size)`.
//! * **Credentialed streams** ([`DataLane::open_credentialed`], `crate::data::credentialed`): alpaca,
//!   ctrader, ig and oanda, logged in as the account the strategy trades. `Scope::Write` only, and the
//!   credential is read HERE and never crosses the hop.
//! * **A network** ([`DataLane::open_on`]): the one venue whose feed follows the account tier
//!   (Hyperliquid) is built for the network the session names.
//!
//! # The lane over the bridges
//!
//! `venues` (behind the `data` feature, which links every bridge) is the real lane: `BridgeData`
//! answers [`DataLane::venues`] with the stream roster of `crate::registry` and [`DataLane::open`]
//! with the venue table `venues::build_client`, moved from the datahub. A build without `data` links
//! no bridge and has no real lane; a root that wants one there passes a lane of its own.

#[cfg(feature = "data")]
pub mod accounts;
#[cfg(feature = "data")]
pub mod credentialed;
#[cfg(feature = "data")]
pub mod pump;
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

    /// The venues that take a NETWORK on `DataOpen` (a `vike_model::VENUES` slug each): a build fact,
    /// advertised as `stream_network`. Empty for a lane with none.
    fn network_venues(&self) -> Vec<String> {
        Vec::new()
    }

    /// [`DataLane::open`] for the named `network` (`"mainnet"` / `"testnet"`, the words of
    /// `vike_gatewayhub_client::proto::DATA_NETWORK_MAINNET` / `_TESTNET`). The server refuses a
    /// network on a venue outside [`DataLane::network_venues`] BEFORE calling this, and an unknown word
    /// is this method's refusal: a network is never ignored (a silently ignored testnet would put a
    /// test-network strategy on a mainnet feed).
    fn open_on(
        &self,
        venue: &str,
        network: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let _ = (holder, sink);
        Err(format!("gatewayhub: venue `{venue}` takes no network (asked for `{network}`)"))
    }

    /// The venues a CEX tick pump is hosted for, in advertisement order: a build fact. Non-empty
    /// advertises `gw_pump`, the string that gates `SubOp::Ticks`.
    fn pump_venues(&self) -> Vec<String> {
        Vec::new()
    }

    /// Subscribe `sink` to the tick pump of `symbol` on `venue`, sized to the caller's `tick_size`
    /// (already validated finite, positive and under the ceiling by the server). `sink` is the session's
    /// sink and the figure of the book frames it has lost ([`PumpSink`]). The returned lease
    /// holds the subscription: dropping it unsubscribes, never blocks, and stops the pump when it was
    /// the last holder. `Err` is the lane's own words and reaches the peer as a refused `Sub`.
    fn open_pump(
        &self,
        venue: &str,
        holder: &Holder,
        symbol: &str,
        tick_size: f64,
        sink: PumpSink,
    ) -> Result<PumpLease, String> {
        let _ = (holder, symbol, tick_size, sink);
        Err(format!("gatewayhub: no tick pump is hosted for `{venue}` by this build"))
    }

    /// The credentialed-stream venues (`cs_venue=`), in advertisement order: a build fact. Disjoint
    /// from [`DataLane::venues`] (a venue is keyless or credentialed, never both).
    fn credentialed_venues(&self) -> Vec<String> {
        Vec::new()
    }

    /// A venue client for the credentialed `venue`, logged in as the account the strategy trades
    /// (the lane reads it: no credential, label or tier ever crosses the hop). The server has already
    /// required `Scope::Write`. `Err` carries the refusal text (absent credentials name the fields).
    fn open_credentialed(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let _ = (holder, sink);
        Err(format!("gatewayhub: no credentialed stream is hosted for `{venue}` by this build"))
    }
}

/// How many book frames a session's sink has lost so far (`vike_market_stream::sink::WireSink::book_losses`
/// of one symbol): a figure compared for CHANGE, never for size.
pub type BookLosses = Arc<dyn Fn() -> u64 + Send + Sync>;

/// What a tick pump delivers into: the session's sink, and (when the session can say) how many book
/// frames that sink has lost. A pump sends book DELTAS, and a lost delta leaves the subscriber's book
/// diverged for good; with the loss figure the pump hub puts an anchor on the wire the moment it moves
/// (`crate::data::pump`'s module doc).
pub struct PumpSink {
    /// The session's sink.
    pub sink: Arc<dyn LiveDataSink>,
    /// The session's book loss figure, `None` for a sink that does not track one.
    pub book_losses: Option<BookLosses>,
}

impl PumpSink {
    /// A sink whose book losses are not tracked (a test's, or a lane that has no queue to lose from).
    pub fn untracked(sink: Arc<dyn LiveDataSink>) -> Self {
        Self { sink, book_losses: None }
    }

    /// A sink with its loss figure.
    pub fn tracked(sink: Arc<dyn LiveDataSink>, book_losses: BookLosses) -> Self {
        Self { sink, book_losses: Some(book_losses) }
    }
}

/// What a session holds while it is subscribed to a tick pump ([`DataLane::open_pump`]).
///
/// Dropping it releases the subscription. The release is the lane's own and MUST NOT block: the session
/// reader drops a lease on an `Unsub`, and a pump thread can take seconds to join, so a lane hands the
/// join to a thread of its own.
pub struct PumpLease {
    _release: Box<dyn Send>,
}

impl PumpLease {
    /// A lease whose drop runs `release`'s drop.
    pub fn new(release: impl Send + 'static) -> Self {
        Self { _release: Box::new(release) }
    }
}

impl std::fmt::Debug for PumpLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpLease").finish_non_exhaustive()
    }
}
