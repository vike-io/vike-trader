//! The venue-neutral REST-exec abstraction: the `VenueRest` trait, the generic `ExecutionClientRest<R>`
//! `ExecutionClient` wrapper, and the shared ambiguous-submit resolution, used by every REST venue
//! bridge (binance/bybit/okx/deribit/aster). REST-exec MACHINERY only: `SymbolProperties` is a
//! vike-model noun (`vike_model::SymbolProperties`).

use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderModifyRejected, OrderRejected};

use crate::transport::VenueApiError;

/// Map an ambiguous (timed-out) submit's order-status re-query to a NON-phantom event — the SHARED
/// post-submit ack-confirm every REST venue reaches for after an `E_TIMEOUT_AMBIGUOUS` submit. The
/// venue-specific part is only the re-query endpoint that produces `query`; the mapping is
/// venue-neutral:
///
/// * `Ok(Some(id))` — the venue HAS the order (it is live, or already filled) → a managed
///   [`Event::OrderAccepted`] carrying the venue order id; the fill, if any, follows on the normal
///   user-data lane (`OrderAccepted → OrderFilled` is a legal transition).
/// * `Ok(None)` — the venue does not list the order NOW → NOTHING. The read comes milliseconds after a
///   503 or a `-1007`, `10000`, `50004`, with no dwell and no dispatch ledger, and a POST that lands
///   after it would have its `NEW` frame dropped as stranded behind a terminal we made up. The core's
///   ladder decides: its confirm asks again with a dispatched order client id ([`crate::confirm`], which alone may
///   say [`venue_confirms_absent`] on a read), or its backstop ends the order by 75 s. The cost: a true
///   absent is rejected up to 75 s later instead of at once.
/// * `Err(_)` — the re-query itself was inconclusive (transport error / double-timeout) → NOTHING.
///   The order stays `Submitted`, where the core's stuck-order ladder
///   (`crates/vike-core/src/runtime/watchdog/orders.rs`'s `sweep_stuck_orders`), the user-data stream
///   and reconcile decide. An id-less `OrderAccepted` used to answer here: when the POST and the re-query
///   fail in one outage the order often never landed, and an order past pre-ack is one the ladder never
///   sweeps, so it rested ACCEPTED for good (reconcile folds nothing on a live mount).
///
/// This is the SUBMIT-TIMEOUT path's answer (a POST was sent and may be live), and the core's
/// last-resort stuck-order watchdog waits a confirm-grace for its event to fold before it would ever
/// synthesize a backstop reject. The ACTIVE confirm (no POST behind it) is NOT this mapping: a failed
/// query says nothing there and "absent" rejects only a provably dispatched order client id
/// ([`crate::confirm`]). Pure — the mapping is unit-tested with no transport.
pub fn resolve_ambiguous_submit(
    order_client_id: &str,
    ts: i64,
    query: Result<Option<String>, VenueApiError>,
) -> Option<Event> {
    match query {
        Ok(Some(id)) => Some(Event::OrderAccepted(OrderAccepted {
            order_client_id: order_client_id.to_string(),
            venue_order_id: Some(id.into()),
            timestamp: ts,
        })),
        Ok(None) => {
            tracing::warn!(
                target: "vike_bridge_core::rest",
                order_client_id = %order_client_id,
                "ambiguous submit: the venue does not list the order yet; saying nothing (the watchdog's confirm and backstop decide)"
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                target: "vike_bridge_core::rest",
                order_client_id = %order_client_id,
                code = e.code,
                msg = %e.msg,
                "ambiguous submit: status query failed; saying nothing (the watchdog's confirm, the stream and reconcile decide)"
            );
            None
        }
    }
}

/// The terminal reject of an order the venue is PROVEN not to hold: an absent answer with evidence behind
/// it (a dispatched order client id inside the venue's lookup window, [`crate::confirm`]; oanda's transaction scan).
/// A bare "absent" on the submit-timeout path is not that ([`resolve_ambiguous_submit`]).
pub fn venue_confirms_absent(order_client_id: &str, ts: i64) -> Event {
    tracing::warn!(
        target: "vike_bridge_core::rest",
        order_client_id = %order_client_id,
        "the venue confirms the order absent: rejected"
    );
    Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: order_client_id.to_string(),
        reason: "submit timed out; venue confirms order absent".to_string().into(),
        timestamp: ts,
    })
}

