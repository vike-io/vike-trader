//! Polymarket `ExecutionClient` — the vike-model seam. A dedicated thread owns the creds, derives L2
//! (ClobAuth L1) on start, and maps each `OrderRequest` → a signed V2 CLOB order → submit → venue
//! events. `OrderRequest.symbol` is the ERC-1155 outcome `token_id`; `qty` is shares; `price` is the
//! probability. The submit response gives Accepted/Rejected; **fills/cancels arrive on the
//! authenticated user WS** — which [`PolymarketExecutionClient::spawn_live`] starts from inside this
//! same exec thread (the bybit/okx convention: one thread owns the pump's lifetime and shuts it down
//! when the command loop ends), so a mounted engine's return lane exists by construction rather than
//! by a composition root remembering to wire one. NegRisk (the signing domain) is resolved per order
//! through [`NegRiskSource`], which — unlike a bare set — can say "don't know" and reject.
//!
//! The exec thread is also where the **dynamic tick-size regime** ([`crate::tick_regime`]) is
//! consumed, when a caller threads one onto [`PolymarketLiveConfig::tick_regime`]: each submit's
//! price is snapped onto that token's cached grid ([`grid_price`]) and each terminal reject is fed
//! back to the cache ([`observe_submit_outcome`]), so an off-grid refusal re-fetches the grid instead
//! of repeating forever. `None` ⇒ neither happens and nothing is built for it.

use std::collections::HashSet;
use std::sync::mpsc::Receiver;

use vike_bridge_core::eip712::eth_address_from_private_key;
use vike_bridge_core::exec_actor::{cancel_event, CancelOutcome, ExecActor, ExecCommand};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::{CancelIntent, EventSender, ExecutionClient};
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};
use vike_model::{OrderRequest, TimeInForce};

use super::config::{PolymarketCreds, CLOB_BASE};
use super::exec::{
    cancel_order, cancel_order_relayer, cancel_orders, gate_cancel_shared, submit_order,
    submit_order_relayer, CancelScope,
};
use super::fill_tracker::FillTracker;
use super::l1::ensure_l2;
use super::neg_risk_lookup::NegRiskSource;
use super::order::{
    build_order, derive_order_id, order_to_json, sign_order, sign_order_1271, Side, SignatureType,
};
use super::pending_events::ParkedEvent;
use super::registry::PolymarketRegistry;
use super::tick_regime::TickRegime;

/// Live Polymarket exec client. `submit`/`cancel` enqueue onto the exec thread (non-blocking).
pub struct PolymarketExecutionClient(ExecActor);

impl PolymarketExecutionClient {
    /// Spawn the exec thread. `maker` is the funder/deposit-wallet address (empty → use the EOA);
    /// `signature_type` matches the wallet: `Poly1271` for a deposit-wallet account (maker == the
    /// order signer, submit/cancel via the relayer — needs `creds.relayer_key`/`relayer_address`),
    /// or `Eoa` for a bare key.
    /// `neg_risk_tokens` = the outcome `token_id`s that trade on NegRisk markets (build it once from
    /// [`fetch_all_markets`](super::instruments::fetch_all_markets)); those orders sign against the
    /// NegRisk domain. Empty = treat all as standard.
    ///
    /// `registry` is the shared CLOB↔coid map: the exec thread records each accepted order in it so
    /// the user-WS pump can re-key fills/cancels back to the coid. Pass the SAME registry clone to
    /// [`spawn_polymarket_user_data`](super::user_data::spawn_polymarket_user_data).
    pub fn spawn(
        creds: PolymarketCreds,
        maker: String,
        signature_type: SignatureType,
        neg_risk_tokens: HashSet<String>,
        registry: PolymarketRegistry,
        events: EventSender,
    ) -> Self {
        Self::spawn_tracked(creds, maker, signature_type, neg_risk_tokens, registry, events, None)
    }

    /// [`spawn`](Self::spawn) plus the optional dust-snap [`FillTracker`]
    /// ([`crate::fill_tracker`]): each accepted order's submitted qty is registered there so the
    /// user-WS pump can snap cent-tick overfills and complete dust remainders. Pass the SAME
    /// tracker clone to
    /// [`spawn_polymarket_user_data_tracked`](super::user_data::spawn_polymarket_user_data_tracked).
    /// `None` is byte-identical to [`spawn`](Self::spawn).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_tracked(
        creds: PolymarketCreds,
        maker: String,
        signature_type: SignatureType,
        neg_risk_tokens: HashSet<String>,
        registry: PolymarketRegistry,
        events: EventSender,
        tracker: Option<FillTracker>,
    ) -> Self {
        Self::spawn_live(
            PolymarketLiveConfig {
                creds,
                maker,
                signature_type,
                neg_risk: NegRiskSource::from_set(neg_risk_tokens),
                registry,
                tracker,
                user_channel: None,
                builder_code: [0u8; 32],
                // legacy entry point (no user-WS pump): nothing reads a pre-registration here, so
                // keep it OFF — byte-identical to before this field existed.
                presubmit_register: false,
                // …and no tick-size regime: no transport built, no price rounded, no reject
                // observed. Only `live_mount_from_vars` threads one in (see `tick_regime`).
                tick_regime: None,
            },
            events,
        )
    }

    /// The LIVE-MOUNT entry point: the exec thread above, PLUS (when
    /// [`PolymarketLiveConfig::user_channel`] is `Some`) the authenticated user-WS fill pump and its
    /// audit-A3 post-reconnect resync, started from inside that same thread and shut down when it
    /// ends.
    ///
    /// Why the pump lives here and not in the composition root: the venue-adapter contract says no
    /// order may silently vanish, and on this venue every terminal AFTER acceptance (fill, cancel,
    /// expiry) arrives ONLY on the user channel. A mount that spawned the exec client without the
    /// pump would accept orders that could never reach a terminal event — so the two are made
    /// inseparable, exactly as bybit/okx do it (their `run` starts the private-WS pump before the
    /// command loop and `shutdown()`s it after). `user_channel: None` reproduces the pre-existing
    /// [`spawn`](Self::spawn)/[`spawn_tracked`](Self::spawn_tracked) behavior byte-for-byte: no
    /// socket, no extra thread.
    ///
    /// ⚠ **The ONE venue in the tree that declares [`ExecActor::with_bulk_cancel`]**, because it is
    /// the one with a native account-wide `cancel-all` AND a client-side cancel-token budget that
    /// has to choose between it and `n` targeted calls. The declaration is what makes
    /// [`ExecCommand::CancelBatch`] reachable at all, and the exec thread's arm for it is the
    /// obligation that comes with declaring one.
    pub fn spawn_live(cfg: PolymarketLiveConfig, events: EventSender) -> Self {
        Self(
            ExecActor::spawn("polymarket-exec", events.clone(), move |rx| run(cfg, events, rx))
                .with_bulk_cancel(),
        )
    }
}

/// Everything the live exec thread owns. A struct rather than a 9-argument `spawn` (clippy's
/// `too_many_arguments`, and the call site reads as a configuration rather than a positional puzzle).
pub struct PolymarketLiveConfig {
    /// L1 key + (ideally already-derived) L2 trio + relayer identity. If `secret` is empty the exec
    /// thread derives L2 itself via `ensure_l2` — a mount that already derived (to build the recon
    /// client from the same handshake) passes them in and costs no second round-trip.
    pub creds: PolymarketCreds,
    /// The funder / deposit-wallet address orders are made for; empty ⇒ the key's own EOA.
    pub maker: String,
    /// `Poly1271` for a deposit-wallet account (relayer submit/cancel), `Eoa` for a bare key.
    pub signature_type: SignatureType,
    /// How the per-order EIP-712 signing domain is decided — see [`NegRiskSource`].
    pub neg_risk: NegRiskSource,
    /// The shared CLOB-id ↔ coid map. Pass the SAME clone to the user pump (done for you when
    /// `user_channel` is `Some`) and to `recon_client` so reconcile reports re-key to local coids.
    pub registry: PolymarketRegistry,
    /// Optional dust-snap ledger ([`crate::fill_tracker`]).
    pub tracker: Option<FillTracker>,
    /// `Some` ⇒ start the authenticated user-WS pump inside the exec thread. `None` ⇒ don't (the
    /// historical shape; the caller is then responsible for the return lane).
    pub user_channel: Option<UserChannelConfig>,
    /// bytes32 `builderCode` stamped into every signed order for fee attribution (see
    /// [`crate::order::Order::builder`]). `[0u8; 32]` (the default from every non-`live_mount_from_vars`
    /// entry point) is unattributed — byte-identical to before this field existed.
    pub builder_code: [u8; 32],
    /// Close the ack-race by pre-registering `coid`↔`derive_order_id(&order)` in the shared
    /// `registry` just BEFORE each submit, so a user-WS fill/cancel that beats the HTTP ack still
    /// re-keys to the local order (done in the exec thread's submit arm; resolved from
    /// `POLY_PRESUBMIT_REGISTER` at the mount via [`crate::mount::presubmit_register_enabled`]).
    /// DEFAULT-OFF (`false`): `false` computes and registers NOTHING pre-submit — byte-identical to
    /// before this field existed, with the ack path ([`PolymarketRegistry::on_accept`] after
    /// acceptance) the sole registry writer, exactly as today.
    pub presubmit_register: bool,
    /// The SHARED dynamic tick-size cache ([`crate::tick_regime`]) the exec thread prices orders on
    /// and teaches from venue rejects. `Some` ⇒ each submit's price is snapped onto this token's
    /// cached grid before the order is built, and an off-grid reject re-fetches that grid so the NEXT
    /// order is priced on the one the venue is actually enforcing — the self-heal for the
    /// reject-forever loop the module doc describes. `Clone` shares one cache, so a quoting path that
    /// keeps its own clone reads the same learned grid.
    ///
    /// `None` (every entry point but [`crate::live_mount_from_vars`]) is byte-identical to before
    /// this field existed: no transport is constructed, no price is rounded, no reject is observed.
    /// A `Some` regime that has never resolved this token is ALSO inert — an unknown token prices
    /// verbatim (see [`TickRegime::round_price`]), so nothing moves until the venue itself teaches it.
    pub tick_regime: Option<TickRegime>,
}

