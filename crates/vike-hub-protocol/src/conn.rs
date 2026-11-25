//! The blocking client half of the keyed data-plane handshake, and the one exchange every positional
//! verb goes through: `vike_datahub_client::DatahubClient` and `vike_backtesthub_client::BacktesthubClient`
//! are a [`HubConnection`] with their verbs on top, so the two hubs' clients cannot disagree about how a
//! connection opens or how a reply is bounded. Nothing here knows a verb: a request is whatever
//! `Serialize` the caller hands [`HubConnection::exchange_frame`], with its name and default bound.
//!
//! # The connect handshake
//!
//! Every constructor performs the `Hello` / `Welcome` version handshake before returning and fails,
//! NAMING BOTH versions, when they disagree: a stale client fails legibly instead of desyncing on its
//! first request. ⚠ The DAEMON is identified first: a `Welcome` naming the other plane, at another
//! version, is refused as a [`WrongDaemon`] that says which daemon answered, never as a bare version
//! skew (the two data-plane daemons' numbers differ). The server's advertised feature list is readable via [`HubConnection::features`]; a verb
//! added after protocol 7 is negotiated through it, never by a version bump
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//!
//! # Authentication (`docs/decisions/0025-datahub-remote-posture.md`)
//!
//! - [`connect`](HubConnection::connect): key-less. Against a KEYED server (one advertising [`FEATURE_AUTH`])
//!   it fails IMMEDIATELY with an actionable message, instead of connecting and having every verb
//!   refused one round trip later.
//! - [`connect_authed`](HubConnection::connect_authed): `Hello` -> `Welcome{nonce}` -> `Auth{scope, mac}` ->
//!   `AuthOk`, signing the per-connection nonce with the scope's key. It degrades to a plain
//!   unauthenticated connect against a key-less server, so one keyed caller works against both; check
//!   [`authenticated_scope`](HubConnection::authenticated_scope) when that distinction matters.
//! - [`connect_authed_on`](HubConnection::connect_authed_on): the same, refused before the first byte of
//!   `Auth` unless the `Welcome` names the plane the caller asked for ([`welcome_plane`]). A caller
//!   holding a key for ONE plane uses this one.
//! - [`connect_with_keys`](HubConnection::connect_with_keys): `connect_authed` when the caller holds keys,
//!   `connect` when it does not: the one spelling of that choice.
//!
//! The scope chosen at connect is the connection's ceiling for its whole life. `Scope::Read` reads
//! history and catalog; `Scope::Write` additionally admits the `Backfill` WRITE and every `Run*` verb
//! that can carry source: those COMPILE CLIENT-SUPPLIED RHAI on the server, so they are not reads
//! whatever they return. Each client's `proto`'s `required_scope` is the authority for its verbs.
//!
//! # Bounding the connect and the handshake
//!
//! A routable but black-holed host (daemon down, a firewall that DROPs, a dead tunnel) stalls an
//! unbounded `TcpStream::connect` for the OS SYN-retry time, and a peer that accepts and then says
//! nothing (a wrong service on the port, a half-open connection) parks a `read` forever. Every caller
//! here is BLOCKING and single-connection, so an unbounded leg is the join a fan-out over several hubs
//! waits out. Three constants bound it, each armed on a different phase:
//!
//! - `CLIENT_CONNECT_TIMEOUT` bounds the TCP handshake, PER RESOLVED ADDRESS ([`connect_bounded`]).
//!   ⚠ NAME RESOLUTION itself stays unbounded: the standard library has no bounded `to_socket_addrs`,
//!   so a hostname against a black-holed DNS server still parks the caller. Declared, not fixed: every
//!   deployment reaches its hub by loopback, LAN address or tunnel.
//! - `CLIENT_HANDSHAKE_DEADLINE` bounds EACH read and EACH write of the `Hello`/`Welcome` (+
//!   `Auth`/`AuthOk`) exchange.
//! - [`REQUEST_WRITE_TIMEOUT`] then replaces the write bound for the connection's life, armed by
//!   `HubConnection::arm_request_timeouts`.
//!
//! # Bounding a reply: an IDLE bound per verb
//!
//! Past the handshake every verb goes through ONE exchange ([`HubConnection::exchange_frame`]), which arms the
//! socket's read timeout for THAT verb before writing it. A socket read timeout is per syscall, so it
//! is an IDLE bound (no byte for that long), never a total deadline: a large reply streaming steadily is
//! never clipped, and a peer that vanished without a FIN or an RST (a suspended laptop, a dropped
//! tunnel, a NAT eviction) produces no byte and is.
//!
//! The bound comes from [`ReplyWait::default_bound`], keyed on each client's exhaustive `reply_wait`:
//! [`REPLY_IDLE_TIMEOUT`] for a verb whose work the SERVER bounds, NONE for one whose size the CLIENT
//! names (`Backfill`, `ImportArchive`, every `Run*`). Those stay unbounded on purpose: the wire has no
//! progress frame, so their reply is silent until the work is done, and a bound that clips a slow but
//! live one is worse than a parked thread. A caller that knows its scale replaces the table for its
//! client with [`set_reply_timeout`](HubConnection::set_reply_timeout) (the desktop's options poll asks for
//! seconds), and a caller that must abandon an unbounded verb holds a
//! [`CancelHandle`](HubConnection::cancel_handle).
//!
//! ⚠ **A failed exchange makes the client UNUSABLE.** A timeout or any transport error mid-frame leaves
//! the stream desynced, so the exchange records the failure and every later verb on that client is
//! refused LOCALLY as [`HubCallError::NotSent`] (kind [`io::ErrorKind::NotConnected`]), naming the first
//! failure. A server's `Response::Error` is not a transport failure and leaves the client usable. A
//! timed-out read is [`HubCallError::OutcomeUnknown`] of kind [`io::ErrorKind::TimedOut`] on every
//! platform (the socket layer answers `WouldBlock` on unix and `TimedOut` on windows), naming the verb
//! and the bound.
//!
//! # What a failure DID ([`HubCallError`])
//!
//! Every constructor and every exchange here returns a [`HubCallError`], its variant chosen by the PHASE
//! that failed, which only this connection knows: a dial or a handshake that broke before any request is
//! [`HubCallError::NotSent`] whatever the OS said (a firewall's `EACCES` on `connect` included); the
//! hub's `AuthDenied`, or a keyed hub and no key, is [`HubCallError::AccessDenied`]; a `Welcome` this
//! client cannot speak to (another version or plane, no nonce on a keyed hub, not a hub's frame) is
//! [`HubCallError::Protocol`]; a request refused before its first byte is `Protocol` too and keeps the
//! connection; anything after the request began to leave is [`HubCallError::OutcomeUnknown`].
//!
//! ⚠ **The words name the hub that ANSWERED, off its `Welcome`** ([`hub_word`]), never the client's own
//! name: a datahub client pointed at the backtesthub's port is talking to a backtesthub. "hub" before a
//! `Welcome`, and for one that states no single plane.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::access::{FEATURE_PERSON_AUTH, Signer};
use crate::auth::{BACKTESTHUB_DOMAIN, DATAHUB_DOMAIN};
use crate::auth::{Domain, FEATURE_AUTH, HubKeys, Scope};
use crate::call::HubCallError;
use crate::frame::{
    BodyCodec, connect_bounded, decode_body, read_frame_raw, write_frame, write_frame_as,
};
use crate::handshake::{HandshakeFrame, HandshakeReply, auth_answer, reads_answer};
#[cfg(doc)]
use crate::limits::CLIENT_HANDSHAKE_DEADLINE;
use crate::plane::{Plane, frame_tag, welcome_plane};