/// `[OrderSubmitted, OrderRejected(why)]`: the emitter split of an order refused before the wire
/// (`OrderSubmitted` synchronously, then a TERMINAL `OrderRejected` carrying the reason). *"No order
/// may silently vanish"* is the venue-adapter contract, and a refusal is the easiest place to break it.
pub fn refused(request: &OrderRequest, why: String) -> Vec<Event> {
    vec![
        Event::OrderSubmitted(vike_model::events::OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        }),
        Event::OrderRejected(vike_model::events::OrderRejected {
            cause: None,
            order_client_id: request.order_client_id.clone(),
            reason: why.into(),
            timestamp: request.timestamp,
        }),
    ]
}

/// **The exec thread of an account whose mounted instrument the bridge refuses to trade** (bybit's
/// `non_linear_perpetual_refusal`, okx's `inverse_swap_refusal`) — a [`VenueRest`] that never touches
/// the wire and never lets an order vanish: every submit is [`refused`] with `why`.
///
/// It is the whole refusal mechanism, and it is a `VenueRest` rather than a bespoke loop on purpose:
/// [`crate::exec_actor::run_loop`] already owns the command protocol (the emitter split, the
/// terminal-event contract, the shutdown), so the refusing path and the trading path cannot drift
/// about what a submitted order is owed. The caller builds no REST client, posts no leverage and opens
/// no private WS for it.
///
/// **A cancel is answered by who placed the order** (the owner's B-15, its safe variant). An order this
/// exec thread refused itself never reached the venue, so its cancel is `Ok` (`OrderCanceled`, never a
/// second failure). Any other order was placed by an earlier process (the core restored it) and the
/// venue may still hold it: its cancel is an `Err`, the NON-terminal `OrderCancelRejected`, because a
/// terminal `OrderCanceled` would drop the fill that order may still get, and nothing was sent.
///
/// **Every amend is refused in words** (owner, 2026-11-11): a price/qty amend and a trigger amend both get
/// the non-terminal `OrderModifyRejected(why)`, so no command is left unanswered and no order changes status.
pub struct VenueRestRefusing {
    /// The refusal text, repeated verbatim on every rejected order so an operator reading one event
    /// sees the same sentence the startup log carried.
    pub why: String,
}

/// How many of the orders it refused one exec thread remembers ([`VenueRestRefusing`]): past it the oldest
/// goes, and its cancel answers as an earlier process's would, non-terminally, which is safe for an order
/// that already ended rejected.
const MAX_REFUSED_REMEMBERED: usize = 4096;

thread_local! {
    /// The orders this exec thread's [`VenueRestRefusing`] refused, oldest first.
    /// shortcut: per THREAD, not per client: right because a refusing client's every call runs on its one
    /// exec thread (as `crate::exec_actor`'s `SERVING_SINCE` is), and it keeps `VenueRestRefusing { why }`
    /// buildable as a literal; a field is the upgrade if a client is ever driven from two threads.
    static REFUSED_HERE: std::cell::RefCell<std::collections::VecDeque<String>> =
        const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

impl VenueRest for VenueRestRefusing {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        REFUSED_HERE.with_borrow_mut(|seen| {
            if seen.len() >= MAX_REFUSED_REMEMBERED {
                seen.pop_front();
            }
            seen.push_back(request.order_client_id.clone());
        });
        refused(request, self.why.clone())
    }

    /// `Ok` for an order this thread refused ("unknown ≠ rejection": nothing of it ever rested); an `Err`
    /// for any other, which an earlier process placed and the venue may still hold. See the type's doc.
    fn cancel_order(&self, order_client_id: &str) -> Result<OrderCancelAck, VenueApiError> {
        if REFUSED_HERE.with_borrow(|seen| seen.iter().any(|c| c == order_client_id)) {
            return Ok(OrderCancelAck::Canceled);
        }
        Err(VenueApiError {
            code: 0,
            msg: format!(
                "{}; this cancel was NOT sent, and an earlier process placed this order, so the venue \
                 may still hold it",
                self.why
            ),
        })
    }

    /// The non-terminal `OrderModifyRejected` carrying the refusal (owner, 2026-11-11, variant A). Nothing is
    /// sent and the order keeps its terms; the strategy is told so instead of being left waiting for an answer.
    /// The `VenueRest` default answers a price/qty amend with nothing, a working venue's "no native amend".
    fn modify_order(
        &self,
        order: &OrderRequest,
        _new_quantity: Option<f64>,
        _new_price: Option<f64>,
    ) -> Vec<Event> {
        vec![self.modify_refused(order)]
    }

    /// The same answer for a trigger amend: the refusal's words, not "this venue cannot amend a trigger".
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        _new_quantity: Option<f64>,
        _new_price: Option<f64>,
        _new_trigger_price: f64,
    ) -> Vec<Event> {
        vec![self.modify_refused(order)]
    }
}

