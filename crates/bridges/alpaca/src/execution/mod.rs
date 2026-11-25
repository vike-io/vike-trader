//! Alpaca exec — `ExecutionClient` over `/v1/trading/accounts/{id}/orders`: the shared `ExecutionClientActor`
//! ([`spawn`]) over one dedicated REST thread (`run`: the shared `run_loop` over this venue's `VenueRest`);
//! `submit`/`cancel` enqueue. `OrderSubmitted` is
//! emitted synchronously; the POST returns Accepted (fills come back async on the SSE stream — the shared
//! private-stream pump the REST thread starts and joins, see `crate::execution::user_data`). Alpaca cancels by venue
//! order id (not client id), so `cancel`
//! resolves the venue id with a fresh `orders:by_client_order_id` lookup then DELETEs it — leak-free (no
//! growing in-memory map) and restart-safe (a map would be lost on restart; a lookup isn't). The DELETE's `204`
//! only takes the request, so the SSE stream's `canceled` (or the fill that beat it) ends the order. Broker orders
//! have no venue-native amend, so a `modify` reaches `VenueRest::modify_order`'s default and does nothing
//! (cancel/replace is deferred); under HALT the actor refuses it in words first.

pub(crate) mod event_mapper;
pub(crate) mod submitted_symbols;
pub(crate) mod user_data;

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use vike_bridge_core::exec_actor::{
    DEFAULT_FILL_SENTINEL, ExecutionClientActor, ExecutionClientActorCommand,
    ExecutionClientActorLateCloses, MAX_SUBMIT_WAIT, late_close_size, run_loop, serving_since,
    too_old_to_post,
};
use vike_bridge_core::per_symbol::OrderClientIdSymbols;
use vike_bridge_core::rest::{OrderCancelAck, VenueRest, refused};
use vike_bridge_core::transport::http_error::VenueApiHttpError;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_execution::EventSender;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderRejected, OrderSubmitted};
use vike_venues::venue_timings;

use crate::config::AlpacaConfig;
use crate::execution::event_mapper::{build_order_body, map_submit_answer};
use crate::execution::submitted_symbols::submitted_symbols;
use crate::transport::AlpacaTransport;
use crate::transport::sign::{MINT_TIMEOUT, TokenSource};

// The row is this code (`vike_venues::venue_timings::ALPACA`'s `submit`, the chain the table's test holds under the
// core's last resort): the queue's wait, which the mint in front of the POST counts against (`VenueRestAlpaca`'s
// `submit_order`), then the order POST on the transport's shared agent and one re-mint on the mint's own.
const _: () = assert!(match venue_timings::ALPACA.exec().submit {
    Some(chain) => {
        chain.write_by.as_nanos() == MAX_SUBMIT_WAIT.as_nanos()
            && chain.calls.len() == 2
            && chain.calls[0].each.as_nanos() == vike_transport::http::DEFAULT_TIMEOUT.as_nanos()
            && chain.calls[1].each.as_nanos() == MINT_TIMEOUT.as_nanos()
    }
    None => false,
});

/// The live Alpaca exec client: the ONE shared [`ExecutionClientActor`] over this venue's REST thread, with its hooks
/// ([`with_submitted_symbols`]); that thread starts the SSE trade-events pump and joins it when its loop ends.
/// `submit`/`cancel` enqueue onto the REST thread (non-blocking); every venue event returns through the core ingest.
/// Dropping it stops both threads (command + SSE). The start hands it the HALT sentinel with
/// [`ExecutionClientActor::with_halt_path`] (`crates/vike-ops/tests/settings_secrets/bridge_inputs_gate.rs` holds that
/// every live start does).
pub fn spawn(config: AlpacaConfig, events: EventSender) -> ExecutionClientActor {
    let token = Arc::new(TokenSource::new(
        config.client_id.expose().clone(),
        config.client_secret.expose().clone(),
        config.hosts.authx.to_string(),
    ));
    let symbols = submitted_symbols();
    let actor = ExecutionClientActor::spawn("alpaca-exec", events.clone(), {
        let symbols = symbols.clone();
        move |rx| run(config, token, events, rx, symbols)
    });
    with_submitted_symbols(actor, symbols)
}

/// `actor` with this venue's two hooks over `symbols`, the table the exec thread and the SSE stream stamp
/// every fill with (see [`crate::execution::submitted_symbols`]):
/// - [`ExecutionClientActor::with_before_submit`]: the submitted symbol is recorded BEFORE the order is queued, so no
///   fill of it can be built first;
/// - [`ExecutionClientActor::with_adopt_restored`]: an order a previous session left resting was restored under its
///   symbol, so its later fill must carry that symbol. A pure table write that returns at once (the
///   fold-thread contract).
///
/// `pub` and `#[doc(hidden)]` only so an integration test can mount a fake command loop under the REAL
/// hooks; [`spawn`] is the one production caller.
#[doc(hidden)]
pub fn with_submitted_symbols(
    actor: ExecutionClientActor,
    symbols: Arc<OrderClientIdSymbols>,
) -> ExecutionClientActor {
    let restored = symbols.clone();
    actor
        .with_before_submit(Arc::new(move |request: &mut OrderRequest| {
            symbols.record(&request.order_client_id, &request.symbol);
        }))
        .with_adopt_restored(Arc::new(move |orders: &[vike_execution::RestoredOrder]| {
            restored.adopt(orders);
        }))
}

