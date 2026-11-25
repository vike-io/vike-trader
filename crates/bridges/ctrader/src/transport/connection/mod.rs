//! The multiplexed cTrader connection actor: ONE protobuf-over-TLS socket carries both market
//! data and order flow. `connect_and_auth` performs the PROVEN-LIVE handshake
//! (ApplicationAuth -> GetAccountListByAccessToken -> AccountAuth -> Trader -> SymbolCategoryList
//! -> AssetClassList -> AssetList -> SymbolsList -> SymbolById)
//! and hands back an [`ActorHandle`] whose background thread owns the stream, dispatches inbound
//! frames, drains outbound [`ConnectionCommand`]s, and sends periodic heartbeats. Ports nothing — mirrors
//! the cTrader Open API (https://help.ctrader.com/open-api/) and the sequence proven by
//! `scratchpad/ctrader_catcher.py`.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use prost::Message;
use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::transport::auth::TokenMintError;
use vike_bridge_core::venue_link::VenueLink;

use vike_data::DataClientSink;
use vike_execution::EventSender;
use vike_model::secret::Secret;
use vike_venues::venue_timings;

use crate::execution::positions::{CloseTracker, PositionsById};
use crate::instrument::symbols::{SymbolLists, SymbolMap};
use crate::transport::framing::{self, FrameReader};
use crate::transport::proto::{ProtoMessage, ProtoOaTrendbarPeriod, pt};

mod actor;
pub mod grant;
// `pub(crate)` for ONE item: `require_money_digits`, which the reconcile client's balance read scales by.
pub(crate) mod handshake;
mod history;
mod inbound;
mod outbound;
mod quote;
// `pub(crate)` for ONE item: `LIST_WINDOW_MARGIN_MS`, the margin the reconcile client's order list reads too.
pub(crate) mod reconcile;
mod session;
// `pub(crate)` for the fake server (`crate::test_support`), which wraps its side of a socket in the
// same `EofStream`.
pub(crate) mod stream;
mod symbol_state;
mod tape_hole;
mod trendbar_fill;

use handshake::{
    connect_and_auth_inner, open_and_read_deposit_currency_by, open_and_read_lists,
    open_and_read_lists_by, require_money_digits,
};
pub use history::HistoryTransportLive;
pub use quote::{
    ACCOUNT_CURRENCY_DEADLINE, RATE_ASK_DEADLINE, SPOT_WAIT, fetch_account_currency,
    fetch_account_currency_within, fetch_conversion_quote, fetch_conversion_quote_assuming,
    fetch_conversion_quote_timed, fetch_conversion_quote_within,
};
use stream::{ConnectionSocket, EofStream, FrameWriteFailure, decode_error, write_frame};

#[cfg(doc)]
use handshake::open_and_handshake_retry;
#[cfg(doc)]
use inbound::update_position_map;
#[cfg(doc)]
use reconcile::{read_reconcile_res, reconcile_after_reconnect, seed_positions_at_connect};
#[cfg(doc)]
use session::{is_auth_error, reconnect_with_backoff};

/// Venue tag used on every `DataClientSink` call and log line — matches the crate name / the
/// `credentials.rs` `{VENUE}_...` naming convention.
pub(crate) const VENUE: &str = "ctrader";

/// Shared book of the account's currently-OPEN positions — see [`crate::execution::positions::PositionsById`]
/// for why it is a struct with a `fetched_at_ms` rather than a bare map. The actor keeps it fresh
/// from every execution event's `ProtoOAExecutionEvent.position`, SEEDS it at initial connect and
/// re-seeds it from a reconnect reconcile; [`crate::execution::ExecutionClientCtrader`] reads a snapshot of it to
/// decide whether a submit REDUCES an open position (→ close-by-position-id) or OPENS one (→ new
/// order), and — under `vike_model::HaltAdmit::Verify` — whether a halted submit has anything to
/// reduce (`crates/bridges/ctrader/src/execution/mod.rs`'s `halt_evidence`). `Arc<Mutex<..>>` because both
/// threads touch it.
pub type PositionsByIdShared = Arc<Mutex<PositionsById>>;

/// Shared per-order-client-id aggregation state for in-flight `ProtoOAClosePositionReq` closes (see
/// [`crate::execution::positions::CloseTracker`]). `ExecutionClientCtrader::submit` registers a planned close here; the
/// actor folds the venue's closing execution events against it to emit one coherent
/// `OrderAccepted → OrderPartiallyFilled* → OrderFilled` lifecycle for the reduce order.
pub type CloseState = Arc<Mutex<CloseTracker>>;

/// Heartbeat cadence for the actor loop (cTrader disconnects idle sockets): the exec lane's
/// `keepalive_every` in `vike_venues::venue_timings::CTRADER`, which carries the reason.
/// `pub(crate)`: also the idle age [`crate::reconcile`] revives its own dedicated
/// (heartbeat-less) connection at, so the two idle policies are driven by ONE constant.
pub(crate) const HEARTBEAT_INTERVAL: Duration = venue_timings::CTRADER.exec().keepalive_every();

/// How long one read of a socket blocks on every path but an exec actor's loop: each handshake step, the one-shot asks,
/// the connect-time seed and a data actor's whole life ([`ConnectionConfig::read_timeout`]).
///
/// ⚠ **A TLS handshake runs inside its first write, and a read that times out there ends the attempt** (rustls hands the
/// `WouldBlock` up through `write_all`), so this is also how long the venue may take over a handshake flight. It must stay
/// a network's worth of patience: [`EXECUTION_READ_POLL`] here would fail every connect.
pub const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// **How long an exec actor's read blocks before it looks at its command queue: the most a quiet socket adds to an order,
/// a cancel, an amend or a close** (`crates/bridges/ctrader/CLAUDE.md`'s "A quiet exec socket"). One thread owns the socket
/// and the queue, and nothing wakes a blocked read but a frame.
pub const EXECUTION_READ_POLL: Duration = Duration::from_millis(10);

