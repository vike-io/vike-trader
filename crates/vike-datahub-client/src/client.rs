//! The blocking datahub client: a thin wrapper over one [`TcpStream`] that speaks the [`crate::proto`]
//! request/response protocol.
//!
//! The "thin client" half of the compute-to-data design: it ships a small profile (as TOML text)
//! or a typed read query and receives a compact answer — never a raw UPSTREAM data slice (a read
//! verb returns exactly the query's bounded result). One client owns one connection and issues
//! requests sequentially; it is not internally synchronized, so share it per thread.
//!
//! # The connect handshake
//!
//! Every constructor performs the [`Request::Hello`] / [`Response::Welcome`] version handshake
//! before returning and fails, NAMING BOTH versions, when they disagree — a stale client fails
//! legibly instead of desyncing on its first request. The server's advertised feature list is
//! readable via [`features`](DatahubClient::features); a verb added after protocol 7 is negotiated
//! through it, never by a version bump
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//!
//! # Authentication (`docs/decisions/0025-datahub-remote-posture.md`)
//!
//! - [`connect`](DatahubClient::connect) — key-less. Against a KEYED server (one advertising
//!   [`crate::proto::FEATURE_AUTH`]) it fails IMMEDIATELY with an actionable message, instead of
//!   connecting and having every verb refused one round trip later.
//! - [`connect_authed`](DatahubClient::connect_authed) — `Hello` → `Welcome{nonce}` →
//!   `Auth{scope, mac}` → `AuthOk`, signing the per-connection nonce with the scope's key. It
//!   degrades to a plain unauthenticated connect against a key-less server, so one keyed caller
//!   works against both; check [`authenticated_scope`](DatahubClient::authenticated_scope) when
//!   that distinction matters.
//! - [`connect_authed_on`](DatahubClient::connect_authed_on) — the same, refused before the first
//!   byte of `Auth` unless the `Welcome` names the plane the caller asked for
//!   ([`crate::proto::welcome_plane`]). A caller holding a key for ONE plane uses this one.
//!
//! The scope chosen at connect is the connection's ceiling for its whole life. [`Scope::Read`]
//! reads history and catalog; [`Scope::Write`] additionally admits the `Backfill` WRITE and every
//! `Run*` verb — those COMPILE CLIENT-SUPPLIED RHAI on the server, so they are not reads whatever
//! they return. [`crate::proto::required_scope`] is the authority for that mapping.
//!
//! # Bounding the connect and the handshake
//!
//! A routable but black-holed host (daemon down, a firewall that DROPs, a dead tunnel) stalls an
//! unbounded `TcpStream::connect` for the OS SYN-retry time, and a peer that accepts and then says
//! nothing (a wrong service on the port, a half-open connection) parks a `read` forever. Every
//! caller here is BLOCKING and single-connection, so an unbounded leg is the join a fan-out over
//! several datahubs waits out. Three constants bound it, each armed on a different phase:
//!
//! - `CONNECT_TIMEOUT` bounds the TCP handshake, PER RESOLVED ADDRESS (`connect_bounded`).
//!   ⚠ NAME RESOLUTION itself stays unbounded: the standard library has no bounded
//!   `to_socket_addrs`, so a hostname against a black-holed DNS server still parks the caller.
//!   Declared, not fixed — every deployment reaches its datahub by loopback, LAN address or tunnel.
//! - `HANDSHAKE_DEADLINE` bounds EACH read and EACH write of the `Hello`/`Welcome`
//!   (+ `Auth`/`AuthOk`) exchange.
//! - `REQUEST_WRITE_TIMEOUT` then replaces the write bound for the connection's life, armed by
//!   `DatahubClient::arm_request_timeouts`.
//!
//! ⚠ **A post-handshake READ is deliberately NOT bounded.** `arm_request_timeouts` CLEARS the read
//! timeout: past the handshake a read waits on SERVER-SIDE COMPUTATION (`RunParamscanProfile` over
//! a large grid, a long walk-forward, a `Backfill` over a venue's REST) whose legitimate duration
//! has no ceiling this crate could name — "a value that would clip a slow but live client is worse
//! than a leaked thread", the server's `IDLE_READ_TIMEOUT` rule. Telling "still computing" from
//! "died mid-request" needs a progress or keepalive frame, which [`crate::proto`] does not have.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use vike_data::store::removal::SeriesSelector;
use vike_data::{
    CohortRow, ExecFillRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage, SeriesId, TsRange,
};
use vike_model::{Bar, BookUpdate, EquitySample, QuoteTick, SymbolProperties, TradeTick};