/// The exec thread's tick-regime handle: the shared [`TickRegime`] cache PLUS the transport its
/// re-fetch rides. Bundled because the two are useless apart, and built ONLY when a caller threaded
/// a regime in — so `None` costs no `ureq::Agent`, no rounding and no reject observation.
///
/// The transport is deliberately the PROXIED one (`crate::egress::agent()`, the same SOCKS egress
/// every other Polymarket REST lane uses): a direct-dialling transport cannot even resolve the CLOB
/// host from a geo-blocked box, so every refresh would fail and the cached grid — correctly, per
/// [`TickRegime::refresh`] — would simply never move.
struct TickGrid<T: RestTransport> {
    regime: TickRegime,
    transport: T,
}

/// The price an order is BUILT and SIGNED with: `price` snapped onto this token's cached grid when a
/// regime is threaded in, and `price` verbatim when it is not — or when the regime has never resolved
/// this token, which [`TickRegime::round_price`] returns unchanged rather than guessing a grid.
fn grid_price<T: RestTransport>(grid: Option<&TickGrid<T>>, token_id: &str, price: f64) -> f64 {
    match grid {
        Some(g) => g.regime.round_price(token_id, price),
        None => price,
    }
}

/// Teach the tick regime from a submit's TERMINAL event. An [`Event::OrderRejected`] whose reason is
/// an off-grid refusal ([`crate::is_tick_size_reject`], the pure decision) re-fetches and caches this
/// token's tick size; the resolved tick is returned for the tests' benefit — the exec thread wants
/// only the caching side effect.
///
/// Everything else is a no-op that costs NO REST round-trip: no regime, an accepted order, a
/// balance/allowance refusal, a local signing failure, or a transport error. A refresh that resolves
/// nothing leaves the previously cached grid untouched (that contract lives in
/// [`TickRegime::refresh`], and is the reason this never routes through
/// `instruments::fetch_token_tick_size`, which always answers — with the `0.01` default).
fn observe_submit_outcome<T: RestTransport>(
    grid: Option<&TickGrid<T>>,
    token_id: &str,
    ev: &Event,
) -> Option<f64> {
    let g = grid?;
    let Event::OrderRejected(r) = ev else {
        return None;
    };
    g.regime.on_reject(&g.transport, token_id, &r.reason)
}

/// The authenticated user-channel pump's configuration (see [`PolymarketLiveConfig::user_channel`]).
pub struct UserChannelConfig {
    /// Usually [`WS_USER`](crate::config::WS_USER).
    pub ws_url: String,
    /// The CLOB **condition ids** (markets) to subscribe to — NOT token ids. See
    /// [`crate::mount::poly_exec_markets`] for where a mount gets them and what an empty list means.
    pub markets: Vec<String>,
    /// Rows of `/data/trades` + `/data/orders` the post-reconnect A3 resync replays.
    pub history_limit: u32,
}

/// Default rows the A3 resync replays per endpoint after a user-WS reconnect.
pub const DEFAULT_HISTORY_LIMIT: u32 = 100;

impl ExecutionClient for PolymarketExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        // ONE cancel door: both go through `cancel_with_intent`, so a caller that names no intent
        // is explicitly the flatten-safe `Unspecified` rather than silently skipping the override.
        self.cancel_with_intent(client_order_id, CancelIntent::Unspecified)
    }
    /// The intent must reach the exec thread, because that thread owns the cancel-token mirror and
    /// makes the reserve decision beside the debit it causes (see [`crate::exec::gate_cancel`]).
    /// Forwarding is all this wrapper does — `ExecActor` puts the intent on the queued command.
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        self.0.cancel_with_intent(client_order_id, intent)
    }
    /// Explicit, not inherited: the trait default routes to `cancel_batch`, which would fan out to
    /// the intent-LESS `cancel` and drop the classification for every batched cancel. `ExecActor`
    /// carries the whole batch, with its intent, to the exec thread as ONE
    /// [`ExecCommand::CancelBatch`] — this client declared that lane at
    /// [`spawn_live`](Self::spawn_live) — where [`crate::exec::plan_cancels`] decides between the
    /// account-wide `cancel-all` and `n` targeted calls under the cancel-token budget.
    ///
    /// ⚠ This forwarded to a per-order FAN-OUT until the bulk lane existed, which is what made
    /// `plan_cancels`/`cancel_orders`/`cancel_all_orders` unreachable: the batch was already `n`
    /// singles before any venue code ran, so nothing could ever choose the bulk arm.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        self.0.cancel_batch_with_intent(client_order_ids, intent)
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())
}

/// Decode a 0x-prefixed 32-byte hex builder code; `None` (→ zero) on any malformed value so a bad
/// `.env` entry degrades to unattributed rather than failing the mount. Shared by
/// [`crate::mount::live_mount_from_vars`] (the production resolve) and the live-money
/// `live_place_and_cancel` smoke below, so there is ONE decode path.
pub(crate) fn decode_builder_bytes32(code: String) -> Option<[u8; 32]> {
    let hex = code.strip_prefix("0x").unwrap_or(&code);
    let bytes = hex::decode(hex).ok()?;
    (bytes.len() == 32).then(|| {
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        out
    })
}

/// This venue's row of the ONE cross-venue TIF authority ([`vike_bridge_core::tif::venue_tif`]),
/// consumed: `Ioc` is COERCED to `"FOK"` — the OPPOSITE direction of hyperliquid's `Fok`→`Ioc`
/// fold (same pair!); step 2 resolves that deliberately, behind demo smokes.
/// GTD's wire `expiration` is resolved by the sibling [`expiration_secs_of`]; this function only
/// picks the `orderType` string.
fn order_type_of(tif: TimeInForce) -> &'static str {
    // `wire()` is Some for every polymarket row (Mapped/Coerced only) — the fallback is
    // unreachable, kept so the exec thread can never panic.
    vike_bridge_core::tif::venue_tif(crate::market_feed::VENUE, tif).wire().unwrap_or("GTC")
}

/// The CLOB's minimum GTD lifetime: a limit order whose `expiration` is nearer than this is
/// refused by the venue. `180`s as of `@polymarket/client` 0.1.0-beta.12 (earlier builds used 60s).
///
/// ⚠ TRANSCRIBED from the SDK constant recorded in [`crate::order`]'s module doc, **not**
/// independently confirmed against a live venue response. It is applied as a CLIENT-SIDE floor, so
/// the failure mode of a wrong value is a local refusal of an order the venue might have taken —
/// never a malformed order on the wire.
pub(crate) const GTD_MIN_LEAD_SECS: i64 = 180;

/// The wire `expiration` for one request, in **unix SECONDS** — the sibling of [`order_type_of`],
/// and the reason a GTD order can no longer ship the GTC `"0"`.
///
/// - Every NON-GTD tif → `Ok(0)`, "no expiry": the endpoint's `orderType` drives the lifetime for
///   GTC/FOK, and `Ioc`/`Day` reach the wire as `FOK`/`GTC` (see [`order_type_of`]). This arm is
///   byte-identical to every order this venue has ever sent.
/// - `Gtd` → the request's [`OrderRequest::gtd_expiry`] (epoch **ms**, so it is divided down to
///   seconds), but only once it clears `now + `[`GTD_MIN_LEAD_SECS`].
/// - `Gtd` with NO expiry, or one inside that floor, → `Err(reason)`. The caller turns it into a
///   terminal `OrderRejected` BEFORE signing or submitting, so a request the venue would bounce is
///   refused locally with a reason naming the actual constraint.
///
/// The `Gtd` arm can never yield `0`: `0` would have to clear `now_secs + 180`, which no positive
/// clock satisfies. That is the property the wire-body test pins.
pub(crate) fn expiration_secs_of(
    tif: TimeInForce,
    gtd_expiry_ms: Option<i64>,
    now_ms: i64,
) -> Result<u64, String> {
    if tif != TimeInForce::Gtd {
        return Ok(0);
    }
    let Some(deadline_ms) = gtd_expiry_ms else {
        return Err(
            "time_in_force GTD requires gtd_expiry — polymarket needs a wire expiration and \
             will not rest an order without one"
                .to_string(),
        );
    };
    // Truncate toward negative infinity: a deadline mid-second must not round UP past the floor.
    let deadline_secs = deadline_ms.div_euclid(1_000);
    let earliest = now_ms.div_euclid(1_000) + GTD_MIN_LEAD_SECS;
    if deadline_secs < earliest {
        return Err(format!(
            "gtd_expiry {deadline_secs} is inside polymarket's minimum GTD lead of \
             {GTD_MIN_LEAD_SECS}s (earliest accepted expiration: {earliest})"
        ));
    }
    u64::try_from(deadline_secs)
        .map_err(|_| format!("gtd_expiry {deadline_secs} is not a valid unix timestamp"))
}