/// How often the actor reads its own trader record, one small account-scoped request, to find a session the venue
/// ended without a word. MEASURED 2026-11-04 to 11-06 on the CI box: an access token lapsed with nothing logged, the
/// socket stayed up, and the account session was found dead only by a far order two days later
/// (`crates/bridges/ctrader/CLAUDE.md`'s "A lapsed account session"). A session that ended stops delivering fills
/// too, so an idle account must not wait for its next order. A minute is the reconcile's cadence and 1/3000 of the
/// vendor's 50 requests a second. MEASURED 2026-11-06 on the demo: an ended session refuses this request, so its
/// answer also proves the session is authorized.
///
/// The exec lane's `auth_probe_every` in `vike_venues::venue_timings::CTRADER`, which carries the reason.
pub const AUTH_PROBE_EVERY: Duration = venue_timings::CTRADER.exec().auth_probe_every();

/// **How long the venue may answer nothing on a socket before the actor calls it dead and reconnects**: three of the venue's
/// heartbeats. A blackholed socket (the peer or a path between gone, no FIN) writes into the kernel's buffer and reads
/// nothing, and it used to read RUNNING until the kernel gave up, with orders written into it unanswered. The actor asks a
/// question ([`ProtoOaTraderReq`](crate::transport::proto::ProtoOaTraderReq)) when the venue has answered nothing for two thirds of this,
/// so a live socket has something to say: refused or answered, it is an answer.
///
/// ⚠ **Only an answer counts, never a frame the venue sends unasked** (`transport::connection::actor`'s `answers_the_socket`). The proto calls the
/// proxy's `ProtoHeartbeatEvent` the criterion of a healthy connection, and it was counted until 2026-11-09, when a cut of the
/// outgoing packets alone on the CI box left the exec socket with a close written in it RUNNING for the whole 45 s: frames kept
/// coming in while nothing of ours went out, and the reconnect that would end that close by name never came.
///
/// The exec lane's `idle_threshold` in `vike_venues::venue_timings::CTRADER`, which carries the reason.
pub const READ_IDLE_AFTER: Duration = venue_timings::CTRADER.exec().idle_threshold();

// The row's threshold is three of its own heartbeats, the rule the doc above states.
const _: () = assert!(READ_IDLE_AFTER.as_secs() == 3 * HEARTBEAT_INTERVAL.as_secs());

/// Bounded exponential-backoff policy for the INITIAL connect+auth (see [`open_and_handshake_retry`]),
/// so a transient network/TLS blip during startup is ridden out within the start (one that outlasts it is
/// [`ConnectionError::kind`]'s `Network`, and the tradehub starts the account again).
/// INERT on the happy path: a first-attempt success neither sleeps nor re-sends a frame, so a
/// healthy startup is byte-identical to a single [`open_and_handshake`] call — the retry only
/// engages AFTER a transient failure ([`ConnectionError::is_transient`]). A PERMANENT auth rejection fails
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
    /// 250ms + 500ms + 1s ≈ 1.75s in total before the start fails.
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
/// [`ConnectionConfig::for_test`] against the in-process plaintext fake server. The three credential
/// values are `vike_model::secret::Secret`s, so no `Debug` of them (this struct's, a field's, a
/// panic message's) prints a value; `Debug` is still written by hand because
/// [`ConnectionConfig::refresher`] is a closure with none.
#[derive(Clone)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub access_token: Secret<String>,
    /// The OAuth2 refresh token, when known — used by the actor loop to call `transport::oauth::refresh` when a
    /// new session's handshake is refused the access token, or the token nears its expiry. `None` means the actor
    /// cannot self-heal a token invalidation: it goes on trying new sessions and refuses orders by name.
    pub refresh_token: Option<Secret<String>>,
    /// The OAuth2 base `transport::oauth::refresh` spends [`ConnectionConfig::refresh_token`] at: the account's
    /// `ctrader <type> login rest` row, set by `CtraderConfig::to_connection_config`. Empty from
    /// [`ConnectionConfig::new`]/[`ConnectionConfig::for_test`], where nothing refreshes against the venue.
    pub oauth_base: String,
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
    /// See [`crate::transport::token_store`]: cTrader ROTATES the refresh token on every refresh, so a
    /// process that does not persist leaves the store carrying a pair that has already been spent.
    pub token_persist: Option<crate::transport::token_store::TokenPersist>,
    /// Wall-clock ms at which [`ConnectionConfig::access_token`] lapses, when known. Drives the PROACTIVE
    /// refresh on the actor's heartbeat tick; `None` (a process that has not refreshed yet, so it
    /// has never seen an `expiresIn`) leaves only the reactive path, unchanged.
    pub token_expires_at_ms: Option<i64>,
    /// **The cell this process shares the grant through**, when the config came from a
    /// [`CtraderConfig`](crate::config::CtraderConfig) (`to_connection_config`): every socket of one
    /// grant holds the SAME cell, so a rotation by one is seen — and not re-spent — by the others
    /// ([`grant::SharedGrant`] argues it). `None` (`ConnectionConfig::new`/`for_test`, every test and
    /// tool) keeps the historical behaviour exactly: this config refreshes alone from its own copy.
    pub grant: Option<Arc<grant::SharedGrant>>,
    /// The cell generation [`ConnectionConfig::access_token`] / [`ConnectionConfig::refresh_token`] are from.
    /// Meaningless without [`ConnectionConfig::grant`].
    pub grant_seen: u64,
    /// **The account's ONE path cell** (`vike_bridge_core::venue_link::venue_link` of its route key, claimed once by the
    /// exec start, `crate::adapter::live_runtime_for_account`), which the gatewayhub's status verb reads: the EXEC actor
    /// states its session there, one named sub-path per trading account (`session::SessionLapse`). `None` on every other
    /// socket (data, reconcile, listing, a test or a tool): nothing is stated.
    pub venue_link: Option<VenueLink>,
    /// How long one read blocks where [`EXECUTION_READ_POLL`] does not rule ([`READ_TIMEOUT`]; [`ConnectionConfig::for_test`] narrows
    /// it, because a test on loopback has no slow handshake flight to wait for and a whole second per teardown to save).
    pub read_timeout: Duration,
    /// The actor's heartbeat tick (`HEARTBEAT_INTERVAL`; a test narrows it).
    pub heartbeat_every: Duration,
    /// How often the actor asks the venue something only an authorized session may ask
    /// ([`AUTH_PROBE_EVERY`]; a test narrows it).
    pub auth_probe_every: Duration,
    /// How long the venue may answer nothing on the actor's socket before it is called dead ([`READ_IDLE_AFTER`]; a test
    /// narrows it).
    pub read_idle_after: Duration,
    /// How long an order waits for a session during a reconnect (`session::HOLD_ORDERS_FOR`; a test narrows it).
    pub hold_orders_for: Duration,
    /// How often a venue's word may open a new session on the actor (`session::RENEW_EVERY`; a test narrows it).
    pub new_session_every: Duration,
    /// The OAuth refresh call. `None` is the token endpoint ([`crate::transport::oauth::refresh`]): always in
    /// production, because a test scripting it is the only reason it is a field (CI never dials the venue).
    pub refresher: Option<Refresher>,
    /// **Whether this config's handshakes are held while the FX market is closed** ([`ConnectionConfig::fx_clock`],
    /// owner ruling 2026-11-13). `true` from [`ConnectionConfig::new`]/[`ConnectionConfig::for_test`]. The gatewayhub's
    /// DATA feed clears it for its FIRST handshake only: that open is synchronous, and a refused one refuses the
    /// tradehub's whole start (`crates/vike-gatewayhub/src/data/credentialed.rs`'s module doc). Its reconnects are held
    /// by `session::reconnect_with_backoff` whatever this says.
    pub fx_week_holds_login: bool,
    /// **The FX login gate's clock** (owner ruling 2026-11-13): while `vike_venues::session::fx_login_closed_until` says
    /// the FX market is closed at its time, no handshake of this config is dialled and a dropped session is not
    /// reopened before the open. `None` ([`ConnectionConfig::new`]/[`ConnectionConfig::for_test`], every tool, smoke and
    /// test that does not care) is no gate. Set by `CtraderConfig::to_connection_config` from the config the
    /// gatewayhub handed its clock.
    pub fx_clock: Option<Arc<dyn vike_model::Clock + Send + Sync>>,
}

