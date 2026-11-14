//! Polymarket `ExecutionClient` — the vike-model seam, the shared `ExecutionClientActor` over this venue's exec
//! thread ([`spawn_live`]). A dedicated thread owns the creds, derives L2
//! (ClobAuth L1) on start, and maps each `OrderRequest` → a signed V2 CLOB order → submit → venue
//! events. `OrderRequest.symbol` is the ERC-1155 outcome `token_id`; `qty` is shares; `price` is the
//! probability. The submit response gives Accepted/Rejected; **fills/cancels arrive on the
//! authenticated user WS** — which [`spawn_live`] starts from inside this
//! same exec thread (the bybit/okx convention: one thread owns the pump's lifetime and shuts it down
//! when the command loop ends), so a started engine's return lane exists by construction rather than
//! by a composition root remembering to wire one. NegRisk (the signing domain) is resolved per order
//! through [`NegRiskSource`], which — unlike a bare set — can say "don't know" and reject.
//!
//! The exec thread is also where the **dynamic tick-size regime** ([`crate::instrument::tick_regime`]) is
//! consumed, when a caller threads one onto [`PolymarketLiveConfig::tick_regime`]: each submit's
//! price is snapped onto that token's cached grid ([`grid_price`]) and each terminal reject is fed
//! back to the cache ([`observe_submit_outcome`]), so an off-grid refusal re-fetches the grid instead
//! of repeating forever. A token the cache has not resolved is looked up before its order is signed, and
//! while that lookup fails its order is REFUSED in words and the next order asks again: never a guessed
//! grid (owner, 2026-11-13). `None` ⇒ none of this happens and nothing is built for it.

pub mod convert_arb;
pub mod fill_tracker;
pub mod heartbeat;
pub(crate) mod order;
// The bounded, expiring staging area for a user-channel event whose CLOB order id `registry` cannot
// re-key YET (the ack race) — the fix for an executed fill silently vanishing at `user_websocket`'s three
// re-key sites. Public because `PendingStats` is the observable half of "a silent drop must not
// survive in any form": it is read off the shared registry (`OrderVenueIdTracker::pending_stats`).
pub mod pending_events;
pub(crate) mod registry;
pub(crate) mod rest;
pub(crate) mod resync;
pub mod scoring;
// The loopback HTTP double the REST-plane unit tests dial (l1, onboarding, preflight, client).
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod user_data;

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use vike_bridge_core::eip712::eth_address_from_private_key;
use vike_bridge_core::exec_actor::{
    ConfirmFn, ExecutionClientActor, ExecutionClientActorCancelOutcome,
    ExecutionClientActorCommand, cancel_event, closes_at_once,
};
use vike_bridge_core::execution_client::hooks::BeforeSubmitFn;
use vike_bridge_core::transport::{RestTransport, UreqTransport};
use vike_execution::EventSender;
use vike_model::events::{
    Event, OrderAccepted, OrderCanceled, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRequest, TimeInForce};

use crate::account::preflight::{Preflight, READ_TIMEOUT, loggable};
use crate::catalog::gamma::GammaClient;
use crate::catalog::market_ref::explain_symbol;
use crate::config::PolymarketConfig;
use crate::execution::fill_tracker::FillTracker;
use crate::execution::order::{
    OrderSideWire, SignatureType, build_order, derive_order_id, order_to_json, sign_order,
    sign_order_1271,
};
use crate::execution::pending_events::ParkedEvent;
use crate::execution::registry::OrderVenueIdTracker;
use crate::execution::rest::{
    CancelScope, FoundOrder, cancel_order, cancel_order_relayer, cancel_orders, found_order,
    gate_cancel_shared, get_order, submit_error_is_ambiguous, submit_order, submit_order_relayer,
};
use crate::instrument::tick_regime::TickRegime;
use crate::neg_risk_lookup::NegRiskSource;
use crate::transport::sign::l1::ensure_l2;

