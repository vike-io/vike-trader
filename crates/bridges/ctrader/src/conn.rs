//! The multiplexed cTrader connection actor: ONE protobuf-over-TLS socket carries both market
//! data and order flow. `connect_and_auth` performs the PROVEN-LIVE handshake
//! (ApplicationAuth -> GetAccountListByAccessToken -> AccountAuth -> Trader -> SymbolCategoryList
//! -> AssetClassList -> AssetList -> SymbolsList -> SymbolById)
//! and hands back an [`ActorHandle`] whose background thread owns the stream, dispatches inbound
//! frames, drains outbound [`Command`]s, and sends periodic heartbeats. Ports nothing — mirrors
//! the cTrader Open API (https://help.ctrader.com/open-api/) and the sequence proven by
//! `scratchpad/ctrader_catcher.py`.

use std::collections::HashMap;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;

use crate::framing::{self, FrameReader};
use crate::positions::{CloseTracker, PositionBook};
use crate::proto::{ProtoMessage, ProtoOaTrendbarPeriod, pt};
use crate::symbols::{SymbolLists, SymbolMap};

mod actor;
mod handshake;
mod inbound;
mod outbound;
mod reconcile;
mod session;
mod stream;

use handshake::{connect_and_auth_inner, open_and_read_lists};
use stream::{EofStream, Stream, decode_error};

#[cfg(doc)]
use handshake::open_and_handshake_retry;
#[cfg(doc)]
use inbound::update_position_map;
#[cfg(doc)]
use reconcile::{read_reconcile_res, reconcile_after_reconnect, seed_positions_at_connect};
#[cfg(doc)]
use session::{is_auth_error, reconnect_with_backoff};

/// Venue tag used on every `LiveDataSink` call and log line — matches the crate name / the
/// `credentials.rs` `{VENUE}_...` naming convention.
pub(crate) const VENUE: &str = "ctrader";

/// Shared `client_order_id → venue order_id` correlation map. cTrader's cancel/amend requests key
/// on the numeric `orderId`, which we only learn from execution events — the actor thread writes
/// this map as those events arrive; [`crate::exec::CtraderExec`] reads it to resolve a coid to an
/// order id for cancel/modify. `Arc<Mutex<..>>` because both threads touch it.
pub type OrderIdMap = Arc<Mutex<HashMap<String, i64>>>;

/// Shared book of the account's currently-OPEN positions — see [`crate::positions::PositionBook`]
/// for why it is a struct with a `fetched_at_ms` rather than a bare map. The actor keeps it fresh
/// from every execution event's `ProtoOAExecutionEvent.position`, SEEDS it at initial connect and
/// re-seeds it from a reconnect reconcile; [`crate::exec::CtraderExec`] reads a snapshot of it to
/// decide whether a submit REDUCES an open position (→ close-by-position-id) or OPENS one (→ new
/// order), and — under `vike_model::HaltAdmit::Verify` — whether a halted submit has anything to
/// reduce (`crates/bridges/ctrader/src/exec.rs`'s `halt_evidence`). `Arc<Mutex<..>>` because both
/// threads touch it.
pub type PositionMap = Arc<Mutex<PositionBook>>;

/// Shared per-coid aggregation state for in-flight `ProtoOAClosePositionReq` closes (see
/// [`crate::positions::CloseTracker`]). `CtraderExec::submit` registers a planned close here; the
/// actor folds the venue's closing execution events against it to emit one coherent
/// `OrderAccepted → OrderPartiallyFilled* → OrderFilled` lifecycle for the reduce order.
pub type CloseState = Arc<Mutex<CloseTracker>>;

