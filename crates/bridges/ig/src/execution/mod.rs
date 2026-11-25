//! IG exec half — `ExecutionClient` over positions + working orders.
//!
//! Order I/O runs on ONE dedicated thread owning a logged-in [`IgSession`] (blocking REST off the
//! single-writer core). Each submit is followed by `GET /confirms/{dealReference}` to resolve
//! accept/fill/reject. An OPEN sends a `dealReference` derived from its order client id (`deal_reference`) and records it
//! before the POST, so a create whose reply is lost is asked about through the same `/confirms` ladder
//! (`VenueApiHttpError::outcome_unknown`), never rejected: an echo-confirmed reply resolves it, an exhausted
//! ladder leaves it `Submitted` for the stuck-order watchdog (`submit_outcome`). A failure PROVABLY before
//! the write (`vike_transport::send_outcome::send_error_is_definite`) is a clean `OrderRejected`. `order_client_id → dealId` is tracked (an `OrderVenueIdRegistry<String>`, `spawn_sharing`'s `deal_ids`, shared with the confirm
//! watchdog and the reconcile client) so cancel can `DELETE /workingorders/otc/{dealId}` and
//! modify can `PUT` it. Cancel and modify are each answered by their own `/confirms` reply, not by
//! the HTTP 200 — `crate::execution::lifecycle` carries those decisions.
//!
//! ## `reduce_only` is a ROUTE on this venue, not a flag
//!
//! IG is position-per-deal, and its POST endpoint always OPENS:
//!
//! | request | endpoint | note |
//! |---|---|---|
//! | MARKET | `POST /positions/otc` v2, `forceOpen: true` | opens its own deal, fills inline |
//! | LIMIT / STOP | `POST /workingorders/otc` v2 | rests; fills later, on the stream |
//! | `reduce_only` MARKET | `POST /positions/otc` v1 + `_method: DELETE` | CLOSES, by epic |
//! | `reduce_only` LIMIT / STOP | — | terminally refused ([`CLOSE_MARKET_ONLY`] — measured: the close endpoint cannot rest) |
//!
//! ⚠ **Before that split, a flatten could not flatten.** Every order took the `forceOpen: true`
//! path, so an order asking to close opened a SECOND, opposing deal — paying the spread twice and
//! holding both legs. [`build_close_request`] carries why the close goes by `epic` rather than by
//! `dealId`, and `crate::execution::event_mapper::confirm_trade_id` carries the trap that makes a close
//! actually BOOK once it is sent.
//!
//! `OrderRequest.symbol` is the USER name (`EURUSD`, decision 0290); the order is written on the EPIC of its
//! kept row (`crate::epic`), and a name with no row, or a row whose name is no epic
//! ([`lifecycle::non_epic_refusal`]), is refused in words before the network. Every fill comes back under the user
//! name of the kept row whose epic its confirm states. The Lightstreamer trade-update stream (see `stream.rs`)
//! carries the delayed working-order fills.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use serde_json::json;
use vike_bridge_core::exec_actor::submit_fence::SubmitFence;
use vike_bridge_core::exec_actor::{
    ConfirmFn, ExecutionClientActor, ExecutionClientActorCancelOutcome,
    ExecutionClientActorCommand, Sending, cancel_batch_undeclared, cancel_event, sending,
    too_old_to_amend, too_old_to_send,
};
use vike_bridge_core::execution_client::hooks::AdoptRestoredFn;
use vike_bridge_core::execution_client::order_client_id::OrderVenueIdRegistry;
use vike_bridge_core::format::format_to_step;
use vike_bridge_core::venue_link::{VenueLink, venue_link};
use vike_execution::EventSender;
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderModifyRejected, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::{DEFAULT_CURRENCY, OrderRefusalId, OrderRequest, OrderType};

use crate::account::venue_link::SessionLink;
use crate::config::IgConfig;
use crate::execution::lifecycle::{Unsettled, amend_working_order, cancel_working_order};
use crate::execution::user_data::{
    DealRefs, LightstreamerDial, OrderVenueIdRegistryDealReference, stream_trades,
};
use crate::instrument::WIRE_EXPIRY;
use crate::transport::IgSession;
use vike_bridge_core::transport::http_error::VenueApiHttpError;

pub(crate) mod event_mapper;
pub(crate) mod lifecycle;

pub(crate) mod user_data;

const VENUE: &str = "ig";

/// Why the trade stream's link gave up when its thread panicked: working-order fills and cancels stop arriving.
const TRADE_STREAM_PANICKED: &str = "the trade stream's thread panicked (a bug; the log has its message): working-order \
                                     fills and cancels will NOT arrive until the adapter restarts (set the account's \
                                     `account.active` off and on)";

/// **The live IG exec client: the shared [`ExecutionClientActor`] with IG's hooks.** `submit`/`cancel` enqueue onto the
/// session thread (non-blocking); a background thread drives the Lightstreamer trade-update stream (delayed
/// working-order fills + audit-A3 snapshot recovery on reconnect). Dropping it stops both. Its `confirm` is
/// [`confirm_order`], and its `adopt_restored` refills the start's `order_client_id <-> dealId` map ([`teach_deal_ids`]) and
/// the stream's `dealReference` routes ([`teach_deal_refs`]) after a restart. A start hands it the HALT sentinel
/// with `ExecutionClientActor::with_halt_path` (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
/// every live start does). `fallback` is the epic whose wire words the ACCOUNT states for an unreadable table
/// ([`FallbackEpic::of`] over its row; `None`: every epic is read).
pub fn spawn_execution_client(
    config: IgConfig,
    events: EventSender,
    fallback: Option<FallbackEpic>,
) -> ExecutionClientActor {
    spawn_sharing(config, events, Arc::default(), SessionLink::default(), fallback, None)
}

/// Why an order on the MOUNTED name is refused when the account's start read no value for it (`crate::adapter`: the
/// name had no kept row then, so the engine books it at a unit of 1 where its row would say 10,000).
fn unvalued_mount_refusal(symbol: &str) -> String {
    format!(
        "ig: what one unit of `{symbol}` is worth was not read when this account started (no kept row named it then), \
         so an order on it would be booked at the wrong notional; once its row is kept \
         (`vike-cli data instruments add --venue ig {symbol}`), restart the account and the start reads it"
    )
}

