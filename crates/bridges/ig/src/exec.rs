//! IG exec half — `ExecutionClient` over positions + working orders.
//!
//! Order I/O runs on ONE dedicated thread owning a logged-in [`IgSession`] (blocking REST off the
//! single-writer core). Each submit is followed by `GET /confirms/{dealReference}` to resolve
//! accept/fill/reject. `client_order_id → dealId` is tracked so cancel can
//! `DELETE /workingorders/otc/{dealId}`.
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
//! `dealId`, and `crate::event_mapper::confirm_trade_id` carries the trap that makes a close
//! actually BOOK once it is sent.
//!
//! `OrderRequest.symbol` is taken to be the IG **epic** (symbol→epic market search is a follow-up).
//! The Lightstreamer trade-update stream (see `stream.rs`) carries the delayed working-order fills.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::{
    cancel_batch_undeclared, cancel_event, CancelOutcome, ConfirmFn, ExecActor, ExecCommand,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderFilled, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::OrderRequest;

use crate::config::IgConfig;
use crate::rest::{IgApiError, IgSession};
use crate::stream::{stream_trades, DealRefMap, DealRefs, LsParams};

const VENUE: &str = "ig";

/// Live IG exec client. `submit`/`cancel` enqueue onto the session thread (non-blocking); a second
/// thread drives the Lightstreamer trade-update stream (delayed working-order fills + audit-A3
/// snapshot recovery on reconnect). Dropping it stops both.
pub struct IgExecutionClient(ExecActor);

impl IgExecutionClient {
    pub fn spawn(config: IgConfig, events: EventSender) -> Self {
        // dealReference -> client_order_id, written by the exec thread at submit and read by the
        // stream to correlate a streamed CONFIRMS back to the submitting order.
        let deal_refs: DealRefMap = Arc::new(Mutex::new(DealRefs::default()));
        let stop = Arc::new(AtomicBool::new(false));
        // Background: the Lightstreamer trade-update stream. It logs in independently for its own
        // streaming session (IG permits it); a login failure or missing LS endpoint just exits the
        // thread — the live gate. ExecActor flag-stops + joins it on teardown.
        let stream_join = thread::Builder::new()
            .name("ig-stream".into())
            .spawn({
                let (config, events, deal_refs, stop) =
                    (config.clone(), events.clone(), deal_refs.clone(), stop.clone());
                move || {
                    let Some(session) = login_with_retry(&config, &stop, "trade-stream") else {
                        return;
                    };
                    if session.lightstreamer_endpoint.is_empty() {
                        tracing::error!(
                            target: "vike_ig::exec",
                            "IG login returned no lightstreamerEndpoint — the trade stream cannot \
                             start. Working-order fills and cancels will NOT arrive on this mount"
                        );
                        return;
                    }
                    let params = LsParams {
                        endpoint: session.lightstreamer_endpoint.clone(),
                        account_id: session.account_id.clone(),
                        session,
                    };
                    stream_trades(params, deal_refs, events, stop);
                }
            })
            .expect("spawn ig-stream thread");
        // Published by the exec thread once its login lands, so the confirm worker can re-query
        // without opening (and paying for) a session of its own. See `SharedSession`.
        let live: SharedSession = Arc::new(Mutex::new(None));
        let confirm: ConfirmFn = {
            let (live, deal_refs) = (live.clone(), deal_refs.clone());
            Arc::new(move |coid: &str| confirm_order(&live, &deal_refs, coid))
        };
        let actor = {
            let (deal_refs, stop, live) = (deal_refs.clone(), stop.clone(), live.clone());
            ExecActor::spawn("ig-exec", events.clone(), move |rx| {
                run(config, events, rx, deal_refs, stop, live)
            })
        }
        .with_confirm(confirm);
        Self(actor.with_background(stop, stream_join))
    }
}

