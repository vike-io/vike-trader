//! The blocking, thread-per-connection backtest server.
//!
//! [`serve`] owns a bound [`TcpListener`] and accepts forever; each accepted connection is handled
//! on its own `std::thread` running a read-frame -> handle -> write-frame loop. The store is an
//! `Arc<dyn HistStore + Send + Sync>`, so the server is BACKEND-AGNOSTIC — `DataFusionHist` in
//! production, `MemHistStore` in tests — and every connection thread shares the one store cheaply.
//!
//! Isolation guarantees:
//! - A read error or clean EOF ends ONLY that connection's loop (never the accept loop).
//! - One connection's work runs on its own thread, so even a panic inside a handler cannot take
//!   down the accept loop or another connection — it just drops that one socket.
//! - Every request produces a [`Response`]: a run failure becomes [`Response::Error`], not a
//!   dropped connection, so a client always learns the outcome.
//!
//! # Connection hygiene (PR-2)
//!
//! - **A bad request never drops the connection.** Framing and DECODING are split: the loop reads a
//!   frame's body bytes with [`read_frame_raw`] and decodes them separately, so a well-framed body
//!   that fails to decode into a known [`Request`] (an unknown/incompatible verb from a newer
//!   client) is answered with [`Response::Error`] and the loop CONTINUES — one bad request is a bad
//!   *request*, not a bad *connection*.
//! - **A half-open connection cannot pin a thread forever.** Each accepted stream gets a generous
//!   read timeout ([`IDLE_READ_TIMEOUT`]); a `WouldBlock`/`TimedOut` on an idle or half-open peer
//!   (laptop lid closed, cable pulled) closes the connection and frees the thread. A timeout is
//!   NEVER recovered into a resumed loop (which would desync the stream) — it always closes.
//! - **The `Hello` handshake is optional ON A KEY-LESS SERVER.** [`Request::Hello`] negotiates the
//!   protocol version and returns the served-verb `features`, and a client that sends a normal
//!   request WITHOUT a prior `Hello` is served exactly as before — the handshake informs, it does
//!   not gate. ⚠ That was unconditional until authentication landed; on a KEYED server it is the
//!   opposite (see the next section), and the two arms must not be confused.
//!
//! # Authentication — OPT-IN, and the credential IS the switch (0025's adopting change)
//!
//! [`serve_authed`] takes `Option<NodeKeys>`, and the two arms are the whole contract:
//!
//! - **`None` — key-LESS.** The server behaves EXACTLY as it did before authentication existed:
//!   `Hello` is optional and informs rather than gates, every verb is served to whoever can reach
//!   the socket, and the `Welcome` bytes are IDENTICAL (its `nonce` field is `None` and skipped on
//!   the wire, and [`FEATURE_AUTH`] is not advertised). This is the default and it is what
//!   [`serve`] / [`serve_with_backfill`] still do, so every existing local flow — the Studio's
//!   `Backend::Remote`, `vike-cli backtest`, the GUI's store branch,
//!   `vike_datahub_client::RemoteHistStore` — keeps working untouched. The loopback bind guard
//!   below is what holds the posture in that mode.
//! - **`Some(keys)` — KEYED.** `Hello` becomes MANDATORY and must be the FIRST frame; the
//!   `Welcome` answering it carries a fresh 32-byte per-connection nonce; a
//!   [`Request::Auth`] carrying the HMAC over that nonce must follow; and every subsequent verb is
//!   checked against the authenticated [`Scope`]. Any verb before `AuthOk` is refused and the
//!   connection closes.
//!
//! That shape is this workspace's credential-is-the-gate idiom (absent creds ⇒ every venue stays
//! paper), pointed at a server: an operator turns authentication ON by writing
//! `VIKE_DATAHUB_OBSERVE_KEY` / `VIKE_DATAHUB_CONTROL_KEY` into `<project>/settings/secrets.env`,
//! and there is no second switch to forget.
//!
//! [`required_scope`] is the ONE authority for which verb needs which scope, and its match is
//! EXHAUSTIVE (no `_` arm) so a new wire verb cannot be added unclassified — it fails to compile
//! instead. The split, and the one part of it that is not obvious:
//!
//! - **[`Scope::Observe`]** — the history and catalog READS (`LoadBars`, `ScanQuotes`,
//!   `ScanTrades`, `PropertiesAsOf`, `ListSeries`, `Inventory`, `SeriesGaps`, `ListStrategies`)
//!   plus `Ping`. These answer from the store and change nothing.
//! - **[`Scope::Control`]** — [`Request::Backfill`], which WRITES the store and spends venue-API
//!   budget from this box's IP; **and every `Run*` verb, because they COMPILE CLIENT-SUPPLIED
//!   RHAI**. That second clause is the classification that matters and it is not a guess:
//!   `RunSlice`/`RunSweep`/`RunWalkforward` reach `crate::wire_run`'s `to_strategy_spec`, whose
//!   `WireSpec::Rhai(src)` arm hands the client's SOURCE to `StrategySpec::rhai`; and
//!   `RunBacktest`/`RunSweepProfile`/`RunWalkforwardProfile` carry a profile whose
//!   `[strategy.params].src` resolves through `vike_backtest::harness::registry`'s `"rhai"` arm to
//!   the same compiler (`crate::bin`'s `install_user_indicators` documents that flow, and
//!   `vike-cli backtest --script` is the surface that ships the source). Remote code execution by
//!   design is not a read, whatever it returns — so it sits behind Control, not Observe.
//!
//! # The outer barrier is still the bind posture, and auth does not replace it
//!
//! The handshake is PLAINTEXT and authenticates the CONNECTION, not each frame: confidentiality
//! and integrity still come from the SSH tunnel (or a VPN) in front of it — 0025's "honest limits
//! of B" is explicit about this, and it is why the bind guard did not soften. The bind target is
//! still CLASSIFIED before the listener opens: [`bind_exposure`] is the pure classification,
//! [`bind_decision`] the policy (a non-loopback bind is REFUSED without the named opt-in) — the
//! exact idiom of `vike_tradehub::server`, mirrored — and the bin enforces the verdict.
//!
//! ⚠ **A non-loopback datahub is an AUTHENTICATED one, by construction rather than by advice.**
//! [`bind_decision`] takes the [`ServerAuth`] this process is about to serve under, and a
//! [`ServerAuth::Keyless`] server REFUSES a non-loopback bind even WITH the opt-in
//! ([`BindDecision::RefuseUnauthenticated`]). That sentence stood in this doc before the guard
//! composed the two knobs, and it was not true then: the opt-in alone bound and served, behind one
//! `warn!`. 0025's "what would reopen this" names a non-loopback need as the TRIGGER for adopting
//! the keys — "an opt-in warning in front of an unauthenticated write verb is the exact state this
//! record exists to prevent" — so the trigger is now enforced instead of documented. LOOPBACK is
//! untouched: a key-less loopback datahub is the ordinary developer configuration.
//!
//! Logging is at CONNECTION boundaries only (open/close/fault, plus the auth verdict), never per
//! frame — this path is not the vike-core hot fold, but per-message logging would still be noise
//! under load.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use rand::Rng; // rand 0.10 core trait — provides `fill_bytes` (formerly `RngCore` in rand 0.8)

use vike_backtest::harness::{self, BacktestProfile, BacktestReport};
use vike_data::{HistStore, TsRange};
use vike_datahub_client::node_auth::{self, NodeKeys, Scope, DATAHUB_DOMAIN};
use vike_datahub_client::proto::{
    read_frame_raw, read_frame_raw_capped, write_frame, BackfillDone, Request, Response,
    FEATURE_AUTH, FEATURE_BACKFILL, FEATURE_COVERAGE, PROTO_VERSION,
};
use vike_datahub_client::wire_studio::{
    WireEngineParams, WireSlice, WireSpec, WireSweep, WireWalkforward,
};

use crate::backfill::BackfillTable;

/// A generous per-connection read timeout so a half-open or idle peer cannot park a connection
/// thread in `read` for the process lifetime (PR-2).
///
/// Sized for the localhost + SSH-tunnel deployment: a real request body is kilobytes and arrives in
/// milliseconds once its length prefix is seen, so a timeout realistically only fires BETWEEN
/// requests (an idle or half-open connection), which the loop then closes to free the thread. It is
/// deliberately far larger than any legitimate inter-request gap — a value that would clip a slow
/// but live client is worse than a leaked thread — and a timeout is never recovered into a resumed
/// loop (see [`handle_connection`]).
const IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);