/// [`spawn_execution_client`] over an `order_client_id <-> dealId` map the caller also hands the reconcile client
/// (`crate::reconcile::reconcile_client_sharing`), so a live working order of THIS process is
/// reported under the order client id it was placed with, and whose two lanes state the account's
/// path on `link` (`crate::account::venue_link`): what a venue start passes, so the status verb can say it.
///
/// # `deal_ids`: `order_client_id <-> dealId`
///
/// The venue identity of a RESTING order, in both directions: the shared [`OrderVenueIdRegistry`] (built by its
/// `Default`, `new`: an order client id re-recorded under a new `dealId` drops its stale reverse row). The exec thread, the
/// confirm watchdog and the reconcile client share ONE per start.
///
/// ⚠ **This used to be a plain local of the exec thread's `run`**, which is why it was filled by the
/// submit's `/confirms` reply and by nothing else: an order whose confirm 404'd, or one the
/// confirm WATCHDOG later resolved, never got a `dealId` here and was uncancellable for good. It
/// is shared (like [`DealRefs`]) so the watchdog's `confirm_order` fills it
/// too, and so the reconcile client — which sees working orders by `dealId` only — can name the
/// local order a live one belongs to instead of reporting it as externally placed.
///
/// In memory and never persisted: an order that predates a restart is not in it until the core re-teaches it with the
/// `dealId` it holds (the exec client's `adopt_restored`, after a gatewayhub restart). A restart of both daemons
/// teaches nothing: the boot restore's join by `dealId` exists (`vike_execution::recon::name_by_venue_order_id`), but IG's
/// row of `crates/vike-venues/src/venue_restore.rs` stays `Never` until a demo smoke proves the round trip, so the
/// boot restore does not run for IG.
///
/// `unvalued`: the mounted USER name whose value the start could not read for want of its kept row; every order on it
/// is refused for the life of this client ([`unvalued_mount_refusal`]).
pub(crate) fn spawn_sharing(
    config: IgConfig,
    events: EventSender,
    deal_ids: Arc<Mutex<OrderVenueIdRegistry<String>>>,
    link: SessionLink,
    fallback: Option<FallbackEpic>,
    unvalued: Option<String>,
) -> ExecutionClientActor {
    // The account's ONE cell (the gatewayhub's status verb reads it by the route key), claimed once, before either lane
    // can write; a start with no route key (a test, a tool) writes a cell nobody reads.
    link.claim(&events.route_key.map_or_else(VenueLink::default, |key| venue_link(&key)));
    // dealReference -> order_client_id, written by the exec thread at submit and read by the
    // stream to correlate a streamed position update (`OPU`) back to the submitting order.
    let deal_refs: OrderVenueIdRegistryDealReference = Arc::new(Mutex::new(DealRefs::default()));
    // The exec thread's submit arm holds it (`SubmitFence::enter`) and the trade stream delivers through it, so a
    // streamed fill of an order in flight goes out behind that order's own answer (owner ruling B-17).
    let fence = Arc::new(SubmitFence::new(events.clone()));
    let stop = Arc::new(AtomicBool::new(false));
    // Background: the Lightstreamer trade-update stream. It logs in independently for its own
    // streaming session (IG permits it); a login failure or missing LS endpoint just exits the
    // thread — the live gate. ExecutionClientActor flag-stops + joins it on teardown.
    let stream_join = thread::Builder::new()
        .name("ig-stream".into())
        .spawn({
            let (config, events, deal_refs, stop, link, fence) = (
                config.clone(),
                events.clone(),
                deal_refs.clone(),
                stop.clone(),
                link.clone(),
                fence.clone(),
            );
            move || {
                // A panic in the stream (a bug in a decoder) ends it and nothing else, and its link says so like a stream
                // that gave up (decision 0302: a dead thread is noticed). `AssertUnwindSafe`: only the stop flag and the
                // link are read after it.
                let (watched_stop, watched_link) = (Arc::clone(&stop), link.clone());
                let streamed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let session = match login_with_retry(&config, &stop, "trade-stream") {
                        Ok(session) => session,
                        // A client that is stopping says nothing: the cell may be its successor's by now.
                        Err(_) if stop.load(std::sync::atomic::Ordering::Relaxed) => return,
                        Err(why) => {
                            link.stream_gave_up(&why);
                            return;
                        }
                    };
                    if session.lightstreamer_endpoint.is_empty() {
                        tracing::error!(
                            target: "vike_ig::execution",
                            "IG login returned no lightstreamerEndpoint — the trade stream cannot \
                             start. Working-order fills and cancels will NOT arrive on this mount"
                        );
                        link.stream_gave_up("IG's login returned no Lightstreamer endpoint");
                        return;
                    }
                    let params = LightstreamerDial {
                        endpoint: session.lightstreamer_endpoint.clone(),
                        account_id: session.account_id.clone(),
                        session,
                        link,
                        fence,
                    };
                    stream_trades(params, deal_refs, events, stop);
                }));
                if streamed.is_err() && !watched_stop.load(std::sync::atomic::Ordering::Relaxed) {
                    tracing::error!(target: "vike_ig::execution", "{TRADE_STREAM_PANICKED}");
                    watched_link.stream_gave_up(TRADE_STREAM_PANICKED);
                }
            }
        })
        .expect("spawn ig-stream thread");
    // Published by the exec thread once its login lands, so the confirm worker can re-query
    // without opening (and paying for) a session of its own. See `SharedSession`.
    let live: SharedSession = Arc::new(Mutex::new(None));
    let confirm: ConfirmFn = {
        let (live, deal_refs, deal_ids) = (live.clone(), deal_refs.clone(), deal_ids.clone());
        Arc::new(move |order_client_id: &str| {
            confirm_order(&live, &deal_refs, &deal_ids, order_client_id)
        })
    };
    let actor = {
        let (deal_refs, deal_ids, stop, live) =
            (deal_refs.clone(), deal_ids.clone(), stop.clone(), live.clone());
        ExecutionClientActor::spawn("ig-exec", events.clone(), move |rx| {
            run(
                config,
                events,
                rx,
                deal_refs,
                deal_ids,
                stop,
                live,
                link,
                &fence,
                (fallback.as_ref(), unvalued.as_deref()),
            )
        })
    }
    .with_confirm(confirm);
    // The orders a previous process placed (a gatewayhub restart's re-teaching): a working order is addressed by
    // its `dealId` alone and IG lists no reference of ours on it, so the venue order id the core holds (the
    // `dealId` the accept carried) refills `deal_ids`; the cancel and the amend of such an order reach IG again,
    // and the reconcile client names it by our order client id. The stream's route is refilled too, so a streamed fill of
    // such an order reaches it; the confirm watchdog's half of `DealRefs` is not: whether the order was a market
    // deal is not known here. Table writes under each map's lock, nothing sent.
    let adopt: AdoptRestoredFn = {
        let (deal_ids, deal_refs) = (deal_ids.clone(), deal_refs.clone());
        Arc::new(move |orders: &[vike_execution::RestoredOrder]| {
            teach_deal_ids(&deal_ids, orders);
            teach_deal_refs(&deal_refs, orders);
        })
    };
    actor.with_adopt_restored(adopt).with_background(stop, stream_join)
}

/// Record the `dealId` of every restored order that carries one (an empty id is none).
fn teach_deal_ids(
    deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    orders: &[vike_execution::RestoredOrder],
) {
    let mut ids = deal_ids.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for o in orders {
        if let Some(deal_id) = o.venue_order_id.as_deref().filter(|id| !id.is_empty()) {
            ids.bind(&o.order_client_id, deal_id.to_string());
        }
    }
}

/// Teach the stream the `dealReference` every restored order was opened with: [`deal_reference`] of its order client id, the
/// same in every process and release, which IG echoes on the create and the stream routes an `OPU` by
/// ([`DealRefs::record_route`]). The routing rule stays the one rule, by reference; a reference no process sent (a
/// close sends none) is never heard and routes nothing.
fn teach_deal_refs(
    deal_refs: &OrderVenueIdRegistryDealReference,
    orders: &[vike_execution::RestoredOrder],
) {
    let mut refs = deal_refs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for o in orders {
        refs.record_route(&o.order_client_id, &deal_reference(&o.order_client_id));
    }
}

/// The exec thread's session, published for the confirm worker.
///
/// `None` until that thread's login lands — and forever if it never does. A confirm then reports
/// NOTHING, which is the [`ConfirmFn`] contract for "inconclusive" and leaves the watchdog's
/// last-resort reject to backstop the order. Sharing rather than opening a second session is what
/// keeps `confirm` off IG's un-metered login path (this crate opens more sessions than any other
/// bridge already; see `crates/bridges/ig/CLAUDE.md`), and is safe because `IgSession` keeps its
/// expiring token pair behind a lock — a 401 either thread meets re-authenticates it for both.
type SharedSession = Arc<Mutex<Option<Arc<IgSession>>>>;

/// Re-query ONE order's status with IG and return the authoritative events — the venue-side answer
/// to the core's `Command::ConfirmOrder`.
///
/// IG has no "get order by order client id" endpoint: `GET /confirms/{dealReference}` is the only
/// per-order status read it offers, so this is answerable at all only because the exec thread
/// records `order_client_id -> dealReference` at submit ([`DealRefs::record`]).
///
/// An EMPTY result is the honest answer to every question this cannot settle — no session yet, no
/// `dealReference` for that order client id (IG never acknowledged the deal, so it has nothing to confirm), or
/// a confirm that still will not answer. Per the [`ConfirmFn`] contract that means "nothing
/// conclusive", and it is deliberately not a rejection: guessing a terminal here is the same
/// phantom-position failure [`unresolved_confirm`] exists to avoid.
///
/// A confirm that DOES answer also teaches `deal_ids` the order's `dealId` — this is the only way an
/// order whose submit-time confirm 404'd ever becomes cancellable or amendable.
fn confirm_order(
    live: &SharedSession,
    deal_refs: &OrderVenueIdRegistryDealReference,
    deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    order_client_id: &str,
) -> Vec<Event> {
    let Some(session) = live.lock().unwrap().clone() else {
        return Vec::new();
    };
    answer_confirm(deal_refs, deal_ids, order_client_id, |deal_ref| {
        confirm_with_retry(&session, deal_ref)
    })
}

/// [`confirm_order`] once a session is up: `fetch` is the `/confirms` read of a deal reference, ladder included.
fn answer_confirm(
    deal_refs: &OrderVenueIdRegistryDealReference,
    deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    order_client_id: &str,
    fetch: impl FnOnce(&str) -> Result<serde_json::Value, VenueApiHttpError>,
) -> Vec<Event> {
    let Some(pending) = deal_refs.lock().unwrap().pending_for(order_client_id) else {
        tracing::warn!(
            target: "vike_ig::execution",
            venue = VENUE,
            %order_client_id,
            "confirm requested for an order this process holds no dealReference for — IG has \
             nothing to be asked about it; reporting nothing conclusive"
        );
        return Vec::new();
    };
    match fetch(&pending.deal_ref) {
        Ok(confirm) => {
            note_deal_id(deal_ids, order_client_id, &confirm);
            map_confirm(order_client_id, vike_model::now_ms(), pending.market, &confirm)
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::execution",
                venue = VENUE,
                %order_client_id,
                deal_ref = %pending.deal_ref,
                status = e.status,
                error = %e.message,
                "confirm re-query still could not reach IG's /confirms — reporting nothing \
                 conclusive rather than guessing a terminal"
            );
            Vec::new()
        }
    }
}

/// Teach `deal_ids` the `dealId` of `order_client_id`'s order from a `/confirms` reply that ACCEPTS it: a rejected deal does not
/// exist and must never become something cancel could address, and any other word proves no deal.
fn note_deal_id(
    deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    order_client_id: &str,
    confirm: &serde_json::Value,
) {
    if confirm.get("dealStatus").and_then(|s| s.as_str()) != Some("ACCEPTED") {
        return;
    }
    if let Some(deal_id) = confirm.get("dealId").and_then(|d| d.as_str()) {
        deal_ids.lock().unwrap().bind(order_client_id, deal_id.to_string());
    }
}

/// How many times a start's login is attempted before the lane gives up and says so. Bounded rather
/// than endless: a login that is being REFUSED (bad key, changed password) cannot be cured by
/// repeating it, and an unbounded loop against IG's login endpoint is how an account gets
/// rate-limited out of the venue entirely.
const LOGIN_ATTEMPTS: u32 = 5;
/// Backoff between those attempts. Deliberately coarse — nothing is waiting on these threads, and
/// the failures being retried past (a DNS blip, a gateway restart) are seconds-scale.
const LOGIN_BACKOFF: std::time::Duration = std::time::Duration::from_secs(3);