/// How long the handshake waits for each expected response before giving up.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Heartbeat cadence for the actor loop (cTrader disconnects idle sockets). `pub(crate)`: also
/// the idle age [`crate::recon_client`] revives its own dedicated (heartbeat-less) connection at,
/// so the two idle policies are driven by ONE constant.
pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Bounded exponential-backoff policy for the INITIAL connect+auth (see [`open_and_handshake_retry`]),
/// so a transient network/TLS blip during startup does not permanently drop the venue to paper.
/// INERT on the happy path: a first-attempt success neither sleeps nor re-sends a frame, so a
/// healthy startup is byte-identical to a single [`open_and_handshake`] call — the retry only
/// engages AFTER a transient failure ([`ConnError::is_transient`]). A PERMANENT auth rejection fails
/// fast regardless of `max_attempts`. Distinct from [`reconnect_with_backoff`], the UNBOUNDED
/// mid-session reconnect loop for an already-authenticated socket that drops.
#[derive(Clone, Copy, Debug)]
pub struct ConnectRetry {
    /// Total connect+auth attempts before giving up (clamped to ≥ 1 by the loop; `1` disables retry
    /// entirely — the pre-existing single-shot behavior). Default 4.
    pub max_attempts: u32,
    /// Backoff before the FIRST retry (after attempt 1 fails); doubles each subsequent retry, capped
    /// at [`max_backoff`](Self::max_backoff). Default 250ms.
    pub initial_backoff: Duration,
    /// Ceiling on the per-retry backoff. Default 2s — with the defaults, 4 attempts wait
    /// 250ms + 500ms + 1s ≈ 1.75s in total before the terminal failure/paper-fallback.
    pub max_backoff: Duration,
}

impl Default for ConnectRetry {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(2),
        }
    }
}

/// Connection configuration. Production builds default `no_tls = false` (rustls); tests use
/// [`ConnConfig::for_test`] against the in-process plaintext fake server. `Debug` is manually
/// implemented to redact `client_secret`/`access_token` — never let a secret leak into a log line
/// or panic message (mirrors `oauth::Token`'s manual `Debug`).
#[derive(Clone)]
pub struct ConnConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub client_secret: String,
    pub access_token: String,
    /// The OAuth2 refresh token, when known — used by the actor loop to call `oauth::refresh` on
    /// an `ACCOUNTS_TOKEN_INVALIDATED_EVENT`/auth `ERROR_RES` (Task 6). `None` means the actor
    /// cannot self-heal a token invalidation and degrades (stops) instead of guessing.
    pub refresh_token: Option<String>,
    /// If set, skip account discovery and authorize this ctid directly.
    pub account_id: Option<i64>,
    /// Skip the rustls wrap and speak plaintext (in-process test server only).
    pub no_tls: bool,
    /// Bounded connect-retry policy for the INITIAL connect+auth ([`open_and_handshake_retry`]). The
    /// [`Default`] gives a healthy startup byte-identical behavior (first attempt succeeds, no
    /// backoff) while riding out a transient blip; the reconnect path keeps its own separate loop.
    pub connect_retry: ConnectRetry,
    /// Where a REFRESHED grant is written back — the credential store and the account row whose two
    /// grant fields it rotates. `None` keeps the historical behaviour exactly: a refresh lives as
    /// long as the process.
    ///
    /// See [`crate::token_store`]: cTrader ROTATES the refresh token on every refresh, so a
    /// process that does not persist leaves the store carrying a pair that has already been spent.
    pub token_persist: Option<crate::token_store::TokenPersist>,
    /// Wall-clock ms at which [`ConnConfig::access_token`] lapses, when known. Drives the PROACTIVE
    /// refresh on the actor's heartbeat tick; `None` (a process that has not refreshed yet, so it
    /// has never seen an `expiresIn`) leaves only the reactive path, unchanged.
    pub token_expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for ConnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &if self.refresh_token.is_some() { "<redacted>" } else { "None" },
            )
            .field("account_id", &self.account_id)
            .field("no_tls", &self.no_tls)
            .field("connect_retry", &self.connect_retry)
            // A PATH and an account row id — no secret. See `token_store::TokenPersist`.
            .field("token_persist", &self.token_persist)
            .field("token_expires_at_ms", &self.token_expires_at_ms)
            .finish()
    }
}

impl ConnConfig {
    /// Production config for a real cTrader endpoint (e.g. `demo.ctraderapi.com:5035`, TLS on).
    pub fn new(
        host: impl Into<String>,
        port: u16,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            port,
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            access_token: access_token.into(),
            refresh_token: None,
            account_id: None,
            no_tls: false,
            connect_retry: ConnectRetry::default(),
            token_persist: None,
            token_expires_at_ms: None,
        }
    }

    /// Plaintext config pointed at an in-process fake server (`no_tls = true`).
    pub fn for_test(
        addr: SocketAddr,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        access_token: impl Into<String>,
    ) -> Self {
        Self {
            host: addr.ip().to_string(),
            port: addr.port(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            access_token: access_token.into(),
            refresh_token: None,
            account_id: None,
            no_tls: true,
            connect_retry: ConnectRetry::default(),
            token_persist: None,
            token_expires_at_ms: None,
        }
    }
}