/// Spends a refresh token: [`ConnectionConfig::refresher`].
pub type Refresher =
    Arc<dyn Fn(&str) -> Result<crate::transport::oauth::Token, TokenMintError> + Send + Sync>;

impl std::fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret)
            .field("access_token", &self.access_token)
            .field("refresh_token", &self.refresh_token)
            .field("oauth_base", &self.oauth_base)
            .field("account_id", &self.account_id)
            .field("no_tls", &self.no_tls)
            .field("connect_retry", &self.connect_retry)
            // A PATH and an account row id — no secret. See `transport::token_store::TokenPersist`.
            .field("token_persist", &self.token_persist)
            .field("token_expires_at_ms", &self.token_expires_at_ms)
            // The generation and whether a home is armed, never a token (`SharedGrant`'s Debug).
            .field("grant", &self.grant)
            .field("grant_seen", &self.grant_seen)
            .field("venue_link", &self.venue_link.is_some())
            .field("read_timeout", &self.read_timeout)
            .field("heartbeat_every", &self.heartbeat_every)
            .field("auth_probe_every", &self.auth_probe_every)
            .field("read_idle_after", &self.read_idle_after)
            .field("hold_orders_for", &self.hold_orders_for)
            .field("new_session_every", &self.new_session_every)
            .field("refresher", &self.refresher.is_some())
            .field("fx_week_holds_login", &self.fx_week_holds_login)
            .field("fx_clock", &self.fx_clock.is_some())
            .finish()
    }
}

impl ConnectionConfig {
    /// Production config for a real cTrader endpoint (the account's trade socket row, TLS on).
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
            client_secret: Secret::new(client_secret.into()),
            access_token: Secret::new(access_token.into()),
            refresh_token: None,
            oauth_base: String::new(),
            account_id: None,
            no_tls: false,
            connect_retry: ConnectRetry::default(),
            token_persist: None,
            token_expires_at_ms: None,
            grant: None,
            grant_seen: 0,
            venue_link: None,
            read_timeout: READ_TIMEOUT,
            heartbeat_every: HEARTBEAT_INTERVAL,
            auth_probe_every: AUTH_PROBE_EVERY,
            read_idle_after: READ_IDLE_AFTER,
            hold_orders_for: session::HOLD_ORDERS_FOR,
            new_session_every: session::RENEW_EVERY,
            refresher: None,
            fx_week_holds_login: true,
            fx_clock: None,
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
            client_secret: Secret::new(client_secret.into()),
            access_token: Secret::new(access_token.into()),
            refresh_token: None,
            oauth_base: String::new(),
            account_id: None,
            no_tls: true,
            connect_retry: ConnectRetry::default(),
            token_persist: None,
            token_expires_at_ms: None,
            grant: None,
            grant_seen: 0,
            venue_link: None,
            read_timeout: EXECUTION_READ_POLL,
            heartbeat_every: HEARTBEAT_INTERVAL,
            auth_probe_every: AUTH_PROBE_EVERY,
            read_idle_after: READ_IDLE_AFTER,
            hold_orders_for: session::HOLD_ORDERS_FOR,
            new_session_every: session::RENEW_EVERY,
            refresher: None,
            fx_week_holds_login: true,
            fx_clock: None,
        }
    }

    /// The words a handshake of this config is refused in while the FX market is closed on [`ConnectionConfig::fx_clock`]
    /// (`FX market closed: login at ...`); `None` when it may log in, which it always may with no clock.
    pub(crate) fn fx_closed_words(&self) -> Option<String> {
        let now_ms = self.fx_clock.as_ref()?.now_ms();
        vike_venues::session::fx_login_closed_until(VENUE, now_ms)
            .map(vike_venues::session::fx_login_closed_words)
    }

    /// The words an attempt `pause` from now is held in, because it would land while the FX market is closed on
    /// [`ConnectionConfig::fx_clock`]: `vike_venues::session::fx_login_held_pause` holds it to the open, which the words
    /// name. `None` when it may be made then, which it always may with no clock.
    pub(crate) fn fx_held_words(&self, pause: Duration) -> Option<String> {
        let now_ms = self.fx_clock.as_ref()?.now_ms();
        let held = vike_venues::session::fx_login_held_pause(VENUE, pause, now_ms);
        let open_ms = now_ms.saturating_add(i64::try_from(held.as_millis()).unwrap_or(i64::MAX));
        (held > pause).then(|| vike_venues::session::fx_login_closed_words(open_ms))
    }

    /// **Take the process's current grant when a sibling socket rotated it since this config last
    /// looked** — the pair, its expiry and the generation, all from the one cell. `true` when the
    /// config moved. A config with no [`ConnectionConfig::grant`] is never touched.
    ///
    /// Called before every handshake a socket makes (the initial connect, each reconnect attempt,
    /// each reconcile-client revive), so a socket built from a stale snapshot, or reconnecting after
    /// a sibling's rotation, authenticates with the pair the venue still honours instead of one it
    /// has already ended. Never waits for a rotation in flight ([`grant::SharedGrant::snapshot`]):
    /// mid-rotation it takes the pair about to be superseded, and the refusal that follows goes
    /// through the renewal.
    pub fn sync_grant(&mut self) -> bool {
        let Some(cell) = self.grant.as_ref() else { return false };
        let current = cell.snapshot();
        if current.generation <= self.grant_seen {
            return false;
        }
        self.adopt_grant(current);
        true
    }

    /// Replace this config's pair, expiry and generation with `grant`'s.
    pub(crate) fn adopt_grant(&mut self, grant: grant::Grant) {
        self.access_token = Secret::new(grant.access_token);
        self.refresh_token = Some(Secret::new(grant.refresh_token));
        self.token_expires_at_ms = grant.expires_at_ms;
        self.grant_seen = grant.generation;
    }

    /// Has a handshake of this process been refused for the very pair this config holds
    /// ([`grant::SharedGrant::note_token_refused`])? Read on the actor's heartbeat tick.
    fn pair_was_refused(&self) -> bool {
        self.grant.as_ref().is_some_and(|cell| cell.token_refused_at(self.grant_seen))
    }
}