/// Log one of this client's two lanes in, retrying a BOUNDED number of times and reporting every
/// outcome. `lane` names the caller in the log, because the two failures have different blast
/// radii: an `"exec"` failure makes every submit reject, a `"trade-stream"` failure makes every
/// working-order fill and cancel go missing.
///
/// ⚠ **Both call sites used to swallow the failure entirely** — the stream's was
/// `if let Ok(session) = IgSession::login(&config)` with no `else`, the exec thread's an
/// `Err(_) => return`. So an IG gateway that was merely restarting killed the lane permanently,
/// with nothing in the log: a start that submits fine and never delivers a fill, or a venue that
/// rejects everything with no stated cause.
///
/// The two failure shapes are separated because only one of them is worth retrying: a **refusal**
/// (IG answered with an HTTP status — a rejected key, a locked account) will answer the same way
/// forever, so it stops at once and says why; anything else is transport-shaped and gets the
/// budget. `stop` is honoured between attempts so teardown never waits out the backoff.
///
/// `Err` is why the lane is down, in words for `crate::account::venue_link` (the log lines are unchanged).
/// The ladder is `vike_utility::retry::retry_on_pauses`: an answer from IG, or a stop, settles it.
fn login_with_retry(
    config: &IgConfig,
    stop: &Arc<AtomicBool>,
    lane: &'static str,
) -> Result<Arc<IgSession>, String> {
    let mut last = String::new();
    let settled = vike_utility::retry::retry_on_pauses(
        &[LOGIN_BACKOFF; LOGIN_ATTEMPTS as usize - 1],
        |attempt| {
            // No IG login while the FX market is closed (owner ruling 2026-11-13): ONE wait to the open, then the login.
            let held = config.fx_held_pause(std::time::Duration::ZERO);
            vike_bridge_core::websocket_session::user::sleep_unless_stopped(stop, held);
            if stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Some(Err("the client is stopping".to_string()));
            }
            match IgSession::login(config) {
                Ok(session) => {
                    if attempt > 1 {
                        tracing::info!(
                            target: "vike_ig::execution",
                            lane,
                            attempt,
                            "IG login succeeded after retrying"
                        );
                    }
                    Some(Ok(Arc::new(session)))
                }
                // `status == 0` is this crate's marker for "never reached IG" (`IgSession::net_err`);
                // any real status means IG answered, and answered no.
                Err(e) if e.status != 0 => {
                    tracing::error!(
                        target: "vike_ig::execution",
                        lane,
                        status = e.status,
                        error = %e.message,
                        "IG REFUSED this login — retrying cannot fix a refusal. This IG lane is DOWN \
                         for the life of the mount until the credentials are fixed"
                    );
                    Some(Err(format!("IG refused it ({e})")))
                }
                Err(e) => {
                    tracing::warn!(
                        target: "vike_ig::execution",
                        lane,
                        attempt,
                        of = LOGIN_ATTEMPTS,
                        error = %e.message,
                        "IG login failed; retrying"
                    );
                    last = e.message;
                    None
                }
            }
        },
        Option::is_some,
        |pause| vike_bridge_core::websocket_session::user::sleep_unless_stopped(stop, pause),
    );
    if let Some(settled) = settled {
        return settled;
    }
    // The last failure waits out its backoff too, as every other one does, before the lane is called down.
    vike_bridge_core::websocket_session::user::sleep_unless_stopped(stop, LOGIN_BACKOFF);
    tracing::error!(
        target: "vike_ig::execution",
        lane,
        attempts = LOGIN_ATTEMPTS,
        "IG login exhausted its retry budget. This IG lane is DOWN for the life of the mount"
    );
    Err(format!("{LOGIN_ATTEMPTS} attempts failed, the last with {last}"))
}

fn direction(side: i32) -> &'static str {
    if side >= 0 { "BUY" } else { "SELL" }
}

/// The `ts` an event of one submit carries: the request's own when it states one (a strategy's order carries the
/// engine clock), else the moment this thread stamps it. An order from the command line or the control door states
/// none (`OrderRequest::ts` defaults to 0), and every event of it, a FILL included, used to carry 0 = 1970 (three
/// `exec_fill` rows of the demo account in the hist store).
fn event_ts(stated: i64) -> i64 {
    if stated > 0 { stated } else { vike_model::now_ms() }
}

fn fmt_level(p: f64) -> String {
    format!("{p:.5}")
}

/// The finest `size` IG takes: its REST reference says of the deal size on `POST /positions/otc` (v1 and v2) "Check
/// precision is not more than 12 decimal places". The venue publishes no deal-size STEP (`minDealSize` is a floor, not
/// a grid), so this is the one grid a size is written on.
const SIZE_STEP: &str = "0.000000000001";

/// `qty` as the order's `size`: through `format_to_step` (the pinned Decimal wire site) at [`SIZE_STEP`], read back
/// into a JSON number, so the float dust a sum or a gate's rounding leaves (`0.30000000000000004`) never reaches the
/// wire, while a size with twelve decimals or fewer is unchanged.
fn wire_size(qty: f64) -> f64 {
    format_to_step(qty, SIZE_STEP).parse().unwrap_or(qty)
}

/// A market order: the market endpoint opens it (or closes, when reduce-only). Exactly [`OrderType::Market`]:
/// every kind but `limit` and `stop` used to read as market here and went out as a MARKET open or close
/// (`stop_limit`, `take_profit`, an unrecognised word); [`unbuilt_kind`] refuses those now, in every builder.
fn is_market(req: &OrderRequest) -> bool {
    req.order_type == OrderType::Market
}

/// Why `req` is refused before anything is read or sent (a terminal `OrderRejected` with this text): a
/// kind this bridge builds no order for. Only `market`, `limit` and `stop` are built. The capability
/// preflight refuses every other kind first (IG's row lists the three); this is the adapter's own guard,
/// asked by the exec thread before any read and by [`build_request_expiring`] and
/// [`build_close_request_expiring`], so no caller can build a MARKET body for such a kind.
fn unbuilt_kind(req: &OrderRequest) -> Option<String> {
    match req.order_type {
        OrderType::Market | OrderType::Limit | OrderType::Stop => None,
        _ => Some(format!(
            "ig builds no `{}` order (only market, limit and stop); refused, never sent as a \
             market order",
            req.order_type
        )),
    }
}

/// The terminal reason a `reduce_only` order this client cannot express is refused with. Named so
/// the refusal is greppable from a log and assertable from a test.
///
/// ⚠ **The refusal is MEASURED, not cautious.** Probed live against `demo-api.ig.com` on
/// 2026-08-22 (`tests/ig_smokes/ig_limit_close_probe.rs`, re-runnable;
/// `tests/fixtures/confirm_close_limit_rejected.json` is the captured reply): IG's close endpoint
/// has no resting arm at all. `orderType: LIMIT` there is execute-at-level-or-better-NOW — a
/// NON-marketable level (exactly what a resting reduce-only close would be) answers 200 with a
/// `dealReference` whose confirm is `dealStatus: REJECTED`,
/// `reason: LIMIT_ORDER_WRONG_SIDE_OF_MARKET`, resting NOTHING (`/workingorders` stays empty, the
/// position stays open), while a MARKETABLE level executes immediately at level-or-better.
/// `orderType: STOP` does not exist on that endpoint: HTTP 400 `invalid.request.orderType`, before
/// any deal is minted. So the only alternatives to this refusal are a silent immediate close
/// wearing a limit price, or the pre-#1431 defect (a working order that OPENS when it triggers) —
/// both silently wrong. `tests/offline/close_limit_never_rests.rs` carries the verbatim wire table and
/// pins both mappers over the captured rejection.
pub(crate) const CLOSE_MARKET_ONLY: &str = "ig: a reduce_only close is MARKET-only (measured: IG's close endpoint cannot rest an order — \
     its LIMIT arm executes at level-or-better NOW and it has no STOP arm); submit it as \
     order_type=market";

/// The `dealReference` this bridge SENDS on an open (IG accepts a client-supplied one, `[A-Za-z0-9_-]`,
/// at most 30 characters), derived from the order client id: FNV-1a 64 so it is the same in every process and
/// release (`DefaultHasher` promises neither). It is what makes a submit whose reply was lost
/// resolvable: IG's `/confirms/{ref}` is the only per-order read it has, and without a reference of
/// our own a lost POST reply leaves nothing to ask about.
pub(crate) fn deal_reference(order_client_id: &str) -> String {
    let h = order_client_id
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    format!("vk{h:016x}")
}

/// Build the IG request that CLOSES exposure: `(path, version, body)` for
/// `POST /positions/otc` + `_method: DELETE` (see [`IgSession::post_method_delete`] for why the
/// verb override rather than a real `DELETE`).
///
/// ⚠ **This closes by `epic` + `expiry`, deliberately, and not by `dealId`.** IG is
/// position-per-deal: this bridge's own opens carry `forceOpen: true`, so N orders in one epic are
/// N separate deals, and a flatten has to net across all of them. Closing by `dealId` closes ONE,
/// capped at that deal's size — which means a multi-deal flatten becomes several calls, several
/// `dealReference`s, and several fills for a single `order_client_id`. IG's own mappers here are
/// whole-fill (`FillShape::Whole` in the cross-bridge conformance harness): neither carries
/// cumulative state, so the second fill of one order would terminalize it a second time.
///
/// The epic form makes IG do the netting and answer with ONE `dealReference` for the WHOLE
/// requested size — one confirm, one fill, exactly one terminal. Measured against
/// `demo-api.ig.com` (2026-08-21): two separate 0.1 BUY deals, closed by a single
/// `{epic, expiry, direction: SELL, size: 0.2}` call, answered one reference whose confirm carried
/// `size: 0.2` and `affectedDeals` naming BOTH deals `FULLY_CLOSED`. It is also the only form that
/// survives a restart or a position this process did not open, since it needs no local deal map.
///
/// `direction` is the CLOSING side, i.e. `req.side` unchanged — `OrderIntent::Flatten` already mints
/// the side opposite the position (`vike_model::closing_side`), so negating it here would re-open.
///
/// # Errors
/// The refusal of any order that is not a market one: a kind this bridge builds no order for
/// ([`unbuilt_kind`]), and a `limit` or `stop` ([`CLOSE_MARKET_ONLY`]). Neither is ever written as a
/// MARKET close.
pub fn build_close_request(
    req: &OrderRequest,
) -> Result<(String, &'static str, serde_json::Value), String> {
    build_close_request_expiring(req, WIRE_EXPIRY)
}