/// Outbound commands the actor loop encodes and writes onto the socket.
pub enum Command {
    /// Subscribe to spot (bid/ask tick) events for a symbol id.
    SubscribeSpots { symbol_id: i64 },
    /// Unsubscribe from spot events for a symbol id.
    UnsubscribeSpots { symbol_id: i64 },
    /// Subscribe to live trend bars for a symbol id at a given period. cTrader requires an
    /// active spot subscription on the same symbol for live trendbar pushes to arrive
    /// (`ProtoOASubscribeLiveTrendbarReq` doc) — `data::CtraderData::subscribe_bars` sends
    /// `SubscribeSpots` first.
    SubscribeTrendbar { symbol_id: i64, period: ProtoOaTrendbarPeriod },
    /// Unsubscribe from live trend bars for a symbol id at a given period.
    UnsubscribeTrendbar { symbol_id: i64, period: ProtoOaTrendbarPeriod },
    /// Fetch historical trend bars (`ProtoOAGetTrendbarsReq`) for `symbol_id`/`period`, bounded to
    /// `[from_ts, to_ts]` (epoch ms) and capped at `count` bars — the one-shot historical seed
    /// `data::CtraderData::subscribe_bars` fires before `SubscribeTrendbar`'s live stream starts
    /// (F2), so a fresh subscription seeds with recent closed bars instead of starting blank.
    /// Fire-and-forget like every other `Command`: the response (`GET_TRENDBARS_RES`) is routed by
    /// `on_inbound` straight to `LiveDataSink::seed_bars`, never blocking the caller thread.
    GetTrendbars {
        symbol_id: i64,
        period: ProtoOaTrendbarPeriod,
        from_ts: i64,
        to_ts: i64,
        count: u32,
    },
    /// Place a new order (fully-formed by the exec client, Task 5).
    NewOrder(crate::proto::ProtoOaNewOrderReq),
    /// Cancel a pending order by id. `client_order_id` is carried alongside the venue `order_id`
    /// (Task 6) purely so a write failure can synthesize the coid-keyed `OrderCancelRejected` the
    /// venue-adapter contract requires — it plays no role in the wire request itself.
    CancelOrder { order_id: i64, client_order_id: String },
    /// Amend a pending order (fully-formed by the exec client, Task 5). `client_order_id` is
    /// carried alongside the request for the same write-failure-synthesis reason as
    /// [`Command::CancelOrder`].
    AmendOrder { client_order_id: String, req: crate::proto::ProtoOaAmendOrderReq },
    /// Close (or partially close) an OPEN position by its numeric `position_id` — the reduce/flatten
    /// verb (`ProtoOAClosePositionReq`). `volume` is the centi-unit amount to close (≤ the position's
    /// own volume); `client_order_id` is the coid of the reducing submit these close legs correlate
    /// to (both for the write-failure reject the venue-adapter contract requires AND for the
    /// `on_error_res` envelope echo).
    ClosePosition { position_id: i64, volume: i64, client_order_id: String },
    /// Ask the actor thread to send Shutdown and exit its loop.
    Shutdown,
}

/// Handle returned once authenticated: the command channel, the shared symbol map, the resolved
/// ctid, and the join handle for the background actor thread (joined on drop after Shutdown).
pub struct ActorHandle {
    pub tx: mpsc::Sender<Command>,
    pub symbols: Arc<SymbolMap>,
    pub ctid: i64,
    /// coid→venue-order-id map the actor keeps updated from execution events (Task 5); shared with
    /// the exec client so it can resolve a coid for cancel/modify. Empty when exec is not wired.
    pub orders: OrderIdMap,
    /// positionId→open-position map the actor keeps fresh from execution events; the exec client
    /// reads it to route a reducing submit to close-by-position-id. Empty when exec is not wired.
    pub positions: PositionMap,
    /// Per-coid in-flight close aggregation state (shared with the exec client, which registers a
    /// planned close before enqueuing its `ClosePosition` legs).
    pub closes: CloseState,
    join: Option<JoinHandle<()>>,
}

