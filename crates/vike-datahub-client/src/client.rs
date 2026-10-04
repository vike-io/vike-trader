//! The blocking datahub client: a thin wrapper over one [`TcpStream`] that speaks the [`crate::proto`]
//! request/response protocol.
//!
//! This is the "thin client" half of the compute-to-data design: it ships a small profile (as TOML
//! text) or a typed read query, and receives a compact answer — it never pulls a raw UPSTREAM data
//! slice across the wire (a read verb returns exactly the query's bounded result). One client owns
//! one connection and issues requests sequentially (request, then read its response); it is not
//! internally synchronized, so share it per thread.
//!
//! # The connect handshake (PR-2)
//!
//! [`connect`](DatahubClient::connect) performs the [`Request::Hello`] / [`Response::Welcome`]
//! version handshake before returning: it sends the client's [`PROTO_VERSION`], reads the server's
//! `Welcome`, and returns `Err` (NAMING BOTH versions) if they disagree — so a stale client fails
//! with a legible message instead of a silent frame desync on the first real request. Because every
//! caller (`vike-cli`, [`RemoteHistStore`](crate::remote::RemoteHistStore)) constructs the client
//! through `connect`, they all inherit the handshake for free. The server's advertised feature list
//! is captured and readable via [`features`](DatahubClient::features).
//!
//! # Authentication (`docs/decisions/0025-datahub-remote-posture.md`)
//!
//! A datahub server is KEYED or key-less, and the client mirrors that in two constructors:
//!
//! - [`connect`](DatahubClient::connect) — the pre-existing one, unchanged against a key-less
//!   server. Against a KEYED one (which advertises [`crate::proto::FEATURE_AUTH`]) it now fails
//!   IMMEDIATELY with an actionable message, instead of connecting fine and having every verb
//!   refused one round trip later.
//! - [`connect_authed`](DatahubClient::connect_authed) — `Hello` → `Welcome{nonce}` →
//!   `Auth{scope, mac}` → `AuthOk`, signing the per-connection nonce with the scope's key. It
//!   degrades to a plain unauthenticated connect against a key-less server (which has nothing to
//!   verify a mac with), so one keyed caller works against both; check
//!   [`authenticated_scope`](DatahubClient::authenticated_scope) when that distinction matters.
//! - [`connect_authed_on`](DatahubClient::connect_authed_on) — the same exchange, refused between
//!   the `Welcome` and the first byte of `Auth` unless the `Welcome` names the plane the caller
//!   asked for ([`crate::proto::welcome_plane`]). One node pair authenticates BOTH daemons, so a
//!   Write key aimed at the wrong port does not fail — it opens a Write session on whichever daemon
//!   answered, and on the data plane that session carries `Backfill` and `DeleteSeries`. A caller
//!   holding a key for ONE plane uses this one.
//!
//! The scope chosen at connect is the connection's ceiling for its whole life. [`Scope::Read`]
//! reads history and catalog; [`Scope::Write`] additionally admits the `Backfill` WRITE and every
//! `Run*` verb — those COMPILE CLIENT-SUPPLIED RHAI on the server, so they are not reads whatever
//! they return. `vike_datahub::server`'s `required_scope` is the authority for that mapping.
//!
//! # Bounding the connect and the handshake
//!
//! The SERVER has bounded itself for a long time (`vike_datahub::server`'s `IDLE_READ_TIMEOUT`
//! since PR-2, #728, and on a keyed server its `HANDSHAKE_DEADLINE`, which landed with the NodeKeys
//! auth of split-plane I11, #1412); the client had NO bound of any kind, which is the asymmetry
//! this section closes. Both constructors used a bare `TcpStream::connect`, so a routable
//! but black-holed datahub host — a box that is up with the daemon down, a firewall that DROPs
//! rather than REJECTs, a stale tunnel whose far end is gone — produced an OS SYN-retry stall
//! nothing in this workspace controlled (minutes on Linux, ~20 s on Windows), and a peer that
//! completed the TCP handshake and then said nothing (a WRONG service on the port, a half-open
//! connection left behind by a suspended laptop) parked the calling thread in `read` forever. That
//! matters more than it looks: every caller here is BLOCKING and single-connection, so an unbounded
//! leg is not one slow client — it is the join that a fan-out over several datahubs still waits out.
//!
//! Three constants do it, and each is armed on a different phase:
//!
//! - [`CONNECT_TIMEOUT`] bounds the TCP handshake, PER RESOLVED ADDRESS (a name can resolve to
//!   several — see [`connect_bounded`]). ⚠ NAME RESOLUTION itself stays unbounded: the walk starts
//!   with `to_socket_addrs`, and the standard library offers no bounded resolver, so a hostname
//!   against a black-holed DNS server still parks the caller before this bound is ever consulted.
//!   Declared, not fixed — closing it means a resolver thread or a literal address, and every
//!   deployment this client has reaches its datahub by loopback, LAN address or tunnel.
//! - [`HANDSHAKE_DEADLINE`] bounds EACH read and EACH write of the `Hello`/`Welcome`
//!   (+ `Auth`/`AuthOk`) exchange — a socket timeout is per syscall, so the whole exchange may take
//!   a few multiples of it on a peer that answers every frame just inside the window. It is the
//!   client mirror of `vike_datahub::server`'s `HANDSHAKE_DEADLINE` and is sized by the same
//!   argument: neither side is COMPUTING anything here, so a handshake read that has not completed
//!   in this window is not slow, it is a socket nobody is speaking the protocol on.
//! - [`REQUEST_WRITE_TIMEOUT`] then replaces the write bound for the connection's life, armed by
//!   [`DatahubClient::arm_request_timeouts`].
//!
//! ⚠ **What this deliberately does NOT do: bound a post-handshake READ.** `arm_request_timeouts`
//! CLEARS the read timeout, and that is the considered choice, not an oversight. Past the handshake
//! a client read is waiting on SERVER-SIDE COMPUTATION — `RunParamscanProfile` over a large grid, a
//! walk-forward over many splits, a `Backfill` fetching a bounded range from a venue's REST — whose
//! legitimate duration has no ceiling this crate could name, and this codebase's own rule for such a
//! value is written on the server's `IDLE_READ_TIMEOUT`: "a value that would clip a slow but live
//! client is worse than a leaked thread". Distinguishing "still computing" from "died mid-request"
//! needs a protocol-level progress or keepalive frame, which [`crate::proto`] does not have; adding
//! one is a `PROTO_VERSION` question, not a socket-option one. So the wait for an ANSWER is exactly
//! as unbounded as it was before — while the two legs that had no business being unbounded, and
//! that are what a dead host actually stalls on, now are.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use vike_data::removal::SeriesSelector;
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
/// Sized for the two deployments this client actually has — a loopback server and a datahub reached
/// over an SSH tunnel or a LAN — where a live peer completes the handshake in milliseconds and the
/// only thing that takes seconds is a host that is not going to answer. It exists to replace the
/// OS SYN-retry default, which is MINUTES on Linux and is what a black-holed host stalls on; it is
/// deliberately not tuned finer than that, because the failure it bounds is binary (the peer
/// answers, or it does not) rather than a matter of a few hundred milliseconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long EACH read and EACH write of the protocol handshake gets — `Hello`/`Welcome` plus, on a
/// keyed server, `Auth`/`AuthOk`. Armed as BOTH the socket read and write timeout for that phase
/// only, then replaced by [`arm_request_timeouts`](DatahubClient::arm_request_timeouts).
///
/// ⚠ A socket timeout is PER SYSCALL, not per phase: a keyed handshake is two frames each way — a
/// sent frame is one bounded `write_all` (`write_frame` writes the prefix and the body together,
/// since 2026-10-03) and a received one is two `read_exact`s, the prefix then the body — so six
/// bounded calls, and a peer that answers each one just inside the window can stretch the whole
/// exchange to about six times this value before anything fires. (A `write_all` the kernel splits
/// into several `write`s re-arms the bound per `write`, but a handshake frame is a few hundred
/// bytes and is never split.) That is
/// accepted — a peer doing that is answering, and the bound exists for the one that does not.
///
/// The client mirror of `vike_datahub::server`'s `HANDSHAKE_DEADLINE`, equal to it and for the same
/// reason read from the other end: neither peer is computing anything during the handshake — the
/// `Welcome` is a version number plus a feature list, the `AuthOk` is one HMAC verification — so a
/// handshake read still unfinished after this window is not a slow server, it is a socket with the
/// wrong thing (or nothing) on the far end. Matching the server's value is not a coupling that must hold:
/// the two bound different waits and either may be tuned alone, which is why this crate names its
/// own constant rather than importing the server's (it could not anyway — vike-datahub depends on
/// THIS crate, so the edge only exists in that direction).
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Write timeout for the connection's life once the handshake is done.
///
/// A request frame is small — kilobytes even for a profile TOML or a Rhai source — so a write that
/// blocks this long is not a large body in flight: it is a peer that has stopped READING, i.e. a
/// server whose connection thread is wedged or gone while the socket stays open. Generous enough
/// that a legitimately busy server draining its receive buffer is never clipped.
///
/// ⚠ A timeout here leaves a PARTIALLY WRITTEN frame on the wire, so the stream is desynced and the
/// client must be dropped rather than reused — the same rule `vike_datahub::server`'s
/// `handle_connection` states for its own read timeouts ("a timeout is NEVER recovered into a
/// resumed loop"). Nothing enforces that here; the verb methods surface the `io::Error` and it is
/// the caller that must not issue a second request on a client that failed to write one. The one
/// caller today that DOES keep going on a single client is `crates/vike-app-core/src/data/backfill_wire.rs`'s
/// `run_wire_backfill` — one connect, then every job × range in a loop that marks a failed range
/// and continues, by design. On a desynced stream the consequence is a CASCADE of failed ranges
/// — each later `Backfill` frame lands mid-frame on the server, whose `handle_connection` either
/// answers `Response::Error` to the garbage it decodes or closes on a read fault, and the report's
/// `first_error` still names the write that started it — never corrupt data: the server folds no
/// half-frame, and every range it did not acknowledge is simply not written. The other callers
/// (`crates/vike-cli/src/cmd/backtest.rs`, `crates/vike-cli/src/cmd/sweep.rs`,
/// `crates/vike-cli/src/cmd/mcp.rs`) construct a fresh
/// client per operation, so the rule cannot bite them. (⚠ `crates/vike-cli/src/cmd/walkforward.rs`
/// was a fourth until decision 3 of the backtest-CLI-surface design folded that verb into
/// `backtest run`; the file still exists but constructs no client at all now — it is the report
/// renderer and nothing else.)
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