/// Outbound commands the actor loop encodes and writes onto the socket.
pub enum ConnectionCommand {
    /// Subscribe to spot (bid/ask tick) events for a symbol id.
    SubscribeSpots { symbol_id: i64 },
    /// Unsubscribe from spot events for a symbol id.
    UnsubscribeSpots { symbol_id: i64 },
    /// Subscribe to live trend bars for a symbol id at a given period. cTrader requires an
    /// active spot subscription on the same symbol for live trendbar pushes to arrive
    /// (`ProtoOASubscribeLiveTrendbarReq` doc) — `feed::DataClientBusSpecHoldCtrader::ask` sends
    /// `SubscribeSpots` first.
    SubscribeTrendbar { symbol_id: i64, period: ProtoOaTrendbarPeriod },
    /// Unsubscribe from live trend bars for a symbol id at a given period.
    UnsubscribeTrendbar { symbol_id: i64, period: ProtoOaTrendbarPeriod },
    /// Fetch historical trend bars (`ProtoOAGetTrendbarsReq`) for `symbol_id`/`period`, bounded to
    /// `[from_timestamp, to_timestamp]` (epoch ms) and capped at `count` bars — the one-shot historical seed
    /// `feed::DataClientBusSpecHoldCtrader::ask` fires before `SubscribeTrendbar`'s live stream starts
    /// (F2), so a fresh subscription seeds with recent closed bars instead of starting blank.
    /// Fire-and-forget like every other `ConnectionCommand`: the response (`GET_TRENDBARS_RES`) is routed by
    /// `on_inbound` straight to `DataClientSink::seed_bars`, never blocking the caller thread.
    GetTrendbars {
        symbol_id: i64,
        period: ProtoOaTrendbarPeriod,
        from_timestamp: i64,
        to_timestamp: i64,
        count: u32,
    },
    /// Place a new order (fully-formed by the exec client, Task 5).
    NewOrder(crate::transport::proto::ProtoOaNewOrderReq),
    /// Cancel a pending order by id. `order_client_id` is carried alongside the venue `order_id`
    /// (Task 6) purely so a write failure can synthesize the order-client-id-keyed `OrderCancelRejected` the
    /// venue-adapter contract requires — it plays no role in the wire request itself.
    CancelOrder { order_id: i64, order_client_id: String },
    /// Amend a pending order (fully-formed by the exec client, Task 5). `order_client_id` is
    /// carried alongside the request for the same write-failure-synthesis reason as
    /// [`ConnectionCommand::CancelOrder`].
    AmendOrder { order_client_id: String, req: crate::transport::proto::ProtoOaAmendOrderReq },
    /// Close (or partially close) an OPEN position by its numeric `position_id` — the reduce/flatten
    /// verb (`ProtoOAClosePositionReq`). `volume` is the centi-unit amount to close (≤ the position's
    /// own volume); `order_client_id` is the order client id of the reducing submit these close legs correlate
    /// to (both for the write-failure reject the venue-adapter contract requires AND for the
    /// `on_error_res` envelope echo).
    ClosePosition { position_id: i64, volume: i64, order_client_id: String },
    /// **Read the account's OPEN positions from the venue NOW**, on the actor's own socket
    /// (`reconcile::read_positions_now`: one `ProtoOAReconcileReq`, the answer folded into the shared book), and
    /// tell `reply` whether an answer for this account arrived (`true`) or not (`false`: a write failure, a
    /// venue refusal, a timeout, a dead socket). It is not a frame to write: the actor answers it itself,
    /// between two reads. A `reduce_only` MARKET order on an account that can hold a hedge waits on it
    /// (`crates/bridges/ctrader/src/execution/mod.rs`'s `submit`), because the book it routes by is empty for a start whose
    /// seed failed and a plain market order sent against an empty book OPENS a hedge. A command that is dropped
    /// unanswered (a reconnect in flight, the actor gone) drops `reply` with it, which the asker reads as `false`.
    ReadPositions { reply: mpsc::Sender<bool> },
    /// Ask the actor thread to send Shutdown and exit its loop.
    Shutdown,
}