use crate::archive::{ImportDone, ImportSpec, validate_import_spec};
use crate::catalog::CatalogListing;
use crate::history::HistoryChannelsReport;
use crate::market::{MD_READ_TIMEOUT, MdRefusal, MdSessionId, MdSpec, validate_md_symbol};
use crate::named_run::{NamedRoster, NamedRunOutcome, NamedRunSpec, validate_named_run};
use crate::proto::{
    BackfillCancelDone, BackfillDone, DeleteDone, FEATURE_ARCHIVE_IMPORT, FEATURE_AUTH,
    FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE,
    FEATURE_DELETE_SERIES, FEATURE_HISTORY_CHANNELS, FEATURE_MARKET_DATA, FEATURE_NAMED_RUN,
    FEATURE_SEARCH_METHOD, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES, FEATURE_STUDY,
    FEATURE_VENUE_CATALOG, FEATURE_WALKFORWARD_SEARCH, PROTO_VERSION, Plane, Request, Response,
    RunningBackfill, SeedDone, WireSearch, WireStudy, advertised_import_formats, read_frame,
    welcome_plane, write_frame,
};
use crate::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunResult, WireSlice, WireSpec,
    WireWalkforward, WireWalkforwardResult, WireWindowSearch,
};
use vike_node_proto::auth::{self, NodeKeys, Scope};

/// How long ONE resolved socket address gets for its TCP handshake before the next candidate is
/// tried (see [`connect_bounded`]).
///
/// It replaces the OS SYN-retry default (MINUTES on Linux) that a black-holed host stalls on. A live
/// peer — loopback, a LAN, an SSH tunnel — answers in milliseconds and the failure it bounds is
/// binary, so it is not tuned finer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long EACH read and EACH write of the protocol handshake gets — `Hello`/`Welcome` plus, on a
/// keyed server, `Auth`/`AuthOk`. Armed as BOTH socket timeouts for that phase only, then replaced
/// by [`arm_request_timeouts`](DatahubClient::arm_request_timeouts).
///
/// ⚠ A socket timeout is PER SYSCALL: a keyed handshake is six bounded calls (each sent frame one
/// `write_all`, each received one two `read_exact`s), so a peer answering each just inside the
/// window stretches the exchange to about six times this value. Accepted: that peer is answering.
///
/// The client mirror of `vike_datahub::server`'s `HANDSHAKE_DEADLINE` (nothing is COMPUTED during
/// a handshake), equal by choice, not coupling: either may be tuned alone.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Write timeout for the connection's life once the handshake is done.
///
/// A request frame is kilobytes even for a profile TOML or a Rhai source, so a write blocked this
/// long is a peer that has stopped READING (its connection thread wedged or gone on an open
/// socket); generous enough never to clip a busy server draining its receive buffer.
///
/// ⚠ A timeout here leaves a PARTIALLY WRITTEN frame on the wire: the stream is desynced and the
/// client must be dropped, not reused (the rule `vike_datahub::server`'s `handle_connection` states
/// for its own read timeouts). Nothing enforces it here; the caller must not issue a second request
/// on a client that failed to write one. The one caller that keeps going on one client by design is
/// `crates/vike-app-core/src/data/backfill_wire.rs`'s `run_wire_backfill`: on a desynced stream its
/// later ranges fail in a CASCADE (the server answers `Response::Error` to the garbage or closes),
/// and the report's `first_error` still names the write that started it — never corrupt data, since
/// the server folds no half-frame.
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

