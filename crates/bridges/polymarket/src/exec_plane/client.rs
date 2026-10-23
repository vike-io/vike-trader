//! Polymarket `ExecutionClient` — the vike-model seam. A dedicated thread owns the creds, derives L2
//! (ClobAuth L1) on start, and maps each `OrderRequest` → a signed V2 CLOB order → submit → venue
//! events. `OrderRequest.symbol` is the ERC-1155 outcome `token_id`; `qty` is shares; `price` is the
//! probability. The submit response gives Accepted/Rejected; **fills/cancels arrive on the
//! authenticated user WS** — which [`PolymarketExecutionClient::spawn_live`] starts from inside this
//! same exec thread (the bybit/okx convention: one thread owns the pump's lifetime and shuts it down
//! when the command loop ends), so a started engine's return lane exists by construction rather than
//! by a composition root remembering to wire one. NegRisk (the signing domain) is resolved per order
//! through [`NegRiskSource`], which — unlike a bare set — can say "don't know" and reject.
//!
//! The exec thread is also where the **dynamic tick-size regime** ([`crate::tick_regime`]) is
//! consumed, when a caller threads one onto [`PolymarketLiveConfig::tick_regime`]: each submit's
//! price is snapped onto that token's cached grid ([`grid_price`]) and each terminal reject is fed
//! back to the cache ([`observe_submit_outcome`]), so an off-grid refusal re-fetches the grid instead
//! of repeating forever. `None` ⇒ neither happens and nothing is built for it.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vike_bridge_core::eip712::eth_address_from_private_key;
use vike_bridge_core::exec_actor::{
    CancelOutcome, ConfirmFn, ExecActor, ExecCommand, cancel_event,
};
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_exec::{CancelIntent, EventSender, ExecutionClient};
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, TimeInForce};

use super::exec::{
    CancelScope, cancel_order, cancel_order_relayer, cancel_orders, found_order,
    gate_cancel_shared, get_order, submit_error_is_ambiguous, submit_order, submit_order_relayer,
};
use super::fill_tracker::FillTracker;
use super::l1::ensure_l2;
use super::order::{
    Side, SignatureType, build_order, derive_order_id, order_to_json, sign_order, sign_order_1271,
};
use super::pending_events::ParkedEvent;
use super::registry::PolymarketRegistry;
use crate::config::{CLOB_BASE, PolymarketCreds};
use crate::neg_risk_lookup::NegRiskSource;
use crate::tick_regime::TickRegime;

/// Live Polymarket exec client. `submit`/`cancel` enqueue onto the exec thread (non-blocking).
pub struct PolymarketExecutionClient(ExecActor);

impl PolymarketExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    /// The ONE entry point: the exec thread, PLUS (when
    /// [`PolymarketLiveConfig::user_channel`] is `Some`) the authenticated user-WS fill pump and its
    /// audit-A3 post-reconnect resync, started from inside that same thread and shut down when it
    /// ends.
    ///
    /// Why the pump lives here and not in the composition root: the venue-adapter contract says no
    /// order may silently vanish, and on this venue every terminal AFTER acceptance (fill, cancel,
    /// expiry) arrives ONLY on the user channel. A start that spawned the exec client without the
    /// pump would accept orders that could never reach a terminal event — so the two are made
    /// inseparable, exactly as bybit/okx do it (their `run` starts the private-WS pump before the
    /// command loop and `shutdown()`s it after). `user_channel: None` starts no socket and no extra
    /// thread (the caller then owns the return lane).
    ///
    /// ⚠ **The ONE venue in the tree that declares [`ExecActor::with_bulk_cancel`]**, because it is
    /// the one with a native account-wide `cancel-all` AND a client-side cancel-token budget that
    /// has to choose between it and `n` targeted calls. The declaration is what makes
    /// [`ExecCommand::CancelBatch`] reachable at all, and the exec thread's arm for it is the
    /// obligation that comes with declaring one.
    pub fn spawn_live(cfg: PolymarketLiveConfig, events: EventSender) -> Self {
        // The submits left UNKNOWN (coid -> derived CLOB id), shared by the exec thread (writer) and
        // the `confirm` re-query worker (reader).
        let unknown = UnknownSubmits::default();
        let confirm: ConfirmFn = {
            let (unknown, creds) = (unknown.clone(), cfg.creds.clone());
            Arc::new(move |coid: &str| {
                confirm_unknown_submit(&unknown, coid, vike_model::now_ms(), |clob_id| {
                    let mut creds = creds.clone();
                    ensure_l2(&mut creds, CLOB_BASE, vike_model::time::clock::now_secs())
                        .map_err(|e| e.to_string())?;
                    get_order(CLOB_BASE, &creds, clob_id)
                })
            })
        };
        Self(
            ExecActor::spawn("polymarket-exec", events.clone(), move |rx| {
                run(cfg, events, rx, unknown)
            })
            .with_bulk_cancel()
            .with_confirm(confirm),
        )
    }
}