// Compile-time bounds on the three, the idiom of `vike_datahub::server` and of
// `crates/vike-tradehub/src/telegram/confirm.rs`: a RANGE, so a deliberate tweak stays free while
// "the bound was effectively removed" and "the bound refuses every legitimate connect" both fail to
// compile instead of shipping.
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
/// `TcpStream::connect` walks every resolved address itself; `connect_timeout` takes ONE
/// `SocketAddr`, so the walk has to be written out here — a name like `datahub.internal:7878`
/// routinely resolves to both an A and an AAAA record, and a client that tried only the first would
/// fail against a dual-stack host whose IPv6 route is dark. The budget is therefore PER ADDRESS, as
/// the standard library's own walk is: N candidates can cost up to N × [`CONNECT_TIMEOUT`], which is
/// the price of not giving up on a reachable host to keep one number tidy.
///
/// The LAST error is returned when every candidate fails (the first is the least interesting — it
/// is usually the dead IPv6 leg), and an address that resolves to nothing at all is its own
/// [`io::ErrorKind::InvalidInput`] rather than a confusing "connection refused" borrowed from
/// somewhere else.
fn connect_bounded<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    let mut last_err: Option<io::Error> = None;
    for candidate in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&candidate, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(HANDSHAKE_DEADLINE))?;
                stream.set_write_timeout(Some(HANDSHAKE_DEADLINE))?;
                // Nagle OFF on the dialled end too — each end's option governs only that end's
                // sends (`vike_node_proto::frame::configure_node_stream`). Unlike the two timeouts
                // above, a refusal is not worth failing the connect over: it costs latency, never
                // correctness, and this crate carries no logger to report it through.
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
    /// [`Request::Hello`] / [`Response::Welcome`] version handshake (PR-2).
    ///
    /// Returns `Err` on a transport failure OR a protocol-version mismatch — the latter an
    /// [`io::ErrorKind::InvalidData`] whose message NAMES BOTH versions (client and server), so a
    /// stale binary fails legibly rather than desyncing on its first request.
    ///
    /// ⚠ Against a KEYED server (one advertising [`FEATURE_AUTH`]) this fails HERE, with an
    /// actionable message, rather than succeeding and having every verb refused later. Use
    /// [`connect_authed`](Self::connect_authed) for those. Against a key-less server — the default,
    /// and every deployment before authentication existed — behaviour is completely unchanged,
    /// which is what keeps `vike-cli backtest`, the Studio's `Backend::Remote` and
    /// [`RemoteHistStore`](crate::remote::RemoteHistStore) working untouched.
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
    /// computed by [`vike_node_proto::auth::sign`] over the DATAHUB domain separator —
    /// so the tag binds the key, the scope, the protocol version AND this one connection's nonce,
    /// and cannot be replayed onto another connection or against the tradehub node.
    ///
    /// ⚠ **A KEY-LESS server is not an error here — it is a successful unauthenticated connect.**
    /// A server that advertises no [`FEATURE_AUTH`] has no key to verify a mac against, so sending
    /// `Auth` would earn a refusal for doing the right thing; the client detects the absent
    /// advertisement, sends nothing, and returns a working connection with
    /// [`authenticated_scope`](Self::authenticated_scope) `== None`. That is what lets ONE caller
    /// (a GUI, `vike-cli`) hold keys and still work against a local key-less dev server without
    /// branching. A caller that must be sure it authenticated checks `authenticated_scope`.
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
    /// # Why a Write key needs this and a Read key did not
    ///
    /// One node pair authenticates BOTH daemons (`crates/vike-backtest/src/compute_server.rs`'s
    /// `serve_authed` verifies the datahub's own keys under the datahub's own domain separator), so
    /// a CONTROL key signed toward the wrong port does not fail — it SUCCEEDS, and opens a
    /// `VerbScope::Write` session on whichever daemon answered. On the compute plane that scope
    /// runs strategies; on the DATA plane it also admits `Backfill` (a store write that spends the
    /// venue budget over a range the client names) and `DeleteSeries` (the destructive verb). A
    /// caller that holds that key for the compute plane alone therefore has to know which daemon
    /// it reached before it signs, and the `Welcome` is the one frame both send before auth.
    ///
    /// It is the address regression `crates/vike-desktop/src/main.rs` records — Studio's compute
    /// address overwritten with the datahub's — and an operator typing the datahub's port into the
    /// Backend field, both closed at the one place either could spend the key.
    ///
    /// # What it checks, and why the check is unconditional
    ///
    /// [`crate::proto::welcome_plane`] must answer exactly `plane`: a pre-split daemon (both planes
    /// in one process) and an unknown peer are refused as well as the wrong plane. The check runs
    /// on a KEY-LESS server too, although nothing would be signed there: the caller asked for one
    /// daemon, and a legible "this is the data daemon" at connect beats a wrong-plane `Error` one
    /// frame later.
    ///
    /// ⚠ What it cannot see: a peer that LIES in its `Welcome`. The feature list is unauthenticated
    /// by construction (it precedes the nonce), so this defends against a MISDIRECTED client, not a
    /// hostile server — and a hostile server holding this key's other end could not be defended
    /// against by anything a client does.
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
            // Key-less server: nothing to authenticate against. See the ⚠ above. The handshake is
            // over either way, so the request-phase timeouts are armed on this arm too.
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
    /// [`REQUEST_WRITE_TIMEOUT`] and the READ timeout is CLEARED.
    ///
    /// Clearing the read is the deliberate half — the module doc's ⚠ carries the argument, and the
    /// short version is that a post-handshake read waits on server-side COMPUTATION whose legitimate
    /// duration this crate cannot name, so any ceiling put here would eventually abort a healthy
    /// sweep. The connection is therefore no less patient after this call than it was before the
    /// bounds existed; what the bounds buy is the CONNECT and the HANDSHAKE, which are the two legs
    /// a dead host actually stalls on.
    ///
    /// The `Err` is a failed `setsockopt`, not a protocol failure. It is propagated rather than
    /// ignored because a client that silently kept [`HANDSHAKE_DEADLINE`] as its read timeout would
    /// abort every legitimate long-running verb after ten seconds — a far more confusing failure
    /// than refusing to hand back the connection.
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
    /// [`FEATURE_AUTH`]. The capability-negotiation half of the no-version-bump design: a client
    /// learns this from the `Welcome` rather than from a protocol number.
    pub fn requires_auth(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_AUTH)
    }

    /// The [`Scope`] this connection authenticated under, or `None` on a key-less server (where no
    /// authentication took place, and every verb EXCEPT the destructive
    /// [`Request::DeleteSeries`](crate::proto::Request::DeleteSeries) is served). A caller that
    /// must be certain it is talking to an authenticated connection checks this, not merely that
    /// [`connect_authed`](Self::connect_authed) returned `Ok`.
    ///
    /// ⚠ The parenthesis read "and every verb is served" until 2026-09-07, and it was true when
    /// written. [`FEATURE_DELETE_SERIES`](crate::proto::FEATURE_DELETE_SERIES) is what made it
    /// false: a key-less server neither advertises that verb nor answers it, so `None` here is now
    /// also the answer to "may this connection delete anything" — no.
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