/// The account's signed position in `symbol` (the venue spells it `wire`) now (`/positions` omits a flat one, which
/// reads 0).
fn held_position(
    rest: &AlpacaTransport,
    base: &str,
    account: &str,
    wire: &str,
    symbol: &str,
) -> Result<f64, String> {
    let path = format!("/v1/trading/accounts/{account}/positions");
    let body = rest.get(base, &path, "").map_err(|e| e.to_string())?.to_string();
    let rows = crate::reconcile::parse_positions(&body, wire, symbol)?;
    Ok(rows.iter().map(|row| row.qty).sum())
}

/// The REST thread: the shared [`run_loop`] over [`VenueRestAlpaca`], with the SSE trade-events pump (the fills the
/// order POST can't carry inline) beside it, stopped and joined when the loop ends. Alpaca replays nothing over REST
/// (its SSE stream resumes from a `since_id` cursor inside the open, `crate::execution::user_data`), so the
/// gap-sentinel's replay is empty.
fn run(
    config: AlpacaConfig,
    token: Arc<TokenSource>,
    events: EventSender,
    rx: &Receiver<ExecutionClientActorCommand>,
    symbols: Arc<OrderClientIdSymbols>,
) {
    let stream = crate::execution::user_data::spawn_trade_events(
        &config,
        Arc::clone(&token),
        events.clone(),
        Arc::clone(&symbols),
    );
    let rest = VenueRestAlpaca {
        rest: AlpacaTransport::new(token),
        base: config.hosts.broker.to_string(),
        account: config.account_id.clone(),
        events: events.clone(),
        symbols,
        late: RefCell::default(),
    };
    run_loop(&rest, &events, rx, Vec::<Event>::new, DEFAULT_FILL_SENTINEL);
    // The loop ended (the actor's stop): stop and join the SSE pump (deterministic teardown).
    let _ = stream.shutdown();
}

/// This venue's [`VenueRest`] under [`run_loop`]: the broker account's orders, the late-close read and the
/// submitted-symbol table ([`crate::execution::submitted_symbols`]).
struct VenueRestAlpaca {
    rest: AlpacaTransport,
    base: String,
    account: String,
    /// The lane `OrderSubmitted` goes out on BEFORE the POST, as this venue has always sent it.
    events: EventSender,
    symbols: Arc<OrderClientIdSymbols>,
    /// Closes that waited are sized against the position read then: this venue ignores the flag ([`ExecutionClientActorLateCloses`]).
    late: RefCell<ExecutionClientActorLateCloses>,
}

impl VenueRest for VenueRestAlpaca {
    /// `OrderSubmitted` first, on the lane, then the POST's answer as events. The order goes out under its USER name's
    /// kept-row spelling (`crate::names::wire_of`); a name with no row is rejected in words naming the fix, with
    /// nothing sent. A close that waited asks the position
    /// first ([`ExecutionClientActorLateCloses`], with the wait [`serving_since`] tells). A POST that may have reached Alpaca says nothing
    /// more: the order may be live, so it stays Submitted (its symbol kept) for the SSE lane's `new` or `fill` and,
    /// behind that, the stuck-order watchdog and reconcile; a terminal reject there could stand over a live order.
    fn submit_order(&self, req: &OrderRequest) -> Vec<Event> {
        let _ = self.events.blocking_send(Event::OrderSubmitted(OrderSubmitted {
            order_client_id: req.order_client_id.clone(),
            timestamp: req.timestamp,
        }));
        let wire = match crate::names::wire_of(&req.symbol) {
            Ok((wire, _)) => wire,
            Err(why) => return vec![self.rejected(req, format!("{why}; nothing was sent"))],
        };
        let mut body = build_order_body(req, &wire);
        let late = serving_since()
            .is_some_and(|queued_at| self.late.borrow_mut().must_read(req, queued_at));
        if late {
            match late_close_size(
                req,
                held_position(&self.rest, &self.base, &self.account, &wire, &req.symbol),
            ) {
                Ok(Some(qty)) => {
                    body = build_order_body(&OrderRequest { qty, ..req.clone() }, &wire);
                }
                Ok(None) => {}
                Err(why) => return vec![self.rejected(req, why)],
            }
        }
        // The token first, then the order's age asked again: a due mint (or a wait on the stream's mint of the same
        // cell) is spent before the last age check, never inside the chain behind the POST, which is the POST and one
        // re-mint after a 401 (`vike_venues::venue_timings::ALPACA`'s `submit`, pinned below).
        if let Err(e) = self.rest.mint_if_due() {
            return vec![self.rejected(req, e.to_string())];
        }
        // `OrderSubmitted` went out above, so the refusal is the reject alone.
        if let Some(why) = too_old_to_post(req) {
            return vec![self.rejected(req, why)];
        }
        let path = format!("/v1/trading/accounts/{}/orders", self.account);
        let answer = self.rest.post_json(&self.base, &path, &body);
        let events = map_submit_answer(&req.order_client_id, req.timestamp, &answer);
        if events.iter().any(|event| matches!(event, Event::OrderRejected(_))) {
            self.symbols.forget(&req.order_client_id);
        }
        events
    }