/// Frame ceiling for the PRE-AUTH phase — the `Hello` and the `Auth` an unauthenticated peer sends
/// to a KEYED server. Mirrors `vike_tradehub::server`'s `HANDSHAKE_MAX_FRAME_LEN`, byte for byte
/// and for the same reason.
///
/// The shared `MAX_FRAME_LEN` is 64 MiB because a legitimate datahub *answer* (a chart's worth of
/// bars) can be large — this is the crate that made it 64 MiB. A handshake frame cannot be:
/// `Hello` is a version number and `Auth` is a scope plus a 32-byte mac, together a few hundred
/// bytes even JSON-encoded. Accepting 64 MiB of them means anyone who can reach the socket makes
/// this server allocate 64 MiB per connection by sending FOUR BYTES — no key, no round trip. 64 KiB
/// is ~200x the largest real handshake frame and 1024x cheaper to be wrong about.
///
/// `docs/decisions/0025-datahub-remote-posture.md` names this gap explicitly as a cost route B
/// repeats ("which today has NO pre-auth frame cap at all — its one shared frame ceiling is sized
/// for legitimate large answers"). Post-auth frames keep the full `MAX_FRAME_LEN`: by then the peer
/// has proved possession of a scoped key, and a profile TOML or a Rhai source legitimately runs to
/// kilobytes.
///
/// ⚠ It applies ONLY on a KEYED server. A key-less one has no pre-auth phase at all — every frame
/// is a post-auth frame by definition — so imposing it there would shrink a limit that legitimate
/// traffic already uses, for no security gain behind the loopback guard.
pub const HANDSHAKE_MAX_FRAME_LEN: u32 = 64 * 1024;

/// How long a KEYED server waits for the whole two-frame handshake before closing the connection.
///
/// Deliberately FAR shorter than [`IDLE_READ_TIMEOUT`] (which is 300 s, sized for the gap BETWEEN
/// requests on a live client): an unauthenticated peer has nothing to think about — the `Hello` is
/// a constant and the `Auth` is one HMAC over 32 bytes it was just handed — so a handshake that has
/// not completed in this window is not slow, it is a socket somebody opened and said nothing on.
/// Without it, `IDLE_READ_TIMEOUT` would let an unauthenticated peer park a connection thread for
/// five minutes per socket.
///
/// Applied as the stream's read timeout for the pre-auth phase only, then RESET to
/// [`IDLE_READ_TIMEOUT`] once `AuthOk` is written — so it bounds the handshake without clipping a
/// legitimately idle authenticated client.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

// Compile-time bounds on the two pre-auth limits, the tradehub `confirm.rs` idiom: a RANGE, so a
// deliberate tweak stays free while "the bound was effectively removed" and "the bound refuses
// every legitimate handshake" both fail to compile.
const _: () = assert!(
    HANDSHAKE_MAX_FRAME_LEN > 0 && HANDSHAKE_MAX_FRAME_LEN <= 1024 * 1024,
    "HANDSHAKE_MAX_FRAME_LEN must stay far under MAX_FRAME_LEN — an unauthenticated peer must not \
     be able to name a large allocation"
);
const _: () = assert!(
    HANDSHAKE_DEADLINE.as_secs() > 0 && HANDSHAKE_DEADLINE.as_secs() <= 120,
    "HANDSHAKE_DEADLINE must stay a POSITIVE, short bound — it exists to stop an unauthenticated \
     peer parking a connection thread, which a 0 (refuse everything) or a multi-minute value both \
     defeat"
);

/// The default listen address — localhost only, reached over an SSH tunnel
/// (`ssh -L 7878:localhost:7878 <box>`), the same posture as the tradehub node server's
/// `DEFAULT_ADDR`. The listener MUST stay bound to loopback: this protocol authenticates NOTHING
/// (see the module doc), so reachability is not defense in depth here — it is the whole barrier.
pub const DEFAULT_ADDR: &str = "127.0.0.1:7878";

/// How exposed a resolved bind target is — the input to the bin's non-loopback refusal. Mirrors
/// `vike_tradehub::server`'s `BindExposure` (same variants, same classification rule), so the two
/// backend daemons answer "is this address reachable off-box" identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindExposure {
    /// Every resolved address is a loopback address: reachable only from this host.
    Loopback,
    /// At least one resolved address is NOT loopback (a specific interface, or the wildcard
    /// binds). Carries the first such address, for the operator-facing message.
    Public(SocketAddr),
    /// The address resolved to nothing at all — `bind` would fail anyway; classified separately so
    /// a caller never has to read "not loopback" as "publicly exposed".
    Unresolvable,
}

/// Classify an already-RESOLVED bind target. Pure, so the policy is unit-testable without a socket
/// and without DNS; the caller does the `to_socket_addrs()` that produced `resolved`, which is the
/// same resolution `TcpListener::bind` performs.
///
/// A target is [`BindExposure::Loopback`] only when EVERY resolved address is loopback — a hostname
/// resolving to both a loopback and a LAN address is exposed on the LAN, so the strictest answer
/// is the true one. `0.0.0.0` and `::` are the wildcard binds, which `IpAddr::is_loopback` reports
/// false for, so they classify as [`BindExposure::Public`] — correctly: they listen on every
/// interface, which is the single most common way this surface gets accidentally exposed.
pub fn bind_exposure(resolved: &[SocketAddr]) -> BindExposure {
    match resolved.iter().find(|a| !a.ip().is_loopback()) {
        Some(a) => BindExposure::Public(*a),
        None if resolved.is_empty() => BindExposure::Unresolvable,
        None => BindExposure::Loopback,
    }
}

/// Whether the server about to open this listener CAN authenticate a connection — the third input
/// to [`bind_decision`], and what makes it a POSTURE decision rather than an address one.
///
/// It is a named enum rather than a second `bool` deliberately: [`bind_decision`]'s other policy
/// input is already a `bool` (`allow_public`), and two adjacent `bool` parameters TRANSPOSE
/// SILENTLY at a call site — the same hazard the bin's `resolve_store_root_from` comment describes
/// for two adjacent `Option<PathBuf>`s, and a transposition here would restore exactly the defect
/// this variant exists to remove. Named variants cannot be swapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAuth {
    /// [`serve_authed`] is about to be handed `Some(keys)`: every connection must complete the
    /// handshake before any verb is answered, and each verb is then checked against
    /// [`required_scope`].
    Keyed,
    /// [`serve_authed`] is about to be handed `None`: no handshake is required and EVERY verb — the
    /// history reads, the `Run*` verbs that COMPILE CLIENT-SUPPLIED RHAI, and (in a
    /// `backfill-serve` build) the `Backfill` store WRITE — is served to whoever can reach the
    /// socket. The bind posture is then the entire barrier, which is why it may not be waived.
    Keyless,
}

impl ServerAuth {
    /// Classify from the very value the caller is about to hand [`serve_authed`], so the bind guard
    /// and the server cannot disagree about whether this process authenticates.
    ///
    /// `Some(_)` is [`ServerAuth::Keyed`] even when only ONE scope has a key, because
    /// [`run_handshake`] refuses a scope this server holds no key for — `if !keys.has(scope)` —
    /// BEFORE the key is consulted at all. So a partially-keyed server still answers no verb to an
    /// unauthenticated peer, and `None` is the only shape that serves everything.
    ///
    /// ⚠ Cite `has`, NOT `NodeKeys::key_for`. The first draft of this comment said an absent scope
    /// key "verifies against an empty key and fails" — inherited from `key_for`'s own doc, and
    /// false: `node_auth`'s `verify` builds `HmacSha256::new_from_slice(key)`, which accepts a
    /// zero-length key, so the empty slice is an ordinary HMAC rather than a closed gate. The
    /// CONCLUSION here is unaffected; only its reason was wrong. `key_for`'s doc now carries the
    /// correction.
    pub fn of(keys: Option<&NodeKeys>) -> Self {
        match keys {
            Some(_) => ServerAuth::Keyed,
            None => ServerAuth::Keyless,
        }
    }
}

/// What the bin should DO about a bind target — the policy, kept here in the library rather than
/// inline in the binary so it is unit-testable and so a change to it shows up as a change to a
/// named function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// Loopback (or unresolvable — `bind` will report that itself): bind, say nothing special.
    Proceed,
    /// Non-loopback, WITH the operator's explicit opt-in, on a server that CAN authenticate: bind,
    /// but announce what is now reachable. ⚠ Reachable only under [`ServerAuth::Keyed`] — the
    /// key-less half of this arm is [`BindDecision::RefuseUnauthenticated`] below.
    ProceedExposed(SocketAddr),
    /// Non-loopback with NO opt-in: do not bind. What "do not bind" means is the caller's — the
    /// tradehub daemon keeps trading headless, while this server's bin EXITS, because serving is
    /// its only job.
    Refuse(SocketAddr),
    /// Non-loopback WITH the opt-in, on a [`ServerAuth::Keyless`] server: do not bind either. The
    /// opt-in consents to being REACHABLE; it is not, and never was, consent to serve a store
    /// write and a Rhai compiler to anyone who can open a socket. A SEPARATE variant rather than a
    /// reason field on [`BindDecision::Refuse`] because the two refusals have different FIXES —
    /// one says "keep the address on loopback", this one says "configure the node keys" — and
    /// because adding it makes every existing `match` on this enum fail to compile until its
    /// author confronts the case.
    RefuseUnauthenticated(SocketAddr),
}

