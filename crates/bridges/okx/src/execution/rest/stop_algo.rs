//! `VenueRestOkx`'s conditional STOP leg on the algo endpoint (`/api/v5/trade/order-algo`): a `stop`,
//! a `stop_limit` and a `take_profit` are each ONE `ordType=conditional` algo order. A stop and a
//! stop-limit are its stop-loss side, told apart by `slOrdPx` alone (`-1` fires a market order, a price
//! rests a limit order there); a take-profit is its take-profit side, `tpTriggerPx` and `tpOrdPx` (`-1`
//! for a market-if-touched, the limit for a limit-if-touched). One side per order: the venue performs
//! only the stop-loss when both are sent. The field words are the official "Place algo order" table's
//! (<https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-place-algo-order>, read
//! 2026-11-08: `slTriggerPx` "Stop-loss trigger price", `slOrdPx` "Stop-loss order price ... If the
//! price is -1, stop-loss will be executed at the market price", `tpTriggerPx` "Take-profit trigger
//! price", `tpOrdPx` "Take-profit order price ... If the price is -1, take-profit will be executed at
//! the market price", `tpOrdKind` "condition limit The default is condition": not sent, so the
//! take-profit is the conditional one). Every path a stop takes (the `cancel-algos` retry, the
//! `order-algo` lookup, the strict report, the algo socket, a fired order's child filed under
//! `algoClOrdId`) holds for the take-profit unchanged: it is the same `conditional` order.
//!
//! A `trailing` order is the same endpoint's `ordType=move_order_stop` (the same table: "Trailing
//! order"; `callbackSpread` "Callback price variance", our `trail`, a price distance; or
//! `callbackRatio`, our `trail_percent` / 100 ("Callback ratio, e.g. `0.05` represents 5%", "Either
//! `callbackRatio` or `callbackSpread` is required. Only one can be passed", read 2026-11-09; no range
//! or precision is documented); `activePx` "Active price", our activation, the trigger price of a
//! trailing kind, absent = "activated as soon as it's placed"; "Once the trigger price is reached, a
//! market order is placed"). So okx has no trailing LIMIT: a `trailing_limit` is refused by name.
//! The trailing order states no trigger-price type, it carries no stop-loss or take-profit word, and
//! "Amend algo order" excludes it ("not including Move_order_stop order"): a trailing order's amend is
//! refused in words.
//! `cancel-algos` and `order-algo` take any algo order by `algoClOrdId`, so its cancel and lookup are a
//! stop's. ⚠ Its own state is pushed on the business socket's `algo-advance` channel ("Iceberg order,
//! TWAP order, Trailing order"), not on `orders-algo`; the algo socket subscribes it only while okx's row
//! lists `trailing` (`crate::execution::user_data::open_okx_algo_websocket`), and until then a trailing order the venue ends
//! by itself reaches the node through reconcile alone (its fired child through the private socket's
//! `orders` rows, filed under `algoClOrdId`).

use serde_json::{Value, json};

use vike_bridge_core::format::{format_scaled_to_step_f, format_to_step_f};
use vike_bridge_core::rest::{OrderCancelAck, amend_answer_time};
use vike_bridge_core::transport::json::RestTransportJson;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_model::events::{
    Event, OrderAccepted, OrderModified, OrderModifyRejected, OrderRejected, OrderSubmitted,
};
use vike_model::{OrderRefusalId, OrderRequest, OrderType, TimeInForce, TriggerBy};

use super::{
    PATH_AMEND_ALGOS, PATH_CANCEL_ALGOS, PATH_ORDER_ALGO, PATH_ORDER_CANCEL,
    PATH_ORDERS_ALGO_HISTORY, VENUE, VenueRestOkx, class, first_row_id, row_id,
};
use crate::instrument::{InstrumentType, SpotMode};
use crate::transport::client::unwrap_okx;
use crate::transport::error_codes::{TAXONOMY, cancel_says_gone, modify_failed};