/// Write timeout for the connection's life once the handshake is done.
///
/// A request frame is kilobytes even for a profile TOML or a Rhai source, so a write blocked this long
/// is a peer that has stopped READING (its connection thread wedged or gone on an open socket); generous
/// enough never to clip a busy server draining its receive buffer.
///
/// ⚠ A timeout here leaves a PARTIALLY WRITTEN frame on the wire: the stream is desynced (the rule
/// `vike_datahub::server`'s `handle_connection` states for its own read timeouts), so
/// [`HubConnection::exchange_frame`] marks the client unusable and refuses every later verb locally. The one
/// caller that keeps going on one client by design is
/// `crates/vike-app-core/src/data/backfill/mod.rs`'s `run_datahub_backfill`: its later ranges are refused
/// without a frame, and the report's `first_error` names the write that started it.
pub const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The idle read bound of a verb whose work the SERVER bounds ([`ReplyWait::Bounded`]): the longest a
/// reply may go without a byte before the link is called dead.
///
/// Set above the longest LEGITIMATE silence found in those verbs, with headroom. Store reads are
/// manifest walks or ceiling-capped scans (`inventory` and `coverage` measured at about 1 s end-to-end,
/// `crates/vike-app-core/src/data/data_manager/mod.rs`'s module doc). The slow ones call a venue, and each
/// venue request is capped by `vike_transport::http`'s 30 s global timeout: `SeedSeries` pages its
/// `SEED_BARS` window in at most six paced requests (OKX) and the deribit catalog is five sequential
/// requests, so their honest worst case is about 150 to 180 s of silence. 300 s clears that by more than
/// 1.5x.
///
/// ⚠ It happens to equal the server's default idle read timeout
/// ([`crate::limits::SERVER_IDLE_READ_TIMEOUT`]) and is NOT tied to it, in either direction: this is how
/// long a CLIENT waits for a reply, that is how long a SERVER waits for the next request, and the
/// server's is a row an operator may set (`config.node_idle_read_timeout_ms`). A server that reaps an
/// idle connection does not touch a reply this client is waiting for (the server reads nothing while it
/// works), and a reply bound shorter or longer than the server's changes nothing about when the server
/// reaps.
pub const REPLY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

// Compile-time RANGES on the two request-phase bounds: a deliberate tweak stays free, while a removed
// bound fails to compile instead of shipping. The connect and handshake bounds carry theirs in
// `crate::limits`.
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

/// How long a verb's reply may legitimately stay SILENT: the class each hub client's exhaustive
/// `reply_wait` gives a verb, and that client turns into the socket's idle read bound for that one
/// exchange. A CLIENT policy, not a wire fact: no frame carries it and no daemon reads it, so it moves
/// without a protocol bump or a feature string. ONE class for every hub client: the datahub's and the
/// backtesthub's ([`ReplyWait::default_bound`] is their table) and the gatewayhub's, which keeps its own
/// table over these classes (`vike_gatewayhub_client::client::GatewayTimings`).
///
/// The data plane has no progress or keepalive frame, so a reply there is silent from the request's
/// last byte until the server has finished the work. The class therefore says WHO bounds that work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyWait {
    /// A pure LOOKUP: a table read, a presence word, a counter snapshot, an open acknowledgement. The
    /// daemon touches no venue to answer, so the idle bound is short and a wedged daemon is called dead
    /// in seconds. Only the gatewayhub's client classes a verb so (`LOOKUP_REPLY_TIMEOUT` there).
    Lookup,
    /// The SERVER bounds the work before the first reply byte: an answer from its memory, a store read
    /// under its row ceilings, a manifest walk, or a venue fetch whose size the server fixes (the seed's
    /// window, a catalog's request set, one REST call at the bridges' own HTTP timeout). A finite idle
    /// bound is safe.
    Bounded,
    /// A PROGRESS-framed reply (the gatewayhub's fetch family): the idle bound is per frame and per
    /// request, `max(FETCH_STALL_FLOOR, 3 × FetchProgress.next_in_ms)`, so a fetch that keeps emitting
    /// frames is never clipped and a vanished server is still called dead. Only the gatewayhub's
    /// client classes a verb so, and reads such a reply through its own `fetch`.
    Progress,
    /// The CLIENT names the work's size (a range, a grid, a script, a month of archive days) and the
    /// server writes nothing until it is done, so no ceiling this crate could name fits it. Unbounded by
    /// default; a caller that knows its scale sets its own bound, and a caller that must abandon one
    /// holds a [`CancelHandle`].
    Unbounded,
}