/// The live Polymarket exec client: the ONE shared [`ExecutionClientActor`] over this venue's exec thread, with its
/// hooks (no wrapper of its own). `submit`/`cancel` enqueue onto the exec thread (non-blocking). The start
/// hands it the HALT sentinel with [`ExecutionClientActor::with_halt_path`]
/// (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that every live start does).
///
/// The ONE entry point: the exec thread, PLUS (when [`PolymarketLiveConfig::user_channel`] is `Some`) the
/// authenticated user-WS fill pump and its audit-A3 post-reconnect resync, started from inside that same
/// thread and shut down when it ends.
///
/// Why the pump lives here and not in the composition root: the venue-adapter contract says no
/// order may silently vanish, and on this venue every terminal AFTER acceptance (fill, cancel,
/// expiry) arrives ONLY on the user channel. A start that spawned the exec client without the
/// pump would accept orders that could never reach a terminal event — so the two are made
/// inseparable, exactly as bybit/okx do it (their `run` starts the private-WS pump before the
/// command loop and `shutdown()`s it after). `user_channel: None` starts no socket and no extra
/// thread (the caller then owns the return lane).
///
/// The hooks, each the venue's own:
/// - **`ts` becomes the moment the order reaches the client** ([`ExecutionClientActor::with_before_submit`]): it is
///   the clock of [`MAX_SUBMIT_AGE_MS`], and this order's events carry it, where one from the control door
///   used to say 1970.
/// - ⚠ **[`ExecutionClientActor::with_bulk_cancel`]** (hyperliquid's batch-first cancel is the other declaration), because
///   this is the venue with a native account-wide `cancel-all` AND a client-side cancel-token budget that has to choose
///   between it and `n` targeted calls. The declaration is what makes [`ExecutionClientActorCommand::CancelBatch`]
///   reachable at all (the batch arrives whole, with its intent, where
///   [`crate::execution::rest::plan_cancels`] decides), and the exec thread's arm for it is the
///   obligation that comes with declaring one. A single cancel's intent rides the queued command to the
///   thread that owns the cancel-token mirror ([`crate::execution::rest::gate_cancel`]).
/// - **The `confirm` re-query** ([`ExecutionClientActor::with_confirm`]) of the submits left UNKNOWN.
///
/// The hosts are explicit: the exec thread's every REST call (the L2 derive, the wallet read,
/// submit, cancel, the history replay) and the `confirm` re-query go to `clob_base`, its approval
/// relayer to `relayer_base`, and a market NAME on the refusal path is explained from Gamma at
/// `gamma_base`. Production passes the venue's `venue_url` rows (`crate::config`'s `clob_base`,
/// `relayer_base` and `gamma_base`, looked up by the start); an offline test passes a loopback
/// double, which is what lets one test drive the REAL exec thread end to end.
pub(crate) fn spawn_live(
    config: PolymarketLiveConfig,
    events: EventSender,
    clob_base: &str,
    relayer_base: &str,
    gamma_base: &str,
) -> ExecutionClientActor {
    let (clob_base, relayer_base) = (clob_base.to_string(), relayer_base.to_string());
    let gamma = GammaClient { base: gamma_base.to_string() };
    // The submits left UNKNOWN (order client id -> derived CLOB id), shared by the exec thread (writer) and
    // the `confirm` re-query worker (reader).
    let unknown = UnknownSubmits::default();
    let confirm: ConfirmFn = {
        let (unknown, creds) = (unknown.clone(), config.creds.clone());
        let is_1271 = config.signature_type == SignatureType::Polymarket1271;
        let clob = clob_base.clone();
        Arc::new(move |order_client_id: &str| {
            let l2 = || {
                let mut creds = creds.clone();
                ensure_l2(&mut creds, &clob, vike_model::time::clock::now_secs())
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>(creds)
            };
            confirm_unknown_submit(
                &unknown,
                order_client_id,
                vike_model::now_ms(),
                |id| get_order(&clob, &l2()?, id, crate::transport::REST_TIMEOUT),
                |id| cancel_by_id(&clob, &l2()?, is_1271, id),
            )
        })
    };
    let arrived_now: BeforeSubmitFn =
        Arc::new(|request: &mut OrderRequest| request.timestamp = vike_model::now_ms());
    ExecutionClientActor::spawn("polymarket-exec", events.clone(), move |rx| {
        run(config, events, rx, unknown, &clob_base, &relayer_base, &gamma)
    })
    .with_before_submit(arrived_now)
    .with_bulk_cancel()
    .with_confirm(confirm)
}