impl VenueRestRefusing {
    fn modify_refused(&self, order: &OrderRequest) -> Event {
        Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
            order_client_id: order.order_client_id.clone(),
            reason: self.why.clone().into(),
            timestamp: amend_answer_time(None),
        })
    }
}

/// What a venue answered to a cancel that reached it, for [`VenueRest::cancel_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderCancelAck {
    /// The venue cancelled the order.
    Canceled,
    /// The venue no longer holds it (`-2011`, `110001`, `51402`, ...): it may have just FILLED, with the
    /// fill still on its way, or been cancelled before. Idempotent, so an `Ok`, but no statement about
    /// how the order ended: a terminal `OrderCanceled` here reads a filled order CANCELED for good, since
    /// the order table has no `Canceled -> Filled` edge.
    /// [`crate::exec_actor::run_loop`] answers it non-terminally and its gap-sentinel settles it.
    AlreadyGone,
}

impl OrderCancelAck {
    /// A cancel request's venue `answer` as the ack: any `Ok` is [`OrderCancelAck::Canceled`]; an error
    /// `says_gone` names (the venue's own codes or words, a venue fact) is [`OrderCancelAck::AlreadyGone`],
    /// logged at debug; any other error stays the error (a non-terminal `OrderCancelRejected`).
    pub fn from_venue_answer<T>(
        venue: &str,
        order_client_id: &str,
        answer: Result<T, VenueApiError>,
        says_gone: impl FnOnce(&VenueApiError) -> bool,
    ) -> Result<OrderCancelAck, VenueApiError> {
        match answer {
            Ok(_) => Ok(OrderCancelAck::Canceled),
            Err(exc) if says_gone(&exc) => {
                tracing::debug!(venue, order_client_id, "cancel: order already gone");
                Ok(OrderCancelAck::AlreadyGone)
            }
            Err(exc) => Err(exc),
        }
    }
}

/// **The time an amend's answer is stamped with** (`OrderModified`, `OrderModifyRejected`, a modify
/// timeout included; the owner's ruling of 2026-11-12): the VENUE's time from the reply when the reply
/// states one (`stated`, read from the venue's own field by its bridge: which field is a venue fact),
/// else the moment the reply was received, read here. Never `0`, and never the order's own creation
/// time (`OrderRequest::ts`). A refusal made before anything was sent has no reply: it is stamped when
/// it is made. No consumer decides on this time
/// (`crates/vike-execution/tests/engine/pending_modify_price.rs`'s `the_fold_reads_no_amend_answers_time`).
pub fn amend_answer_time(stated: Option<i64>) -> i64 {
    stated.filter(|ts| *ts > 0).unwrap_or_else(vike_model::now_ms)
}