impl ReplyWait {
    /// The default idle read bound of this class on a [`HubConnection`]: the table behind
    /// [`HubConnection::exchange_frame`]'s `default_bound`, derived from each data-plane client's exhaustive
    /// `reply_wait` so a new verb cannot get a value by accident.
    ///
    /// No data-plane verb is a `Lookup` or a `Progress` (the gatewayhub's client has its own table):
    /// a lookup is a server-bounded verb, so it gets the server-bounded bound here, and a progress-framed
    /// reply has no single idle bound before its first frame, so it gets none, the crate's choice for a
    /// silence it cannot size (a parked thread over a clipped live reply).
    pub fn default_bound(self) -> Option<Duration> {
        match self {
            ReplyWait::Lookup | ReplyWait::Bounded => Some(REPLY_IDLE_TIMEOUT),
            ReplyWait::Progress | ReplyWait::Unbounded => None,
        }
    }
}

/// A connected hub client: one [`TcpStream`]. One connection, requests issued sequentially, not
/// internally synchronized, so share it per thread. Drop it to close the connection.
#[derive(Debug)]
pub struct HubConnection {
    stream: TcpStream,
    /// The server's advertised feature strings, captured during the handshake. Empty before/without a
    /// successful `Welcome`.
    features: Vec<String>,
    /// The scope this connection authenticated under, or `None` against a key-less server (which
    /// requires and offers no authentication).
    authed: Option<Scope>,
    /// Who the hub's access list admitted, when a device authenticated and the hub says so
    /// ([`crate::handshake::Admitted`]); `None` for a shared key or an older hub. Boxed: three
    /// strings inline made every client that holds a connection (and returns itself in an `Err`)
    /// 72 bytes larger.
    admitted: Option<Box<crate::handshake::Admitted>>,
    /// The caller's idle reply bound for EVERY verb, or `None` for the per-verb table. See
    /// [`set_reply_timeout`](Self::set_reply_timeout).
    reply_override: Option<Duration>,
    /// Why this client may send nothing more: the first exchange that failed mid-frame. `None` while
    /// the stream is in step. See [`exchange_frame`](Self::exchange_frame).
    unusable: Option<String>,
    /// The codec every request after the handshake is written in ([`set_body_codec`](Self::set_body_codec)); a
    /// reply is read in whichever codec it came.
    codec: BodyCodec,
}

impl HubConnection {
    /// Connect to a hub at `addr` (e.g. `"127.0.0.1:7878"`) and perform the `Hello` / `Welcome` version
    /// handshake, speaking `proto_version`, as a client of `plane`.
    ///
    /// Returns `Err` on a transport failure OR a protocol-version mismatch: the latter a
    /// [`HubCallError::Protocol`] whose message NAMES BOTH versions (client and server), or, when
    /// the `Welcome` names the OTHER plane, a [`WrongDaemon`] naming the daemon that answered.
    ///
    /// ⚠ Against a KEYED server (one advertising [`FEATURE_AUTH`]) this fails HERE, with an actionable
    /// message ([`HubCallError::AccessDenied`]), rather than succeeding and having every verb refused
    /// later. Use [`connect_authed`](Self::connect_authed) for those.
    pub fn connect<A: ToSocketAddrs>(
        addr: A,
        plane: Plane,
        proto_version: u32,
    ) -> Result<Self, HubCallError> {
        Self::connect_key_less(addr, Some(plane), proto_version)
    }

    /// [`connect`](Self::connect)'s body, for a client that knows its plane (`wanted`) or, signing
    /// under a separator of no data-plane hub, does not.
    fn connect_key_less<A: ToSocketAddrs>(
        addr: A,
        wanted: Option<Plane>,
        proto_version: u32,
    ) -> Result<Self, HubCallError> {
        let stream = connect_bounded(addr).map_err(HubCallError::NotSent)?;
        let mut client = Self::on_stream(stream);
        client.handshake(proto_version, wanted, None)?;
        if client.requires_auth() {
            let hub = hub_word(&client.features);
            let loopback = client.stream.peer_addr().is_ok_and(|a| a.ip().is_loopback());
            return Err(HubCallError::AccessDenied(format!(
                "this {hub} REQUIRES authentication (it advertises the `auth` feature) but this \
                 connection carries no key. {} (decision 0288)",
                no_device_key_cure(loopback)
            )));
        }
        client.finish_handshake()?;
        Ok(client)
    }