/// [`build_close_request`] for an epic whose `expiry` is `expiry` (`-` for a cash CFD, a dated contract's own word,
/// `DEC-26`, for one of those: IG closes by `epic` + `expiry`, and a dated deal is accepted and closed with it,
/// measured on the demo gateway on 2026-10-30). The bytes for `-` are exactly [`build_close_request`]'s.
///
/// # Errors
/// As [`build_close_request`].
pub fn build_close_request_expiring(
    req: &OrderRequest,
    expiry: &str,
) -> Result<(String, &'static str, serde_json::Value), String> {
    if let Some(why) = unbuilt_kind(req) {
        return Err(why);
    }
    if !is_market(req) {
        return Err(CLOSE_MARKET_ONLY.to_string());
    }
    let body = json!({
        "epic": req.symbol,
        "expiry": expiry,
        "direction": direction(req.side),
        "size": wire_size(req.qty),
        "orderType": "MARKET",
    });
    Ok(("/positions/otc".to_string(), "1", body))
}

/// The `currencyCode` an order on `epic` is written in, from the epic's own `GET /markets/{epic}` body.
///
/// IG restricts the field to `instrument.currencies[]` ("Restricted to available instrument currencies"), and
/// an FX Mini lists its QUOTE currency ALONE (measured on the demo gateway, 2026-10-19: EURUSD lists `USD`,
/// EURGBP `GBP`, USDJPY `JPY`), so a constant `USD` was a terminal reject for every epic not quoted in dollars
/// (`INSTRUMENT_NOT_TRADEABLE_IN_THIS_CURRENCY`, expected from the reference, not yet observed). The choice:
/// `vike_model::DEFAULT_CURRENCY` when listed (so an epic that has always been written `USD` still is, whatever the
/// account), else the account's own currency when listed, else the first listed. A table that lists none
/// leaves nothing to write: `Err`, in words.
pub(crate) fn order_currency(
    epic: &str,
    market: &serde_json::Value,
    account: Option<&str>,
) -> Result<String, String> {
    let listed: Vec<&str> = market
        .pointer("/instrument/currencies")
        .and_then(serde_json::Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|c| c.get("code").and_then(serde_json::Value::as_str))
                .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()))
                .collect()
        })
        .unwrap_or_default();
    [Some(DEFAULT_CURRENCY), account]
        .into_iter()
        .flatten()
        .find(|c| listed.contains(c))
        .or_else(|| listed.first().copied())
        .map(str::to_string)
        .ok_or_else(|| {
            format!(
                "ig: {epic} lists no dealing currency in instrument.currencies[], so no currencyCode can \
                 be sent for it (IG restricts the field to that list); refused before anything reached IG"
            )
        })
}

/// Why an OPENING order on this market is refused: IG charges commission on a SHARES market (share dealing and share
/// CFDs alike), and this client reads it from nowhere, so every fill of it would book `commission: 0.0` (`map_confirm`,
/// `decode_trade_confirm` and the reconcile fill report all hard-code it) and the fee ledger would be wrong without a
/// word. A CFD or spread bet on a currency pair, an index or a commodity is spread-only, where 0 is right. `None` for
/// every other `instrument.type`, and for a table that states none.
fn commission_refusal(epic: &str, market: &serde_json::Value) -> Option<String> {
    let class = market.pointer("/instrument/type").and_then(serde_json::Value::as_str)?;
    (class == "SHARES").then(|| {
        format!(
            "ig: {epic} is a SHARES market and IG charges commission on shares (share dealing and share CFDs) that \
             this client reads from nowhere, so every fill would book commission 0 and the fee ledger would be \
             wrong without a word; the order was refused rather than booked silently wrong. A position already held \
             is closed as usual (a closing order reads no table)"
        )
    })
}

/// **The epic whose wire words the ACCOUNT states**, for when its table cannot be read: the account's row's
/// `fallback_symbol`, `fallback_symbol_expiry` and `fallback_symbol_currency` facts (`vike_secrets::AccountFact`; owner,
/// 2026-11-13: the epic the mount used to be wired to is the account's row, never a compiled constant). `None` when the
/// account states no complete set: every epic is then read, and an unreadable one refused (an order) or closed with
/// `WIRE_EXPIRY` (a close), as for any other epic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackEpic {
    /// The epic.
    pub(crate) epic: String,
    /// What an order and a close of it write when its table cannot be read.
    pub(crate) wire: EpicWire,
}

impl FallbackEpic {
    /// The account's stated fallback epic, or `None` when its row states no complete set of the three facts.
    #[must_use]
    pub fn of(row: Option<&vike_secrets::AccountRow>) -> Option<FallbackEpic> {
        use vike_secrets::AccountFact;
        let row = row?;
        Some(FallbackEpic {
            epic: row.fact(AccountFact::FallbackSymbol)?.to_string(),
            wire: EpicWire {
                currency: row.fact(AccountFact::FallbackSymbolCurrency)?.to_string(),
                expiry: row.fact(AccountFact::FallbackSymbolExpiry)?.to_string(),
            },
        })
    }

    /// The fallback's wire words when it is `epic`'s.
    fn for_epic(fallback: Option<&FallbackEpic>, epic: &str) -> Option<EpicWire> {
        fallback.filter(|f| f.epic == epic).map(|f| f.wire.clone())
    }
}

/// What an order on one epic writes that the epic's OWN table decides, read once per epic: the `currencyCode` and the
/// `expiry` (`-` for an undated cash CFD, the epic's own word for a dated contract such as `DEC-26`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EpicWire {
    pub(crate) currency: String,
    pub(crate) expiry: String,
}

/// `epic`'s order currency and expiry, its table read ONCE per epic and kept in `kept`.
///
/// ⚠ **FAILS CLOSED.** A table that cannot be READ (a transport error, a 429 on the shared non-trading
/// allowance, a 5xx) is refused in words, retryably, with an `error` log, and nothing is kept, so the next
/// order reads again: guessing `vike_model::DEFAULT_CURRENCY` for an epic that lists another currency alone would send
/// the order IG's reference makes a terminal reject (never observed) or, worse, one it accepts in a currency
/// nobody chose. Two answers are exempt because a good read already stands behind them: an epic in `kept`
/// (served without a read), and the epic the ACCOUNT states with its wire words ([`FallbackEpic`]: EUR/USD mini, which
/// lists USD, measured, on the account it was wired to), which keeps trading through an unreadable table on the words
/// the account states. A table that is read and lists no currency is a verdict ([`order_currency`]), not kept.
///
/// ⚠ **The read runs ON THE EXEC COMMAND THREAD** (`run`'s one `rx.recv()` loop), once per epic, so a slow
/// `GET /markets/{epic}` delays whatever is queued behind the first order on a new epic: a cancel or a close
/// waits for it. It is bounded, not instant: the session's agent allows 30 s a request
/// (`vike_transport::http::blocking_agent`), and a 401 adds the one re-login and the one retry
/// (`IgSession`'s `call_with_reauth`), so the worst case is a few of them. An epic already read is served from
/// `kept`, so only the first OPENING order on an epic pays; a CLOSE of an epic this process has not opened reads
/// the table once for its `expiry` alone ([`close_expiry`]). Left unbounded on purpose: a shorter bound would
/// refuse (fail closed) an order whose read was only slow, and the table is static, so a warm read before the
/// first order is the cure if it ever bites.
fn epic_wire(
    session: &IgSession,
    kept: &mut HashMap<String, EpicWire>,
    epic: &str,
    fallback: Option<&FallbackEpic>,
) -> Result<EpicWire, String> {
    epic_wire_with(kept, epic, session.account_currency.as_deref(), fallback, || {
        session.get(&format!("/markets/{epic}"), "3", "")
    })
}

/// The `expiry` an order on `epic` writes: the table's own word when it is a DATED contract's (`DEC-26`), else
/// [`WIRE_EXPIRY`] (an undated cash CFD, a table that states none, and `DFB`: every body this bridge ever sent
/// stays byte for byte what it was).
fn wire_expiry_of(market: &serde_json::Value) -> String {
    market
        .pointer("/instrument/expiry")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::instrument::wire_expiry)
        .unwrap_or(WIRE_EXPIRY)
        .to_string()
}

/// The `expiry` a CLOSE of `epic` writes (IG closes by `epic` + `expiry`, [`build_close_request_expiring`]): the one
/// the open of this process read ([`epic_wire`], in `wires`), else the table's own, read once and kept in `closes`
/// for a position this process did not open (a restart, a foreign deal). Never a reason to refuse a close: the epic the
/// account states ([`FallbackEpic`]) reads nothing and closes with the expiry the account states for it, and a table
/// that cannot be read sends [`WIRE_EXPIRY`] as every close always did (a dated epic would then be refused by IG,
/// loudly, as it was before dated epics were wired).
fn close_expiry(
    session: &IgSession,
    wires: &HashMap<String, EpicWire>,
    closes: &mut HashMap<String, String>,
    epic: &str,
    fallback: Option<&FallbackEpic>,
) -> String {
    close_expiry_with(wires, closes, epic, fallback, || {
        session.get(&format!("/markets/{epic}"), "3", "")
    })
}