// Compile-time RANGES on the three: a deliberate tweak stays free, while a removed bound or one that
// refuses every legitimate connect fails to compile instead of shipping.
const _: () = assert!(
    CONNECT_TIMEOUT.as_secs() > 0 && CONNECT_TIMEOUT.as_secs() <= 60,
    "CONNECT_TIMEOUT must stay a POSITIVE, short bound — it exists to replace the OS SYN-retry \
     default, which a 0 (refuse everything) and a multi-minute value both fail to improve on"
);
const _: () = assert!(
    HANDSHAKE_DEADLINE.as_secs() > 0 && HANDSHAKE_DEADLINE.as_secs() <= 120,
    "HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — nothing is COMPUTED during the \
     handshake, so a long value only lengthens how long a silent peer parks the calling thread"
);
const _: () = assert!(
    REQUEST_WRITE_TIMEOUT.as_secs() > 0 && REQUEST_WRITE_TIMEOUT.as_secs() <= 600,
    "REQUEST_WRITE_TIMEOUT must stay a POSITIVE bound — request frames are kilobytes, so a value \
     large enough to matter is one that never fires on a peer that stopped reading"
);

/// Resolve `addr` and open a TCP connection to the FIRST address that answers within
/// [`CONNECT_TIMEOUT`], with [`HANDSHAKE_DEADLINE`] armed as the new stream's read and write
/// timeout for the protocol handshake that follows, and `TCP_NODELAY` on
/// (`vike_node_proto::frame::configure_node_stream`).
///
/// `connect_timeout` takes ONE `SocketAddr`, so the walk `TcpStream::connect` does itself is
/// written out here: a name routinely resolves to both an A and an AAAA record, and trying only the
/// first would fail against a dual-stack host whose IPv6 route is dark. The budget is therefore PER
/// ADDRESS: N candidates can cost N × [`CONNECT_TIMEOUT`].
///
/// The LAST error is returned when every candidate fails (the first is usually the dead IPv6 leg),
/// and an address that resolves to nothing is its own [`io::ErrorKind::InvalidInput`].
fn connect_bounded<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    let mut last_err: Option<io::Error> = None;
    for candidate in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&candidate, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(HANDSHAKE_DEADLINE))?;
                stream.set_write_timeout(Some(HANDSHAKE_DEADLINE))?;
                // Nagle OFF on the dialled end too (each end's option governs only its own sends).
                // A refusal costs latency, never correctness, and this crate has no logger, so
                // unlike the timeouts it does not fail the connect.
                let _ = vike_node_proto::frame::configure_node_stream(&stream);
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "datahub connect: the address resolved to no socket addresses",
        )
    }))
}

/// A connected datahub client. Wraps the underlying [`TcpStream`]; drop it to close the connection.
#[derive(Debug)]
pub struct DatahubClient {
    stream: TcpStream,
    /// The server's advertised feature strings, captured during the [`connect`](Self::connect)
    /// handshake. Empty before/without a successful `Welcome`.
    features: Vec<String>,
    /// The scope this connection authenticated under, or `None` against a key-less server (which
    /// requires and offers no authentication). See [`authenticated_scope`](Self::authenticated_scope).
    authed: Option<Scope>,
}