/// Handle returned once authenticated: the command channel, the shared symbol map, the resolved
/// ctid, and the join handle for the background actor thread (joined on drop after Shutdown).
pub struct ActorHandle {
    pub tx: mpsc::Sender<ConnectionCommand>,
    pub symbols: Arc<SymbolMap>,
    pub ctid: i64,
    /// Shared `order_client_id ↔ venue order_id` correlation map (the shared [`OrderVenueIdRegistry`], built by its
    /// `Default`, `new`). cTrader's cancel/amend requests key on the numeric `orderId`, which we only learn from
    /// execution events — the actor thread writes this map as those events arrive; [`crate::execution::ExecutionClientCtrader`]
    /// reads it to resolve an order client id to an order id for cancel/modify, and an `ORDER_ERROR_EVENT` (which names only the
    /// `orderId`) is turned back into its order client id through the reverse direction. `Arc<Mutex<..>>` because both threads
    /// touch it. Empty when exec is not wired.
    pub orders: Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    /// positionId→open-position map the actor keeps fresh from execution events; the exec client
    /// reads it to route a reducing submit to close-by-position-id. Empty when exec is not wired.
    pub positions: PositionsByIdShared,
    /// Per-order-client-id in-flight close aggregation state (shared with the exec client, which registers a
    /// planned close before enqueuing its `ClosePosition` legs).
    pub closes: CloseState,
    /// Raised by [`Self::stop`] before its `Shutdown`: a thread whose loop panicked (`actor::answer_dead_actor`) then ends
    /// at its next idle moment even when the panic ate the `Shutdown`, and gives no link word a next start may own.
    stopping: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl ActorHandle {
    /// Best-effort graceful stop: send Shutdown and join the actor thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.tx.send(ConnectionCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// Clone the NON-owning, shareable slice of this connection — the command channel plus the
    /// handshake state ([`ConnectionShared`]) — for handing to [`crate::feed::DataClientBusSpecCtrader::view`]/
    /// [`crate::execution::ExecutionClientCtrader::from_shared`]. Deliberately does NOT clone the join handle: the
    /// actor-thread lifecycle stays owned in exactly one place (this `ActorHandle`, or the
    /// [`crate::transport::CtraderClient`] that wraps one), so a client view built from the returned
    /// `ConnectionShared` can be dropped without joining/killing the shared socket.
    pub fn shared(&self) -> ConnectionShared {
        ConnectionShared {
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
/// command channel (`Sender<ConnectionCommand>` is `Clone`), the shared symbol map, the resolved ctid, and
/// the shared order client id→orderId correlation map. Handed to [`crate::feed::DataClientBusSpecCtrader::view`] and
/// [`crate::execution::ExecutionClientCtrader::from_shared`] so a `DataClient` view AND an `ExecutionClient` view
/// can drive ONE actor/socket at once (F4 single-socket data+exec start).
///
/// It deliberately omits the actor thread's [`JoinHandle`]: the thread lifecycle is owned in exactly
/// ONE place — an [`ActorHandle`] for the single-client entry points, or a
/// [`crate::transport::CtraderClient`] for the combined start — so dropping a view built from a
/// `ConnectionShared` never joins/closes the socket. Only dropping (or explicitly shutting down) that sole
/// owner tears the connection down.
#[derive(Clone)]
pub struct ConnectionShared {
    pub tx: mpsc::Sender<ConnectionCommand>,
    pub symbols: Arc<SymbolMap>,
    pub ctid: i64,
    /// [`ActorHandle::orders`].
    pub orders: Arc<Mutex<OrderVenueIdRegistry<i64>>>,
    pub positions: PositionsByIdShared,
    pub closes: CloseState,
}

/// Connection/handshake failure. Never carries a secret.
#[derive(Debug)]
pub enum ConnectionError {
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
    /// The venue refused the access token, and the OAuth refresh that renews it never got the token endpoint's
    /// answer or found it failing (`session::RenewError::Unreachable`): nothing says the grant is dead. The kind is
    /// the refresh's failure (`ErrorKind::Network`, `Timeout`, `ServerError`, `RateLimited`).
    TokenRefreshUnreachable(ErrorKind),
    /// Nothing was dialled: the FX market is closed, and no session logs in until it opens
    /// ([`ConnectionConfig::fx_clock`]). Carries the words that name the open.
    FxMarketClosed(String),
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::Io(e) => write!(f, "io: {e}"),
            ConnectionError::Tls(m) => write!(f, "tls: {m}"),
            ConnectionError::Decode(m) => write!(f, "decode: {m}"),
            ConnectionError::Venue { error_code, description } => {
                write!(f, "cTrader error {error_code}: {description}")
            }
            ConnectionError::Timeout(pt) => write!(f, "timed out waiting for payloadType {pt}"),
            ConnectionError::NoAccounts => write!(f, "no trading accounts for access token"),
            ConnectionError::TokenRefreshUnreachable(_) => write!(
                f,
                "the access token was refused and its OAuth refresh could not reach the token endpoint"
            ),
            ConnectionError::FxMarketClosed(words) => write!(f, "{words}"),
        }
    }
}

impl ConnectionError {
    /// Is this a refusal a fresh OAuth grant would fix (`crate::transport::error_codes::refuses_the_grant`, by exact
    /// name, as [`is_auth_error`]'s frame-level twin reads it)?
    #[must_use]
    pub fn is_auth_failure(&self) -> bool {
        matches!(self, ConnectionError::Venue { error_code, .. }
            if crate::transport::error_codes::refuses_the_grant(error_code))
    }

    /// Did the venue refuse the ACCESS TOKEN itself (`CH_ACCESS_TOKEN_INVALID`, `OA_AUTH_TOKEN_EXPIRED`), the one
    /// refusal spending the refresh token cures? [`ConnectionError::is_auth_failure`] also reads a refused
    /// APPLICATION (`CH_CLIENT_AUTH_FAILURE`), which no refresh fixes.
    #[must_use]
    pub fn is_token_refusal(&self) -> bool {
        matches!(self, ConnectionError::Venue { error_code, .. }
            if crate::transport::error_codes::refuses_the_token(error_code))
    }
}

impl std::error::Error for ConnectionError {}

impl ConnectionError {
    /// Whether this failure is worth retrying during the INITIAL connect ([`open_and_handshake_retry`]).
    ///
    /// TRANSIENT (retry) — a fresh attempt can clear it:
    /// - [`ConnectionError::Io`]: a failed TCP/TLS connect, a read timeout surfacing as an error, a
    ///   connection reset, or a peer that closed the socket mid-handshake (the deferred rustls
    ///   handshake also fails here, NOT as `Tls`). A network blip.
    /// - [`ConnectionError::Timeout`]: the venue accepted the socket but sent no expected response before
    ///   the deadline — a stalled peer a retry can get past.
    ///
    /// PERMANENT (fail fast) — the same inputs will fail the same way, so retrying only delays the
    /// failed start:
    /// - [`ConnectionError::Venue`]: the app/account auth was REJECTED (bad credentials, invalid/expired
    ///   token, unauthorized account). The crux permanent case — a bad secret never becomes good by
    ///   waiting.
    /// - [`ConnectionError::NoAccounts`]: the access token owns no non-live account; retrying cannot
    ///   conjure one.
    /// - [`ConnectionError::Decode`]: a protocol/schema mismatch — deterministic, not a blip.
    /// - [`ConnectionError::Tls`]: a rustls CONFIG error (provider/root-store/server-name setup), not a
    ///   handshake failure (those are `Io`); a deterministic misconfiguration.
    /// - [`ConnectionError::FxMarketClosed`]: nothing was dialled, and nothing will be before the FX open.
    fn is_transient(&self) -> bool {
        matches!(self, ConnectionError::Io(_) | ConnectionError::Timeout(_))
    }

    /// **What kind of failure this is**, in the one classification of a venue failure
    /// (`vike_bridge_core::transport::ErrorKind`): the adapter's start folds it into its outcome
    /// (`vike_bridge_core::venue_adapter::AdapterOutcome::failed`), so a kind a later start can pass is asked again
    /// and every other parks until the operator acts.
    /// - [`ConnectionError::Io`] → [`ErrorKind::Network`]: the session never got the venue's answer (DNS, a refused
    ///   or reset connect, the TLS handshake, the venue's socket closing before it answered).
    /// - [`ConnectionError::Timeout`] → [`ErrorKind::Timeout`]: the socket was accepted and the answer never came.
    /// - [`ConnectionError::Venue`]: the venue answered, and its code's kind is the venue's table's
    ///   (`crate::transport::error_codes::kind_of`): a refused token, application or account → [`ErrorKind::Auth`];
    ///   the venue's own "not now" codes → [`ErrorKind::ServerError`] (`CH_SERVER_NOT_REACHABLE`, `CANT_ROUTE_REQUEST`),
    ///   [`ErrorKind::VenueMaintenance`] (`SERVER_IS_UNDER_MAINTENANCE`) or [`ErrorKind::RateLimited`]
    ///   (`REQUEST_FREQUENCY_EXCEEDED`, `BLOCKED_PAYLOAD_TYPE`, `CONNECTIONS_LIMIT_EXCEEDED`); a custom code →
    ///   [`ErrorKind::UnmappedCode`], never final.
    /// - [`ConnectionError::NoAccounts`] → [`ErrorKind::Auth`]: the grant authorises no account; only a new grant helps.
    /// - [`ConnectionError::TokenRefreshUnreachable`] → the refresh's own kind ([`ErrorKind::Network`] never reached,
    ///   [`ErrorKind::Timeout`] its answer never came, [`ErrorKind::ServerError`] a 5xx, [`ErrorKind::RateLimited`] a
    ///   429): the refresh never got the OAuth
    ///   endpoint's answer (or found it failing), so the grant is not known to be dead. A refresh the endpoint REFUSED
    ///   leaves the venue's own refusal of the token ([`ConnectionError::Venue`], `Auth`).
    /// - [`ConnectionError::FxMarketClosed`] → [`ErrorKind::VenueMaintenance`]: nothing was dialled, and the first
    ///   start after the FX open dials.
    /// - [`ConnectionError::Decode`], [`ConnectionError::Tls`] → [`ErrorKind::Unknown`]: deterministic, so they park.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        match self {
            ConnectionError::Io(_) => ErrorKind::Network,
            ConnectionError::Timeout(_) => ErrorKind::Timeout,
            ConnectionError::Venue { error_code, description } => {
                crate::transport::error_codes::kind_of(error_code, description)
            }
            ConnectionError::NoAccounts => ErrorKind::Auth,
            ConnectionError::TokenRefreshUnreachable(kind) => *kind,
            ConnectionError::FxMarketClosed(_) => ErrorKind::VenueMaintenance,
            ConnectionError::Decode(_) | ConnectionError::Tls(_) => ErrorKind::Unknown,
        }
    }
}