/// Decide whether to open the listener. `allow_public` is the operator's explicit opt-in — for
/// this server `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1`, environment-only, because this binary loads no
/// settings files at all (its tradehub twin pairs the variable with a `flags.toml` key). `auth` is
/// what the process is about to hand [`serve_authed`], classified by [`ServerAuth::of`].
///
/// ⚠ The refusal is the point, and it is a DEFAULT rather than a prohibition. On a KEY-LESS server
/// — still the default — this protocol authenticates nothing (see the module doc), so everything it
/// serves, including the store WRITE in a `backfill-serve` build, comes down to who can reach the
/// socket. A KEYED server does authenticate, and the guard STILL applies to it: the handshake is
/// plaintext and authenticates the CONNECTION, not each frame, so the tunnel is what supplies
/// confidentiality and integrity under either posture (0025's "honest limits of B"). The design
/// answer is a loopback listener plus an SSH tunnel; nothing ENFORCED it until this existed, and
/// the shipped unit cannot (its `EnvironmentFile=` beats its own bind default, so one `.env` line
/// used to expose the server with no unit edit and no warning). But a data server on a trusted LAN
/// or a VPN interface is a real deployment, and a guard with no escape hatch is one people route
/// around by other means, so the escape hatch is NAMED and separate: an address cannot be its own
/// consent, because typing the address is the mistake.
///
/// ⚠ **The escape hatch has a floor, and `auth` is what puts it there.** The opt-in used to be the
/// WHOLE consent: `VIKE_DATAHUB_ADDR=0.0.0.0` plus `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1` on a key-less
/// server bound and served, behind one `warn!` line — a publicly reachable history read, a Rhai
/// compiler and (in a `backfill-serve` build) a store write, gated by a log message. That is the
/// state `docs/decisions/0025-datahub-remote-posture.md` exists to prevent, in that record's own
/// words: "an opt-in warning in front of an unauthenticated write verb is the exact state this
/// record exists to prevent", and it names a non-loopback need as the TRIGGER for adopting the
/// keys rather than a reason to set the opt-in as a workaround. So the two knobs now compose:
/// `allow_public` consents to being REACHABLE, the node keys make what is reached
/// AUTHENTICATED, and a non-loopback bind needs both — [`BindDecision::RefuseUnauthenticated`].
/// LOOPBACK is untouched by `auth` in either direction: a key-less loopback datahub is the ordinary
/// developer configuration and stays byte-identical.
///
/// An UNRESOLVABLE target proceeds deliberately: `TcpListener::bind` is about to fail on it with a
/// better message than this function could invent, and reporting "not loopback" for an address
/// that is not anything would send an operator looking for an exposure that does not exist.
///
/// ⚠ This is where the datahub guard STOPS being signature-identical to `vike_tradehub::server`'s
/// `bind_decision`, and the divergence is the honest one: the tradehub node server cannot BE
/// key-less — its `serve` takes `keys: NodeKeys` by value and `start_observe_server` returns
/// `None` (no listener at all) when no observe key resolves — so over there the question `auth`
/// answers has no second case to compute. `bind_exposure`/[`BindExposure`] — the classification of
/// an ADDRESS, which is the part that must not drift — stays mirrored name-for-name.
pub fn bind_decision(
    resolved: &[SocketAddr],
    allow_public: bool,
    auth: ServerAuth,
) -> BindDecision {
    match bind_exposure(resolved) {
        BindExposure::Loopback | BindExposure::Unresolvable => BindDecision::Proceed,
        // No opt-in at all: the first thing to say is still "keep it on loopback", whether or not
        // keys happen to be configured — so this stays the pre-existing verdict, unchanged.
        BindExposure::Public(a) if !allow_public => BindDecision::Refuse(a),
        BindExposure::Public(a) => match auth {
            ServerAuth::Keyed => BindDecision::ProceedExposed(a),
            ServerAuth::Keyless => BindDecision::RefuseUnauthenticated(a),
        },
    }
}

/// Accept connections forever, handling each on its own thread over the shared `store`.
///
/// Returns only if `listener.incoming()` yields `None` (it does not for a TCP listener), so in
/// practice this runs for the process lifetime. An accept error is logged and the loop continues —
/// one failed accept never ends the server.
///
/// This entry mounts NO backfill table: `Request::Backfill` is answered with a clean refusal and
/// `Welcome` does not advertise [`FEATURE_BACKFILL`]. The bin's `backfill-serve` build goes
/// through [`serve_with_backfill`] instead.
pub fn serve(listener: TcpListener, store: Arc<dyn HistStore + Send + Sync>) -> io::Result<()> {
    serve_with_backfill(listener, store, None)
}

/// [`serve`] with an optional backfill-on-demand collector table (split-plane REQ-9).
///
/// `Some(table)` serves [`Request::Backfill`] through the table's venue → collector dispatch and
/// advertises [`FEATURE_BACKFILL`] in every `Welcome`; `None` is byte-identical to [`serve`]. The
/// table's entries must write through the SAME store handle `store` wraps (the
/// [`crate::backfill::real_backfill_table`] constructor guarantees it; a test fake owes the same),
/// so a backfilled range is visible to the next read on any connection.
pub fn serve_with_backfill(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
) -> io::Result<()> {
    serve_authed(listener, store, backfill, None)
}

/// [`serve_with_backfill`] with OPTIONAL `NodeKeys` authentication — the full entry, and the one
/// the bin calls (`docs/decisions/0025-datahub-remote-posture.md`, the adopting PR).
///
/// `keys`:
/// - **`None`** — byte-identical to [`serve_with_backfill`]. No handshake is required, `Hello` is
///   optional and informs rather than gates, `Welcome` carries no nonce and does not advertise
///   [`FEATURE_AUTH`], every verb is served. This is the pre-auth protocol, unchanged, and it is
///   what a server whose credential store holds no datahub keys does. See the module doc.
/// - **`Some(keys)`** — every connection MUST complete `Hello` → `Welcome{nonce}` → `Auth{scope,
///   mac}` → `AuthOk` before any verb is answered, and each verb is then checked against
///   [`required_scope`]. Pre-auth frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] and the whole
///   handshake under [`HANDSHAKE_DEADLINE`].
///
/// ⚠ A `Some(keys)` whose scopes are BOTH empty cannot happen through
/// `vike_datahub_client::node_auth::node_keys_from_vars` (it returns `None` when neither key is
/// configured), and if one is constructed by hand it is a CLOSED server, not an open one: every
/// `Auth` fails against an empty key, which is the credential-is-the-gate idiom's safe direction.
pub fn serve_authed(
    listener: TcpListener,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<BackfillTable>,
    keys: Option<NodeKeys>,
) -> io::Result<()> {
    let backfill = backfill.map(Arc::new);
    let keys = keys.map(Arc::new);
    match keys.as_deref() {
        Some(k) => tracing::info!(
            keys = ?k,
            "vike-datahub: AUTHENTICATION REQUIRED — every connection must complete the NodeKeys \
             handshake before any verb is answered (the `Debug` above reports key PRESENCE only)"
        ),
        None => tracing::info!(
            "vike-datahub: no datahub node keys configured — serving UNAUTHENTICATED (the loopback \
             bind guard is the whole barrier). Set VIKE_DATAHUB_OBSERVE_KEY / \
             VIKE_DATAHUB_CONTROL_KEY in the credential store to require authentication"
        ),
    }
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let store = Arc::clone(&store);
                let backfill = backfill.clone();
                let keys = keys.clone();
                thread::spawn(move || handle_connection(stream, store, backfill, keys.as_deref()));
            }
            Err(e) => {
                // A failed accept is per-connection; keep serving.
                tracing::warn!(error = %e, "vike-datahub: accept failed, continuing");
            }
        }
    }
    Ok(())
}

/// The scope a verb requires — the ONE authority for the datahub's read/write split, consulted by
/// [`handle_connection`] on a KEYED server and iterated by the exhaustive table test in
/// `crates/vike-datahub/tests/auth_roundtrip.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerbScope {
    /// A pre-auth handshake frame — [`Request::Hello`] / [`Request::Auth`]. Not a verb at all: it
    /// is how a connection GETS a scope, so it cannot require one.
    Handshake,
    /// Servable by a connection authenticated under [`Scope::Observe`] or [`Scope::Control`].
    Observe,
    /// Servable ONLY by a connection authenticated under [`Scope::Control`].
    Control,
}

