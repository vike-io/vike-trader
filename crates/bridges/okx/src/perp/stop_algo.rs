//! `OkxPerpRest`'s conditional STOP leg on the algo endpoint (`/api/v5/trade/order-algo`).

use serde_json::{Value, json};

use vike_bridge_core::format::format_to_step_f;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};

use super::{OkxPerpRest, PATH_ORDER_ALGO, VENUE, swap_td_mode};
use crate::error_codes::submit_outcome_unknown;
use crate::transport::OkxTransport;

impl<T: OkxTransport> OkxPerpRest<T> {
    /// The stop-algo body, pure (the algo twin of [`Self::build_order_params`], extracted so the
    /// wire shape is pinnable without a transport). Base params byte-identical to the historical
    /// inline vec; a requested `trigger_by` appends `slTriggerPxType` off okx's
    /// `vike_bridge_core::trigger::venue_trigger_by` row (`last`/`mark`/`index` — all three
    /// express, nothing to deny). `None` emits no param — the venue default (`last`) rules,
    /// byte-identical to before the field existed.
    pub(super) fn build_stop_algo_params(
        &self,
        request: &OrderRequest,
        td_mode: &str,
    ) -> Vec<(&str, Value)> {
        let mut params: Vec<(&str, Value)> = vec![
            ("instId", json!(self.symbol)),
            ("tdMode", json!(td_mode)),
            ("side", json!(if request.side > 0 { "buy" } else { "sell" })),
            ("ordType", json!("conditional")),
            (
                "sz",
                json!(format_to_step_f(self.to_contracts(request.qty), self.properties.step_size)),
            ),
            ("posSide", json!("net")),
            ("reduceOnly", json!(request.reduce_only)),
            ("algoClOrdId", json!(request.client_order_id)),
            (
                "slTriggerPx",
                json!(format_to_step_f(
                    request.trigger_price.unwrap_or(0.0),
                    self.properties.tick_size
                )),
            ),
            ("slOrdPx", json!("-1")), // -1 = fire as a market order on trigger
        ];
        if let Some(wire) = request
            .trigger_by
            .and_then(|s| vike_bridge_core::trigger::venue_trigger_by(VENUE, s).wire())
        {
            params.push(("slTriggerPxType", json!(wire)));
        }
        params
    }

    /// Native conditional STOP (a bracket stop-loss leg) via /api/v5/trade/order-algo — OKX places
    /// stops as ALGO orders, not on the regular /order endpoint. `ordType:"conditional"` with an
    /// `slTriggerPx` and a market `slOrdPx` ("-1"); `algoClOrdId` carries the coid so the accept
    /// maps back. Returns [Submitted, Accepted(algoId)|Rejected]. RUST-NATIVE, no Python twin.
    pub fn submit_stop_algo(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            client_order_id: request.client_order_id.clone(),
            ts: request.ts,
        })];
        // Same margin-mode gate as the regular order path (unset = the historical "cross").
        let td_mode = match swap_td_mode(request.margin_mode) {
            Ok(m) => m,
            Err(reason) => {
                events.push(Event::OrderRejected(OrderRejected {
                    client_order_id: request.client_order_id.clone(),
                    reason: reason.into(),
                    ts: request.ts,
                }));
                return events;
            }
        };
        let params = self.build_stop_algo_params(request, td_mode);
        match self.call(PATH_ORDER_ALGO, "POST", &params) {
            Ok(data) => events.push(Event::OrderAccepted(OrderAccepted {
                client_order_id: request.client_order_id.clone(),
                venue_order_id: data
                    .as_array()
                    .and_then(|d| d.first())
                    .and_then(|d0| d0.get("algoId"))
                    .and_then(|a| a.as_str())
                    .map(Into::into),
                ts: request.ts,
            })),
            // audit T1: a stop-algo is keyed by algoClOrdId on the order-algo endpoint, NOT queryable
            // by clOrdId — so an ambiguous timeout resolves OPTIMISTICALLY (never a false terminal;
            // authoritative state still arrives on the user-data WS). Precise algo requery is a
            // follow-up (see the OKX stop-algo lifecycle audit finding).
            Err(exc) if submit_outcome_unknown(exc.code) => {
                events.push(Event::OrderAccepted(OrderAccepted {
                    client_order_id: request.client_order_id.clone(),
                    venue_order_id: None,
                    ts: request.ts,
                }));
            }
            Err(exc) => events.push(Event::OrderRejected(OrderRejected {
                client_order_id: request.client_order_id.clone(),
                reason: exc.msg.into(),
                ts: request.ts,
            })),
        }
        events
    }
}