/// [`close_expiry`] over the table read `read` performs (called only when neither map has an answer).
fn close_expiry_with(
    wires: &HashMap<String, EpicWire>,
    closes: &mut HashMap<String, String>,
    epic: &str,
    fallback: Option<&FallbackEpic>,
    read: impl FnOnce() -> Result<serde_json::Value, VenueApiHttpError>,
) -> String {
    if let Some(wire) = wires.get(epic) {
        return wire.expiry.clone();
    }
    if let Some(expiry) = closes.get(epic) {
        return expiry.clone();
    }
    if let Some(wire) = FallbackEpic::for_epic(fallback, epic) {
        return wire.expiry;
    }
    match read() {
        Ok(market) => {
            let expiry = wire_expiry_of(&market);
            closes.insert(epic.to_string(), expiry.clone());
            expiry
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %epic, status = e.status, error = %e.message,
                "could not read the table of an epic being closed; closing with expiry {WIRE_EXPIRY}, as every close did"
            );
            WIRE_EXPIRY.to_string()
        }
    }
}

/// [`epic_wire`] over the table read `read` performs (called only when `kept` has no answer).
fn epic_wire_with(
    kept: &mut HashMap<String, EpicWire>,
    epic: &str,
    account: Option<&str>,
    fallback: Option<&FallbackEpic>,
    read: impl FnOnce() -> Result<serde_json::Value, VenueApiHttpError>,
) -> Result<EpicWire, String> {
    if let Some(wire) = kept.get(epic) {
        return Ok(wire.clone());
    }
    let market = match read() {
        Ok(market) => market,
        Err(e) => {
            if let Some(wire) = FallbackEpic::for_epic(fallback, epic) {
                tracing::warn!(
                    target: "vike_ig::execution", venue = VENUE, %epic, status = e.status, error = %e.message,
                    "could not read the dealing currencies of the epic the account states; writing the \
                     currencyCode {} and expiry {} its row states (`fallback_symbol_currency`, \
                     `fallback_symbol_expiry`)",
                    wire.currency, wire.expiry
                );
                return Ok(wire);
            }
            // Returned, not logged: `run` refuses the order and logs it once.
            return Err(format!(
                "ig: could not read the dealing currencies of {epic} ({e}), so no currencyCode can be \
                 chosen for it; the order was refused rather than guess one: retry"
            ));
        }
    };
    if let Some(why) = commission_refusal(epic, &market) {
        return Err(why);
    }
    let currency = order_currency(epic, &market, account)?;
    let expiry = wire_expiry_of(&market);
    tracing::info!(
        target: "vike_ig::execution", venue = VENUE, %epic, %currency, %expiry,
        "orders on this epic are written in the currency and expiry its table lists"
    );
    let wire = EpicWire { currency, expiry };
    kept.insert(epic.to_string(), wire.clone());
    Ok(wire)
}

/// Build the IG request that OPENS exposure: `(path, version, body)`. MARKET → open a position;
/// LIMIT/STOP → a working order. `epic` = `OrderRequest.symbol`; `currency` = [`order_currency`] of it.
///
/// ⚠ `forceOpen: true` is right HERE and only here. It is what makes an order open its own deal
/// rather than net against an existing one — correct for an open, and the reason a close cannot go
/// through this function at all (see [`build_close_request`]).
///
/// # Errors
/// The refusal of a kind this bridge builds no order for ([`unbuilt_kind`]): every kind but `market`,
/// `limit` and `stop`. Such an order is never written as a MARKET open (it used to be).
pub fn build_request(
    req: &OrderRequest,
    currency: &str,
) -> Result<(String, &'static str, serde_json::Value), String> {
    build_request_expiring(req, currency, WIRE_EXPIRY)
}

/// [`build_request`] for an epic whose `expiry` is `expiry` (`-` for a cash CFD, a dated contract's own word,
/// `DEC-26`, for one of those; a market order and a working order alike were accepted with it on the demo gateway,
/// 2026-10-30). The bytes for `-` are exactly [`build_request`]'s.
///
/// # Errors
/// As [`build_request`].
pub fn build_request_expiring(
    req: &OrderRequest,
    currency: &str,
    expiry: &str,
) -> Result<(String, &'static str, serde_json::Value), String> {
    if let Some(why) = unbuilt_kind(req) {
        return Err(why);
    }
    let dir = direction(req.side);
    if is_market(req) {
        let body = json!({
            "epic": req.symbol,
            "direction": dir,
            "size": wire_size(req.qty),
            "orderType": "MARKET",
            "currencyCode": currency,
            "expiry": expiry,
            "forceOpen": true,
            "guaranteedStop": false,
            "dealReference": deal_reference(&req.order_client_id),
        });
        Ok(("/positions/otc".to_string(), "2", body))
    } else {
        // only a `limit` or a `stop` reaches here (`unbuilt_kind` refused every other kind)
        let (otype, level) = if req.order_type == OrderType::Limit {
            ("LIMIT", req.price)
        } else {
            ("STOP", req.trigger_price)
        };
        let body = json!({
            "epic": req.symbol,
            "direction": dir,
            "size": wire_size(req.qty),
            "type": otype,
            "level": level.map(fmt_level),
            // tif_for step-2: `req.time_in_force` is deliberately NOT read — every working
            // order rests GOOD_TILL_CANCELLED (the Ignored ig row of
            // `vike_venues::venue_time_in_force::venue_time_in_force`, pinned there and in the tests below).
            // Honoring the request is a step-2 wire change behind demo smokes.
            "timeInForce": "GOOD_TILL_CANCELLED",
            "currencyCode": currency,
            "expiry": expiry,
            "guaranteedStop": false,
            "dealReference": deal_reference(&req.order_client_id),
        });
        Ok(("/workingorders/otc".to_string(), "2", body))
    }
}

/// Map an IG `/confirms/{ref}` response to events. `market` → a fill is emitted from the deal
/// level; a working order emits Accepted only (its fill arrives later via the stream).
///
/// ⚠ **Only `dealStatus` `ACCEPTED` accepts, and only a stated `size` and `level` fill.** Anything but `REJECTED` used to
/// read as a deal done: an absent or other `dealStatus` on a market order gave `OrderAccepted`, `Fill` and `OrderFilled`
/// at size 0 and price 0, ending the order over a deal nobody saw. Such a reply now says nothing (the order stays as it
/// was, for the re-ask and the watchdog), and an accepted market deal with no size or level is the accept alone: its fill
/// is the stream's `OPU` or the reconcile's. A `REJECTED` whose `reason` says IG does not know the result (`UNKNOWN`,
/// `GENERAL_ERROR`: `event_mapper.rs`'s `rejected_with_unknown_result`) says nothing either: the deal may have been done.
// `pub` (the `exec` module stays private; re-exported `#[doc(hidden)]` at the crate root) so the
// cross-bridge conformance harness (`vike-bridge-core/tests/bridge_conformance.rs`) can drive the
// REAL accept mapper — the only production caller is `run()`'s network path. Same
// test-reachability rationale as oanda's `map_order_response`. Not part of the public API.
pub fn map_confirm(
    order_client_id: &str,
    ts: i64,
    market: bool,
    confirm: &serde_json::Value,
) -> Vec<Event> {
    let mut out = Vec::new();
    let status = confirm.get("dealStatus").and_then(|s| s.as_str()).unwrap_or("");
    if crate::execution::event_mapper::rejected_with_unknown_result(confirm) {
        tracing::warn!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, reason = ?confirm.get("reason"),
            "IG's confirm says it does not know the deal's result - the order's outcome is UNKNOWN, not refused; it is asked about again"
        );
        return out;
    }
    if status == "REJECTED" {
        let reason = confirm.get("reason").and_then(|r| r.as_str()).unwrap_or("REJECTED");
        tracing::warn!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, %reason, "IG REJECTED the deal at confirm"
        );
        out.push(Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: order_client_id.to_string(),
            reason: crate::execution::event_mapper::rejection_words(reason).into(),
            timestamp: ts,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::VenueRefused.word(),
            ),
        }));
        return out;
    }
    if status != "ACCEPTED" {
        tracing::warn!(
            target: "vike_ig::execution", venue = VENUE, %order_client_id, deal_status = %status,
            "IG's confirm neither accepts nor rejects the deal - it says nothing about this order yet"
        );
        return out;
    }
    let deal_id = confirm.get("dealId").and_then(|d| d.as_str()).map(str::to_string);
    out.push(Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: deal_id.clone().map(Into::into),
        timestamp: ts,
    }));
    if market {
        let positive =
            |key: &str| confirm.get(key).and_then(serde_json::Value::as_f64).filter(|v| *v > 0.0);
        let (Some(level), Some(size)) = (positive("level"), positive("size")) else {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id,
                "IG accepted the deal and its confirm states no size or no level - no fill is booked from it; the trade stream or the reconcile carries the fill"
            );
            return out;
        };
        let side =
            if confirm.get("direction").and_then(|d| d.as_str()) == Some("SELL") { -1 } else { 1 };
        // The user name of the kept row whose epic the confirm states (decision 0290), else the epic, said.
        let symbol = crate::epic::user_name_or_epic(
            confirm.get("epic").and_then(|e| e.as_str()).unwrap_or_default(),
        );
        // Dual-publish (crypto contract): bare Fill first so the Account folds position/PnL,
        // then the OrderFilled wrap so the FSM applies it — both the same FillEvent.
        let fill = FillEvent {
            // SYNTHESIZED, not dropped — and the difference from oanda is what `order_client_id` guarantees
            // here. `/confirms/{ref}` is the synchronous confirmation of ONE deal, so this mapper
            // emits at most one fill per call and `order_client_id` alone identifies it; re-polling the same
            // confirm therefore re-derives the SAME id and dedups, which is exactly the property a
            // synthesized id must have. (oanda's stream can carry many fills per order, so no such
            // per-fill key exists there — hence a drop there and a synthesis here.)
            // ⚠ Was `deal_id.unwrap_or_default()` = `""` on an absent dealId, which skipped dedup.
            // ⚠ ...and was `dealId` UNCONDITIONALLY, which made a CLOSE fill collide with the
            // OPENING fill of the very position it closes — IG reuses the closed deal's id on a
            // close confirm. `confirm_trade_id` owns that rule and is shared with the streamed
            // twin so the two lanes cannot drift apart; read its doc before touching this.
            trade_id: match crate::execution::event_mapper::confirm_trade_id(confirm) {
                Some(t) => t,
                None => {
                    tracing::warn!(
                        venue = VENUE,
                        %order_client_id,
                        "confirm carries no usable execution id — synthesizing a deterministic \
                         per-confirm trade_id from the order client id so the fill still folds exactly once"
                    );
                    TradeId::prefixed("IG-CONFIRM-", order_client_id)
                }
            },
            order_client_id: order_client_id.to_string(),
            venue: VENUE.to_string().into(),
            symbol: symbol.into(),
            side,
            last_qty: size,
            last_price: level,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: String::new().into(),
            timestamp: ts,
            mark_price: None,
            position_side: vike_model::events::PositionSide::Both,
        };
        out.extend(vike_bridge_core::terminal_events(order_client_id.to_string(), fill, ts, true));
    }
    out
}