/// Classify one request. **The match is EXHAUSTIVE — no `_` arm, deliberately** — so a new wire
/// verb fails to COMPILE here until somebody classifies it, rather than silently defaulting to
/// whichever side the catch-all happened to pick. (A default of `Observe` would leak a write; a
/// default of `Control` would silently break a read for observe clients. Neither is a decision a
/// wildcard should make.)
///
/// The split, with the reasoning that is not self-evident spelled out:
///
/// - **Reads are [`VerbScope::Observe`]**: they answer from the store and change nothing.
///   `ListStrategies` joins them — it returns a compile-time const roster, not store contents.
///   `Ping` is Observe rather than Handshake: a liveness probe from an unauthenticated peer is a
///   free oracle for "is this address a datahub", and there is no reason to hand that out.
/// - **[`Request::Backfill`] is [`VerbScope::Control`]**: it WRITES the served store and spends
///   venue-API budget from this box's own IP. This is the verb 0025 calls "the class change" —
///   the reason the record chose authentication over tunnel-only-forever at all.
/// - **⚠ EVERY `Run*` verb is [`VerbScope::Control`], because they COMPILE CLIENT-SUPPLIED RHAI.**
///   This is the classification a reader is most likely to get wrong, since these verbs *return*
///   an answer and look like reads. They are not. Two paths, both verified in the code:
///   `RunSlice`/`RunSweep`/`RunWalkforward` reach `crate::wire_run`'s `to_strategy_spec`, whose
///   `WireSpec::Rhai(src)` arm passes the client's SOURCE to `StrategySpec::rhai`;
///   `RunBacktest`/`RunSweepProfile`/`RunWalkforwardProfile` carry profile TOML whose
///   `[strategy.params].src` resolves through `vike_backtest::harness::registry`'s `"rhai"` arm to
///   the same compiler. That is remote code execution BY DESIGN — it is how `vike-cli backtest
///   --script` works — and the correct posture for it is the write scope. A `serve-datafusion`-less
///   build answers the Studio three with a clean error anyway, but the CLASSIFICATION does not
///   depend on the build: the profile-shaped three run on every build.
///
///   The cost of this choice, stated so nobody has to rediscover it: an Observe client cannot run a
///   backtest. That is intended. A deployment that wants remote read-only history and remote
///   compute for the same peer gives that peer the control key; the scopes exist to make the OTHER
///   configuration — history without code execution — expressible at all, which it was not before.
pub fn required_scope(request: &Request) -> VerbScope {
    match request {
        Request::Hello { .. } | Request::Auth { .. } => VerbScope::Handshake,

        Request::Ping
        | Request::LoadBars { .. }
        | Request::ScanQuotes { .. }
        | Request::ScanTrades { .. }
        | Request::PropertiesAsOf { .. }
        | Request::ListSeries
        | Request::Inventory
        | Request::SeriesGaps { .. }
        // Coverage is the cross-kind READ behind the Data Manager's "Partial" column (split-plane
        // spec §6 Q2) — a plain `vike_data::HistStore` trait verb like `inventory`/`series_gaps`,
        // answering what a store already holds and writing nothing.
        | Request::Coverage
        | Request::ListStrategies => VerbScope::Observe,

        // The WRITE verb.
        Request::Backfill { .. }
        // ...and the six that compile client-supplied Rhai — see the ⚠ above.
        | Request::RunBacktest(_)
        | Request::RunSlice { .. }
        | Request::RunSweep { .. }
        | Request::RunWalkforward { .. }
        | Request::RunSweepProfile { .. }
        | Request::RunWalkforwardProfile { .. } => VerbScope::Control,
    }
}

/// Whether a connection authenticated under `authed` may send a verb requiring `needed`.
///
/// [`VerbScope::Handshake`] is `false` here on purpose: those frames belong to the pre-auth phase,
/// and a SECOND `Hello`/`Auth` on an established connection is a protocol error (it would be a
/// re-negotiation, and a scope that can be re-negotiated after the fact is not a ceiling).
fn scope_admits(authed: Scope, needed: VerbScope) -> bool {
    match needed {
        VerbScope::Handshake => false,
        VerbScope::Observe => matches!(authed, Scope::Observe | Scope::Control),
        VerbScope::Control => matches!(authed, Scope::Control),
    }
}

/// 32 fresh random bytes for ONE connection's auth challenge — what makes a captured transcript
/// unreplayable against a later connection. Mirrors `vike_tradehub::server`'s `fresh_nonce`,
/// including the crate: `rand`'s OS-backed default generator, already a workspace dep.
fn fresh_nonce() -> [u8; 32] {
    let mut nonce = [0u8; 32];
    rand::rng().fill_bytes(&mut nonce);
    nonce
}

/// The result of the pre-auth phase on a KEYED server.
enum HandshakeOutcome {
    /// The client authenticated under this [`Scope`]; proceed to the session with it as the ceiling.
    Authed(Scope),
    /// Refused (or a transport error) — the connection must close.
    Closed,
}

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server. Mirrors
/// `vike_tradehub::server`'s `run_handshake`, including its refusal ordering.
///
/// Both frames are read under [`HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling —
/// an unauthenticated peer must not be able to name a large allocation — and the caller has already
/// armed [`HANDSHAKE_DEADLINE`] as the read timeout, so a peer that opens a socket and says nothing
/// frees its thread in seconds rather than minutes.
///
/// The mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is refused
/// WITHOUT consulting it (the closed-gate shape), and otherwise `node_auth::verify` decides in
/// constant time.
fn run_handshake(
    stream: &mut TcpStream,
    keys: &NodeKeys,
    features: &[String],
    peer: Option<SocketAddr>,
) -> HandshakeOutcome {
    // Frame 1 — it MUST be Hello. Anything else is an unauthenticated verb: refuse it here. This is
    // where an unauthenticated LoadBars / Backfill / RunSlice is denied and the socket dropped.
    let body = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let client_version = match serde_json::from_slice::<Request>(&body) {
        Ok(Request::Hello { proto_version }) => proto_version,
        Ok(other) => {
            tracing::info!(
                ?peer,
                verb = request_kind(&other),
                "vike-datahub: verb refused — connection is not authenticated"
            );
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "not authenticated: expected Hello first".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Hello (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // The challenge. We always answer Hello with our version + a fresh nonce; a client on another
    // protocol version cannot forge a valid mac anyway (the version is SIGNED), so a skew fails
    // cleanly at verify rather than needing a branch here.
    let nonce = fresh_nonce();
    if write_frame(
        stream,
        &Response::Welcome {
            proto_version: PROTO_VERSION,
            features: features.to_vec(),
            nonce: Some(nonce),
        },
    )
    .is_err()
    {
        return HandshakeOutcome::Closed;
    }
    if client_version != PROTO_VERSION {
        tracing::debug!(
            ?peer,
            client_version,
            server_version = PROTO_VERSION,
            "vike-datahub: client protocol version differs; auth will fail on the signed version"
        );
    }

    // Frame 2 — the Auth answer. Still PRE-AUTH, same small ceiling (a scope plus a 32-byte mac).
    let body2 = match read_frame_raw_capped(stream, HANDSHAKE_MAX_FRAME_LEN) {
        Ok(b) => b,
        Err(_) => return HandshakeOutcome::Closed,
    };
    let (scope, mac) = match serde_json::from_slice::<Request>(&body2) {
        Ok(Request::Auth { scope, mac }) => (scope, mac),
        Ok(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth after Welcome".into() },
            );
            return HandshakeOutcome::Closed;
        }
        Err(_) => {
            let _ = write_frame(
                stream,
                &Response::AuthDenied { reason: "expected Auth (undecodable request)".into() },
            );
            return HandshakeOutcome::Closed;
        }
    };

    // A scope this server holds NO key for is refused without consulting the key — the closed-gate
    // shape. It is also how an observe-only datahub declines control outright rather than letting
    // an empty key decide it by accident.
    if !keys.has(scope) {
        tracing::info!(
            ?peer,
            ?scope,
            "vike-datahub: auth refused — no key configured for this scope on this server"
        );
        let _ = write_frame(
            stream,
            // Deliberately the SAME coarse reason a bad mac gets: distinguishing "no key for that
            // scope" from "wrong key for that scope" tells an unauthenticated peer which scopes
            // this server offers. The operator's log line above carries the real answer.
            &Response::AuthDenied { reason: "bad mac".into() },
        );
        return HandshakeOutcome::Closed;
    }

    // Constant-time verify against the REQUESTED scope's key, under the DATAHUB domain separator —
    // so a tag minted for the tradehub node (which may hold the same key bytes) never verifies here.
    if node_auth::verify(DATAHUB_DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope, &mac) {
        if write_frame(stream, &Response::AuthOk { scope }).is_err() {
            return HandshakeOutcome::Closed;
        }
        tracing::info!(?peer, ?scope, "vike-datahub: authenticated");
        HandshakeOutcome::Authed(scope)
    } else {
        tracing::info!(?peer, ?scope, "vike-datahub: auth denied (bad mac)");
        let _ = write_frame(stream, &Response::AuthDenied { reason: "bad mac".into() });
        HandshakeOutcome::Closed
    }
}

/// A request's variant name, for a log line — never its PAYLOAD, which on the `Run*` verbs is a
/// client-supplied script and on every verb is remote text.
fn request_kind(r: &Request) -> &'static str {
    match r {
        Request::Hello { .. } => "Hello",
        Request::Auth { .. } => "Auth",
        Request::Ping => "Ping",
        Request::RunBacktest(_) => "RunBacktest",
        Request::RunSlice { .. } => "RunSlice",
        Request::RunSweep { .. } => "RunSweep",
        Request::RunWalkforward { .. } => "RunWalkforward",
        Request::RunSweepProfile { .. } => "RunSweepProfile",
        Request::RunWalkforwardProfile { .. } => "RunWalkforwardProfile",
        Request::LoadBars { .. } => "LoadBars",
        Request::ScanQuotes { .. } => "ScanQuotes",
        Request::ScanTrades { .. } => "ScanTrades",
        Request::PropertiesAsOf { .. } => "PropertiesAsOf",
        Request::ListSeries => "ListSeries",
        Request::Inventory => "Inventory",
        Request::SeriesGaps { .. } => "SeriesGaps",
        Request::ListStrategies => "ListStrategies",
        Request::Coverage => "Coverage",
        Request::Backfill { .. } => "Backfill",
    }
}

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Never panics the caller — a fault logs and returns, dropping the
/// socket.
///
/// Framing and decoding are separate (PR-2): the body bytes are read with [`read_frame_raw`] and
/// then decoded, so a well-framed but undecodable request is answered with [`Response::Error`] and
/// the loop CONTINUES — the connection survives a bad request. A read timeout (an idle/half-open
/// peer) or any other read fault CLOSES the connection; a timeout is never recovered into a resumed
/// loop, which would desync the stream.
fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistStore + Send + Sync>,
    backfill: Option<Arc<BackfillTable>>,
    keys: Option<&NodeKeys>,
) {
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");

    let features = served_features(backfill.is_some(), keys.is_some());

    // On a KEYED server the PRE-AUTH phase runs first, under a far shorter deadline than the idle
    // timeout below: an unauthenticated peer has nothing to think about, so a handshake that has not
    // completed in seconds is a socket somebody opened and said nothing on.
    //
    // `None` — the key-less server — takes NEITHER branch: no handshake, no deadline, no scope. That
    // is what makes the key-less path byte-identical to the pre-auth protocol rather than merely
    // similar to it.
    let authed: Option<Scope> = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(HANDSHAKE_DEADLINE)) {
                tracing::warn!(?peer, error = %e, "vike-datahub: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer) {
                HandshakeOutcome::Authed(scope) => Some(scope),
                HandshakeOutcome::Closed => {
                    tracing::info!(?peer, "vike-datahub: connection closed (handshake refused)");
                    return;
                }
            }
        }
        None => None,
    };

    // Bound how long a single read may block, so a half-open peer cannot pin this thread forever. If
    // even setting the timeout fails, close rather than risk an unbounded park. On the keyed path
    // this also RESETS the short handshake deadline — an authenticated client may legitimately idle
    // between requests, exactly like an unauthenticated one always could.
    if let Err(e) = stream.set_read_timeout(Some(IDLE_READ_TIMEOUT)) {
        tracing::warn!(?peer, error = %e, "vike-datahub: could not set read timeout, closing connection");
        return;
    }

    loop {
        // Read the next frame's BODY BYTES only — decode is deliberately separate (below).
        let body = match read_frame_raw(&mut stream) {
            Ok(body) => body,
            Err(e) => {
                match e.kind() {
                    // The normal way a connection ends — no log.
                    io::ErrorKind::UnexpectedEof => {}
                    // No (further) bytes arrived within the window: an idle or half-open connection.
                    // Close it to free the thread; a live client simply reconnects for its next
                    // request. We never RESUME after a timeout — a mid-frame timeout would otherwise
                    // leave the stream desynced.
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(
                            ?peer,
                            "vike-datahub: idle read timeout, closing connection"
                        );
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "vike-datahub: read fault, closing connection");
                    }
                }
                break;
            }
        };

        // Decode the body. A well-framed body that is not a known `Request` (an unknown/incompatible
        // verb from a mismatched client) is answered with `Response::Error` and the loop CONTINUES —
        // one bad request never drops the connection.
        let response = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => match authed {
                // KEYED: check the verb against the connection's authenticated ceiling BEFORE it
                // reaches `handle_request`. A refusal is answered and the loop CONTINUES — an
                // Observe client asking for a Control verb has made a bad *request*, not opened a
                // bad *connection*, and dropping it would make an over-scoped read indistinguishable
                // from a transport fault. (An UNAUTHENTICATED verb is a different matter and is
                // refused with a closed socket, in `run_handshake`.)
                Some(scope) => {
                    let needed = required_scope(&request);
                    if scope_admits(scope, needed) {
                        handle_request(request, &store, backfill.as_deref())
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-datahub: verb refused — outside this connection's scope"
                        );
                        Response::Error(match needed {
                            VerbScope::Handshake => format!(
                                "{kind} is a handshake frame; this connection is already \
                                 authenticated"
                            ),
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Backfill write verb and every Run* verb (which \
                                 COMPILE client-supplied Rhai on this server) are Control-only"
                            ),
                        })
                    }
                }
                // KEY-LESS: unchanged — every verb served, exactly as before authentication existed.
                None => handle_request(request, &store, backfill.as_deref()),
            },
            Err(e) => Response::Error(format!("unrecognized/undecodable request: {e}")),
        };

        if let Err(e) = write_frame(&mut stream, &response) {
            tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection");
            break;
        }
    }
    tracing::info!(?peer, "vike-datahub: connection closed");
}