impl From<io::Error> for ConnectionError {
    fn from(e: io::Error) -> Self {
        ConnectionError::Io(e)
    }
}

pub(crate) type Reader = FrameReader<EofStream<ConnectionSocket>>;

/// Encode `msg` as payload of a `pt`-typed frame and write it to the stream. `pub(crate)`: reused
/// by [`crate::reconcile`]'s dedicated reconcile connection (ReconFactory seam, wave-2 task 6)
/// — the SAME wire encode every request on this connection already uses, not a second one.
pub(crate) fn send<M: Message>(
    reader: &mut Reader,
    payload_type: u32,
    msg: &M,
    message_id: &str,
) -> io::Result<()> {
    send_frame(reader, payload_type, msg, message_id).map_err(|failure| failure.error)
}

/// [`send`], saying on a failure what the venue may hold of the frame ([`stream::write_frame`]): what an order
/// command's write is decided on (`outbound`'s `book_write`).
fn send_frame<M: Message>(
    reader: &mut Reader,
    payload_type: u32,
    msg: &M,
    message_id: &str,
) -> Result<(), FrameWriteFailure> {
    let body = msg.encode_to_vec();
    write_frame(reader.get_mut(), &framing::encode(payload_type, &body, message_id))
}

/// Read frames until one of `want` payload type arrives (returned), an `ERROR_RES` arrives
/// (mapped to `Err`), or the deadline elapses. Other frames (heartbeats etc.) are skipped — the
/// handshake is strictly sequential. The deadline is the exec lane's `response_timeout` in
/// `vike_venues::venue_timings::CTRADER`: a wait for EACH expected response, started afresh
/// on every call, never one deadline over the whole handshake. `pub(crate)`: also the blocking
/// request/response primitive [`crate::reconcile`]'s dedicated reconcile connection uses
/// (ReconFactory seam, wave-2 task 6).
pub(crate) fn read_until(reader: &mut Reader, want: u32) -> Result<ProtoMessage, ConnectionError> {
    read_until_by(reader, want, None)
}