/// Classify a CLOB submit response body. `Ok(orderID)` = accepted; `Err(reason)` = rejected.
/// A 2xx body can still carry `success:false` (e.g. balance/allowance) — never blind-accept.
fn accept_outcome(resp: &serde_json::Value) -> Result<String, String> {
    if resp.get("success").and_then(|s| s.as_bool()) == Some(false) {
        let msg = resp.get("errorMsg").and_then(|m| m.as_str()).unwrap_or("order rejected");
        return Err(msg.to_string());
    }
    match resp.get("orderID").and_then(|o| o.as_str()).filter(|s| !s.is_empty()) {
        Some(id) => Ok(id.to_string()),
        None => Err(resp
            .get("errorMsg")
            .and_then(|m| m.as_str())
            .unwrap_or("submit response missing orderID")
            .to_string()),
    }
}

/// The `/data/{trades,orders}` rows out of either wire shape the CLOB answers with — a bare array,
/// or the paged `{"data": [...]}` envelope. Same tolerance `recon_client`'s `rows` applies; anything
/// else replays as an empty array (never a panic on the resync thread).
fn history_rows(v: &serde_json::Value) -> &serde_json::Value {
    const EMPTY: &serde_json::Value = &serde_json::Value::Null;
    if v.is_array() {
        v
    } else {
        v.get("data").filter(|d| d.is_array()).unwrap_or(EMPTY)
    }
}

/// Emit the user-channel events that were staged while this order had no CLOB→coid mapping.
///
/// This is the other half of the fix in [`crate::pending_events`]: the exec thread is the ONLY
/// place that learns "this venue id is ours", so it is the only place a staged event can be
/// released promptly and exactly. Frame-driven release would be at the mercy of the next inbound
/// frame — fine while the account is busy, a silent TTL loss when it is not.
///
/// Called AFTER this order's own terminal event has been sent, never before — the caller's comment
/// at the flush site explains why the FSM makes that ordering load-bearing.
fn emit_replayed(
    parked: &[ParkedEvent],
    registry: &PolymarketRegistry,
    tracker: Option<&FillTracker>,
    events: &EventSender,
) {
    if parked.is_empty() {
        return; // the overwhelmingly common path: nothing raced, nothing staged
    }
    for ev in crate::user_ws::replay_parked(parked, registry, tracker) {
        let _ = events.blocking_send(ev);
    }
}

/// The [`CancelScope`] one batch is planned under — the safety decision of the whole bulk lane,
/// pulled out of the arm because it is the ONE thing that decides whether an ACCOUNT-WIDE call can
/// happen at all.
///
/// [`CancelScope::WholeBook`] is the caller ASSERTING that `DELETE /cancel-all` is equivalent to
/// cancelling exactly these ids one by one, so it takes BOTH halves: the batch must name every
/// order this mount believes is resting (`PolymarketRegistry::covers_all_live`), and every id it
/// named must have RESOLVED to a venue order id. An id that did not resolve means the caller asked
/// for something outside this book — a subset request wearing a whole-book shape — and the arm's
/// unknown-id rejects have already gone out for it. Either half missing ⇒ [`CancelScope::Subset`],
/// which `plan_cancels` can never take to the account-wide arm however much budget is free.
///
/// One process mounts exactly one Polymarket exec thread (`vike_run`'s `build_node` calls
/// `make_engine_with_legs` once per VENUE; extra symbols only widen the risk grid), so this
/// registry IS the whole of what this process placed. What it is not is the whole ACCOUNT — see
/// [`CancelScope::WholeBook`]'s declared residual.
fn batch_scope(covers_all_live: bool, resolved: usize, requested: usize) -> CancelScope {
    if covers_all_live && resolved == requested {
        CancelScope::WholeBook
    } else {
        CancelScope::Subset
    }
}