/// Everything the live exec thread owns. A struct rather than a 9-argument `spawn` (clippy's
/// `too_many_arguments`, and the call site reads as a configuration rather than a positional puzzle).
pub struct PolymarketLiveConfig {
    /// L1 key + (ideally already-derived) L2 trio + relayer identity. If `secret` is empty the exec
    /// thread derives L2 itself via `ensure_l2` — a start that already derived (to build the recon
    /// client from the same handshake) passes them in and costs no second round-trip.
    pub creds: PolymarketConfig,
    /// The funder / deposit-wallet address orders are made for; empty ⇒ the key's own EOA.
    pub maker: String,
    /// `Polymarket1271` for a deposit-wallet account (relayer submit/cancel), `Eoa` for a bare key.
    pub signature_type: SignatureType,
    /// How the per-order EIP-712 signing domain is decided — see [`NegRiskSource`].
    pub neg_risk: NegRiskSource,
    /// The shared CLOB-id ↔ order client id map. Pass the SAME clone to the user pump (done for you when
    /// `user_channel` is `Some`) and to `recon_client` so reconcile reports re-key to local order client ids.
    pub registry: OrderVenueIdTracker,
    /// Optional dust-snap ledger ([`crate::execution::fill_tracker`]).
    pub tracker: Option<FillTracker>,
    /// `Some` ⇒ start the authenticated user-WS pump inside the exec thread. `None` ⇒ don't (the
    /// historical shape; the caller is then responsible for the return lane).
    pub user_channel: Option<UserChannelConfig>,
    /// bytes32 `builderCode` stamped into every signed order for fee attribution (see
    /// [`crate::execution::order::PolymarketOrder::builder`]). `[0u8; 32]` is unattributed — byte-identical
    /// to before this field existed.
    pub builder_code: [u8; 32],
    /// Close the ack-race by pre-registering `order_client_id`↔`derive_order_id(&order)` in the shared
    /// `registry` just BEFORE each submit, so a user-WS fill/cancel that beats the HTTP ack still
    /// re-keys to the local order (done in the exec thread's submit arm; resolved from
    /// `POLY_PRESUBMIT_REGISTER` at the start via [`crate::adapter::presubmit_register_enabled`]).
    /// DEFAULT-OFF (`false`): `false` computes and registers NOTHING pre-submit — byte-identical to
    /// before this field existed, with the ack path ([`OrderVenueIdTracker::on_accept`] after
    /// acceptance) the sole registry writer, exactly as today.
    pub presubmit_register: bool,
    /// Whether the local submit gate REFUSES an over-budget submit (`true`) or only counts it
    /// (`false`, the default) — resolved ONCE at the start by
    /// [`crate::execution::rest::rate_gate_enforced`] from `venue.polymarket.rate_gate`.
    pub rate_gate: bool,
    /// The SHARED dynamic tick-size cache ([`crate::instrument::tick_regime`]) the exec thread prices orders on
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
/// The transport is deliberately the PROXIED one (`crate::transport::agent()`, the same SOCKS egress
/// every other Polymarket REST lane uses): a direct-dialling transport cannot even resolve the CLOB
/// host from a geo-blocked box, so every refresh would fail and the cached grid — correctly, per
/// [`TickRegime::refresh`] — would simply never move.
struct TickGrid<T: RestTransport> {
    regime: TickRegime,
    transport: T,
    /// The CLOB base the re-fetch asks, the exec thread's own.
    clob: String,
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

/// **Why an order on `token_id` may not be signed yet**: a regime is threaded, and the token's tick is
/// neither cached nor resolvable now ([`TickRegime::refresh`]: the `/tick-size` lookup, then the
/// `/markets` walk, cached once it answers). The words are the shared "properties not loaded" refusal; the
/// next order on the token asks again. `None`: no regime (inert), or the tick is known. Never a guessed
/// grid (owner, 2026-11-13).
fn tick_unknown<T: RestTransport>(grid: Option<&TickGrid<T>>, token_id: &str) -> Option<String> {
    let g = grid?;
    if g.regime.tick_size(token_id).is_some()
        || g.regime.refresh(&g.transport, &g.clob, token_id).is_some()
    {
        return None;
    }
    Some(vike_bridge_core::venue_adapter::properties_not_loaded(
        crate::feed::VENUE,
        token_id,
        "neither its /tick-size lookup nor the /markets walk resolved its tick",
    ))
}

/// Teach the tick regime from a submit's TERMINAL event. An [`Event::OrderRejected`] whose reason is
/// an off-grid refusal ([`crate::is_tick_size_reject`], the pure decision) re-fetches and caches this
/// token's tick size; the resolved tick is returned for the tests' benefit — the exec thread wants
/// only the caching side effect.
///
/// Everything else is a no-op that costs NO REST round-trip: no regime, an accepted order, a
/// balance/allowance refusal, a local signing failure, or a transport error. A refresh that resolves
/// nothing leaves the previously cached grid untouched (that contract lives in
/// [`TickRegime::refresh`]).
fn observe_submit_outcome<T: RestTransport>(
    grid: Option<&TickGrid<T>>,
    token_id: &str,
    event: &Event,
) -> Option<f64> {
    let g = grid?;
    let Event::OrderRejected(r) = event else {
        return None;
    };
    g.regime.on_reject(&g.transport, &g.clob, token_id, &r.reason)
}

/// The authenticated user-channel pump's configuration (see [`PolymarketLiveConfig::user_channel`]).
pub struct UserChannelConfig {
    /// The user channel's full URL: the venue's row plus `/ws/user` (`crate::config`'s `user_websocket_url`).
    pub websocket_url: String,
    /// The CLOB **condition ids** (markets) to subscribe to — NOT token ids. See
    /// [`crate::adapter::polymarket_execution_markets`] for where a start gets them and what an empty list means.
    pub markets: Vec<String>,
    /// Rows of `/data/trades` + `/data/orders` the post-reconnect A3 resync replays (the start passes
    /// [`DEFAULT_HISTORY_LIMIT`]).
    pub history_limit: u32,
}

/// Rows the A3 resync replays per endpoint after a user-WS reconnect: polymarket keeps its own 100, not the
/// shared `vike_bridge_core::websocket_session::user::RESYNC_HISTORY_LIMIT` of 50 (owner, 2026-11-11). Fewer rows can only miss
/// more fills of a busy gap, and a fill the replay misses stays a divergence the `quarantine` reconcile reports and
/// does not fold; one page of 100 costs the same request as one of 50.
pub(crate) const DEFAULT_HISTORY_LIMIT: u32 = 100;

/// Decode a 0x-prefixed 32-byte hex builder code; `None` (→ zero) on any malformed value so a bad
/// `.env` entry degrades to unattributed rather than failing the start. Shared by
/// [`crate::adapter::live_runtime_for_account`] (the production resolve) and the live-money
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

/// This venue's row of the ONE cross-venue TIF authority ([`vike_venues::venue_time_in_force::venue_time_in_force`]),
/// consumed: `Ioc` is COERCED to `"FOK"` — the OPPOSITE direction of hyperliquid's `Fok`→`Ioc`
/// fold (same pair!); step 2 resolves that deliberately, behind demo smokes.
/// GTD's wire `expiration` is resolved by the sibling [`expiration_secs_of`]; this function only
/// picks the `orderType` string.
fn order_type_of(tif: TimeInForce) -> &'static str {
    // `wire()` is Some for every polymarket row but `PostOnly` (`Unsupported`: the CLOB has the flag but
    // no proof exists, no order ever being placed there) — and the core preflight refuses that one by
    // name before it reaches here, so the fallback is unreachable, kept so the exec thread can never panic.
    vike_venues::venue_time_in_force::venue_time_in_force(crate::feed::VENUE, tif)
        .wire()
        .unwrap_or("GTC")
}

/// The CLOB's minimum GTD lifetime: a limit order whose `expiration` is nearer than this is
/// refused by the venue. `180`s as of `@polymarket/client` 0.1.0-beta.12 (earlier builds used 60s).
///
/// ⚠ TRANSCRIBED from the SDK constant recorded in [`crate::execution::order`]'s module doc, **not**
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

/// How long a submit may wait on the exec thread's queue and still be signed.
///
/// ⚠ **One thread serves that queue and its calls block**, so a slow or dead tunnel makes the orders
/// behind the one in hand wait. The core gives up on an unanswered order at its last-resort reject
/// (`crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`), and `confirm` cannot
/// save an order that was never sent. Signed after that, the order rests at the venue under an id the
/// core has rejected, its accept is dropped as an illegal transition, and nothing tracks it.
///
/// Why 30 s: the core's reject comes first at the ack timeout plus two graces, 60 s at the shipped
/// defaults, and a submit let through at this age still has `crate::transport::REST_TIMEOUT` for its
/// POST, so its answer is in by then (`the_slowest_submit_is_answered_before_the_core_can_reject_it`
/// pins the sum). A run profile that lowers those guards shortens the core's side, and this bridge
/// cannot read it.
///
/// ⚠ **The clock starts when the order reaches this client's `ExecutionClient::submit`, never at the
/// `ts` it arrived with.** That field states no creation time: an order from the command line or the
/// control door carries 0 and a strategy may stamp the trade that moved it, while the core counts
/// from its own registration (`crates/vike-execution/src/execution_engine/mod.rs`'s `gate_and_register`)
/// on the same wall clock. The two starts differ by the trade lane's transit, which the sum above
/// does not cover: milliseconds on one box. A lost link restarts the core's count
/// (`crates/vike-core/src/runtime/execution_link.rs`'s `execution_link_resumes`) and re-sends no written order.
const MAX_SUBMIT_AGE_MS: i64 = 30_000;

/// The words of the terminal reject of an order that reached this client at `arrived_ms` and is too
/// old to sign at `now_ms`, or `None` while it is not.
fn too_old_to_sign(arrived_ms: i64, now_ms: i64) -> Option<String> {
    let age_ms = now_ms.saturating_sub(arrived_ms);
    (age_ms > MAX_SUBMIT_AGE_MS).then(|| {
        format!(
            "polymarket: this order was NOT sent. It waited {:.1} s behind earlier calls to the venue \
             (is the proxy slow or down?), past the {} s an order may wait, so nothing was signed. \
             Send it again if it is still wanted.",
            age_ms as f64 / 1000.0,
            MAX_SUBMIT_AGE_MS / 1000
        )
    })
}

/// Classify a CLOB submit response body. `Ok(orderID)` = accepted; `Err(reason)` = rejected.
/// A 2xx body can still carry `success:false` (e.g. balance/allowance) — never blind-accept.
fn accept_outcome(response: &serde_json::Value) -> Result<String, String> {
    if response.get("success").and_then(|s| s.as_bool()) == Some(false) {
        let msg = response.get("errorMsg").and_then(|m| m.as_str()).unwrap_or("order rejected");
        return Err(msg.to_string());
    }
    match response.get("orderID").and_then(|o| o.as_str()).filter(|s| !s.is_empty()) {
        Some(id) => Ok(id.to_string()),
        None => Err(response
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

/// Emit the user-channel events that were staged while this order had no CLOB→order client id mapping.
///
/// This is the other half of the fix in [`crate::execution::pending_events`]: the exec thread is the ONLY
/// place that learns "this venue id is ours", so it is the only place a staged event can be
/// released promptly and exactly. Frame-driven release would be at the mercy of the next inbound
/// frame — fine while the account is busy, a silent TTL loss when it is not.
///
/// Called AFTER this order's own terminal event has been sent, never before — the caller's comment
/// at the flush site explains why the FSM makes that ordering load-bearing.
fn emit_replayed(
    parked: &[ParkedEvent],
    registry: &OrderVenueIdTracker,
    tracker: Option<&FillTracker>,
    events: &EventSender,
) {
    if parked.is_empty() {
        return; // the overwhelmingly common path: nothing raced, nothing staged
    }
    for event in crate::execution::user_data::decode::replay_parked(parked, registry, tracker) {
        let _ = events.blocking_send(event);
    }
}

/// The [`CancelScope`] one batch is planned under — the safety decision of the whole bulk lane,
/// pulled out of the arm because it is the ONE thing that decides whether an ACCOUNT-WIDE call can
/// happen at all.
///
/// [`CancelScope::WholeBook`] is the caller ASSERTING that `DELETE /cancel-all` is equivalent to
/// cancelling exactly these ids one by one, so it takes BOTH halves: the batch must name every
/// order this start believes is resting (`OrderVenueIdTracker::covers_all_live`), and every id it
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

/// Submits left UNKNOWN (a transport failure after the request may have left the host), order client id ->
/// the order's DERIVED CLOB id (`derive_order_id`, live-verified equal to the venue's `orderID`) and
/// when the order reached this client ([`MAX_SUBMIT_AGE_MS`]'s clock).
/// The exec thread writes it and reads it once; `ExecutionClient::confirm`'s worker reads it. Only
/// these order client ids are ever re-queried, so a confirm for an accepted order is a clean no-op.
#[derive(Clone, Default)]
struct UnknownSubmits(Arc<Mutex<HashMap<String, (String, i64)>>>);

/// How old an UNKNOWN submit may be when it is found at the venue and still be reported as accepted.
///
/// ⚠ **The core takes an accept only from `Submitted`** (`crates/vike-model/src/orders/record.rs`'s
/// `transition`). After its last-resort reject the accept is dropped and only counted
/// (`crates/vike-execution/src/execution_engine/fold.rs`'s `stranded_terminal_drops`), and no wire tells this
/// bridge that the reject fired. So past this age no event can report a resting order truthfully, and
/// [`confirm_unknown_submit`] cancels it at the venue instead.
///
/// Why 55 s: the core's earliest reject (60 s at the shipped defaults, [`MAX_SUBMIT_AGE_MS`]'s doc)
/// less 5 s for the trade lane and this thread. A find just inside the core's patience is canceled too:
/// that costs one order, where the other mistake leaves live money that nothing tracks.
const LATEST_ACCEPT_AGE_MS: i64 = 55_000;

/// One order's cancel at the venue, by the route its wallet type takes.
fn cancel_by_id(
    base: &str,
    creds: &PolymarketConfig,
    is_1271: bool,
    order_id: &str,
) -> Result<serde_json::Value, String> {
    if is_1271 {
        cancel_order_relayer(
            base,
            creds,
            order_id,
            creds.relayer_key.expose(),
            &creds.relayer_address,
        )
    } else {
        cancel_order(base, creds, order_id)
    }
}

/// The read of a submit left UNKNOWN: ask the CLOB for the derived id (`lookup` = `GET
/// /data/order/<id>`). The exec thread asks once as soon as the POST fails, because the core's one
/// `confirm` can come while that POST is still in flight, and `confirm`'s worker asks when the core
/// does. FOUND is read by its `status`: `CANCELED` / `UNMATCHED` / `INVALID` /
/// `CANCELED_MARKET_RESOLVED` are an `OrderAccepted` followed by an `OrderCanceled` (the FSM takes a
/// cancel only from an accepted order), everything else (`LIVE`, `MATCHED`, `DELAYED`, a status we
/// do not know) is the `OrderAccepted` alone, carrying the venue id (a fill, if any, follows on the
/// user channel). Found at or past [`LATEST_ACCEPT_AGE_MS`], it is [`settle_late_find`]'s instead.
/// Not found, an error or a reply of a shape we cannot verify offline -> NOTHING:
/// "unknown yet" is never a terminal here (the core's watchdog owns the last resort), because a
/// phantom reject of a resting order strands a real position.
fn confirm_unknown_submit(
    unknown: &UnknownSubmits,
    order_client_id: &str,
    ts: i64,
    lookup: impl FnOnce(&str) -> Result<serde_json::Value, String>,
    cancel: impl FnOnce(&str) -> Result<serde_json::Value, String>,
) -> Vec<Event> {
    let Some((clob_id, arrived_ms)) = unknown.0.lock().unwrap().get(order_client_id).cloned()
    else {
        return Vec::new();
    };
    let Some(found) = lookup(&clob_id).ok().and_then(|reply| found_order(&reply, &clob_id)) else {
        return Vec::new();
    };
    // Whoever takes the row answers: the exec thread and the worker can both be holding this find.
    if unknown.0.lock().unwrap().remove(order_client_id).is_none() {
        return Vec::new();
    }
    let closed = matches!(
        found.status.as_str(),
        "CANCELED" | "UNMATCHED" | "INVALID" | "CANCELED_MARKET_RESOLVED"
    );
    // Judged after the read, which is what spent the time.
    let age_ms = vike_model::now_ms().saturating_sub(arrived_ms);
    if age_ms >= LATEST_ACCEPT_AGE_MS {
        return settle_late_find(order_client_id, &found, closed, age_ms, ts, cancel);
    }
    let mut events = vec![Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: Some(found.id.into()),
        timestamp: ts,
    })];
    if closed {
        events.push(Event::OrderCanceled(OrderCanceled {
            order_client_id: order_client_id.to_string(),
            reason: format!("venue reports the order {}", found.status).into(),
            timestamp: ts,
        }));
    }
    events
}

/// An UNKNOWN submit found too late to report ([`LATEST_ACCEPT_AGE_MS`]): nothing tracks it, so one
/// still open is canceled at the venue by its id, and the log says so at `error` either way.
///
/// The reject after a cancel that worked is true whichever state the core is in: it is the reason an
/// operator reads if the core was still waiting, and a dropped repeat if the core already rejected.
/// A cancel that failed sends nothing: the order may be open, and the core's watchdog owns its terminal.
fn settle_late_find(
    order_client_id: &str,
    found: &FoundOrder,
    closed: bool,
    age_ms: i64,
    ts: i64,
    cancel: impl FnOnce(&str) -> Result<serde_json::Value, String>,
) -> Vec<Event> {
    let (id, status) = (found.id.as_str(), found.status.as_str());
    if closed {
        tracing::warn!(
            venue = "polymarket",
            order_client_id,
            venue_order_id = id,
            status,
            age_ms,
            "a timed-out order was found already closed at the venue, too late to report: nothing \
             rests, nothing to do"
        );
        return Vec::new();
    }
    match cancel(id) {
        Ok(_) => {
            tracing::error!(
                venue = "polymarket",
                order_client_id,
                venue_order_id = id,
                status,
                age_ms,
                "a timed-out order was found open at the venue after the core may have rejected it, \
                 so nothing tracked it: CANCELED AT THE VENUE by its id"
            );
            vec![Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: order_client_id.to_string(),
                reason: format!(
                    "polymarket: this order's POST timed out and the venue held it as {id} {:.1} s \
                     after it was placed, too late to report it as accepted. It was canceled at the \
                     venue; any fill it took before that stands.",
                    age_ms as f64 / 1000.0
                )
                .into(),
                timestamp: ts,
            })]
        }
        Err(e) => {
            tracing::error!(
                venue = "polymarket",
                order_client_id,
                venue_order_id = id,
                status,
                age_ms,
                error = %loggable(&e),
                "a timed-out order was found open at the venue after the core may have rejected it, \
                 and it could NOT be canceled: it may still rest there with nothing tracking it. \
                 CANCEL IT BY HAND"
            );
            Vec::new()
        }
    }
}