// `DatahubClient`'s one `impl` is split by concern into the child modules below; this file keeps
// the module doc, the bounds, the struct, the connect/handshake constructors and the helpers they
// share.
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
/// ⚠ The message names the spec's POSITION in the list and its venue and lane, never the symbol.
/// The whole point of the rule is that the symbol is the unbounded field, and a client that echoed
/// it into an error a caller then logs has moved the cost rather than refused it — the reason
/// `validate_md_symbol` states in full.
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
///
/// Split out as a pure function so the version-compare contract is unit-testable without a socket;
/// the `handshake` wraps the `Err` message into an [`io::ErrorKind::InvalidData`] error.
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
/// It says three things an operator acts on: which daemon ANSWERED (or that the answer named none),
/// what the key would have opened there, and which command serves the plane they wanted and which
/// setting points at it. It names no key byte and no key name; the scope is enough to say what was
/// at stake.
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

    /// The stream `connect_bounded` hands back is ALREADY bounded — both directions, at
    /// [`HANDSHAKE_DEADLINE`] — so the `Hello`/`Welcome` exchange that runs on it next cannot park
    /// the calling thread on a peer that accepted the socket and then said nothing. Proven against a
    /// real listener that is deliberately never accepted from: the connect completes regardless (the
    /// kernel's backlog does that), which is exactly the shape a wrong-service-on-the-port peer has.
    #[test]
    fn connect_bounded_arms_the_handshake_deadline_both_ways() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");

        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");

        assert_eq!(stream.read_timeout().expect("read_timeout"), Some(HANDSHAKE_DEADLINE));
        assert_eq!(stream.write_timeout().expect("write_timeout"), Some(HANDSHAKE_DEADLINE));
    }

    /// The dialled stream carries `TCP_NODELAY` (`vike_node_proto::frame::configure_node_stream`),
    /// so a request written while an earlier one is unacknowledged — or the tail of one larger than a
    /// segment — is not held for the server's delayed ACK. Every `DatahubClient` constructor and
    /// every caller of them (the GUI's market-data session, `RemoteHistStore`, `vike-cli`) dials
    /// through `connect_bounded`, so this one socket option is all of them.
    #[test]
    fn connect_bounded_turns_nagle_off() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local_addr");

        let stream = connect_bounded(addr).expect("connect to a bound loopback listener");

        assert!(stream.nodelay().expect("read TCP_NODELAY"), "the dialled stream has Nagle on");
    }

    /// `arm_request_timeouts` performs the phase swap the module doc describes, and the CLEARED read
    /// is asserted as loudly as the armed write: a future change that "tidied" the `None` into a
    /// concrete ceiling would abort a legitimate long `RunParamscanProfile`, so it has to fail here
    /// rather than in production a grid later.
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

    /// An address that resolves to NOTHING gets its own [`io::ErrorKind::InvalidInput`] rather than
    /// a borrowed "connection refused" from a candidate that was never tried — the walk in
    /// `connect_bounded` has no `last_err` to report in that case, and the fallback must say so.
    #[test]
    fn connect_bounded_reports_an_address_that_resolves_to_nothing() {
        // An empty slice of `SocketAddr` is a `ToSocketAddrs` impl that yields no candidates — the
        // resolution-succeeded-but-produced-nothing case, without needing a DNS lookup in a test.
        let empty: &[std::net::SocketAddr] = &[];
        let err = connect_bounded(empty).expect_err("no candidates must not be an Ok connection");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("no socket addresses"), "legible message: {err}");
    }
}
