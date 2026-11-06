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
//! - [`connect_with_keys`](DatahubClient::connect_with_keys) — `connect_authed` when the caller
//!   holds keys, `connect` when it does not: the one spelling of that choice.
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
//! several datahubs waits out. Three constants bound it, each armed on a different phase (the first two
//! are `vike_hub_proto::limits`, shared with `vike-strategy-builder`'s client):
//!
//! - `CLIENT_CONNECT_TIMEOUT` bounds the TCP handshake, PER RESOLVED ADDRESS (`vike_hub_proto::frame::connect_bounded`).
//!   ⚠ NAME RESOLUTION itself stays unbounded: the standard library has no bounded
//!   `to_socket_addrs`, so a hostname against a black-holed DNS server still parks the caller.
//!   Declared, not fixed — every deployment reaches its datahub by loopback, LAN address or tunnel.
//! - `CLIENT_HANDSHAKE_DEADLINE` bounds EACH read and EACH write of the `Hello`/`Welcome`
//!   (+ `Auth`/`AuthOk`) exchange.
//! - `REQUEST_WRITE_TIMEOUT` then replaces the write bound for the connection's life, armed by
//!   `DatahubClient::arm_request_timeouts`.
//!
//! # Bounding a reply: an IDLE bound per verb
//!
//! Past the handshake every verb goes through ONE exchange (`DatahubClient::exchange`), which arms
//! the socket's read timeout for THAT verb before writing it. A socket read timeout is per
//! syscall, so it is an IDLE bound (no byte for that long), never a total deadline: a large reply
//! streaming steadily is never clipped, and a peer that vanished without a FIN or an RST (a
//! suspended laptop, a dropped tunnel, a NAT eviction) produces no byte and is.
//!
//! The bound comes from `default_reply_timeout`, keyed on [`crate::proto::reply_wait`]'s
//! exhaustive class: `REPLY_IDLE_TIMEOUT` for a verb whose work the SERVER bounds, NONE for one
//! whose size the CLIENT names (`Backfill`, `ImportArchive`, every `Run*`). Those stay unbounded on
//! purpose: the wire has no progress frame, so their reply is silent until the work is done, and a
//! bound that clips a slow but live one is worse than a parked thread. A caller that knows its
//! scale replaces the table for its client with
//! [`set_reply_timeout`](DatahubClient::set_reply_timeout) (the desktop's options poll asks for
//! seconds), and a caller that must abandon an unbounded verb holds a
//! [`CancelHandle`](DatahubClient::cancel_handle).
//!
//! ⚠ **A failed exchange makes the client UNUSABLE.** A timeout or any transport error mid-frame
//! leaves the stream desynced, so `exchange` records the failure and every later verb on that
//! client is refused LOCALLY with [`io::ErrorKind::NotConnected`], nothing sent, naming the first
//! failure. A server's `Response::Error` is not a transport failure and leaves the client usable.
//! A timed-out read is reported as [`io::ErrorKind::TimedOut`] on every platform (the socket layer
//! answers `WouldBlock` on unix and `TimedOut` on windows), naming the verb and the bound.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use vike_data::store::removal::SeriesSelector;
use vike_data::{
    CohortRow, ExecFillRow, ExecOrderRow, InstrumentCoverage, PerpMetricRow, SeriesCoverage,
    SeriesId, TsRange,
};
use vike_marketdata::{Bar, BookUpdate, QuoteTick, TradeTick};
use vike_model::{EquitySample, SymbolProperties};

