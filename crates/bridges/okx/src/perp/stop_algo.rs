//! `OkxPerpRest`'s conditional STOP leg on the algo endpoint (`/api/v5/trade/order-algo`).

use serde_json::{Value, json};

use vike_bridge_core::format::format_to_step_f;
use vike_bridge_core::json::json_text;
use vike_bridge_core::transport::VenueApiError;
use vike_model::OrderRequest;
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted};

use super::{
    NOT_FOUND, OkxPerpRest, PATH_CANCEL_ALGOS, PATH_ORDER_ALGO, PATH_ORDERS_ALGO_HISTORY, VENUE,
    swap_td_mode,
};
use crate::error_codes::submit_outcome_unknown;
use crate::transport::{OkxTransport, unwrap_okx};

/// Do `code`'s words say "the algo endpoints have no such algo order"? The regular book's cancel
/// family ([`NOT_FOUND`]) plus `51603`. **Measured 2026-10-23 on the demo account** (the smoke's
/// `MEASURED cancel-algos of an unknown id` and `MEASURED GET order-algo of an unknown id` lines in
/// `tests/okx_algo_stop_smoke.rs`): `cancel-algos` of an unknown id answers `51400` and `order-algo` of
/// an unknown id answers `51603`, both in the set. The algo endpoints' OWN unknown-id answer is not in
/// the vendor docs (`okx-algo-cancel.md` claim 20), so anything outside the set stays an error the
/// caller must not read as "gone". `51413` and `51403` ("order type not supported") are deliberately
/// NOT in it, and still UNMEASURED: no line shows OKX answering them.
fn algo_unknown(code: i64) -> bool {
    NOT_FOUND.contains(&code) || code == 51603
}

/// The code of the error `cancel_algo` synthesizes when `cancel-algos` said "unknown" but `order-algo`
/// still holds the stop in a state other than `canceled`: the vendor's "triggered and can't be
/// canceled" word, so a caller sees a venue-shaped refusal and not a made-up number.
const ALGO_NOT_CANCELED: i64 = 51412;

impl<T: OkxTransport> OkxPerpRest<T> {
    /// Cancel one STOP: `POST /api/v5/trade/cancel-algos` by `algoClOrdId` (= our coid). The body is a
    /// JSON ARRAY, so it rides `signed_json`, which returns the RAW envelope: `unwrap_okx` reads the
    /// per-row `sCode`. A coded non-2xx reply arrives as `Err` and goes through the same code check as
    /// a 200 envelope. "Unknown" ([`algo_unknown`]) means it already left the venue by itself
    /// (triggered, cancelled elsewhere) — idempotent, like the regular cancel, but only once
    /// [`Self::confirm_algo_gone`] agrees; any other answer is an error, never "gone" while the stop
    /// may still rest.
    pub(super) fn cancel_algo(&self, coid: &str) -> Result<(), VenueApiError> {
        let body = json!([{ "instId": self.symbol, "algoClOrdId": coid }]).to_string();
        let resp = self.transport.signed_json(
            &self.base_url,
            PATH_CANCEL_ALGOS,
            "POST",
            &body,
            &self.signer,
        );
        match resp.and_then(unwrap_okx) {
            Ok(_) => Ok(()),
            Err(exc) if algo_unknown(exc.code) => self.confirm_algo_gone(coid),
            Err(exc) => Err(exc),
        }
    }

    /// "Unknown" from `cancel-algos` is not yet "gone": a stop that just TRIGGERED may be unknown to
    /// the cancel too (the vendor lists `51412`, "triggered and can't be canceled"), and booking an
    /// `OrderCanceled` over it hides the fill that follows. So the algo book is asked once
    /// ([`Self::algo_row`], the ungated requery agent): no row, state `canceled` or state
    /// `order_failed` = gone (`Ok`); any other state (`live`, `pause`, `effective`, ... — the stop
    /// rests, or fired) = `Err`, no cancel is booked and reconcile or the child order's fill resolves
    /// it. `order_failed` is a trigger that FIRED and whose order the venue refused (a reduce-only stop
    /// on a flat position): nothing rests and nothing filled, and its word was MEASURED on the demo
    /// account on 2026-10-24 (`order-algo` held the stop in it while `cancel-algos` did not know it),
    /// so the node may close the stop instead of holding it ACCEPTED forever. ⚠ `partially_failed` is
    /// NOT mapped: part of the stop took effect, its word is unmeasured, and it stays an `Err` on
    /// every cancel (an open item, spelled out at the end of the stop paragraph of the okx crate's own
    /// CLAUDE.md). ⚠ A lookup that FAILS leaves the venue's own "unknown" standing (`Ok`): an ordinary
    /// order's cancel race must not turn on a second endpoint's health, and the
    /// stop-fired-AND-lookup-failed case is the narrow residual.
    fn confirm_algo_gone(&self, coid: &str) -> Result<(), VenueApiError> {
        match self.algo_row(coid) {
            Ok(Some(row)) => match row.get("state").and_then(Value::as_str) {
                Some("canceled" | "order_failed") => Ok(()),
                state => Err(VenueApiError {
                    code: ALGO_NOT_CANCELED,
                    msg: format!(
                        "stop {coid} was not cancelled: cancel-algos did not know it but order-algo \
                         holds it in state {}",
                        state.unwrap_or("(none)")
                    ),
                }),
            },
            Ok(None) => {
                tracing::debug!(target: "vike_okx::perp", coid, "cancel: algo order already gone");
                Ok(())
            }
            Err(e) => {
                tracing::debug!(target: "vike_okx::perp", coid, code = e.code, "cancel: order-algo lookup failed, the venue's unknown stands");
                Ok(())
            }
        }
    }

    /// `GET /api/v5/trade/order-algo` by `algoClOrdId`: `Ok(Some(algoId))` = the venue knows the stop
    /// (live or finished, read by existence like the regular lookup), `Ok(None)` = it does not, `Err` =
    /// the query failed (says nothing about the order).
    pub(super) fn lookup_algo(&self, coid: &str) -> Result<Option<String>, VenueApiError> {
        Ok(self
            .algo_row(coid)?
            .and_then(|row| row.get("algoId").map(json_text))
            .filter(|id| !id.is_empty()))
    }

    /// Audit A3 resync of the ALGO socket: the newest `limit` stops of `state` (`canceled` or
    /// `order_failed`; the venue requires one) from `GET /api/v5/trade/orders-algo-history`, on the
    /// short-timeout requery transport, as the unwrapped `data` list. `ordType` and `state` are both
    /// required by the venue (`ordType=conditional` is the only kind this adapter places).
    pub fn get_algo_history(&self, state: &str, limit: u32) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("ordType", json!("conditional")),
            ("state", json!(state)),
            ("instId", json!(self.symbol)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_ORDERS_ALGO_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// The first row of `GET /api/v5/trade/order-algo` for `coid` (`algoClOrdId`), or `None` when the
    /// venue answers "unknown" ([`algo_unknown`]) or an empty list.
    fn algo_row(&self, coid: &str) -> Result<Option<Value>, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![("algoClOrdId", json!(coid))];
        let resp = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &params,
            &self.signer,
        );
        match resp.and_then(unwrap_okx) {
            Ok(data) => Ok(data.as_array().and_then(|d| d.first()).cloned()),
            Err(exc) if algo_unknown(exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

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
