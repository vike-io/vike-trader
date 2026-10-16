//! `OkxPerpRest`'s native batch submit/cancel (`/api/v5/trade/batch-orders`, `cancel-batch-orders`).

use serde_json::{Value, json};

use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};

use super::{BATCH_MAX, OkxPerpRest, PATH_BATCH_ORDERS, PATH_CANCEL_BATCH, VENUE};
use crate::transport::OkxTransport;

impl<T: OkxTransport> OkxPerpRest<T> {
    /// Native batch-submit (POST /api/v5/trade/batch-orders — a JSON ARRAY body, ≤20/req). Emits
    /// [OrderSubmitted] for every order, then per-order [OrderAccepted|OrderRejected] mapped from
    /// the response's `data[i].sCode` (matched by clOrdId). Chunks at BATCH_MAX; a whole-request
    /// failure rejects that chunk so NO order silently vanishes. RUST-NATIVE, no Python twin.
    pub fn submit_batch(&self, requests: &[OrderRequest]) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::with_capacity(requests.len() * 2);
        for r in requests {
            events.push(Event::OrderSubmitted(OrderSubmitted {
                client_order_id: r.client_order_id.clone(),
                ts: r.ts,
            }));
        }
        // Per-order gates, applied BEFORE chunking so a denied order never enters a wire chunk:
        // the tif step-2 gate (`deny_unsupported_tif` — GTD/Day have no okx ordType) and the
        // margin-mode gate (Cash can't ride the SWAP wire). The rest batch as before.
        let mut sendable: Vec<(&OrderRequest, Vec<(&'static str, Value)>)> =
            Vec::with_capacity(requests.len());
        for r in requests {
            if let Some(reject) = vike_venues::venue_tif::deny_unsupported_tif(VENUE, r) {
                events.push(reject);
                continue;
            }
            // TODO(unified-venue-attribution): batch submits don't yet carry the FD-broker `tag`
            // (scoped to the single-order path this task covers); a follow-up should thread
            // `self.broker_code.as_deref()` here too so batch orders attribute identically.
            match self.build_order_params(r, None) {
                Ok(p) => sendable.push((r, p)),
                Err(reason) => events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: r.client_order_id.clone(),
                    reason: reason.into(),
                    ts: r.ts,
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
                Ok(resp) => {
                    let data =
                        resp.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
                    for (r, _) in chunk {
                        let row = data.iter().find(|d| {
                            d.get("clOrdId").and_then(|c| c.as_str())
                                == Some(r.client_order_id.as_str())
                        });
                        match row {
                            Some(d) if d.get("sCode").and_then(|s| s.as_str()) == Some("0") => {
                                events.push(Event::OrderAccepted(OrderAccepted {
                                    client_order_id: r.client_order_id.clone(),
                                    venue_order_id: d
                                        .get("ordId")
                                        .and_then(|o| o.as_str())
                                        .map(Into::into),
                                    ts: r.ts,
                                }));
                            }
                            Some(d) => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: d
                                    .get("sMsg")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("batch reject")
                                    .into(),
                                ts: r.ts,
                            })),
                            None => events.push(Event::OrderRejected(OrderRejected {
                                client_order_id: r.client_order_id.clone(),
                                reason: "no batch ack".to_string().into(),
                                ts: r.ts,
                            })),
                        }
                    }
                }
                // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each.
                Err(exc) if exc.code == vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS => {
                    for (r, _) in chunk {
                        events.push(self.resolve_ambiguous_submit(r));
                    }
                }
                Err(exc) => {
                    for (r, _) in chunk {
                        events.push(Event::OrderRejected(OrderRejected {
                            client_order_id: r.client_order_id.clone(),
                            reason: exc.msg.clone().into(),
                            ts: r.ts,
                        }));
                    }
                }
            }
        }
        events
    }

    /// Native batch-cancel (POST /api/v5/trade/cancel-batch-orders — JSON ARRAY body, ≤20/req).
    /// Per-order "already gone" is non-fatal (the authoritative OrderCanceled arrives on the
    /// user-data WS, mirroring cancel_order's idempotence); a whole-request/network failure returns
    /// the first error. RUST-NATIVE.
    pub fn cancel_batch(&self, client_order_ids: &[String]) -> Result<(), VenueApiError> {
        let mut first_err: Option<VenueApiError> = None;
        for chunk in client_order_ids.chunks(BATCH_MAX) {
            let body_orders: Vec<Value> =
                chunk.iter().map(|c| json!({ "instId": self.symbol, "clOrdId": c })).collect();
            let body = serde_json::to_string(&body_orders).expect("array body");
            if let Err(e) = self.transport.signed_json(
                &self.base_url,
                PATH_CANCEL_BATCH,
                "POST",
                &body,
                &self.signer,
            ) {
                first_err.get_or_insert(e);
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}