/// Everything the live exec thread owns. A struct rather than a 9-argument `spawn` (clippy's
/// `too_many_arguments`, and the call site reads as a configuration rather than a positional puzzle).
pub struct PolymarketLiveConfig {
    /// L1 key + (ideally already-derived) L2 trio + relayer identity. If `secret` is empty the exec
    /// thread derives L2 itself via `ensure_l2` — a start that already derived (to build the recon
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
    /// Optional dust-snap ledger ([`crate::exec_plane::fill_tracker`]).
    pub tracker: Option<FillTracker>,
    /// `Some` ⇒ start the authenticated user-WS pump inside the exec thread. `None` ⇒ don't (the
    /// historical shape; the caller is then responsible for the return lane).
    pub user_channel: Option<UserChannelConfig>,
    /// bytes32 `builderCode` stamped into every signed order for fee attribution (see
    /// [`crate::exec_plane::order::Order::builder`]). `[0u8; 32]` is unattributed — byte-identical
    /// to before this field existed.
    pub builder_code: [u8; 32],
    /// Close the ack-race by pre-registering `coid`↔`derive_order_id(&order)` in the shared
    /// `registry` just BEFORE each submit, so a user-WS fill/cancel that beats the HTTP ack still
    /// re-keys to the local order (done in the exec thread's submit arm; resolved from
    /// `POLY_PRESUBMIT_REGISTER` at the start via [`crate::exec_plane::adapter::presubmit_register_enabled`]).
    /// DEFAULT-OFF (`false`): `false` computes and registers NOTHING pre-submit — byte-identical to
    /// before this field existed, with the ack path ([`PolymarketRegistry::on_accept`] after
    /// acceptance) the sole registry writer, exactly as today.
    pub presubmit_register: bool,
    /// Whether the local submit gate REFUSES an over-budget submit (`true`) or only counts it
    /// (`false`, the default) — resolved ONCE at the start by
    /// [`crate::exec_plane::exec::rate_gate_enforced`] from `venue.polymarket.rate_gate`.
    pub rate_gate: bool,
    /// The SHARED dynamic tick-size cache ([`crate::tick_regime`]) the exec thread prices orders on
    /// and teaches from venue rejects. `Some` ⇒ each submit's price is snapped onto this token's
    /// cached grid before the order is built, and an off-grid reject re-fetches that grid so the NEXT
    /// order is priced on the one the venue is actually enforcing — the self-heal for the
    /// reject-forever loop the module doc describes. `Clone` shares one cache, so a quoting path that
    /// keeps its own clone reads the same learned grid.
    ///
    /// `None` (the live start threads `Some`, see [`crate::live_runtime_for_account`]) is byte-identical
    /// to before this field existed: no transport is constructed, no price is rounded, no reject is
    /// observed. A `Some` regime that has never resolved this token is ALSO inert — an unknown token prices
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
    /// [`crate::exec_plane::adapter::poly_exec_markets`] for where a start gets them and what an empty list means.
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
    /// makes the reserve decision beside the debit it causes (see [`crate::exec_plane::exec::gate_cancel`]).
    /// Forwarding is all this wrapper does — `ExecActor` puts the intent on the queued command.
    fn cancel_with_intent(&mut self, client_order_id: &str, intent: CancelIntent) {
        self.0.cancel_with_intent(client_order_id, intent)
    }
    /// Explicit, not inherited: the trait default routes to `cancel_batch`, which would fan out to
    /// the intent-LESS `cancel` and drop the classification for every batched cancel. `ExecActor`
    /// carries the whole batch, with its intent, to the exec thread as ONE
    /// [`ExecCommand::CancelBatch`] — this client declared that lane at
    /// [`spawn_live`](Self::spawn_live) — where [`crate::exec_plane::exec::plan_cancels`] decides between the
    /// account-wide `cancel-all` and `n` targeted calls under the cancel-token budget.
    ///
    /// ⚠ This forwarded to a per-order FAN-OUT until the bulk lane existed, which is what made
    /// `plan_cancels`/`cancel_orders`/`cancel_all_orders` unreachable: the batch was already `n`
    /// singles before any venue code ran, so nothing could ever choose the bulk arm.
    fn cancel_batch_with_intent(&mut self, client_order_ids: &[String], intent: CancelIntent) {
        self.0.cancel_batch_with_intent(client_order_ids, intent)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
    }
    /// ⚠ Explicit, not inherited: the trait default is a NO-OP, so a wrapper that omits this makes
    /// the `with_confirm` re-query wired in [`spawn_live`](Self::spawn_live) unreachable. Nothing
    /// fails when it is missing; the pin is `confirm_reaches_the_actors_re_query` in the tests.
    fn confirm(&mut self, client_order_id: &str) {
        self.0.confirm(client_order_id)
    }
    fn detach(&mut self) {
        self.0.detach()
    }
}