use crate::archive::{ImportDone, ImportSpec, validate_import_spec};
use crate::catalog::CatalogListing;
use crate::history::HistoryChannelsReport;
use crate::market::{MD_READ_TIMEOUT, MdRefusal, MdSessionId, MdSpec, validate_md_symbol};
use crate::named_run::{NamedRoster, NamedRunOutcome, NamedRunSpec, validate_named_run};
use crate::options::{OptionsChainReply, validate_options_request};
use crate::proto::{
    BackfillCancelDone, BackfillDone, DeleteDone, FEATURE_ARCHIVE_IMPORT, FEATURE_AUTH,
    FEATURE_BACKFILL, FEATURE_BACKFILL_CANCEL, FEATURE_BACKFILL_FUNDING, FEATURE_COVERAGE,
    FEATURE_DELETE_SERIES, FEATURE_EXEC_MATERIALIZE, FEATURE_HISTORY_CHANNELS, FEATURE_MARKET_DATA,
    FEATURE_NAMED_RUN, FEATURE_OPTIONS_CHAIN, FEATURE_SCAN_EXEC_FILLS_RANGE,
    FEATURE_SCAN_EXEC_ORDERS, FEATURE_SEARCH_METHOD, FEATURE_SEED_CLASS, FEATURE_SEED_SERIES,
    FEATURE_STUDY, FEATURE_TOOL_FEEDS, FEATURE_VENUE_CATALOG, FEATURE_WALKFORWARD_SEARCH,
    PROTO_VERSION, Plane, ReplyWait, Request, Response, RunningBackfill, SeedDone, WireSearch,
    WireStudy, advertised_import_formats, read_frame, reply_wait, request_kind, welcome_plane,
    write_frame,
};
use crate::tool_feeds::{
    EconomicCalendarReply, EquityCalendarReply, NewsReply, validate_equity_week_shape,
};
use crate::wire_studio::{
    WireEngineParams, WireParamscan, WireParamscanResult, WireRunResult, WireSlice, WireSpec,
    WireWalkforward, WireWalkforwardResult, WireWindowSearch,
};
use vike_hub_proto::auth::{self, HubKeys, Scope};
use vike_hub_proto::frame::connect_bounded;
use vike_hub_proto::limits::CLIENT_HANDSHAKE_DEADLINE;

/// Write timeout for the connection's life once the handshake is done.
///
/// A request frame is kilobytes even for a profile TOML or a Rhai source, so a write blocked this
/// long is a peer that has stopped READING (its connection thread wedged or gone on an open
/// socket); generous enough never to clip a busy server draining its receive buffer.
///
/// ⚠ A timeout here leaves a PARTIALLY WRITTEN frame on the wire: the stream is desynced (the rule
/// `vike_datahub::server`'s `handle_connection` states for its own read timeouts), so
/// `DatahubClient::exchange` marks the client unusable and refuses every later verb locally. The
/// one caller that keeps going on one client by design is
/// `crates/vike-app-core/src/data/backfill_wire.rs`'s `run_wire_backfill`: its later ranges are
/// refused without a frame, and the report's `first_error` names the write that started it.
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The idle read bound of a verb whose work the SERVER bounds ([`ReplyWait::Bounded`]): the
/// longest a reply may go without a byte before the link is called dead.
///
/// Set above the longest LEGITIMATE silence found in those verbs, with headroom. Store reads are
/// manifest walks or ceiling-capped scans (`inventory` and `coverage` measured at about 1 s
/// end-to-end, `crates/vike-app-core/src/data/stored_load.rs`'s module doc). The slow ones call a
/// venue, and each venue request is capped by `vike_bridge_core::http`'s 30 s global timeout:
/// `SeedSeries` pages its `SEED_BARS` window in at most six paced requests (OKX) and the deribit
/// catalog is five sequential requests, so their honest worst case is about 150 to 180 s of
/// silence. 300 s clears that by more than 1.5x.
///
/// ⚠ It happens to equal the server's default idle read timeout (`vike_hub_proto::limits::SERVER_IDLE_READ_TIMEOUT`)
/// and is NOT tied to it, in either direction: this is how long a CLIENT waits for a reply, that is how
/// long a SERVER waits for the next request, and the server's is a row an operator may set
/// (`config.node_idle_read_timeout_ms`). A server that reaps an idle connection does not touch a reply
/// this client is waiting for (the server reads nothing while it works), and a reply bound shorter or
/// longer than the server's changes nothing about when the server reaps.
const REPLY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The default idle read bound of one request: the table behind [`DatahubClient::exchange`],
/// derived from [`reply_wait`]'s exhaustive class so a new verb cannot get a value by accident.
fn default_reply_timeout(request: &Request) -> Option<Duration> {
    match reply_wait(request) {
        ReplyWait::Handshake => Some(CLIENT_HANDSHAKE_DEADLINE),
        ReplyWait::Bounded => Some(REPLY_IDLE_TIMEOUT),
        ReplyWait::Unbounded => None,
    }
}