    /// Alpaca cancels by venue order id, not client id: a fresh `orders:by_client_order_id` lookup resolves it, then
    /// the DELETE. Unknown/gone is an error, the NON-terminal reject (audit A2: never silently swallow a failed cancel).
    /// The DELETE's `204` is [`OrderCancelAck::Requested`], never `Canceled`: the Broker API reference's cancel says
    /// "Upon acceptance of the cancel request, it returns status 204" (a no-longer-cancelable order is a `422`), and the
    /// order can sit in `pending_cancel` until it is cancelled or fills, so the trade-event stream's `canceled` or
    /// `fill` ends it, or its `order_cancel_rejected` refuses the cancel.
    ///
    /// Every failure is typed by what it says of the cancel (decision 0303, AL-3): a lookup that failed sent nothing
    /// (NOT SENT, code `0`, in its own words); the lookup's `404` is Alpaca holding no such order (its refusal, the
    /// status as the code); a DELETE failure is `cancel_failure`'s.
    fn cancel_order(&self, order_client_id: &str) -> Result<OrderCancelAck, VenueApiError> {
        let lookup = format!("/v1/trading/accounts/{}/orders:by_client_order_id", self.account);
        let found =
            self.rest.get(&self.base, &lookup, &format!("client_order_id={order_client_id}"));
        let vid = match found {
            Ok(v) => v.get("id").and_then(|i| i.as_str()).map(str::to_string),
            Err(e) if e.status == 404 => {
                return Err(VenueApiError {
                    code: 404,
                    msg: format!("unknown order {order_client_id}: {e}"),
                });
            }
            Err(e) => {
                return Err(VenueApiError {
                    code: 0,
                    msg: format!("cancel not sent: the order lookup failed: {e}"),
                });
            }
        };
        let Some(vid) = vid else {
            return Err(VenueApiError {
                code: 0,
                msg: format!(
                    "cancel not sent: the order lookup named no order for {order_client_id}"
                ),
            });
        };
        let path = format!("/v1/trading/accounts/{}/orders/{vid}", self.account);
        match self.rest.delete(&self.base, &path) {
            Ok(_) => Ok(OrderCancelAck::Requested),
            Err(e) => Err(cancel_failure(&e)),
        }
    }

    /// Too old to send: refused before anything is read or sent, and its submitted symbol leaves with it.
    fn submit_refused(&self, req: &OrderRequest, why: String) -> Vec<Event> {
        self.symbols.forget(&req.order_client_id);
        refused(req, why)
    }

    // Broker orders have no in-place amend: `modify_order` keeps the trait's no-op (cancel/replace is deferred).
}

impl VenueRestAlpaca {
    /// The terminal reject of an order this thread already said `OrderSubmitted` for and refused BEFORE the POST (a
    /// name with no row, a late close's read, the token, its age), so `order.not_sent`; its symbol leaves with it. The
    /// POST's own answer is [`map_submit_answer`]'s.
    fn rejected(&self, req: &OrderRequest, why: String) -> Event {
        self.symbols.forget(&req.order_client_id);
        Event::OrderRejected(OrderRejected {
            cause: None,
            order_client_id: req.order_client_id.clone(),
            reason: why.into(),
            timestamp: req.timestamp,
            refusal_id: vike_model::events::refusal_id_of(
                vike_model::OrderRefusalId::NotSent.word(),
            ),
        })
    }
}

/// A cancel DELETE's failure as the cancel's error: one that may have reached Alpaca (lost after the write, a 5xx, a
/// gateway status: `VenueApiHttpError::outcome_unknown`) is OUTCOME UNKNOWN, carried by the sentinel (the shared
/// `vike_bridge_core::rest::cancel_rejected` words it); one never sent keeps the never-sent `0`; Alpaca's own refusal
/// (a `404` "order is not found", a `422` "not cancelable") its status as the code.
fn cancel_failure(e: &VenueApiHttpError) -> VenueApiError {
    if e.outcome_unknown() {
        return VenueApiError { code: E_TIMEOUT_AMBIGUOUS, msg: e.to_string() };
    }
    VenueApiError { code: i64::from(e.status), msg: e.to_string() }
}

#[path = "execution_tests.rs"]
#[cfg(test)]
mod execution_tests;
