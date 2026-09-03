//! The multiplexed cTrader connection actor: ONE protobuf-over-TLS socket carries both market
//! data and order flow. `connect_and_auth` performs the PROVEN-LIVE handshake
//! (ApplicationAuth -> GetAccountListByAccessToken -> AccountAuth -> SymbolsList -> SymbolById)
//! and hands back an [`ActorHandle`] whose background thread owns the stream, dispatches inbound
//! frames, drains outbound [`Command`]s, and sends periodic heartbeats. Ports nothing — mirrors
//! the cTrader Open API (https://help.ctrader.com/open-api/) and the sequence proven by
//! `scratchpad/ctrader_catcher.py`.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use prost::Message;

use vike_data::LiveDataSink;
use vike_exec::EventSender;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderCancelRejected, OrderCanceled, OrderFilled,
    OrderModifyRejected, OrderPartiallyFilled, OrderRejected, TradeId,
};
use vike_model::{now_ms, Bar, QuoteTick};

use crate::event_mapper;
use crate::framing::{self, FrameReader};
use crate::oauth;
use crate::positions::{self, CloseEmit, CloseFailure, CloseTracker, PositionBook};
use crate::proto::{
    pt, ProtoHeartbeatEvent, ProtoMessage, ProtoOaAccountAuthReq, ProtoOaApplicationAuthReq,
    ProtoOaCancelOrderReq, ProtoOaClosePositionReq, ProtoOaErrorRes, ProtoOaExecutionEvent,
    ProtoOaExecutionType, ProtoOaGetAccountListByAccessTokenReq,
    ProtoOaGetAccountListByAccessTokenRes, ProtoOaGetTrendbarsReq, ProtoOaGetTrendbarsRes,
    ProtoOaOrderErrorEvent, ProtoOaReconcileReq, ProtoOaReconcileRes, ProtoOaSpotEvent,
    ProtoOaSubscribeLiveTrendbarReq, ProtoOaSubscribeSpotsReq, ProtoOaSymbolByIdReq,
    ProtoOaSymbolByIdRes, ProtoOaSymbolsListReq, ProtoOaSymbolsListRes, ProtoOaTraderReq,
    ProtoOaTraderRes, ProtoOaTrendbarPeriod, ProtoOaUnsubscribeLiveTrendbarReq,
    ProtoOaUnsubscribeSpotsReq,
};
use crate::symbols::SymbolMap;

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
    /// Where a REFRESHED grant is written back — the credential store and this tier's two token
    /// keys. `None` keeps the historical behaviour exactly: a refresh lives as long as the process.
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
            // A PATH and two KEY NAMES — no secret. See `token_store::TokenPersist`.
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

/// A `Read`/`Write` stream that flags EOF (a `read` returning `Ok(0)`) so an actor loop can
/// break instead of busy-spinning on a closed socket (the framing layer maps both EOF and a
/// read-timeout to `Ok(None)`, which are otherwise indistinguishable). Reused by the fake test
/// server. Read-timeouts surface as `WouldBlock`/`TimedOut` errors (not `Ok(0)`), so they do NOT
/// set the flag.
pub(crate) struct EofStream<S> {
    inner: S,
    eof: Arc<AtomicBool>,
}

impl<S> EofStream<S> {
    pub(crate) fn new(inner: S) -> (Self, Arc<AtomicBool>) {
        let eof = Arc::new(AtomicBool::new(false));
        (Self { inner, eof: eof.clone() }, eof)
    }
}

impl<S: Read> Read for EofStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n == 0 {
            self.eof.store(true, Ordering::SeqCst);
        }
        Ok(n)
    }
}

impl<S: Write> Write for EofStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// The underlying socket: plaintext (tests) or rustls-wrapped (production).
pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Build a rustls client stream (ring provider, webpki-roots) — see resolution 3: the default
/// aws-lc provider is disabled, so an explicit provider is required.
fn wrap_tls(host: &str, tcp: TcpStream) -> Result<Stream, ConnError> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| ConnError::Tls(e.to_string()))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|e| ConnError::Tls(e.to_string()))?;
    let conn = rustls::ClientConnection::new(Arc::new(config), server_name)
        .map_err(|e| ConnError::Tls(e.to_string()))?;
    Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
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

/// Decode an `ERROR_RES` frame into its code/description.
fn decode_error(msg: &ProtoMessage) -> ConnError {
    match ProtoOaErrorRes::decode(msg.payload.as_deref().unwrap_or(&[])) {
        Ok(e) => ConnError::Venue {
            error_code: e.error_code,
            description: e.description.unwrap_or_default(),
        },
        Err(e) => ConnError::Decode(format!("error_res: {e}")),
    }
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
    let (_reader, _ctid, symbols, _money_digits) = open_and_handshake(&cfg, None)?;
    // `_reader` (hence the underlying TcpStream/TLS session) drops here — the socket closes and no
    // actor thread was ever spawned, so this leaves nothing running behind it.
    Ok(symbols)
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
    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port))?;
    // Short read timeout BEFORE any TLS wrap so both the handshake loop and the actor loop see
    // `Ok(None)` on idle rather than blocking.
    tcp.set_read_timeout(Some(Duration::from_secs(1)))?;
    let stream = if cfg.no_tls { Stream::Plain(tcp) } else { wrap_tls(&cfg.host, tcp)? };

    let (eof_stream, _eof) = EofStream::new(stream);
    let mut reader = FrameReader::new(eof_stream);

    // 1. ApplicationAuth.
    let app = ProtoOaApplicationAuthReq {
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        ..Default::default()
    };
    send(&mut reader, pt::APPLICATION_AUTH_REQ, &app, "app")?;
    read_until(&mut reader, pt::APPLICATION_AUTH_RES)?;

    // 2. Resolve the ctid: the caller's fixed override (reconnect) -> the configured one -> else
    //    discover via GetAccountListByAccessToken and pick the first demo (non-live) account
    //    (matches ctrader_catcher.py::prove). NEVER fall back to a live account here — an explicit
    //    ctid is the only sanctioned way to target a live/specific account; silently authorizing
    //    whatever account happens to be first (which may be live) is a safety hazard.
    let ctid = match fixed_ctid.or(cfg.account_id) {
        Some(id) => id,
        None => {
            let req = ProtoOaGetAccountListByAccessTokenReq {
                access_token: cfg.access_token.clone(),
                ..Default::default()
            };
            send(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ, &req, "accts")?;
            let msg = read_until(&mut reader, pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES)?;
            let res: ProtoOaGetAccountListByAccessTokenRes = decode_payload(&msg)?;
            let acct = res
                .ctid_trader_account
                .iter()
                .find(|a| !a.is_live.unwrap_or(false))
                .ok_or(ConnError::NoAccounts)?;
            acct.ctid_trader_account_id as i64
        }
    };

    // 3. AccountAuth.
    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: cfg.access_token.clone(),
        ..Default::default()
    };
    send(&mut reader, pt::ACCOUNT_AUTH_REQ, &account_auth, "aa")?;
    read_until(&mut reader, pt::ACCOUNT_AUTH_RES)?;

    // 4. Trader (account info) — the account-level `money_digits`, the exponent balance
    //    integers are scaled by; also the FALLBACK exponent for a deal's commission when that
    //    deal omits its own `ProtoOADeal.moneyDigits` (tag 17 — present per-deal in practice; its
    //    wire doc says "Affects commission." directly, so `event_mapper::deal_fill` prefers it and
    //    only falls back to this account-level value). Live-verified 2026-07-14: a demo account's
    //    `money_digits` is 2 (commission=-3 raw -> -0.03 after the vike-model sign flip), but this
    //    is fetched per-account rather than hardcoded — a different broker/account could scale
    //    differently.
    let trader_req = ProtoOaTraderReq { ctid_trader_account_id: ctid, ..Default::default() };
    send(&mut reader, pt::TRADER_REQ, &trader_req, "tr")?;
    let msg = read_until(&mut reader, pt::TRADER_RES)?;
    let trader_res: ProtoOaTraderRes = decode_payload(&msg)?;
    let money_digits = trader_res.trader.money_digits.unwrap_or(0);

    // 5. SymbolsList (light symbols: name<->id, NO digits).
    let symbols_req = ProtoOaSymbolsListReq {
        ctid_trader_account_id: ctid,
        include_archived_symbols: Some(false),
        ..Default::default()
    };
    send(&mut reader, pt::SYMBOLS_LIST_REQ, &symbols_req, "sy")?;
    let msg = read_until(&mut reader, pt::SYMBOLS_LIST_RES)?;
    let list: ProtoOaSymbolsListRes = decode_payload(&msg)?;

    // 6. SymbolById (full symbols: digits -> scale). ONE batch request for every id (resolution
    //    1). If a broker's list is enormous this is the place to switch to lazy per-subscribe
    //    fetches; the batch keeps `scale()` populated eagerly.
    let ids: Vec<i64> = list.symbol.iter().map(|s| s.symbol_id).collect();
    let by_id_req =
        ProtoOaSymbolByIdReq { ctid_trader_account_id: ctid, symbol_id: ids, ..Default::default() };
    send(&mut reader, pt::SYMBOL_BY_ID_REQ, &by_id_req, "sbid")?;
    let msg = read_until(&mut reader, pt::SYMBOL_BY_ID_RES)?;
    let by_id: ProtoOaSymbolByIdRes = decode_payload(&msg)?;

    let symbols = SymbolMap::from_symbols(&list.symbol, &by_id.symbol);
    Ok((reader, ctid, symbols, money_digits))
}