// Compile-time RANGES on the two request-phase bounds: a deliberate tweak stays free, while a removed
// bound fails to compile instead of shipping. The connect and handshake bounds carry theirs in
// `vike_hub_proto::limits`.
const _: () = assert!(
    REQUEST_WRITE_TIMEOUT.as_secs() > 0 && REQUEST_WRITE_TIMEOUT.as_secs() <= 600,
    "REQUEST_WRITE_TIMEOUT must stay a POSITIVE bound — request frames are kilobytes, so a value \
     large enough to matter is one that never fires on a peer that stopped reading"
);
const _: () = assert!(
    REPLY_IDLE_TIMEOUT.as_secs() >= 180 && REPLY_IDLE_TIMEOUT.as_secs() <= 3600,
    "REPLY_IDLE_TIMEOUT must stay above the longest legitimate silence of a server-bounded verb \
     (a six-page seed or a five-request catalog at 30 s a request) and finite enough that a dead \
     link is reported within the hour"
);

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
    /// The caller's idle reply bound for EVERY verb, or `None` for the per-verb table
    /// (`default_reply_timeout`). See [`set_reply_timeout`](Self::set_reply_timeout).
    reply_override: Option<Duration>,
    /// Why this client may send nothing more: the first exchange that failed mid-frame. `None`
    /// while the stream is in step. See [`exchange`](Self::exchange).
    unusable: Option<String>,
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
        let mut client = Self::on_stream(stream);
        client.handshake()?;
        if client.requires_auth() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this datahub REQUIRES authentication (it advertises the `auth` feature) but no \
                 hub keys were supplied. Connect with `DatahubClient::connect_authed`, and set \
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
    /// computed by [`vike_hub_proto::auth::sign`] over the DATAHUB domain separator, so the tag
    /// binds the key, the scope, the protocol version AND this connection's nonce, and cannot be
    /// replayed onto another connection or against the tradehub.
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
        keys: &HubKeys,
        scope: Scope,
    ) -> io::Result<Self> {
        Self::connect_authed_inner(addr, keys, scope, None, |_| {})
    }

    /// [`connect_authed`](Self::connect_authed), but only to a server whose PRE-AUTH `Welcome` says
    /// it serves `plane` — checked after `Hello`/`Welcome` and BEFORE a single byte of `Auth` is
    /// written. On any other answer it refuses with [`io::ErrorKind::InvalidData`], naming the plane
    /// it found and the command that serves the one it wanted, and the peer has received nothing
    /// but the `Hello`.
    ///
    /// Why a Write key needs this: one node pair authenticates BOTH daemons
    /// (`crates/vike-backtesthub/src/backtesthub_server.rs`'s `serve_authed` verifies the datahub's keys
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
        keys: &HubKeys,
        scope: Scope,
        plane: Plane,
    ) -> io::Result<Self> {
        Self::connect_authed_inner(addr, keys, scope, Some(plane), |_| {})
    }

    /// The dial of a caller that MAY hold keys: [`connect_authed`](Self::connect_authed) under
    /// `scope` when `keys` is `Some`, plain [`connect`](Self::connect) when it is `None` (`scope` is
    /// then unused — a key-less dial negotiates no scope).
    ///
    /// Each arm keeps its own constructor's contract exactly: `Some` against a KEY-LESS server is a
    /// clean unauthenticated connect (`connect_authed`'s ⚠), and `None` against a KEYED server
    /// fails at the handshake naming the missing keys (`connect`'s ⚠). There is no third branch —
    /// no fallback from a refused key to an unauthenticated dial.
    ///
    /// ⚠ It is UNGUARDED on the plane, like `connect_authed`: a caller holding a key for ONE plane
    /// dials [`connect_authed_on`](Self::connect_authed_on) instead.
    pub fn connect_with_keys<A: ToSocketAddrs>(
        addr: A,
        keys: Option<&HubKeys>,
        scope: Scope,
    ) -> io::Result<Self> {
        match keys {
            Some(k) => Self::connect_authed(addr, k, scope),
            None => Self::connect(addr),
        }
    }

    /// The one body both keyed constructors share. `expect` is `None` for the unguarded
    /// [`connect_authed`](Self::connect_authed); when it is `Some`, the plane is checked between the
    /// `Welcome` and the first byte of `Auth`, and every refusal returns before anything is written.
    /// `observe` is handed the freshly dialled socket BEFORE the handshake reads from it (the strategy
    /// lane's long-lived owner stores a clone so a shutdown can break a handshake parked on a peer that
    /// accepted and went quiet); the other callers pass `|_| {}`.
    fn connect_authed_inner<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        expect: Option<Plane>,
        observe: impl FnOnce(&TcpStream),
    ) -> io::Result<Self> {
        let stream = connect_bounded(addr)?;
        observe(&stream);
        let mut client = Self::on_stream(stream);
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

    /// A client over a freshly dialled `stream`: nothing learned yet, the per-verb reply table, in
    /// step.
    fn on_stream(stream: TcpStream) -> Self {
        Self { stream, features: Vec::new(), authed: None, reply_override: None, unusable: None }
    }

    /// Swap the handshake-phase socket timeouts for the request-phase ones, once the handshake (and
    /// the `Auth` exchange, on a keyed server) has completed: the write timeout drops to
    /// [`REQUEST_WRITE_TIMEOUT`] and the READ timeout is CLEARED, because each verb arms its own
    /// in [`exchange`](Self::exchange) (the module doc's "Bounding a reply").
    ///
    /// The `Err` is a failed `setsockopt`, propagated rather than ignored because a client that
    /// silently kept [`CLIENT_HANDSHAKE_DEADLINE`] as its write timeout would bound a large request by the
    /// handshake's number.
    fn arm_request_timeouts(&self) -> io::Result<()> {
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(Some(REQUEST_WRITE_TIMEOUT))?;
        Ok(())
    }

    /// **Bound every verb's reply on this client by `timeout`** — an IDLE bound (no byte for that
    /// long), armed per exchange. `None` restores the per-verb default table, under which a verb
    /// whose work the client names (`Backfill`, `ImportArchive`, every `Run*`) is unbounded and
    /// every other verb gets 300 s.
    ///
    /// For a caller that knows its own scale: the desktop's options poll asks a snapshot verb once
    /// a second and passes seconds; a CLI that sends a one-month import can pass hours. A
    /// `Some(Duration::ZERO)` is refused by the socket layer at the next verb
    /// ([`io::ErrorKind::InvalidInput`], nothing sent).
    pub fn set_reply_timeout(&mut self, timeout: Option<Duration>) {
        self.reply_override = timeout;
    }

    /// The idle reply bound a verb arms for `request` on this client before writing it: the
    /// caller's [`set_reply_timeout`](Self::set_reply_timeout) value, else the per-verb default
    /// ([`crate::proto::reply_wait`]'s class).
    pub fn reply_timeout_for(&self, request: &Request) -> Option<Duration> {
        self.reply_override.or_else(|| default_reply_timeout(request))
    }

    /// Whether this client may still send: `false` once an exchange failed mid-frame (a timeout, a
    /// reset, a closed peer), after which every verb is refused locally. Drop it and dial again.
    pub fn is_usable(&self) -> bool {
        self.unusable.is_none()
    }

    /// **The ONE post-handshake exchange**: arm `request`'s idle reply bound, write it, read one
    /// response. Every positional verb goes through here.
    ///
    /// ⚠ Any transport error from the write or the read MARKS THE CLIENT UNUSABLE (the module doc's
    /// ⚠): a later call returns [`io::ErrorKind::NotConnected`] without writing, naming the first
    /// failure. A timeout on either leg comes back as [`io::ErrorKind::TimedOut`] whatever the
    /// platform's own kind. A refused `setsockopt` and a frame `write_frame` refuses before its
    /// first byte (`InvalidData`) send nothing and leave the client usable.
    fn exchange(&mut self, request: &Request) -> io::Result<Response> {
        let verb = request_kind(request);
        if let Some(first) = &self.unusable {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                format!(
                    "datahub {verb}: not sent — this connection is unusable since {first}; drop \
                     the client and dial a new one"
                ),
            ));
        }
        let bound = self.reply_timeout_for(request);
        self.stream.set_read_timeout(bound)?;
        if let Err(e) = write_frame(&mut self.stream, request) {
            // `InvalidData` is `write_frame`'s refusal BEFORE the first byte (a body that does not
            // serialize, or one over `MAX_FRAME_LEN`): nothing reached the wire, the stream is in
            // step, and the client stays usable.
            if e.kind() == io::ErrorKind::InvalidData {
                return Err(e);
            }
            let e = legible_stall(e, || {
                format!(
                    "datahub {verb}: the server accepted no request byte for \
                     {REQUEST_WRITE_TIMEOUT:?} (it stopped reading)"
                )
            });
            return Err(self.mark_unusable(verb, e));
        }
        read_frame::<_, Response>(&mut self.stream).map_err(|e| {
            let e = legible_stall(e, || {
                let bound = bound.map_or_else(|| "its bound".to_string(), |b| format!("{b:?}"));
                format!(
                    "datahub {verb}: no reply byte for {bound} — the link is dead or the server \
                     is wedged (a half-open socket answers nothing); this connection is not reused"
                )
            });
            self.mark_unusable(verb, e)
        })
    }

    /// Record `e` as the reason this client may send nothing more, and hand it back.
    fn mark_unusable(&mut self, verb: &str, e: io::Error) -> io::Error {
        self.unusable = Some(format!("{verb} failed mid-exchange ({e})"));
        e
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

    /// A handle that can CLOSE this connection from another thread — the way a caller abandons a
    /// verb blocked in its reply read before the idle bound fires, or an unbounded one
    /// (`Backfill`, `ImportArchive`, a `Run*`) that has none.
    ///
    /// It holds a `try_clone` of the socket, so [`CancelHandle::shutdown`] wakes the blocked read
    /// with an error, and the server sees its peer go: a backtesthub can stop working for a
    /// client that is no longer listening. The client itself is unusable after that, exactly as
    /// after any other transport failure; the stream field stays private.
    ///
    /// `Err` is a failed `try_clone` (descriptor exhaustion), reported rather than swallowed so a
    /// caller never believes it holds a cancel it does not.
    pub fn cancel_handle(&self) -> io::Result<CancelHandle> {
        Ok(CancelHandle { stream: self.stream.try_clone()? })
    }

    /// Liveness probe: send [`Request::Ping`] and assert the server answers [`Response::Pong`].
    /// A non-`Pong` reply is an [`io::ErrorKind::InvalidData`] protocol error.
    pub fn ping(&mut self) -> io::Result<()> {
        match self.exchange(&Request::Ping)? {
            Response::Pong => Ok(()),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected Pong, got {}", resp_kind(&other)),
            )),
        }
    }
}