impl ActorHandle {
    /// Best-effort graceful stop: send Shutdown and join the actor thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// Clone the NON-owning, shareable slice of this connection — the command channel plus the
    /// handshake state ([`ConnShared`]) — for handing to [`crate::data::CtraderData::from_shared`]/
    /// [`crate::exec::CtraderExec::from_shared`]. Deliberately does NOT clone the join handle: the
    /// actor-thread lifecycle stays owned in exactly one place (this `ActorHandle`, or the
    /// [`crate::client::CtraderClient`] that wraps one), so a client view built from the returned
    /// `ConnShared` can be dropped without joining/killing the shared socket.
    pub fn shared(&self) -> ConnShared {
        ConnShared {
            tx: self.tx.clone(),
            symbols: self.symbols.clone(),
            ctid: self.ctid,
            orders: self.orders.clone(),
            positions: self.positions.clone(),
            closes: self.closes.clone(),
        }
    }
}

impl Drop for ActorHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The cloneable, NON-owning slice of an authenticated cTrader actor connection: the outbound
/// command channel (`Sender<Command>` is `Clone`), the shared symbol map, the resolved ctid, and
/// the shared coid→orderId correlation map. Handed to [`crate::data::CtraderData::from_shared`] and
/// [`crate::exec::CtraderExec::from_shared`] so a `DataClient` view AND an `ExecutionClient` view
/// can drive ONE actor/socket at once (F4 single-socket data+exec mount).
///
/// It deliberately omits the actor thread's [`JoinHandle`]: the thread lifecycle is owned in exactly
/// ONE place — an [`ActorHandle`] for the single-client entry points, or a
/// [`crate::client::CtraderClient`] for the combined mount — so dropping a view built from a
/// `ConnShared` never joins/closes the socket. Only dropping (or explicitly shutting down) that sole
/// owner tears the connection down.
#[derive(Clone)]
pub struct ConnShared {
    pub tx: mpsc::Sender<Command>,
    pub symbols: Arc<SymbolMap>,
    pub ctid: i64,
    pub orders: OrderIdMap,
    pub positions: PositionMap,
    pub closes: CloseState,
}

/// Connection/handshake failure. Never carries a secret.
#[derive(Debug)]
pub enum ConnError {
    Io(io::Error),
    Tls(String),
    Decode(String),
    /// The venue returned `ProtoOAErrorRes` during the handshake.
    Venue {
        error_code: String,
        description: String,
    },
    /// No expected frame of a given payload type arrived before the deadline.
    Timeout(u32),
    /// `GetAccountListByAccessToken` returned zero accounts.
    NoAccounts,
}

impl std::fmt::Display for ConnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnError::Io(e) => write!(f, "io: {e}"),
            ConnError::Tls(m) => write!(f, "tls: {m}"),
            ConnError::Decode(m) => write!(f, "decode: {m}"),
            ConnError::Venue { error_code, description } => {
                write!(f, "cTrader error {error_code}: {description}")
            }
            ConnError::Timeout(pt) => write!(f, "timed out waiting for payloadType {pt}"),
            ConnError::NoAccounts => write!(f, "no trading accounts for access token"),
        }
    }
}

impl ConnError {
    /// Is this an AUTH/TOKEN refusal — i.e. the kind a fresh OAuth grant would fix?
    ///
    /// Same substring rule as [`is_auth_error`]'s frame-level twin and for the same reason:
    /// cTrader publishes no closed enum of `errorCode` strings, so matching `AUTH`/`TOKEN`
    /// (case-insensitive) covers the documented codes (`CH_ACCESS_TOKEN_INVALID`,
    /// `OA_AUTH_TOKEN_EXPIRED`, …) without hardcoding a list that could drift from the wire.
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        match self {
            ConnError::Venue { error_code, .. } => {
                let code = error_code.to_ascii_uppercase();
                code.contains("AUTH") || code.contains("TOKEN")
            }
            _ => false,
        }
    }
}

impl std::error::Error for ConnError {}

