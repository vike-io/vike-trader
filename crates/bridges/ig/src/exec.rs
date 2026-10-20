//! IG exec half — `ExecutionClient` over positions + working orders.
//!
//! Order I/O runs on ONE dedicated thread owning a logged-in [`IgSession`] (blocking REST off the
//! single-writer core). Each submit is followed by `GET /confirms/{dealReference}` to resolve
//! accept/fill/reject. An OPEN sends a `dealReference` derived from its coid (`deal_reference`) and records it
//! before the POST, so a create whose reply is lost is asked about through the same `/confirms` ladder
//! (`IgApiError::outcome_unknown`), never rejected: an echo-confirmed reply resolves it, an exhausted
//! ladder leaves it `Submitted` for the stuck-order watchdog (`submit_outcome`). A failure PROVABLY before
//! the write (`vike_bridge_core::transport::send_error_is_definite`) is a clean `OrderRejected`. `client_order_id → dealId` is tracked ([`DealIds`], shared with the confirm
//! watchdog and the reconcile client) so cancel can `DELETE /workingorders/otc/{dealId}` and
//! modify can `PUT` it. Cancel and modify are each answered by their own `/confirms` reply, not by
//! the HTTP 200 — `crate::exec::lifecycle` carries those decisions.
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
//! `dealId`, and `crate::exec::event_mapper::confirm_trade_id` carries the trap that makes a close
//! actually BOOK once it is sent.
//!
//! `OrderRequest.symbol` is taken to be the IG **epic** (symbol→epic market search is a follow-up);
//! a symbol that cannot be one (no dots: `EURUSD`) is refused before the network
//! ([`lifecycle::non_epic_refusal`]). The Lightstreamer trade-update stream (see `stream.rs`) carries the delayed working-order fills.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::json;
use vike_bridge_core::exec_actor::{
    CancelOutcome, ConfirmFn, ExecActor, ExecCommand, cancel_batch_undeclared, cancel_event,
};
use vike_exec::{EventSender, ExecutionClient};
use vike_model::events::{
    Event, FillEvent, OrderAccepted, OrderFilled, OrderRejected, OrderSubmitted, TradeId,
};
use vike_model::{OrderRequest, OrderType};

use crate::config::IgConfig;
use crate::exec::lifecycle::{amend_working_order, cancel_working_order, non_epic_refusal};
use crate::exec::stream::{DealIdMap, DealIds, DealRefMap, DealRefs, LsParams, stream_trades};
use crate::market_facts::{WIRE_CURRENCY, WIRE_EXPIRY};
use crate::rest::{IgApiError, IgSession};

pub(crate) mod event_mapper;
pub(crate) mod lifecycle;
pub(crate) mod stream;

const VENUE: &str = "ig";

/// Live IG exec client. `submit`/`cancel` enqueue onto the session thread (non-blocking); a second
/// thread drives the Lightstreamer trade-update stream (delayed working-order fills + audit-A3
/// snapshot recovery on reconnect). Dropping it stops both.
pub struct IgExecutionClient(ExecActor);

impl IgExecutionClient {
    /// Hand this client the HALT sentinel it watches: the venue's start passes
    /// `AdapterInputs::process.halt_path`, the one file `vike-tradehub-core` resolved for the whole process
    /// (`vike_bridge_core::exec_actor::ExecActor::with_halt_path` carries the contract). A client
    /// built without it watches NOTHING — `crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
    /// every live start calls this.
    #[must_use]
    pub fn with_halt_path(self, path: std::path::PathBuf) -> Self {
        Self(self.0.with_halt_path(path))
    }

    pub fn spawn(config: IgConfig, events: EventSender) -> Self {
        Self::spawn_sharing(config, events, DealIds::shared())
    }