fn run(cfg: PolymarketLiveConfig, events: EventSender, rx: Receiver<ExecCommand>) {
    let PolymarketLiveConfig {
        mut creds,
        maker,
        signature_type: sig_type,
        mut neg_risk,
        registry,
        tracker,
        user_channel,
        builder_code,
        presubmit_register,
        tick_regime,
    } = cfg;
    // signer = the EOA from the L1 key; derive L2 creds if not supplied (ClobAuth L1).
    //
    // Both early returns below end the exec thread WITHOUT emitting anything — deliberately, and
    // NOT a silent vanish: `ExecActor` detects a dead command thread and synthesizes the terminal
    // `OrderRejected` itself (see its `submit` / `on_send_error`), so an order submitted into a
    // credential-less client still reaches exactly one terminal event.
    let signer = match eth_address_from_private_key(&creds.private_key) {
        Ok(a) => a,
        Err(_) => return, // no key → no session (live gate)
    };
    let boot_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if ensure_l2(&mut creds, CLOB_BASE, boot_ts).is_err() {
        return; // cannot obtain L2 creds → no orders
    }
    // The user-channel pump + A3 resync, owned by THIS thread (see `spawn_live`'s doc). Started
    // BEFORE the command loop so the return lane is live before the first order can be accepted.
    let user_feed = user_channel.map(|uc| {
        let resync_creds = creds.clone();
        let resync_reg = registry.clone();
        let limit = uc.history_limit;
        tracing::info!(
            target: "vike_polymarket::client",
            markets = uc.markets.len(),
            "starting the Polymarket user-channel pump (fills/cancels return lane)"
        );
        crate::user_data::spawn_polymarket_user_data_with_resync_tracked(
            uc.ws_url,
            creds.clone(),
            uc.markets,
            registry.clone(),
            events.clone(),
            move || {
                // Account-wide REST replay after every reconnect: recovers a fill/cancel that
                // landed inside the gap REGARDLESS of the WS subscribe list. An endpoint failure
                // replays nothing rather than poisoning the lane.
                let trades = crate::exec::get_trades(CLOB_BASE, &resync_creds, limit)
                    .unwrap_or(serde_json::Value::Null);
                let orders = crate::exec::get_orders(CLOB_BASE, &resync_creds, limit)
                    .unwrap_or(serde_json::Value::Null);
                crate::history::map_polymarket_history(
                    history_rows(&trades),
                    history_rows(&orders),
                    &resync_reg,
                )
            },
            tracker.clone(),
        )
    });
    // The opt-in dynamic tick-size regime, resolved ONCE per exec thread: a caller that threaded one
    // in gets a `TickGrid` (cache + proxied transport); a caller that did not gets `None`, and with
    // it no `ureq::Agent`, no rounding and no reject observation — byte-identical to before.
    let tick_grid = tick_regime.map(|regime| TickGrid {
        regime,
        transport: UreqTransport::with_agent(crate::market_feed::VENUE, crate::egress::agent()),
    });
    let maker = if maker.is_empty() { signer.clone() } else { maker };
    let is_1271 = sig_type == SignatureType::Poly1271;
    // POLY_1271 (deposit wallet): the order's `signer` field is the maker (deposit wallet) itself and
    // orders route through the relayer; the EOA key still does the actual signing. Else signer = EOA.
    let order_signer = if is_1271 { maker.clone() } else { signer.clone() };
    let mut salt: u128 = 1;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                salt = salt.wrapping_add(1);
                let side = if req.side >= 0 { Side::Buy } else { Side::Sell };
                let reject = |reason: String| {
                    Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: reason.into(),
                        ts: req.ts,
                    })
                };
                // The wire `expiration` (unix secs), resolved BEFORE the signing domain lookup, the
                // signature and the network submit — so a GTD request carrying no expiry, or one
                // inside the venue's minimum lead, is refused LOCALLY (a terminal `OrderRejected`
                // after the synchronous `OrderSubmitted` above, per the emitter split) instead of
                // shipping a malformed order to a live market. Every non-GTD tif resolves to `0`
                // here — the value this venue has always sent — so GTC/FOK/IOC/Day are untouched.
                let expiration = match expiration_secs_of(
                    req.time_in_force,
                    req.gtd_expiry,
                    i64::try_from(now_ms()).unwrap_or(i64::MAX),
                ) {
                    Ok(secs) => secs,
                    Err(reason) => {
                        let _ = events.blocking_send(reject(reason));
                        continue;
                    }
                };
                // The EIP-712 signing domain. `None` = we could not establish whether this token's
                // market is NegRisk, and guessing would sign against the wrong `verifyingContract`
                // — so REJECT with the real reason instead of shipping a signature the CLOB will
                // bounce as "invalid signature". See `crate::neg_risk_lookup`.
                let Some(is_neg_risk) = neg_risk.resolve(&req.symbol) else {
                    let _ = events.blocking_send(reject(format!(
                        "cannot resolve the NegRisk signing domain for token {}",
                        req.symbol
                    )));
                    continue;
                };
                // The price this order is SIGNED with: snapped onto the tick grid this token was last
                // known to trade on, so a maker quoting off a grid the venue has since moved is
                // corrected instead of refused forever. Inert without a regime, and inert for a token
                // no regime has resolved yet — see `grid_price`.
                let order = build_order(
                    grid_price(tick_grid.as_ref(), &req.symbol, req.price.unwrap_or(0.0)),
                    req.qty,
                    side,
                    &req.symbol,
                    &maker,
                    &order_signer,
                    sig_type,
                    now_ms(),
                    salt,
                    is_neg_risk,
                    builder_code,
                );
                // Ack-race close (DEFAULT-OFF, `POLY_PRESUBMIT_REGISTER=1`). The CLOB echoes no
                // client id and keys every order by its EIP-712 order hash, and
                // `derive_order_id(&order)` is LIVE-VERIFIED equal to the server's `orderID` (see
                // `live_place_and_cancel`). Registering `coid`↔derived-id HERE — after the order is
                // fully built (so the hash is fixed) but BEFORE the network submit — lets a user-WS
                // fill/cancel that BEATS the HTTP ack still re-key to this order, instead of landing
                // on the `client_order_id: None` path. The real `OrderAccepted` below calls
                // `on_accept` again with the SAME `(coid, id, side)` → a true no-op by the registry's
                // idempotency contract (see `PolymarketRegistry::on_accept`). OFF ⇒ this block does
                // not run: no derive, no register, byte-identical to before (the ack is the sole
                // writer). The `i32` side matches the ack path's own `if req.side >= 0 { 1 } else
                // { -1 }` exactly, so the later duplicate carries an identical side.
                //
                // If the submit is then REJECTED, the pre-registered pair is deliberately LEFT: on a
                // transport-error reject the order may still have reached the venue, so keeping the
                // mapping is precisely the ack-race protection we want (a later fill re-keys); on a
                // genuine venue reject the order never rested, so the entry is inert — never looked
                // up (no fill can arrive), never collided (salt+timestamp make every derived id
                // unique within a session), and any stray lookup would re-key to an order the FSM has
                // already terminated (an invalid transition it drops). A future terminal-reject
                // cleanup could `registry.remove` here, but must NOT do so on the transport-error arm.
                //
                // Whatever `on_accept` hands back here is user-channel activity that was staged
                // while this order had no mapping (`crate::pending_events`). It is collected, NOT
                // emitted yet — see `replay` below.
                let mut replay: Vec<ParkedEvent> = Vec::new();
                if presubmit_register {
                    replay = registry.on_accept(
                        &req.client_order_id,
                        &derive_order_id(&order),
                        if req.side >= 0 { 1 } else { -1 },
                    );
                }
                // deposit-wallet (POLY_1271) signs the ERC-1271 wrapper + submits via the relayer;
                // else a plain V2 signature + submit.
                let signed = if is_1271 {
                    sign_order_1271(&order, &creds.private_key)
                } else {
                    sign_order(&order, &creds.private_key)
                };
                let ev = match signed {
                    Err(e) => reject(e),
                    Ok(sig) => {
                        let obj = order_to_json(&order, &sig, expiration);
                        let ot = order_type_of(req.time_in_force);
                        let submitted = if is_1271 {
                            submit_order_relayer(
                                CLOB_BASE,
                                &creds,
                                obj,
                                ot,
                                &creds.relayer_key,
                                &creds.relayer_address,
                            )
                        } else {
                            submit_order(CLOB_BASE, &creds, obj, ot)
                        };
                        match submitted {
                            Ok(resp) => match accept_outcome(&resp) {
                                Ok(oid) => {
                                    let side = if req.side >= 0 { 1 } else { -1 };
                                    // Register the dust-snap ledger entry (what we asked for, to
                                    // reconcile the venue's cent-tick match arithmetic against)
                                    // BEFORE `on_accept`: the entry is inert until the registry
                                    // makes this order visible to the user-WS pump, whereas the
                                    // reverse order leaves a window in which an instant match is
                                    // re-keyed and emitted untracked, permanently under-counting
                                    // the ledger by that qty.
                                    if let Some(t) = tracker.as_ref() {
                                        t.register(&req.client_order_id, req.qty);
                                    }
                                    // THE ack race, closed. Registering this id is the instant a
                                    // user-WS event that arrived first stops being unattributable,
                                    // and `on_accept` hands those staged frames straight back under
                                    // the same lock. Collected, not sent here: they must go out
                                    // AFTER this `OrderAccepted` (see `replay` at the bottom).
                                    replay.extend(registry.on_accept(
                                        &req.client_order_id,
                                        &oid,
                                        side,
                                    ));
                                    Event::OrderAccepted(OrderAccepted {
                                        client_order_id: req.client_order_id.clone(),
                                        venue_order_id: Some(oid.into()),
                                        ts: req.ts,
                                    })
                                }
                                Err(reason) => reject(reason),
                            },
                            Err(e) => reject(e),
                        }
                    }
                };
                // Learn from the terminal BEFORE it leaves: an off-grid refusal re-fetches this
                // token's tick size (ONE `/tick-size` GET, the `/markets` walk only as fallback) so
                // the next order is built on the grid the venue is actually enforcing. Every other
                // outcome — including this arm's own local rejects — costs nothing. Per-order
                // boundary, never per message (the hot-fold rule).
                observe_submit_outcome(tick_grid.as_ref(), &req.symbol, &ev);
                let _ = events.blocking_send(ev);
                // …and only NOW the staged user-channel events, strictly AFTER this order's own
                // terminal. Ordering is load-bearing, not cosmetic: `vike_exec`'s `ManagedOrder`
                // admits `OrderPartiallyFilled` only from Accepted/Triggered/PartiallyFilled, so
                // replaying a raced fill before the `OrderAccepted` would have the FSM drop the
                // wrapper as an invalid transition and skip `accumulate_fill` — recovering the
                // money (the bare `Fill` folds `Account` regardless) while leaving the order's own
                // filled qty short. On the reject arms this still emits: a rejected submit may
                // nonetheless have reached the venue and matched, which is exactly why the
                // pre-registered mapping is deliberately left in place above.
                emit_replayed(&replay, &registry, tracker.as_ref(), &events);
            }
            ExecCommand::Cancel { client_order_id: coid, intent } => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                let outcome = match registry.coid_to_clob(&coid) {
                    // THE RESERVE FLOOR, and this is the ONLY place it is armed. The venue meters
                    // cancels per signer, so a maker that spends its whole bucket on requote churn
                    // is cancel-LOCKED exactly when it needs to pull a book. `gate_cancel_shared`
                    // therefore sheds a `CancelIntent::Routine` cancel while the bucket sits at or
                    // below the reserve — and reads NOTHING for any other intent, so an operator
                    // ticket, a DOM click, a flatten or a dead-man trip is byte-identical to this
                    // venue's behaviour before the intent existed. Run HERE, on the exec thread,
                    // and before `cancel_order`'s debit: same thread, so the decision and the debit
                    // it authorizes cannot interleave with another cancel's.
                    //
                    // A shed cancel becomes a NON-terminal `OrderCancelRejected` like any other
                    // refused cancel: the order is STILL RESTING at the venue, so reporting it
                    // canceled — or reporting nothing — would be the silent-vanish the
                    // emitter-split contract forbids. The caller re-offers it; the reason names the
                    // refill ETA.
                    Some(oid) => match gate_cancel_shared(intent) {
                        Err(reason) => {
                            tracing::warn!(
                                venue = "polymarket",
                                coid = %coid,
                                reason = %reason,
                                "routine cancel shed to protect the flatten reserve — the order is \
                                 still resting; re-offer it"
                            );
                            CancelOutcome::Rejected(reason)
                        }
                        Ok(()) => {
                            let cancelled = if is_1271 {
                                cancel_order_relayer(
                                    CLOB_BASE,
                                    &creds,
                                    &oid,
                                    &creds.relayer_key,
                                    &creds.relayer_address,
                                )
                            } else {
                                cancel_order(CLOB_BASE, &creds, &oid)
                            };
                            match cancelled {
                                Ok(_) => CancelOutcome::Canceled,
                                Err(e) => CancelOutcome::Rejected(e),
                            }
                        }
                    },
                    None => CancelOutcome::Rejected(format!("no venue order id for {coid}")),
                };
                if matches!(outcome, CancelOutcome::Canceled) {
                    registry.remove(&coid);
                    if let Some(t) = tracker.as_ref() {
                        t.remove(&coid);
                    }
                }
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            ExecCommand::CancelBatch { client_order_ids: coids, intent } => {
                // THE BULK LANE — the whole batch, still whole, on the thread that owns the
                // cancel-token mirror. `plan_cancels` (inside `cancel_orders`) makes BOTH decisions
                // here and nowhere else: account-wide `cancel-all` (one round trip, `1 + n` tokens)
                // versus `n` targeted calls (`n` round trips, `n` tokens), and — for a
                // `CancelIntent::Routine` batch only — how many of the ids the reserve floor lets
                // through. That is the trade an emergency flatten wants: twenty resting orders is
                // ~2s of serial HTTP, and paying one extra token to make it one hop is exactly what
                // the reserve is being protected FOR.
                //
                // Asked BEFORE anything is resolved or sent: `covers_all_live` is a question about
                // the registry as it stands, and `cancel_orders` will `remove` from it below.
                let whole_book = registry.covers_all_live(&coids);
                // Resolve each coid to its venue order id, keeping the pairing so the venue's
                // per-id outcome can be reported back under the coid the core FSM keys on. An
                // unknown coid is refused with the SAME wording the single-cancel arm uses — one
                // batched cancel must not look different from one lone cancel of the same order.
                let mut resolved: Vec<(String, String)> = Vec::with_capacity(coids.len());
                for coid in &coids {
                    match registry.coid_to_clob(coid) {
                        Some(oid) => resolved.push((coid.clone(), oid)),
                        None => {
                            let reason = format!("no venue order id for {coid}");
                            let ev = cancel_event(coid, CancelOutcome::Rejected(reason));
                            let _ = events.blocking_send(ev);
                        }
                    }
                }
                if resolved.is_empty() {
                    continue;
                }
                let scope = batch_scope(whole_book, resolved.len(), coids.len());
                let order_ids: Vec<String> = resolved.iter().map(|(_, oid)| oid.clone()).collect();
                let relayer =
                    is_1271.then_some((creds.relayer_key.as_str(), creds.relayer_address.as_str()));
                let batch = cancel_orders(CLOB_BASE, &creds, &order_ids, scope, intent, relayer);
                // The bulk arm cannot attribute a failure to one order — one call covered them all
                // — so `cancel_orders` reports it under `"*"` and it applies to every id sent.
                let bulk_err =
                    batch.errors.iter().find(|(id, _)| id == "*").map(|(_, why)| why.clone());
                for (i, (coid, oid)) in resolved.iter().enumerate() {
                    let outcome = if i >= batch.plan.send {
                        // Held back by the reserve floor — only ever a `Routine` batch, and
                        // reported exactly like a shed single cancel: NON-terminal, because the
                        // order is STILL RESTING. Anything else would be the silent vanish.
                        CancelOutcome::Rejected(format!(
                            "routine cancel shed to protect the flatten reserve (retry in {}ms)",
                            batch.plan.retry_ms
                        ))
                    } else {
                        let per_id =
                            batch.errors.iter().find(|(id, _)| id == oid).map(|(_, w)| w.clone());
                        match bulk_err.clone().or(per_id) {
                            Some(reason) => CancelOutcome::Rejected(reason),
                            None => CancelOutcome::Canceled,
                        }
                    };
                    // Same teardown as the single-cancel arm, per confirmed id: the coid→clob row
                    // goes (nothing may cancel a dead order twice) while the clob→coid row is
                    // DEMOTED, so a match that raced this cancel still re-keys.
                    if matches!(outcome, CancelOutcome::Canceled) {
                        registry.remove(coid);
                        if let Some(t) = tracker.as_ref() {
                            t.remove(coid);
                        }
                    }
                    let _ = events.blocking_send(cancel_event(coid, outcome));
                }
            }
            // no native amend on this venue: a modify leaves the resting order at its terms
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
    // Deterministic teardown (the bybit convention): stop + join the user pump AND its resync
    // supervisor before this thread returns, so a window close never strands a WS thread.
    if let Some(feed) = user_feed {
        let _ = feed.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_accept_decision() {
        // success:true with an orderID → accept, carrying the id
        let ok = serde_json::json!({ "success": true, "orderID": "0xABC", "status": "live" });
        assert_eq!(accept_outcome(&ok), Ok("0xABC".to_string()));

        // success:false → reject with the errorMsg
        let bad =
            serde_json::json!({ "success": false, "errorMsg": "not enough balance / allowance" });
        assert_eq!(accept_outcome(&bad), Err("not enough balance / allowance".to_string()));

        // 2xx body with neither success nor orderID → reject (defensive; never blind-accept)
        let empty = serde_json::json!({});
        assert!(accept_outcome(&empty).is_err());
    }

    /// OFFLINE, through the REAL `ExecActor` + exec thread: a token whose NegRisk flag cannot be
    /// resolved is REJECTED with a named reason — never signed against a guessed EIP-712 domain, and
    /// never silently dropped (the venue-adapter contract: exactly one terminal per order).
    ///
    /// No network: the key is valid-shaped so the EOA derives purely, and `secret` is pre-filled so
    /// `ensure_l2` short-circuits. The injected resolver always says "don't know", which is the
    /// production behavior when `/neg-risk` is unreachable or answers without a usable flag.
    #[test]
    fn unresolvable_neg_risk_rejects_the_order_offline() {
        let (tx, mut rx) = vike_exec::event_channel(64);
        let mut client = PolymarketExecutionClient::spawn_live(
            PolymarketLiveConfig {
                creds: PolymarketCreds {
                    private_key:
                        "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                            .to_string(),
                    // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                    secret: "not-a-real-secret".to_string(),
                    ..Default::default()
                },
                maker: String::new(),
                signature_type: SignatureType::Eoa,
                neg_risk: NegRiskSource::lookup_with(
                    std::collections::HashMap::new(),
                    Box::new(|_| None),
                ),
                registry: PolymarketRegistry::new(),
                tracker: None,
                user_channel: None, // no socket, no extra thread
                builder_code: [0u8; 32],
                presubmit_register: false,
                tick_regime: None, // no transport, no rounding, no re-fetch
            },
            tx,
        );
        client.submit(&OrderRequest {
            client_order_id: "coid-nr".to_string(),
            venue: "polymarket".to_string(),
            symbol: "999".to_string(),
            side: 1,
            qty: 10.0,
            order_type: "limit".to_string(),
            price: Some(0.01),
            ..Default::default()
        });
        client.detach();

        let mut seen = Vec::new();
        while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        assert!(
            seen.iter()
                .any(|e| matches!(e, Event::OrderSubmitted(s) if s.client_order_id == "coid-nr")),
            "the synchronous OrderSubmitted still fires: {seen:?}"
        );
        let rejected = seen
            .iter()
            .find_map(|e| match e {
                Event::OrderRejected(r) if r.client_order_id == "coid-nr" => Some(r),
                _ => None,
            })
            .expect("exactly one terminal — an OrderRejected naming the reason");
        assert!(
            rejected.reason.contains("NegRisk"),
            "the reason must name the real cause, not a generic failure: {}",
            rejected.reason
        );
        assert!(
            !seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "an unresolvable domain must never reach the wire"
        );
    }

    /// `history_rows` accepts BOTH CLOB wire shapes and degrades to an empty replay rather than
    /// panicking on the resync thread.
    #[test]
    fn history_rows_tolerates_both_wire_shapes() {
        let bare = serde_json::json!([{ "id": "1" }]);
        assert_eq!(history_rows(&bare), &bare);
        let paged = serde_json::json!({ "data": [{ "id": "1" }], "next_cursor": "LTE=" });
        assert_eq!(history_rows(&paged), &serde_json::json!([{ "id": "1" }]));
        assert!(history_rows(&serde_json::json!({ "error": "nope" })).is_null());
        assert!(history_rows(&serde_json::Value::Null).is_null());
    }

    #[test]
    fn tif_to_order_type() {
        // Pins the routed polymarket row of `vike_bridge_core::tif::venue_tif` byte-for-byte.
        // NOTE the Ioc→FOK fold: the OPPOSITE direction of hyperliquid's Fok→Ioc (step-2 flip).
        assert_eq!(order_type_of(TimeInForce::Gtc), "GTC");
        assert_eq!(order_type_of(TimeInForce::Fok), "FOK");
        assert_eq!(order_type_of(TimeInForce::Ioc), "FOK");
        assert_eq!(order_type_of(TimeInForce::Gtd), "GTD");
        assert_eq!(order_type_of(TimeInForce::Day), "GTC");
    }

    /// **WIRE-BODY pin.** The tif table's own tests are declaration-vs-declaration — they assert
    /// `venue_tif("polymarket", Gtd) == Mapped("GTD")` against a matrix that repeats the same
    /// claim — so they stayed GREEN while the builder hardcoded `"expiration": "0"` and never read
    /// `gtd_expiry`, shipping `orderType: "GTD"` with a GTC expiry to a live market. This asserts
    /// on the JSON that actually goes on the wire, which is the only level that catches it.
    #[test]
    fn gtd_wire_body_carries_the_expiry_and_never_the_gtc_zero() {
        let order = build_order(
            0.52,
            100.0,
            Side::Buy,
            "71321045679252212594626385532706912750332728571942532289631379312455583992563",
            "0xmaker",
            "0xsigner",
            SignatureType::PolyProxy,
            1_700_000_000_000,
            7,
            false,
            [0u8; 32],
        );
        let now_ms = 1_700_000_000_000_i64;

        // GTD with an hour of lead → the REAL deadline reaches the wire, in unix SECONDS.
        let deadline_ms = now_ms + 3_600_000;
        let secs = expiration_secs_of(TimeInForce::Gtd, Some(deadline_ms), now_ms)
            .expect("an hour of lead clears the floor");
        let body = order_to_json(&order, "0xsig", secs);
        assert_eq!(body["expiration"], (deadline_ms / 1_000).to_string());
        assert_ne!(body["expiration"], "0", "THE BUG: a GTD order shipping the GTC expiry");
        assert_eq!(order_type_of(TimeInForce::Gtd), "GTD", "and it rides with orderType GTD");

        // …while every OTHER tif keeps the historical `"0"` byte-for-byte. GTC/FOK/IOC/Day are the
        // only orders this venue has ever actually sent, and this fix must not move any of them.
        for tif in [TimeInForce::Gtc, TimeInForce::Fok, TimeInForce::Ioc, TimeInForce::Day] {
            assert_eq!(expiration_secs_of(tif, None, now_ms), Ok(0), "{tif:?}");
            // a stray gtd_expiry on a non-GTD request is ignored, never smuggled onto the wire
            assert_eq!(expiration_secs_of(tif, Some(deadline_ms), now_ms), Ok(0), "{tif:?}");
            assert_eq!(order_to_json(&order, "0xsig", 0)["expiration"], "0", "{tif:?}");
        }
    }

    /// The client-side floor, at the boundary: `now + `[`GTD_MIN_LEAD_SECS`] is accepted and one
    /// second nearer is not, a GTD with no expiry at all is refused, and a mid-second deadline
    /// truncates DOWN so it can never round up past the bound.
    ///
    /// ⚠ 180s is TRANSCRIBED from the `@polymarket/client` SDK constant, not confirmed against a
    /// live venue response. It is applied as a client-side floor precisely so that a wrong value
    /// costs a local refusal of an order the venue might have taken — never a malformed wire body.
    #[test]
    fn gtd_expiry_inside_the_venue_floor_is_refused_locally() {
        let now_ms = 1_700_000_000_000_i64;
        let floor_secs = now_ms / 1_000 + GTD_MIN_LEAD_SECS;

        assert_eq!(
            expiration_secs_of(TimeInForce::Gtd, Some(floor_secs * 1_000), now_ms),
            Ok(u64::try_from(floor_secs).unwrap()),
            "exactly at the floor is accepted"
        );
        let err = expiration_secs_of(TimeInForce::Gtd, Some((floor_secs - 1) * 1_000), now_ms)
            .expect_err("one second inside the floor");
        assert!(err.contains("180"), "the reason must name the bound: {err}");

        // the case the old builder silently turned into `expiration: "0"`
        let err = expiration_secs_of(TimeInForce::Gtd, None, now_ms).expect_err("no expiry at all");
        assert!(err.contains("gtd_expiry"), "the reason must name the missing field: {err}");

        assert!(
            expiration_secs_of(TimeInForce::Gtd, Some(floor_secs * 1_000 - 1), now_ms).is_err(),
            "a mid-second deadline truncates down, never up past the floor"
        );
    }

    /// OFFLINE, through the REAL `ExecActor` + exec thread — the sibling of
    /// `unresolvable_neg_risk_rejects_the_order_offline`: a GTD request with no expiry gets exactly
    /// one terminal, a LOCAL `OrderRejected`, and never reaches the wire.
    ///
    /// The injected neg-risk resolver is the always-`None` one, so a reason naming `gtd_expiry`
    /// rather than NegRisk ALSO proves the expiry gate runs before the signing-domain lookup — i.e.
    /// before any signing or network work, which is the placement that makes it a local refusal.
    #[test]
    fn gtd_without_an_expiry_never_reaches_the_wire() {
        let (tx, mut rx) = vike_exec::event_channel(64);
        let mut client = PolymarketExecutionClient::spawn_live(
            PolymarketLiveConfig {
                creds: PolymarketCreds {
                    private_key:
                        "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                            .to_string(),
                    // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                    secret: "not-a-real-secret".to_string(),
                    ..Default::default()
                },
                maker: String::new(),
                signature_type: SignatureType::Eoa,
                neg_risk: NegRiskSource::lookup_with(
                    std::collections::HashMap::new(),
                    Box::new(|_| None),
                ),
                registry: PolymarketRegistry::new(),
                tracker: None,
                user_channel: None,
                builder_code: [0u8; 32],
                presubmit_register: false,
                tick_regime: None,
            },
            tx,
        );
        client.submit(&OrderRequest {
            client_order_id: "coid-gtd".to_string(),
            venue: "polymarket".to_string(),
            symbol: "999".to_string(),
            side: 1,
            qty: 10.0,
            order_type: "limit".to_string(),
            price: Some(0.01),
            time_in_force: TimeInForce::Gtd,
            gtd_expiry: None,
            ..Default::default()
        });
        client.detach();

        let mut seen = Vec::new();
        while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        assert!(
            seen.iter()
                .any(|e| matches!(e, Event::OrderSubmitted(s) if s.client_order_id == "coid-gtd")),
            "the synchronous OrderSubmitted still fires first, per the emitter split: {seen:?}"
        );
        let rejected = seen
            .iter()
            .find_map(|e| match e {
                Event::OrderRejected(r) if r.client_order_id == "coid-gtd" => Some(r),
                _ => None,
            })
            .expect("exactly one terminal — an OrderRejected naming the reason");
        assert!(
            rejected.reason.contains("gtd_expiry"),
            "the reason must name the missing expiry (and so prove the gate ran BEFORE the \
             NegRisk lookup this config can never satisfy): {}",
            rejected.reason
        );
        assert!(
            !seen.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
            "a GTD with no expiry must never reach the wire"
        );
    }

    /// The safety decision of the whole bulk lane, both halves. `WholeBook` is an ASSERTION that
    /// the account-wide `cancel-all` is equivalent to cancelling exactly these ids, so a batch that
    /// does not name this mount's whole known book — or that named an id which did not resolve, and
    /// is therefore reaching outside it — must stay `Subset`, which `plan_cancels` can never take
    /// to the account-wide arm however much budget is free.
    #[test]
    fn only_a_fully_resolved_whole_book_batch_may_assert_the_account_wide_scope() {
        assert_eq!(batch_scope(true, 3, 3), CancelScope::WholeBook);
        assert_eq!(batch_scope(false, 3, 3), CancelScope::Subset, "not the whole book");
        assert_eq!(batch_scope(true, 2, 3), CancelScope::Subset, "an id did not resolve");
        assert_eq!(batch_scope(false, 2, 3), CancelScope::Subset);
    }

    /// OFFLINE, through the REAL `ExecActor` + exec thread — the batch door's sibling of
    /// `unresolvable_neg_risk_rejects_the_order_offline`. A batch of coids this mount never
    /// registered resolves to nothing, so the arm refuses each id and returns before any network
    /// work: it proves the batch crossed the seam as ONE command and was still reported PER ID,
    /// with the same wording one lone cancel of the same order would carry. Non-terminal, because
    /// an order we cannot address is not an order we cancelled.
    #[test]
    fn a_batch_of_unregistered_coids_is_refused_per_id_offline() {
        let (tx, mut rx) = vike_exec::event_channel(64);
        let mut client = PolymarketExecutionClient::spawn_live(
            PolymarketLiveConfig {
                creds: PolymarketCreds {
                    private_key:
                        "0xc85ef7d79691fe79573b1a7064c19c1a9819ebdbd1faaab1a8ec92344438aaf4"
                            .to_string(),
                    // non-empty ⇒ `ensure_l2` returns Ok immediately, so this test makes NO call
                    secret: "not-a-real-secret".to_string(),
                    ..Default::default()
                },
                maker: String::new(),
                signature_type: SignatureType::Eoa,
                neg_risk: NegRiskSource::lookup_with(
                    std::collections::HashMap::new(),
                    Box::new(|_| None),
                ),
                registry: PolymarketRegistry::new(),
                tracker: None,
                user_channel: None,
                builder_code: [0u8; 32],
                presubmit_register: false,
                tick_regime: None,
            },
            tx,
        );
        // RiskOff: the reserve may not shed it, so nothing here can be a shed rather than a miss.
        client.cancel_batch_with_intent(
            &["gone-1".to_string(), "gone-2".to_string()],
            CancelIntent::RiskOff,
        );
        client.detach();

        let mut seen = Vec::new();
        while let Ok(vike_exec::Ingest::Event(e)) = rx.try_recv() {
            seen.push(e);
        }
        let refused: Vec<(String, String)> = seen
            .iter()
            .map(|e| match e {
                Event::OrderCancelRejected(r) => (r.client_order_id.clone(), r.reason.to_string()),
                other => panic!("an unaddressable cancel must be NON-terminal: {other:?}"),
            })
            .collect();
        assert_eq!(refused.len(), 2, "one advisory per id, not one per batch: {refused:?}");
        for (coid, reason) in &refused {
            assert!(reason.contains(coid), "the reason must name the order: {reason}");
            assert!(reason.contains("no venue order id"), "single-arm wording: {reason}");
        }
    }

    /// The DEFAULT-OFF ack-race close (`POLY_PRESUBMIT_REGISTER`), proven at the exact granularity
    /// the submit arm injects it — no signing, no network: `build_order` + `derive_order_id` +
    /// `on_accept` + `lookup_clob` are the four pure calls the `if presubmit_register { … }` block
    /// plus the user-WS pump make. It builds the SAME `Order` the exec thread would for a request,
    /// derives its CLOB id pre-submit, registers coid↔id BEFORE any ack, and confirms a fill/cancel
    /// that BEATS the ack (a `lookup_clob` on that derived id, the pump's real re-key path) already
    /// resolves to our coid + side. It also pins the OFF state (nothing keyed before the register)
    /// and the idempotent duplicate ack (the real `OrderAccepted`, same id + side, changes nothing).
    #[test]
    fn presubmit_register_lets_a_fill_beat_the_ack() {
        let registry = PolymarketRegistry::new();
        let coid = "coid-presub";
        let req_side = 1_i32; // BUY

        // exactly the Order the submit arm builds for this request (same call, same args) …
        let order = build_order(
            0.52,
            100.0,
            if req_side >= 0 { Side::Buy } else { Side::Sell },
            "71321045679252212594626385532706912750332728571942532289631379312455583992563",
            "0xmaker",
            "0xsigner",
            SignatureType::PolyProxy,
            1_700_000_000_000,
            7,
            false,
            [0u8; 32],
        );
        let clob_id = derive_order_id(&order);
        let side_code = if req_side >= 0 { 1 } else { -1 };

        // OFF / pre-injection: with `POLY_PRESUBMIT_REGISTER` unset the block never runs, so the
        // derived id is not keyed yet — byte-identical to before the flag (the ack is sole writer).
        assert_eq!(registry.lookup_clob(&clob_id), None);
        assert_eq!(registry.coid_to_clob(coid), None);

        // ON: exactly what the submit arm's `if presubmit_register { … }` block does —
        // `registry.on_accept(coid, &derive_order_id(&order), side)`. Nothing raced this one, so the
        // park hands nothing back; asserting that (rather than discarding the `#[must_use]`) is what
        // pins "pre-registering an order does not, by itself, replay anything".
        assert!(registry.on_accept(coid, &clob_id, side_code).is_empty(), "nothing was parked");

        // a fill/cancel that BEAT the HTTP ack lands on the user-WS keyed by the CLOB order id,
        // which is exactly this derived id → the pump's `lookup_clob` re-keys it to our coid + side.
        assert_eq!(registry.lookup_clob(&clob_id), Some((coid.to_string(), 1)));
        // and the cancel direction (coid → venue id) is live before any ack, too.
        assert_eq!(registry.coid_to_clob(coid), Some(clob_id.clone()));

        // the real server `OrderAccepted` carries the SAME id (== derived) + side → idempotent no-op,
        // and the park makes that idempotency TOTAL: the second call finds it empty and claims nothing,
        // so the duplicate ack cannot re-emit a fill the first one already replayed.
        assert!(
            registry.on_accept(coid, &clob_id, side_code).is_empty(),
            "the duplicate ack claims nothing"
        );
        assert_eq!(registry.lookup_clob(&clob_id), Some((coid.to_string(), 1)));
        assert_eq!(registry.coid_to_clob(coid), Some(clob_id));
    }

    // ---- the dynamic tick-size regime, at the exact seam the exec thread consumes it ------------
    //
    // The submit arm's two tick-regime touches ARE `grid_price` (the price `build_order` is handed)
    // and `observe_submit_outcome` (the terminal `Event` fed back to the cache), so driving those two
    // with the crate's REST doubles (`tick_regime::stubs`) exercises the wiring itself rather than a
    // paraphrase of it. The submit that sits between them is a signed network call, so an end-to-end
    // exec-thread test cannot reach it offline — `unresolvable_neg_risk_rejects_the_order_offline`
    // above is the exec-thread-level coverage, and it pins the `tick_regime: None` path.

    use crate::tick_regime::stubs::{DeadStub, TickStub};

    /// Rounded prices compare with a tolerance, NOT `==`: `n * tick` is a floating-point product
    /// whose last bit depends on the multiplier, and nothing here is a parity fixture — the property
    /// under test is "which grid was used", not a bit pattern.
    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    /// Exactly the `OrderRejected` the submit arm's `reject(...)` closure builds, so the tests feed
    /// the wiring the same event shape production does.
    fn rejected(reason: &str) -> Event {
        Event::OrderRejected(OrderRejected {
            client_order_id: "coid-tick".to_string(),
            reason: reason.to_string().into(),
            ts: 0,
        })
    }

    fn grid<T: RestTransport>(regime: TickRegime, transport: T) -> TickGrid<T> {
        TickGrid { regime, transport }
    }

    /// THE FIX: a maker quoting a longshot on a stale cent grid is refused, the refusal re-fetches
    /// the tick ONCE, and the very next order is priced on the tightened grid — the reject loop the
    /// venue never tells us about is broken after exactly one rejected order.
    #[test]
    fn an_off_grid_reject_refetches_once_and_reprices_the_next_order() {
        let g = grid(TickRegime::new(), TickStub::new(0.001));
        g.regime.set("111", 0.01); // the stale cent grid the maker was quoting on

        let stale = grid_price(Some(&g), "111", 0.9636);
        assert!(approx(stale, 0.96), "the stale cent grid prices 0.9636 at 0.96, got {stale}");

        assert_eq!(
            observe_submit_outcome(Some(&g), "111", &rejected("invalid tick size")),
            Some(0.001),
            "the off-grid refusal resolved the grid the venue is now enforcing"
        );
        assert_eq!(
            g.transport.calls(),
            1,
            "exactly one direct /tick-size lookup, no /markets walk"
        );

        let fresh = grid_price(Some(&g), "111", 0.9636);
        assert!(approx(fresh, 0.964), "the next order is priced on the new grid, got {fresh}");
        assert_eq!(g.transport.calls(), 1, "and pricing itself never costs a REST call");
    }

    /// Every non-off-grid outcome is a no-op that costs NO REST round-trip — the balance/allowance
    /// refusal is the one that would otherwise put a `/tick-size` GET on the reject path of an
    /// under-funded account, and an ACCEPTED order must obviously never trigger one.
    #[test]
    fn an_unrelated_reject_or_an_acceptance_costs_no_rest_call() {
        let g = grid(TickRegime::new(), TickStub::new(0.001));
        g.regime.set("111", 0.01);

        for ev in [
            rejected("not enough balance / allowance"),
            rejected("cannot resolve the NegRisk signing domain for token 111"),
            rejected("submit response missing orderID"),
            Event::OrderAccepted(OrderAccepted {
                client_order_id: "coid-tick".to_string(),
                venue_order_id: Some("0xABC".into()),
                ts: 0,
            }),
        ] {
            assert_eq!(observe_submit_outcome(Some(&g), "111", &ev), None, "{ev:?}");
        }
        assert_eq!(g.transport.calls(), 0, "no REST traffic on any of them");
        assert_eq!(g.regime.tick_size("111"), Some(0.01), "and the cached grid is untouched");
    }

    /// A re-fetch that resolves NOTHING (the Dublin proxy dropping the request) must leave the
    /// known-good grid alone rather than defaulting it to `0.01` — otherwise one transient blip
    /// silently re-prices every subsequent order onto a grid the venue is not enforcing.
    #[test]
    fn a_failed_refetch_preserves_the_grid_the_next_order_is_priced_on() {
        let g = grid(TickRegime::new(), DeadStub::default());
        g.regime.set("111", 0.001); // the known-good tightened grid

        assert_eq!(observe_submit_outcome(Some(&g), "111", &rejected("invalid tick size")), None);
        assert!(g.transport.calls() >= 2, "the direct lookup AND the paged fallback were tried");
        assert_eq!(g.regime.tick_size("111"), Some(0.001), "never clobbered by DEFAULT_TICK_SIZE");
        let after = grid_price(Some(&g), "111", 0.9636);
        assert!(approx(after, 0.964), "so pricing still uses the surviving grid, got {after}");
    }

    /// An UNKNOWN token is priced VERBATIM: a regime that has never resolved this token must not
    /// guess it onto the venue default, which would silently move a price the caller chose. This is
    /// also why a freshly-mounted (empty) regime is inert until the venue itself teaches it.
    #[test]
    fn an_unknown_token_is_priced_verbatim() {
        let g = grid(TickRegime::new(), TickStub::new(0.001));
        // no arithmetic runs on an untouched price, so `==` IS exact here
        assert_eq!(grid_price(Some(&g), "999", 0.96351), 0.96351);
        assert_eq!(g.regime.tick_size("999"), None, "and it stays unknown, not born on a grid");
        assert_eq!(g.transport.calls(), 0, "pricing an unknown token is not a lookup");
    }

    /// `tick_regime: None` — every entry point but the live mount — is byte-identical: the price is
    /// passed through untouched and no reject is ever observed (there is no transport to observe it
    /// with, which is exactly why `run` builds none).
    #[test]
    fn without_a_regime_the_submit_path_is_byte_identical() {
        let none: Option<&TickGrid<TickStub>> = None;
        for price in [0.9636, 0.5, 0.0, 1.0] {
            assert_eq!(grid_price(none, "111", price), price);
        }
        assert_eq!(observe_submit_outcome(none, "111", &rejected("invalid tick size")), None);
    }

    /// LIVE, real-money (Polygon mainnet): place a TINY BUY at 0.01 — far below any real mid so it
    /// rests and CANNOT fill — then cancel. Live acceptance proves the V2 order signature is
    /// byte-correct. Order placement is geo-blocked, so run WITH the Dublin proxy:
    /// `POLY_SOCKS_PROXY=socks5://127.0.0.1:1080 cargo test -p vike-polymarket --features polymarket \
    ///   --lib live_place_and_cancel -- --ignored --nocapture`
    #[test]
    #[ignore = "LIVE real-money: places+cancels a tiny non-filling order on Polymarket mainnet"]
    fn live_place_and_cancel() {
        vike_log::test_init();
        use crate::ensure_l2;
        use crate::eth_address_from_private_key;
        use crate::{
            build_order, cancel_order, cancel_order_relayer, order_to_json, sign_order,
            sign_order_1271, submit_order, submit_order_relayer, Side, SignatureType,
        };
        use crate::{PolymarketCreds, CLOB_BASE};

        // 1. real creds (python env) → EOA + L2
        let vars = vike_bridge_core::credentials::load_workspace_dotenv();
        let pk = vars.get("POLY_PRIVATE_KEY").expect("POLY_PRIVATE_KEY").clone();
        let relayer_addr = vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
        let relayer_key = vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
        let signer = eth_address_from_private_key(&pk).expect("EOA");
        let mut creds = PolymarketCreds {
            private_key: pk.clone(),
            address: signer.clone(),
            ..Default::default()
        };
        let boot =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
                as i64;
        ensure_l2(&mut creds, CLOB_BASE, boot).expect("derive L2");
        // The account's funder is a Polymarket DEPOSIT WALLET → signatureType POLY_1271, with
        // maker == signer == the deposit wallet (NOT the EOA); the EOA key is the authorized signer
        // the deposit-wallet contract recognises. Orders route via the relayer (gasless).
        let deposit_wallet = std::env::var("POLY_FUNDER")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "0x107C01D04Fd68557ACd52E89dD01972b22803aD5".to_string());
        let sig_type = match std::env::var("POLY_SIGNATURE_TYPE").ok().as_deref() {
            Some("0") => SignatureType::Eoa,
            Some("1") => SignatureType::PolyProxy,
            Some("2") => SignatureType::PolyGnosisSafe,
            _ => SignatureType::Poly1271,
        };
        // for POLY_1271 the order's `signer` field is the deposit wallet itself (verifyingContract).
        let order_signer = if sig_type == SignatureType::Poly1271 {
            deposit_wallet.clone()
        } else {
            signer.clone()
        };
        tracing::info!(target: "vike_polymarket::client", "EOA(auth key)={signer}  maker=signer={deposit_wallet}  sigType={sig_type:?}");

        // 2. a liquid token with a safe mid (reads via the proxy — clob is DNS-blocked here)
        let markets = crate::egress::get_json(CLOB_BASE, "/sampling-markets", "").expect("markets");
        let empty = Vec::new();
        let data = markets.get("data").and_then(|d| d.as_array()).unwrap_or(&empty);
        let mut chosen: Option<(String, bool, f64)> = None;
        'outer: for m in data {
            let neg_risk = m.get("neg_risk").and_then(|n| n.as_bool()).unwrap_or(false);
            for tk in m.get("tokens").and_then(|t| t.as_array()).unwrap_or(&empty) {
                let Some(tid) =
                    tk.get("token_id").and_then(|x| x.as_str()).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let mid =
                    crate::egress::get_json(CLOB_BASE, "/midpoint", &format!("token_id={tid}"))
                        .ok()
                        .and_then(|v| {
                            v.get("mid")
                                .and_then(|m| m.as_str())
                                .and_then(|s| s.parse::<f64>().ok())
                        });
                if let Some(mid) = mid {
                    if (0.15..0.85).contains(&mid) {
                        chosen = Some((tid.to_string(), neg_risk, mid));
                        break 'outer;
                    }
                }
            }
        }
        let (token_id, neg_risk, mid) = chosen.expect("a token with a safe midpoint");
        tracing::info!(target: "vike_polymarket::client", "token={token_id}  neg_risk={neg_risk}  mid={mid}");

        // 3. tiny BUY @ 0.01 (mid > 0.15 → CANNOT fill), ~$1.20 notional
        let price = 0.01;
        let size = (1.2_f64 / price).ceil();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        // salt MUST be ≤ 2^53-1 (the wire carries it as a JSON number); a wider salt makes the
        // server rebuild a different EIP-712 hash → "invalid signature". Match the SDK's 53-bit cap.
        let salt = now.as_nanos() & ((1u128 << 53) - 1);
        // builder code for fee attribution, resolved through the SAME path production uses
        // (`live_mount_from_vars`): `attribution_code_from` + `decode_builder_bytes32`. Malformed/
        // absent degrades to `[0u8; 32]` (unattributed) rather than failing the smoke.
        let builder_code =
            vike_bridge_core::credentials::attribution_code_from(&vars, "polymarket")
                .and_then(decode_builder_bytes32)
                .unwrap_or([0u8; 32]);
        let order = build_order(
            price,
            size,
            Side::Buy,
            &token_id,
            &deposit_wallet,
            &order_signer,
            sig_type,
            now.as_millis(),
            salt,
            neg_risk,
            builder_code,
        );
        tracing::info!(target: "vike_polymarket::client", "BUY {size} @ {price}  makerAmt={} takerAmt={}", order.maker_amount, order.taker_amount);
        // deposit wallet (POLY_1271) signs via the ERC-1271 wrapper; else a plain V2 signature.
        let sig = if sig_type == SignatureType::Poly1271 {
            sign_order_1271(&order, &pk).expect("sign 1271")
        } else {
            sign_order(&order, &pk).expect("sign")
        };
        let order_obj = order_to_json(&order, &sig, 0); // GTC smoke → "no expiry"
        tracing::debug!(target: "vike_polymarket::client", "payload order = {order_obj}");
        // deposit-wallet orders route through the relayer (gasless); else the plain submit.
        let submitted = if sig_type == SignatureType::Poly1271 {
            submit_order_relayer(CLOB_BASE, &creds, order_obj, "GTC", &relayer_key, &relayer_addr)
        } else {
            submit_order(CLOB_BASE, &creds, order_obj, "GTC")
        };
        match submitted {
            Ok(resp) => {
                tracing::info!(target: "vike_polymarket::client", "✅ SUBMIT ACCEPTED — order signature valid: {resp}");
                if let Some(oid) =
                    resp.get("orderID").or_else(|| resp.get("orderId")).and_then(|o| o.as_str())
                {
                    // LIVE VERIFICATION of the `derive_order_id` premise: the CLOB returns the
                    // EIP-712 order hash as `orderID`, so the value computable pre-submit must equal
                    // it. Proving this on demo is the gate for wiring `coid`↔`clob_id`
                    // pre-registration into the exec submit path (registry.on_accept before the ack).
                    let derived = crate::order::derive_order_id(&order);
                    assert!(
                        oid.eq_ignore_ascii_case(&derived),
                        "CLOB orderID {oid} must equal the derived EIP-712 order hash {derived}"
                    );
                    let cancelled = if sig_type == SignatureType::Poly1271 {
                        cancel_order_relayer(CLOB_BASE, &creds, oid, &relayer_key, &relayer_addr)
                    } else {
                        cancel_order(CLOB_BASE, &creds, oid)
                    };
                    match cancelled {
                        Ok(c) => {
                            tracing::info!(target: "vike_polymarket::client", "✅ CANCELLED clean: {c}")
                        }
                        Err(e) => {
                            tracing::warn!(target: "vike_polymarket::client", "⚠ CANCEL FAILED — cancel {oid} MANUALLY: {e}")
                        }
                    }
                }
            }
            Err(e) => tracing::error!(target: "vike_polymarket::client", "❌ SUBMIT REJECTED: {e}"),
        }
    }

    /// Cancel one resting order by id (deposit-wallet/relayer flow). Run with the Dublin proxy:
    /// `POLY_CANCEL_ORDER_ID=0x… cargo test -p vike-polymarket --features polymarket --lib \
    ///   live_cancel_order -- --ignored --nocapture`
    #[test]
    #[ignore = "LIVE: cancels the POLY_CANCEL_ORDER_ID order on Polymarket mainnet"]
    fn live_cancel_order() {
        vike_log::test_init();
        use crate::{
            cancel_order_relayer, ensure_l2, eth_address_from_private_key, PolymarketCreds,
            CLOB_BASE,
        };
        let oid = std::env::var("POLY_CANCEL_ORDER_ID").expect("set POLY_CANCEL_ORDER_ID");
        let vars = vike_bridge_core::credentials::load_workspace_dotenv();
        let pk = vars.get("POLY_PRIVATE_KEY").expect("POLY_PRIVATE_KEY").clone();
        let relayer_addr = vars.get("POLY_RELAYER_API_KEY_ADDRESS").cloned().unwrap_or_default();
        let relayer_key = vars.get("POLY_RELAYER_API_KEY").cloned().unwrap_or_default();
        let signer = eth_address_from_private_key(&pk).expect("EOA");
        let mut creds = PolymarketCreds { private_key: pk, address: signer, ..Default::default() };
        let boot =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
                as i64;
        ensure_l2(&mut creds, CLOB_BASE, boot).expect("derive L2");
        match cancel_order_relayer(CLOB_BASE, &creds, &oid, &relayer_key, &relayer_addr) {
            Ok(c) => tracing::info!(target: "vike_polymarket::client", "✅ CANCELLED {oid}: {c}"),
            Err(e) => panic!("cancel failed: {e}"),
        }
    }
}