/// Do `code`'s words say "the algo endpoints have no such algo order"? The regular book's cancel
/// family ([`cancel_says_gone`]) plus `51603`. **Measured 2026-10-23 on the demo account** (the smoke's
/// `MEASURED cancel-algos of an unknown id` and `MEASURED GET order-algo of an unknown id` lines in
/// `tests/okx_smokes/okx_algo_stop_smoke.rs`): `cancel-algos` of an unknown id answers `51400` and `order-algo` of
/// an unknown id answers `51603`, both in the set. The algo endpoints' OWN unknown-id answer is not in
/// the vendor docs (`okx-algo-cancel.md` claim 20), so anything outside the set stays an error the
/// caller must not read as "gone". `51413` and `51403` ("order type not supported") are deliberately
/// NOT in it, and still UNMEASURED: no line shows OKX answering them.
fn algo_unknown(code: i64) -> bool {
    cancel_says_gone(code) || code == 51603
}

/// Why a `stop_limit` or a `take_profit` cannot be written as one `conditional` algo order, or `None`:
/// the shared order-terms rule first (a trigger always; a stop-limit's limit), then, for an order that
/// carries a limit, a time in force other than GTC (OKX's algo order has no TIF field, and the order its
/// trigger places is a plain good-till-cancel limit). A take-profit with no limit fires a market order,
/// which takes no lifetime here, as a stop's does not. Beside the text, its id: a term is `NotSent`, the
/// lifetime `VenueCapability`.
fn trigger_order_refusal(request: &OrderRequest) -> Option<(String, OrderRefusalId)> {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    vike_model::order_terms_refusal(
        &request.order_type,
        request.price,
        request.trigger_price,
        request.trail,
        request.limit_offset,
        request.peg.as_ref(),
        request.trail_percent,
    )
    .map(|why| (why, NotSent))
    .or_else(|| {
        (request.price.is_some() && request.time_in_force != TimeInForce::Gtc).then(|| {
            let why = format!(
                "okx places a {} as a conditional algo order whose triggered order is a \
                     good-till-cancel limit; time in force `{}` cannot be written",
                request.order_type,
                request.time_in_force.as_word()
            );
            (why, VenueCapability)
        })
    })
}

/// Why a `trailing` or a `trailing_limit` cannot be written as one `move_order_stop` algo order, or
/// `None`. A `trailing_limit` always: the venue's trailing order fires a MARKET order. A `trailing`: the
/// shared order-terms rule (exactly one distance: a finite trail above zero or a percent above 0 and
/// below 100; no price; an activation above zero when one is stated), a PRICE trail that the `tick`
/// grid rounds to nothing (a percent has no grid to round to), a `trigger_by` other than `Last` (the
/// order states no trigger-price type; the table says it "tracks the market price"), and a SPOT buy (the
/// `tgtCcy` row counts a spot market buy's `sz` in the quote and is documented for conditional orders
/// only, so a buy's size in the base cannot be written). Beside the text, its id: a term is `NotSent`, what
/// the venue's trailing order cannot carry `VenueCapability`.
fn trailing_order_refusal(
    request: &OrderRequest,
    class: InstrumentType,
    tick: f64,
) -> Option<(String, OrderRefusalId)> {
    use vike_model::OrderRefusalId::{NotSent, VenueCapability};
    if request.order_type == OrderType::TrailingLimit {
        return Some((
            "okx has no trailing limit order: its trailing order (move_order_stop) places a market \
             order when it fires; a trailing_limit is refused"
                .to_string(),
            VenueCapability,
        ));
    }
    if let Some(why) = vike_model::order_terms_refusal(
        &request.order_type,
        request.price,
        request.trigger_price,
        request.trail,
        request.limit_offset,
        request.peg.as_ref(),
        request.trail_percent,
    ) {
        return Some((why, NotSent));
    }
    if let Some(trail) = request.trail
        && format_to_step_f(trail, tick).parse::<f64>().unwrap_or(0.0) <= 0.0
    {
        let why = format!(
            "okx: the trail {trail} rounds to nothing on the {tick} price grid; a trailing order \
             needs a trail of at least one tick"
        );
        return Some((why, NotSent));
    }
    if let Some(source @ (TriggerBy::Mark | TriggerBy::Index)) = request.trigger_by {
        let why = format!(
            "okx's trailing order states no trigger-price type (it tracks the market price); \
             trigger_by {source:?} cannot be written"
        );
        return Some((why, VenueCapability));
    }
    let spot_buy = (class == InstrumentType::Spot && request.side > 0).then(|| {
        "okx counts a spot market buy's size in the quote coin and documents `tgtCcy` (the base) for \
         conditional orders only; a spot BUY trailing order is refused"
            .to_string()
    });
    spot_buy.map(|why| (why, VenueCapability))
}