impl DatahubClient {
    /// Connect to a datahub server at `addr` (e.g. `"127.0.0.1:7878"`) and perform the
    /// [`Request::Hello`] / [`Response::Welcome`] version handshake.
    ///
    /// Returns `Err` on a transport failure OR a protocol-version mismatch — the latter an
    /// [`io::ErrorKind::InvalidData`] whose message NAMES BOTH versions (client and server).
    ///
    /// ⚠ Against a KEYED server (one advertising [`FEATURE_AUTH`]) this fails HERE, with an
    /// actionable message, rather than succeeding and having every verb refused later. Use
    /// [`connect_authed`](Self::connect_authed) for those.
    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let stream = connect_bounded(addr)?;
        let mut client = Self { stream, features: Vec::new(), authed: None };
        client.handshake()?;
        if client.requires_auth() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this datahub REQUIRES authentication (it advertises the `auth` feature) but no \
                 node keys were supplied. Connect with `DatahubClient::connect_authed`, and set \
                 VIKE_DATAHUB_OBSERVE_KEY / VIKE_DATAHUB_CONTROL_KEY in the credential store \
                 (`vike-cli secrets path` prints where that is)",
            ));
        }
        client.arm_request_timeouts()?;
        Ok(client)
    }

    /// Connect and AUTHENTICATE under `scope`, signing the server's per-connection nonce challenge
    /// with the matching key from `keys` (`docs/decisions/0025-datahub-remote-posture.md`).
    ///
    /// The exchange is `Hello` → `Welcome{nonce}` → `Auth{scope, mac}` → `AuthOk`, with the mac
    /// computed by [`vike_node_proto::auth::sign`] over the DATAHUB domain separator, so the tag
    /// binds the key, the scope, the protocol version AND this connection's nonce, and cannot be
    /// replayed onto another connection or against the tradehub node.
    ///
    /// ⚠ **A KEY-LESS server is not an error here — it is a successful unauthenticated connect.**
    /// A server that advertises no [`FEATURE_AUTH`] has no key to verify a mac against, so the
    /// client sends nothing and returns a working connection with
    /// [`authenticated_scope`](Self::authenticated_scope) `== None`. That lets ONE caller hold keys
    /// and still work against a local key-less dev server; a caller that must be sure it
    /// authenticated checks `authenticated_scope`.
    ///
    /// Fails with [`io::ErrorKind::PermissionDenied`] on a refused mac — a wrong key, a key this
    /// server does not hold for that scope, or a protocol-version skew (the version is signed).
    pub fn connect_authed<A: ToSocketAddrs>(
        addr: A,
        keys: &NodeKeys,
        scope: Scope,
    ) -> io::Result<Self> {
        Self::connect_authed_inner(addr, keys, scope, None)
    }

    /// [`connect_authed`](Self::connect_authed), but only to a server whose PRE-AUTH `Welcome` says
    /// it serves `plane` — checked after `Hello`/`Welcome` and BEFORE a single byte of `Auth` is
    /// written. On any other answer it refuses with [`io::ErrorKind::InvalidData`], naming the plane
    /// it found and the command that serves the one it wanted, and the peer has received nothing
    /// but the `Hello`.
    ///
    /// Why a Write key needs this: one node pair authenticates BOTH daemons
    /// (`crates/vike-backtest/src/compute_server.rs`'s `serve_authed` verifies the datahub's keys
    /// under the datahub's domain separator), so a CONTROL key signed toward the wrong port
    /// SUCCEEDS — and on the DATA plane its `VerbScope::Write` session also admits `Backfill` and
    /// `DeleteSeries`. The `Welcome` is the one frame both daemons send before auth.
    ///
    /// [`crate::proto::welcome_plane`] must answer exactly `plane`: a pre-split daemon and an
    /// unknown peer are refused as well as the wrong plane, and the check runs on a KEY-LESS server
    /// too (a legible "this is the data daemon" at connect beats a wrong-plane `Error` later).
    ///
    /// ⚠ It cannot see a peer that LIES in its `Welcome` (the feature list precedes the nonce): it
    /// defends against a MISDIRECTED client, not a hostile server.
    pub fn connect_authed_on<A: ToSocketAddrs>(
        addr: A,
        keys: &NodeKeys,
        scope: Scope,
        plane: Plane,
    ) -> io::Result<Self> {
        Self::connect_authed_inner(addr, keys, scope, Some(plane))
    }

    /// The one body both keyed constructors share. `expect` is `None` for the unguarded
    /// [`connect_authed`](Self::connect_authed); when it is `Some`, the plane is checked between the
    /// `Welcome` and the first byte of `Auth`, and every refusal returns before anything is written.
    fn connect_authed_inner<A: ToSocketAddrs>(
        addr: A,
        keys: &NodeKeys,
        scope: Scope,
        expect: Option<Plane>,
    ) -> io::Result<Self> {
        let stream = connect_bounded(addr)?;
        let mut client = Self { stream, features: Vec::new(), authed: None };
        let nonce = client.handshake()?;
        if let Some(wanted) = expect {
            let found = welcome_plane(&client.features);
            if found != Some(wanted) {
                let peer = client
                    .stream
                    .peer_addr()
                    .map_or_else(|_| "the server".to_string(), |a| a.to_string());
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    wrong_welcome_message(&peer, wanted, found, scope, &client.features),
                ));
            }
        }
        if !client.requires_auth() {
            // Key-less server: nothing to authenticate against (`connect_authed`'s ⚠). The
            // handshake is over, so the request-phase timeouts are armed on this arm too.
            client.arm_request_timeouts()?;
            return Ok(client);
        }
        let Some(nonce) = nonce else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "datahub handshake: server advertises `auth` but its Welcome carried no nonce",
            ));
        };
        let mac =
            auth::sign(auth::DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
        write_frame(&mut client.stream, &Request::Auth { scope, mac })?;
        match read_frame::<_, Response>(&mut client.stream)? {
            Response::AuthOk { scope: granted } => {
                client.authed = Some(granted);
                client.arm_request_timeouts()?;
                Ok(client)
            }
            Response::AuthDenied { reason } => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("datahub auth denied ({scope:?}): {reason}"),
            )),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub auth: expected AuthOk/AuthDenied, got {}", resp_kind(&other)),
            )),
        }
    }

    /// Open the connection with the version handshake: send our [`PROTO_VERSION`] in a
    /// [`Request::Hello`], read the server's [`Response::Welcome`], fail (naming both versions) on a
    /// mismatch, and otherwise store the server's advertised `features`.
    ///
    /// Returns the server's auth `nonce` — `Some` on a KEYED server, `None` on a key-less one.
    fn handshake(&mut self) -> io::Result<Option<[u8; 32]>> {
        write_frame(&mut self.stream, &Request::Hello { proto_version: PROTO_VERSION })?;
        match read_frame::<_, Response>(&mut self.stream)? {
            Response::Welcome { proto_version, features, nonce } => {
                check_proto_version(proto_version)
                    .map_err(|msg| io::Error::new(io::ErrorKind::InvalidData, msg))?;
                self.features = features;
                Ok(nonce)
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub handshake: expected Welcome, got {}", resp_kind(&other)),
            )),
        }
    }

    /// Swap the handshake-phase socket timeouts for the request-phase ones, once the handshake (and
    /// the `Auth` exchange, on a keyed server) has completed: the write timeout drops to
    /// [`REQUEST_WRITE_TIMEOUT`] and the READ timeout is CLEARED (the module doc's ⚠ says why).
    ///
    /// The `Err` is a failed `setsockopt`, propagated rather than ignored because a client that
    /// silently kept [`HANDSHAKE_DEADLINE`] as its read timeout would abort every legitimate
    /// long-running verb after ten seconds.
    fn arm_request_timeouts(&self) -> io::Result<()> {
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(Some(REQUEST_WRITE_TIMEOUT))?;
        Ok(())
    }

    /// The server's advertised feature strings, learned during the [`connect`](Self::connect)
    /// handshake (e.g. `"backtest"`, `"load_bars"`). Empty until a successful handshake.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Whether the connected server REQUIRES authentication — i.e. it advertises
    /// [`FEATURE_AUTH`], learned from the `Welcome` rather than from a protocol number.
    pub fn requires_auth(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_AUTH)
    }

    /// The [`Scope`] this connection authenticated under, or `None` on a key-less server (where no
    /// authentication took place, and every verb EXCEPT the destructive
    /// [`Request::DeleteSeries`] is served — so `None` also
    /// answers "may this connection delete anything": no). A caller that must be certain it is
    /// talking to an authenticated connection checks this, not merely that
    /// [`connect_authed`](Self::connect_authed) returned `Ok`.
    pub fn authenticated_scope(&self) -> Option<Scope> {
        self.authed
    }

    /// Liveness probe: send [`Request::Ping`] and assert the server answers [`Response::Pong`].
    /// A non-`Pong` reply is an [`io::ErrorKind::InvalidData`] protocol error.
    pub fn ping(&mut self) -> io::Result<()> {
        write_frame(&mut self.stream, &Request::Ping)?;
        match read_frame::<_, Response>(&mut self.stream)? {
            Response::Pong => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Pong, got {}", resp_kind(&other)),
            )),
        }
    }
}