/// Decode a 0x-prefixed 32-byte hex builder code; `None` (→ zero) on any malformed value so a bad
/// `.env` entry degrades to unattributed rather than failing the start. Shared by
/// [`crate::exec_plane::adapter::live_runtime_for_account`] (the production resolve) and the live-money
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

/// This venue's row of the ONE cross-venue TIF authority ([`vike_venues::venue_tif::venue_tif`]),
/// consumed: `Ioc` is COERCED to `"FOK"` — the OPPOSITE direction of hyperliquid's `Fok`→`Ioc`
/// fold (same pair!); step 2 resolves that deliberately, behind demo smokes.
/// GTD's wire `expiration` is resolved by the sibling [`expiration_secs_of`]; this function only
/// picks the `orderType` string.
fn order_type_of(tif: TimeInForce) -> &'static str {
    // `wire()` is Some for every polymarket row (Mapped/Coerced only) — the fallback is
    // unreachable, kept so the exec thread can never panic.
    vike_venues::venue_tif::venue_tif(crate::market_feed::VENUE, tif).wire().unwrap_or("GTC")
}

/// The CLOB's minimum GTD lifetime: a limit order whose `expiration` is nearer than this is
/// refused by the venue. `180`s as of `@polymarket/client` 0.1.0-beta.12 (earlier builds used 60s).
///
/// ⚠ TRANSCRIBED from the SDK constant recorded in [`crate::exec_plane::order`]'s module doc, **not**
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
    if v.is_array() { v } else { v.get("data").filter(|d| d.is_array()).unwrap_or(EMPTY) }
}

/// Emit the user-channel events that were staged while this order had no CLOB→coid mapping.
///
/// This is the other half of the fix in [`crate::exec_plane::pending_events`]: the exec thread is the ONLY
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
    for ev in crate::exec_plane::user_ws::replay_parked(parked, registry, tracker) {
        let _ = events.blocking_send(ev);
    }
}

/// The [`CancelScope`] one batch is planned under — the safety decision of the whole bulk lane,
/// pulled out of the arm because it is the ONE thing that decides whether an ACCOUNT-WIDE call can
/// happen at all.
///
/// [`CancelScope::WholeBook`] is the caller ASSERTING that `DELETE /cancel-all` is equivalent to
/// cancelling exactly these ids one by one, so it takes BOTH halves: the batch must name every
/// order this start believes is resting (`PolymarketRegistry::covers_all_live`), and every id it
/// named must have RESOLVED to a venue order id. An id that did not resolve means the caller asked
/// for something outside this book — a subset request wearing a whole-book shape — and the arm's
/// unknown-id rejects have already gone out for it. Either half missing ⇒ [`CancelScope::Subset`],
/// which `plan_cancels` can never take to the account-wide arm however much budget is free.
///
/// One process starts exactly one Polymarket exec thread (`vike_tradehub_core`'s `assemble_runtime` calls
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