    /// Connect and AUTHENTICATE under `scope`, signing the server's per-connection nonce challenge with
    /// the matching key from `keys` (`docs/decisions/0025-datahub-remote-posture.md`).
    ///
    /// The exchange is `Hello` -> `Welcome{nonce}` -> `Auth{scope, mac}` -> `AuthOk`, with the mac
    /// computed by [`crate::auth::sign`] over `domain`, the separator of the hub the CALLER means to reach
    /// ([`DATAHUB_DOMAIN`] or [`BACKTESTHUB_DOMAIN`], never one read off the peer's `Welcome`), so
    /// the tag binds the key, the scope, the protocol version, the hub AND this connection's nonce, and
    /// cannot be replayed onto another connection or against another hub.
    ///
    /// ⚠ **A KEY-LESS server is not an error here: it is a successful unauthenticated connect.** A server
    /// that advertises no [`FEATURE_AUTH`] has no key to verify a mac against, so the client sends nothing
    /// and returns a working connection with [`authenticated_scope`](Self::authenticated_scope) `== None`.
    /// That lets ONE caller hold keys and still work against a local key-less dev server; a caller that
    /// must be sure it authenticated checks `authenticated_scope`.
    ///
    /// Fails with [`HubCallError::AccessDenied`] on a refused mac: a wrong key, a key this server
    /// does not hold for that scope, or a protocol-version skew (the version is signed).
    pub fn connect_authed<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        domain: Domain,
        proto_version: u32,
    ) -> Result<Self, HubCallError> {
        Self::connect_authed_observed(addr, keys, scope, None, domain, proto_version, |_| {})
    }

    /// [`connect_authed`](Self::connect_authed), but only to a server whose PRE-AUTH `Welcome` says it
    /// serves `plane`: checked after `Hello`/`Welcome` and BEFORE a single byte of `Auth` is written. On
    /// any other answer it refuses with [`HubCallError::Protocol`], naming the plane it found and the
    /// command that serves the one it wanted, and the peer has received nothing but the `Hello`.
    ///
    /// Why a Write key needs this: one device key is on the access list of BOTH daemons, so a caller
    /// dialling the wrong port names the wrong plane long before anything else would. Its signature no
    /// longer opens the other daemon (each signs under its own `domain` since decision 0288's amendment),
    /// but this guard refuses before a byte of it is sent, and says which daemon answered. The `Welcome`
    /// is the one frame both daemons send before auth.
    ///
    /// [`welcome_plane`] must answer exactly `plane`: a pre-split daemon and an unknown peer are refused as
    /// well as the wrong plane, and the check runs on a KEY-LESS server too (a legible "this is the data
    /// daemon" at connect beats a wrong-plane `Error` later).
    ///
    /// ⚠ It cannot see a peer that LIES in its `Welcome` (the feature list precedes the nonce): it defends
    /// against a MISDIRECTED client, not a hostile server.
    pub fn connect_authed_on<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        plane: Plane,
        domain: Domain,
        proto_version: u32,
    ) -> Result<Self, HubCallError> {
        Self::connect_authed_observed(addr, keys, scope, Some(plane), domain, proto_version, |_| {})
    }

    /// The dial of a caller that MAY hold keys: [`connect_authed`](Self::connect_authed) under `scope`
    /// when `keys` is `Some`, plain [`connect`](Self::connect) when it is `None` (`scope` is then unused:
    /// a key-less dial negotiates no scope).
    ///
    /// Each arm keeps its own constructor's contract exactly: `Some` against a KEY-LESS server is a clean
    /// unauthenticated connect (`connect_authed`'s ⚠), and `None` against a KEYED server fails at the
    /// handshake naming the missing keys (`connect`'s ⚠). There is no third branch: no fallback from a
    /// refused key to an unauthenticated dial.
    ///
    /// ⚠ It is UNGUARDED on the plane, like `connect_authed`: a caller holding a key for ONE plane dials
    /// [`connect_authed_on`](Self::connect_authed_on) instead.
    pub fn connect_with_keys<A: ToSocketAddrs>(
        addr: A,
        keys: Option<&HubKeys>,
        scope: Scope,
        domain: Domain,
        proto_version: u32,
    ) -> Result<Self, HubCallError> {
        match keys {
            Some(k) => Self::connect_authed(addr, k, scope, domain, proto_version),
            None => Self::connect_key_less(addr, plane_of_domain(domain), proto_version),
        }
    }

    /// The one body the keyed constructors share. `expect` is `None` for the unguarded
    /// [`connect_authed`](Self::connect_authed); when it is `Some`, the plane is checked between the
    /// `Welcome` and the first byte of `Auth`, and every refusal returns before anything is written.
    /// `observe` is handed the freshly dialled socket BEFORE the handshake reads from it (the strategy
    /// lane's long-lived owner stores a clone so a shutdown can break a handshake parked on a peer that
    /// accepted and went quiet); the other callers pass `|_| {}`.
    pub fn connect_authed_observed<A: ToSocketAddrs>(
        addr: A,
        keys: &HubKeys,
        scope: Scope,
        expect: Option<Plane>,
        domain: Domain,
        proto_version: u32,
        observe: impl FnOnce(&TcpStream),
    ) -> Result<Self, HubCallError> {
        let stream = connect_bounded(addr).map_err(HubCallError::NotSent)?;
        observe(&stream);
        let mut client = Self::on_stream(stream);
        // The plane this client was built for: the guard's, else the one its `domain` signs for.
        let wanted = expect.or_else(|| plane_of_domain(domain));
        let nonce = client.handshake(proto_version, wanted, expect.map(|plane| (plane, scope)))?;
        if !client.requires_auth() {
            // Key-less server: nothing to authenticate against (`connect_authed`'s ⚠). The handshake
            // is over, so the request-phase timeouts are armed on this arm too.
            client.finish_handshake()?;
            return Ok(client);
        }
        let hub = hub_word(&client.features);
        let Some(nonce) = nonce else {
            return Err(HubCallError::Protocol(
                format!(
                    "{hub} handshake: server advertises `auth` but its Welcome carried no nonce"
                )
                .into(),
            ));
        };
        // A device key signs `PersonAuth` where the hub advertises it (decision 0288); a hub that does not is
        // answered with the shared key (a daemon's or a test's: no person holds one), and a device-only client
        // holding none refuses in `auth_answer`, having sent no answer.
        let person_auth = client.features.iter().any(|f| f == FEATURE_PERSON_AUTH);
        let signer = match keys.device() {
            Some(device) if person_auth || !keys.has(scope) => Signer::Device(device),
            _ => Signer::Shared(keys.key_for(scope)),
        };
        let frame = auth_answer(signer, domain, &nonce, proto_version, scope, &client.features)?;
        write_frame(&mut client.stream, &frame).map_err(HubCallError::NotSent)?;
        match client.read_reply()? {
            Ok(HandshakeReply::AuthOk { scope: granted, admitted }) => {
                client.authed = Some(granted);
                client.admitted = admitted.map(Box::new);
                client.finish_handshake()?;
                Ok(client)
            }
            Ok(HandshakeReply::AuthDenied { reason }) => {
                Err(HubCallError::AccessDenied(format!("{hub} auth denied ({scope:?}): {reason}")))
            }
            Ok(HandshakeReply::Welcome { .. }) => Err(not_the_auth_answer(hub, "Welcome")),
            Ok(HandshakeReply::Registered(_)) => Err(not_the_auth_answer(hub, "Registered")),
            Err(tag) => Err(not_the_auth_answer(hub, &tag)),
        }
    }

    /// Open the connection with the version handshake: send `proto_version` in a `Hello`, read the
    /// server's `Welcome` and store its advertised `features`, then judge it: fail (naming both
    /// versions) on a mismatch.
    ///
    /// ⚠ **The daemon is identified BEFORE the version is compared.** The data-plane daemons' numbers
    /// differ (the backtesthub's 9 against the datahub's 8), so a client dialling the other one would
    /// otherwise read a version skew where the fact is a wrong address. `guard`
    /// ([`connect_authed_on`](Self::connect_authed_on)'s plane, with the scope its refusal names)
    /// refuses any `Welcome` that does not name exactly that plane; `wanted` (the plane the client was
    /// built for) refuses, as a [`WrongDaemon`], a `Welcome` naming the OTHER plane when the versions
    /// also differ. At equal versions such an unguarded dial still connects and each verb is refused by
    /// name (`crate::plane::wrong_plane_message`), as it always was.
    ///
    /// Returns the server's auth `nonce`: `Some` on a KEYED server, `None` on a key-less one.
    fn handshake(
        &mut self,
        proto_version: u32,
        wanted: Option<Plane>,
        guard: Option<(Plane, Scope)>,
    ) -> Result<Option<[u8; 32]>, HubCallError> {
        write_frame(&mut self.stream, &HandshakeFrame::Hello { proto_version })
            .map_err(HubCallError::NotSent)?;
        let (theirs, nonce) = match self.read_reply()? {
            Ok(HandshakeReply::Welcome { proto_version: theirs, features, nonce }) => {
                self.features = features;
                (theirs, nonce)
            }
            Ok(HandshakeReply::AuthOk { .. }) => return Err(not_the_welcome("AuthOk")),
            Ok(HandshakeReply::AuthDenied { .. }) => return Err(not_the_welcome("AuthDenied")),
            Ok(HandshakeReply::Registered(_)) => return Err(not_the_welcome("Registered")),
            Err(tag) => return Err(not_the_welcome(&tag)),
        };
        let found = welcome_plane(&self.features);
        if let Some((plane, scope)) = guard
            && found != Some(plane)
        {
            return Err(HubCallError::Protocol(
                wrong_welcome_message(&self.peer(), plane, found, scope, &self.features).into(),
            ));
        }
        if theirs != proto_version
            && let (Some(wanted), Some(found)) = (wanted, found)
            && found != wanted
        {
            let message = wrong_daemon_message(&self.peer(), wanted, found, proto_version, theirs);
            return Err(HubCallError::Protocol(Box::new(WrongDaemon { found, wanted, message })));
        }
        check_proto_version(hub_word(&self.features), proto_version, theirs)
            .map_err(|msg| HubCallError::Protocol(msg.into()))?;
        Ok(nonce)
    }

    /// The peer's address as a refusal names it.
    fn peer(&self) -> String {
        self.stream.peer_addr().map_or_else(|_| "the server".to_string(), |a| a.to_string())
    }

    /// One handshake reply. `Ok(Err(tag))` is a well-formed frame of some OTHER verb's reply (a peer
    /// speaking the protocol out of order), named by its wire tag; a body that is neither, or a length
    /// over the frame ceiling (the codec's `InvalidData`), is a peer that is not a hub
    /// ([`HubCallError::Protocol`]); a read that broke is [`HubCallError::NotSent`], since no request
    /// has left yet.
    fn read_reply(&mut self) -> Result<Result<HandshakeReply, String>, HubCallError> {
        let body = read_frame_raw(&mut self.stream).map_err(|e| match e.kind() {
            io::ErrorKind::InvalidData => HubCallError::Protocol(e.into()),
            _ => HubCallError::NotSent(e),
        })?;
        match serde_json::from_slice::<HandshakeReply>(&body) {
            Ok(reply) => Ok(Ok(reply)),
            Err(e) => match frame_tag(&body) {
                Some(tag) if !matches!(tag.as_str(), "Welcome" | "AuthOk" | "AuthDenied") => {
                    Ok(Err(tag))
                }
                _ => Err(HubCallError::Protocol(Box::new(e))),
            },
        }
    }

    /// A client over a freshly dialled `stream`: nothing learned yet, the per-verb reply table, in step.
    fn on_stream(stream: TcpStream) -> Self {
        Self {
            stream,
            features: Vec::new(),
            authed: None,
            admitted: None,
            reply_override: None,
            unusable: None,
            codec: BodyCodec::Json,
        }
    }

    /// Swap the handshake-phase socket timeouts for the request-phase ones, once the handshake (and the
    /// `Auth` exchange, on a keyed server) has completed: the write timeout drops to
    /// [`REQUEST_WRITE_TIMEOUT`] and the READ timeout is CLEARED, because each verb arms its own in
    /// [`exchange_frame`](Self::exchange_frame) (the module doc's "Bounding a reply").
    ///
    /// The `Err` is a failed `setsockopt`, propagated rather than ignored because a client that silently
    /// kept [`CLIENT_HANDSHAKE_DEADLINE`] as its write timeout would bound a large request by the
    /// handshake's number.
    fn arm_request_timeouts(&self) -> io::Result<()> {
        self.stream.set_read_timeout(None)?;
        self.stream.set_write_timeout(Some(REQUEST_WRITE_TIMEOUT))?;
        Ok(())
    }

    /// The end of every constructor's handshake: the request-phase timeouts, then, to a hub whose `Welcome`
    /// advertised `crate::features::FEATURE_REFUSAL_FRAME`, the word that this client reads the refusal frame
    /// (`crate::handshake::reads_answer`; no reply comes). An older hub is sent nothing. A failure is
    /// [`HubCallError::NotSent`]: no request has left.
    fn finish_handshake(&mut self) -> Result<(), HubCallError> {
        self.arm_request_timeouts().map_err(HubCallError::NotSent)?;
        match reads_answer(&self.features) {
            Some(reads) => write_frame(&mut self.stream, &reads).map_err(HubCallError::NotSent),
            None => Ok(()),
        }
    }

    /// **Bound every verb's reply on this client by `timeout`**: an IDLE bound (no byte for that long),
    /// armed per exchange. `None` restores the per-verb default table, under which a verb whose work the
    /// client names (`Backfill`, `ImportArchive`, every `Run*`) is unbounded and every other verb gets
    /// 300 s.
    ///
    /// For a caller that knows its own scale: the desktop's options poll asks a snapshot verb once a
    /// second and passes seconds; a CLI that sends a one-month import can pass hours. A
    /// `Some(Duration::ZERO)` is refused by the socket layer at the next verb
    /// ([`HubCallError::NotSent`] of kind [`io::ErrorKind::InvalidInput`]).
    pub fn set_reply_timeout(&mut self, timeout: Option<Duration>) {
        self.reply_override = timeout;
    }

    /// The idle reply bound a verb whose class gives it `default_bound` arms on this client before
    /// writing: the caller's [`set_reply_timeout`](Self::set_reply_timeout) value, else that default.
    pub fn reply_bound(&self, default_bound: Option<Duration>) -> Option<Duration> {
        self.reply_override.or(default_bound)
    }

    /// Whether this client may still send: `false` once an exchange failed mid-frame (a timeout, a reset,
    /// a closed peer), after which every verb is refused locally. Drop it and dial again.
    pub fn is_usable(&self) -> bool {
        self.unusable.is_none()
    }

    /// **The ONE post-handshake exchange**: arm the verb's idle reply bound, write `request`, read one
    /// response. Every positional verb goes through here; `verb` is its wire tag, for the error text.
    ///
    /// ⚠ Any transport error from the write or the read MARKS THE CLIENT UNUSABLE (the module doc's ⚠) and
    /// is [`HubCallError::OutcomeUnknown`], a reply that does not decode included: a later call returns
    /// [`HubCallError::NotSent`] without writing, naming the first failure. A timeout on either leg is of
    /// kind [`io::ErrorKind::TimedOut`] whatever the platform's own. A refused `setsockopt`
    /// ([`HubCallError::NotSent`]) and a frame `write_frame` refuses before its first byte
    /// ([`HubCallError::Protocol`]) send nothing and leave the client usable.
    pub fn exchange_frame<Q: Serialize, A: DeserializeOwned>(
        &mut self,
        verb: &str,
        default_bound: Option<Duration>,
        request: &Q,
    ) -> Result<A, HubCallError> {
        self.exchange_with(verb, default_bound, request, |stream| {
            decode_body(&read_frame_raw(stream)?)
        })
    }

    /// **Write every later request in `codec`** (the msgpack body, `crate::frame`'s module doc). A hub client
    /// sets it only toward a hub that advertised its plane's feature word for it: a hub that predates it reads
    /// a msgpack body as an undecodable request. Replies are read in whichever codec they come, so this
    /// never changes what a reply may be.
    pub fn set_body_codec(&mut self, codec: BodyCodec) {
        self.codec = codec;
    }

    /// The codec this client writes its requests in ([`set_body_codec`](Self::set_body_codec)); JSON until set.
    pub fn body_codec(&self) -> BodyCodec {
        self.codec
    }

    /// [`Self::exchange_frame`] that hands back the reply's BODY undecoded, for the one reply that may
    /// not be JSON (the datahub's bar columns, `vike_datahub_client::proto`'s `BAR_COLUMNS_MAGIC`): the
    /// caller tells the two apart and decodes. The same bound, the same write, the same unusable rule.
    pub fn exchange_frame_raw<Q: Serialize>(
        &mut self,
        verb: &str,
        default_bound: Option<Duration>,
        request: &Q,
    ) -> Result<Vec<u8>, HubCallError> {
        self.exchange_with(verb, default_bound, request, read_frame_raw)
    }

    /// The body of [`Self::exchange_frame`], over the reply `read` it is given.
    fn exchange_with<Q: Serialize, A>(
        &mut self,
        verb: &str,
        default_bound: Option<Duration>,
        request: &Q,
        read: impl FnOnce(&mut TcpStream) -> io::Result<A>,
    ) -> Result<A, HubCallError> {
        let hub = hub_word(&self.features);
        if let Some(first) = &self.unusable {
            return Err(HubCallError::NotSent(io::Error::new(
                io::ErrorKind::NotConnected,
                format!(
                    "{hub} {verb}: not sent — this connection is unusable since {first}; drop \
                     the client and dial a new one"
                ),
            )));
        }
        let bound = self.reply_bound(default_bound);
        self.stream.set_read_timeout(bound).map_err(HubCallError::NotSent)?;
        if let Err(e) = write_frame_as(&mut self.stream, self.codec, request) {
            // `InvalidData` is the frame codec's refusal BEFORE the first byte (a body that does not
            // serialize, or one over `MAX_FRAME_LEN`): nothing reached the wire, the stream is in
            // step, and the client stays usable.
            if e.kind() == io::ErrorKind::InvalidData {
                return Err(HubCallError::Protocol(e.into()));
            }
            let e = legible_stall(e, || {
                format!(
                    "{hub} {verb}: the server accepted no request byte for \
                     {REQUEST_WRITE_TIMEOUT:?} (it stopped reading)"
                )
            });
            return Err(HubCallError::OutcomeUnknown(self.mark_unusable(verb, e)));
        }
        read(&mut self.stream).map_err(|e| {
            let e = legible_stall(e, || {
                let bound = bound.map_or_else(|| "its bound".to_string(), |b| format!("{b:?}"));
                format!(
                    "{hub} {verb}: no reply byte for {bound} — the link is dead or the server \
                     is wedged (a half-open socket answers nothing); this connection is not reused"
                )
            });
            HubCallError::OutcomeUnknown(self.mark_unusable(verb, e))
        })
    }

    /// Record `e` as the reason this client may send nothing more, and hand it back.
    fn mark_unusable(&mut self, verb: &str, e: io::Error) -> io::Error {
        self.unusable = Some(format!("{verb} failed mid-exchange ({e})"));
        e
    }

    /// The server's advertised feature strings, learned during the handshake (e.g. `"backtest"`,
    /// `"load_bars"`). Empty until a successful handshake.
    pub fn features(&self) -> &[String] {
        &self.features
    }

    /// Whether the connected server REQUIRES authentication, i.e. it advertises [`FEATURE_AUTH`], learned
    /// from the `Welcome` rather than from a protocol number.
    pub fn requires_auth(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_AUTH)
    }

    /// The [`Scope`] this connection authenticated under, or `None` on a key-less server (where no
    /// authentication took place, and every verb EXCEPT the destructive `DeleteSeries` is served, so
    /// `None` also answers "may this connection delete anything": no). A caller that must be certain it is
    /// talking to an authenticated connection checks this, not merely that
    /// [`connect_authed`](Self::connect_authed) returned `Ok`.
    pub fn authenticated_scope(&self) -> Option<Scope> {
        self.authed
    }

    /// Who the hub's access list admitted this connection as, when the hub said it.
    pub fn admitted(&self) -> Option<&crate::handshake::Admitted> {
        self.admitted.as_deref()
    }

    /// A handle that can CLOSE this connection from another thread: the way a caller abandons a verb
    /// blocked in its reply read before the idle bound fires, or an unbounded one (`Backfill`,
    /// `ImportArchive`, a `Run*`) that has none.
    ///
    /// It holds a `try_clone` of the socket, so [`CancelHandle::shutdown`] wakes the blocked read with an
    /// error, and the server sees its peer go: a backtesthub can stop working for a client that is no
    /// longer listening. The client itself is unusable after that, exactly as after any other transport
    /// failure; the stream field stays private.
    ///
    /// `Err` is a failed `try_clone` (descriptor exhaustion), reported rather than swallowed so a caller
    /// never believes it holds a cancel it does not.
    pub fn cancel_handle(&self) -> io::Result<CancelHandle> {
        Ok(CancelHandle { stream: self.stream.try_clone()? })
    }

    /// The socket, for a client's own mode switch to arm its bounds on.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// The socket, consumed: a mode switch hands it to whatever speaks on it next.
    pub fn into_stream(self) -> TcpStream {
        self.stream
    }
}