/// [`read_until`] that ALSO gives up at `ceiling` (when set): the end of a whole bounded ask
/// ([`fetch_conversion_quote_within`]), so that ten handshake steps at 15 s each cannot add up to
/// minutes. The step's own `response_timeout` still applies; the earlier of the two wins. The wait is
/// checked between reads, and a read blocks for [`ConnectionConfig::read_timeout`] at most, so the ceiling is
/// overshot by one read at worst.
pub(crate) fn read_until_by(
    reader: &mut Reader,
    want: u32,
    ceiling: Option<Instant>,
) -> Result<ProtoMessage, ConnectionError> {
    let own = Instant::now() + venue_timings::CTRADER.exec().response_timeout();
    let deadline = ceiling.map_or(own, |ceiling| ceiling.min(own));
    loop {
        if Instant::now() >= deadline {
            return Err(ConnectionError::Timeout(want));
        }
        match reader.next_frame()? {
            Some(msg) if msg.payload_type == want => return Ok(msg),
            Some(msg) if msg.payload_type == pt::ERROR_RES => return Err(decode_error(&msg)),
            Some(_) => continue, // unrelated frame — skip
            None => {
                // A read that returned `Ok(0)` (peer closed the socket mid-handshake) sets the
                // `EofStream` flag; without this check the loop would busy-spin on `Ok(None)` for
                // the full response timeout, burning a core. Fail fast on a closed connection;
                // a genuine read timeout leaves `eof` false and falls through to retry.
                if reader.get_mut().eof.load(Ordering::SeqCst) {
                    return Err(ConnectionError::Io(io::Error::new(
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
/// [`crate::reconcile`] (ReconFactory seam, wave-2 task 6).
pub(crate) fn decode_payload<M: Message + Default>(
    msg: &ProtoMessage,
) -> Result<M, ConnectionError> {
    M::decode(msg.payload.as_deref().unwrap_or(&[]))
        .map_err(|e| ConnectionError::Decode(e.to_string()))
}

/// Connect, run the two-stage auth handshake + symbol discovery, and spawn the actor thread.
/// `sink` is where the actor pushes live quotes/bars decoded from inbound `SPOT_EVENT` frames
/// (Task 4) — given here, at construction, rather than threaded through later calls, matching
/// `vike_data::DataClient`'s "sink at construction" contract. This data-only entry point leaves
/// order flow unwired; [`connect_and_auth_execution`] adds the analogous `EventSender` for exec events.
pub fn connect_and_auth(
    config: ConnectionConfig,
    sink: Arc<dyn DataClientSink>,
) -> Result<ActorHandle, ConnectionError> {
    connect_and_auth_inner(config, sink, None)
}

/// Exec-enabled variant of [`connect_and_auth`]: additionally wires an [`EventSender`] so the
/// actor routes inbound `EXECUTION_EVENT` frames to the core ingest lane (via
/// `feed::decode::execution_event_to_events`) and keeps the returned handle's order client id→order-id map updated. Pair
/// the returned [`ActorHandle`] with [`crate::execution::ExecutionClientCtrader::new`] (share the same
/// `EventSender` clone, so its synchronous `OrderSubmitted`/synthetic-reject emits land on the same
/// lane). Order flow is off until this is used — `connect_and_auth` leaves exec unwired.
pub fn connect_and_auth_execution(
    config: ConnectionConfig,
    sink: Arc<dyn DataClientSink>,
    events: EventSender,
) -> Result<ActorHandle, ConnectionError> {
    connect_and_auth_inner(config, sink, Some(events))
}

/// One-shot symbol enumeration: connect, run the full auth handshake + symbol discovery
/// ([`open_and_handshake`]), and return JUST the built [`SymbolMap`] — WITHOUT spawning the actor
/// thread or wiring any `DataClientSink`/`EventSender`. The `Reader` (and the socket it owns) is
/// dropped on return, so no live data/exec session is ever started; this is the clean path the
/// [`crate::catalog::CatalogProviderCtrader`] provider uses to enumerate the venue's symbols for the Symbol
/// picker (cTrader has no REST symbol-list endpoint — the protobuf session IS the list). Blocking:
/// runs the whole handshake inline on the calling thread and returns once `SymbolById` is decoded.
pub fn fetch_symbols(config: ConnectionConfig) -> Result<SymbolMap, ConnectionError> {
    Ok(SymbolMap::from_lists(&fetch_symbol_lists(config)?))
}

/// [`fetch_symbols`] before the map is built: the raw [`SymbolLists`] the handshake read (light
/// and full symbols plus the category / asset-class / asset reference lists), verbatim. The
/// read-only catalog smoke (`tests/ctrader_catalog_smoke.rs`) prints these to capture what a
/// broker really publishes; the same socket-drop guarantee holds.
pub fn fetch_symbol_lists(config: ConnectionConfig) -> Result<SymbolLists, ConnectionError> {
    // A provider that kept its boot-time config (the catalog's) presents the pair the process holds
    // NOW; a one-shot fetch never refreshes, so it only follows.
    let mut config = config;
    config.sync_grant();
    let (_reader, _ctid, lists, _money_digits) = open_and_read_lists(&config, None)?;
    // `_reader` (hence the underlying TcpStream/TLS session) drops here — the socket closes and no
    // actor thread was ever spawned, so this leaves nothing running behind it.
    Ok(lists)
}

/// [`fetch_symbol_lists`] under ONE overall `deadline` (the connect and every handshake step), for the
/// callers a gatewayhub lookup thread waits on: a listing that stalls would otherwise hold the thread for as
/// long as the venue likes, and every converting lookup lists first. The unbounded variant stays for the
/// catalog picker and the smokes, which run on their own threads.
pub fn fetch_symbol_lists_within(
    config: ConnectionConfig,
    deadline: Duration,
) -> Result<SymbolLists, ConnectionError> {
    let mut config = config;
    config.sync_grant();
    let ceiling = Instant::now() + deadline;
    let (_reader, _ctid, lists, _money_digits) =
        open_and_read_lists_by(&config, None, Some(ceiling))?;
    Ok(lists)
}

/// Connect + run the two-stage auth handshake + symbol discovery, returning a ready [`Reader`]
/// plus the resolved ctid, a freshly-built [`SymbolMap`], and the account's `moneyDigits` (the
/// exponent commission/balance integers are scaled by — see `ProtoOATrader.money_digits`'s wire
/// doc and `feed::decode::deal_fill`'s commission descale). Shared by the initial connect
/// ([`connect_and_auth_inner`]) and reconnect ([`reconnect_with_backoff`], Task 6) — cTrader
/// expects the exact same sequence after a dropped socket. `fixed_ctid`, when set, skips account
/// discovery and authorizes that ctid directly; reconnect ALWAYS passes the already-resolved ctid
/// here (never re-discovers) so a stale/reordered `GetAccountListByAccessToken` response can never
/// silently reauthorize a DIFFERENT account mid-session.
///
/// `pub(crate)`: also the entry point [`crate::reconcile`]'s `ReconcileClientCtrader::connect`
/// uses to open its OWN dedicated authed connection (ReconFactory seam, wave-2 task 6) — the
/// SAME handshake sequence, isolated from the exec/data actor's own socket (mirrors Deribit's
/// dedicated-socket `ReconcileClient`).
pub(crate) fn open_and_handshake(
    config: &ConnectionConfig,
    fixed_ctid: Option<i64>,
) -> Result<(Reader, i64, SymbolMap, u32), ConnectionError> {
    let (reader, ctid, lists, money_digits) = open_and_read_lists(config, fixed_ctid)?;
    // FAIL CLOSED: every session that comes through here scales money integers (a deal's commission and
    // conversion fee), so an absent `ProtoOATrader.moneyDigits` ends it in words, never as a scale of 0.
    let money_digits = require_money_digits(money_digits)?;
    let map = SymbolMap::from_lists(&lists);
    // The login just received the whole list: it is the kept list's, asked of nobody (`crate::catalog::hold`).
    crate::catalog::hold(&map);
    Ok((reader, ctid, map, money_digits))
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
///   order client id→orderId map), so it stays 5s.
/// - The seed runs on the START PATH. cTrader is the one venue whose exec client connects
///   SYNCHRONOUSLY at start, inside [`crate::adapter::live_runtime_for_account`] (called by
///   [`crate::adapter::VenueAdapterCtrader`], which `crates/vike-tradehub-core/src/engine.rs`'s `make_engine`
///   reaches through the venue-adapter contract), and `crates/vike-tradehub-core/src/node.rs` starts venues
///   one after another — so every millisecond here is paid by an operator watching a daemon that
///   has not started yet, and a start that looks hung is a start somebody kills.
///
/// **MEASURED, against the real `demo.ctraderapi.com:5035` (the CI box, 2026-08-08, 15 real starts by
/// `crates/bridges/ctrader/tests/ctrader_seed_smoke.rs`): the seed's own round trip took 24ms
/// median, 23ms min, 25ms max — a 2ms spread.** 1500ms is therefore ~60x the observed cost,
/// and it is a bound on a socket that has ALREADY completed the handshake's six request/response
/// round trips (app auth → \[accounts\] → account auth → trader → symbols list → symbol-by-id,
/// [`open_and_handshake`]) milliseconds earlier. A venue that answers six times and then goes quiet
/// for 1.5s is not "a bit slow"; it is anomalous, and waiting 3.5s longer for it buys nothing.
///
/// ⚠ **The stall it produces is QUANTIZED to the socket's 1s read timeout, so this constant is a
/// floor on the wait, not the wait.** [`read_reconcile_res`] tests the deadline between reads, and
/// [`open_and_handshake`] leaves the socket on [`READ_TIMEOUT`] — so a mute venue is left in
/// a blocking read that only returns on a 1s boundary, and 1500ms costs the start **2.0s**
/// (measured: 2.030s / 2.077s / 2.080s, three reps against
/// `crates/bridges/ctrader/src/test_support/fake.rs`'s `FakeCtrader::start_mute_reconcile`). Under
/// [`RECONCILE_TIMEOUT`] the same stall measured **5.120s**. Any value in `(1s, 2s]` buys the same
/// 2.0s; 1500ms sits in the middle of that band deliberately, so neither a slow clock nor an early
/// `SO_RCVTIMEO` wakeup can tip the answer into the next second. Cutting to 1.0s means a bound of
/// ~500ms — 20x the measured cost rather than 60x — and that trade was declined: a lost seed
/// restores the routing bug this whole path exists to fix, and 3.1s of the 5.1s is already gone.
///
/// Losing the seed is cheap and self-healing, which is what lets the bound be tight: the book stays
/// UNFETCHED, routing is exactly what it was before the seed existed, and it refills at the first
/// execution event carrying a position ([`update_position_map`]) or the next reconnect
/// ([`reconcile_after_reconnect`]). `pub` because it is the operator-visible ceiling on the start:
/// the timeout warn prints it, and `crates/bridges/ctrader/tests/offline/execution_close.rs`'s
/// `a_mute_venue_costs_the_start_the_seed_bound_not_the_reconnect_one` pins that a mute venue
/// really is bounded by THIS number rather than by [`RECONCILE_TIMEOUT`].
pub const SEED_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long a `submit` waits for the actor's answer to a [`ConnectionCommand::ReadPositions`]: the actor sees the
/// command between two reads ([`EXECUTION_READ_POLL`] apart), then waits for the venue for
/// [`SEED_TIMEOUT`], and 5 s covers both with room. Only a
/// `reduce_only` MARKET order on a hedge-capable account whose book is not authoritative pays it, and it
/// pays it on its own account's order thread (the gatewayhub's per-adapter dispatch), never on the fold.
pub const POSITIONS_READ_WAIT: Duration = Duration::from_secs(5);

#[path = "connection_tests.rs"]
#[cfg(test)]
mod connection_tests;