/// Submits left UNKNOWN (a transport failure after the request may have left the host), coid ->
/// the order's DERIVED CLOB id (`derive_order_id`, live-verified equal to the venue's `orderID`).
/// The exec thread writes it; `ExecutionClient::confirm`'s worker reads it. Only these coids are
/// ever re-queried, so a confirm for an accepted order is a clean no-op.
#[derive(Clone, Default)]
struct UnknownSubmits(Arc<Mutex<HashMap<String, String>>>);

/// The `confirm` body for a stuck order: ask the CLOB for the derived id (`lookup` = `GET
/// /data/order/<id>`). FOUND is read by its `status`: `CANCELED` / `UNMATCHED` / `INVALID` /
/// `CANCELED_MARKET_RESOLVED` are an `OrderAccepted` followed by an `OrderCanceled` (the FSM takes a
/// cancel only from an accepted order), everything else (`LIVE`, `MATCHED`, `DELAYED`, a status we
/// do not know) is the `OrderAccepted` alone, carrying the venue id (a fill, if any, follows on the
/// user channel). Not found, an error or a reply of a shape we cannot verify offline -> NOTHING:
/// "unknown yet" is never a terminal here (the core's watchdog owns the last resort), because a
/// phantom reject of a resting order strands a real position.
fn confirm_unknown_submit(
    unknown: &UnknownSubmits,
    coid: &str,
    ts: i64,
    lookup: impl FnOnce(&str) -> Result<serde_json::Value, String>,
) -> Vec<Event> {
    let Some(clob_id) = unknown.0.lock().unwrap().get(coid).cloned() else {
        return Vec::new();
    };
    let Some(found) = lookup(&clob_id).ok().and_then(|reply| found_order(&reply, &clob_id)) else {
        return Vec::new();
    };
    unknown.0.lock().unwrap().remove(coid);
    let mut events = vec![Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: Some(found.id.into()),
        ts,
    })];
    if matches!(
        found.status.as_str(),
        "CANCELED" | "UNMATCHED" | "INVALID" | "CANCELED_MARKET_RESOLVED"
    ) {
        events.push(Event::OrderCanceled(OrderCanceled {
            client_order_id: coid.to_string(),
            reason: format!("venue reports the order {}", found.status).into(),
            ts,
        }));
    }
    events
}