/// The other end of [`HubConnection::cancel_handle`]: one method, [`shutdown`](Self::shutdown).
///
/// Safe to send to another thread and to call while the client is blocked in a verb; calling it twice,
/// or after the peer closed, is harmless.
#[derive(Debug)]
pub struct CancelHandle {
    stream: TcpStream,
}

impl CancelHandle {
    /// Shut the connection down both ways. The client's blocked read (if any) returns an error and the
    /// server reads end-of-stream. The `Err` of a socket already shut or reset is ignored: there is
    /// nothing left to close, and the caller has no better answer to give.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// **How a box with no device key gets one** — the ONE wording of that cure (the CLI's
/// `vike_client::hubkeys::HubKeyring` messages and the desktop's `registry::no_device_key` are built from
/// it), by whether the hub is reached on THIS box (`this_box`: a loopback address). A LOOPBACK hub is
/// most often this box's own, whose list `backend mint-missing` puts this box's key on (a fresh
/// project's first read was refused for want of exactly that, measured 2026-11-21); a loopback port can
/// still be a tunnel to another server, and a remote hub always is one, where `backend connect`
/// registers it. PURE.
pub fn no_device_key_cure(this_box: bool) -> &'static str {
    if this_box {
        "On this box's own hubs, register its device key with `vike-cli backend mint-missing` (after \
         `vike-cli secrets init` when there is no settings database); through a tunnel to another \
         server, with `vike-cli backend connect HOST --server-project DIR`. `vike-cli access ls` on \
         the server shows who may do what"
    } else {
        "Register this box's own device key with its server: `vike-cli backend connect HOST \
         --server-project DIR`. `vike-cli access ls` on the server shows who may do what"
    }
}