/// `callbackRatio` of a percent trail: `percent` / 100 as an exact decimal (a step of 0 quantizes
/// nothing: the venue documents no grid for it), normalized so no trailing zero rides the wire (2%
/// is `0.02`, never `0.020`).
fn callback_ratio(percent: f64) -> String {
    let exact = format_scaled_to_step_f(percent, 1, 100, 0.0);
    exact.parse::<rust_decimal::Decimal>().map_or(exact, |d| d.normalize().to_string())
}

/// The regular order a FIRED limit-bounded trigger (a stop-limit, a take-profit with a limit) placed, by
/// `ordId`, or `None`: the row must carry a limit (`crate::reconcile::conditional_kind`: a priced
/// `slOrdPx`, or a priced `tpOrdPx` on a take-profit; a `-1` fired a market order, which rests nowhere)
/// and name the child in `ordIdList` (the official algo-list field: "Order ID list"), else in `ordId`
/// ("Latest order ID").
fn fired_limit_child(row: &Value) -> Option<String> {
    let text = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("");
    crate::reconcile::conditional_kind(row).1?;
    let listed = row.get("ordIdList").and_then(Value::as_array).and_then(|ids| ids.first());
    let id = listed.and_then(Value::as_str).unwrap_or_else(|| text("ordId"));
    (!id.is_empty()).then(|| id.to_string())
}

/// The code of the error `cancel_algo` synthesizes when `cancel-algos` said "unknown" but `order-algo`
/// still holds the stop in a state other than `canceled`: the vendor's "triggered and can't be
/// canceled" word, so a caller sees a venue-shaped refusal and not a made-up number.
const ALGO_NOT_CANCELED: i64 = 51412;