// `DatahubClient`'s verbs, split by concern; this file keeps the bounds, the struct, the
// connect/handshake constructors and the helpers the verbs share.
mod backfill;
mod compute;
mod data_ops;
mod market_data;
mod reads;

/// What a successful [`DatahubClient::md_subscribe`] learned — the positional answer, unpacked, so a
/// caller never has to match `Response` again on a socket that has stopped carrying one.
#[derive(Debug, Clone, PartialEq)]
pub struct MdSubscribedInfo {
    /// The session token, for a later [`DatahubClient::md_update`] on a SHORT-LIVED connection.
    pub session: MdSessionId,
    /// The specs now served, in the server's AUTHORITATIVE form — depths already clamped.
    pub accepted: Vec<MdSpec>,
    /// The specs refused, each with its own typed reason. ⚠ A non-empty list is NOT a failure: a
    /// subscribe with one bad spec still opens the stream for the good ones, and
    /// [`MdRefusal::is_permanent`] is what decides whether the refused ones stay in the caller's
    /// desired set for a retry.
    pub refused: Vec<(MdSpec, MdRefusal)>,
    /// The server's own heartbeat period. The read deadline is already armed from it; this is here
    /// so a caller can LOG what it agreed to.
    pub heartbeat_ms: u64,
}

/// What a [`DatahubClient::md_update`] did.
#[derive(Debug, Clone, PartialEq)]
pub struct MdUpdatedInfo {
    /// Newly-served specs, authoritative form.
    pub accepted: Vec<MdSpec>,
    /// Refused additions.
    pub refused: Vec<(MdSpec, MdRefusal)>,
    /// The specs actually RELEASED — a `remove` naming a spec the session never held is absent here
    /// rather than an error.
    pub released: Vec<MdSpec>,
}