/// The host names that mean THIS box on every system [`names_this_box`] meets, by their usual
/// `/etc/hosts` lines; any other name is another box to it.
const LOOPBACK_NAMES: [&str; 4] =
    ["localhost", "localhost.localdomain", "ip6-localhost", "ip6-loopback"];

/// Whether `address` (`host:port`, as a client is configured with it) names THIS box, judged from the
/// TEXT alone: a loopback IP, the unspecified address (`0.0.0.0`, `::`, which a dial reaches as this
/// box), or one of [`LOOPBACK_NAMES`]. The handshake judges its RESOLVED peer instead
/// (`peer_addr().ip().is_loopback()`); this resolves nothing, because it also runs where a GUI frame
/// draws, so a host name only this box's `/etc/hosts` maps to loopback reads as another box and gets the
/// remote cure (`backend connect`, a route that works through a tunnel too). PURE.
pub fn names_this_box(address: &str) -> bool {
    match address.parse::<std::net::SocketAddr>() {
        Ok(socket) => socket.ip().is_loopback() || socket.ip().is_unspecified(),
        Err(_) => address
            .rsplit_once(':')
            .is_some_and(|(host, _)| LOOPBACK_NAMES.iter().any(|n| host.eq_ignore_ascii_case(n))),
    }
}