/// [`open_and_handshake_retry`] that SELF-HEALS an expired grant: on an auth/token refusal it
/// spends the configured refresh token once, adopts + PERSISTS the new pair, and retries the
/// handshake a single time.
///
/// # Why the initial connect needed this, and not just the actor loop
///
/// The reactive refresh (`try_refresh_and_reauth`) only ever ran on an ALREADY-AUTHENTICATED
/// socket — an `ACCOUNTS_TOKEN_INVALIDATED_EVENT` mid-session. A grant that had already lapsed
/// before startup therefore never reached it: `open_and_handshake_retry` classifies an auth
/// rejection as PERMANENT and returns immediately, so the daemon refused to start while a perfectly
/// good refresh token sat in the same credential store. That is exactly what the live rehearsal hit
/// (the alpaca+ctrader live rehearsal (PR #1407), Evidence 2 — `CH_ACCESS_TOKEN_INVALID`,
/// "Access token expired", daemon down).
///
/// # Bounded, and it stays bounded
///
/// EXACTLY ONE refresh + ONE retry. A refresh that succeeds and a handshake that then fails again
/// returns that second error unchanged — retrying further would spin an OAuth endpoint on a
/// credential that is not the problem. A non-auth failure is returned untouched and costs nothing:
/// this wrapper is inert on every healthy startup, which is the byte-identical claim.
///
/// # The refusal names the remedy
///
/// When there is no refresh token, or the refresh itself is refused, the operator's grant is dead
/// and only re-authorizing fixes it — so the error log names
/// [`crate::token_store::REAUTHORIZE_CMD`] verbatim. The rehearsal's daemon printed the venue's
/// `CH_ACCESS_TOKEN_INVALID` and stopped there, which says what broke and not what to do.
fn open_and_handshake_self_healing(
    cfg: &mut ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnError> {
    let first = match open_and_handshake_retry(cfg, fixed_ctid) {
        Ok(ok) => return Ok(ok),
        Err(e) => e,
    };
    if !first.is_auth_failure() {
        return Err(first);
    }

    let Some(refresh_token) = cfg.refresh_token.clone() else {
        tracing::error!(
            target: "ctrader",
            error = %first,
            "the access token was refused and NO refresh token is configured — re-issue the grant              with `{}` (the daemon cannot self-heal this)",
            crate::token_store::REAUTHORIZE_CMD
        );
        return Err(first);
    };

    tracing::warn!(
        target: "ctrader",
        error = %first,
        "the access token was refused at connect; spending the refresh token once before giving up"
    );
    // ⚠ The error is NEVER interpolated: `oauth`'s token endpoint embeds the client_secret and the
    // refresh token as query params, and a transport error's `Display` can echo that URL back.
    let token = match oauth::refresh(&cfg.client_id, &cfg.client_secret, &refresh_token) {
        Ok(t) => t,
        Err(_) => {
            tracing::error!(
                target: "ctrader",
                "the refresh token was ALSO refused (no creds/token in this message) — the grant is                  dead; re-issue it with `{}`",
                crate::token_store::REAUTHORIZE_CMD
            );
            return Err(first);
        }
    };
    // Adopts the pair into `cfg` AND writes it back to the credential store, so the very next
    // start does not repeat this round trip.
    adopt_refreshed_token(cfg, &token);
    tracing::info!(target: "ctrader", "grant refreshed at connect; retrying the handshake once");
    open_and_handshake_retry(cfg, fixed_ctid)
}

/// Run [`open_and_handshake`] under the bounded exponential-backoff [`ConnConfig::connect_retry`]
/// policy — the hardening that keeps a transient network/TLS blip during startup from permanently
/// dropping the venue to paper.
///
/// Retries ONLY a TRANSIENT fault ([`ConnError::is_transient`] — a failed TCP/TLS connect, a read
/// timeout, a connection reset, a peer that closed mid-handshake, or no response), sleeping the
/// doubling, capped backoff between attempts. A PERMANENT failure (auth REJECTED, no accounts,
/// decode/tls-config) returns IMMEDIATELY — retrying a bad credential only delays the paper
/// fallback. On exhausting `max_attempts` the LAST error is returned UNCHANGED, so the ultimate
/// failure contract (terminal error → paper fallback) is preserved verbatim; the retry only gives a
/// transient blip more chances first.
///
/// INERT on success: the first attempt succeeding neither sleeps nor re-sends a frame, so a healthy
/// startup is byte-identical to calling [`open_and_handshake`] directly. Deliberately SEPARATE from
/// [`reconnect_with_backoff`] (the unbounded mid-session reconnect for an already-authed socket that
/// drops) — this bounds the FIRST connect so a dead venue eventually yields to paper rather than
/// looping forever. `fetch_symbols` (catalog enumeration) and [`crate::recon_client`] (lazy
/// revive-on-next-fetch) keep their own single-shot paths and do NOT route through here.
fn open_and_handshake_retry(
    cfg: &ConnConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnError> {
    let policy = cfg.connect_retry;
    let max_attempts = policy.max_attempts.max(1);
    let mut backoff = policy.initial_backoff;
    let mut attempt = 1u32;
    loop {
        match open_and_handshake(cfg, fixed_ctid) {
            Ok(ready) => return Ok(ready),
            // Give up on the last attempt, or immediately on a permanent (non-transient) fault.
            Err(e) if attempt >= max_attempts || !e.is_transient() => return Err(e),
            Err(e) => {
                tracing::warn!(
                    target: "ctrader",
                    error = %e,
                    attempt,
                    max_attempts,
                    backoff_ms = backoff.as_millis() as u64,
                    "initial connect attempt failed transiently; retrying after backoff"
                );
                thread::sleep(backoff);
                backoff = (backoff * 2).min(policy.max_backoff);
                attempt += 1;
            }
        }
    }
}

fn connect_and_auth_inner(
    cfg: ConnConfig,
    sink: Arc<dyn LiveDataSink>,
    events: Option<EventSender>,
) -> Result<ActorHandle, ConnError> {
    let mut cfg = cfg;
    let (mut reader, ctid, symbols, money_digits) =
        open_and_handshake_self_healing(&mut cfg, None)?;
    let symbols = Arc::new(symbols);

    // Spawn the read/heartbeat/command loop. The actor gets its OWN clone of `cfg` (it needs
    // `client_id`/`client_secret`/`access_token`/`refresh_token`/`host`/`port` to reconnect and,
    // separately, to refresh an invalidated token — Task 6).
    let (tx, rx) = mpsc::channel::<Command>();
    let actor_symbols = symbols.clone();
    let orders: OrderIdMap = Arc::new(Mutex::new(HashMap::new()));
    let actor_orders = orders.clone();
    let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
    let actor_positions = positions.clone();
    let closes: CloseState = Arc::new(Mutex::new(CloseTracker::default()));
    let actor_closes = closes.clone();

    // Seed the OPEN-position book BEFORE the actor thread starts — see `seed_positions_at_connect`
    // for why a fresh mount used to start (and stay) blind. EXEC MOUNTS ONLY: a data-only
    // connection has no reduce routing and no halt boundary to feed, so it keeps its byte-identical
    // handshake with no extra round trip.
    if events.is_some() {
        seed_positions_at_connect(
            &mut reader,
            ctid,
            symbols.as_ref(),
            money_digits,
            sink.as_ref(),
            events.as_ref(),
            &orders,
            &positions,
            &closes,
        );
    }

    let actor_cfg = cfg.clone();
    let join = thread::Builder::new()
        .name("ctrader-actor".into())
        .spawn(move || {
            actor_loop(
                reader,
                rx,
                actor_cfg,
                ctid,
                actor_symbols,
                money_digits,
                sink,
                events,
                actor_orders,
                actor_positions,
                actor_closes,
            )
        })
        .map_err(ConnError::Io)?;

    Ok(ActorHandle { tx, symbols, ctid, orders, positions, closes, join: Some(join) })
}

/// Dispatch point for inbound frames. Handles heartbeats/errors, routes `SPOT_EVENT` (quotes +
/// any embedded live trendbars) and `GET_TRENDBARS_RES` (the F2 historical seed, see
/// `on_get_trendbars_res`) to the `LiveDataSink`, and (when exec is wired) routes
/// `EXECUTION_EVENT` to the `EventSender` after updating the coid→order-id map. `money_digits` is
/// the account's `ProtoOATrader.money_digits` (fetched once at handshake, refreshed on reconnect —
/// see `open_and_handshake`) — threaded into `event_mapper::exec_event_to_events` as the fallback
/// exponent for a fill's signed commission integer, when the deal itself omits its own
/// `money_digits`.
#[allow(clippy::too_many_arguments)]
fn on_inbound(
    msg: &ProtoMessage,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    match msg.payload_type {
        pt::HEARTBEAT_EVENT => {} // peer keepalive — nothing to do
        pt::ERROR_RES => on_error_res(msg, events, closes),
        pt::ORDER_ERROR_EVENT => on_order_error_event(msg, events, orders),
        pt::SPOT_EVENT => match decode_payload::<ProtoOaSpotEvent>(msg) {
            Ok(ev) => {
                if let Some((symbol, bid, ask)) = event_mapper::spot_to_quote(&ev, symbols) {
                    let quote = QuoteTick {
                        ts: ev.timestamp.unwrap_or_else(vike_model::now_ms),
                        local_ts: vike_model::now_ms(),
                        bid,
                        ask,
                        bid_size: 0.0,
                        ask_size: 0.0,
                        symbol: symbol.clone(),
                    };
                    sink.quote(VENUE, &symbol, quote);
                }
                for (symbol, interval, bar) in event_mapper::spot_to_bars(&ev, symbols) {
                    // A live trendbar riding a SPOT_EVENT is the bar STILL forming (cTrader keeps
                    // pushing updated snapshots of the current period until it closes) — the
                    // conflating lane is the correct sink verb, not `close_bar`.
                    sink.forming_bar(VENUE, &symbol, interval, bar);
                }
            }
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "failed to decode SPOT_EVENT");
            }
        },
        pt::GET_TRENDBARS_RES => match decode_payload::<ProtoOaGetTrendbarsRes>(msg) {
            Ok(res) => on_get_trendbars_res(&res, symbols, sink),
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "failed to decode GET_TRENDBARS_RES");
            }
        },
        pt::EXECUTION_EVENT => {
            // Order flow: decode, update the coid→order-id correlation map, and push canonical
            // events onto the ingest lane. Only active when an `EventSender` was wired
            // (`connect_and_auth_exec`); with data-only wiring an execution event is a no-op.
            let Some(events) = events else { return };
            match decode_payload::<ProtoOaExecutionEvent>(msg) {
                Ok(ev) => {
                    // Keep the OPEN-position map fresh (drives the exec client's reduce routing) and
                    // the coid→orderId map, then map to canonical events — routing a CLOSE-correlated
                    // event through the per-coid aggregator (see `exec_event_to_canonical`).
                    update_position_map(&ev, positions);
                    update_order_map(&ev, orders);
                    for event in exec_event_to_canonical(&ev, symbols, money_digits, closes) {
                        if events.blocking_send(event).is_err() {
                            tracing::warn!(target: "ctrader", "core ingest gone; dropping exec event");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(target: "ctrader", error = %e, "failed to decode EXECUTION_EVENT");
                }
            }
        }
        other => {
            tracing::debug!(target: "ctrader", payload_type = other, "unhandled inbound frame");
        }
    }
}

/// Route a `GET_TRENDBARS_RES` frame (F2 historical seed): resolve the symbol name + interval,
/// map every trendbar entry through `event_mapper::trendbar_to_bar` (skipping incomplete entries per
/// that function's contract), sort ascending by timestamp (the wire order is not documented/
/// guaranteed), and hand the whole batch to `LiveDataSink::seed_bars` in ONE call — the dedicated
/// "recent history before live closes start arriving" verb (see that trait method's doc).
/// Best-effort: an unresolvable symbol id/period or an empty/all-incomplete trendbar list is a
/// silent no-op (one `warn!`, never per-bar) — the live `SubscribeTrendbar` stream (sent
/// independently, and unconditionally, by `data::CtraderData::subscribe_bars`) starts regardless
/// of whether this seed succeeds.
fn on_get_trendbars_res(
    res: &ProtoOaGetTrendbarsRes,
    symbols: &SymbolMap,
    sink: &dyn LiveDataSink,
) {
    let Some(symbol_id) = res.symbol_id else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES missing symbolId; dropping seed");
        return;
    };
    let Some(name) = symbols.name_of(symbol_id) else {
        tracing::warn!(
            target: "ctrader",
            symbol_id,
            "GET_TRENDBARS_RES for unknown symbol id; dropping seed"
        );
        return;
    };
    let Ok(period) = ProtoOaTrendbarPeriod::try_from(res.period) else {
        tracing::warn!(target: "ctrader", "GET_TRENDBARS_RES has an unrecognized period; dropping seed");
        return;
    };
    let mut bars: Vec<Bar> =
        res.trendbar.iter().filter_map(event_mapper::trendbar_to_bar).collect();
    if bars.is_empty() {
        return; // nothing to seed (e.g. a brand-new symbol/period with no history yet) — not an error
    }
    bars.sort_by_key(|b| b.ts);
    sink.seed_bars(VENUE, name, event_mapper::interval_for_trendbar_period(period), bars);
}

/// Build a reject `reason` string from a venue error's code + optional description. Contains ONLY
/// venue-provided error text (never creds/tokens — `decode_error`/the wire error carry neither).
fn error_reason(error_code: &str, description: &str) -> String {
    if description.is_empty() {
        error_code.to_string()
    } else {
        format!("{error_code}: {description}")
    }
}

/// Reverse the coid→order-id correlation map: find the `client_order_id` a numeric venue
/// `order_id` belongs to, if any. Used to correlate an `ORDER_ERROR_EVENT` (which carries only the
/// `orderId`) back to the coid the OMS keys on.
fn reverse_lookup(orders: &OrderIdMap, order_id: i64) -> Option<String> {
    let map = orders.lock().ok()?;
    map.iter().find(|(_, &oid)| oid == order_id).map(|(coid, _)| coid.clone())
}