fn run(
    cfg: PolymarketLiveConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    unknown: UnknownSubmits,
) {
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
        rate_gate,
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
    let boot_ts = vike_model::time::clock::now_secs();
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
        crate::exec_plane::user_data::spawn_polymarket_user_data_with_resync_tracked(
            uc.ws_url,
            creds.clone(),
            uc.markets,
            registry.clone(),
            events.clone(),
            move || {
                // Account-wide REST replay after every reconnect: recovers a fill/cancel that
                // landed inside the gap REGARDLESS of the WS subscribe list. An endpoint failure
                // replays nothing rather than poisoning the lane.
                let trades = crate::exec_plane::exec::get_trades(CLOB_BASE, &resync_creds, limit)
                    .unwrap_or(serde_json::Value::Null);
                let orders = crate::exec_plane::exec::get_orders(CLOB_BASE, &resync_creds, limit)
                    .unwrap_or(serde_json::Value::Null);
                crate::exec_plane::history::map_polymarket_history(
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
                    vike_model::now_ms(),
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
                    // Saturates to 0 before the epoch, so the widening to u128 is exact.
                    vike_model::now_ms() as u128,
                    salt,
                    is_neg_risk,
                    builder_code,
                );
                // Ack-race close (DEFAULT-OFF, `venue.polymarket.presubmit_register` = `1`). The CLOB echoes no
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
                // while this order had no mapping (`crate::exec_plane::pending_events`). It is collected, NOT
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
                // `None` = the submit's fate is UNKNOWN (see the transport-error arm below): no
                // terminal event is sent for it.
                let ev: Option<Event> = match signed {
                    Err(e) => Some(reject(e)),
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
                                rate_gate,
                            )
                        } else {
                            submit_order(CLOB_BASE, &creds, obj, ot, rate_gate)
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
                                    Some(Event::OrderAccepted(OrderAccepted {
                                        client_order_id: req.client_order_id.clone(),
                                        venue_order_id: Some(oid.into()),
                                        ts: req.ts,
                                    }))
                                }
                                Err(reason) => Some(reject(reason)),
                            },
                            // A transport failure AFTER the request may have left this host (a send
                            // timeout or reset, an unreadable or unparseable 2xx body, a gateway 5xx)
                            // says nothing about the order: it may be resting, or filled, at the CLOB.
                            // A terminal `OrderRejected` here would strand it and the position it
                            // opens, so the submit stays UNKNOWN: no terminal event, the order keeps
                            // its pre-ack state. The derived CLOB id (`derive_order_id`, the venue's
                            // own `orderID`) is registered NOW, whatever `presubmit_register` says, so
                            // the user-WS pump and the A3 resync bind this order's events by id, and
                            // it is remembered for `confirm` (`GET /data/order/<id>`), which turns a
                            // found order into the `OrderAccepted` the core is waiting for. If the
                            // confirm finds nothing the core's stuck-order watchdog is the last
                            // resort (a reject after ack_timeout + 2 * grace). A failure that provably
                            // never left this host (DNS/connect/proxy/cert, a local gate, a venue
                            // HTTP refusal) stays a definite reject.
                            Err(e) if submit_error_is_ambiguous(&e) => {
                                tracing::warn!(
                                    venue = "polymarket",
                                    coid = %req.client_order_id,
                                    error = %e,
                                    "submit outcome UNKNOWN (transport failed after send); no \
                                     terminal event - awaiting confirm/user-WS/resync/watchdog"
                                );
                                let clob_id = derive_order_id(&order);
                                if let Some(t) = tracker.as_ref() {
                                    t.register(&req.client_order_id, req.qty);
                                }
                                replay.extend(registry.on_accept(
                                    &req.client_order_id,
                                    &clob_id,
                                    if req.side >= 0 { 1 } else { -1 },
                                ));
                                unknown
                                    .0
                                    .lock()
                                    .unwrap()
                                    .insert(req.client_order_id.clone(), clob_id);
                                None
                            }
                            Err(e) => Some(reject(e)),
                        }
                    }
                };
                // Learn from the terminal BEFORE it leaves: an off-grid refusal re-fetches this
                // token's tick size (ONE `/tick-size` GET, the `/markets` walk only as fallback) so
                // the next order is built on the grid the venue is actually enforcing. Every other
                // outcome — including this arm's own local rejects — costs nothing. Per-order
                // boundary, never per message (the hot-fold rule).
                if let Some(ev) = ev {
                    observe_submit_outcome(tick_grid.as_ref(), &req.symbol, &ev);
                    let _ = events.blocking_send(ev);
                }
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
            // No native amend on this venue (its `AmendSemantics` row is `Unsupported`, which
            // `ExecutionEngine::modify_order` already refuses out loud before a command is queued).
            // This arm is the belt to that brace: a modify that reaches the exec thread by any other
            // door is still answered, with a NON-terminal advisory (the order keeps its terms),
            // never silently dropped.
            ExecCommand::Modify { order, .. } => {
                let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                    client_order_id: order.client_order_id.clone(),
                    reason: "polymarket has no order amend; cancel and re-place".into(),
                    ts: order.ts,
                }));
            }
            ExecCommand::Shutdown => break,
        }
    }
    // Deterministic teardown (the bybit convention): stop + join the user pump AND its resync
    // supervisor before this thread returns, so a window close never strands a WS thread.
    if let Some(feed) = user_feed {
        let _ = feed.shutdown();
    }
}

#[path = "client_tests.rs"]
#[cfg(test)]
mod client_tests;
