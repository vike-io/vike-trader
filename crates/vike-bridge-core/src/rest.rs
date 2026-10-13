//! The venue-neutral REST-exec abstraction: the `VenueRest` trait, the generic `LiveRestClient<R>`
//! `ExecutionClient` wrapper, and the shared ambiguous-submit resolution, used by every REST venue
//! bridge (binance/bybit/okx/deribit/aster). REST-exec MACHINERY only: `SymbolProperties` is a
//! vike-model noun (`vike_model::SymbolProperties`).

use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected};

use crate::transport::VenueApiError;

/// Map an ambiguous (timed-out) submit's order-status re-query to a NON-phantom event — the SHARED
/// post-submit ack-confirm every REST venue reaches for after an `E_TIMEOUT_AMBIGUOUS` submit. The
/// venue-specific part is only the re-query endpoint that produces `query`; the mapping is
/// venue-neutral:
///
/// * `Ok(Some(id))` — the venue HAS the order (it is live, or already filled) → a managed
///   [`Event::OrderAccepted`] carrying the venue order id; the fill, if any, follows on the normal
///   user-data lane (`OrderAccepted → OrderFilled` is a legal transition).
/// * `Ok(None)` — the venue confirms the order never landed → the TRUE terminal
///   [`Event::OrderRejected`].
/// * `Err(_)` — the re-query itself was inconclusive (transport error / double-timeout) → an
///   OPTIMISTIC [`Event::OrderAccepted`] with no venue id, NEVER a false terminal (a phantom reject
///   here would strand a real position the venue actually opened).
///
/// This is the authoritative confirm the core's last-resort stuck-order watchdog defers to: the
/// watchdog waits a confirm-grace for exactly this event to fold before it would ever synthesize a
/// backstop reject. Pure — the mapping is unit-tested with no transport.
pub fn resolve_ambiguous_submit(
    coid: &str,
    ts: i64,
    query: Result<Option<String>, VenueApiError>,
) -> Event {
    match query {
        Ok(Some(id)) => Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: Some(id.into()),
            ts,
        }),
        Ok(None) => {
            tracing::warn!(
                target: "vike_bridge_core::rest",
                client_order_id = %coid,
                "ambiguous submit resolved to rejected"
            );
            Event::OrderRejected(OrderRejected {
                client_order_id: coid.to_string(),
                reason: "submit timed out; venue confirms order absent".to_string().into(),
                ts,
            })
        }
        // inconclusive re-query → optimistic managed order (never a false terminal)
        Err(_) => Event::OrderAccepted(OrderAccepted {
            client_order_id: coid.to_string(),
            venue_order_id: None,
            ts,
        }),
    }
}

/// A venue's blocking REST order interface — one `LiveRestClient` wrapper turns any impl into a
/// `vike_exec::ExecutionClient`, and [`crate::exec_actor::run_loop`] drives it on an exec thread.
pub trait VenueRest: Send {
    /// Shared submit flow: [Submitted, Accepted|Rejected] for the core ingest.
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event>;
    /// Idempotent cancel ("unknown ≠ rejection" — already-gone orders are Ok).
    fn cancel_order(&self, client_order_id: &str) -> Result<(), VenueApiError>;

    /// ACTIVELY re-confirm one order's status: the venue-side answer to the
    /// core's `Command::ConfirmOrder`, run on the adapter's OWN thread (via
    /// [`crate::exec_actor::ExecActor::with_confirm`]) so a truly-wedged adapter is prodded to
    /// re-query instead of only being waited on. Returns the authoritative event(s) to emit — reuse
    /// the SAME order-status re-query behind [`resolve_ambiguous_submit`]: `Ok(Some(id))` → managed
    /// `OrderAccepted` (order live/filled at the venue; the fill, if any, follows on user-data),
    /// `Ok(None)` → the true terminal `OrderRejected` (venue confirms absent), `Err(_)` → optimistic
    /// `OrderAccepted` (never a false terminal). DEFAULT `vec![]` — a venue with no status re-query
    /// reports nothing (the watchdog's stage-2 reject still backstops it). NEVER runs on the core fold.
    fn confirm_order(&self, _client_order_id: &str, _ts: i64) -> Vec<Event> {
        Vec::new()
    }

    /// Modify a resting order in place. Takes the whole resting `order` so a venue can read
    /// `side`/current qty/price (Binance-futures modify needs them). DEFAULT: no-op — the venue can't modify natively, so the order keeps its terms (nothing
    /// vanishes). Override to hit a native modify endpoint; return `[OrderModified]` on success, `[]`
    /// on failure. Wired to `ExecutionClient::modify` by `LiveRestClient`.
    fn modify_order(
        &self,
        _order: &OrderRequest,
        _new_qty: Option<f64>,
        _new_price: Option<f64>,
    ) -> Vec<Event> {
        Vec::new()
    }