    /// [`Self::spawn`] over a `coid <-> dealId` map the caller also hands the reconcile client
    /// (`crate::recon_client::recon_client_sharing`), so a live working order of THIS process is
    /// reported under the client order id it was placed with.
    pub(crate) fn spawn_sharing(
        config: IgConfig,
        events: EventSender,
        deal_ids: DealIdMap,
    ) -> Self {
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
            let (live, deal_refs, deal_ids) = (live.clone(), deal_refs.clone(), deal_ids.clone());
            Arc::new(move |coid: &str| confirm_order(&live, &deal_refs, &deal_ids, coid))
        };
        let actor = {
            let (deal_refs, stop, live) = (deal_refs.clone(), stop.clone(), live.clone());
            ExecActor::spawn("ig-exec", events.clone(), move |rx| {
                run(config, events, rx, deal_refs, deal_ids, stop, live)
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
    /// ⚠ Forwarded, never inherited: the trait default is an EMPTY `modify`, so omitting this made
    /// every IG amend vanish with no event while `amend_working_order` sat unreachable (v0.1.62).
    /// `crates/vike-ops/tests/venues/modify_forwarded_gate.rs` holds it for every `supports_modify` venue.
    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.0.modify(order, new_qty, new_price)
    }
    /// The confirm-grace watchdog's prod. Forwards to the actor, which runs [`confirm_order`] on
    /// its own short-lived worker thread — never on the core fold. Without this forward the trait
    /// default made IG's `confirm` a no-op, so an order left live by an unanswered `/confirms` had
    /// nothing actively asking IG about it.
    fn confirm(&mut self, client_order_id: &str) {
        self.0.confirm(client_order_id)
    }
    /// Phase-one teardown seam — raise the stop flags, join nothing. See
    /// `crates/vike-exec/src/execution_engine/client.rs`'s `ExecutionClient::begin_detach`.
    /// ⚠ It MUST be delegated like every other method on this wrapper: a newtype that omits it
    /// inherits the trait's no-op, and the core's raise-all phase then skips this venue entirely
    /// while `detach` below still pays its full wind-down.
    fn begin_detach(&mut self) {
        self.0.begin_detach()
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
///
/// A confirm that DOES answer also teaches `deal_ids` the order's `dealId` — this is the only way an
/// order whose submit-time confirm 404'd ever becomes cancellable or amendable.
fn confirm_order(
    live: &SharedSession,
    deal_refs: &DealRefMap,
    deal_ids: &DealIdMap,
    coid: &str,
) -> Vec<Event> {
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
        Ok(confirm) => {
            note_deal_id(deal_ids, coid, &confirm);
            map_confirm(coid, vike_model::now_ms(), pending.market, &confirm)
        }
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

/// Teach `deal_ids` the `dealId` of `coid`'s order from a `/confirms` reply — unless that reply is a
/// REJECTION, whose deal does not exist and must never become something cancel could address.
fn note_deal_id(deal_ids: &DealIdMap, coid: &str, confirm: &serde_json::Value) {
    if confirm.get("dealStatus").and_then(|s| s.as_str()) == Some("REJECTED") {
        return;
    }
    if let Some(deal_id) = confirm.get("dealId").and_then(|d| d.as_str()) {
        deal_ids.lock().unwrap().record(coid, deal_id);
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
    if side >= 0 { "BUY" } else { "SELL" }
}

fn fmt_level(p: f64) -> String {
    format!("{p:.5}")
}

/// Every kind but `limit` and `stop` goes to the market endpoint, exactly as the string test did
/// (stop_limit, take_profit and an unrecognised word are refused by the capability preflight
/// first).
fn is_market(req: &OrderRequest) -> bool {
    !matches!(req.order_type, OrderType::Limit | OrderType::Stop)
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
/// both silently wrong. `tests/offline/close_limit_never_rests.rs` carries the verbatim wire table and
/// pins both mappers over the captured rejection.
pub(crate) const CLOSE_MARKET_ONLY: &str = "ig: a reduce_only close is MARKET-only (measured: IG's close endpoint cannot rest an order — \
     its LIMIT arm executes at level-or-better NOW and it has no STOP arm); submit it as \
     order_type=market";

/// The `dealReference` this bridge SENDS on an open (IG accepts a client-supplied one, `[A-Za-z0-9_-]`,
/// at most 30 characters), derived from the coid: FNV-1a 64 so it is the same in every process and
/// release (`DefaultHasher` promises neither). It is what makes a submit whose reply was lost
/// resolvable: IG's `/confirms/{ref}` is the only per-order read it has, and without a reference of
/// our own a lost POST reply leaves nothing to ask about.
pub(crate) fn deal_reference(coid: &str) -> String {
    let h = coid
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
        "expiry": WIRE_EXPIRY,
        "direction": direction(req.side),
        "size": req.qty,
        "orderType": "MARKET",
    });
    ("/positions/otc".to_string(), "1", body)
}

/// The `currencyCode` an order on `epic` is written in, from the epic's own `GET /markets/{epic}` body.
///
/// IG restricts the field to `instrument.currencies[]` ("Restricted to available instrument currencies"), and
/// an FX Mini lists its QUOTE currency ALONE (measured on the demo gateway, 2026-10-19: EURUSD lists `USD`,
/// EURGBP `GBP`, USDJPY `JPY`), so a constant `USD` was a terminal reject for every epic not quoted in dollars
/// (`INSTRUMENT_NOT_TRADEABLE_IN_THIS_CURRENCY`, expected from the reference, not yet observed). The choice:
/// [`WIRE_CURRENCY`] when listed (so an epic that has always been written `USD` still is, whatever the
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
    [Some(WIRE_CURRENCY), account]
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

/// `epic`'s order currency, its table read ONCE per epic and kept in `kept`.
///
/// ⚠ **FAILS CLOSED.** A table that cannot be READ (a transport error, a 429 on the shared non-trading
/// allowance, a 5xx) is refused in words, retryably, with an `error` log, and nothing is kept, so the next
/// order reads again: guessing [`WIRE_CURRENCY`] for an epic that lists another currency alone would send
/// the order IG's reference makes a terminal reject (never observed) or, worse, one it accepts in a currency
/// nobody chose. Two answers are exempt because a good read already stands behind them: an epic in `kept`
/// (served without a read), and the epic the mount is WIRED to (`vike_venues::venue_wiring`'s ig row, EUR/USD
/// mini, which lists USD: measured), which has always been written `USD` and keeps trading through an
/// unreadable table. A table that is read and lists no currency is a verdict ([`order_currency`]), not kept.
fn epic_currency(
    session: &IgSession,
    kept: &mut HashMap<String, String>,
    epic: &str,
) -> Result<String, String> {
    epic_currency_with(kept, epic, session.account_currency.as_deref(), || {
        session.get(&format!("/markets/{epic}"), "3", "")
    })
}

/// [`epic_currency`] over the table read `read` performs (called only when `kept` has no answer).
fn epic_currency_with(
    kept: &mut HashMap<String, String>,
    epic: &str,
    account: Option<&str>,
    read: impl FnOnce() -> Result<serde_json::Value, IgApiError>,
) -> Result<String, String> {
    if let Some(currency) = kept.get(epic) {
        return Ok(currency.clone());
    }
    let market = match read() {
        Ok(market) => market,
        Err(e) if vike_venues::venue_wiring::wired_symbol(VENUE) == Some(epic) => {
            tracing::warn!(
                target: "vike_ig::exec", venue = VENUE, %epic, status = e.status, error = %e.message,
                "could not read the wired epic's dealing currencies; writing currencyCode {WIRE_CURRENCY}, as it always was"
            );
            return Ok(WIRE_CURRENCY.to_string());
        }
        Err(e) => {
            let why = format!(
                "ig: could not read the dealing currencies of {epic} ({e}), so no currencyCode can be \
                 chosen for it; the order was refused rather than guess one: retry"
            );
            tracing::error!(
                target: "vike_ig::exec", venue = VENUE, %epic, status = e.status, error = %e.message,
                "{why}"
            );
            return Err(why);
        }
    };
    let currency = order_currency(epic, &market, account).inspect_err(|why| {
        tracing::warn!(target: "vike_ig::exec", venue = VENUE, %epic, "{why}");
    })?;
    tracing::info!(
        target: "vike_ig::exec", venue = VENUE, %epic, %currency,
        "orders on this epic are written in the currency its table lists"
    );
    kept.insert(epic.to_string(), currency.clone());
    Ok(currency)
}

/// Build the IG request that OPENS exposure: `(path, version, body)`. MARKET → open a position;
/// LIMIT/STOP → a working order. `epic` = `OrderRequest.symbol`; `currency` = [`order_currency`] of it.
///
/// ⚠ `forceOpen: true` is right HERE and only here. It is what makes an order open its own deal
/// rather than net against an existing one — correct for an open, and the reason a close cannot go
/// through this function at all (see [`build_close_request`]).
pub fn build_request(
    req: &OrderRequest,
    currency: &str,
) -> (String, &'static str, serde_json::Value) {
    let dir = direction(req.side);
    if is_market(req) {
        let body = json!({
            "epic": req.symbol,
            "direction": dir,
            "size": req.qty,
            "orderType": "MARKET",
            "currencyCode": currency,
            "expiry": WIRE_EXPIRY,
            "forceOpen": true,
            "guaranteedStop": false,
            "dealReference": deal_reference(&req.client_order_id),
        });
        ("/positions/otc".to_string(), "2", body)
    } else {
        let (otype, level) = if req.order_type == OrderType::Limit {
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
            // `vike_venues::venue_tif::venue_tif`, pinned there and in the tests below).
            // Honoring the request is a step-2 wire change behind demo smokes.
            "timeInForce": "GOOD_TILL_CANCELLED",
            "currencyCode": currency,
            "expiry": WIRE_EXPIRY,
            "guaranteedStop": false,
            "dealReference": deal_reference(&req.client_order_id),
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
        tracing::warn!(
            target: "vike_ig::exec", venue = VENUE, %coid, %reason, "IG REJECTED the deal at confirm"
        );
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
            trade_id: match crate::exec::event_mapper::confirm_trade_id(confirm) {
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
            position_side: vike_model::events::PositionSide::Both,
        };
        out.push(Event::Fill(fill.clone()));
        out.push(Event::OrderFilled(OrderFilled { client_order_id: coid.to_string(), fill, ts }));
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

/// `GET /confirms/{deal_ref}`, retried a BOUNDED number of times.
///
/// Generic over the fetch (and the pause between attempts) so the LADDER — how many attempts, how
/// many pauses, and that it stops — is assertable offline without sleeping; the production caller
/// passes the session's own GET and a real sleep.
fn confirm_with_retry_using<F, P>(
    deal_ref: &str,
    mut fetch: F,
    mut pause: P,
) -> Result<serde_json::Value, IgApiError>
where
    F: FnMut(&str) -> Result<serde_json::Value, IgApiError>,
    P: FnMut(),
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
                    pause();
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
    confirm_with_retry_using(
        deal_ref,
        |r| session.get(&format!("/confirms/{r}"), "1", ""),
        || std::thread::sleep(CONFIRM_BACKOFF),
    )
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
/// The disposition is exactly `vike_bridge_core::resolve_ambiguous_submit`'s inconclusive
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
    coid: &str,
    ts: i64,
    market: bool,
    deal_ref: &str,
    acked: bool,
    confirmed: &Result<serde_json::Value, IgApiError>,
) -> Vec<Event> {
    match confirmed {
        Ok(confirm) if acked || confirm_echoes(confirm, deal_ref) => {
            map_confirm(coid, ts, market, confirm)
        }
        Err(e) if acked => unresolved_confirm(coid, ts, deal_ref, e),
        _ => {
            tracing::warn!(
                target: "vike_ig::exec", venue = VENUE, %coid, %deal_ref,
                "IG create outcome still unknown after the /confirms ladder - emitting nothing; the order stays Submitted for the stuck-order watchdog and reconcile"
            );
            Vec::new()
        }
    }
}

fn run(
    config: IgConfig,
    events: EventSender,
    rx: Receiver<ExecCommand>,
    deal_refs: DealRefMap,
    deal_ids: DealIdMap,
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
    // Publish it for the confirm worker. Done HERE rather than at spawn so a start still costs the
    // one login it always did — `confirm` borrows this thread's session instead of opening another.
    *live.lock().unwrap() = Some(session.clone());
    // epic -> its order `currencyCode`, each epic's table read once (`epic_currency`).
    let mut currencies: HashMap<String, String> = HashMap::new();

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
                // A symbol that cannot be an epic is refused here, with the form to submit, rather
                // than sent to be answered by an HTTP 500 that names nothing.
                if let Some(refusal) = non_epic_refusal(&req.symbol) {
                    tracing::warn!(
                        target: "vike_ig::exec", venue = VENUE, coid = %req.client_order_id,
                        symbol = %req.symbol, "refusing an order whose symbol is not an IG epic"
                    );
                    let _ = events.blocking_send(reject(refusal));
                    continue;
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
                    let _ = events.blocking_send(reject(CLOSE_MARKET_ONLY.to_string()));
                    continue;
                }
                let market = is_market(&req);
                let (path, version, body) = if closing {
                    build_close_request(&req)
                } else {
                    // The currency is the epic's own, not a constant: see `order_currency`.
                    match epic_currency(&session, &mut currencies, &req.symbol) {
                        Ok(currency) => build_request(&req, &currency),
                        Err(why) => {
                            tracing::warn!(
                                target: "vike_ig::exec", venue = VENUE, coid = %req.client_order_id,
                                epic = %req.symbol, "{why}"
                            );
                            let _ = events.blocking_send(reject(why));
                            continue;
                        }
                    }
                };
                // Recorded BEFORE the POST so the reference of a create whose reply is lost still
                // correlates a streamed CONFIRMS and `confirm` can re-ask IG after the ladder.
                let own_ref = (!closing).then(|| deal_reference(&req.client_order_id));
                if let Some(r) = &own_ref {
                    deal_refs.lock().unwrap().record(&req.client_order_id, r, market);
                }
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
                    Ok(resp) => {
                        resp.get("dealReference").and_then(|r| r.as_str()).map(str::to_string)
                    }
                    // The request may have reached IG (timeout / reset after the write, a gateway
                    // 5xx, an unreadable reply): the deal may be LIVE or filled. An open carries a
                    // reference of ours, so ask IG about THAT through the same confirm ladder a
                    // normal create uses; its failure arm is already the optimistic, never-terminal
                    // `unresolved_confirm`.
                    Err(e) if e.outcome_unknown() => {
                        let Some(own_ref) = own_ref else {
                            // A close sends no reference (IG's close body has no such field), so
                            // there is nothing to ask: leave it UNKNOWN for the core's confirm
                            // watchdog and reconcile; a reject here could strand a closed position.
                            tracing::error!(
                                target: "vike_ig::exec", venue = VENUE, coid = %req.client_order_id,
                                epic = %req.symbol, status = e.status, error = %e.message,
                                "IG close outcome UNKNOWN (no dealReference to ask) - not rejecting; reconcile and the stuck-order watchdog settle it"
                            );
                            continue;
                        };
                        tracing::warn!(
                            target: "vike_ig::exec", venue = VENUE, coid = %req.client_order_id,
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
                            target: "vike_ig::exec", venue = VENUE, coid = %req.client_order_id,
                            epic = %req.symbol, status = e.status, error = %e.message,
                            "IG refused the order create"
                        );
                        deal_refs.lock().unwrap().forget(&req.client_order_id);
                        let _ = events.blocking_send(reject(e.to_string()));
                        continue;
                    }
                };
                let Some(deal_ref) = deal_ref else {
                    deal_refs.lock().unwrap().forget(&req.client_order_id);
                    let _ = events.blocking_send(reject("no dealReference".to_string()));
                    continue;
                };
                // Record the dealReference BEFORE fetching the confirm, in BOTH directions: the
                // Lightstreamer stream needs `ref -> coid` to correlate a later fill, and
                // `ExecutionClient::confirm` needs `coid -> ref` to re-ask IG about this order.
                deal_refs.lock().unwrap().record(&req.client_order_id, &deal_ref, market);
                let confirmed = confirm_with_retry(&session, &deal_ref);
                let out = submit_outcome(
                    &req.client_order_id,
                    req.ts,
                    market,
                    &deal_ref,
                    acked,
                    &confirmed,
                );
                if let (Ok(confirm), false) = (&confirmed, out.is_empty()) {
                    note_deal_id(&deal_ids, &req.client_order_id, confirm);
                }
                // A rejected order is never asked about again: do not keep its references forever.
                if out.iter().any(|e| matches!(e, Event::OrderRejected(_))) {
                    deal_refs.lock().unwrap().forget(&req.client_order_id);
                }
                for ev in out {
                    let _ = events.blocking_send(ev);
                }
            }
            ExecCommand::Cancel { client_order_id: coid, .. } => {
                // A failed/unknown cancel must not vanish (audit A2): map every outcome to an event.
                // The outcome is IG's CONFIRM of the DELETE, not the DELETE's HTTP 200
                // (`lifecycle::cancel_working_order`).
                let deal_id = deal_ids.lock().unwrap().deal_id_for(&coid);
                let outcome = match deal_id {
                    Some(deal_id) => cancel_working_order(
                        &deal_refs,
                        &coid,
                        &deal_id,
                        |path| session.delete(path, "2"),
                        |deal_ref| confirm_with_retry(&session, deal_ref),
                    ),
                    None => CancelOutcome::Rejected(format!("no working order for {coid}")),
                };
                if matches!(outcome, CancelOutcome::Canceled) {
                    deal_ids.lock().unwrap().forget(&coid);
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
            // Price-only amend of a resting LIMIT/STOP via `PUT /workingorders/otc/{dealId}`: ONE
            // explicit event, `OrderModified` on IG's AMENDED confirm, else a non-terminal
            // `OrderModifyRejected` carrying the reason (a size change included). ⚠ The PUT's own
            // `dealReference` is NOT recorded in `deal_refs` — see `lifecycle`'s module doc.
            ExecCommand::Modify { order, new_qty, new_price } => {
                let deal_id = deal_ids.lock().unwrap().deal_id_for(&order.client_order_id);
                let ev = amend_working_order(
                    &order,
                    new_qty,
                    new_price,
                    deal_id.as_deref(),
                    vike_model::now_ms(),
                    |path, body| session.put(path, "2", body),
                    |deal_ref| confirm_with_retry(&session, deal_ref),
                );
                let _ = events.blocking_send(ev);
            }
            ExecCommand::Shutdown => break,
        }
    }
}

#[path = "exec/exec_tests.rs"]
#[cfg(test)]
mod exec_tests;