fn run(
    config: PolymarketLiveConfig,
    events: EventSender,
    rx: Receiver<ExecutionClientActorCommand>,
    unknown: UnknownSubmits,
    clob_base: &str,
    relayer_base: &str,
    gamma: &GammaClient,
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
    } = config;
    // signer = the EOA from the L1 key; derive L2 creds if not supplied (ClobAuth L1).
    //
    // Both early returns below end the exec thread WITHOUT emitting anything — deliberately, and
    // NOT a silent vanish: `ExecutionClientActor` detects a dead command thread and synthesizes the terminal
    // `OrderRejected` itself (see its `submit` / `on_send_error`), so an order submitted into a
    // credential-less client still reaches exactly one terminal event.
    let signer = match eth_address_from_private_key(creds.private_key.expose()) {
        Ok(a) => a,
        Err(_) => return, // no key → no session (live gate)
    };
    let boot_ts = vike_model::time::clock::now_secs();
    if ensure_l2(&mut creds, clob_base, boot_ts).is_err() {
        return; // cannot obtain L2 creds → no orders
    }
    // The user-channel pump + A3 resync, owned by THIS thread (see `spawn_live`'s doc). Started
    // BEFORE the command loop so the return lane is live before the first order can be accepted.
    let user_feed = user_channel.map(|uc| {
        let resync_credentials = creds.clone();
        let resync_reg = registry.clone();
        let resync_base = clob_base.to_string();
        let limit = uc.history_limit;
        tracing::info!(
            target: "vike_polymarket::client",
            markets = uc.markets.len(),
            "starting the Polymarket user-channel pump (fills/cancels return lane)"
        );
        crate::execution::user_data::spawn_polymarket_user_data_with_resync_tracked(
            uc.websocket_url,
            creds.clone(),
            uc.markets,
            registry.clone(),
            events.clone(),
            move || {
                // Account-wide REST replay after every reconnect: recovers a fill/cancel that
                // landed inside the gap REGARDLESS of the WS subscribe list. An endpoint failure
                // replays nothing rather than poisoning the lane.
                let trades =
                    crate::execution::rest::get_trades(&resync_base, &resync_credentials, limit)
                        .unwrap_or(serde_json::Value::Null);
                let orders =
                    crate::execution::rest::get_orders(&resync_base, &resync_credentials, limit)
                        .unwrap_or(serde_json::Value::Null);
                crate::execution::resync::map_polymarket_history(
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
        transport: UreqTransport::with_agent(crate::feed::VENUE, crate::transport::agent()),
        clob: clob_base.to_string(),
    });
    let maker = if maker.is_empty() { signer.clone() } else { maker };
    let is_1271 = sig_type == SignatureType::Polymarket1271;
    // POLY_1271 (deposit wallet): the order's `signer` field is the maker (deposit wallet) itself and
    // orders route through the relayer; the EOA key still does the actual signing. Else signer = EOA.
    let order_signer = if is_1271 { maker.clone() } else { signer.clone() };
    let mut salt: u128 = 1;
    // The pre-order READ of the wallet's balance and allowance (`crate::account::preflight`): owns its own
    // cache and the once-per-process approval state, so it lives and dies with this thread.
    let mut preflight = Preflight::new(clob_base, relayer_base, sig_type);

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecutionClientActorCommand::Submit(req, _) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: req.timestamp,
                }));
                salt = salt.wrapping_add(1);
                let side = if req.side >= 0 { OrderSideWire::Buy } else { OrderSideWire::Sell };
                let reject = |reason: String| {
                    Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: req.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: req.timestamp,
                    })
                };
                // Every order here is a signed limit at its price, whatever its kind says, so a MARKET
                // order that names a price would rest or fill at that number while the node's ceilings
                // size a market order at the mark. A market order with NO price (a sell at 0.0: the shape
                // `Flatten` and `MarketExit` rely on) is what this client means by market.
                if req.order_type == vike_model::OrderType::Market && req.price.is_some() {
                    let _ = events.blocking_send(reject(
                        "polymarket: a market order takes no price (every order here is a signed \
                         limit at its price, so it would rest or fill at that number): send a limit \
                         order, or drop the price"
                            .to_string(),
                    ));
                    continue;
                }
                // Too old to send: refused before it costs a read. A cancel has no such arm: a late
                // cancel is safe and a dropped one is not, however long it waited. A close has none either:
                // left unsent it leaves the position open, and a sale spends only what the wallet holds.
                let closes = closes_at_once(&req);
                if let Some(words) =
                    too_old_to_sign(req.timestamp, vike_model::now_ms()).filter(|_| !closes)
                {
                    let _ = events.blocking_send(reject(words));
                    continue;
                }
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
                    // A market NAME typed where a token id belongs (a slug, a polymarket.com
                    // address) is explained with the ids of its outcomes. It is NOT translated: an
                    // order submitted under the name would book its fills under the token id, one
                    // position with two names (`crate::catalog::market_ref`'s "One name"). Anything that is
                    // not name-shaped keeps the NegRisk reason, with no extra read.
                    let reason = explain_symbol(&req.symbol, gamma).unwrap_or_else(|| {
                        format!(
                            "cannot resolve the NegRisk signing domain for token {}",
                            req.symbol
                        )
                    });
                    let _ = events.blocking_send(reject(reason));
                    continue;
                };
                // The token's tick: never signed on a guessed grid ([`tick_unknown`]).
                if let Some(why) = tick_unknown(tick_grid.as_ref(), &req.symbol) {
                    let _ = events.blocking_send(reject(why));
                    continue;
                }
                // The price this order is SIGNED with: snapped onto the tick grid this token was last
                // known to trade on, so a maker quoting off a grid the venue has since moved is
                // corrected instead of refused forever. Inert without a regime — see `grid_price`.
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
                // READ the wallet before anything is signed or sent: a BUY the wallet cannot pay for
                // and a market whose exchange it never approved are refused in words that name what
                // is missing (`crate::account::preflight`), not by the venue's bare "not enough balance /
                // allowance" after a signed order has spent a submit token. An unreadable wallet is
                // no evidence, so it only ever refuses on a positive, parsed shortfall.
                if let Err(words) = preflight.check(&creds, &order) {
                    let _ = events.blocking_send(reject(words));
                    continue;
                }
                // Asked again: the two reads above can spend longer than the order had left, and it is
                // the moment of signing that `MAX_SUBMIT_AGE_MS` bounds.
                if let Some(words) =
                    too_old_to_sign(req.timestamp, vike_model::now_ms()).filter(|_| !closes)
                {
                    let _ = events.blocking_send(reject(words));
                    continue;
                }
                // Ack-race close (DEFAULT-OFF, `venue.polymarket.presubmit_register` = `1`). The CLOB echoes no
                // client id and keys every order by its EIP-712 order hash, and
                // `derive_order_id(&order)` is LIVE-VERIFIED equal to the server's `orderID` (see
                // `live_place_and_cancel`). Registering `order_client_id`↔derived-id HERE — after the order is
                // fully built (so the hash is fixed) but BEFORE the network submit — lets a user-WS
                // fill/cancel that BEATS the HTTP ack still re-key to this order, instead of landing
                // on the `order_client_id: None` path. The real `OrderAccepted` below calls
                // `on_accept` again with the SAME `(order client id, id, side)` → a true no-op by the registry's
                // idempotency contract (see `OrderVenueIdTracker::on_accept`). OFF ⇒ this block does
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
                // while this order had no mapping (`crate::execution::pending_events`). It is collected, NOT
                // emitted yet — see `replay` below.
                let mut replay: Vec<ParkedEvent> = Vec::new();
                if presubmit_register {
                    replay = registry.on_accept(
                        &req.order_client_id,
                        &derive_order_id(&order),
                        if req.side >= 0 { 1 } else { -1 },
                    );
                }
                // deposit-wallet (POLY_1271) signs the ERC-1271 wrapper + submits via the relayer;
                // else a plain V2 signature + submit.
                let signed = if is_1271 {
                    sign_order_1271(&order, creds.private_key.expose())
                } else {
                    sign_order(&order, creds.private_key.expose())
                };
                // `None` = the submit's fate is UNKNOWN (see the transport-error arm below): no
                // terminal event is sent for it.
                let event: Option<Event> = match signed {
                    Err(e) => Some(reject(e)),
                    Ok(sig) => {
                        let obj = order_to_json(&order, &sig, expiration);
                        let ot = order_type_of(req.time_in_force);
                        let submitted = if is_1271 {
                            submit_order_relayer(
                                clob_base,
                                &creds,
                                obj,
                                ot,
                                creds.relayer_key.expose(),
                                &creds.relayer_address,
                                rate_gate,
                            )
                        } else {
                            submit_order(clob_base, &creds, obj, ot, rate_gate)
                        };
                        match submitted {
                            Ok(response) => match accept_outcome(&response) {
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
                                        t.register(&req.order_client_id, req.qty);
                                    }
                                    // THE ack race, closed. Registering this id is the instant a
                                    // user-WS event that arrived first stops being unattributable,
                                    // and `on_accept` hands those staged frames straight back under
                                    // the same lock. Collected, not sent here: they must go out
                                    // AFTER this `OrderAccepted` (see `replay` at the bottom).
                                    replay.extend(registry.on_accept(
                                        &req.order_client_id,
                                        &oid,
                                        side,
                                    ));
                                    Some(Event::OrderAccepted(OrderAccepted {
                                        order_client_id: req.order_client_id.clone(),
                                        venue_order_id: Some(oid.into()),
                                        timestamp: req.timestamp,
                                    }))
                                }
                                Err(reason) => Some(reject(reason)),
                            },
                            // A transport failure AFTER the request may have left this host (a send
                            // timeout or reset, an unreadable or unparseable 2xx body, any 5xx)
                            // says nothing about the order: it may be resting, or filled, at the CLOB.
                            // A terminal `OrderRejected` here would strand it and the position it
                            // opens, so the submit stays UNKNOWN: no terminal event, the order keeps
                            // its pre-ack state. The derived CLOB id (`derive_order_id`, the venue's
                            // own `orderID`) is registered NOW, whatever `presubmit_register` says, so
                            // the user-WS pump and the A3 resync bind this order's events by id, and
                            // it is remembered for `confirm` (`GET /data/order/<id>`), which turns a
                            // found order into the `OrderAccepted` the core is waiting for. This thread
                            // asks once itself, under `READ_TIMEOUT`: the core's one confirm can come
                            // while this POST is in flight, and would then find no row and never ask
                            // again. If nothing finds it the core's stuck-order watchdog is the last
                            // resort (a reject after ack_timeout + 2 * grace). A failure that provably
                            // never left this host (DNS/connect/proxy/cert, a local gate, a venue
                            // HTTP refusal) stays a definite reject.
                            Err(e) if submit_error_is_ambiguous(&e) => {
                                tracing::warn!(
                                    venue = "polymarket",
                                    order_client_id = %req.order_client_id,
                                    error = %e,
                                    "submit outcome UNKNOWN (transport failed after send); no \
                                     terminal event - awaiting confirm/user-WS/resync/watchdog"
                                );
                                let clob_id = derive_order_id(&order);
                                if let Some(t) = tracker.as_ref() {
                                    t.register(&req.order_client_id, req.qty);
                                }
                                // Nothing said yet: a fill that arrives from here on, the read below included,
                                // accepts it first (`user_websocket`). Ahead of `on_accept`, which is what lets one resolve.
                                registry.note_silent(&req.order_client_id);
                                replay.extend(registry.on_accept(
                                    &req.order_client_id,
                                    &clob_id,
                                    if req.side >= 0 { 1 } else { -1 },
                                ));
                                unknown
                                    .0
                                    .lock()
                                    .unwrap()
                                    .insert(req.order_client_id.clone(), (clob_id, req.timestamp));
                                let found = confirm_unknown_submit(
                                    &unknown,
                                    &req.order_client_id,
                                    req.timestamp,
                                    |id| get_order(clob_base, &creds, id, READ_TIMEOUT),
                                    |id| cancel_by_id(clob_base, &creds, is_1271, id),
                                );
                                if !found.is_empty() {
                                    let _ = registry.take_silent(&req.order_client_id);
                                }
                                for event in found {
                                    let _ = events.blocking_send(event);
                                }
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
                if let Some(event) = event {
                    observe_submit_outcome(tick_grid.as_ref(), &req.symbol, &event);
                    let _ = events.blocking_send(event);
                }
                // …and only NOW the staged user-channel events, strictly AFTER this order's own
                // terminal. Ordering is load-bearing, not cosmetic: `vike_execution`'s `OrderRecord`
                // admits `OrderPartiallyFilled` only from Accepted/Triggered/PartiallyFilled, so
                // replaying a raced fill before the `OrderAccepted` would have the FSM drop the
                // wrapper as an invalid transition and skip `accumulate_fill` — recovering the
                // money (the bare `Fill` folds `Account` regardless) while leaving the order's own
                // filled qty short. On the reject arms this still emits: a rejected submit may
                // nonetheless have reached the venue and matched, which is exactly why the
                // pre-registered mapping is deliberately left in place above.
                emit_replayed(&replay, &registry, tracker.as_ref(), &events);
            }
            ExecutionClientActorCommand::Cancel { order_client_id, intent } => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                let outcome = match registry.order_client_id_to_clob(&order_client_id) {
                    // THE RESERVE FLOOR, and this is the ONLY place it is armed. The venue meters
                    // cancels per signer, so a maker that spends its whole bucket on requote churn
                    // is cancel-LOCKED exactly when it needs to pull a book. `gate_cancel_shared`
                    // therefore sheds an `OrderCancelIntent::Routine` cancel while the bucket sits at or
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
                                order_client_id = %order_client_id,
                                reason = %reason,
                                "routine cancel shed to protect the flatten reserve — the order is \
                                 still resting; re-offer it"
                            );
                            ExecutionClientActorCancelOutcome::Rejected(reason)
                        }
                        Ok(()) => match cancel_by_id(clob_base, &creds, is_1271, &oid) {
                            Ok(_) => ExecutionClientActorCancelOutcome::Canceled,
                            Err(e) => ExecutionClientActorCancelOutcome::Rejected(e),
                        },
                    },
                    None => ExecutionClientActorCancelOutcome::Rejected(format!(
                        "no venue order id for {order_client_id}"
                    )),
                };
                if matches!(outcome, ExecutionClientActorCancelOutcome::Canceled) {
                    registry.remove(&order_client_id);
                    if let Some(t) = tracker.as_ref() {
                        t.remove(&order_client_id);
                    }
                }
                let _ = events.blocking_send(cancel_event(&order_client_id, outcome));
            }
            ExecutionClientActorCommand::CancelBatch { order_client_ids, intent } => {
                // THE BULK LANE — the whole batch, still whole, on the thread that owns the
                // cancel-token mirror. `plan_cancels` (inside `cancel_orders`) makes BOTH decisions
                // here and nowhere else: account-wide `cancel-all` (one round trip, `1 + n` tokens)
                // versus `n` targeted calls (`n` round trips, `n` tokens), and — for a
                // `OrderCancelIntent::Routine` batch only — how many of the ids the reserve floor lets
                // through. That is the trade an emergency flatten wants: twenty resting orders is
                // ~2s of serial HTTP, and paying one extra token to make it one hop is exactly what
                // the reserve is being protected FOR.
                //
                // Asked BEFORE anything is resolved or sent: `covers_all_live` is a question about
                // the registry as it stands, and `cancel_orders` will `remove` from it below.
                let whole_book = registry.covers_all_live(&order_client_ids);
                // Resolve each order client id to its venue order id, keeping the pairing so the venue's
                // per-id outcome can be reported back under the order client id the core FSM keys on. An
                // unknown order client id is refused with the SAME wording the single-cancel arm uses — one
                // batched cancel must not look different from one lone cancel of the same order.
                let mut resolved: Vec<(String, String)> =
                    Vec::with_capacity(order_client_ids.len());
                for order_client_id in &order_client_ids {
                    match registry.order_client_id_to_clob(order_client_id) {
                        Some(oid) => resolved.push((order_client_id.clone(), oid)),
                        None => {
                            let reason = format!("no venue order id for {order_client_id}");
                            let event = cancel_event(
                                order_client_id,
                                ExecutionClientActorCancelOutcome::Rejected(reason),
                            );
                            let _ = events.blocking_send(event);
                        }
                    }
                }
                if resolved.is_empty() {
                    continue;
                }
                let scope = batch_scope(whole_book, resolved.len(), order_client_ids.len());
                let order_ids: Vec<String> = resolved.iter().map(|(_, oid)| oid.clone()).collect();
                let relayer = is_1271.then_some((
                    creds.relayer_key.expose().as_str(),
                    creds.relayer_address.as_str(),
                ));
                let batch = cancel_orders(clob_base, &creds, &order_ids, scope, intent, relayer);
                // The bulk arm cannot attribute a failure to one order — one call covered them all
                // — so `cancel_orders` reports it under `"*"` and it applies to every id sent.
                let bulk_err =
                    batch.errors.iter().find(|(id, _)| id == "*").map(|(_, why)| why.clone());
                for (i, (order_client_id, oid)) in resolved.iter().enumerate() {
                    let outcome = if i >= batch.plan.send {
                        // Held back by the reserve floor — only ever a `Routine` batch, and
                        // reported exactly like a shed single cancel: NON-terminal, because the
                        // order is STILL RESTING. Anything else would be the silent vanish.
                        ExecutionClientActorCancelOutcome::Rejected(format!(
                            "routine cancel shed to protect the flatten reserve (retry in {}ms)",
                            batch.plan.retry_ms
                        ))
                    } else {
                        let per_id =
                            batch.errors.iter().find(|(id, _)| id == oid).map(|(_, w)| w.clone());
                        match bulk_err.clone().or(per_id) {
                            Some(reason) => ExecutionClientActorCancelOutcome::Rejected(reason),
                            None => ExecutionClientActorCancelOutcome::Canceled,
                        }
                    };
                    // Same teardown as the single-cancel arm, per confirmed id: the order client id→clob row
                    // goes (nothing may cancel a dead order twice) while the clob→order client id row is
                    // DEMOTED, so a match that raced this cancel still re-keys.
                    if matches!(outcome, ExecutionClientActorCancelOutcome::Canceled) {
                        registry.remove(order_client_id);
                        if let Some(t) = tracker.as_ref() {
                            t.remove(order_client_id);
                        }
                    }
                    let _ = events.blocking_send(cancel_event(order_client_id, outcome));
                }
            }
            // No native amend on this venue (its `AmendSemantics` row is `Unsupported`, which
            // `Engine::modify_order` already refuses out loud before a command is queued).
            // This arm is the belt to that brace: a modify that reaches the exec thread by any other
            // door is still answered, with a NON-terminal advisory (the order keeps its terms),
            // never silently dropped.
            ExecutionClientActorCommand::Modify { order, .. } => {
                let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                    order_client_id: order.order_client_id.clone(),
                    reason: "polymarket has no order amend; cancel and re-place".into(),
                    timestamp: vike_bridge_core::rest::amend_answer_time(None),
                }));
            }
            // Unreachable: this venue declares no bulk submit and no queued adopt (`ExecutionClientActor` fans a batch out).
            ExecutionClientActorCommand::SubmitBatch(requests, _) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(&events, &requests)
            }
            ExecutionClientActorCommand::Adopt(order) => {
                vike_bridge_core::exec_actor::adopt_undeclared(&order)
            }
            ExecutionClientActorCommand::Shutdown => break,
        }
    }
    // Deterministic teardown (the bybit convention): stop + join the user pump AND its resync
    // supervisor before this thread returns, so a window close never strands a WS thread.
    if let Some(feed) = user_feed {
        let _ = feed.shutdown();
    }
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