/// The NON-terminal advisory for a modify the venue refused or the transport could not deliver
/// (`OrderModifyRejected` maps the order back to modifiable). A refusal says why; an AMBIGUOUS
/// failure ([`E_TIMEOUT_AMBIGUOUS`](crate::transport::E_TIMEOUT_AMBIGUOUS): the PUT may have been
/// applied) says "outcome unknown" instead, because the order may NOT have kept its terms. `ts` is
/// [`amend_answer_time`]'s: a [`VenueApiError`] carries no venue time, so it is the receipt time.
pub fn modify_rejected(order_client_id: &str, err: &VenueApiError, ts: i64) -> Event {
    let reason = if err.code == crate::transport::E_TIMEOUT_AMBIGUOUS {
        format!(
            "modify outcome unknown (timed out after send; the venue may have applied it): {}",
            err.msg
        )
    } else {
        err.msg.clone()
    };
    Event::OrderModifyRejected(OrderModifyRejected {
        order_client_id: order_client_id.to_string(),
        reason: reason.into(),
        timestamp: ts,
    })
}

/// [`modify_rejected`] for a venue whose own code table also files venue codes as status-unknown:
/// a code `outcome_unknown` answers `true` for (the venue's `error_codes::submit_outcome_unknown`)
/// is worded as the timeout sentinel is (`modify outcome unknown`: the modify may have been applied)
/// instead of as a refusal; a definite one keeps the venue's words. The one body behind binance's
/// (and aster's perp), bybit's and okx's `error_codes::modify_failed`, stamped `ts` as there.
pub fn modify_failed_with(
    order_client_id: &str,
    err: &VenueApiError,
    outcome_unknown: fn(i64) -> bool,
    ts: i64,
) -> Event {
    let err = if outcome_unknown(err.code) {
        VenueApiError { code: crate::transport::E_TIMEOUT_AMBIGUOUS, msg: err.msg.clone() }
    } else {
        err.clone()
    };
    modify_rejected(order_client_id, &err, ts)
}

/// A venue's blocking REST order interface — one `ExecutionClientRest` wrapper turns any impl into a
/// `vike_execution::ExecutionClient`, and [`crate::exec_actor::run_loop`] drives it on an exec thread.
pub trait VenueRest: Send {
    /// Shared submit flow for the core ingest: `[Submitted, Accepted|Rejected]`, or `[Submitted]` alone when the POST got no
    /// answer and its re-query found nothing ([`resolve_ambiguous_submit`]): the stream, the active confirm and the
    /// core's backstop speak from there.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event>;
    /// Idempotent cancel ("unknown ≠ rejection" — an already-gone order is `Ok`, and says so:
    /// [`OrderCancelAck::AlreadyGone`]).
    fn cancel_order(&self, order_client_id: &str) -> Result<OrderCancelAck, VenueApiError>;

    /// What an order [`crate::exec_actor::run_loop`] refused BEFORE calling [`Self::submit_order`] says (it waited
    /// past `crate::exec_actor::too_old_to_send`): nothing was read, signed or sent. DEFAULT [`refused`]. A client
    /// that keeps something per order from the moment it was queued (alpaca's submitted symbol) drops it here, since
    /// no answer of the venue will ever end it. ⚠ A wrapper forwards it by hand, or an inner override is shadowed.
    fn submit_refused(&self, request: &OrderRequest, why: String) -> Vec<Event> {
        refused(request, why)
    }

    /// ACTIVELY re-confirm one order's status: the venue-side answer to the
    /// core's `Command::ConfirmOrder`, run on the adapter's OWN thread (via
    /// [`crate::exec_actor::ExecutionClientActor::with_confirm`]) so a truly-wedged adapter is prodded to
    /// re-query instead of only being waited on. Returns the authoritative event(s) to emit, in the
    /// ONE form of [`crate::confirm`] (a venue that looks an order up by order client id implements
    /// [`crate::confirm::ConfirmByOrderClientId`] and wires [`crate::confirm::confirm_closure`]): found → the
    /// order's state as events, absent → the terminal `OrderRejected` only for an order client id provably
    /// dispatched inside the venue's lookup window, a failed query → nothing. DEFAULT `vec![]` — a
    /// venue with no status re-query reports nothing (the watchdog's stage-2 reject still backstops
    /// it). NEVER runs on the core fold.
    fn confirm_order(&self, _order_client_id: &str, _ts: i64) -> Vec<Event> {
        Vec::new()
    }