/// How many times a `GET /confirms/{dealReference}` is attempted before the call is resolved
/// without it. IG answers **404** `error.service.execution.find` for a reference it has not
/// finished processing (measured against `demo-api.ig.com`, 2026-08-21), so a miss is routine
/// rather than an answer — retrying is what turns a race into a result.
///
/// ⚠ **The first ladder (3 attempts, 200 ms apart, ~0.4 s) was too short.** Measured on the CI box
/// (v0.1.61, demo account 13): `/confirms` still answered 404 after two tries, so the submit
/// resolved as accepted-with-no-venue-id ([`unresolved_confirm`]) and — since the `dealId` only ever
/// arrived through this reply — the order was left uncancellable. 7 attempts with 6 pauses of
/// 400 ms is ~2.4 s of waiting (~3 s with the round trips): enough for IG to finish processing,
/// still bounded.
const CONFIRM_ATTEMPTS: u32 = 7;
/// Backoff between those attempts. ⚠ This runs on the exec command thread (off the core fold, but in
/// front of the next command), so an unanswerable reference holds that thread for the whole ladder
/// — ~2.4 s, once per order whose confirm never comes. That is the price of not resolving a live
/// order blind; the confirm watchdog's own re-ask runs on its OWN worker thread and is not charged.
/// (This ladder is a REST retry policy, not a liveness timing, so it is not a row of
/// `vike_venues::venue_timings` — that table's module doc declares such policies unmodelled.)
const CONFIRM_BACKOFF: std::time::Duration = std::time::Duration::from_millis(400);

/// **The most a submit's calls may take, from its order POST**: a confirm is asked only while it can still end
/// inside it ([`confirm_within`]), so an order IG took is answered (its confirm, or [`unresolved_confirm`]'s
/// id-less accept) by the queue's wait plus this, before the core's last-resort reject drops the late answer and
/// leaves the order resting untracked. 35 s: the POST's own 30 s and one confirm's 5 s
/// ([`crate::transport::CONFIRM_CALL_TIMEOUT`]); an IG that answers both in time keeps its whole ladder. Not counted:
/// a 401's re-login inside a call (`crate::transport::IgSession`'s `call_with_reauth`), the deribit bridge's
/// exclusion too.
const SUBMIT_HOLD: std::time::Duration = std::time::Duration::from_secs(35);

// The row is this code (`vike_venues::venue_timings::IG`'s `submit`, the chain the table's test holds under the
// core's last resort): the queue's wait, which the epic read in front of the POST counts against, the POST's bound,
// the ladder's attempts, bound and pause, and the hold, which is the POST's bound and one confirm's.
const _: () = assert!(match vike_venues::venue_timings::IG.exec().submit {
    Some(chain) => {
        chain.write_by.as_nanos() == vike_bridge_core::exec_actor::MAX_SUBMIT_WAIT.as_nanos()
            && chain.calls.len() == 3
            && chain.calls[0].each.as_nanos() == vike_transport::http::DEFAULT_TIMEOUT.as_nanos()
            && chain.calls[1].count == CONFIRM_ATTEMPTS
            && chain.calls[1].each.as_nanos() == crate::transport::CONFIRM_CALL_TIMEOUT.as_nanos()
            && chain.calls[2].count == CONFIRM_ATTEMPTS - 1
            && chain.calls[2].each.as_nanos() == CONFIRM_BACKOFF.as_nanos()
            && matches!(chain.hold, Some(hold) if hold.as_nanos() == SUBMIT_HOLD.as_nanos())
            && SUBMIT_HOLD.as_nanos()
                == vike_transport::http::DEFAULT_TIMEOUT.as_nanos()
                    + crate::transport::CONFIRM_CALL_TIMEOUT.as_nanos()
    }
    None => false,
});

/// `GET /confirms/{deal_ref}`, retried a BOUNDED number of times, and never once `out_of_time` says the next ask
/// could not end in time.
///
/// Generic over the fetch (and the pause between attempts) so the LADDER — how many attempts, how
/// many pauses, and that it stops — is assertable offline without sleeping; the production caller
/// passes the session's own GET and a real sleep. The ladder is `vike_utility::retry::retry_on_pauses`.
fn confirm_with_retry_using<F, P, T>(
    deal_ref: &str,
    mut fetch: F,
    mut pause: P,
    out_of_time: T,
) -> Result<serde_json::Value, VenueApiHttpError>
where
    F: FnMut(&str) -> Result<serde_json::Value, VenueApiHttpError>,
    P: FnMut(),
    T: Fn() -> bool,
{
    vike_utility::retry::retry_on_pauses(
        &[CONFIRM_BACKOFF; CONFIRM_ATTEMPTS as usize - 1],
        |attempt| {
            let answer = fetch(deal_ref);
            if let Err(e) = &answer {
                tracing::warn!(
                    target: "vike_ig::execution",
                    %deal_ref,
                    attempt,
                    of = CONFIRM_ATTEMPTS,
                    status = e.status,
                    error = %e.message,
                    "IG /confirms did not answer; retrying"
                );
            }
            answer
        },
        |answer| answer.is_ok() || out_of_time(),
        // `confirm_with_retry`'s pause sleeps `CONFIRM_BACKOFF`, the one entry of this schedule
        |_| pause(),
    )
}

/// The confirm ladder with no deadline: a cancel's, an amend's and the confirm watchdog's, none of which an order's
/// last-resort reject waits on.
fn confirm_with_retry(
    session: &IgSession,
    deal_ref: &str,
) -> Result<serde_json::Value, VenueApiHttpError> {
    confirm_with_retry_using(
        deal_ref,
        |r| session.confirm(r),
        || std::thread::sleep(CONFIRM_BACKOFF),
        || false,
    )
}

/// A submit's confirm ladder, asked again only while the next ask (its pause and its bound) still ends by
/// `held_until` ([`SUBMIT_HOLD`] from the POST).
fn confirm_within(
    session: &IgSession,
    deal_ref: &str,
    held_until: Instant,
) -> Result<serde_json::Value, VenueApiHttpError> {
    confirm_with_retry_using(
        deal_ref,
        |r| session.confirm(r),
        || std::thread::sleep(CONFIRM_BACKOFF),
        || next_confirm_misses(Instant::now(), held_until),
    )
}

/// Would one more confirm, its pause and its bound, end past `held_until` if asked at `now`?
fn next_confirm_misses(now: Instant, held_until: Instant) -> bool {
    now + CONFIRM_BACKOFF + crate::transport::CONFIRM_CALL_TIMEOUT > held_until
}

/// What a submit resolves to when its `/confirms` call never answered — an **`OrderAccepted` with
/// no venue id**, and never a terminal.
///
/// ⚠ **This is the bug that made local state FALSE rather than merely stuck.** The `Err` arm here
/// used to emit `OrderRejected` carrying the confirm's error text. IG had already accepted the
/// deal — the submit POST returned a `dealReference`, which is IG saying "I have this" — so a
/// dropped network reply, a timed-out read or a 404 from a reference IG had not finished processing
/// all published a TERMINAL rejection over an order that was live and very possibly FILLED. The
/// strategy then believed it was flat while holding a position, and reconciliation would not
/// necessarily catch it inside the lookback.
///
/// An inconclusive re-query becomes an OPTIMISTIC managed order, never a false terminal (this bridge's
/// own: `vike_bridge_core::resolve_ambiguous_submit` says nothing there, because no reference proves its
/// POST arrived, where IG's `dealReference` does). Being STUCK is recoverable — three separate mechanisms are now
/// pointed at it: the exec thread asking `/confirms` again ([`Unsettled`], which teaches the start's `order_client_id <-> dealId` map ([`spawn_sharing`]) and turns a
/// late `REJECTED` into `OrderCanceled` in IG's words), the Lightstreamer `OPU` of the position this very
/// `dealReference` opens (recorded before this call, so it correlates), and reconcile. Being FALSE is recoverable
/// by none of them, because nothing is looking for an order everybody agrees is finished.
pub fn unresolved_confirm(
    order_client_id: &str,
    ts: i64,
    deal_ref: &str,
    err: &VenueApiHttpError,
) -> Vec<Event> {
    tracing::error!(
        target: "vike_ig::execution",
        venue = VENUE,
        %order_client_id,
        %deal_ref,
        status = err.status,
        error = %err.message,
        "IG accepted the deal but its /confirms never answered — resolving OPTIMISTICALLY as \
         accepted-with-no-venue-id. This order's fate is UNKNOWN: it may already have filled. It \
         stays live for the trade stream, the confirm watchdog and reconcile to settle; a terminal \
         rejection here would tell the strategy it is flat while it holds a position"
    );
    vec![Event::OrderAccepted(OrderAccepted {
        order_client_id: order_client_id.to_string(),
        venue_order_id: None,
        timestamp: ts,
    })]
}