impl ExecutionClient for IgExecutionClient {
    fn submit(&mut self, request: &OrderRequest) {
        self.0.submit(request)
    }
    fn cancel(&mut self, client_order_id: &str) {
        self.0.cancel(client_order_id)
    }
    /// The confirm-grace watchdog's prod. Forwards to the actor, which runs [`confirm_order`] on
    /// its own short-lived worker thread — never on the core fold. Without this forward the trait
    /// default made IG's `confirm` a no-op, so an order left live by an unanswered `/confirms` had
    /// nothing actively asking IG about it.
    fn confirm(&mut self, client_order_id: &str) {
        self.0.confirm(client_order_id)
    }
    fn detach(&mut self) {
        self.0.detach()
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
/// IG has no "get order by client order id" endpoint: `GET /confirms/{dealReference}` is the only
/// per-order status read it offers, so this is answerable at all only because the exec thread
/// records `coid -> dealReference` at submit ([`DealRefs::record`]).
///
/// An EMPTY result is the honest answer to every question this cannot settle — no session yet, no
/// `dealReference` for that coid (IG never acknowledged the deal, so it has nothing to confirm), or
/// a confirm that still will not answer. Per the [`ConfirmFn`] contract that means "nothing
/// conclusive", and it is deliberately not a rejection: guessing a terminal here is the same
/// phantom-position failure [`unresolved_confirm`] exists to avoid.
fn confirm_order(live: &SharedSession, deal_refs: &DealRefMap, coid: &str) -> Vec<Event> {
    let Some(session) = live.lock().unwrap().clone() else {
        return Vec::new();
    };
    let Some(pending) = deal_refs.lock().unwrap().pending_for(coid) else {
        tracing::warn!(
            target: "vike_ig::exec",
            venue = VENUE,
            %coid,
            "confirm requested for an order this process holds no dealReference for — IG has \
             nothing to be asked about it; reporting nothing conclusive"
        );
        return Vec::new();
    };
    match confirm_with_retry(&session, &pending.deal_ref) {
        Ok(confirm) => map_confirm(coid, vike_model::now_ms(), pending.market, &confirm),
        Err(e) => {
            tracing::warn!(
                target: "vike_ig::exec",
                venue = VENUE,
                %coid,
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

/// How many times a mount's login is attempted before the lane gives up and says so. Bounded rather
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
/// with nothing in the log: a mount that submits fine and never delivers a fill, or a venue that
/// rejects everything with no stated cause.
///
/// The two failure shapes are separated because only one of them is worth retrying: a **refusal**
/// (IG answered with an HTTP status — a rejected key, a locked account) will answer the same way
/// forever, so it stops at once and says why; anything else is transport-shaped and gets the
/// budget. `stop` is honoured between attempts so teardown never waits out the backoff.
fn login_with_retry(
    config: &IgConfig,
    stop: &Arc<AtomicBool>,
    lane: &'static str,
) -> Option<Arc<IgSession>> {
    for attempt in 1..=LOGIN_ATTEMPTS {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        match IgSession::login(config) {
            Ok(session) => {
                if attempt > 1 {
                    tracing::info!(
                        target: "vike_ig::exec",
                        lane,
                        attempt,
                        "IG login succeeded after retrying"
                    );
                }
                return Some(Arc::new(session));
            }
            // `status == 0` is this crate's marker for "never reached IG" (`IgSession::net_err`);
            // any real status means IG answered, and answered no.
            Err(e) if e.status != 0 => {
                tracing::error!(
                    target: "vike_ig::exec",
                    lane,
                    status = e.status,
                    error = %e.message,
                    "IG REFUSED this login — retrying cannot fix a refusal. This IG lane is DOWN \
                     for the life of the mount until the credentials are fixed"
                );
                return None;
            }
            Err(e) => {
                tracing::warn!(
                    target: "vike_ig::exec",
                    lane,
                    attempt,
                    of = LOGIN_ATTEMPTS,
                    error = %e.message,
                    "IG login failed; retrying"
                );
                vike_bridge_core::user_data::sleep_unless_stopped(stop, LOGIN_BACKOFF);
            }
        }
    }
    tracing::error!(
        target: "vike_ig::exec",
        lane,
        attempts = LOGIN_ATTEMPTS,
        "IG login exhausted its retry budget. This IG lane is DOWN for the life of the mount"
    );
    None
}

fn direction(side: i32) -> &'static str {
    if side >= 0 {
        "BUY"
    } else {
        "SELL"
    }
}

fn fmt_level(p: f64) -> String {
    format!("{p:.5}")
}

fn is_market(req: &OrderRequest) -> bool {
    !(req.order_type == "limit" || req.order_type == "stop")
}

/// The terminal reason a `reduce_only` order this client cannot express is refused with. Named so
/// the refusal is greppable from a log and assertable from a test.
///
/// ⚠ **The refusal is MEASURED, not cautious.** Probed live against `demo-api.ig.com` on
/// 2026-08-22 (`tests/ig_limit_close_probe.rs`, re-runnable;
/// `tests/fixtures/confirm_close_limit_rejected.json` is the captured reply): IG's close endpoint
/// has no resting arm at all. `orderType: LIMIT` there is execute-at-level-or-better-NOW — a
/// NON-marketable level (exactly what a resting reduce-only close would be) answers 200 with a
/// `dealReference` whose confirm is `dealStatus: REJECTED`,
/// `reason: LIMIT_ORDER_WRONG_SIDE_OF_MARKET`, resting NOTHING (`/workingorders` stays empty, the
/// position stays open), while a MARKETABLE level executes immediately at level-or-better.
/// `orderType: STOP` does not exist on that endpoint: HTTP 400 `invalid.request.orderType`, before
/// any deal is minted. So the only alternatives to this refusal are a silent immediate close
/// wearing a limit price, or the pre-#1431 defect (a working order that OPENS when it triggers) —
/// both silently wrong. `tests/close_limit_never_rests.rs` carries the verbatim wire table and
/// pins both mappers over the captured rejection.
pub const CLOSE_MARKET_ONLY: &str =
    "ig: a reduce_only close is MARKET-only (measured: IG's close endpoint cannot rest an order — \
     its LIMIT arm executes at level-or-better NOW and it has no STOP arm); submit it as \
     order_type=market";

/// Build the IG request that CLOSES exposure: `(path, version, body)` for
/// `POST /positions/otc` + `_method: DELETE` (see [`IgSession::post_method_delete`] for why the
/// verb override rather than a real `DELETE`).
///
/// ⚠ **This closes by `epic` + `expiry`, deliberately, and not by `dealId`.** IG is
/// position-per-deal: this bridge's own opens carry `forceOpen: true`, so N orders in one epic are
/// N separate deals, and a flatten has to net across all of them. Closing by `dealId` closes ONE,
/// capped at that deal's size — which means a multi-deal flatten becomes several calls, several
/// `dealReference`s, and several fills for a single `client_order_id`. IG's own mappers here are
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
pub fn build_close_request(req: &OrderRequest) -> (String, &'static str, serde_json::Value) {
    let body = json!({
        "epic": req.symbol,
        // Cash/daily only, matching `build_request`'s open — this bridge wires no dated epic.
        "expiry": "-",
        "direction": direction(req.side),
        "size": req.qty,
        "orderType": "MARKET",
    });
    ("/positions/otc".to_string(), "1", body)
}

/// Build the IG request that OPENS exposure: `(path, version, body)`. MARKET → open a position;
/// LIMIT/STOP → a working order. `epic` = `OrderRequest.symbol`.
///
/// ⚠ `forceOpen: true` is right HERE and only here. It is what makes an order open its own deal
/// rather than net against an existing one — correct for an open, and the reason a close cannot go
/// through this function at all (see [`build_close_request`]).
pub fn build_request(req: &OrderRequest) -> (String, &'static str, serde_json::Value) {
    let dir = direction(req.side);
    if is_market(req) {
        let body = json!({
            "epic": req.symbol,
            "direction": dir,
            "size": req.qty,
            "orderType": "MARKET",
            "currencyCode": "USD",
            "expiry": "-",
            "forceOpen": true,
            "guaranteedStop": false,
        });
        ("/positions/otc".to_string(), "2", body)
    } else {
        let (otype, level) = if req.order_type == "limit" {
            ("LIMIT", req.price)
        } else {
            ("STOP", req.trigger_price)
        };
        let body = json!({
            "epic": req.symbol,
            "direction": dir,
            "size": req.qty,
            "type": otype,
            "level": level.map(fmt_level),
            // tif_for step-2: `req.time_in_force` is deliberately NOT read — every working
            // order rests GOOD_TILL_CANCELLED (the Ignored ig row of
            // `vike_bridge_core::tif::venue_tif`, pinned there and in the tests below).
            // Honoring the request is a step-2 wire change behind demo smokes.
            "timeInForce": "GOOD_TILL_CANCELLED",
            "currencyCode": "USD",
            "expiry": "-",
            "guaranteedStop": false,
        });
        ("/workingorders/otc".to_string(), "2", body)
    }
}

/// Map an IG `/confirms/{ref}` response to events. `market` → a fill is emitted from the deal
/// level; a working order emits Accepted only (its fill arrives later via the stream).
// `pub` (the `exec` module stays private; re-exported `#[doc(hidden)]` at the crate root) so the
// cross-bridge conformance harness (`vike-bridge-core/tests/bridge_conformance.rs`) can drive the
// REAL accept mapper — the only production caller is `run()`'s network path. Same
// test-reachability rationale as oanda's `map_order_response`. Not part of the public API.
pub fn map_confirm(coid: &str, ts: i64, market: bool, confirm: &serde_json::Value) -> Vec<Event> {
    let mut out = Vec::new();
    let status = confirm.get("dealStatus").and_then(|s| s.as_str()).unwrap_or("");
    if status == "REJECTED" {
        let reason = confirm.get("reason").and_then(|r| r.as_str()).unwrap_or("REJECTED");
        out.push(Event::OrderRejected(OrderRejected {
            client_order_id: coid.to_string(),
            reason: reason.to_string().into(),
            ts,
        }));
        return out;
    }
    let deal_id = confirm.get("dealId").and_then(|d| d.as_str()).map(str::to_string);
    out.push(Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: deal_id.clone().map(Into::into),
        ts,
    }));
    if market {
        let level = confirm.get("level").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let size = confirm.get("size").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
        let side =
            if confirm.get("direction").and_then(|d| d.as_str()) == Some("SELL") { -1 } else { 1 };
        let symbol = confirm.get("epic").and_then(|e| e.as_str()).unwrap_or_default().to_string();
        // Dual-publish (crypto contract): bare Fill first so the Account folds position/PnL,
        // then the OrderFilled wrap so the FSM applies it — both the same FillEvent.
        let fill = FillEvent {
            // SYNTHESIZED, not dropped — and the difference from oanda is what `coid` guarantees
            // here. `/confirms/{ref}` is the synchronous confirmation of ONE deal, so this mapper
            // emits at most one fill per call and `coid` alone identifies it; re-polling the same
            // confirm therefore re-derives the SAME id and dedups, which is exactly the property a
            // synthesized id must have. (oanda's stream can carry many fills per order, so no such
            // per-fill key exists there — hence a drop there and a synthesis here.)
            // ⚠ Was `deal_id.unwrap_or_default()` = `""` on an absent dealId, which skipped dedup.
            // ⚠ ...and was `dealId` UNCONDITIONALLY, which made a CLOSE fill collide with the
            // OPENING fill of the very position it closes — IG reuses the closed deal's id on a
            // close confirm. `confirm_trade_id` owns that rule and is shared with the streamed
            // twin so the two lanes cannot drift apart; read its doc before touching this.
            trade_id: match crate::event_mapper::confirm_trade_id(confirm) {
                Some(t) => t,
                None => {
                    tracing::warn!(
                        venue = VENUE,
                        %coid,
                        "confirm carries no usable execution id — synthesizing a deterministic \
                         per-confirm trade_id from the coid so the fill still folds exactly once"
                    );
                    TradeId::prefixed("IG-CONFIRM-", coid)
                }
            },
            client_order_id: coid.to_string(),
            venue: VENUE.to_string().into(),
            symbol: symbol.into(),
            side,
            last_qty: size,
            last_px: level,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: String::new().into(),
            ts,
            mark_price: None,
            position_side: "BOTH".to_string().into(),
        };
        out.push(Event::Fill(fill.clone()));
        out.push(Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts }));
    }
    out
}

/// How many times a submit's `GET /confirms/{dealReference}` is attempted before the submit is
/// resolved without it. IG answers **404** `error.service.execution.find` for a reference it has
/// not finished processing (measured against `demo-api.ig.com`, 2026-08-21), so the first miss is
/// routine rather than an answer — retrying is what turns a race into a result.
const CONFIRM_ATTEMPTS: u32 = 3;
/// Backoff between those attempts. Small on purpose: this runs on the exec command thread (off the
/// core fold, but in front of the next command), so the whole ladder must stay well under a second.
const CONFIRM_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

/// `GET /confirms/{deal_ref}`, retried a BOUNDED number of times.
///
/// Generic over the fetch so the LADDER — how many attempts, and that it stops — is assertable
/// offline; the production caller passes the session's own GET.
fn confirm_with_retry_using<F>(
    deal_ref: &str,
    mut fetch: F,
) -> Result<serde_json::Value, IgApiError>
where
    F: FnMut(&str) -> Result<serde_json::Value, IgApiError>,
{
    let mut last = None;
    for attempt in 1..=CONFIRM_ATTEMPTS {
        match fetch(deal_ref) {
            Ok(v) => return Ok(v),
            Err(e) => {
                tracing::warn!(
                    target: "vike_ig::exec",
                    %deal_ref,
                    attempt,
                    of = CONFIRM_ATTEMPTS,
                    status = e.status,
                    error = %e.message,
                    "IG /confirms did not answer; retrying"
                );
                last = Some(e);
                if attempt < CONFIRM_ATTEMPTS {
                    std::thread::sleep(CONFIRM_BACKOFF);
                }
            }
        }
    }
    Err(last.expect("CONFIRM_ATTEMPTS is non-zero, so a failure path always recorded an error"))
}

fn confirm_with_retry(
    session: &IgSession,
    deal_ref: &str,
) -> Result<serde_json::Value, IgApiError> {
    confirm_with_retry_using(deal_ref, |r| session.get(&format!("/confirms/{r}"), "1", ""))
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
/// The disposition is exactly `vike_bridge_core::rest::resolve_ambiguous_submit`'s inconclusive
/// arm, for the same reason it exists there: an inconclusive re-query becomes an OPTIMISTIC managed
/// order, never a false terminal. Being STUCK is recoverable — three separate mechanisms are now
/// pointed at it: the Lightstreamer `CONFIRMS` for this very `dealReference` (recorded before this
/// call, so it correlates), the core's confirm-grace watchdog prodding
/// [`ExecutionClient::confirm`], and reconcile. Being FALSE is recoverable by none of them, because
/// nothing is looking for an order everybody agrees is finished.
pub fn unresolved_confirm(coid: &str, ts: i64, deal_ref: &str, err: &IgApiError) -> Vec<Event> {
    tracing::error!(
        target: "vike_ig::exec",
        venue = VENUE,
        %coid,
        %deal_ref,
        status = err.status,
        error = %err.message,
        "IG accepted the deal but its /confirms never answered — resolving OPTIMISTICALLY as \
         accepted-with-no-venue-id. This order's fate is UNKNOWN: it may already have filled. It \
         stays live for the trade stream, the confirm watchdog and reconcile to settle; a terminal \
         rejection here would tell the strategy it is flat while it holds a position"
    );
    vec![Event::OrderAccepted(OrderAccepted {
        client_order_id: coid.to_string(),
        venue_order_id: None,
        ts,
    })]
}

fn run(
    config: IgConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    deal_refs: DealRefMap,
    stop: Arc<AtomicBool>,
    live: SharedSession,
) {
    // Returning here closes the command channel, which is what makes `ExecActor` synthesize the
    // terminal `OrderRejected` for every subsequent command (the no-silent-vanish contract). The
    // helper's log is what turns "a venue that rejects everything" into a diagnosable cause — the
    // old `Err(_) => return` said nothing at all.
    let Some(session) = login_with_retry(&config, &stop, "exec") else {
        return;
    };
    // Publish it for the confirm worker. Done HERE rather than at spawn so a mount still costs the
    // one login it always did — `confirm` borrows this thread's session instead of opening another.
    *live.lock().unwrap() = Some(session.clone());
    let mut deal_ids: HashMap<String, String> = HashMap::new(); // coid -> dealId (working-order cancel)

    while let Ok(cmd) = rx.recv() {
        match cmd {
            ExecCommand::Submit(req) => {
                let _ = events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
                    client_order_id: req.client_order_id.clone(),
                    ts: req.ts,
                }));
                let reject = |reason: String| {
                    Event::OrderRejected(OrderRejected {
                        client_order_id: req.client_order_id.clone(),
                        reason: reason.into(),
                        ts: req.ts,
                    })
                };
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
                    let _ = events.blocking_send(reject(CLOSE_MARKET_ONLY.to_string()));
                    continue;
                }
                let market = is_market(&req);
                let (path, version, body) =
                    if closing { build_close_request(&req) } else { build_request(&req) };
                let posted = if closing {
                    session.post_method_delete(&path, version, &body)
                } else {
                    session.post(&path, version, &body)
                };
                let deal_ref = match posted {
                    Ok(resp) => {
                        resp.get("dealReference").and_then(|r| r.as_str()).map(str::to_string)
                    }
                    Err(e) => {
                        let _ = events.blocking_send(reject(e.to_string()));
                        continue;
                    }
                };
                let Some(deal_ref) = deal_ref else {
                    let _ = events.blocking_send(reject("no dealReference".to_string()));
                    continue;
                };
                // Record the dealReference BEFORE fetching the confirm, in BOTH directions: the
                // Lightstreamer stream needs `ref -> coid` to correlate a later fill, and
                // `ExecutionClient::confirm` needs `coid -> ref` to re-ask IG about this order.
                deal_refs.lock().unwrap().record(&req.client_order_id, &deal_ref, market);
                match confirm_with_retry(&session, &deal_ref) {
                    Ok(confirm) => {
                        if let Some(did) = confirm.get("dealId").and_then(|d| d.as_str()) {
                            deal_ids.insert(req.client_order_id.clone(), did.to_string());
                        }
                        for ev in map_confirm(&req.client_order_id, req.ts, market, &confirm) {
                            let _ = events.blocking_send(ev);
                        }
                    }
                    Err(e) => {
                        for ev in unresolved_confirm(&req.client_order_id, req.ts, &deal_ref, &e) {
                            let _ = events.blocking_send(ev);
                        }
                    }
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                let outcome = match deal_ids.get(&coid) {
                    Some(deal_id) => {
                        match session.delete(&format!("/workingorders/otc/{deal_id}"), "2") {
                            Ok(_) => CancelOutcome::Canceled,
                            Err(e) => CancelOutcome::Rejected(e.to_string()),
                        }
                    }
                    None => CancelOutcome::Rejected(format!("no working order for {coid}")),
                };
                if matches!(outcome, CancelOutcome::Canceled) {
                    deal_ids.remove(&coid);
                }
                let _ = events.blocking_send(cancel_event(&coid, outcome));
            }
            // No bulk-cancel path here, and this venue declares none — so `ExecActor` fanned the
            // batch out into the per-id `Cancel`s above before it ever reached this channel, and
            // this arm is unreachable. It exists because a new command variant is exhaustive; the
            // shared helper refuses every id NON-terminally rather than letting a future mis-wiring
            // drop them silently.
            ExecCommand::CancelBatch { client_order_ids, .. } => {
                cancel_batch_undeclared(&events, &client_order_ids)
            }
            // no native amend on this venue: a modify leaves the resting order at its terms
            ExecCommand::Modify { .. } => {}
            ExecCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(order_type: &str, side: i32, qty: f64) -> OrderRequest {
        OrderRequest {
            combo_legs: Vec::new(),
            client_order_id: "coid-1".into(),
            venue: VENUE.into(),
            symbol: "CS.D.EURUSD.MINI.IP".into(),
            side,
            qty,
            order_type: order_type.into(),
            price: Some(1.09),
            trigger_price: Some(1.08),
            reduce_only: false,
            time_in_force: vike_model::TimeInForce::Gtc,
            gtd_expiry: None,
            ts: 222,
            parent_order_id: None,
            linked_order_ids: vec![],
            order_list_id: None,
            contingency_type: None,
            weight: 0.0,
            stop: None,
            trail: None,
            extreme: None,
            on_close: false,
            margin_mode: None,
            trigger_by: None,
        }
    }

    #[test]
    fn market_opens_position() {
        let (path, ver, body) = build_request(&req("market", 1, 2.0));
        assert_eq!(path, "/positions/otc");
        assert_eq!(ver, "2");
        assert_eq!(body["orderType"], "MARKET");
        assert_eq!(body["direction"], "BUY");
        assert_eq!(body["size"], 2.0);
        assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
    }

    /// The close endpoint, its verb override, and — the part a reader will get wrong — that
    /// `direction` is the request's OWN side. `OrderIntent::Flatten` already mints the side opposite
    /// the position, so negating it here would re-open the very position it was asked to close.
    #[test]
    fn reduce_only_market_builds_the_close_body_with_the_closing_side() {
        let mut r = req("market", -1, 0.2); // flatten a LONG → a SELL close
        r.reduce_only = true;
        let (path, ver, body) = build_close_request(&r);
        assert_eq!(path, "/positions/otc");
        assert_eq!(ver, "1", "the close endpoint is Version 1, not the open's Version 2");
        assert_eq!(body["direction"], "SELL");
        assert_eq!(body["size"], 0.2);
        assert_eq!(body["orderType"], "MARKET");
        assert_eq!(body["epic"], "CS.D.EURUSD.MINI.IP");
        assert_eq!(body["expiry"], "-");
        // ⚠ A close body carries NO `forceOpen` — the field only exists on the open endpoint, and
        // sending it here is what a copy-paste from `build_request` would do.
        assert!(body.get("forceOpen").is_none(), "a close cannot carry forceOpen: {body}");
        // Closing by epic+expiry rather than dealId is what keeps a multi-deal flatten to ONE
        // confirm; see the function doc and `tests/close_does_not_collide_with_its_open.rs`.
        assert!(body.get("dealId").is_none(), "closed by epic, so IG nets across its own deals");

        // ...and a SHORT flatten is the mirror image.
        let mut long_close = req("market", 1, 0.1);
        long_close.reduce_only = true;
        assert_eq!(build_close_request(&long_close).2["direction"], "BUY");
    }

    /// An OPEN still opens — `forceOpen: true` belongs on that path and only that path.
    #[test]
    fn a_plain_market_order_still_opens_its_own_deal() {
        let (path, ver, body) = build_request(&req("market", 1, 2.0));
        assert_eq!((path.as_str(), ver), ("/positions/otc", "2"));
        assert_eq!(body["forceOpen"], true);
    }

    #[test]
    fn limit_is_working_order_with_level() {
        let (path, _ver, body) = build_request(&req("limit", -1, 1.0));
        assert_eq!(path, "/workingorders/otc");
        assert_eq!(body["type"], "LIMIT");
        assert_eq!(body["direction"], "SELL");
        assert_eq!(body["level"], "1.09000");
    }

    #[test]
    fn request_tif_is_ignored_working_orders_rest_gtc() {
        // The Ignored ig row of `vike_bridge_core::tif::venue_tif`: the request TIF is NEVER
        // read — a limit asking Ioc still rests GOOD_TILL_CANCELLED, and the MARKET body
        // carries no timeInForce at all. Honoring it is a step-2 wire change behind smokes.
        let mut r = req("limit", 1, 1.0);
        r.time_in_force = vike_model::TimeInForce::Ioc;
        let (_path, _ver, body) = build_request(&r);
        assert_eq!(body["timeInForce"], "GOOD_TILL_CANCELLED");
        assert_eq!(
            vike_bridge_core::tif::venue_tif(VENUE, vike_model::TimeInForce::Ioc),
            vike_bridge_core::tif::TifOutcome::Ignored { wire: "GOOD_TILL_CANCELLED" }
        );
        let (_path, _ver, mbody) = build_request(&req("market", 1, 1.0));
        assert!(mbody.get("timeInForce").is_none());
    }

    #[test]
    fn confirm_market_accepted_then_filled() {
        let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","reason":"SUCCESS","dealId":"DIAAA1","epic":"CS.D.EURUSD.MINI.IP",
                "direction":"BUY","size":2.0,"level":1.09300}"#,
        )
        .unwrap();
        // Dual-publish: Accepted, then bare Fill (Account folds position/PnL), then the
        // OrderFilled wrap (FSM) — both fills carrying the same dealId trade_id.
        let evs = map_confirm("coid-1", 222, true, &c);
        assert_eq!(evs.len(), 3);
        assert!(
            matches!(&evs[0], Event::OrderAccepted(a) if a.venue_order_id.as_deref() == Some("DIAAA1"))
        );
        match &evs[1] {
            Event::Fill(fill) => {
                assert_eq!(fill.side, 1);
                assert_eq!(fill.last_qty, 2.0);
                assert_eq!(fill.last_px, 1.093);
                assert_eq!(fill.trade_id, "DIAAA1");
            }
            other => panic!("expected bare Fill second, got {other:?}"),
        }
        match &evs[2] {
            Event::OrderFilled(of) => {
                assert_eq!(of.fill.side, 1);
                assert_eq!(of.fill.trade_id, "DIAAA1");
            }
            other => panic!("expected OrderFilled wrap third, got {other:?}"),
        }
    }

    #[test]
    fn confirm_working_order_accepted_only() {
        let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"ACCEPTED","dealId":"DIAAA2","epic":"CS.D.EURUSD.MINI.IP","direction":"BUY","size":1.0}"#,
        )
        .unwrap();
        let evs = map_confirm("coid-1", 222, false, &c);
        assert_eq!(evs.len(), 1); // no fill for a resting working order
        assert!(matches!(&evs[0], Event::OrderAccepted(_)));
    }

    /// IG answers 404 `error.service.execution.find` for a `dealReference` it has not finished
    /// processing, so the FIRST miss is a race rather than an answer — a submit that gave up on it
    /// would resolve optimistically for a deal IG was about to confirm normally.
    #[test]
    fn a_confirm_that_answers_on_a_later_attempt_is_used() {
        let attempts = std::cell::Cell::new(0u32);
        let got = confirm_with_retry_using("REF1", |r| {
            assert_eq!(r, "REF1", "the same reference is re-asked, never a mutated one");
            attempts.set(attempts.get() + 1);
            if attempts.get() < CONFIRM_ATTEMPTS {
                Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
            } else {
                Ok(serde_json::json!({"dealStatus": "ACCEPTED", "dealId": "DIAAA1"}))
            }
        })
        .expect("the late answer is taken");
        assert_eq!(got["dealId"], "DIAAA1");
        assert_eq!(attempts.get(), CONFIRM_ATTEMPTS);
    }

    /// ⚠ ...and it STOPS. This runs on the exec command thread, in front of every later command, so
    /// an unbounded confirm ladder would wedge the whole venue on one order IG will never confirm.
    /// The failure is handed back for [`unresolved_confirm`] to resolve — never swallowed.
    #[test]
    fn the_confirm_retry_is_bounded_and_surfaces_the_last_failure() {
        let attempts = std::cell::Cell::new(0u32);
        let err = confirm_with_retry_using("REF1", |_| {
            attempts.set(attempts.get() + 1);
            Err(IgApiError { status: 404, message: "error.service.execution.find".into() })
        })
        .expect_err("a confirm that never answers must not be reported as success");
        assert_eq!(attempts.get(), CONFIRM_ATTEMPTS, "bounded, never a loop");
        assert_eq!(err.status, 404);
        assert_eq!(err.message, "error.service.execution.find", "IG's own code reaches the log");
    }

    #[test]
    fn confirm_rejected() {
        let c: serde_json::Value = serde_json::from_str(
            r#"{"dealStatus":"REJECTED","reason":"INSUFFICIENT_BALANCE","dealId":"DIAAA3"}"#,
        )
        .unwrap();
        let evs = map_confirm("coid-1", 222, true, &c);
        assert_eq!(evs.len(), 1);
        assert!(matches!(&evs[0], Event::OrderRejected(r) if r.reason == "INSUFFICIENT_BALANCE"));
    }
}
