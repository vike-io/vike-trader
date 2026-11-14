//! `VenueRestOkx`'s native batch submit (`/api/v5/trade/batch-orders`). The native batch cancel was deleted (owner
//! ruling X-2: no command loop declares a bulk cancel lane, so it was unreachable); a batch cancel is the
//! `VenueRest` default's per-order fan-out.

use serde_json::Value;

use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
use vike_bridge_core::transport::json::RestTransportJson;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};

use super::{BATCH_MAX, PATH_BATCH_ORDERS, VENUE, VenueRestOkx};
use crate::transport::error_codes::submit_outcome_unknown;

impl<T: RestTransportJson> VenueRestOkx<T> {
    /// Native batch-submit (POST /api/v5/trade/batch-orders — a JSON ARRAY body, ≤20/req). Emits
    /// [OrderSubmitted] for every order, then per-order [OrderAccepted|OrderRejected] mapped from
    /// the response's `data[i].sCode` (matched by clOrdId). Chunks at BATCH_MAX; a whole-request
    /// failure rejects that chunk so NO order silently vanishes. RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::with_capacity(requests.len() * 2);
        for r in requests {
            events.push(Event::OrderSubmitted(OrderSubmitted {
                order_client_id: r.order_client_id.clone(),
                timestamp: r.timestamp,
            }));
        }
        // Per-order gates, applied BEFORE chunking so a denied order never enters a wire chunk:
        // the tif step-2 gate (`deny_unsupported_time_in_force` — GTD/Day have no okx ordType) and the
        // margin-mode gate (Cash can't ride the SWAP wire). The rest batch as before.
        let mut sendable: Vec<(&OrderRequest, Vec<(&'static str, Value)>)> =
            Vec::with_capacity(requests.len());
        for r in requests {
            if let Some(reject) =
                vike_venues::venue_time_in_force::deny_unsupported_time_in_force(VENUE, r)
            {
                events.push(reject);
                continue;
            }
            // The FD-broker `tag` rides a batch row exactly as it rides a single order (`submit_order`).
            match self.build_order_params(r, self.broker_code.as_deref()) {
                Ok(p) => sendable.push((r, p)),
                Err(reason) => events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: r.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: r.timestamp,
                })),
            }
        }
        for chunk in sendable.chunks(BATCH_MAX) {
            let body_orders: Vec<Value> = chunk
                .iter()
                .map(|(_, params)| {
                    Value::Object(params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
                })
                .collect();
            let body = serde_json::to_string(&body_orders).expect("array body");
            match self.transport.signed_json(
                &self.base_url,
                PATH_BATCH_ORDERS,
                "POST",
                &body,
                &self.signer,
            ) {
                Ok(response) => {
                    // HTTP 200 + a status-unknown envelope `code` (OKX sends it as a string): the
                    // batch may have landed — re-query each order. "0"/"1"/"2" are the batch verdicts
                    // and the predicate rejects them, so the per-row reading below stands.
                    let envelope_code = response
                        .get("code")
                        .and_then(|c| c.as_str())
                        .and_then(|c| c.parse::<i64>().ok());
                    if let Some(code) = envelope_code.filter(|c| submit_outcome_unknown(*c)) {
                        for (r, _) in chunk {
                            events.extend(self.resolve_ambiguous_submit(r, code));
                        }
                        continue;
                    }
                    let data = response
                        .get("data")
                        .and_then(|d| d.as_array())
                        .cloned()
                        .unwrap_or_default();
                    for (r, _) in chunk {
                        let row = data.iter().find(|d| {
                            d.get("clOrdId").and_then(|c| c.as_str())
                                == Some(r.order_client_id.as_str())
                        });
                        let row_code = row
                            .and_then(|d| d.get("sCode"))
                            .and_then(|s| s.as_str())
                            .and_then(|s| s.parse::<i64>().ok());
                        let id = row
                            .and_then(|d| d.get("ordId"))
                            .and_then(|o| o.as_str())
                            .filter(|id| !id.is_empty());
                        match (row, row_code) {
                            // a status-unknown per-order `sCode`: that order may be live — re-query it
                            (_, Some(code)) if submit_outcome_unknown(code) => {
                                events.extend(self.resolve_ambiguous_submit(r, code));
                            }
                            (_, Some(0)) if id.is_some() => {
                                events.push(Event::OrderAccepted(OrderAccepted {
                                    order_client_id: r.order_client_id.clone(),
                                    venue_order_id: id.map(Into::into),
                                    timestamp: r.timestamp,
                                }));
                            }
                            (Some(d), Some(code)) if code != 0 => {
                                events.push(Event::OrderRejected(OrderRejected {
                                    cause: None,
                                    order_client_id: r.order_client_id.clone(),
                                    reason: crate::account::modes::explain_refusal(
                                        d.get("sMsg")
                                            .and_then(|m| m.as_str())
                                            .unwrap_or("batch reject"),
                                        self.transport.network(),
                                    )
                                    .into(),
                                    timestamp: r.timestamp,
                                }));
                            }
                            // a 2xx that says nothing about this order (no row, no `sCode`, or a
                            // success that names no order): it may rest, so ask
                            _ => {
                                events.extend(self.resolve_ambiguous_submit(r, E_TIMEOUT_AMBIGUOUS))
                            }
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if submit_outcome_unknown(exc.code) => {
                    for (r, _) in chunk {
                        events.extend(self.resolve_ambiguous_submit(r, exc.code));
                    }
                }
                Err(exc) => {
                    for (r, _) in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            cause: None,
                            order_client_id: r.order_client_id.clone(),
                            reason: crate::account::modes::explain_refusal(
                                &exc.msg,
                                self.transport.network(),
                            )
                            .into(),
                            timestamp: r.timestamp,
                        }));
                    }
                }
            }
        }
        events
    }
}