/// Route an `ERROR_RES` (2142) frame. cTrader echoes the request's envelope `clientMsgId` on the
/// error to that request; the order commands set that field to the order's `client_order_id`
/// ([`write_command`]), so a non-empty `clientMsgId` here identifies the failed order request. We
/// synthesize a TERMINAL [`Event::OrderRejected`] for it (the safe default: the verb — submit vs
/// cancel vs amend — is not distinguishable from the envelope alone, and a submit reject is the
/// common case; a cancel/amend failure surfaces instead as an `ORDER_ERROR_EVENT`, handled below).
/// An `ERROR_RES` with an EMPTY `clientMsgId` is not order-correlated — keep the original `warn!`.
/// Auth-flavored `ERROR_RES` never reaches here: the actor loop diverts it to token refresh first.
///
/// If the coid is a CLOSE coid (a `ProtoOAClosePositionReq` the venue rejected at the protocol
/// layer), it is resolved through [`close_failure_terminal`] instead — Filled-if-progressed, else
/// Rejected — and FORGOTTEN from the tracker, so a wire-level close reject can never (a) leave a
/// stale `by_position` entry that misroutes a later leg's fill, nor (b) strand the coid with an
/// illegal `OrderRejected` from `PartiallyFilled`. An ordinary order's coid gets the plain reject.
fn on_error_res(msg: &ProtoMessage, events: Option<&EventSender>, closes: &CloseState) {
    let ConnError::Venue { error_code, description } = decode_error(msg) else {
        return; // undecodable error frame — nothing correlatable to synthesize
    };
    let coid = msg.client_msg_id.as_deref().filter(|s| !s.is_empty());
    match (coid, events) {
        (Some(coid), Some(events)) => {
            let reason = error_reason(&error_code, &description);
            // A close coid resolves via the tracker (forget + legal terminal); a normal one is a
            // plain terminal reject.
            let is_close = closes.lock().ok().is_some_and(|t| t.is_tracked(coid));
            let event = if is_close {
                close_failure_terminal(coid, &reason, closes, now_ms())
            } else {
                Event::OrderRejected(OrderRejected {
                    client_order_id: coid.to_string(),
                    reason: reason.into(),
                    ts: now_ms(),
                })
            };
            if events.blocking_send(event).is_err() {
                tracing::warn!(target: "ctrader", "core ingest gone; dropping synthesized reject");
            }
        }
        // Empty clientMsgId (not order-correlated) or data-only wiring: log, don't fabricate.
        _ => tracing::warn!(target: "ctrader", %error_code, %description, "venue error frame"),
    }
}

/// Route an `ORDER_ERROR_EVENT` (2132) frame — an error against a specific order request. Reverse-
/// correlate its `orderId` to a `client_order_id` via the shared [`OrderIdMap`]: a hit means the
/// order was accepted (we only learn its `orderId` from an execution event), so the failed request
/// was a cancel/amend → NON-terminal [`Event::OrderCancelRejected`] (the order stays live). If the
/// `orderId` is absent/uncorrelatable but the envelope `clientMsgId` carries a coid, the order was
/// never accepted → TERMINAL [`Event::OrderRejected`]. With no correlation at all we `warn!` rather
/// than fabricate a wrong coid. A no-op under data-only wiring (no `EventSender`).
fn on_order_error_event(msg: &ProtoMessage, events: Option<&EventSender>, orders: &OrderIdMap) {
    let Some(events) = events else { return };
    let ev = match decode_payload::<ProtoOaOrderErrorEvent>(msg) {
        Ok(ev) => ev,
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "failed to decode ORDER_ERROR_EVENT");
            return;
        }
    };
    let reason = error_reason(&ev.error_code, ev.description.as_deref().unwrap_or_default());
    let from_order = ev.order_id.and_then(|oid| reverse_lookup(orders, oid));
    let event = match from_order {
        // Correlated via a known venue orderId → the order was accepted → non-terminal advisory.
        Some(coid) => Event::OrderCancelRejected(OrderCancelRejected {
            client_order_id: coid,
            reason: reason.into(),
            ts: now_ms(),
        }),
        None => match msg.client_msg_id.as_deref().filter(|s| !s.is_empty()) {
            // Only the envelope coid known (never accepted) → terminal submit reject.
            Some(coid) => Event::OrderRejected(OrderRejected {
                client_order_id: coid.to_string(),
                reason: reason.into(),
                ts: now_ms(),
            }),
            // No correlation possible — do not fabricate a coid.
            None => {
                tracing::warn!(
                    target: "ctrader",
                    error_code = %ev.error_code,
                    "ORDER_ERROR_EVENT with no correlatable order; dropping"
                );
                return;
            }
        },
    };
    if events.blocking_send(event).is_err() {
        tracing::warn!(target: "ctrader", "core ingest gone; dropping synthesized reject");
    }
}

/// Record the referenced order's `client_order_id → order_id` mapping so the exec client can later
/// resolve a coid to the numeric order id cTrader's cancel/amend requests need. Uses the order's
/// dedicated `client_order_id`, falling back to the `label` we set at submit time. No-op when the
/// event carries no order ref or no correlation id.
///
/// A DEFINITELY-TERMINAL execution event (full FILLED / CANCELLED / REJECTED / EXPIRED — classified
/// once by [`event_mapper::exec_event_is_terminal`], not re-derived here) instead PRUNES the coid from the
/// map. Cancel/amend for a live order resolve from the map before any terminal event arrives, so
/// removal never breaks them; keeping the entry forever, by contrast, would make every reconnect's
/// gap-diff warn for every historically-completed coid (they never reappear in `RECONCILE_RES`,
/// which lists only PENDING orders) — so after this prune the gap-warn is actionable.
fn update_order_map(ev: &ProtoOaExecutionEvent, orders: &OrderIdMap) {
    if let Some(order) = &ev.order {
        let coid = order.client_order_id.clone().or_else(|| order.trade_data.label.clone());
        if let Some(coid) = coid {
            if !coid.is_empty() {
                if let Ok(mut map) = orders.lock() {
                    if event_mapper::exec_event_is_terminal(ev) {
                        map.remove(&coid);
                    } else {
                        map.insert(coid, order.order_id);
                    }
                }
            }
        }
    }
}

/// Keep the shared OPEN-position map in step with every execution event: an event carrying a
/// `position` ref UPSERTS it when the position is live+OPEN (`positions::tracked_position`) or
/// REMOVES it once it is CLOSED/zero-volume. This is the map the exec client reads to decide a
/// reducing submit's close-by-position-id routing, so it must reflect an open as soon as its fill
/// lands and drop it the moment a close flattens it. No-op for an event with no position ref (e.g.
/// a pending-order accept before any position exists).
///
/// ⚠ **A `position` ref this build cannot CLASSIFY invalidates the book's evidence flag**, by the
/// same split [`rebuild_position_book`] makes on the reconcile path
/// (`positions::row_is_known_to_carry_no_exposure`: CLOSED/CREATED is knowledge, everything else is
/// a hole). Without it the completeness discipline stopped at the reconcile — the flag is set ONCE,
/// from one past answer, and the book then drifts forwards on events for the whole life of the
/// socket, so an ERROR-status position ref would silently REMOVE a live row while the book went on
/// calling itself authoritative, and that manufactured absence would refuse the position's own exit
/// under `halt_admit = "verify"`. The entry is still dropped (routing runs on the best guess
/// available); only the claim to be EVIDENCE goes, until a reconcile answers again.
fn update_position_map(ev: &ProtoOaExecutionEvent, positions: &PositionMap) {
    let Some(p) = &ev.position else { return };
    if let Ok(mut book) = positions.lock() {
        match positions::tracked_position(p) {
            Some((id, tracked)) => book.upsert(id, tracked),
            None => {
                book.remove(p.position_id);
                if !positions::row_is_known_to_carry_no_exposure(p) {
                    book.invalidate();
                    tracing::warn!(
                        target: "ctrader",
                        position_id = p.position_id,
                        position_status = p.position_status,
                        "an execution event carried a position this build cannot classify; the \
                         entry is dropped for routing and the book is no longer EVIDENCE (a halt \
                         under halt_admit=verify ADMITS) until a reconcile answers again"
                    );
                }
            }
        }
    }
}

/// The numeric `positionId` an execution event references — the deal's source position, else the
/// order's linked position — or `0` when it references none (an ordinary opening order before the
/// position exists). Used to spot a CLOSE-correlated event.
fn event_position_id(ev: &ProtoOaExecutionEvent) -> i64 {
    ev.deal
        .as_ref()
        .map(|d| d.position_id)
        .or_else(|| ev.order.as_ref().and_then(|o| o.position_id))
        .unwrap_or(0)
}

/// Map an execution event to canonical `Event`s. When the event belongs to an in-flight CLOSE (its
/// `positionId` is registered in `closes`, keyed by a reducing submit's coid), it is folded through
/// the per-coid aggregator ([`map_close_event`]) so N FIFO close legs surface as ONE coherent
/// `OrderAccepted → OrderPartiallyFilled* → OrderFilled` lifecycle under that coid — the closing
/// order carries no `clientOrderId` of ours, so this position-id correlation is the ONLY link back
/// to the submit. Every non-close event falls through to the stateless
/// `event_mapper::exec_event_to_events`.
fn exec_event_to_canonical(
    ev: &ProtoOaExecutionEvent,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let position_id = event_position_id(ev);
    let coid = if position_id != 0 {
        closes.lock().ok().and_then(|t| t.coid_for(position_id).map(str::to_string))
    } else {
        None
    };
    match coid {
        Some(coid) => map_close_event(ev, &coid, symbols, money_digits, closes),
        None => event_mapper::exec_event_to_events(ev, symbols, money_digits),
    }
}

/// A ZERO-quantity `FillEvent` that gives a close coid a LEGAL FSM terminal without moving money:
/// used to terminalize a partially-filled close (some legs done) as `OrderFilled` when a later leg
/// FAILS — an `OrderRejected` from `PartiallyFilled` is illegal and would be DROPPED, stranding the
/// order. The already-filled legs folded their own bare `Event::Fill` into the Account, so this
/// carries `last_qty = 0.0`, and no bare `Event::Fill` is emitted alongside it (nothing to fold).
///
/// ⚠ The `trade_id` used to be the EMPTY string, deliberately, to BYPASS the engine's
/// `seen_fsm_trade_ids` dedup "so the wrap always applies". That made this the one site in the tree
/// depending on the dedup hole, and it cannot survive a `TradeId` that has no empty value. It is now
/// a deterministic per-coid id, which is not a behaviour regression but the same intent stated
/// safely: the id is a pure function of `coid`, so re-terminalizing the SAME close is the only thing
/// it dedups — and that is a duplicate by definition (the FSM refuses a second terminal from a
/// terminal state anyway). One coid can never need two DISTINCT zero-qty terminals.
fn fsm_terminal_fill(coid: &str, ts: i64) -> FillEvent {
    FillEvent {
        trade_id: TradeId::prefixed("CTRADER-FSMTERM-", coid),
        client_order_id: coid.to_string(),
        venue: VENUE.into(),
        symbol: "".into(),
        side: 0,
        last_qty: 0.0,
        last_px: 0.0,
        commission: 0.0,
        commission_asset: "".into(),
        liquidity_side: Default::default(),
        ts,
        mark_price: None,
        position_side: Default::default(),
    }
}

/// The LEGAL terminal event for a failed close coid: `OrderFilled` (for the volume already closed)
/// when earlier legs `progressed` the FSM to `PartiallyFilled` — where `OrderRejected` would be an
/// illegal, DROPPED transition — else a clean `OrderRejected`. PURE: does not touch the tracker, so
/// it is safe to build at CAPTURE time (before a write is attempted) without a side effect.
fn close_terminal_event(coid: &str, progressed: bool, reason: &str, ts: i64) -> Event {
    if progressed {
        Event::OrderFilled(OrderFilled {
            client_order_id: coid.to_string(),
            fill: fsm_terminal_fill(coid, ts),
            ts,
        })
    } else {
        Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: reason.into(),
            ts,
        })
    }
}

/// Terminalize a close coid on a FAILURE THAT HAS ALREADY HAPPENED (a leg reject or a wire
/// `ERROR_RES`): FORGET it from the tracker and return the legal terminal ([`close_terminal_event`]).
/// The write-failure path does NOT use this — it must not forget at capture time (see
/// [`write_failure_event`]).
fn close_failure_terminal(coid: &str, reason: &str, closes: &CloseState, ts: i64) -> Event {
    let progressed = matches!(
        closes.lock().ok().map(|mut t| t.resolve_failure(coid)),
        Some(CloseFailure::TerminalFilled)
    );
    close_terminal_event(coid, progressed, reason, ts)
}