/// The other end of [`DatahubClient::cancel_handle`]: one method, [`shutdown`](Self::shutdown).
///
/// Safe to send to another thread and to call while the client is blocked in a verb; calling it
/// twice, or after the peer closed, is harmless.
#[derive(Debug)]
pub struct CancelHandle {
    stream: TcpStream,
}

impl CancelHandle {
    /// Shut the connection down both ways. The client's blocked read (if any) returns an error and
    /// the server reads end-of-stream. The `Err` of a socket already shut or reset is ignored: there
    /// is nothing left to close, and the caller has no better answer to give.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

// `DatahubClient`'s verbs, split by concern; this file keeps the bounds, the struct, the
// connect/handshake constructors and the helpers the verbs share.
mod backfill;
mod backtest;
mod data_ops;
mod market_data;
mod reads;
mod strat_lane;

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

/// A socket TIMEOUT re-spelled as [`io::ErrorKind::TimedOut`] carrying `sentence`; any other error
/// unchanged. Both kinds are matched because the socket layer reports an expired `SO_RCVTIMEO` /
/// `SO_SNDTIMEO` as `WouldBlock` (`EAGAIN`) on unix and as `TimedOut` (`WSAETIMEDOUT`) on windows,
/// and a caller deciding "the link is dead" must not have to know which box it runs on.
fn legible_stall(e: io::Error, sentence: impl FnOnce() -> String) -> io::Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, sentence())
        }
        _ => e,
    }
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
        Response::ExecOrders(_) => "ExecOrders",
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
        Response::OptionsChain(_) => "OptionsChain",
        Response::NewsFeed(_) => "NewsFeed",
        Response::EconomicCalendar(_) => "EconomicCalendar",
        Response::EquityCalendar(_) => "EquityCalendar",
        Response::Deleted(_) => "Deleted",
        Response::ArchiveImported(_) => "ArchiveImported",
        Response::MdSubscribed { .. } => "MdSubscribed",
        Response::MdUpdated { .. } => "MdUpdated",
        Response::StratOpened { .. } => "StratOpened",
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
        Some(Plane::Backtest) => "the backtesthub (`vike-backend backtesthub --addr`)".to_string(),
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
            Plane::Backtest,
            Some(Plane::Data),
            Scope::Write,
            &data,
        );
        assert!(msg.contains("DATA daemon"), "names what answered: {msg}");
        assert!(msg.contains("DeleteSeries"), "says what the key would have opened: {msg}");
        assert!(msg.contains("config.backtest_addr"), "says where to point: {msg}");
        assert!(msg.contains("Nothing was sent"), "says nothing leaked: {msg}");

        let unknown = wrong_welcome_message("h:1", Plane::Backtest, None, Scope::Write, &[]);
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

    /// `arm_request_timeouts` performs the phase swap: the handshake's 10 s read bound must NOT
    /// survive it (it would abort a legitimate long `RunParamscanProfile`), because each verb arms
    /// its own in `exchange`.
    #[test]
    fn arm_request_timeouts_clears_the_read_and_bounds_the_write() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");
        let client = DatahubClient::on_stream(stream);

        client.arm_request_timeouts().expect("arm the request-phase timeouts");

        assert_eq!(
            client.stream.read_timeout().expect("read_timeout"),
            None,
            "the handshake's bound must not leak into the request phase; each verb arms its own"
        );
        assert_eq!(
            client.stream.write_timeout().expect("write_timeout"),
            Some(REQUEST_WRITE_TIMEOUT)
        );
    }

    /// The default table follows `reply_wait`'s class: a server-bounded verb gets
    /// [`REPLY_IDLE_TIMEOUT`], a client-sized one none, a handshake frame the handshake's bound —
    /// and a caller's `set_reply_timeout` replaces the table for EVERY verb until it is cleared.
    #[test]
    fn the_reply_bound_is_the_verb_class_until_the_caller_names_one() {
        assert_eq!(default_reply_timeout(&Request::Ping), Some(REPLY_IDLE_TIMEOUT));
        assert_eq!(default_reply_timeout(&Request::Inventory), Some(REPLY_IDLE_TIMEOUT));
        let backfill = Request::Backfill {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            interval: "1m".into(),
            start: 0,
            end: 1,
        };
        assert_eq!(default_reply_timeout(&backfill), None, "a client-sized verb stays unbounded");
        assert_eq!(default_reply_timeout(&Request::RunBacktest(String::new())), None);
        assert_eq!(
            default_reply_timeout(&Request::Hello { proto_version: PROTO_VERSION }),
            Some(CLIENT_HANDSHAKE_DEADLINE)
        );

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let stream = connect_bounded(listener.local_addr().expect("addr")).expect("connect");
        let mut client = DatahubClient::on_stream(stream);
        let short = Duration::from_millis(250);
        client.set_reply_timeout(Some(short));
        assert_eq!(client.reply_timeout_for(&Request::Ping), Some(short));
        assert_eq!(client.reply_timeout_for(&backfill), Some(short), "the override covers all");
        client.set_reply_timeout(None);
        assert_eq!(client.reply_timeout_for(&backfill), None, "and clearing it restores the table");
    }

    /// `exchange` ARMS the verb's bound on the socket before it writes, so the bound a verb gets is
    /// the one [`DatahubClient::reply_timeout_for`] reports, not whatever the last verb left.
    #[test]
    fn an_exchange_arms_its_verbs_bound_on_the_socket() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let stream = connect_bounded(listener.local_addr().expect("addr")).expect("connect");
        let mut client = DatahubClient::on_stream(stream);
        client.arm_request_timeouts().expect("request phase");
        let (mut server_side, _) = listener.accept().expect("accept");
        let answer = std::thread::spawn(move || {
            let _: Request = read_frame(&mut server_side).expect("the Ping arrives");
            write_frame(&mut server_side, &Response::Pong).expect("answer it");
            server_side
        });
        client.ping().expect("answered");
        let _server_side = answer.join().expect("server thread");
        assert_eq!(client.stream.read_timeout().expect("read_timeout"), Some(REPLY_IDLE_TIMEOUT));
        assert!(client.is_usable(), "an answered exchange leaves the client in step");
    }

    /// `cancel_handle` closes the connection from ANOTHER thread while the client is blocked in a
    /// verb's reply read: the verb returns an error instead of waiting forever, and the server reads
    /// end-of-stream — the "client gone" a backtesthub watches for. A second `shutdown` is
    /// harmless.
    #[test]
    fn a_cancel_handle_unblocks_the_verb_and_the_server_reads_end_of_stream() {
        use std::io::Read;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");
        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");
        let mut client = DatahubClient::on_stream(stream);
        client.arm_request_timeouts().expect("arm the request-phase timeouts");
        let (mut server_side, _) = listener.accept().expect("accept");
        let handle = client.cancel_handle().expect("clone the socket");

        // The verb writes its frame and then blocks: this server never answers.
        let blocked = std::thread::spawn(move || client.ping());
        let mut frame = [0u8; 4];
        server_side.read_exact(&mut frame).expect("the Ping frame's length prefix arrives");

        handle.shutdown();
        let outcome = blocked.join().expect("the verb thread does not panic");
        assert!(
            outcome.is_err(),
            "the blocked verb fails once the connection is shut: {outcome:?}"
        );

        let mut rest = Vec::new();
        server_side.read_to_end(&mut rest).expect("the server reads to end-of-stream");
        handle.shutdown();
    }
}