/// What an error calls the far side: the daemon its `Welcome` says it is (`features`, empty before
/// one), so the sentence is true wherever the client was pointed. The module doc's ⚠.
fn hub_word(features: &[String]) -> &'static str {
    match welcome_plane(features) {
        Some(Plane::Data) => "datahub",
        Some(Plane::Backtest) => "backtesthub",
        None => "hub",
    }
}

fn not_the_welcome(got: &str) -> HubCallError {
    HubCallError::Protocol(format!("hub handshake: expected Welcome, got {got}").into())
}

fn not_the_auth_answer(hub: &str, got: &str) -> HubCallError {
    HubCallError::Protocol(format!("{hub} auth: expected AuthOk/AuthDenied, got {got}").into())
}

/// A socket TIMEOUT re-spelled as [`io::ErrorKind::TimedOut`] carrying `sentence`; any other error
/// unchanged. Both kinds are matched because the socket layer reports an expired `SO_RCVTIMEO` /
/// `SO_SNDTIMEO` as `WouldBlock` (`EAGAIN`) on unix and as `TimedOut` (`WSAETIMEDOUT`) on windows, and a
/// caller deciding "the link is dead" must not have to know which box it runs on.
fn legible_stall(e: io::Error, sentence: impl FnOnce() -> String) -> io::Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
            io::Error::new(io::ErrorKind::TimedOut, sentence())
        }
        _ => e,
    }
}