/// Refuse a market-data request LOCALLY when any spec's symbol is not one this wire carries —
/// [`crate::market::validate_md_symbol`], the same function the server's door calls.
///
/// ⚠ The message names the spec's POSITION, venue and lane, never the symbol: the symbol is the
/// unbounded field, and echoing it into an error a caller logs moves the cost rather than refusing
/// it (`validate_md_symbol` states the rule in full).
fn refuse_bad_symbols(verb: &str, specs: &[MdSpec]) -> Result<(), String> {
    for (i, s) in specs.iter().enumerate() {
        if let Err(why) = validate_md_symbol(&s.symbol) {
            return Err(format!(
                "{verb}: spec {i} ({}, {:?}) — {why} The connection is unchanged and nothing was \
                 sent.",
                s.venue, s.lane
            ));
        }
    }
    Ok(())
}

/// The variant NAME of a response (no payload) — for protocol-desync error messages, so we never
/// stringify a whole (possibly large) `Bars`/`Quotes`/`Trades` payload just to report "unexpected
/// variant".
fn resp_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::Pong => "Pong",
        Response::Report(_) => "Report",
        Response::RunResult(_) => "RunResult",
        Response::ParamscanResult(_) => "SweepResult",
        Response::WalkforwardResult(_) => "WalkforwardResult",
        Response::ParamscanReport(_) => "SweepReport",
        Response::WalkforwardReport(_) => "WalkforwardReport",
        Response::StudyReport(_) => "StudyReport",
        Response::Error(_) => "Error",
        Response::Bars(_) => "Bars",
        Response::Quotes(_) => "Quotes",
        Response::Trades(_) => "Trades",
        Response::BookUpdates(_) => "BookUpdates",
        Response::Depth(_) => "Depth",
        Response::Cohort(_) => "Cohort",
        Response::PerpMetrics(_) => "PerpMetrics",
        Response::Equity(_) => "Equity",
        Response::ExecFills(_) => "ExecFills",
        Response::Properties(_) => "Properties",
        Response::SeriesList(_) => "SeriesList",
        Response::Inventory(_) => "Inventory",
        Response::SeriesGaps(_) => "SeriesGaps",
        Response::SeriesFacts(_) => "SeriesFacts",
        Response::Coverage(_) => "Coverage",
        Response::Strategies(_) => "Strategies",
        Response::NamedStrategies(_) => "NamedStrategies",
        Response::NamedRun(_) => "NamedRun",
        Response::BackfillDone(_) => "BackfillDone",
        Response::RunningBackfills(_) => "RunningBackfills",
        Response::BackfillsCancelled(_) => "BackfillsCancelled",
        Response::HistoryChannels(_) => "HistoryChannels",
        Response::SeriesSeeded(_) => "SeriesSeeded",
        Response::VenueCatalog(_) => "VenueCatalog",
        Response::Deleted(_) => "Deleted",
        Response::ArchiveImported(_) => "ArchiveImported",
        Response::MdSubscribed { .. } => "MdSubscribed",
        Response::MdUpdated { .. } => "MdUpdated",
        Response::Md(_) => "Md",
    }
}