    /// Batch-submit. DEFAULT: fan out to `submit_order` (works on every venue immediately); each
    /// order still yields its own [Submitted, Accepted|Rejected] stream. Override for a native
    /// batch endpoint.
    fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        requests.iter().flat_map(|r| self.submit_order(r)).collect()
    }

    /// Batch-cancel. DEFAULT: fan out to `cancel_order`; every id is attempted and the FIRST error
    /// is returned. Override for a native batch/mass-cancel endpoint.
    fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for c in client_order_ids {
            if let Err(e) = self.cancel_order(c) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

/// A `vike_exec::ExecutionClient` over a venue REST client: `submit` runs the REST flow and
/// queues its [Submitted, Accepted|Rejected] into `pending` (the core pumps them through the bus
/// via `poll_events`); the authoritative OrderCanceled/fills arrive on the user-data WS through
/// the ingest channel.
///
/// ⚠ The REST call runs ON the caller's thread: mounted on the core, it blocks the fold for a
/// round trip.
pub struct LiveRestClient<R: VenueRest> {
    pub rest: R,
    pending: std::collections::VecDeque<Event>,
    /// Last NON-idempotent cancel-path venue error. Each venue's own `cancel_order` swallows its
    /// idempotent "already gone" codes (e.g. Binance's −2011) before returning here, so only
    /// genuine cancel failures land in this field.
    pub last_cancel_error: Option<VenueApiError>,
}

impl<R: VenueRest> LiveRestClient<R> {
    pub fn new(rest: R) -> Self {
        LiveRestClient { rest, pending: std::collections::VecDeque::new(), last_cancel_error: None }
    }
}

impl<R: VenueRest> vike_exec::ExecutionClient for LiveRestClient<R> {
    fn submit(&mut self, request: &OrderRequest) {
        self.pending.extend(self.rest.submit_order(request));
    }

    fn cancel(&mut self, client_order_id: &str) {
        if let Err(exc) = self.rest.cancel_order(client_order_id) {
            // Surface the failure as a NON-terminal advisory (the order stays live) instead of
            // burying it in `last_cancel_error` where nothing reads it. The authoritative
            // OrderCanceled still arrives over the user-data WS if the cancel later takes effect.
            tracing::warn!(
                target: "vike_bridge_core::rest",
                client_order_id,
                reason = %exc.msg,
                "cancel rejected"
            );
            self.pending.push_back(Event::OrderCancelRejected(
                vike_model::events::OrderCancelRejected {
                    client_order_id: client_order_id.to_string(),
                    reason: exc.msg.clone().into(),
                    ts: 0,
                },
            ));
            self.last_cancel_error = Some(exc);
        }
    }

    fn modify(&mut self, order: &OrderRequest, new_qty: Option<f64>, new_price: Option<f64>) {
        self.pending.extend(self.rest.modify_order(order, new_qty, new_price));
    }

    fn submit_batch(&mut self, requests: &[OrderRequest]) {
        self.pending.extend(self.rest.submit_batch(requests));
    }

    fn cancel_batch(&mut self, client_order_ids: &[String]) {
        if let Err(exc) = self.rest.cancel_batch(client_order_ids) {
            // The batch API returns one error for the whole batch, so we can't attribute it to a
            // single id. Emit a NON-terminal advisory per id: over-reporting is safe because a
            // spurious reject never changes status, so a genuine OrderCanceled still applies.
            tracing::warn!(
                target: "vike_bridge_core::rest",
                count = client_order_ids.len(),
                reason = %exc.msg,
                "cancel batch rejected"
            );
            for coid in client_order_ids {
                self.pending.push_back(Event::OrderCancelRejected(
                    vike_model::events::OrderCancelRejected {
                        client_order_id: coid.clone(),
                        reason: exc.msg.clone().into(),
                        ts: 0,
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

    /// The three-way mapping every REST venue depends on: live → managed Accepted (with the venue
    /// id); venue-confirmed-absent → true terminal Rejected; inconclusive → optimistic Accepted (no
    /// id) — NEVER a false terminal.
    #[test]
    fn maps_live_absent_and_inconclusive() {
        // live at venue (order id came back) → managed OrderAccepted carrying that id
        match resolve_ambiguous_submit("c1", 5, Ok(Some("od-1".to_string()))) {
            Event::OrderAccepted(a) => {
                assert_eq!(a.client_order_id, "c1");
                assert_eq!(a.venue_order_id.as_deref(), Some("od-1"));
                assert_eq!(a.ts, 5);
            }
            other => panic!("expected OrderAccepted, got {other:?}"),
        }
        // venue confirms absent → the only case that becomes a TRUE terminal reject
        match resolve_ambiguous_submit("c1", 5, Ok(None)) {
            Event::OrderRejected(r) => {
                assert_eq!(r.client_order_id, "c1");
                assert!(r.reason.contains("venue confirms order absent"), "reason: {}", r.reason);
            }
            other => panic!("expected OrderRejected, got {other:?}"),
        }
        // inconclusive re-query (transport error) → optimistic OrderAccepted, NEVER a false terminal
        match resolve_ambiguous_submit("c1", 5, Err(VenueApiError { code: 0, msg: "x".into() })) {
            Event::OrderAccepted(a) => assert_eq!(a.venue_order_id, None),
            other => panic!("expected optimistic OrderAccepted, got {other:?}"),
        }
    }

    /// A filled order re-query returns the venue id (the venue HAS the order), so the resolver emits
    /// OrderAccepted — the order is managed, and its fill follows on the user-data lane. The one
    /// thing it must never do for a venue-held order is synthesize a reject.
    #[test]
    fn venue_held_order_is_never_rejected() {
        assert_matches!(
            resolve_ambiguous_submit("c9", 1, Ok(Some("v-9".to_string()))),
            Event::OrderAccepted(_)
        );
    }
}