/// Fold ONE close-correlated execution event for reduce-order `coid` into canonical events via the
/// shared [`CloseTracker`]. A REJECTED closing order terminates the coid via
/// [`close_failure_terminal`] (Filled-if-progressed, else Rejected — never a dropped illegal
/// transition); a CANCELLED one terminates as `OrderCanceled` (legal from `PartiallyFilled`).
/// Otherwise the event is an accept/(partial) fill — the tracker decides whether it emits the single
/// `OrderAccepted`, an intermediate `OrderPartiallyFilled`, or the terminal `OrderFilled` (once the
/// cumulative closed volume reaches the planned reduce total). The fill itself is built by the SAME
/// `event_mapper::deal_fill` the live fill path uses, only with the substituted coid — the closing
/// deal's `tradeSide` is already the reducing direction (a SELL deal closes a LONG), so the folded
/// `FillEvent` nets the position toward flat with no special-casing.
fn map_close_event(
    ev: &ProtoOaExecutionEvent,
    coid: &str,
    symbols: &SymbolMap,
    money_digits: u32,
    closes: &CloseState,
) -> Vec<Event> {
    let ts = event_mapper::lifecycle_ts(ev);
    match ProtoOaExecutionType::try_from(ev.execution_type) {
        Ok(ProtoOaExecutionType::OrderRejected) => {
            let reason = ev.error_code.clone().unwrap_or_default();
            return vec![close_failure_terminal(coid, &reason, closes, ts)];
        }
        Ok(ProtoOaExecutionType::OrderCancelled) => {
            if let Ok(mut t) = closes.lock() {
                t.forget(coid);
            }
            return vec![Event::OrderCanceled(OrderCanceled {
                client_order_id: coid.to_string(),
                reason: ev.error_code.clone().unwrap_or_default().into(),
                ts,
            })];
        }
        _ => {}
    }

    let fill_volume = ev.deal.as_ref().map(|d| d.filled_volume).unwrap_or(0);
    let Some(emit) = closes.lock().ok().map(|mut t| t.on_event(coid, fill_volume)) else {
        return Vec::new();
    };
    let venue_order_id = ev.order.as_ref().map(|o| o.order_id).unwrap_or(0);
    let accepted = |out: &mut Vec<Event>| {
        out.push(Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(venue_order_id.to_string().into()),
            ts,
        }));
    };
    let mut out = Vec::new();
    match emit {
        CloseEmit::AcceptOnly => accepted(&mut out),
        CloseEmit::Partial { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = event_mapper::deal_fill(ev, coid, symbols, money_digits) {
                let fts = fill.ts;
                // DUAL-PUBLISH (mirrors `event_mapper::exec_event_to_events` and every crypto venue):
                // the bare `Event::Fill` folds into `Account` (REDUCES the position — the whole point
                // of close-position), the wrap advances the OMS order FSM. The closing deal's side is
                // already the reducing direction (a SELL deal closes a LONG), so the folded fill nets
                // toward flat with no special-casing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderPartiallyFilled(OrderPartiallyFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts: fts,
                }));
            }
        }
        CloseEmit::Final { accept_first } => {
            if accept_first {
                accepted(&mut out);
            }
            if let Some(fill) = event_mapper::deal_fill(ev, coid, symbols, money_digits) {
                let fts = fill.ts;
                // Dual-publish: bare `Event::Fill` (Account fold) THEN the terminal wrap (FSM). See
                // the `Partial` arm above for why the bare fill is load-bearing.
                out.push(Event::Fill(fill.clone()));
                out.push(Event::OrderFilled(OrderFilled {
                    client_order_id: coid.to_string(),
                    fill,
                    ts: fts,
                }));
            }
        }
    }
    out
}

/// Encode+write one outbound command. Returns `false` on Shutdown (caller breaks the loop).
///
/// Only the three ORDER commands (`NewOrder`/`CancelOrder`/`AmendOrder`) set the envelope
/// `clientMsgId` to the real `client_order_id` — that is the ONLY id `on_error_res`/
/// `on_order_error_event` may correlate to a synthesized order event. Every non-order command
/// (Subscribe/Unsubscribe spots+trendbar, GetTrendbars) sends an EMPTY `clientMsgId`: its response
/// is routed by `payload_type` in `on_inbound`, never by envelope id, so there is nothing to
/// correlate — and a static per-verb label here (the old "sub"/"unsub"/"subtb"/"unsubtb"/"gtb")
/// would otherwise get echoed back on a failing request's `ERROR_RES` and be misread by
/// `on_error_res` as a genuine order's coid, fabricating a phantom `Event::OrderRejected` on a
/// combined data+exec connection (the bug this comment documents the fix for).
fn write_command(reader: &mut Reader, ctid: i64, cmd: Command) -> io::Result<bool> {
    match cmd {
        Command::SubscribeSpots { symbol_id } => {
            let req = ProtoOaSubscribeSpotsReq {
                ctid_trader_account_id: ctid,
                symbol_id: vec![symbol_id],
                ..Default::default()
            };
            send(reader, pt::SUBSCRIBE_SPOTS_REQ, &req, "")?;
        }
        Command::UnsubscribeSpots { symbol_id } => {
            let req = ProtoOaUnsubscribeSpotsReq {
                ctid_trader_account_id: ctid,
                symbol_id: vec![symbol_id],
                ..Default::default()
            };
            send(reader, pt::UNSUBSCRIBE_SPOTS_REQ, &req, "")?;
        }
        Command::SubscribeTrendbar { symbol_id, period } => {
            let req = ProtoOaSubscribeLiveTrendbarReq {
                ctid_trader_account_id: ctid,
                period: period as i32,
                symbol_id,
                ..Default::default()
            };
            send(reader, pt::SUBSCRIBE_LIVE_TRENDBAR_REQ, &req, "")?;
        }
        Command::UnsubscribeTrendbar { symbol_id, period } => {
            let req = ProtoOaUnsubscribeLiveTrendbarReq {
                ctid_trader_account_id: ctid,
                period: period as i32,
                symbol_id,
                ..Default::default()
            };
            send(reader, pt::UNSUBSCRIBE_LIVE_TRENDBAR_REQ, &req, "")?;
        }
        Command::GetTrendbars { symbol_id, period, from_ts, to_ts, count } => {
            let req = ProtoOaGetTrendbarsReq {
                ctid_trader_account_id: ctid,
                from_timestamp: Some(from_ts),
                to_timestamp: Some(to_ts),
                period: period as i32,
                symbol_id,
                count: Some(count),
                ..Default::default()
            };
            send(reader, pt::GET_TRENDBARS_REQ, &req, "")?;
        }
        // The envelope `clientMsgId` is set to the order's `client_order_id` (not a static per-verb
        // label) so cTrader's echo of it on the response/`ERROR_RES` correlates the reply back to
        // the order (see `on_error_res`). NewOrder ALSO keeps the coid in the wire `label`/
        // `client_order_id` fields (set by the mapper) for the exec-event correlation path.
        Command::NewOrder(req) => {
            let msg_id =
                req.client_order_id.clone().or_else(|| req.label.clone()).unwrap_or_default();
            send(reader, pt::NEW_ORDER_REQ, &req, &msg_id)?
        }
        Command::CancelOrder { order_id, client_order_id } => {
            let req = ProtoOaCancelOrderReq {
                ctid_trader_account_id: ctid,
                order_id,
                ..Default::default()
            };
            send(reader, pt::CANCEL_ORDER_REQ, &req, &client_order_id)?;
        }
        Command::AmendOrder { req, client_order_id } => {
            send(reader, pt::AMEND_ORDER_REQ, &req, &client_order_id)?
        }
        // Close-by-position-id (reduce/flatten). The envelope `clientMsgId` is the reducing submit's
        // coid (like every other order command) so a venue `ERROR_RES` correlates back to it; the
        // successful close's execution events correlate by `positionId` instead (the closing order
        // carries no coid of ours) — see `map_close_event`.
        Command::ClosePosition { position_id, volume, client_order_id } => {
            let req = ProtoOaClosePositionReq {
                ctid_trader_account_id: ctid,
                position_id,
                volume,
                ..Default::default()
            };
            send(reader, pt::CLOSE_POSITION_REQ, &req, &client_order_id)?;
        }
        Command::Shutdown => return Ok(false),
    }
    Ok(true)
}

/// Initial backoff before the first reconnect attempt after a dropped socket; doubles on every
/// failed attempt, capped at [`RECONNECT_MAX_BACKOFF`]. Short enough that the in-process fake
/// server test reconnects promptly; long enough in production not to hammer a briefly-down venue.
const RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(200);
/// Cap on the reconnect backoff (see [`RECONNECT_INITIAL_BACKOFF`]).
const RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Track/untrack a (de)subscription command against the actor's own view of "what should be live
/// right now" — the set [`reconnect_with_backoff`]'s caller replays after a reconnect (cTrader does
/// not remember subscriptions across a dropped socket).
fn track_subscription(
    cmd: &Command,
    spot_subs: &mut HashSet<i64>,
    trendbar_subs: &mut HashSet<(i64, ProtoOaTrendbarPeriod)>,
) {
    match cmd {
        Command::SubscribeSpots { symbol_id } => {
            spot_subs.insert(*symbol_id);
        }
        Command::UnsubscribeSpots { symbol_id } => {
            spot_subs.remove(symbol_id);
        }
        Command::SubscribeTrendbar { symbol_id, period } => {
            trendbar_subs.insert((*symbol_id, *period));
        }
        Command::UnsubscribeTrendbar { symbol_id, period } => {
            trendbar_subs.remove(&(*symbol_id, *period));
        }
        Command::GetTrendbars { .. }
        | Command::NewOrder(_)
        | Command::CancelOrder { .. }
        | Command::AmendOrder { .. }
        | Command::ClosePosition { .. } => {}
        Command::Shutdown => {}
    }
}

/// Reason text embedded in a synthesized reject when a command's write to the socket failed
/// mid-send (Task 6, Fix 1). Kept as static text — never interpolate the underlying `io::Error`
/// (mirrors `try_refresh_and_reauth`'s no-secrets-in-messages discipline elsewhere in this file).
const WRITE_FAILED_NEW_ORDER: &str = "submit write failed; connection reset";
const WRITE_FAILED_CANCEL: &str = "cancel write failed; connection reset";
const WRITE_FAILED_AMEND: &str = "amend write failed; connection reset";
const WRITE_FAILED_CLOSE: &str = "close-position write failed; connection reset";

/// Build the canonical event to synthesize when `cmd`'s write to the socket failed, so the
/// specific order-affecting command that never reached the wire never vanishes from the core's
/// event stream (the venue-adapter contract: "No order may silently vanish"). Called on a
/// `&Command` BEFORE it is handed to [`write_command`] (which consumes it), so the coid is still
/// available even though the write itself fails.
///
/// `NewOrder` synthesizes a TERMINAL `OrderRejected` — a partially-sent order could double-fill if
/// blindly retried, so this rejects and leaves resubmission to the strategy rather than
/// auto-retrying. `CancelOrder`/`AmendOrder` synthesize their NON-terminal advisory reject (the
/// order stays live under its existing terms), matching [`crate::exec::CtraderExec`]'s existing
/// dead-actor-channel handling for the same two commands. Subscribe/Unsubscribe/GetTrendbars/
/// Shutdown carry no order intent and need no synthesis (a failed subscribe still self-heals via
/// the tracked-subscription replay on reconnect, since `track_subscription` runs before the write
/// — see [`replay_after_reconnect`]; a failed `GetTrendbars` seed is simply lost — best-effort,
/// per that command's own doc — while the independently-sent `SubscribeTrendbar` still starts the
/// live stream).
///
/// `ClosePosition` picks a LEGAL terminal via [`close_terminal_event`] (Filled-if-an-earlier-leg-
/// progressed, else Rejected) so a close leg that dies on the wire never strands the coid. This
/// function is PURE — it is called for EVERY drained command BEFORE the write is even attempted, so
/// it must NOT mutate `closes` (an earlier bug forgot the coid here, wiping the just-registered
/// mapping on a SUCCESSFUL write); the tracker `forget` happens only at the actual failure site (see
/// [`forget_close_on_write_failure`]). It reads `closes` only for the non-mutating progress check.
/// The coid of a `ClosePosition` command (else `None`), captured BEFORE `write_command` consumes it
/// so a write FAILURE can [`forget`](crate::positions::CloseTracker::forget) it from `closes`: that
/// leg will never reach the venue, and a stale `by_position` entry would otherwise misroute a later
/// leg's fill or block the next reduce on the position. No-op for every non-close command.
fn close_command_coid(cmd: &Command) -> Option<String> {
    match cmd {
        Command::ClosePosition { client_order_id, .. } => Some(client_order_id.clone()),
        _ => None,
    }
}