    /// Modify a resting order in place. Takes the whole resting `order` so a venue can read
    /// `side`/current qty/price (Binance-futures modify needs them). DEFAULT: no-op — the venue can't modify natively, so the order keeps its terms (nothing
    /// vanishes). Override to hit a native modify endpoint; return `[OrderModified]` on success and
    /// `[OrderModifyRejected]` (non-terminal: the order keeps its terms) when the venue or transport
    /// refused or could not deliver it. Wired to `ExecutionClient::modify` by `ExecutionClientRest`.
    fn modify_order(
        &self,
        _order: &OrderRequest,
        _new_quantity: Option<f64>,
        _new_price: Option<f64>,
    ) -> Vec<Event> {
        Vec::new()
    }

    /// Move a resting stop-limit's or take-profit's TRIGGER in place, with its qty and limit as
    /// [`Self::modify_order`] takes them (`vike_execution::ExecutionClient::modify_trigger` is the contract):
    /// return `[OrderModified]` carrying `new_trigger_price` on success, a non-terminal
    /// `[OrderModifyRejected]` otherwise. `run_loop` calls it for an actor that DECLARED it
    /// (`ExecutionClientActor::with_trigger_amend`). DEFAULT: refuse in words and send nothing, never the limit alone.
    /// ⚠ A wrapper forwards it by hand, or the inner override is shadowed by the refusal.
    fn modify_trigger(
        &self,
        order: &OrderRequest,
        _new_quantity: Option<f64>,
        _new_price: Option<f64>,
        _new_trigger_price: f64,
    ) -> Vec<Event> {
        vec![Event::OrderModifyRejected(vike_model::events::OrderModifyRejected {
            order_client_id: order.order_client_id.clone(),
            reason: format!(
                "{}: {}",
                order.venue,
                vike_execution::execution_engine::TRIGGER_AMEND_UNSUPPORTED_REASON
            )
            .into(),
            timestamp: amend_answer_time(None),
        })]
    }

    /// The accept a replayed fill wrap `event` implies, when this client knows its order got no answer and
    /// [`crate::exec_actor::run_loop`] cannot: a client that sends its order's events itself returns it none to read.
    /// DEFAULT `None`, and `run_loop` reads every other client's own answer.
    /// [`crate::exec_actor::submit_fence::VenueRestOrderSubmitFence`] is the one override.
    fn accept_implied_by_replay(&self, _event: &Event) -> Option<Event> {
        None
    }