impl<T: RestTransportJson> VenueRestOkx<T> {
    /// Cancel one STOP: `POST /api/v5/trade/cancel-algos` by `algoClOrdId` (= our order client id). The body is a
    /// JSON ARRAY, so it rides `signed_json`, which returns the RAW envelope: `unwrap_okx` reads the
    /// per-row `sCode`. A coded non-2xx reply arrives as `Err` and goes through the same code check as
    /// a 200 envelope. "Unknown" ([`algo_unknown`]) means it already left the venue by itself
    /// (triggered, cancelled elsewhere) — idempotent, like the regular cancel, but only once
    /// [`Self::confirm_algo_gone`] agrees; any other answer is an error, never "gone" while the stop
    /// may still rest.
    pub(super) fn cancel_algo(
        &self,
        order_client_id: &str,
    ) -> Result<OrderCancelAck, VenueApiError> {
        let body = json!([{ "instId": self.symbol, "algoClOrdId": order_client_id }]).to_string();
        let response = self.transport.signed_json(
            &self.base_url,
            PATH_CANCEL_ALGOS,
            "POST",
            &body,
            &self.signer,
        );
        match response.and_then(unwrap_okx) {
            Ok(_) => Ok(OrderCancelAck::Canceled),
            Err(exc) if algo_unknown(exc.code) => self.confirm_algo_gone(order_client_id),
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
    ///
    /// A fired STOP-LIMIT, or a fired TAKE-PROFIT with a limit, is the one `effective` trigger that can
    /// still rest: it placed a regular LIMIT order ([`fired_limit_child`]), which our order client id does not reach on `cancel-order`
    /// (its `clOrdId` is the venue's). That child is cancelled by its `ordId`
    /// ([`Self::cancel_fired_limit`]); a child that already left by itself keeps the `Err` below, as
    /// a fired stop's always did.
    fn confirm_algo_gone(&self, order_client_id: &str) -> Result<OrderCancelAck, VenueApiError> {
        match self.algo_row(order_client_id) {
            Ok(Some(row)) => {
                let state = row.get("state").and_then(Value::as_str);
                if matches!(state, Some("canceled" | "order_failed")) {
                    return Ok(OrderCancelAck::AlreadyGone);
                }
                if state == Some("effective")
                    && let Some(child) = fired_limit_child(&row)
                    && let Some(done) = self.cancel_fired_limit(order_client_id, &child)
                {
                    return done;
                }
                Err(VenueApiError {
                    code: ALGO_NOT_CANCELED,
                    msg: format!(
                        "stop {order_client_id} was not cancelled: cancel-algos did not know it but order-algo \
                         holds it in state {}",
                        state.unwrap_or("(none)")
                    ),
                })
            }
            Ok(None) => {
                tracing::debug!(target: "vike_okx::execution::rest", order_client_id, "cancel: algo order already gone");
                Ok(OrderCancelAck::AlreadyGone)
            }
            Err(e) => {
                tracing::debug!(target: "vike_okx::execution::rest", order_client_id, code = e.code, "cancel: order-algo lookup failed, the venue's unknown stands");
                Ok(OrderCancelAck::AlreadyGone)
            }
        }
    }

    /// Cancel the LIMIT order a fired stop-limit `order_client_id` left on the regular book: `POST cancel-order`
    /// by the child's `ordId` (the ordinary cancel's endpoint and gate). `Some(Ok(Canceled))` when the
    /// venue took it (the child's own `canceled` row, filed under `order_client_id` by
    /// `crate::execution::event_mapper::row_order_client_id`, follows on the private socket); `None` when the venue says the
    /// child is already gone ([`cancel_says_gone`]: filled or cancelled, and its own row closes the
    /// order), so the caller keeps the fired stop's refusal; `Some(Err)` for any other answer.
    fn cancel_fired_limit(
        &self,
        order_client_id: &str,
        child: &str,
    ) -> Option<Result<OrderCancelAck, VenueApiError>> {
        let params: Vec<(&str, Value)> =
            vec![("instId", json!(self.symbol)), ("ordId", json!(child))];
        match self.call(PATH_ORDER_CANCEL, "POST", &params) {
            Ok(_) => Some(Ok(OrderCancelAck::Canceled)),
            Err(exc) if cancel_says_gone(exc.code) => {
                tracing::debug!(target: "vike_okx::execution::rest", order_client_id, child, code = exc.code, "cancel: the fired stop-limit's order already left the book");
                None
            }
            Err(exc) => Some(Err(exc)),
        }
    }

    /// Amend one resting trigger order (a stop, a stop-limit, a take-profit: each one `conditional` algo
    /// order) in place: `POST /api/v5/trade/amend-algos` by `algoClOrdId` (= our order client id), the official "Amend
    /// algo order" table (<https://www.okx.com/docs-v5/en/#order-book-trading-algo-trading-post-amend-algo-order>,
    /// read 2026-11-08: "Amend unfilled algo orders (Support Stop order and Trigger order only ...)";
    /// `newSz`, `newSlTriggerPx`, `newSlOrdPx`, `newTpTriggerPx`, `newTpOrdPx`). `amend-order` never reaches
    /// one. The two prices land where `vike_model::modified_prices` says (a stop's `new_price` IS its
    /// trigger; a stop-limit's and a take-profit's is the limit, the trigger `new_trigger_price`), on the
    /// order's own side: `newSl*` for a stop and a stop-limit, `newTp*` for a take-profit.
    ///
    /// ⚠ When either price moves BOTH words of the side are sent, the unmoved one at the order's current
    /// value (a stop's order price stays `-1`): the table says "Either the stop-loss trigger price or order
    /// price is 0, it means that the stop-loss is deleted" and does not say what an ABSENT one means, so
    /// nothing is left to it. A size alone sends `newSz` alone, as the table's own example does.
    ///
    /// `[OrderModified]` (the `algoId` as its venue id, the prices as asked) on the venue's success; else
    /// the non-terminal `OrderModifyRejected`, with nothing sent when the new terms cannot be written: a
    /// market-if-touched has no limit to move, and the shared order-terms rule
    /// (`vike_model::order_terms_refusal`) judges the terms the amend would leave.
    pub(super) fn amend_algo(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
    ) -> Vec<Event> {
        let order_client_id = &order.order_client_id;
        let params = match self.amend_algo_params(order, new_qty, new_price, new_trigger_price) {
            Ok(params) => params,
            Err((reason, family)) => {
                return vec![Event::OrderModifyRejected(OrderModifyRejected {
                    order_client_id: order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: amend_answer_time(None),
                    refusal_id: vike_model::events::refusal_id_of(family.word()),
                })];
            }
        };
        match self.call(PATH_AMEND_ALGOS, "POST", &params) {
            Ok(data) => vec![Event::OrderModified(OrderModified {
                order_client_id: order_client_id.clone(),
                venue_order_id: first_row_id(&data, "algoId").map(Into::into),
                new_qty,
                new_price,
                new_trigger_price,
                // `amend-algos` states no time in its reply (unlike `amend-order`): the receipt time
                timestamp: amend_answer_time(None),
            })],
            // the resting order keeps its terms; the refusal must not vanish (non-terminal advisory)
            Err(exc) => vec![modify_failed(order_client_id, &exc, None)],
        }
    }

    /// The `amend-algos` body of [`Self::amend_algo`], pure, or why the amend cannot be written and its id:
    /// an amend the venue cannot make is `VenueCapability`, terms out of range `NotSent`.
    fn amend_algo_params(
        &self,
        order: &OrderRequest,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger_price: Option<f64>,
    ) -> Result<Vec<(&'static str, Value)>, (String, OrderRefusalId)> {
        use vike_model::OrderRefusalId::{NotSent, VenueCapability};
        let kind = &order.order_type;
        let (trigger_key, ord_key) = match kind {
            OrderType::Stop | OrderType::StopLimit => ("newSlTriggerPx", "newSlOrdPx"),
            OrderType::TakeProfit => ("newTpTriggerPx", "newTpOrdPx"),
            OrderType::Trailing | OrderType::TrailingLimit => {
                let why = format!(
                    "okx cannot amend a {kind} order in place (amend-algos excludes move_order_stop); \
                     cancel it and place a new one (order {})",
                    order.order_client_id
                );
                return Err((why, VenueCapability));
            }
            other => {
                return Err((
                    format!("okx's amend-algos amends trigger orders; a {other} order is not one"),
                    VenueCapability,
                ));
            }
        };
        let mut params: Vec<(&'static str, Value)> =
            vec![("instId", json!(self.symbol)), ("algoClOrdId", json!(order.order_client_id))];
        if let Some(q) = new_qty {
            params.push((
                "newSz",
                json!(format_to_step_f(self.to_contracts(q), self.properties.step_size)),
            ));
        }
        let (limit, trigger) = vike_model::modified_prices(kind, new_price, new_trigger_price);
        if limit.is_none() && trigger.is_none() {
            return Ok(params);
        }
        if *kind == OrderType::TakeProfit && order.price.is_none() && limit.is_some() {
            let why = format!(
                "okx: this take_profit is market-if-touched (no limit price), so it has no limit to move; \
                 cancel it and place a limit-if-touched (order {})",
                order.order_client_id
            );
            return Err((why, VenueCapability));
        }
        let trigger = trigger.or(order.trigger_price);
        let limit = if *kind == OrderType::Stop { None } else { limit.or(order.price) };
        if let Some(why) =
            vike_model::order_terms_refusal(kind, limit, trigger, None, None, None, None)
        {
            return Err((format!("okx: the amended order cannot be written: {why}"), NotSent));
        }
        let tick = self.properties.tick_size;
        params.push((trigger_key, json!(format_to_step_f(trigger.unwrap_or(0.0), tick))));
        params.push((
            ord_key,
            match limit {
                Some(px) => json!(format_to_step_f(px, tick)),
                None => json!("-1"),
            },
        ));
        Ok(params)
    }

    /// `GET /api/v5/trade/order-algo` by `algoClOrdId`: `Ok(Some((algoId, row)))` = the venue knows the
    /// stop (live or finished), `Ok(None)` = it does not, `Err` = the query failed, or a row that names no
    /// stop (says nothing about the order).
    pub(super) fn lookup_algo(
        &self,
        order_client_id: &str,
    ) -> Result<Option<(String, Value)>, VenueApiError> {
        let Some(row) = self.algo_row(order_client_id)? else { return Ok(None) };
        let id = row_id(&row, "algoId").ok_or_else(|| VenueApiError {
            code: E_TIMEOUT_AMBIGUOUS,
            msg: format!("GET {PATH_ORDER_ALGO} answered 2xx with no algoId"),
        })?;
        Ok(Some((id, row)))
    }

    /// Audit A3 resync of the ALGO socket: the newest `limit` stops of `state` (`canceled` or
    /// `order_failed`; the venue requires one) from `GET /api/v5/trade/orders-algo-history`, on the
    /// short-timeout requery transport, as the unwrapped `data` list. `ordType` and `state` are both
    /// required by the venue (`ordType=conditional` is the only kind this adapter places).
    pub fn get_algo_history(&self, state: &str, limit: u32) -> Result<Value, VenueApiError> {
        self.get_algo_history_for(&self.symbol, state, limit)
    }

    /// [`Self::get_algo_history`] for ANOTHER instrument of the account (an admitted one: the ALGO socket's
    /// resync replays the stops that ended on each instrument the engine trades). The request is the mounted
    /// instrument's, byte for byte, with that `instId`.
    pub fn get_algo_history_for(
        &self,
        instrument: &str,
        state: &str,
        limit: u32,
    ) -> Result<Value, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![
            ("ordType", json!("conditional")),
            ("state", json!(state)),
            ("instId", json!(instrument)),
            ("limit", json!(limit.to_string())),
        ];
        self.transport
            .signed_requery(&self.base_url, PATH_ORDERS_ALGO_HISTORY, "GET", &params, &self.signer)
            .and_then(unwrap_okx)
    }

    /// The first row of `GET /api/v5/trade/order-algo` for `order_client_id` (`algoClOrdId`), or `None` when the
    /// venue answers "unknown" ([`algo_unknown`], its own word and the only thing that says so: an unknown
    /// id answers `51603`, measured). A `0` with no row says nothing, so it is an `Err`.
    fn algo_row(&self, order_client_id: &str) -> Result<Option<Value>, VenueApiError> {
        let params: Vec<(&str, Value)> = vec![("algoClOrdId", json!(order_client_id))];
        let response = self.transport.signed_requery(
            &self.base_url,
            PATH_ORDER_ALGO,
            "GET",
            &params,
            &self.signer,
        );
        match response.and_then(unwrap_okx) {
            Ok(data) => {
                data.as_array().and_then(|d| d.first()).cloned().map(Some).ok_or_else(|| {
                    VenueApiError {
                        code: E_TIMEOUT_AMBIGUOUS,
                        msg: format!("GET {PATH_ORDER_ALGO} answered 2xx with no row"),
                    }
                })
            }
            Err(exc) if algo_unknown(exc.code) => Ok(None),
            Err(exc) => Err(exc),
        }
    }

    /// A stop POST whose outcome is unknown, asked of the algo book ([`Self::lookup_algo`]): found is an
    /// `OrderAccepted` carrying the `algoId`. "No such stop" and a FAILED query are NOTHING, as
    /// `vike_bridge_core::resolve_ambiguous_submit` says for an ordinary order: one read just after the
    /// POST proves no reject. The order stays pre-ack, so the stuck-order watchdog's confirm asks again
    /// with the send finished, and its backstop decides.
    fn resolve_stop_submit(&self, request: &OrderRequest) -> Vec<Event> {
        let order_client_id = &request.order_client_id;
        let id = self.lookup_algo(order_client_id).map(|found| found.map(|(id, _row)| id));
        vike_bridge_core::resolve_ambiguous_submit(order_client_id, request.timestamp, id)
            .into_iter()
            .collect()
    }

    /// The stop-algo body, pure (the algo twin of [`Self::build_order_params`], extracted so the
    /// wire shape is pinnable without a transport). Base params byte-identical to the historical
    /// inline vec; a requested `trigger_by` appends `slTriggerPxType` off okx's
    /// `vike_bridge_core::trigger::venue_trigger_by` row (`last`/`mark`/`index` — all three
    /// express, nothing to deny). `None` emits no param — the venue default (`last`) rules,
    /// byte-identical to before the field existed. A `stop_limit`'s `slOrdPx` is its limit `price`
    /// on the tick grid; a stop keeps `-1` (byte-identical). A `take_profit` writes the same body on
    /// the take-profit side instead: `tpTriggerPx`, `tpOrdPx` (its limit on the tick grid, or `-1` when
    /// it has none: market-if-touched) and `tpTriggerPxType`, in the stop-loss words' places.
    pub(super) fn build_stop_algo_params(
        &self,
        request: &OrderRequest,
        td_mode: &str,
    ) -> Vec<(&'static str, Value)> {
        let (trigger_key, ord_key, type_key) = if request.order_type == OrderType::TakeProfit {
            ("tpTriggerPx", "tpOrdPx", "tpTriggerPxType")
        } else {
            ("slTriggerPx", "slOrdPx", "slTriggerPxType")
        };
        let limit = match request.order_type {
            OrderType::StopLimit => Some(request.price.unwrap_or(0.0)),
            OrderType::TakeProfit => request.price,
            _ => None,
        };
        let mut params = self.algo_head(request, td_mode, "conditional");
        params.push((
            trigger_key,
            json!(format_to_step_f(
                request.trigger_price.unwrap_or(0.0),
                self.properties.tick_size
            )),
        ));
        // -1 = fire as a market order on trigger; a limit rests a limit order there
        params.push((
            ord_key,
            match limit {
                Some(px) => json!(format_to_step_f(px, self.properties.tick_size)),
                None => json!("-1"),
            },
        ));
        if let Some(wire) = request
            .trigger_by
            .and_then(|s| vike_bridge_core::trigger::venue_trigger_by(VENUE, s).wire())
        {
            params.push((type_key, json!(wire)));
        }
        params
    }

    /// The words every algo body of this adapter opens with, in the order the stop's body always sent
    /// them: `instId`, `tdMode`, `side`, `ordType`, `sz` (contracts on the lot grid), `posSide`,
    /// `reduceOnly`, `algoClOrdId`.
    fn algo_head(
        &self,
        request: &OrderRequest,
        td_mode: &str,
        ord_type: &'static str,
    ) -> Vec<(&'static str, Value)> {
        vec![
            ("instId", json!(self.symbol)),
            ("tdMode", json!(td_mode)),
            ("side", json!(if request.side > 0 { "buy" } else { "sell" })),
            ("ordType", json!(ord_type)),
            (
                "sz",
                json!(format_to_step_f(self.to_contracts(request.qty), self.properties.step_size)),
            ),
            ("posSide", json!("net")),
            ("reduceOnly", json!(request.reduce_only)),
            ("algoClOrdId", json!(request.order_client_id)),
        ]
    }

    /// The trailing-order body, pure: the stop's head with `ordType=move_order_stop`, then ONE distance
    /// word, `callbackSpread` (the `trail`, a price distance, down to the tick grid) or, for a
    /// `trail_percent`, `callbackRatio` ([`callback_ratio`]: the percent / 100, exact and normalized),
    /// and, when the request states an activation (a trailing kind's `trigger_price`), `activePx` on the
    /// tick grid. No trigger-price type, no stop-loss or take-profit word. A price trail's body is byte
    /// for byte what it was before the percent existed.
    pub(super) fn build_trailing_algo_params(
        &self,
        request: &OrderRequest,
        td_mode: &str,
    ) -> Vec<(&'static str, Value)> {
        let tick = self.properties.tick_size;
        let mut params = self.algo_head(request, td_mode, "move_order_stop");
        params.push(match request.trail_percent {
            Some(p) => ("callbackRatio", json!(callback_ratio(p))),
            None => ("callbackSpread", json!(format_to_step_f(request.trail.unwrap_or(0.0), tick))),
        });
        if let Some(activation) = request.trigger_price {
            params.push(("activePx", json!(format_to_step_f(activation, tick))));
        }
        params
    }

    /// Native conditional STOP (a bracket stop-loss leg) via /api/v5/trade/order-algo — OKX places
    /// stops as ALGO orders, not on the regular /order endpoint. `ordType:"conditional"` with an
    /// `slTriggerPx` and a market `slOrdPx` ("-1"), or the limit price for a `stop_limit`;
    /// `algoClOrdId` carries the order client id so the accept maps back. Returns
    /// [Submitted, Accepted(algoId)|Rejected]. RUST-NATIVE, no Python twin.
    ///
    /// A `stop_limit` or a `take_profit` is refused by name before the wire when it cannot be written as
    /// one ([`trigger_order_refusal`]): no finite positive trigger, or limit on a stop-limit
    /// (`vike_model::order_terms_refusal`; `slOrdPx -1` would fire it as a market order), or, with a
    /// limit, a time in force other than GTC (the order the trigger places is a plain limit, so an IOC,
    /// FOK, post-only or dated one would rest as something else).
    ///
    /// A `trailing` is the same POST as `ordType=move_order_stop` ([`Self::build_trailing_algo_params`]),
    /// refused first when it cannot be written ([`trailing_order_refusal`]); a `trailing_limit` is
    /// always refused by name (okx's trailing order fires a market order). Its answer, accept and
    /// timeout path are a stop's.
    pub fn submit_stop_algo(&self, request: &OrderRequest) -> Vec<Event> {
        let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
            order_client_id: request.order_client_id.clone(),
            timestamp: request.timestamp,
        })];
        // an id of no known form is refused in words first, never signed as a SWAP stop (OKX-1)
        let class = InstrumentType::of_inst_id_or_refusal(&self.symbol)
            .map_err(|refusal| (refusal.to_string(), OrderRefusalId::NotSent))
            .and_then(|class| {
                let refusal = match request.order_type {
                    OrderType::StopLimit | OrderType::TakeProfit => trigger_order_refusal(request),
                    OrderType::Trailing | OrderType::TrailingLimit => {
                        trailing_order_refusal(request, class, self.properties.tick_size)
                    }
                    _ => None,
                };
                refusal.map_or(Ok(class), Err)
            });
        let class = match class {
            Ok(class) => class,
            Err((reason, family)) => {
                events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: request.timestamp,
                    refusal_id: vike_model::events::refusal_id_of(family.word()),
                }));
                return events;
            }
        };
        // Same margin-mode gate as the regular order path (unset = the historical "cross"); a SPOT pair's
        // stop is written like its orders (`tdMode` by the account level, no `posSide` or `reduceOnly`).
        let build = |mode: SpotMode| -> Result<Vec<(&'static str, Value)>, String> {
            let td_mode = class::td_mode(class, request.margin_mode, mode)?;
            let mut params = if request.order_type == OrderType::Trailing {
                self.build_trailing_algo_params(request, td_mode)
            } else {
                self.build_stop_algo_params(request, td_mode)
            };
            if class == InstrumentType::Spot {
                class::shape_spot_stop(&mut params);
                // the doc's `tgtCcy` row: a SPOT "Market buy conditional order" counts `sz` in the QUOTE
                // ("Default is quote_ccy for buy"), so a BUY that fires a market order (a stop, a
                // market-if-touched) states the base
                let fires_market = match request.order_type {
                    OrderType::Stop => true,
                    OrderType::TakeProfit => request.price.is_none(),
                    _ => false,
                };
                if fires_market && request.side > 0 {
                    params.push(("tgtCcy", json!("base_ccy")));
                }
            }
            Ok(params)
        };
        let params = match build(SpotMode::Cross) {
            Ok(p) => p,
            Err(reason) => {
                events.push(Event::OrderRejected(OrderRejected {
                    cause: None,
                    order_client_id: request.order_client_id.clone(),
                    reason: reason.into(),
                    timestamp: request.timestamp,
                    // a margin mode this bridge does not write (`class::td_mode`)
                    refusal_id: vike_model::events::refusal_id_of(
                        OrderRefusalId::VenueCapability.word(),
                    ),
                }));
                return events;
            }
        };
        match self.post_order_params(PATH_ORDER_ALGO, request, params, build) {
            Ok(data) => match first_row_id(&data, "algoId") {
                Some(id) => events.push(Event::OrderAccepted(OrderAccepted {
                    order_client_id: request.order_client_id.clone(),
                    venue_order_id: Some(id.into()),
                    timestamp: request.timestamp,
                })),
                // a 2xx that names no stop: it may rest, so ask, as for a timeout
                None => events.extend(self.resolve_stop_submit(request)),
            },
            // The stop may have landed: its own book says (`order-algo` by `algoClOrdId`). An id-less
            // accept here read ACCEPTED for a stop that never landed, was never swept, and the
            // position looked protected.
            Err(exc) if TAXONOMY.submit_outcome_unknown(exc.code) => {
                events.extend(self.resolve_stop_submit(request));
            }
            Err(exc) => events.push(Event::OrderRejected(OrderRejected {
                cause: None,
                order_client_id: request.order_client_id.clone(),
                reason: crate::account::modes::explain_refusal(&exc.msg, self.transport.network())
                    .into(),
                timestamp: request.timestamp,
                refusal_id: vike_model::events::refusal_id_of(exc.refusal_id_word()),
            })),
        }
        events
    }
}