/// FORGET a `ClosePosition`'s coid from the tracker at the actual write-failure site (paired with
/// [`close_command_coid`]'s capture). A no-op when `close_coid` is `None` (non-close command).
fn forget_close_on_write_failure(close_coid: &Option<String>, closes: &CloseState) {
    if let Some(coid) = close_coid {
        if let Ok(mut t) = closes.lock() {
            t.forget(coid);
        }
    }
}

fn write_failure_event(cmd: &Command, closes: &CloseState) -> Option<Event> {
    match cmd {
        Command::NewOrder(req) => {
            let coid = req.client_order_id.clone().or_else(|| req.label.clone())?;
            Some(Event::OrderRejected(OrderRejected {
                client_order_id: coid,
                reason: WRITE_FAILED_NEW_ORDER.into(),
                ts: now_ms(),
            }))
        }
        Command::CancelOrder { client_order_id, .. } => {
            Some(Event::OrderCancelRejected(OrderCancelRejected {
                client_order_id: client_order_id.clone(),
                reason: WRITE_FAILED_CANCEL.into(),
                ts: now_ms(),
            }))
        }
        Command::AmendOrder { client_order_id, .. } => {
            Some(Event::OrderModifyRejected(OrderModifyRejected {
                client_order_id: client_order_id.clone(),
                reason: WRITE_FAILED_AMEND.into(),
                ts: now_ms(),
            }))
        }
        // A reduce whose close leg never reached the wire: a LEGAL terminal (Filled if an earlier
        // leg already progressed the FSM, else Rejected) — built WITHOUT forgetting (this runs at
        // capture time, even for a write that will SUCCEED). The forget is done at the failure site
        // by `forget_close_on_write_failure`.
        Command::ClosePosition { client_order_id, .. } => {
            let progressed = closes.lock().ok().is_some_and(|t| t.has_progress(client_order_id));
            Some(close_terminal_event(client_order_id, progressed, WRITE_FAILED_CLOSE, now_ms()))
        }
        Command::SubscribeSpots { .. }
        | Command::UnsubscribeSpots { .. }
        | Command::SubscribeTrendbar { .. }
        | Command::UnsubscribeTrendbar { .. }
        | Command::GetTrendbars { .. }
        | Command::Shutdown => None,
    }
}

/// Heuristic: does this `ERROR_RES` frame represent an auth/token failure (vs. some other venue
/// error, e.g. a rejected order) that should trigger a token refresh + re-auth rather than just
/// being logged? cTrader's Open API does not publish a closed enum of `errorCode` strings for
/// this; matching `AUTH`/`TOKEN` substrings (case-insensitive) covers the documented codes (e.g.
/// `OA_AUTH_TOKEN_EXPIRED`, `ACCESS_TOKEN_INVALID`) without hardcoding an exact list that could
/// drift from the venue's actual wire behavior.
fn is_auth_error(msg: &ProtoMessage) -> bool {
    if msg.payload_type != pt::ERROR_RES {
        return false;
    }
    match decode_error(msg) {
        ConnError::Venue { error_code, .. } => {
            let code = error_code.to_ascii_uppercase();
            code.contains("AUTH") || code.contains("TOKEN")
        }
        _ => false,
    }
}

/// Wall-clock ms. The ONE clock reading in this file, and it is off the fold: it is taken on the
/// actor thread at a heartbeat tick or after an OAuth round trip, never per message.
fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Adopt a freshly-refreshed grant into `cfg` and PERSIST it to the credential store.
///
/// Three things move together, which is why this is one function rather than three lines at two
/// call sites (the reactive refresh and the proactive one):
///
/// 1. the in-memory access token, so the re-auth below and any LATER reconnect carry it;
/// 2. the refresh token — cTrader ROTATES it, so keeping the old one would spend a dead token on
///    the next refresh;
/// 3. the expiry, so the proactive tick knows when to go again.
///
/// The store write is BEST-EFFORT and never fails the refresh: the running session already holds a
/// working token, and refusing to continue because a file could not be written would turn a
/// successful self-heal into an outage. A failure is `warn!`ed with the PATH and the KEY NAMES —
/// never a token value — and says what the consequence is (a restart falls back to the stale pair).
fn adopt_refreshed_token(cfg: &mut ConnConfig, token: &oauth::Token) {
    // ONE clock read for both consumers: the expiry arithmetic and the change-journal stamp
    // (`vike_model::change_journal` reads no clock — the instant is a parameter all the way down).
    let now_ms = wall_clock_ms();
    cfg.access_token = token.access_token.clone();
    cfg.refresh_token = Some(token.refresh_token.clone());
    cfg.token_expires_at_ms = crate::token_store::expires_at_ms(now_ms, token.expires_in);

    let Some(persist) = cfg.token_persist.clone() else {
        tracing::debug!(
            target: "ctrader",
            "token refreshed; no credential store was resolved for this mount, so the new pair              lives only as long as this process"
        );
        return;
    };
    match crate::token_store::persist(&persist, token, now_ms) {
        // The PATH and the KEY NAMES, never a value — and the keys are what make it verifiable.
        Ok(()) => tracing::info!(
            target: "ctrader",
            store = %persist.store.display(),
            access_key = %persist.keys.access,
            refresh_key = %persist.keys.refresh,
            "refreshed cTrader grant persisted to the credential store (other lines untouched)"
        ),
        Err(e) => {
            tracing::warn!(target: "ctrader", error = %e, "could not persist the refreshed grant")
        }
    }
}

/// Handle an `ACCOUNTS_TOKEN_INVALIDATED_EVENT` or an auth-flavored `ERROR_RES`: refresh the
/// access token via `oauth::refresh` and re-send `AccountAuth` on the SAME socket (a token
/// invalidation does not imply the transport died — no reconnect needed). Updates `cfg` in place
/// so a LATER reconnect attempt also carries the fresh token/refresh_token pair.
///
/// Returns `false` when there is no configured `refresh_token`, or the refresh/re-auth itself
/// fails — the caller degrades (stops the actor). Exactly ONE `tracing::error!` is emitted in
/// that case, and it is a static message: the underlying `OAuthError`/`io::Error` is deliberately
/// NEVER interpolated into it, because `oauth`'s token-endpoint URL embeds the client_secret and
/// refresh_token as query params — a transport error's `Display` can otherwise echo that URL back.
fn try_refresh_and_reauth(cfg: &mut ConnConfig, reader: &mut Reader, ctid: i64) -> bool {
    let Some(refresh_token) = cfg.refresh_token.clone() else {
        tracing::error!(
            target: "ctrader",
            "access token invalidated and no refresh_token configured; actor stopping"
        );
        return false;
    };
    let token = match oauth::refresh(&cfg.client_id, &cfg.client_secret, &refresh_token) {
        Ok(token) => token,
        Err(_) => {
            tracing::error!(
                target: "ctrader",
                "OAuth token refresh failed; actor stopping (no creds/token in this message)"
            );
            return false;
        }
    };
    adopt_refreshed_token(cfg, &token);

    let account_auth = ProtoOaAccountAuthReq {
        ctid_trader_account_id: ctid,
        access_token: cfg.access_token.clone(),
        ..Default::default()
    };
    if send(reader, pt::ACCOUNT_AUTH_REQ, &account_auth, "aa-refresh").is_err() {
        tracing::error!(
            target: "ctrader",
            "failed to re-send AccountAuth after token refresh; actor stopping"
        );
        return false;
    }
    tracing::info!(target: "ctrader", "access token refreshed; re-authenticated");
    true
}

/// Reconnect loop for a dropped socket: waits out an exponential backoff, then replays the full
/// connect + auth + symbol-discovery handshake ([`open_and_handshake`]) pinned to the SAME `ctid`.
/// Checks `rx` for a pending `Shutdown` before every attempt, buffering any OTHER queued command
/// into `pending` (in arrival order) so the caller can replay it once reconnected instead of
/// silently dropping it. Returns `None` when `Shutdown` was observed (or the channel died) — the
/// caller should stop the actor without retrying further.
fn reconnect_with_backoff(
    cfg: &ConnConfig,
    ctid: i64,
    rx: &Receiver<Command>,
    pending: &mut Vec<Command>,
) -> Option<(Reader, SymbolMap, u32)> {
    let mut backoff = RECONNECT_INITIAL_BACKOFF;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Command::Shutdown) => return None,
                Ok(cmd) => pending.push(cmd),
                Err(TryRecvError::Disconnected) => return None,
                Err(TryRecvError::Empty) => break,
            }
        }
        thread::sleep(backoff);
        match open_and_handshake(cfg, Some(ctid)) {
            Ok((reader, _ctid, symbols, money_digits)) => {
                tracing::info!(target: "ctrader", "reconnected and re-authenticated");
                return Some((reader, symbols, money_digits));
            }
            Err(e) => {
                tracing::warn!(
                    target: "ctrader",
                    error = %e,
                    backoff_ms = backoff.as_millis() as u64,
                    "reconnect attempt failed; retrying"
                );
                backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
            }
        }
    }
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
///   SYNCHRONOUSLY at mount (`crates/vike-mount/src/lib.rs`'s `make_engine`, whose `("ctrader", _)`
///   arm connects there and then), and `crates/vike-run/src/node.rs` mounts venues one after
///   another — so every millisecond here is paid by an operator watching a daemon that has not
///   started yet, and a mount that looks hung is a mount somebody kills.
///
/// **MEASURED, against the real `demo.ctraderapi.com:5035` (the CI box, 2026-08-08, 15 real mounts by
/// `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs`): the seed's own round trip took 24ms
/// median, 23ms min, 25ms max — a 2ms spread.** 1500ms is therefore ~60x the observed cost,
/// and it is a bound on a socket that has ALREADY completed the handshake's six request/response
/// round trips (app auth → [accounts] → account auth → trader → symbols list → symbol-by-id,
/// [`open_and_handshake`]) milliseconds earlier. A venue that answers six times and then goes quiet
/// for 1.5s is not "a bit slow"; it is anomalous, and waiting 3.5s longer for it buys nothing.
///
/// ⚠ **The stall it produces is QUANTIZED to the socket's 1s read timeout, so this constant is a
/// floor on the wait, not the wait.** [`read_reconcile_res`] tests the deadline between reads, and
/// [`open_and_handshake`] leaves the socket on a 1s `set_read_timeout` — so a mute venue is left in
/// a blocking read that only returns on a 1s boundary, and 1500ms costs the mount **2.0s**
/// (measured: 2.030s / 2.077s / 2.080s, three reps against
/// `crates/bridges/ctrader/tests/common/mod.rs`'s `FakeCtrader::start_mute_reconcile`). Under
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

/// The result of waiting for a `RECONCILE_RES` in [`read_reconcile_res`]. Distinguished so the
/// caller can log a NETWORK stall (`TimedOut`, includes socket-death) apart from a venue-side
/// reconcile REJECTION (`Rejected`, an `ERROR_RES`) — different triage (F3 fix 4).
enum ReconcileOutcome {
    /// The venue answered with a decodable `RECONCILE_RES`.
    Received(ProtoOaReconcileRes),
    /// The venue answered with an `ERROR_RES` (reconcile rejected), or a `RECONCILE_RES` that
    /// failed to decode.
    Rejected,
    /// The deadline elapsed / the socket died before any reconcile response arrived.
    TimedOut,
}