impl ConnError {
    /// Whether this failure is worth retrying during the INITIAL connect ([`open_and_handshake_retry`]).
    ///
    /// TRANSIENT (retry) — a fresh attempt can clear it:
    /// - [`ConnError::Io`]: a failed TCP/TLS connect, a read timeout surfacing as an error, a
    ///   connection reset, or a peer that closed the socket mid-handshake (the deferred rustls
    ///   handshake also fails here, NOT as `Tls`). A network blip.
    /// - [`ConnError::Timeout`]: the venue accepted the socket but sent no expected response before
    ///   the deadline — a stalled peer a retry can get past.
    ///
    /// PERMANENT (fail fast) — the same inputs will fail the same way, so retrying only delays the
    /// paper fallback:
    /// - [`ConnError::Venue`]: the app/account auth was REJECTED (bad credentials, invalid/expired
    ///   token, unauthorized account). The crux permanent case — a bad secret never becomes good by
    ///   waiting.
    /// - [`ConnError::NoAccounts`]: the access token owns no non-live account; retrying cannot
    ///   conjure one.
    /// - [`ConnError::Decode`]: a protocol/schema mismatch — deterministic, not a blip.
    /// - [`ConnError::Tls`]: a rustls CONFIG error (provider/root-store/server-name setup), not a
    ///   handshake failure (those are `Io`); a deterministic misconfiguration.
    fn is_transient(&self) -> bool {
        matches!(self, ConnError::Io(_) | ConnError::Timeout(_))
    }
}

impl From<io::Error> for ConnError {
    fn from(e: io::Error) -> Self {
        ConnError::Io(e)
    }
}

pub(crate) type Reader = FrameReader<EofStream<Stream>>;

/// Encode `msg` as payload of a `pt`-typed frame and write it to the stream. `pub(crate)`: reused
/// by [`crate::recon_client`]'s dedicated reconcile connection (ReconFactory seam, wave-2 task 6)
/// — the SAME wire encode every request on this connection already uses, not a second one.
pub(crate) fn send<M: Message>(
    reader: &mut Reader,
    payload_type: u32,
    msg: &M,
    msg_id: &str,
) -> io::Result<()> {
    let body = msg.encode_to_vec();
    let frame = framing::encode(payload_type, &body, msg_id);
    reader.get_mut().write_all(&frame)?;
    reader.get_mut().flush()
}

/// Read frames until one of `want` payload type arrives (returned), an `ERROR_RES` arrives
/// (mapped to `Err`), or the deadline elapses. Other frames (heartbeats etc.) are skipped — the
/// handshake is strictly sequential. `pub(crate)`: also the blocking request/response primitive
/// [`crate::recon_client`]'s dedicated reconcile connection uses (ReconFactory seam, wave-2 task 6).
pub(crate) fn read_until(reader: &mut Reader, want: u32) -> Result<ProtoMessage, ConnError> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            return Err(ConnError::Timeout(want));
        }
        match reader.next_frame()? {
            Some(msg) if msg.payload_type == want => return Ok(msg),
            Some(msg) if msg.payload_type == pt::ERROR_RES => return Err(decode_error(&msg)),
            Some(_) => continue, // unrelated frame — skip
            None => {
                // A read that returned `Ok(0)` (peer closed the socket mid-handshake) sets the
                // `EofStream` flag; without this check the loop would busy-spin on `Ok(None)` for
                // the full HANDSHAKE_TIMEOUT, burning a core. Fail fast on a closed connection;
                // a genuine read timeout leaves `eof` false and falls through to retry.
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    return Err(ConnError::Io(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "connection closed during handshake",
                    )));
                }
                continue; // read timeout / partial — retry
            }
        }
    }
}

/// Decode a frame's payload into a concrete prost message. `pub(crate)`: reused by
/// [`crate::recon_client`] (ReconFactory seam, wave-2 task 6).
pub(crate) fn decode_payload<M: Message + Default>(msg: &ProtoMessage) -> Result<M, ConnError> {
    M::decode(msg.payload.as_deref().unwrap_or(&[])).map_err(|e| ConnError::Decode(e.to_string()))
}