/// Map one request to its response. Pure dispatch — the backtest run lives in [`run_backtest`]; the
/// read verbs call the store method directly, mapping `Ok` to the matching typed response and any
/// [`vike_data::DataError`] to [`Response::Error`] (so a client always learns the outcome, exactly
/// like the backtest path).
fn handle_request(
    request: Request,
    store: &Arc<dyn HistStore + Send + Sync>,
    backfill: Option<&BackfillTable>,
) -> Response {
    match request {
        // The OPTIONAL version handshake (PR-2): answer with THIS server's `PROTO_VERSION` and the
        // verbs it serves. It is not a gate — a client may skip it and send a normal request; the
        // CLIENT compares the version and fails loudly on a mismatch (see `DatahubClient::connect`).
        Request::Hello { proto_version: client_version } => {
            tracing::debug!(client_version, "vike-datahub: hello handshake");
            Response::Welcome {
                proto_version: PROTO_VERSION,
                // `false`: this arm is only reached on a KEY-LESS server (a keyed one answers
                // `Hello` inside `run_handshake` and never returns here), so `FEATURE_AUTH` is
                // never advertised from it and `nonce` is `None` — which is exactly what makes the
                // key-less `Welcome` byte-identical to the pre-auth protocol's.
                features: served_features(backfill.is_some(), false),
                nonce: None,
            }
        }
        // Only reachable on a KEY-LESS server (a keyed one consumes `Auth` in `run_handshake`).
        // There is nothing to verify it against, so say so rather than pretending: a client that
        // signed a mac deserves to learn its key was never checked, not to be told "ok".
        Request::Auth { .. } => Response::AuthDenied {
            reason: "this datahub has no node keys configured and authenticates nothing; \
                     connect without Auth"
                .into(),
        },
        Request::Ping => Response::Pong,
        Request::RunBacktest(profile_toml) => run_backtest(&profile_toml, store),
        // The Studio compute-to-data run (PR-3). The arm is ALWAYS present (the `Request` schema is
        // in the client crate), but the actual run is served only by a `serve-datafusion` build:
        // `run_slice_verb` is cfg-split so a default build answers with a clean error and references
        // NO vike-studio-core.
        // `slice` is boxed in the variant (enum-size hygiene); unbox it for the verb.
        Request::RunSlice { spec, slice, params } => run_slice_verb(spec, *slice, params, store),
        // The Studio SWEEP / WALK-FORWARD runs (PR-4) — same cfg-split shape as `RunSlice` above:
        // the arm always decodes, but `run_sweep_verb`/`run_walkforward_verb` are cfg-split so a lean
        // build answers with a clean error and names NO vike-studio-core type. `slice` is unboxed for
        // the verb, exactly like `RunSlice`.
        Request::RunSweep { spec, slice, sweep, params } => {
            run_sweep_verb(spec, *slice, sweep, params, store)
        }
        Request::RunWalkforward { spec, slice, walkforward, params } => {
            run_walkforward_verb(spec, *slice, walkforward, params, store)
        }
        // The PROFILE-shaped sweep / walk-forward (v7): the siblings of `RunBacktest`. NO cfg-split
        // — unlike the Studio verbs above, these delegate to the `vike_backtest::harness`, which
        // compiles against the `HistStore` TRAIT, so a LEAN (no-`serve-datafusion`) build serves
        // them exactly as it already serves `RunBacktest`.
        Request::RunSweepProfile { profile_toml, rank_by } => {
            run_sweep_profile(&profile_toml, rank_by.as_deref(), store)
        }
        Request::RunWalkforwardProfile { profile_toml } => {
            run_walkforward_profile(&profile_toml, store)
        }
        Request::LoadBars { venue, symbol, interval, start, end } => {
            match store.load_bars(&venue, &symbol, &interval, TsRange { start, end }) {
                Ok(bars) => Response::Bars(bars),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::ScanQuotes { venue, symbol, start, end } => {
            match store.scan_quotes(&venue, &symbol, TsRange { start, end }) {
                Ok(quotes) => Response::Quotes(quotes),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::ScanTrades { venue, symbol, start, end } => {
            match store.scan_trades(&venue, &symbol, TsRange { start, end }) {
                Ok(trades) => Response::Trades(trades),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        Request::PropertiesAsOf { venue, symbol, ts } => {
            match store.properties_as_of(&venue, &symbol, ts) {
                // Box the payload — `Response::Properties` boxes `SymbolProperties` to keep the enum
                // small (see the proto); serde treats the box transparently on the wire.
                Ok(props) => Response::Properties(props.map(Box::new)),
                Err(e) => Response::Error(e.to_string()),
            }
        }
        // Store-metadata verbs (PR-6). Unlike the `Run*` verbs these need NO `serve-datafusion` split:
        // they call `HistStore` TRAIT methods (the real manifest walk on a DataFusion backend, the
        // in-memory catalog fold on the `MemHistStore` double; a store WITHOUT the catalog verbs
        // refuses, and that refusal rides `Response::Error` like any other store error rather than
        // being served as a fabricated empty catalog), so a lean build compiles + answers them
        // without pulling DataFusion. The catalog is tiny (no Parquet scan), so shipping it whole
        // is safe.
        Request::ListSeries => match store.list_series() {
            Ok(series) => Response::SeriesList(series),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::Inventory => match store.inventory() {
            Ok(inv) => Response::Inventory(inv),
            Err(e) => Response::Error(e.to_string()),
        },
        Request::SeriesGaps { id } => match store.series_gaps(&id) {
            Ok(gaps) => Response::SeriesGaps(gaps),
            Err(e) => Response::Error(e.to_string()),
        },
        // The cross-kind coverage report (split-plane spec §6 Q2) — the FOURTH store-metadata verb,
        // and served exactly like its three siblings above: a `HistStore` TRAIT call, so no
        // `serve-datafusion` split and no second store handle. That the trait carries it is the
        // whole reason this arm is one line: had the report stayed a concrete `DataFusionHist`
        // method, serving it would have meant threading a SECOND, concrete store through `serve`
        // (the `BackfillTable` shape) purely to reach a manifest fold the trait can express.
        Request::Coverage => match store.coverage_report() {
            Ok(report) => Response::Coverage(report),
            Err(e) => Response::Error(e.to_string()),
        },
        // The strategy-roster verb. Store-independent and DataFusion-free (the roster is a
        // compile-time `&[&str]` const), so — like the store-metadata verbs — it is served on EVERY
        // build; own the `&str`s into `String`s for the wire.
        Request::ListStrategies => {
            Response::Strategies(harness::STRATEGIES.iter().map(|s| s.to_string()).collect())
        }
        // Backfill-on-demand (split-plane REQ-9). The arm always DECODES (the schema lives in the
        // client crate) so a mismatched client is never dropped; whether it SERVES depends on the
        // mounted table — `None` (a default or plain `serve-datafusion` build) is a clean refusal
        // naming the missing feature, the recorder's `missing_feature` idiom.
        Request::Backfill { venue, symbol, interval, start, end } => {
            backfill_verb(&venue, &symbol, &interval, start, end, backfill, store)
        }
    }
}

/// The `Backfill` verb: validate the bounded range, dispatch venue → collector through the mounted
/// [`BackfillTable`], then read the range BACK through the served store handle — the write-through
/// proof — and answer [`Response::BackfillDone`]. Every failure (no table, unknown venue, inverted
/// range, collector error, read-back error) is a clean [`Response::Error`], so the client always
/// learns the outcome; a partial `BackfillDone` is never sent.
fn backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    start: i64,
    end: i64,
    table: Option<&BackfillTable>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    let Some(table) = table else {
        return Response::Error(format!(
            "backfill: not compiled into this build — rebuild vike-datahub with \
             `--features backfill-serve`. (It is a Cargo feature because the collectors are \
             heavy: they pull the venue bridge crates into this binary.) The `{FEATURE_BACKFILL}` \
             capability is deliberately absent from this server's Welcome.features."
        ));
    };
    if start > end {
        return Response::Error(format!(
            "backfill: inverted range — start {start} > end {end} (v1 takes one bounded \
             inclusive epoch-ms range per request)"
        ));
    }
    let Some(collector) = table.get(venue) else {
        return Response::Error(format!(
            "backfill: venue `{venue}` has no collector in this build. Supported: [{}]",
            table.supported().join(", ")
        ));
    };
    // Collector runs INLINE (v1 is synchronous per request; the collectors page + pace
    // internally), writing through the same store this server serves.
    let rows_written = match collector(symbol, interval, start, end) {
        Ok(n) => n as u64,
        Err(e) => {
            return Response::Error(format!("backfill {venue}/{symbol}@{interval} failed: {e}"))
        }
    };
    // Write-through proof + the client's seam bookkeeping: what the requested range now holds,
    // read through the SERVED handle — the same read the client's follow-up LoadBars would do.
    match store.load_bars(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(bars) => Response::BackfillDone(BackfillDone {
            rows_written,
            first_ts: bars.first().map(|b| b.ts),
            last_ts: bars.last().map(|b| b.ts),
        }),
        Err(e) => Response::Error(format!(
            "backfill {venue}/{symbol}@{interval}: collector wrote {rows_written} rows but the \
             read-back failed: {e}"
        )),
    }
}

/// The `RunSlice` verb (PR-3), served by a `serve-datafusion` build: convert the DTOs, run
/// `vike_studio_core::run::run_slice` next to the data (via the re-exported [`crate::wire_run::run_slice_local`],
/// the SAME entry the parity test drives), and answer with [`Response::RunResult`] — or a
/// [`Response::Error`] carrying the classified, kind-first run-failure string.
#[cfg(feature = "serve-datafusion")]
fn run_slice_verb(
    spec: WireSpec,
    slice: WireSlice,
    params: Option<WireEngineParams>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    match crate::wire_run::run_slice_local(&spec, &slice, params.as_ref(), Arc::clone(store)) {
        Ok(result) => Response::RunResult(result),
        Err(e) => Response::Error(e.to_error_string()),
    }
}

/// `RunSlice` on a LEAN (no-`serve-datafusion`) build: the request still DECODES (the schema lives in
/// the client crate) so a mismatched client is never dropped, but there is no engine to run it — the
/// arm answers with a clean error and, deliberately, names NO vike-studio-core type, so a default
/// build compiles without pulling DataFusion.
#[cfg(not(feature = "serve-datafusion"))]
fn run_slice_verb(
    _spec: WireSpec,
    _slice: WireSlice,
    _params: Option<WireEngineParams>,
    _store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    Response::Error("RunSlice requires the serve-datafusion build".to_string())
}

/// The `RunSweep` verb (PR-4), served by a `serve-datafusion` build: convert the DTOs, run the sweep
/// grid next to the data (via the re-exported [`crate::wire_run::run_sweep_local`], the SAME entry the
/// parity test drives), and answer with [`Response::SweepResult`] — or a [`Response::Error`] carrying
/// the classified, kind-first run-failure string.
#[cfg(feature = "serve-datafusion")]
fn run_sweep_verb(
    spec: WireSpec,
    slice: WireSlice,
    sweep: WireSweep,
    params: Option<WireEngineParams>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    match crate::wire_run::run_sweep_local(
        &spec,
        &slice,
        &sweep,
        params.as_ref(),
        Arc::clone(store),
    ) {
        Ok(result) => Response::SweepResult(result),
        Err(e) => Response::Error(e.to_error_string()),
    }
}

/// `RunSweep` on a LEAN (no-`serve-datafusion`) build: the request still DECODES so a mismatched
/// client is never dropped, but there is no engine to run it — answer with a clean error and,
/// deliberately, name NO vike-studio-core type so a default build compiles without pulling DataFusion.
#[cfg(not(feature = "serve-datafusion"))]
fn run_sweep_verb(
    _spec: WireSpec,
    _slice: WireSlice,
    _sweep: WireSweep,
    _params: Option<WireEngineParams>,
    _store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    Response::Error("RunSweep requires the serve-datafusion build".to_string())
}

/// The `RunWalkforward` verb (PR-4), served by a `serve-datafusion` build: convert the DTOs, walk the
/// strategy forward next to the data (via the re-exported [`crate::wire_run::run_walkforward_local`]),
/// and answer with [`Response::WalkforwardResult`] — or a [`Response::Error`] carrying the classified,
/// kind-first run-failure string.
#[cfg(feature = "serve-datafusion")]
fn run_walkforward_verb(
    spec: WireSpec,
    slice: WireSlice,
    walkforward: WireWalkforward,
    params: Option<WireEngineParams>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    match crate::wire_run::run_walkforward_local(
        &spec,
        &slice,
        &walkforward,
        params.as_ref(),
        Arc::clone(store),
    ) {
        Ok(result) => Response::WalkforwardResult(result),
        Err(e) => Response::Error(e.to_error_string()),
    }
}

/// `RunWalkforward` on a LEAN build: the request decodes but cannot run — a clean error, naming NO
/// vike-studio-core type, so a default build compiles without DataFusion.
#[cfg(not(feature = "serve-datafusion"))]
fn run_walkforward_verb(
    _spec: WireSpec,
    _slice: WireSlice,
    _walkforward: WireWalkforward,
    _params: Option<WireEngineParams>,
    _store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    Response::Error("RunWalkforward requires the serve-datafusion build".to_string())
}

/// The verbs THIS server answers, advertised in the [`Response::Welcome`] handshake. Kept in sync
/// with the arms of [`handle_request`]: `backtest` (the compute-to-data run) and its v7 profile
/// siblings `run_sweep_profile`/`run_walkforward_profile`, plus the four `HistStore` read verbs the
/// `RemoteHistStore` seam consumes and the four store-metadata verbs beside them
/// (`list_series`/`inventory`/`series_gaps`/[`FEATURE_COVERAGE`]) — and, ONLY on a `serve-datafusion` build, the Studio run verbs
/// `run_slice`/`run_sweep`/`run_walkforward` (a lean build decodes them but cannot serve them, so
/// they are not advertised).
///
/// `has_backfill` is a RUNTIME fact, not a cfg: [`FEATURE_BACKFILL`] is advertised exactly when a
/// collector table is MOUNTED, because "compiled with `backfill-serve`" and "can actually serve a
/// backfill" are the same thing only when the bin wired a table in — a `serve()` entry inside a
/// `backfill-serve` test build still must not advertise what it will refuse.
fn served_features(has_backfill: bool, requires_auth: bool) -> Vec<String> {
    #[allow(unused_mut)]
    let mut features = vec![
        "backtest".to_string(),
        "load_bars".to_string(),
        "scan_quotes".to_string(),
        "scan_trades".to_string(),
        "properties_as_of".to_string(),
        // PR-6 store-metadata verbs — served on EVERY build (HistStore trait methods, no DataFusion).
        "list_series".to_string(),
        "inventory".to_string(),
        "series_gaps".to_string(),
        // Their §6-Q2 sibling, likewise a trait verb on every build. UNCONDITIONAL, unlike
        // `backfill` below: there is no table to mount and nothing runtime about it, so the only
        // question its advertisement answers is "is this server older than the verb" — which is
        // precisely what the GUI's Partial column needs in order to choose between the wire answer
        // and an honest note.
        FEATURE_COVERAGE.to_string(),
        // The strategy-roster verb — store-independent, served on EVERY build.
        "list_strategies".to_string(),
        // The v7 PROFILE-shaped sweep / walk-forward verbs — served on EVERY build, exactly like
        // `backtest`: they run the `vike_backtest::harness` against the `HistStore` TRAIT, with no
        // DataFusion and no vike-studio-core (unlike the Studio `run_sweep`/`run_walkforward` below).
        "run_sweep_profile".to_string(),
        "run_walkforward_profile".to_string(),
    ];
    #[cfg(feature = "serve-datafusion")]
    features.push("run_slice".to_string());
    #[cfg(feature = "serve-datafusion")]
    features.push("run_sweep".to_string());
    #[cfg(feature = "serve-datafusion")]
    features.push("run_walkforward".to_string());
    // The backfill-on-demand verb — advertised per MOUNTED TABLE, not per cfg (see the doc above).
    // This advertisement is the verb's whole negotiation: it shipped without a PROTO_VERSION bump.
    if has_backfill {
        features.push(FEATURE_BACKFILL.to_string());
    }
    // ⚠ The AUTH advertisement, and it must be LAST-but-conditional in exactly this way: a key-less
    // server never pushes it, so its whole `Welcome` — features included — is byte-identical to the
    // pre-auth protocol's. That identity is the backward-compatibility contract, not a nicety, and
    // `a_keyless_servers_welcome_is_byte_identical_to_the_pre_auth_protocol` is what holds it.
    // On a KEYED server this string is how a client learns authentication is mandatory here BEFORE
    // it sends a verb it would only be refused for.
    if requires_auth {
        features.push(FEATURE_AUTH.to_string());
    }
    features
}

/// Parse the wire profile TOML, run it over `store`, and return the [`BacktestReport`] as JSON text.
///
/// `BacktestProfile::from_toml_str` parses AND validates (bad range, empty slice, cross-venue snap,
/// …) in one step — the exact guard the file path applies — so a malformed profile becomes a clean
/// [`Response::Error`] before the engine is touched. A run failure (missing strategy, data error,
/// resolution build error) or a report-serialize failure likewise becomes `Response::Error`, so the
/// caller always learns the outcome.
fn run_backtest(profile_toml: &str, store: &Arc<dyn HistStore + Send + Sync>) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    let result = match harness::run_backtest(&profile, Arc::clone(store)) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    let report = BacktestReport::from_result(
        profile.name.clone(),
        &result,
        harness::report::periods_per_year(&profile),
    );
    match serde_json::to_string(&report) {
        Ok(json) => Response::Report(json),
        Err(e) => Response::Error(format!("report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML, expand + run its `[sweep]` grid over `store`, and return the RANKED
/// [`harness::SweepReport`] as JSON text (v7).
///
/// One authority for the profile, exactly like [`run_backtest`]: `BacktestProfile::from_toml_str`
/// parses AND validates — the same guard the file path applies — and the run is the EXISTING
/// `harness::run_sweep`, so a remote sweep and a local `backtest --profile … --rank-by …` are the
/// same computation over the same `[engine]` (fee schedule included).
///
/// RANKING HAPPENS HERE, over the real `BacktestReport`s, using the canonical
/// [`harness::RankMetric`] comparator (NaN-last) — so no client re-implements sharpe/return/max_dd.
/// An unrecognized `rank_by` is a clean [`Response::Error`] naming the valid set, never a silent
/// fallback to the default metric.
fn run_sweep_profile(
    profile_toml: &str,
    rank_by: Option<&str>,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    let rank_by = match rank_by {
        Some(name) => match harness::RankMetric::from_str_ci(name) {
            Some(m) => m,
            None => {
                return Response::Error(format!(
                    "unknown rank_by {name:?} — expected sharpe|return|max_dd|equity"
                ))
            }
        },
        // The `backtest --rank-by` default, so an omitted field ranks like the shipped bin.
        None => harness::RankMetric::Sharpe,
    };
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    if !profile.is_sweep() {
        return Response::Error(
            "profile has no [sweep] table — a sweep needs a parameter grid, e.g. \
             `[sweep]\nfast = [5, 10, 15]`"
                .to_string(),
        );
    }
    let report = match harness::run_sweep(&profile, Arc::clone(store), rank_by) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::SweepReport(json),
        Err(e) => Response::Error(format!("sweep report serialize failed: {e}")),
    }
}

/// Parse the wire profile TOML and walk it forward over its `[walkforward].n_splits` anchored
/// out-of-sample windows via the EXISTING `harness::run_walkforward`, returning the stitched
/// `WalkForwardReport` as JSON text (v7). Same one-parser contract as [`run_backtest`]; every
/// failure (missing `[walkforward]`, tick/multi-symbol profile, empty slice) is a clean
/// [`Response::Error`].
fn run_walkforward_profile(
    profile_toml: &str,
    store: &Arc<dyn HistStore + Send + Sync>,
) -> Response {
    let profile = match BacktestProfile::from_toml_str(profile_toml) {
        Ok(p) => p,
        Err(e) => return Response::Error(format!("profile parse/validate failed: {e}")),
    };
    let report = match harness::run_walkforward(&profile, Arc::clone(store)) {
        Ok(r) => r,
        Err(e) => return Response::Error(e.to_string()),
    };
    match serde_json::to_string(&report) {
        Ok(json) => Response::WalkforwardReport(json),
        Err(e) => Response::Error(format!("walk-forward report serialize failed: {e}")),
    }
}

/// The bind posture — the guard that keeps "reachability is the ONLY barrier" (module doc) true by
/// construction rather than by convention. The ADDRESS half mirrors `vike_tradehub::server`'s
/// `exposure_tests` name-for-name (minus its pre-auth frame test, which polices a handshake cap
/// this protocol does not have): the two backend daemons must answer "is this reachable off-box"
/// the same way. The AUTH half below has no tradehub twin and cannot have one — that node server
/// cannot be key-less (its `serve` takes `keys: NodeKeys` by value, and `start_observe_server`
/// opens no listener without an observe key), so the case these tests cover does not exist there.
#[cfg(test)]
mod exposure_tests {
    use super::*;
    use std::net::ToSocketAddrs;

    fn resolve(addr: &str) -> Vec<SocketAddr> {
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default()
    }

    /// A `NodeKeys` with an observe key and no control key — the SHAPE that matters here (the
    /// guard asks only whether this process authenticates at all, never which scopes it serves),
    /// and deliberately the partially-configured one: a missing scope key is a closed gate, so it
    /// must still classify [`ServerAuth::Keyed`].
    fn some_keys() -> NodeKeys {
        NodeKeys::new(b"observe-key-bytes".to_vec(), Vec::new())
    }

    /// The DEFAULT is loopback and must stay so: on a key-less server — still the default — it is
    /// the entire barrier, not a convenience.
    #[test]
    fn the_default_address_is_loopback() {
        assert_eq!(bind_exposure(&resolve(DEFAULT_ADDR)), BindExposure::Loopback);
    }

    /// ⚠ The wildcards are the whole point. `0.0.0.0` / `::` are what an operator types when they
    /// want "reachable from my laptop", and they are NOT loopback — `IpAddr::is_loopback` is false
    /// for both — so they must classify as exposed, or the guard would pass the single most common
    /// way this surface gets published to a network.
    #[test]
    fn the_wildcard_binds_are_public_not_loopback() {
        for addr in ["0.0.0.0:7878", "[::]:7878"] {
            assert!(
                matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)),
                "{addr} must classify as exposed — it listens on EVERY interface"
            );
        }
    }

    #[test]
    fn loopback_spellings_are_all_loopback_and_a_routable_ip_is_not() {
        // 127.0.0.0/8 in full, both families, and the name.
        for addr in ["127.0.0.1:7878", "127.9.9.9:7878", "[::1]:7878", "localhost:7878"] {
            assert_eq!(bind_exposure(&resolve(addr)), BindExposure::Loopback, "{addr}");
        }
        for addr in ["<host>:7878", "<host>:7878", "[2001:db8::1]:7878"] {
            assert!(matches!(bind_exposure(&resolve(addr)), BindExposure::Public(_)), "{addr}");
        }
    }

    /// ⚠ **THE GUARD ITSELF.** Without an opt-in a non-loopback bind is REFUSED, not warned about:
    /// a `warn!` in front of an unauthenticated read/compute/write surface is a line in a log file
    /// nobody reads until afterwards. With the opt-in AND node keys it proceeds, and says so.
    #[test]
    fn a_public_bind_is_refused_without_the_opt_in_and_announced_with_it() {
        let public = resolve("0.0.0.0:7878");
        let exposed = public[0];
        let keys = some_keys();
        let keyed = ServerAuth::of(Some(&keys));
        assert_eq!(
            bind_decision(&public, false, keyed),
            BindDecision::Refuse(exposed),
            "default (no VIKE_DATAHUB_ALLOW_PUBLIC_BIND) must REFUSE a wildcard bind"
        );
        assert_eq!(
            bind_decision(&public, true, keyed),
            BindDecision::ProceedExposed(exposed),
            "the named opt-in permits it ON A KEYED SERVER — and the decision still carries what is \
             exposed, so the bin can name it in the warning it logs"
        );
    }

    /// ⚠ **THE SECOND HALF OF THE GUARD, and the one this file shipped without.**
    /// `VIKE_DATAHUB_ADDR=0.0.0.0` + `VIKE_DATAHUB_ALLOW_PUBLIC_BIND=1` + no node keys used to be
    /// `ProceedExposed` behind a `warn!`: a publicly reachable server that answers history, COMPILES
    /// CLIENT-SUPPLIED RHAI (the `Run*` verbs) and — in a `backfill-serve` build — WRITES the store,
    /// gated by one log line. `docs/decisions/0025-datahub-remote-posture.md` names that verbatim as
    /// "the exact state this record exists to prevent". The opt-in consents to REACHABILITY; it is
    /// not consent to serve all of that unauthenticated, so the two knobs COMPOSE.
    #[test]
    fn a_public_bind_is_refused_on_a_keyless_server_even_with_the_opt_in() {
        let public = resolve("0.0.0.0:7878");
        let exposed = public[0];
        // Its OWN variant, not `ProceedExposed` (the old answer) and not the plain `Refuse`: the
        // two refusals have different FIXES — "keep it on loopback" vs "configure the node keys" —
        // and the bin can only name the right one if the verdict distinguishes them.
        assert_eq!(
            bind_decision(&public, true, ServerAuth::Keyless),
            BindDecision::RefuseUnauthenticated(exposed),
            "the opt-in must NOT be enough on a server that authenticates nothing"
        );
        // …and with no opt-in either, the FIRST thing to say is still "keep it on loopback", so
        // that arm is unchanged by key presence in either direction.
        let keys = some_keys();
        for auth in [ServerAuth::Keyless, ServerAuth::of(Some(&keys))] {
            assert_eq!(
                bind_decision(&public, false, auth),
                BindDecision::Refuse(exposed),
                "{auth:?}: no opt-in is the pre-existing refusal, whatever the keys say"
            );
        }
    }

    /// `ServerAuth::of` reads the value the bin is about to hand `serve_authed`, and `Some(_)` is
    /// KEYED even when only one scope has a key — `run_handshake` refuses an unkeyed scope at
    /// `!keys.has(scope)`, before the key is consulted — so such a server still answers no verb to
    /// an unauthenticated peer. (NOT because an empty key fails verification; it does not.)
    #[test]
    fn server_auth_classifies_a_partially_keyed_server_as_keyed() {
        let observe_only = NodeKeys::new(b"observe".to_vec(), Vec::new());
        let control_only = NodeKeys::new(Vec::new(), b"control".to_vec());
        let both = NodeKeys::new(b"observe".to_vec(), b"control".to_vec());
        for keys in [&observe_only, &control_only, &both] {
            assert_eq!(ServerAuth::of(Some(keys)), ServerAuth::Keyed, "{keys:?}");
        }
        assert_eq!(
            ServerAuth::of(None),
            ServerAuth::Keyless,
            "`None` — the value that makes `serve_authed` serve every verb to whoever connects — is \
             the ONLY key-less shape"
        );
    }

    /// …and the opt-in changes NOTHING for a loopback bind: it is not a general "skip the checks"
    /// switch, so leaving it set on a host later moved back to loopback is inert.
    ///
    /// ⚠ Neither does `auth`, and that is load-bearing rather than incidental: a KEY-LESS LOOPBACK
    /// datahub is the ordinary developer configuration (`cargo run -p vike-datahub --features
    /// serve-datafusion` with an empty credential store), so all four combinations must `Proceed`
    /// or the guard would break every local flow it has no business touching.
    #[test]
    fn a_loopback_bind_proceeds_identically_with_or_without_the_opt_in() {
        let local = resolve(DEFAULT_ADDR);
        let keys = some_keys();
        for auth in [ServerAuth::Keyless, ServerAuth::of(Some(&keys))] {
            assert_eq!(bind_decision(&local, false, auth), BindDecision::Proceed, "{auth:?}");
            assert_eq!(bind_decision(&local, true, auth), BindDecision::Proceed, "{auth:?}");
            // An address that resolves to nothing is left to `bind` to reject with its own message,
            // rather than being reported as an exposure it is not — and a key-less server does not
            // turn that into a refusal either: nothing is exposed by an address that is not one.
            assert_eq!(bind_decision(&[], false, auth), BindDecision::Proceed, "{auth:?}");
            assert_eq!(bind_decision(&[], true, auth), BindDecision::Proceed, "{auth:?}");
        }
    }

    /// Every loopback SPELLING keeps the key-less pass, not just the default literal: an operator
    /// who writes `localhost:7878` or `[::1]:7878` into `VIKE_DATAHUB_ADDR` has not exposed
    /// anything, so the new refusal must not fire on any of them.
    #[test]
    fn a_keyless_server_still_binds_every_loopback_spelling() {
        for addr in ["127.0.0.1:7878", "127.9.9.9:7878", "[::1]:7878", "localhost:7878"] {
            assert_eq!(
                bind_decision(&resolve(addr), true, ServerAuth::Keyless),
                BindDecision::Proceed,
                "{addr} is loopback — a key-less server must still bind it"
            );
        }
    }

    /// A hostname resolving to both loopback and a routable address is EXPOSED, so a key-less
    /// server must be refused on it too — the mixed-resolution case reaching the new arm, which the
    /// wildcard tests above cannot show.
    #[test]
    fn a_mixed_resolution_is_refused_on_a_keyless_server_with_the_opt_in() {
        let mixed = vec![
            "127.0.0.1:7878".parse::<SocketAddr>().unwrap(),
            "<host>:7878".parse::<SocketAddr>().unwrap(),
        ];
        assert_eq!(
            bind_decision(&mixed, true, ServerAuth::Keyless),
            BindDecision::RefuseUnauthenticated(mixed[1]),
            "the refusal names the ROUTABLE address, not the loopback one it was mixed with"
        );
    }

    /// A name resolving to BOTH loopback and a routable address is exposed on that address, so the
    /// strictest reading is the true one. (Constructed directly: no DNS in a unit test.)
    #[test]
    fn one_routable_address_among_loopbacks_is_still_public() {
        let mixed = vec![
            "127.0.0.1:7878".parse::<SocketAddr>().unwrap(),
            "<host>:7878".parse::<SocketAddr>().unwrap(),
        ];
        assert!(matches!(bind_exposure(&mixed), BindExposure::Public(_)));
        // …and "resolved to nothing" is its own answer, never silently read as exposed.
        assert_eq!(bind_exposure(&[]), BindExposure::Unresolvable);
    }
}