/// Read frames until a `RECONCILE_RES` arrives (decoded + returned), the deadline elapses, an
/// `ERROR_RES` arrives, or the socket closes. Every OTHER frame read while waiting — critically an
/// `EXECUTION_EVENT` (cTrader pushes fills/accepts/cancels UNCONDITIONALLY once authorized, NOT
/// subscription-gated, so a real fill can land inside this ≤5s window) but also any spot/other
/// frame — is BUFFERED into `buffered` (not dropped) so the caller can dispatch it through the
/// normal inbound path after the reconcile resolves; dropping it would silently diverge `Account`/
/// the OMS ("no order silently vanishes"). Never propagates an error — reconcile is best-effort.
fn read_reconcile_res(
    reader: &mut Reader,
    buffered: &mut Vec<ProtoMessage>,
    timeout: Duration,
) -> ReconcileOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            return ReconcileOutcome::TimedOut;
        }
        match reader.next_frame() {
            Ok(Some(msg)) if msg.payload_type == pt::RECONCILE_RES => {
                return match ProtoOaReconcileRes::decode(msg.payload.as_deref().unwrap_or(&[])) {
                    Ok(res) => ReconcileOutcome::Received(res),
                    Err(_) => ReconcileOutcome::Rejected,
                };
            }
            Ok(Some(msg)) if msg.payload_type == pt::ERROR_RES => {
                return ReconcileOutcome::Rejected
            }
            Ok(Some(msg)) => buffered.push(msg), // in-flight exec/spot/other frame — buffer, replay
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    return ReconcileOutcome::TimedOut; // socket died — next read reconnects
                }
                // read timeout / partial — retry until the deadline
            }
            Err(_) => return ReconcileOutcome::TimedOut,
        }
    }
}

/// Pull the referenced order's correlation id (its dedicated `client_order_id`, falling back to the
/// `label` we set at submit time), or `None` when neither is present/non-empty. Mirrors
/// [`update_order_map`]'s key derivation so the reconcile-rebuilt map keys match the live path.
fn reconcile_order_coid(order: &crate::proto::ProtoOaOrder) -> Option<String> {
    order
        .client_order_id
        .clone()
        .or_else(|| order.trade_data.label.clone())
        .filter(|c| !c.is_empty())
}

/// Fold a `RECONCILE_RES`'s OPEN-position set into the shared book — marking it FETCHED **only if
/// it is about THIS account and every row was understood**. The one writer of
/// [`crate::positions::PositionBook::replace_all`], shared by the INITIAL-connect seed
/// ([`seed_positions_at_connect`]) and the reconnect rebuild ([`reconcile_after_reconnect`]) so the
/// two cannot disagree about what a reconcile answer means.
///
/// ⚠ **`ctid` is checked FIRST, and its absence was a live defect.** `ProtoOAReconcileRes` carries
/// the `ctidTraderAccountId` it answers for, and nothing compared it. An answer for ANOTHER account
/// was folded in wholesale — replacing this account's routing entries with a stranger's, and
/// marking the result AUTHORITATIVE, so a stranger's flat book became this account's `Flat` and
/// refused its exit under `halt_admit = "verify"`
/// (`crates/bridges/ctrader/tests/exec_halt.rs`'s
/// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` measured exactly that).
/// A mismatched answer therefore changes NOTHING except to `invalidate` the evidence flag: we
/// asked, and what came back was not about us, so whatever we believed is now of unknown standing.
///
/// ⚠ **The `unreadable` arm is not defensive decoration — it is the difference between evidence and
/// a manufactured fact.** A row this build cannot classify
/// (`crates/bridges/ctrader/src/positions.rs`'s `reconcile_rows`: an ERROR-status position, a
/// `positionStatus` the vendored proto has no variant for, an OPEN row with a non-positive volume)
/// is a HOLE, and a hole is indistinguishable from an absence once it is dropped. So the entries are
/// taken either way — routing runs on the best guess available, exactly as after an
/// [`crate::positions::PositionBook::invalidate`] — and the book declines to call itself evidence,
/// which lands at the halt boundary as `Unknown` and ADMITS.
///
/// ⚠ **Neither arm can see a row the venue never SENT**, and that is why the halt boundary does not
/// rest on this function alone: `PositionBook::unauthoritative_for` additionally demands POSITIVE
/// coverage of the symbol being judged. Everything here is about the provenance of an answer; only
/// coverage is about what the answer actually said.
fn rebuild_position_book(res: &ProtoOaReconcileRes, ctid: i64, positions: &PositionMap) {
    if res.ctid_trader_account_id != ctid {
        if let Ok(mut book) = positions.lock() {
            book.invalidate();
        }
        tracing::error!(
            target: "ctrader",
            expected_ctid = ctid,
            answered_for = res.ctid_trader_account_id,
            positions = res.position.len(),
            "a RECONCILE_RES arrived for a DIFFERENT ctidTraderAccountId; discarding it and \
             clearing this book's evidence flag — another account's positions (or its emptiness) \
             are not this account's"
        );
        return;
    }
    let (tracked, unreadable) = positions::reconcile_rows(&res.position);
    let Ok(mut book) = positions.lock() else { return };
    if unreadable == 0 {
        book.replace_all(tracked, now_ms());
        return;
    }
    let kept = tracked.len();
    book.replace_unverified(tracked);
    tracing::warn!(
        target: "ctrader",
        reported = res.position.len(),
        tracked = kept,
        unreadable,
        "a reconcile answer carried position rows this build could not classify; the position \
         book is REPLACED for routing but stays UNVERIFIED, so a halt under halt_admit=verify \
         will ADMIT rather than read the gap as a flat account"
    );
}

/// Ask the venue for its OPEN positions at INITIAL connect and seed the book with the answer.
///
/// ⚠ **This did not exist, and its absence was a live bug.** `ProtoOAReconcileReq` was issued only
/// on RECONNECT, so a freshly-mounted process started with an EMPTY position map and — on a process
/// that neither trades nor loses its socket — kept it empty forever. Against an empty map
/// [`crate::positions::plan_reduce`] sees no opposing exposure and routes a flatten as a NEW ORDER,
/// stacking a hedge on a hedging account; `crates/bridges/ctrader/src/exec.rs`'s `close_all` exists
/// precisely as the manual workaround for the book that resulted. Seeding here fixes the routing
/// bug on its own, independently of any halt policy.
///
/// It also makes "flat" a FACT rather than an absence, which is what a position-verified halt admit
/// needs (`crates/bridges/ctrader/src/positions.rs`'s `PositionBook`).
///
/// Runs **before the actor thread starts**, on the handshake's own socket and its own reader, so a
/// mount returns with a seeded book rather than one that fills in asynchronously. Best-effort
/// throughout, exactly like the reconnect twin: a write failure, a venue rejection or the bounded
/// [`SEED_TIMEOUT`] leaves the book UNFETCHED (the honest state — everything downstream reads
/// that as "unknown") and the mount proceeds. Frames that arrive while waiting are BUFFERED and
/// dispatched through the normal inbound path, never dropped.
///
/// # ⚠ What this costs, in milliseconds an operator waits
///
/// This is a BLOCKING round trip added to the mount path, and cTrader is the one venue whose exec
/// client connects SYNCHRONOUSLY at mount (`crates/vike-mount/src/lib.rs`'s `make_engine`, whose
/// `("ctrader", _)` arm connects there and then), so the number is startup latency, not background
/// work. All three figures are MEASURED, none estimated:
///
/// - **Healthy: 24ms median (23ms min / 25ms max, 15 real mounts against
///   `demo.ctraderapi.com:5035`, the CI box 2026-08-08 —
///   `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs` is the harness and reruns it).** For
///   scale, the SAME 15 mounts' whole-handshake wall times ranged 353ms–35.3s, dominated by OAuth
///   and the symbol-list download; the seed is a rounding error beside the connect it rides on.
/// - **Mute venue (accepts, handshakes, then never answers this one request): 2.08s measured**
///   (2.030s / 2.077s / 2.080s) — [`SEED_TIMEOUT`] rounded up to the socket's 1s read granularity,
///   which that constant's doc explains. That is the honest worst case, and the reason the bound is
///   its own constant rather than the reconnect twin's: measured under [`RECONCILE_TIMEOUT`]
///   instead, the identical stall was **5.120s**.
/// - **Every OTHER failure is fast, not bounded-slow**: a write failure and a venue `ERROR_RES`
///   both return on the spot, and a dead socket returns at EOF rather than at the deadline
///   ([`read_reconcile_res`]'s `Ok(None)` + `eof` arm).
///
/// **It does NOT stack, on either axis.**
/// - *Per process*: one seed. `crates/vike-mount/src/lib.rs`'s `make_engine` builds ONE cTrader
///   engine (`crates/vike-run/src/node.rs`'s `CTRADER_MARKET`), extra symbols ride that same
///   handle as declared legs rather than as second mounts, and a DATA connection is excluded
///   outright by the `events.is_some()` gate at the call site.
/// - *Per ACCOUNT*: not representable today — `crates/bridges/ctrader/src/config.rs`'s
///   `CtraderConfig::from_vars` resolves exactly one `CTRADER_DEMO_*` account and nothing loops
///   over a set. If a second account is ever mounted it is a second `connect_and_auth_exec`, and it
///   pays its own seed, sequentially; that is a reason to bound this call, and it is bounded.
/// - *Per RECONNECT*: nothing new. [`reconcile_after_reconnect`] has always issued this same
///   `ProtoOAReconcileReq` under [`RECONCILE_TIMEOUT`], on the actor thread, so a reconnect storm
///   costs exactly what it cost before this function existed — and costs it where nobody is
///   blocked.
///
/// Every outcome logs `elapsed_ms`, so the cost is observable in production rather than inferable
/// from this doc.
///
/// Deliberately seeds positions ONLY — not the coid→orderId map, and it re-emits no `OrderAccepted`.
/// At initial connect the core tracks no orders, so both would be dropped as unknown coids; the
/// reconnect path owns those because it has a BEFORE state to reconcile against.
#[allow(clippy::too_many_arguments)]
fn seed_positions_at_connect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // EMPTY clientMsgId, for the reason `reconcile_after_reconnect` spells out: a LATE `ERROR_RES`
    // for this reconcile must not carry a string `on_error_res` would misread as an order's coid.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "initial position-seed request write failed; the position book stays UNFETCHED \
             (reduce routing may open a hedge against a pre-existing position — use close_all)"
        );
        return;
    }

    // `elapsed_ms` on EVERY arm, because this call is SYNCHRONOUS STARTUP COST an operator pays and
    // could not otherwise see: it is the only thing standing between "the mount is slow" and "the
    // mount is hung". The three numbers it reports are the ones the timeout below is sized against.
    let started = Instant::now();
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    match read_reconcile_res(reader, &mut buffered, SEED_TIMEOUT) {
        ReconcileOutcome::Received(res) => {
            rebuild_position_book(&res, ctid, positions);
            tracing::info!(
                target: "ctrader",
                positions = res.position.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "position book seeded at connect"
            );
        }
        ReconcileOutcome::Rejected => tracing::warn!(
            target: "ctrader",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "initial position seed rejected by the venue (ERROR_RES); the position book stays \
             UNFETCHED"
        ),
        ReconcileOutcome::TimedOut => tracing::warn!(
            target: "ctrader",
            timeout_ms = SEED_TIMEOUT.as_millis() as u64,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "initial position seed not answered before timeout; the position book stays UNFETCHED"
        ),
    }

    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
}