/// The next command, and until it comes the unsettled orders are asked about, one read between two commands: a stream of
/// commands cannot starve them, and they hold no command up for more than the one read. `None` when the channel closed.
fn next_command(
    rx: &Receiver<ExecutionClientActorCommand>,
    unsettled: &mut Unsettled,
    session: &IgSession,
    deal_refs: &OrderVenueIdRegistryDealReference,
    deal_ids: &Arc<Mutex<OrderVenueIdRegistry<String>>>,
    events: &EventSender,
) -> Option<ExecutionClientActorCommand> {
    loop {
        let settled = unsettled.ask_due(
            Instant::now(),
            vike_model::now_ms(),
            deal_refs,
            deal_ids,
            |r| session.get(&format!("/confirms/{r}"), "1", ""),
            |since_ms| {
                let from = crate::reconcile::ms_to_ig_datetime(since_ms);
                session
                    .get(
                        "/history/activity",
                        "3",
                        &format!("from={from}&detailed=true&pageSize=50"),
                    )
                    .map(|body| body.to_string())
                    .map_err(|e| e.to_string())
            },
            || {
                session
                    .get("/workingorders", "2", "")
                    .map(|body| body.to_string())
                    .map_err(|e| e.to_string())
            },
        );
        for event in settled {
            let _ = events.blocking_send(event);
        }
        let Some(due) = unsettled.next_due() else { return rx.recv().ok() };
        match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            Ok(cmd) => return Some(cmd),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Does a `/confirms` reply ECHO the reference we asked about? IG answers 404 for both "not
/// processed yet" and "never seen", so only a 200 that names our `dealReference` (or, naming none,
/// carries a `dealId`) proves the venue holds the order.
fn confirm_echoes(confirm: &serde_json::Value, deal_ref: &str) -> bool {
    match confirm.get("dealReference").and_then(|r| r.as_str()) {
        Some(r) => r == deal_ref,
        None => confirm.get("dealId").and_then(|d| d.as_str()).is_some_and(|d| !d.is_empty()),
    }
}

/// What a submit's `/confirms` ladder resolves to. `acked`: IG answered the POST with a
/// `dealReference`, so it demonstrably holds the order and an exhausted ladder resolves
/// optimistically ([`unresolved_confirm`]). NOT `acked` (the POST outcome was ambiguous: a timeout
/// before any reply, a reset, a gateway 5xx): the ladder's 404 means "not processed yet" OR "never
/// seen" and cannot tell them apart, so an exhausted ladder emits NOTHING and the order stays
/// `Submitted` for the stuck-order watchdog (stage 2) and reconcile. An id-less accept there would
/// take a possibly never-received order OUT of the watchdog's view (it watches only
/// Initialized/Submitted): a phantom "live" order, a protective stop that looks like protection.
/// Only an echo-confirmed reply ([`confirm_echoes`]) may accept or reject it.
pub(crate) fn submit_outcome(
    order_client_id: &str,
    ts: i64,
    market: bool,
    deal_ref: &str,
    acked: bool,
    confirmed: &Result<serde_json::Value, VenueApiHttpError>,
) -> Vec<Event> {
    match confirmed {
        Ok(confirm) if acked || confirm_echoes(confirm, deal_ref) => {
            map_confirm(order_client_id, ts, market, confirm)
        }
        Err(e) if acked => unresolved_confirm(order_client_id, ts, deal_ref, e),
        _ => {
            tracing::warn!(
                target: "vike_ig::execution", venue = VENUE, %order_client_id, %deal_ref,
                "IG create outcome still unknown after the /confirms ladder - emitting nothing; the order stays Submitted for the stuck-order watchdog and reconcile"
            );
            Vec::new()
        }
    }
}

fn run(
    config: IgConfig,
    events: EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    deal_refs: OrderVenueIdRegistryDealReference,
    deal_ids: Arc<Mutex<OrderVenueIdRegistry<String>>>,
    stop: Arc<AtomicBool>,
    live: SharedSession,
    link: SessionLink,
    fence: &SubmitFence,
    // What the account's start states of its mount: its fallback epic, and the user name it read no value for.
    (fallback, unvalued): (Option<&FallbackEpic>, Option<&str>),
) {
    // Returning here closes the command channel, which is what makes `ExecutionClientActor` synthesize the
    // terminal `OrderRejected` for every subsequent command (the no-silent-vanish contract). The
    // helper's log is what turns "a venue that rejects everything" into a diagnosable cause — the
    // old `Err(_) => return` said nothing at all.
    let session = match login_with_retry(&config, &stop, "exec") {
        Ok(session) => session,
        // A client that is stopping says nothing: the cell may be its successor's by now.
        Err(_) if stop.load(std::sync::atomic::Ordering::Relaxed) => return,
        Err(why) => {
            link.rest_login_lost(&why);
            return;
        }
    };
    // From here every authenticated call of this session (the confirm worker's included) states IG's answer.
    link.rest_answered();
    // An operator's read of the account borrows this session rather than logging in (`crate::reconcile::venue_read`).
    link.hold_session(&session);
    session.report_to(link);
    // Publish it for the confirm worker. Done HERE rather than at spawn so a start still costs the
    // one login it always did — `confirm` borrows this thread's session instead of opening another.
    *live.lock().unwrap() = Some(session.clone());
    // epic -> its order `currencyCode` and `expiry`, each epic's table read once (`epic_wire`); and, for a CLOSE of an
    // epic this process never opened, the `expiry` alone (`close_expiry`).
    let mut wires: HashMap<String, EpicWire> = HashMap::new();
    let mut close_expiries: HashMap<String, String> = HashMap::new();
    let mut unsettled = Unsettled::default();

    while let Some(cmd) = next_command(rx, &mut unsettled, &session, &deal_refs, &deal_ids, &events)
    {
        match cmd {
            ExecutionClientActorCommand::Submit(req, queued_at) => {
                // Its USER name is rewritten to its kept row's epic below (decision 0290).
                let mut req = req;
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    order_client_id: req.order_client_id.clone(),
                    timestamp: event_ts(req.timestamp),
                }));
                // The stream's events for this order wait until the arm ends (by any path, a panic included) and its
                // own answer is sent: the guard lets them go when it drops, after whatever the arm sent.
                let _fenced = fence.enter(&req.order_client_id);
                let reject = |reason: String, refusal: OrderRefusalId| {
                    Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: req.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: event_ts(req.timestamp),
                        refusal_id: vike_model::events::refusal_id_of(refusal.word()),
                    })
                };
                // A kind this bridge builds no order for, then an order too old to send: refused before it
                // costs a read. A cancel and a close have no age arm.
                if let Some(why) = unbuilt_kind(&req) {
                    let _ = events.blocking_send(reject(why, OrderRefusalId::VenueCapability));
                    continue;
                }
                if let Some(why) = too_old_to_send(&req, queued_at) {
                    let _ = events.blocking_send(reject(why, OrderRefusalId::NotSent));
                    continue;
                }
                // The engine's USER name is written on the epic of its kept row (decision 0290); from here every
                // read and body is the epic's. A name no kept row names, or a row whose name is no epic, is refused
                // in words rather than sent to be answered by an HTTP 500 that names nothing.
                if unvalued == Some(req.symbol.as_str()) {
                    let why = unvalued_mount_refusal(&req.symbol);
                    let _ = events.blocking_send(reject(why, OrderRefusalId::NotSent));
                    continue;
                }
                match crate::epic::epic_of(&req.symbol) {
                    Ok(epic) => req.symbol = epic,
                    Err(why) => {
                        tracing::warn!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                            symbol = %req.symbol, "refusing an order no IG epic can be written for: {why}"
                        );
                        let _ = events.blocking_send(reject(why, OrderRefusalId::NotSent));
                        continue;
                    }
                }
                // ⚠ `reduce_only` is the ROUTE, not a flag. IG's `/positions/otc` POST always OPENS
                // (this bridge sends `forceOpen: true`), so before this split an order asking to
                // flatten opened a SECOND, opposing deal — paying the spread twice and holding both
                // legs instead of getting flat. Closing is a different endpoint.
                let closing = req.reduce_only;
                if closing && !is_market(&req) {
                    // Loud, terminal, and never a silent wrong action: IG's close endpoint executes
                    // now — measured, not assumed ([`CLOSE_MARKET_ONLY`]'s doc carries the probe) —
                    // so there is no resting close to build. The alternatives were both silent
                    // defects: the old behaviour (a working order that OPENS an opposing deal when
                    // it triggers) or an immediate close wearing a limit price.
                    let why = CLOSE_MARKET_ONLY.to_string();
                    let _ = events.blocking_send(reject(why, OrderRefusalId::VenueCapability));
                    continue;
                }
                let market = is_market(&req);
                let built = if closing {
                    let expiry =
                        close_expiry(&session, &wires, &mut close_expiries, &req.symbol, fallback);
                    build_close_request_expiring(&req, &expiry)
                } else {
                    // The currency and the expiry are the epic's own, not constants: see `order_currency`.
                    epic_wire(&session, &mut wires, &req.symbol, fallback)
                        .inspect_err(|why| {
                            // The one line of an epic refused for its wire (log once, where the decision is made).
                            tracing::warn!(
                                target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                                epic = %req.symbol, refusal = %why, "the order is refused before it is sent"
                            );
                        })
                        .and_then(|wire| build_request_expiring(&req, &wire.currency, &wire.expiry))
                };
                let (path, version, body) = match built {
                    Ok(built) => built,
                    // The epic's table unread or naming no currency, or a SHARES market: all before the POST (a kind
                    // the builders refuse was refused above).
                    Err(why) => {
                        let _ = events.blocking_send(reject(why, OrderRefusalId::NotSent));
                        continue;
                    }
                };
                // Asked again: the epic's reads above can spend longer than the order had left, and the wait bounds
                // the POST. A close has no age.
                if let Some(why) = too_old_to_send(&req, queued_at) {
                    let _ = events.blocking_send(reject(why, OrderRefusalId::NotSent));
                    continue;
                }
                // Recorded BEFORE the POST so the reference of a create whose reply is lost still
                // correlates a streamed `OPU` and `confirm` can re-ask IG after the ladder.
                let own_ref = (!closing).then(|| deal_reference(&req.order_client_id));
                if let Some(r) = &own_ref {
                    deal_refs.lock().unwrap().record(&req.order_client_id, r, market);
                }
                let posted_ms = vike_model::now_ms();
                let held_until = Instant::now() + SUBMIT_HOLD;
                let posted = if closing {
                    session.post_method_delete(&path, version, &body)
                } else {
                    session.post(&path, version, &body)
                };
                // Did IG demonstrably receive the order? Only a POST it ANSWERED says so; an
                // ambiguous failure (timeout, reset, gateway 5xx) does not, and then only an
                // echo-confirmed `/confirms` reply may produce an event (`submit_outcome`).
                let acked = posted.is_ok();
                let deal_ref = match posted {
                    // A 200 that names no reference is still IG taking the order, and an open
                    // carries a reference of our own to ask about.
                    Ok(response) => response
                        .get("dealReference")
                        .and_then(|r| r.as_str())
                        .map(str::to_string)
                        .or_else(|| own_ref.clone()),
                    // The request may have reached IG (timeout / reset after the write, a gateway
                    // 5xx, an unreadable reply): the deal may be LIVE or filled. An open carries a
                    // reference of ours, so ask IG about THAT through the same confirm ladder a
                    // normal create uses; its failure arm is already the optimistic, never-terminal
                    // `unresolved_confirm`.
                    Err(e) if e.outcome_unknown() => {
                        let Some(own_ref) = own_ref else {
                            // A close sends no reference (IG's close body has no such field), so there
                            // is no confirm to ask for: the activity history is asked instead. A reject
                            // here could strand a closed position.
                            tracing::error!(
                                target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                                epic = %req.symbol, status = e.status, error = %e.message,
                                "IG close outcome UNKNOWN (no dealReference to ask) - not rejecting; asking the activity history, and reconcile and the stuck-order watchdog behind it"
                            );
                            unsettled.note_close(&req, posted_ms, Instant::now());
                            continue;
                        };
                        tracing::warn!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                            epic = %req.symbol, status = e.status, error = %e.message,
                            deal_ref = %own_ref,
                            "IG order create outcome unknown - asking /confirms with our own dealReference"
                        );
                        Some(own_ref)
                    }
                    Err(e) => {
                        // IG's own reason (`errorCode`, e.g. `error.service.create.working.order
                        // .generic`) reaches the rejection AND the log: the rejection alone left an
                        // operator reading the journal to learn why a create failed.
                        tracing::warn!(
                            target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                            epic = %req.symbol, status = e.status, error = %e.message,
                            "IG refused the order create"
                        );
                        deal_refs.lock().unwrap().forget(&req.order_client_id);
                        let refusal = e.to_venue_error().refusal();
                        let _ = events.blocking_send(reject(e.to_string(), refusal));
                        continue;
                    }
                };
                // Only a close gets here (an open falls back to its own reference): IG answered 200 and did not
                // say which deal, so whether it closed is as unknown as if the answer had been lost.
                let Some(deal_ref) = deal_ref else {
                    tracing::error!(
                        target: "vike_ig::execution", venue = VENUE, order_client_id = %req.order_client_id,
                        epic = %req.symbol,
                        "IG answered a close with no dealReference - not rejecting; asking the activity history"
                    );
                    unsettled.note_close(&req, posted_ms, Instant::now());
                    continue;
                };
                // Record the dealReference BEFORE fetching the confirm, in BOTH directions: the
                // Lightstreamer stream needs `ref -> order_client_id` to correlate a later fill, and
                // `ExecutionClient::confirm` needs `order_client_id -> ref` to re-ask IG about this order.
                deal_refs.lock().unwrap().record(&req.order_client_id, &deal_ref, market);
                let confirmed = confirm_within(&session, &deal_ref, held_until);
                let out = submit_outcome(
                    &req.order_client_id,
                    // Stamped when the confirm ANSWERED (a fill's time), not when the order was queued.
                    event_ts(req.timestamp),
                    market,
                    &deal_ref,
                    acked,
                    &confirmed,
                );
                if let (Ok(confirm), false) = (&confirmed, out.is_empty()) {
                    note_deal_id(&deal_ids, &req.order_client_id, confirm);
                }
                // The ladder settled nothing: its answer was an id-less accept, or nothing at all.
                if confirmed.is_err() || out.is_empty() {
                    unsettled.note_confirm(
                        &req.order_client_id,
                        &deal_ref,
                        market,
                        acked,
                        confirmed.is_err() && acked,
                        Instant::now(),
                    );
                }
                // A rejected order is never asked about again: do not keep its references forever.
                if out.iter().any(|e| matches!(e, Event::OrderRejected(_))) {
                    deal_refs.lock().unwrap().forget(&req.order_client_id);
                }
                for event in out {
                    let _ = events.blocking_send(event);
                }
            }
            ExecutionClientActorCommand::Cancel { order_client_id, .. } => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                // The outcome is IG's CONFIRM of the DELETE, not the DELETE's HTTP 200
                // (`lifecycle::cancel_working_order`).
                let deal_id = deal_ids.lock().unwrap().venue_order_id_of(&order_client_id).cloned();
                // Marked while it is sent (`sending`): a panic inside leaves it of unknown outcome, which the dead exec
                // thread answers. One of unknown outcome is read back between commands (`Unsettled::note_cancel`).
                let answer = match deal_id {
                    Some(deal_id) => {
                        sending(Sending::Cancels(vec![order_client_id.clone()]), || {
                            cancel_working_order(
                                &deal_refs,
                                &mut unsettled,
                                &order_client_id,
                                &deal_id,
                                |path| session.delete(path, "2"),
                                |deal_ref| confirm_with_retry(&session, deal_ref),
                            )
                        })
                    }
                    None => Some(cancel_event(
                        &order_client_id,
                        ExecutionClientActorCancelOutcome::Rejected(
                            format!("no working order for {order_client_id}"),
                            OrderRefusalId::NotSent,
                        ),
                    )),
                };
                // `None`: the trade stream already ended the order with its own `OrderCanceled`.
                if matches!(answer, Some(Event::OrderCanceled(_)) | None) {
                    deal_ids.lock().unwrap().forget(&order_client_id);
                }
                if let Some(event) = answer {
                    let _ = events.blocking_send(event);
                }
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecutionClientActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            ExecutionClientActorCommand::CancelBatch { order_client_ids, .. } => {
                cancel_batch_undeclared(&events, &order_client_ids)
            }
            // Unreachable the same way: this venue declares no bulk submit and no queued adopt.
            ExecutionClientActorCommand::SubmitBatch(requests, _) => {
                vike_bridge_core::exec_actor::submit_batch_undeclared(&events, &requests)
            }
            ExecutionClientActorCommand::Adopt(order) => {
                vike_bridge_core::exec_actor::adopt_undeclared(&order)
            }
            // Price-only amend of a resting LIMIT/STOP via `PUT /workingorders/otc/{dealId}`: ONE
            // explicit event, `OrderModified` on IG's AMENDED confirm, else a non-terminal
            // `OrderModifyRejected` carrying the reason (a size change included). ⚠ The PUT's own
            // `dealReference` is NOT recorded in `deal_refs` — see `lifecycle`'s module doc.
            ExecutionClientActorCommand::Modify {
                order, new_qty, new_price, queued_at, ..
            } => {
                // Past `MAX_MODIFY_WAIT` on this queue: refused unsent, as the shared `run_loop` does, so a backlog of
                // amends costs no call and the order keeps its level. Nothing waits between here and the PUT.
                if let Some(why) = too_old_to_amend(queued_at) {
                    let _ = events.blocking_send(Event::OrderModifyRejected(OrderModifyRejected {
                        order_client_id: order.order_client_id.clone(),
                        reason: why.into(),
                        timestamp: vike_bridge_core::rest::amend_answer_time(None),
                        refusal_id: vike_model::events::refusal_id_of(
                            vike_model::OrderRefusalId::NotSent.word(),
                        ),
                    }));
                    continue;
                }
                let deal_id =
                    deal_ids.lock().unwrap().venue_order_id_of(&order.order_client_id).cloned();
                let (event, level_unknown) =
                    sending(Sending::Amend(order.order_client_id.clone()), || {
                        amend_working_order(
                            &order,
                            new_qty,
                            new_price,
                            deal_id.as_deref(),
                            |path, body| session.put(path, "2", body),
                            |deal_ref| confirm_with_retry(&session, deal_ref),
                        )
                    });
                // IG may have moved the level with nothing to say so: its working-order list is read for it.
                if let (true, Some(deal_id)) = (level_unknown, deal_id.as_deref()) {
                    unsettled.note_amend(&order, deal_id, Instant::now());
                } else if matches!(event, Event::OrderModified(_)) {
                    unsettled.forget_amend(&order.order_client_id);
                }
                let _ = events.blocking_send(event);
            }
            ExecutionClientActorCommand::Shutdown => break,
        }
    }
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