/// Connect, run the two-stage auth handshake + symbol discovery, and spawn the actor thread.
/// `sink` is where the actor pushes live quotes/bars decoded from inbound `SPOT_EVENT` frames
/// (Task 4) — given here, at construction, rather than threaded through later calls, matching
/// `vike_data::DataClient`'s "sink at construction" contract. This data-only entry point leaves
/// order flow unwired; [`connect_and_auth_exec`] adds the analogous `EventSender` for exec events.
pub fn connect_and_auth(
    cfg: ConnConfig,
    sink: Arc<dyn LiveDataSink>,
) -> Result<ActorHandle, ConnError> {
    connect_and_auth_inner(cfg, sink, None)
}

/// Exec-enabled variant of [`connect_and_auth`]: additionally wires an [`EventSender`] so the
/// actor routes inbound `EXECUTION_EVENT` frames to the core ingest lane (via
/// `event_mapper::exec_event_to_events`) and keeps the returned handle's coid→order-id map updated. Pair
/// the returned [`ActorHandle`] with [`crate::exec::CtraderExec::new`] (share the same
/// `EventSender` clone, so its synchronous `OrderSubmitted`/synthetic-reject emits land on the same
/// lane). Order flow is off until this is used — `connect_and_auth` leaves exec unwired.
pub fn connect_and_auth_exec(
    cfg: ConnConfig,
    sink: Arc<dyn LiveDataSink>,
    events: EventSender,
) -> Result<ActorHandle, ConnError> {
    connect_and_auth_inner(cfg, sink, Some(events))
}

/// One-shot symbol enumeration: connect, run the full auth handshake + symbol discovery
/// ([`open_and_handshake`]), and return JUST the built [`SymbolMap`] — WITHOUT spawning the actor
/// thread or wiring any `LiveDataSink`/`EventSender`. The `Reader` (and the socket it owns) is
/// dropped on return, so no live data/exec session is ever started; this is the clean path the
/// [`crate::catalog::CtraderCatalog`] provider uses to enumerate the venue's symbols for the Symbol
/// picker (cTrader has no REST symbol-list endpoint — the protobuf session IS the list). Blocking:
/// runs the whole handshake inline on the calling thread and returns once `SymbolById` is decoded.
pub fn fetch_symbols(cfg: ConnConfig) -> Result<SymbolMap, ConnError> {
    Ok(SymbolMap::from_lists(&fetch_symbol_lists(cfg)?))
}

/// [`fetch_symbols`] before the map is built: the raw [`SymbolLists`] the handshake read (light
/// and full symbols plus the category / asset-class / asset reference lists), verbatim. The
/// read-only catalog smoke (`tests/ctrader_catalog_smoke.rs`) prints these to capture what a
/// broker really publishes; the same socket-drop guarantee holds.
pub fn fetch_symbol_lists(cfg: ConnConfig) -> Result<SymbolLists, ConnError> {
    let (_reader, _ctid, lists, _money_digits) = open_and_read_lists(&cfg, None)?;
    // `_reader` (hence the underlying TcpStream/TLS session) drops here — the socket closes and no
    // actor thread was ever spawned, so this leaves nothing running behind it.
    Ok(lists)
}