    /// Batch-submit. DEFAULT: fan out to `submit_order` (works on every venue immediately); each
    /// order still yields its own [Submitted, Accepted|Rejected] stream. Override for a native
    /// batch endpoint.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        requests.iter().flat_map(|r| self.submit_order(r)).collect()
    }

    /// Batch-cancel. DEFAULT: fan out to `cancel_order`; every id is attempted and the FIRST error
    /// is returned. Override for a native batch/mass-cancel endpoint.
    fn cancel_batch(&self, order_client_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for c in order_client_ids {
            if let Err(e) = self.cancel_order(c) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

/// A `vike_execution::ExecutionClient` over a venue REST client: `submit` runs the REST flow and
/// queues its [Submitted, Accepted|Rejected] into `pending` (the core pumps them through the bus
/// via `poll_events`); the authoritative OrderCanceled/fills arrive on the user-data WS through
/// the ingest channel.
///
/// ⚠ The REST call runs ON the caller's thread: started on the core, it blocks the fold for a
/// round trip.
pub struct ExecutionClientRest<R: VenueRest> {
    pub rest: R,
    pending: std::collections::VecDeque<Event>,
    /// Last NON-idempotent cancel-path venue error. Each venue's own `cancel_order` swallows its
    /// idempotent "already gone" codes (e.g. Binance's −2011) before returning here, so only
    /// genuine cancel failures land in this field.
    pub last_cancel_error: Option<VenueApiError>,
}

impl<R: VenueRest> ExecutionClientRest<R> {
    pub fn new(rest: R) -> Self {
        ExecutionClientRest {
            rest,
            pending: std::collections::VecDeque::new(),
            last_cancel_error: None,
        }
    }
}

impl<R: VenueRest> vike_execution::ExecutionClient for ExecutionClientRest<R> {
    fn submit(&mut self, request: &OrderRequest) {
        self.pending.extend(self.rest.submit_order(request));
    }

    fn cancel(&mut self, order_client_id: &str) {
        if let Err(exc) = self.rest.cancel_order(order_client_id) {
            // Surface the failure as a NON-terminal advisory (the order stays live) instead of
            // burying it in `last_cancel_error` where nothing reads it. The authoritative
            // OrderCanceled still arrives over the user-data WS if the cancel later takes effect.
            tracing::warn!(
                target: "vike_bridge_core::rest",
                order_client_id,
                reason = %exc.msg,
                "cancel rejected"
            );
            self.pending.push_back(Event::OrderCancelRejected(
                vike_model::events::OrderCancelRejected {
                    order_client_id: order_client_id.to_string(),
                    reason: exc.msg.clone().into(),
                    timestamp: 0,
                },
            ));
            self.last_cancel_error = Some(exc);
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.pending.extend(self.rest.modify_order(order, new_qty, new_price));
    }

    fn modify_trigger(
        &mut self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: f64,
    ) -> Result<(), &'static str> {
        self.pending.extend(self.rest.modify_trigger(order, new_qty, new_price, new_trigger_price));
        Ok(())
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        self.pending.extend(self.rest.submit_batch(requests));
    }

    fn cancel_batch(&mut self, order_client_ids: &[String]) {
        if let Err(exc) = self.rest.cancel_batch(order_client_ids) {
            // The batch API returns one error for the whole batch, so we can't attribute it to a
            // single id. Emit a NON-terminal advisory per id: over-reporting is safe because a
            // spurious reject never changes status, so a genuine OrderCanceled still applies.
            tracing::warn!(
                target: "vike_bridge_core::rest",
                count = order_client_ids.len(),
                reason = %exc.msg,
                "cancel batch rejected"
            );
            for order_client_id in order_client_ids {
                self.pending.push_back(Event::OrderCancelRejected(
                    vike_model::events::OrderCancelRejected {
                        order_client_id: order_client_id.clone(),
                        reason: exc.msg.clone().into(),
                        timestamp: 0,
                    },
                ));
            }
            self.last_cancel_error = Some(exc);
        }
    }

    fn poll_events(&mut self) -> Option<Event> {
        self.pending.pop_front()
    }
}

#[cfg(test)]
mod ambiguous_submit_tests {
    use super::*;
    use std::assert_matches;

    /// The mapping every REST venue depends on: live → managed Accepted (with the venue id); absent or
    /// inconclusive → NOTHING, so the order stays `Submitted` where the stuck-order ladder still sweeps
    /// it: never an id-less Accepted it does not, never a terminal on one read just after the POST.
    #[test]
    fn maps_live_absent_and_inconclusive() {
        // live at venue (order id came back) → managed OrderAccepted carrying that id
        match resolve_ambiguous_submit("c1", 5, Ok(Some("od-1".to_string()))) {
            Some(Event::OrderAccepted(a)) => {
                assert_eq!(a.order_client_id, "c1");
                assert_eq!(a.venue_order_id.as_deref(), Some("od-1"));
                assert_eq!(a.timestamp, 5);
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
        // not listed yet → nothing: the POST may still land, the ladder's confirm asks again
        assert_eq!(resolve_ambiguous_submit("c1", 5, Ok(None)), None);
        // the evidence-backed verdict the confirm reaches for is its own function
        match venue_confirms_absent("c1", 5) {
            Event::OrderRejected(r) => {
                assert_eq!(r.order_client_id, "c1");
                assert!(r.reason.contains("venue confirms order absent"), "reason: {}", r.reason);
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
        // inconclusive re-query (transport error) → nothing, the ladder decides
        let failed = Err(VenueApiError { code: 0, msg: "x".into() });
        assert_eq!(resolve_ambiguous_submit("c1", 5, failed), None);
    }

    /// The refusing exec thread bybit (`non_linear_perpetual_refusal`) and okx (`inverse_swap_refusal`)
    /// each carried a copy of: every submit is `[OrderSubmitted, OrderRejected(why)]` with the order's
    /// own order client id and ts and the reason verbatim, no cause; a cancel of what never rested is `Ok`.
    #[test]
    fn the_refusing_client_submits_then_rejects_every_order_with_the_reason() {
        let rest = VenueRestRefusing { why: "okx exec REFUSES \"BTC-USD-SWAP\"".to_string() };
        let order =
            OrderRequest { order_client_id: "c9".into(), timestamp: 77, ..Default::default() };
        match rest.submit_order(&order).as_slice() {
            [Event::OrderSubmitted(s), Event::OrderRejected(r)] => {
                assert_eq!((s.order_client_id.as_str(), s.timestamp), ("c9", 77));
                assert_eq!((r.order_client_id.as_str(), r.timestamp), ("c9", 77));
                assert_eq!(r.reason.as_str(), "okx exec REFUSES \"BTC-USD-SWAP\"");
                assert!(r.cause.is_none());
            }
            other => panic!("expected [Submitted, Rejected], got {other:?}"),
        }
        assert!(rest.cancel_order("c9").is_ok());
        assert_eq!(rest.submit_batch(&[order.clone(), order]).len(), 4, "the batch fans out");
    }

    /// B-15's safe variant through the real `run_loop`: the cancel of an order this thread refused ends
    /// `OrderCanceled`; the cancel of an order an earlier process placed (restored: the venue may still hold
    /// it) is the NON-terminal `OrderCancelRejected` carrying the refusal, so a later fill of it still lands.
    #[test]
    fn a_refusing_client_cancels_only_what_it_refused_itself() {
        let rest = VenueRestRefusing { why: "refused here".to_string() };
        rest.submit_order(&crate::execution_drive::limit("okx", "own", "BTC-USD-SWAP", 1.0, 1.0));
        match crate::execution_drive::drive_cancel(&rest, "own").as_slice() {
            [Event::OrderCanceled(c)] => assert_eq!(c.order_client_id, "own"),
            other => panic!("its own refused order ends cancelled, got {other:?}"),
        }
        match crate::execution_drive::drive_cancel(&rest, "restored").as_slice() {
            [Event::OrderCancelRejected(r)] => {
                assert_eq!(r.order_client_id, "restored");
                assert!(
                    r.reason.starts_with("refused here; this cancel was NOT sent"),
                    "{}",
                    r.reason
                );
            }
            other => panic!("an earlier process's order is refused non-terminally, got {other:?}"),
        }
        // A second client on the same exec thread (a loop of its own builds one per command) knows it too.
        assert!(VenueRestRefusing { why: String::new() }.cancel_order("own").is_ok());
    }

    /// A cancel the venue took is `Canceled`; one it calls gone (by its own predicate) is `AlreadyGone`;
    /// any other answer stays the error, so it reaches the core as a non-terminal rejection.
    #[test]
    fn a_cancel_answer_is_canceled_gone_or_the_error() {
        let gone = |e: &VenueApiError| e.code == -2011;
        let err = |code| Err::<(), _>(VenueApiError { code, msg: "m".into() });
        assert_eq!(
            OrderCancelAck::from_venue_answer("v", "c1", Ok(()), gone),
            Ok(OrderCancelAck::Canceled)
        );
        assert_eq!(
            OrderCancelAck::from_venue_answer("v", "c1", err(-2011), gone),
            Ok(OrderCancelAck::AlreadyGone)
        );
        assert_eq!(
            OrderCancelAck::from_venue_answer("v", "c1", err(-1003), gone).map_err(|e| e.code),
            Err(-1003)
        );
    }

    /// A refused modify keeps its venue reason; an ambiguous one says the outcome is unknown.
    #[test]
    fn modify_rejected_names_the_doubt_only_for_the_ambiguous_failure() {
        let reason = |code| match modify_rejected("c1", &VenueApiError { code, msg: "m".into() }, 7)
        {
            Event::OrderModifyRejected(r) => r.reason.to_string(),
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        };
        assert_eq!(reason(-4028), "m");
        assert!(reason(crate::transport::E_TIMEOUT_AMBIGUOUS).contains("outcome unknown"));
    }

    /// The owner's 2026-11-12 ruling: an amend's answer carries the venue's time when the reply states one,
    /// else the moment it was received; never `0`, never the order's own creation time.
    #[test]
    fn an_amend_answer_carries_the_venue_time_else_the_receipt_time() {
        assert_eq!(amend_answer_time(Some(1_794_000_000_123)), 1_794_000_000_123);
        let before = vike_model::now_ms();
        for unstated in [None, Some(0), Some(-1)] {
            let ts = amend_answer_time(unstated);
            assert!(ts >= before && ts <= vike_model::now_ms(), "{unstated:?} -> {ts}");
        }
        let refused = |e: Event| match e {
            Event::OrderModifyRejected(r) => r.timestamp,
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        };
        let err = VenueApiError { code: -4028, msg: "m".into() };
        assert_eq!(refused(modify_rejected("c1", &err, 42)), 42);
        // An order created long ago: its refusals are stamped now, not at its creation and not at 0.
        let order =
            OrderRequest { order_client_id: "c1".into(), timestamp: 5, ..Default::default() };
        /// A venue that declares no trigger amend: the trait's default answers it.
        struct Plain;
        impl VenueRest for Plain {
            fn submit_order(&self, _: &OrderRequest) -> Vec<Event> {
                Vec::new()
            }
            fn cancel_order(&self, _: &str) -> Result<OrderCancelAck, VenueApiError> {
                Ok(OrderCancelAck::Canceled)
            }
        }
        let rest = VenueRestRefusing { why: "refused here".to_string() };
        for answer in [
            rest.modify_order(&order, None, Some(1.0)),
            rest.modify_trigger(&order, None, None, 1.0),
            Plain.modify_trigger(&order, None, None, 1.0),
        ] {
            let [answer] = <[Event; 1]>::try_from(answer).expect("one answer");
            assert!(refused(answer) >= before, "stamped when made");
        }
    }

    /// The venue's status-unknown code is worded as the sentinel is, keeping the venue's message;
    /// a definite code keeps the venue's words; the sentinel itself stays "outcome unknown" even when
    /// the venue's predicate does not name it.
    #[test]
    fn modify_failed_with_words_the_venues_unknown_codes_as_the_sentinel() {
        let reason = |code, unknown: fn(i64) -> bool| match modify_failed_with(
            "c1",
            &VenueApiError { code, msg: "m".into() },
            unknown,
            9,
        ) {
            Event::OrderModifyRejected(r) => {
                assert_eq!(r.order_client_id, "c1");
                assert_eq!(r.timestamp, 9, "the time it is given");
                r.reason.to_string()
            }
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        };
        let unknown =
            "modify outcome unknown (timed out after send; the venue may have applied it): m";
        assert_eq!(reason(-1007, |c| c == -1007), unknown);
        assert_eq!(reason(-4028, |c| c == -1007), "m");
        assert_eq!(reason(crate::transport::E_TIMEOUT_AMBIGUOUS, |_| false), unknown);
    }

    /// A filled order re-query returns the venue id (the venue HAS the order), so the resolver emits
    /// OrderAccepted — the order is managed, and its fill follows on the user-data lane. The one
    /// thing it must never do for a venue-held order is synthesize a reject.
    #[test]
    fn venue_held_order_is_never_rejected() {
        assert_matches!(
            resolve_ambiguous_submit("c9", 1, Ok(Some("v-9".to_string()))),
            Some(Event::OrderAccepted(_))
        );
    }
}