/// Compare the server's advertised protocol version against the client's, producing the legible
/// mismatch message (which NAMES BOTH versions) or `Ok(())` on a match. Pure, so the contract is
/// unit-testable without a socket.
fn check_proto_version(hub: &str, ours: u32, server_version: u32) -> Result<(), String> {
    if server_version == ours {
        Ok(())
    } else {
        Err(format!(
            "{hub} protocol version mismatch: client speaks {ours}, server speaks {server_version}"
        ))
    }
}

/// The refusal [`HubConnection::connect_authed_on`] returns when the `Welcome` does not name the plane it was
/// asked for: pure, so its wording is unit-tested without a socket.
///
/// It says which daemon ANSWERED (or that the answer named none), what the key would have opened there,
/// and which command and setting serve the plane the operator wanted. It names no key byte and no key
/// name.
fn wrong_welcome_message(
    peer: &str,
    wanted: Plane,
    found: Option<Plane>,
    scope: Scope,
    features: &[String],
) -> String {
    let answered = answered(found, features);
    format!(
        "refusing to authenticate ({scope:?}) to {peer}: it is {answered}, and this connection was \
         asked for the {wanted:?} daemon, served by `{}`. Nothing was sent after the Hello. Point \
         the client at that daemon ({}).",
        wanted.served_by(),
        wanted.address_key(),
    )
}

/// Which daemon a `Welcome` named, in the words both refusals use.
fn answered(found: Option<Plane>, features: &[String]) -> String {
    match found {
        Some(Plane::Data) => "the DATA daemon (`vike-backend datahub`), whose Write scope also \
                              carries `Backfill` and `DeleteSeries`"
            .to_string(),
        Some(Plane::Backtest) => "the backtesthub (`vike-backend backtesthub --addr`)".to_string(),
        None => format!(
            "a server whose Welcome names no single plane (features {features:?}) — a pre-split \
             daemon serving both, or not a vike daemon at all"
        ),
    }
}

/// The plane a client signs for, read off its separator: the two data-plane hubs' own, `None` for any
/// other.
fn plane_of_domain(domain: Domain) -> Option<Plane> {
    if domain == BACKTESTHUB_DOMAIN {
        Some(Plane::Backtest)
    } else if domain == DATAHUB_DOMAIN {
        Some(Plane::Data)
    } else {
        None
    }
}

/// **A client met the daemon of the OTHER plane, at another protocol version** ([`HubConnection`]'s
/// handshake identifies the daemon before it compares the versions). It rides a [`HubCallError::Protocol`]
/// and is read back with [`WrongDaemon::of`], so a caller that probes either plane
/// (`vike-cli backend ping`) dials the daemon that answered with that daemon's own client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrongDaemon {
    /// The plane the `Welcome` named.
    pub found: Plane,
    /// The plane this client was built for.
    pub wanted: Plane,
    message: String,
}

impl WrongDaemon {
    /// The `WrongDaemon` an error of [`HubConnection`]'s carries, if it is one.
    pub fn of(error: &HubCallError) -> Option<&WrongDaemon> {
        match error {
            HubCallError::Protocol(inner) => inner.downcast_ref::<WrongDaemon>(),
            _ => None,
        }
    }
}

impl std::fmt::Display for WrongDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WrongDaemon {}

/// [`WrongDaemon`]'s words: which daemon answered, at which version, and which daemon this client
/// speaks to and where it is configured. Pure, so its wording is unit-tested without a socket.
fn wrong_daemon_message(peer: &str, wanted: Plane, found: Plane, ours: u32, theirs: u32) -> String {
    format!(
        "{peer} is {} (protocol {theirs}), and this client speaks to the {wanted:?} daemon (protocol \
         {ours}), served by `{}`. Nothing was sent after the Hello. Point the client at that daemon \
         ({}).",
        answered(Some(found), &[]),
        wanted.served_by(),
        wanted.address_key(),
    )
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod connection_tests;