/// Connect + run the two-stage auth handshake + symbol discovery, returning a ready [`Reader`]
/// plus the resolved ctid, a freshly-built [`SymbolMap`], and the account's `moneyDigits` (the
/// exponent commission/balance integers are scaled by — see `ProtoOATrader.money_digits`'s wire
/// doc and `event_mapper::deal_fill`'s commission descale). Shared by the initial connect
/// ([`connect_and_auth_inner`]) and reconnect ([`reconnect_with_backoff`], Task 6) — cTrader
/// expects the exact same sequence after a dropped socket. `fixed_ctid`, when set, skips account
/// discovery and authorizes that ctid directly; reconnect ALWAYS passes the already-resolved ctid
/// here (never re-discovers) so a stale/reordered `GetAccountListByAccessToken` response can never
/// silently reauthorize a DIFFERENT account mid-session.
///
/// `pub(crate)`: also the entry point [`crate::recon_client`]'s `CtraderReconClient::connect`
/// uses to open its OWN dedicated authed connection (ReconFactory seam, wave-2 task 6) — the
/// SAME handshake sequence, isolated from the exec/data actor's own socket (mirrors Deribit's
/// dedicated-socket `ReconClient`).
pub(crate) fn open_and_handshake(
    cfg: &ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnError> {
    let (reader, ctid, lists, money_digits) = open_and_read_lists(cfg, fixed_ctid)?;
    Ok((reader, ctid, SymbolMap::from_lists(&lists), money_digits))
}

/// How long the reconnect reconcile ([`reconcile_after_reconnect`]) waits for its
/// `RECONCILE_RES` before giving up. Bounded (and shorter than the handshake timeout) so a venue
/// that never answers can never wedge the reconnect — subscriptions still replay after this
/// elapses. Best-effort: a miss logs once and returns.
const RECONCILE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the INITIAL-connect seed ([`seed_positions_at_connect`]) waits for its
/// `RECONCILE_RES`. **This is synchronous STARTUP cost**, so it is deliberately its own number and
/// not the reconnect twin's [`RECONCILE_TIMEOUT`] — the two run in places with opposite economics:
///
/// - The reconnect reconcile runs on the ACTOR THREAD, inside a backoff loop nobody is waiting on.
///   Its 5s is cheap and losing it is expensive (subscriptions replay against an unreconciled
///   coid→orderId map), so it stays 5s.
/// - The seed runs on the MOUNT PATH. cTrader is the one venue whose exec client connects
///   SYNCHRONOUSLY at mount, inside [`crate::mount::live_mount_for_account`] (called by
///   [`crate::mount::CtraderVenueMount`], which `crates/vike-mount/src/engine.rs`'s `make_engine`
///   reaches through the venue mount contract), and `crates/vike-mount/src/node.rs` mounts venues
///   one after another — so every millisecond here is paid by an operator watching a daemon that
///   has not started yet, and a mount that looks hung is a mount somebody kills.
///
/// **MEASURED, against the real `demo.ctraderapi.com:5035` (the CI box, 2026-08-08, 15 real mounts by
/// `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs`): the seed's own round trip took 24ms
/// median, 23ms min, 25ms max — a 2ms spread.** 1500ms is therefore ~60x the observed cost,
/// and it is a bound on a socket that has ALREADY completed the handshake's six request/response
/// round trips (app auth → \[accounts\] → account auth → trader → symbols list → symbol-by-id,
/// [`open_and_handshake`]) milliseconds earlier. A venue that answers six times and then goes quiet
/// for 1.5s is not "a bit slow"; it is anomalous, and waiting 3.5s longer for it buys nothing.
///
/// ⚠ **The stall it produces is QUANTIZED to the socket's 1s read timeout, so this constant is a
/// floor on the wait, not the wait.** [`read_reconcile_res`] tests the deadline between reads, and
/// [`open_and_handshake`] leaves the socket on a 1s `set_read_timeout` — so a mute venue is left in
/// a blocking read that only returns on a 1s boundary, and 1500ms costs the mount **2.0s**
/// (measured: 2.030s / 2.077s / 2.080s, three reps against
/// `crates/bridges/ctrader/tests/common/fake.rs`'s `FakeCtrader::start_mute_reconcile`). Under
/// [`RECONCILE_TIMEOUT`] the same stall measured **5.120s**. Any value in `(1s, 2s]` buys the same
/// 2.0s; 1500ms sits in the middle of that band deliberately, so neither a slow clock nor an early
/// `SO_RCVTIMEO` wakeup can tip the answer into the next second. Cutting to 1.0s means a bound of
/// ~500ms — 20x the measured cost rather than 60x — and that trade was declined: a lost seed
/// restores the routing bug this whole path exists to fix, and 3.1s of the 5.1s is already gone.
///
/// Losing the seed is cheap and self-healing, which is what lets the bound be tight: the book stays
/// UNFETCHED, routing is exactly what it was before the seed existed, and it refills at the first
/// execution event carrying a position ([`update_position_map`]) or the next reconnect
/// ([`reconcile_after_reconnect`]). `pub` because it is the operator-visible ceiling on the mount:
/// the timeout warn prints it, and `crates/bridges/ctrader/tests/exec_close.rs`'s
/// `a_mute_venue_costs_the_mount_the_seed_bound_not_the_reconnect_one` pins that a mute venue
/// really is bounded by THIS number rather than by [`RECONCILE_TIMEOUT`].
pub const SEED_TIMEOUT: Duration = Duration::from_millis(1500);

#[path = "conn_tests.rs"]
#[cfg(test)]
mod conn_tests;