/// F3 (reconcile exec state on reconnect). Right after a reconnect+reauth — BEFORE subscriptions
/// replay — ask the venue for its current OPEN positions + PENDING orders (`ProtoOAReconcileReq`)
/// and, from the answer:
///   1. **Rebuild the coid→orderId map** from the pending orders, so post-reconnect cancel/modify
///      (which key on the numeric `orderId`) resolve again — the concretely-valuable, unambiguously
///      -safe fix.
///   2. **Re-emit an `OrderAccepted`** (NOT a fill) for each still-pending order. This is SAFE
///      because `OrderAccepted` never folds into `Account` (no trades/PnL under any path in the core
///      engine's `on_event`): a duplicate on an order the core still tracks is dropped by the FSM
///      (accept is legal only from SUBMITTED), and one for an order the core lost is dropped as an
///      unknown coid — neither double-counts anything. It re-establishes a live order the core may
///      have lost during the dead window; harmless if already known.
///   3. **Gap observability only**: for any coid the adapter tracked BEFORE the reconnect that is
///      ABSENT from the pending set, log ONE `warn` (a possible gap fill/cancel). We do NOT
///      synthesize a terminal (`OrderFilled`/`OrderCanceled`) — reconcile alone cannot tell fill
///      from cancel, and a wrong terminal is worse than a logged gap. Full deal-history recovery is
///      out of scope.
///
/// Best-effort throughout: a write/response failure logs once and returns; the caller still replays
/// subscriptions.
#[allow(clippy::too_many_arguments)]
fn reconcile_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
    events: Option<&EventSender>,
) {
    // Snapshot the coids the adapter knew BEFORE the reconnect (for the gap check below). Terminal
    // coids are pruned from the map as their FILLED/CANCELLED/REJECTED/EXPIRED events arrive
    // (`update_order_map`), so this set holds only still-live orders — the gap-diff below is
    // therefore actionable, not spammed by historically-completed coids.
    let pre: HashSet<String> =
        orders.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default();

    let req = ProtoOaReconcileReq { ctid_trader_account_id: ctid, ..Default::default() };
    // Non-order request → EMPTY clientMsgId (F2b invariant, see `write_command`'s doc): a late
    // `ERROR_RES` for this reconcile — one arriving AFTER our timeout, so it falls through to
    // `on_error_res` on the main loop — must not carry a coid that `on_error_res` would misread as a
    // real order's, fabricating a phantom `OrderRejected{client_order_id:"recon"}`.
    if send(reader, pt::RECONCILE_REQ, &req, "").is_err() {
        tracing::warn!(
            target: "ctrader",
            "reconcile request write failed; skipping reconcile (subscriptions still replay)"
        );
        return;
    }

    // Buffer any non-reconcile frame read while waiting (in-flight EXECUTION_EVENTs, spot, …); they
    // are dispatched through the normal inbound path AFTER the reconcile resolves, below — never
    // dropped.
    let mut buffered: Vec<ProtoMessage> = Vec::new();
    let res = match read_reconcile_res(reader, &mut buffered, RECONCILE_TIMEOUT) {
        ReconcileOutcome::Received(res) => Some(res),
        ReconcileOutcome::Rejected => {
            tracing::warn!(
                target: "ctrader",
                "reconcile rejected by the venue (ERROR_RES); skipping reconcile \
                 (subscriptions still replay)"
            );
            None
        }
        ReconcileOutcome::TimedOut => {
            tracing::warn!(
                target: "ctrader",
                timeout_ms = RECONCILE_TIMEOUT.as_millis() as u64,
                "reconcile response not received before timeout (network stall / socket death); \
                 skipping reconcile (subscriptions still replay)"
            );
            None
        }
    };

    if let Some(res) = res {
        // 1. Rebuild the coid→orderId correlation map from the pending orders.
        let mut present: HashSet<String> = HashSet::new();
        if let Ok(mut map) = orders.lock() {
            for order in &res.order {
                if let Some(coid) = reconcile_order_coid(order) {
                    map.insert(coid.clone(), order.order_id);
                    present.insert(coid);
                }
            }
        }

        // 2. Re-emit OrderAccepted for each still-pending order (idempotent — never a fill).
        if let Some(events) = events {
            for order in &res.order {
                let Some(coid) = reconcile_order_coid(order) else { continue };
                let accepted = Event::OrderAccepted(OrderAccepted {
                    client_order_id: coid,
                    venue_order_id: Some(order.order_id.to_string().into()),
                    ts: order.utc_last_update_timestamp.unwrap_or_else(now_ms),
                });
                if events.blocking_send(accepted).is_err() {
                    tracing::warn!(
                        target: "ctrader",
                        "core ingest gone; dropping reconcile re-establish event"
                    );
                }
            }
        }

        // 3. Rebuild the OPEN-position map from the reconcile's authoritative position set, so the
        //    exec client's reduce routing survives a reconnect (an open the adapter learned before
        //    the outage would otherwise be forgotten, misrouting a post-reconnect flatten as an
        //    open). The reconcile lists ONLY open positions, so replacing the set wholesale is
        //    correct — anything closed during the dead window is absent and thus dropped. This also
        //    RE-VALIDATES the book as evidence (`fetched_at_ms`), which the reconnect invalidated;
        //    a reconcile that never answered leaves it invalidated, which is the honest state.
        rebuild_position_book(&res, ctid, positions);

        // 4. Observability-only gap check: coids tracked before the reconnect but no longer pending.
        for coid in pre.difference(&present) {
            tracing::warn!(
                target: "ctrader",
                coid = %coid,
                "reconcile: previously-tracked order absent from the venue's pending set on \
                 reconnect; possible fill/cancel during the dead window (no terminal synthesized — \
                 deal-history recovery is out of scope)"
            );
        }
        tracing::info!(
            target: "ctrader",
            pending = res.order.len(),
            positions = res.position.len(),
            "reconcile complete on reconnect"
        );
    }

    // Dispatch every buffered in-flight frame through the SAME inbound handler the main actor loop
    // uses, IN ARRIVAL ORDER — so a fill/accept/cancel (or spot) that landed during the reconcile
    // window is processed normally (folded into the ingest lane / sink) rather than lost. Runs
    // regardless of the reconcile outcome above (a fill can arrive even if the reconcile itself
    // times out or is rejected).
    for msg in buffered {
        on_inbound(&msg, symbols, money_digits, sink, events, orders, positions, closes);
    }
}

/// After a successful reconnect+reauth, reconcile exec state (F3, [`reconcile_after_reconnect`])
/// then re-issue every currently-active spot/trendbar subscription (cTrader does not remember
/// subscriptions across a dropped socket) and flush any commands that
/// arrived from `rx` while the actor was reconnecting (buffered by [`reconnect_with_backoff`], in
/// arrival order). A failed SUBSCRIBE replay is swallowed — if the fresh connection is ALSO already
/// broken, the next read hits an error/EOF and triggers another reconnect naturally. But a failed
/// ORDER-command replay must NOT vanish: it synthesizes the same terminal/non-terminal reject the
/// main drain loop does (via [`write_failure_event`]) so the venue-adapter contract still holds.
#[allow(clippy::too_many_arguments)]
fn replay_after_reconnect(
    reader: &mut Reader,
    ctid: i64,
    symbols: &SymbolMap,
    money_digits: u32,
    sink: &dyn LiveDataSink,
    spot_subs: &HashSet<i64>,
    trendbar_subs: &HashSet<(i64, ProtoOaTrendbarPeriod)>,
    pending: Vec<Command>,
    events: Option<&EventSender>,
    orders: &OrderIdMap,
    positions: &PositionMap,
    closes: &CloseState,
) {
    // Reconcile FIRST — restore the coid→orderId map + re-establish pending orders before any new
    // subscription frame can interleave (and before a replayed order command needs the map).
    reconcile_after_reconnect(
        reader,
        ctid,
        symbols,
        money_digits,
        sink,
        orders,
        positions,
        closes,
        events,
    );
    for &symbol_id in spot_subs {
        let _ = write_command(reader, ctid, Command::SubscribeSpots { symbol_id });
    }
    for &(symbol_id, period) in trendbar_subs {
        let _ = write_command(reader, ctid, Command::SubscribeTrendbar { symbol_id, period });
    }
    for cmd in pending {
        // Captured BEFORE `cmd` is consumed by `write_command` (mirrors the main drain loop).
        let on_write_failure = write_failure_event(&cmd, closes);
        let close_coid = close_command_coid(&cmd);
        if write_command(reader, ctid, cmd).is_err() {
            forget_close_on_write_failure(&close_coid, closes);
            if let (Some(event), Some(events)) = (on_write_failure, events) {
                if events.blocking_send(event).is_err() {
                    tracing::warn!(
                        target: "ctrader",
                        "core ingest gone; dropping synthesized reject on replay"
                    );
                }
            }
        }
    }
}