/// Compare the server's advertised protocol version against the client's [`PROTO_VERSION`],
/// producing the legible mismatch message (which NAMES BOTH versions) or `Ok(())` on a match.
/// Pure, so the contract is unit-testable without a socket; `handshake` wraps the `Err` into an
/// [`io::ErrorKind::InvalidData`] error.
fn check_proto_version(server_version: u32) -> Result<(), String> {
    if server_version == PROTO_VERSION {
        Ok(())
    } else {
        Err(format!(
            "datahub protocol version mismatch: client speaks {PROTO_VERSION}, \
             server speaks {server_version}"
        ))
    }
}

/// The refusal [`DatahubClient::connect_authed_on`] returns when the `Welcome` does not name the
/// plane it was asked for — pure, so its wording is unit-tested without a socket.
///
/// It says which daemon ANSWERED (or that the answer named none), what the key would have opened
/// there, and which command and setting serve the plane the operator wanted. It names no key byte
/// and no key name.
fn wrong_welcome_message(
    peer: &str,
    wanted: Plane,
    found: Option<Plane>,
    scope: Scope,
    features: &[String],
) -> String {
    let answered = match found {
        Some(Plane::Data) => "the DATA daemon (`vike-backend datahub`), whose Write scope also \
                              carries `Backfill` and `DeleteSeries`"
            .to_string(),
        Some(Plane::Compute) => "the COMPUTE daemon (`vike-backend backtest --addr`)".to_string(),
        Some(Plane::Shared) | None => format!(
            "a server whose Welcome names no single plane (features {features:?}) — a pre-split \
             daemon serving both, or not a vike daemon at all"
        ),
    };
    format!(
        "refusing to authenticate ({scope:?}) to {peer}: it is {answered}, and this connection was \
         asked for the {wanted:?} daemon, served by `{}`. Nothing was sent after the Hello. Point \
         the client at that daemon ({}).",
        wanted.served_by(),
        wanted.addr_key(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal names the daemon that answered, what a Write session would have opened there,
    /// and where the wanted daemon is configured — and never a key.
    #[test]
    fn the_wrong_welcome_refusal_says_which_daemon_answered_and_where_to_point() {
        let data = vec![crate::proto::DATA_PLANE_SENTINEL.to_string()];
        let msg = wrong_welcome_message(
            "127.0.0.1:7878",
            Plane::Compute,
            Some(Plane::Data),
            Scope::Write,
            &data,
        );
        assert!(msg.contains("DATA daemon"), "names what answered: {msg}");
        assert!(msg.contains("DeleteSeries"), "says what the key would have opened: {msg}");
        assert!(msg.contains("config.backtest_addr"), "says where to point: {msg}");
        assert!(msg.contains("Nothing was sent"), "says nothing leaked: {msg}");

        let unknown = wrong_welcome_message("h:1", Plane::Compute, None, Scope::Write, &[]);
        assert!(unknown.contains("no single plane"), "{unknown}");
    }

    #[test]
    fn check_proto_version_accepts_a_matching_server() {
        assert!(check_proto_version(PROTO_VERSION).is_ok());
    }

    /// A mismatched server version yields an error naming BOTH the client's and the server's number,
    /// so a stale binary reports something a human can act on.
    #[test]
    fn check_proto_version_flags_a_mismatch_naming_both() {
        let server_version = PROTO_VERSION + 1;
        let err = check_proto_version(server_version).unwrap_err();
        assert!(err.contains(&PROTO_VERSION.to_string()), "names the client version: {err}");
        assert!(err.contains(&server_version.to_string()), "names the server version: {err}");
    }

    /// The stream `connect_bounded` hands back is ALREADY bounded both ways at
    /// [`HANDSHAKE_DEADLINE`]. Proven against a listener never accepted from: the connect completes
    /// anyway (the kernel's backlog), the shape a wrong-service-on-the-port peer has.
    #[test]
    fn connect_bounded_arms_the_handshake_deadline_both_ways() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");

        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");

        assert_eq!(stream.read_timeout().expect("read_timeout"), Some(HANDSHAKE_DEADLINE));
        assert_eq!(stream.write_timeout().expect("write_timeout"), Some(HANDSHAKE_DEADLINE));
    }

    /// The dialled stream carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`),
    /// so a request is not held for the server's delayed ACK. Every `DatahubClient` constructor
    /// dials through `connect_bounded`, so this one socket option covers all of them.
    #[test]
    fn connect_bounded_turns_nagle_off() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");

        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");

        assert!(stream.nodelay().expect("read TCP_NODELAY"), "the dialled stream has Nagle on");
    }

    /// `arm_request_timeouts` performs the phase swap, and the CLEARED read is asserted as loudly as
    /// the armed write: "tidying" the `None` into a ceiling would abort a legitimate long
    /// `RunParamscanProfile`.
    #[test]
    fn arm_request_timeouts_clears_the_read_and_bounds_the_write() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");
        let client = DatahubClient { stream, features: Vec::new(), authed: None };

        client.arm_request_timeouts().expect("arm the request-phase timeouts");

        assert_eq!(
            client.stream.read_timeout().expect("read_timeout"),
            None,
            "a post-handshake read waits on server-side computation and must stay unbounded"
        );
        assert_eq!(
            client.stream.write_timeout().expect("write_timeout"),
            Some(REQUEST_WRITE_TIMEOUT)
        );
    }

    /// An address that resolves to NOTHING gets its own [`io::ErrorKind::InvalidInput`]: the walk
    /// in `connect_bounded` has no `last_err` to report in that case.
    #[test]
    fn connect_bounded_reports_an_address_that_resolves_to_nothing() {
        // An empty `SocketAddr` slice yields no candidates, without a DNS lookup in a test.
        let empty: &[std::net::SocketAddr] = &[];
        let err = connect_bounded(empty).expect_err("no candidates must not be an Ok connection");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("no socket addresses"), "legible message: {err}");
    }
}