/// The actor loop: read a frame (dispatch), drain outbound commands, heartbeat every 10s. On a
/// socket error/EOF or a heartbeat/command write failure, backs off and reconnects
/// ([`reconnect_with_backoff`]) instead of exiting — replaying auth + symbol discovery and every
/// active subscription — and only truly stops when `Shutdown` is requested or the reconnect loop
/// gives up (channel gone). `symbols`/`sink` are held for the life of the actor so every inbound
/// `SPOT_EVENT` can be descaled and pushed without any per-frame lookup elsewhere; `symbols` is
/// itself refreshed from the venue on every reconnect (see [`open_and_handshake`]'s step 4/5) —
/// this refreshes the ACTOR's own decode-time view only; the separate `Arc<SymbolMap>` handed to
/// `ActorHandle` (shared with `CtraderExec`/`CtraderData` at construction) is not swapped in place,
/// since cTrader's symbol universe for a live account is static reference data in practice — a
/// documented, deliberate scope limit (see the Task 6 report), not an oversight. `money_digits`
/// (the account's commission/balance integer scaling exponent) is refreshed the same way on every
/// reconnect, for the same reason `symbols` is: `open_and_handshake` re-fetches it every time.
#[allow(clippy::too_many_arguments)]
fn actor_loop(
    mut reader: Reader,
    rx: Receiver<Command>,
    mut cfg: ConnConfig,
    ctid: i64,
    mut symbols: Arc<SymbolMap>,
    mut money_digits: u32,
    sink: Arc<dyn LiveDataSink>,
    events: Option<EventSender>,
    orders: OrderIdMap,
    positions: PositionMap,
    closes: CloseState,
) {
    let mut last_heartbeat = Instant::now();
    let mut spot_subs: HashSet<i64> = HashSet::new();
    let mut trendbar_subs: HashSet<(i64, ProtoOaTrendbarPeriod)> = HashSet::new();

    // Local macro (not a closure) so `break $label` reaches the loop below from every call site —
    // including the one nested inside the command-drain loop — without fighting the borrow
    // checker over the half-dozen locals a reconnect touches (`reader`/`symbols`/subs/heartbeat).
    // The label is passed in explicitly (a `lifetime` macro fragment) because macro hygiene keeps
    // a bare `'outer` written inside the macro body from resolving to the loop label at each call
    // site otherwise.
    macro_rules! try_reconnect_or_break {
        ($label:lifetime) => {{
            // The socket is gone, so the position book stops being EVIDENCE right here — anything
            // could open or close while we are not listening. Its ENTRIES stay (routing is
            // unchanged, or a reconnect would become a hedge-opening window); only its
            // `fetched_at_ms` clears, and `reconcile_after_reconnect` restores it if — and only
            // if — the venue actually answers. See `crates/bridges/ctrader/src/positions.rs`'s
            // `PositionBook`.
            if let Ok(mut book) = positions.lock() {
                book.invalidate();
            }
            let mut pending = Vec::new();
            match reconnect_with_backoff(&cfg, ctid, &rx, &mut pending) {
                Some((new_reader, new_symbols, new_money_digits)) => {
                    reader = new_reader;
                    symbols = Arc::new(new_symbols);
                    money_digits = new_money_digits;
                    replay_after_reconnect(
                        &mut reader,
                        ctid,
                        symbols.as_ref(),
                        money_digits,
                        sink.as_ref(),
                        &spot_subs,
                        &trendbar_subs,
                        pending,
                        events.as_ref(),
                        &orders,
                        &positions,
                        &closes,
                    );
                    last_heartbeat = Instant::now();
                }
                None => break $label,
            }
        }};
    }

    // Cache the EOF flag so a closed socket triggers reconnect instead of busy-spinning on
    // `Ok(None)`. `get_mut()` reaches the `EofStream`; we read its flag directly below.
    'outer: loop {
        match reader.next_frame() {
            Ok(Some(msg)) => {
                if msg.payload_type == pt::ACCOUNTS_TOKEN_INVALIDATED_EVENT || is_auth_error(&msg) {
                    if !try_refresh_and_reauth(&mut cfg, &mut reader, ctid) {
                        break 'outer;
                    }
                } else {
                    on_inbound(
                        &msg,
                        &symbols,
                        money_digits,
                        sink.as_ref(),
                        events.as_ref(),
                        &orders,
                        &positions,
                        &closes,
                    );
                }
            }
            Ok(None) => {
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    tracing::info!(target: "ctrader", "socket closed by peer; reconnecting");
                    try_reconnect_or_break!('outer);
                }
            }
            Err(e) => {
                tracing::warn!(target: "ctrader", error = %e, "read error; reconnecting");
                try_reconnect_or_break!('outer);
            }
        }

        // Drain all queued commands.
        loop {
            match rx.try_recv() {
                Ok(cmd) => {
                    track_subscription(&cmd, &mut spot_subs, &mut trendbar_subs);
                    // Captured BEFORE `cmd` is consumed by `write_command` below (Fix 1): if the
                    // write fails, this is the only chance to read the coid out of the command
                    // that never reached the wire. `close_coid` is the paired capture for a
                    // ClosePosition's tracker-forget on failure (both are pure/inert on success).
                    let on_write_failure = write_failure_event(&cmd, &closes);
                    let close_coid = close_command_coid(&cmd);
                    match write_command(&mut reader, ctid, cmd) {
                        Ok(true) => {}
                        Ok(false) => return, // Shutdown
                        Err(e) => {
                            tracing::warn!(target: "ctrader", error = %e, "write error; reconnecting");
                            if let Some(event) = on_write_failure {
                                match events.as_ref() {
                                    Some(events) => {
                                        if events.blocking_send(event).is_err() {
                                            tracing::warn!(
                                                target: "ctrader",
                                                "core ingest gone; dropping synthesized reject"
                                            );
                                        }
                                    }
                                    // Data-only wiring (`connect_and_auth`, no exec): order
                                    // commands never originate here, so this is unreachable in
                                    // practice — no lane to drop the reject on regardless.
                                    None => tracing::warn!(
                                        target: "ctrader",
                                        "write failed for an order command but no EventSender is wired; reject dropped"
                                    ),
                                }
                            }
                            // The close leg never reached the venue — drop its tracker state so a
                            // later leg's fill isn't misrouted and the next reduce isn't blocked.
                            forget_close_on_write_failure(&close_coid, &closes);
                            try_reconnect_or_break!('outer);
                            break; // resume at the top of 'outer against the fresh connection
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Heartbeat — and, on the same tick, the PROACTIVE token refresh.
        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            // ⚠ Rotate BEFORE the grant lapses rather than after the venue refuses it. The reactive
            // path below (`ACCOUNTS_TOKEN_INVALIDATED_EVENT` / an auth `ERROR_RES`) still exists and
            // is still the backstop, but by the time it fires a request has already failed — and at
            // the INITIAL connect the same expiry refuses daemon startup outright, which is the
            // operator pain this lane exists to remove.
            //
            // The cadence is the heartbeat's (10 s), so this costs one integer comparison per tick
            // and an OAuth round trip at most once per token lifetime. It runs on the ACTOR thread,
            // never on the fold. `token_expires_at_ms` is `None` until this process has refreshed
            // once (the store carries the pair, not its issue time), so a freshly-started daemon
            // simply keeps today's reactive behaviour until it learns an expiry.
            if crate::token_store::refresh_due(cfg.token_expires_at_ms, wall_clock_ms()) {
                tracing::info!(
                    target: "ctrader",
                    "access token is inside the refresh margin; refreshing proactively"
                );
                if !try_refresh_and_reauth(&mut cfg, &mut reader, ctid) {
                    // The refresh itself failed. `try_refresh_and_reauth` has already logged why
                    // (without secrets); stopping is the same degrade the reactive path takes.
                    break 'outer;
                }
            }

            let hb = ProtoHeartbeatEvent::default();
            if let Err(e) = send(&mut reader, pt::HEARTBEAT_EVENT, &hb, "") {
                tracing::warn!(target: "ctrader", error = %e, "heartbeat write failed; reconnecting");
                try_reconnect_or_break!('outer);
            } else {
                last_heartbeat = Instant::now();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One OPEN wire position in symbol 1, for the position-book provenance tests below.
    fn wire_open(position_id: i64, volume: i64) -> crate::proto::ProtoOaPosition {
        crate::proto::ProtoOaPosition {
            position_id,
            position_status: crate::proto::ProtoOaPositionStatus::PositionStatusOpen as i32,
            trade_data: crate::proto::ProtoOaTradeData {
                symbol_id: 1,
                volume,
                trade_side: crate::proto::ProtoOaTradeSide::Buy as i32,
                open_timestamp: Some(10),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// ⚠ **A `RECONCILE_RES` for ANOTHER account is not this account's answer.** It used to be
    /// folded in wholesale — a stranger's positions replacing ours and the result marked
    /// AUTHORITATIVE, so a stranger's flat book manufactured this account's `Flat` and refused its
    /// exit under `halt_admit = "verify"`. Delete the `res.ctid_trader_account_id != ctid` guard in
    /// [`rebuild_position_book`] and this goes red, as does
    /// `crates/bridges/ctrader/tests/exec_halt.rs`'s
    /// `a_reconcile_answer_for_another_account_is_not_this_accounts_evidence` end to end.
    #[test]
    fn a_reconcile_answer_for_another_ctid_is_discarded_and_clears_the_evidence() {
        let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));

        // Our own answer lands and is authoritative.
        let mine = ProtoOaReconcileRes {
            ctid_trader_account_id: 99,
            position: vec![wire_open(7, 100_000)],
            ..Default::default()
        };
        rebuild_position_book(&mine, 99, &positions);
        {
            let book = positions.lock().expect("lock");
            assert!(book.is_fetched());
            assert_eq!(book.len(), 1);
        }

        // A stranger's (flat) answer must change NOTHING except to withdraw the evidence claim.
        let theirs = ProtoOaReconcileRes {
            ctid_trader_account_id: 12_345,
            position: vec![],
            ..Default::default()
        };
        rebuild_position_book(&theirs, 99, &positions);
        let book = positions.lock().expect("lock");
        assert_eq!(
            book.len(),
            1,
            "our routing entries must survive a foreign answer, not be wiped"
        );
        assert!(
            !book.is_fetched(),
            "we asked and what came back was not about us — whatever we believed is now of \
             unknown standing, so it may not refuse anything"
        );
    }

    /// ⚠ **The completeness discipline does not stop at the reconcile.** The evidence flag is set
    /// ONCE and the book then drifts forwards on execution events for the life of the socket, so an
    /// event carrying a position ref this build cannot CLASSIFY would otherwise remove a live row
    /// while the book went on calling itself authoritative — and that manufactured absence refuses
    /// the position's own exit. Delete the `invalidate()` in [`update_position_map`]'s hole arm and
    /// this goes red.
    #[test]
    fn an_unclassifiable_position_ref_on_an_event_withdraws_the_evidence_claim() {
        let positions: PositionMap = Arc::new(Mutex::new(PositionBook::default()));
        let res = ProtoOaReconcileRes {
            ctid_trader_account_id: 99,
            position: vec![wire_open(7, 100_000), wire_open(8, 40_000)],
            ..Default::default()
        };
        rebuild_position_book(&res, 99, &positions);
        assert!(positions.lock().expect("lock").is_fetched());

        // A CLOSED position is KNOWLEDGE: drop the entry, keep the evidence claim.
        let mut closed = wire_open(8, 0);
        closed.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusClosed as i32;
        update_position_map(
            &ProtoOaExecutionEvent { position: Some(closed), ..Default::default() },
            &positions,
        );
        {
            let book = positions.lock().expect("lock");
            assert_eq!(book.len(), 1, "the closed position is gone");
            assert!(
                book.is_fetched(),
                "the venue TOLD us it closed — that is knowledge, not a hole"
            );
        }

        // An ERROR position is a HOLE: drop the entry AND stop being evidence.
        let mut broken = wire_open(7, 100_000);
        broken.position_status = crate::proto::ProtoOaPositionStatus::PositionStatusError as i32;
        update_position_map(
            &ProtoOaExecutionEvent { position: Some(broken), ..Default::default() },
            &positions,
        );
        let book = positions.lock().expect("lock");
        assert!(
            !book.is_fetched(),
            "a position ref this build cannot classify is a hole — the book must stop refusing \
             exits on the absence it just created"
        );
    }

    #[test]
    fn conn_config_debug_redacts_secrets() {
        let cfg = ConnConfig::new(
            "demo.ctraderapi.com",
            5035,
            "my-client-id",
            "super-secret-client-secret",
            "super-secret-access-token",
        );
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("super-secret-client-secret"), "leaked client_secret: {debug}");
        assert!(!debug.contains("super-secret-access-token"), "leaked access_token: {debug}");
        assert!(debug.contains("my-client-id"), "client_id should stay visible: {debug}");
        assert!(debug.contains("<redacted>"));
        assert!(
            debug.contains("refresh_token: \"None\""),
            "unset refresh_token stays None: {debug}"
        );
    }

    #[test]
    fn conn_config_debug_redacts_refresh_token_when_set() {
        let mut cfg = ConnConfig::new(
            "demo.ctraderapi.com",
            5035,
            "my-client-id",
            "super-secret-client-secret",
            "super-secret-access-token",
        );
        cfg.refresh_token = Some("super-secret-refresh-token".to_string());
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("super-secret-refresh-token"), "leaked refresh_token: {debug}");
    }

    /// Build an `ERROR_RES`-typed `ProtoMessage` carrying `error_code`, for [`is_auth_error`]
    /// unit tests below. The full network round-trip (`oauth::refresh` against a real cTrader
    /// OAuth endpoint) is NOT exercised here — it can't run offline — and stays covered only by
    /// the live smoke test (`tests/ctrader_demo_smoke.rs`); this covers just the pure classifier.
    fn error_res_msg(error_code: &str) -> ProtoMessage {
        let err = ProtoOaErrorRes { error_code: error_code.to_string(), ..Default::default() };
        ProtoMessage {
            payload_type: pt::ERROR_RES,
            payload: Some(err.encode_to_vec()),
            client_msg_id: None,
        }
    }

    #[test]
    fn is_auth_error_true_for_auth_and_token_error_codes() {
        assert!(is_auth_error(&error_res_msg("OA_AUTH_TOKEN_EXPIRED")));
        assert!(is_auth_error(&error_res_msg("ACCESS_TOKEN_INVALID")));
        // Case-insensitive: the classifier upper-cases before matching.
        assert!(is_auth_error(&error_res_msg("access_token_invalid")));
    }

    #[test]
    fn is_auth_error_false_for_unrelated_error_code() {
        assert!(!is_auth_error(&error_res_msg("SYMBOL_NOT_FOUND")));
        assert!(!is_auth_error(&error_res_msg("ORDER_REJECTED")));
    }

    #[test]
    fn is_auth_error_false_for_non_error_res_payload_type() {
        // Only `ERROR_RES` frames are ever classified — any other payload type is never an auth
        // error regardless of what bytes happen to be in its payload.
        let mut msg = error_res_msg("OA_AUTH_TOKEN_EXPIRED");
        msg.payload_type = pt::HEARTBEAT_EVENT;
        assert!(!is_auth_error(&msg));
    }

    #[test]
    fn is_transient_retries_network_faults() {
        // A failed connect / reset / EOF-mid-handshake and a no-response timeout are the blips the
        // bounded connect-retry rides out.
        assert!(
            ConnError::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset")).is_transient()
        );
        assert!(ConnError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "closed mid-handshake"
        ))
        .is_transient());
        assert!(ConnError::Timeout(pt::APPLICATION_AUTH_RES).is_transient());
    }

    #[test]
    fn is_transient_fails_fast_on_permanent_faults() {
        // Auth rejection is the crux permanent case — a bad credential never becomes good by
        // waiting, so the retry must NOT burn attempts on it.
        assert!(!ConnError::Venue {
            error_code: "CH_CLIENT_AUTH_FAILURE".into(),
            description: "invalid client credentials".into(),
        }
        .is_transient());
        assert!(!ConnError::NoAccounts.is_transient());
        assert!(!ConnError::Decode("schema mismatch".into()).is_transient());
        assert!(!ConnError::Tls("rustls config".into()).is_transient());
    }

    #[test]
    fn connect_retry_default_is_bounded_and_short() {
        // The default policy must stay bounded (so a dead venue yields to paper) and cheap (a few
        // seconds total), or the "eventually fall back to paper" contract regresses.
        let r = ConnectRetry::default();
        assert!(r.max_attempts >= 1);
        assert!(r.max_attempts <= 6, "keep the initial connect bounded");
        assert!(r.initial_backoff <= r.max_backoff);
        assert!(r.max_backoff <= Duration::from_secs(5), "cap the per-retry wait");
    }
}
