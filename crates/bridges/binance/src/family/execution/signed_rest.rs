//! **The signed REST client of a family venue: the bodies binance and aster share** (R7a; the owner's
//! ruling of 2026-11-13, "both parts, behaviour as now").
//!
//! `VenueRestBinanceSpot`/`VenueRestBinancePerp` here and `VenueRestAsterSpot`/`VenueRestAsterPerp` in vike-aster sent the
//! SAME requests and read the SAME answers, but for three facts: each lane's endpoint PATHS (binance's
//! `/fapi/v1` against aster's `/fapi/v3`, binance spot's `myTrades` against aster's `userTrades`), the
//! broker link id binance stamps on every order client id it sends (aster sends the bare id), and what
//! each bridge does AROUND the wire. Each client keeps its own struct (its fields are its venue's, and
//! both venues' tests build them by name) and implements [`FamilySignedRest`]: its parts, its
//! [`FamilyRestPaths`] and its venue's facts. The bodies live here once: [`submit_order`],
//! [`send_batch_run`], [`resolve_ambiguous_submit`], [`confirm_when`], [`cancel_by_order_client_id`],
//! [`get_all_orders_for`], [`get_trades_for`], [`set_leverage`] and [`modify_perp`].
//!
//! **What each venue keeps, through the trait and never in a body here:**
//! - the bridge FLOORS, refused before the wire ([`FamilySignedRest::floor`]): binance's TIF, trigger
//!   and kind gates on both lanes, aster perp's trigger gate, NOTHING on aster spot;
//! - a refusal's typed cause and words ([`FamilySignedRest::refusal`]): binance's `post_only_cause`
//!   and, on its perpetual, `crate::account::modes`'s `reject_reason`; aster's venue words with no cause;
//! - the refusal's log line ([`FamilySignedRest::log_refused`]): a `tracing` target is a literal baked
//!   into a `static`, so it cannot be read off a generic type, and each client writes its own line
//!   under its own module's target, as it always did;
//! - a conditional order sent elsewhere ([`FamilySignedRest::route`],
//!   [`FamilySignedRest::lookup_by_order_client_id`]): binance's perpetual Algo Service, decision 0186, which
//!   stays in `crate::execution::rest::algo`;
//! - every verb only one venue has (binance spot's cancel-replace, binance perp's algo-aware cancel,
//!   aster perp's position-mode read and builder grant), on that venue's own client.
//!
//! The defaults of the trait's hooks are "nothing more than the wire": no floor, no route, the venue's
//! words with no cause, no stamp. Every byte a client sent before this module existed is what it sends
//! now: `crates/bridges/aster/tests/offline/family_wire_pins.rs` and
//! `crates/bridges/binance/tests/offline/family_wire_pins.rs` were written on the code before the move.

use serde_json::Value;
use vike_bridge_core::execution_client::resync::ReplayRows;
use vike_bridge_core::rest::OrderCancelAck;
use vike_bridge_core::signer::Signer;
use vike_bridge_core::transport::{RestTransport, VenueApiError};
use vike_model::events::{Event, OrderAccepted, OrderRejected, OrderSubmitted, RejectCause};
use vike_model::{OrderRequest, SymbolProperties};

use super::order_map::{binance_broker_order_client_id, json_id};
use super::rest_reads::{self, FoundOrder, PerpModifySpec};

/// The endpoint paths one lane of a family venue differs by. Each client's constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FamilyRestPaths {
    /// The single-order endpoint: the submit, the by-order-client-id status query, the cancel and the perp amend.
    pub order: &'static str,
    /// The resync's order-history read (`allOrders`).
    pub all_orders: &'static str,
    /// The resync's fill-history read: binance spot's `myTrades`, aster spot's `userTrades`, either
    /// perpetual's `userTrades`.
    pub trades: &'static str,
    /// The perpetual's leverage write; empty on a spot lane, which has none.
    pub leverage: &'static str,
    /// The perpetual's native batch endpoint; empty on a spot lane, which has none.
    pub batch: &'static str,
}

/// **One signed REST client of a family venue** (a spot or a perp lane of binance or aster): its parts,
/// its paths, and the facts its venue adds around the shared bodies of this module (see the module doc).
pub trait FamilySignedRest {
    /// The venue word a cancel's answer is told under (`OrderCancelAck::from_venue_answer`).
    const VENUE: &'static str;
    /// This lane's endpoints.
    const PATHS: FamilyRestPaths;

    fn transport(&self) -> &dyn RestTransport;
    fn signer(&self) -> &dyn Signer;
    fn base_url(&self) -> &str;
    /// The EXCHANGE symbol the client was built for (no `.P`).
    fn symbol(&self) -> &str;

    /// The broker link id stamped on every order client id the venue is sent or asked about
    /// ([`binance_broker_order_client_id`]); `None` sends the bare id. Binance's attribution; aster stamps none.
    fn link_id(&self) -> Option<&str> {
        None
    }

    /// The order's params, in the wire's order (each client's own `build_order_params`).
    fn order_params(&self, request: &OrderRequest) -> Vec<(&'static str, String)>;

    /// The bridge's own refusal of an order it does not send, BEFORE the wire: the terminal
    /// `OrderRejected` pushed after the `OrderSubmitted`, or `None` to send it.
    fn floor(&self, _request: &OrderRequest) -> Option<Event> {
        None
    }

    /// An order this lane sends ELSEWHERE than its order endpoint (binance's perpetual stop, to the Algo
    /// Service): the events after the `OrderSubmitted`, or `None` for the order endpoint.
    fn route(&self, _request: &OrderRequest) -> Option<Vec<Event>> {
        None
    }

    /// A refusal's typed cause and its words, from the venue's `code` and `msg`: the venue's words with
    /// no cause unless the venue's client knows more.
    fn refusal(
        &self,
        _request: &OrderRequest,
        _code: i64,
        msg: &str,
    ) -> (Option<RejectCause>, String) {
        (None, msg.to_string())
    }

    /// The line a definite submit refusal writes, under the client's own `tracing` target.
    fn log_refused(&self, exc: &VenueApiError);

    /// The by-order-client-id status query behind BOTH the ambiguous-submit arm and the active confirm: ONE body, so
    /// they cannot disagree. `GET <order>` on the requery transport, by the id as the venue stored it.
    fn lookup_by_order_client_id(
        &self,
        order_client_id: &str,
    ) -> Result<Option<FoundOrder>, VenueApiError> {
        let orig = binance_broker_order_client_id(self.link_id(), order_client_id);
        rest_reads::query_order_orderid(
            self.transport(),
            self.signer(),
            self.base_url(),
            Self::PATHS.order,
            self.symbol(),
            &orig,
        )
    }

    /// The perp replay's THIRD read of the EXCHANGE `symbol`, after its orders and its trades: the
    /// venue's conditional orders (binance's `allAlgoOrders`, decision 0186). `None`: the venue keeps
    /// none apart.
    fn conditional_history_rows(&self, _symbol: &str, _rows: ReplayRows<'_>) -> Option<Value> {
        None
    }

    /// The events of [`Self::conditional_history_rows`]'s answer, after the orders' and trades'.
    fn conditional_history_events(&self, _rows: &Value) -> Vec<Event> {
        Vec::new()
    }
}

/// **The submit**: `OrderSubmitted`, then the client's floor ([`FamilySignedRest::floor`]) and route
/// ([`FamilySignedRest::route`]), then `POST <order>` of its params. The ack's `orderId` accepts it; a
/// 2xx that names no order, or a code the venue documents as status-UNKNOWN, asks the venue by the
/// order client id ([`resolve_ambiguous_submit`]: the order may rest, and a false terminal would strand it); any
/// other failure is the terminal `OrderRejected` of [`FamilySignedRest::refusal`], after one
/// [`FamilySignedRest::log_refused`] line. The events go into the core ingest, never applied locally.
pub fn submit_order<C: FamilySignedRest>(client: &C, request: &OrderRequest) -> Vec<Event> {
    let mut events = vec![Event::OrderSubmitted(OrderSubmitted {
        order_client_id: request.order_client_id.clone(),
        timestamp: request.timestamp,
    })];
    if let Some(reject) = client.floor(request) {
        events.push(reject);
        return events;
    }
    if let Some(routed) = client.route(request) {
        events.extend(routed);
        return events;
    }
    let params = client.order_params(request);
    match client.transport().signed(
        client.base_url(),
        C::PATHS.order,
        "POST",
        &params,
        client.signer(),
    ) {
        Ok(response) => match response.get("orderId").map(json_id).filter(|id| !id.is_empty()) {
            Some(id) => events.push(Event::OrderAccepted(OrderAccepted {
                order_client_id: request.order_client_id.clone(),
                venue_order_id: Some(id.into()),
                timestamp: request.timestamp,
            })),
            // a 2xx that names no order: it may rest, so ask, as for a timeout
            None => events.extend(resolve_ambiguous_submit(client, request)),
        },
        // audit T1: an ambiguous timeout may have been accepted at the venue — re-query instead
        // of emitting a false terminal reject (which would leave a phantom position).
        Err(exc) if crate::transport::error_codes::submit_outcome_unknown(exc.code) => {
            events.extend(resolve_ambiguous_submit(client, request));
        }
        Err(exc) => {
            client.log_refused(&exc);
            let (cause, reason) = client.refusal(request, exc.code, &exc.msg);
            events.push(Event::OrderRejected(OrderRejected {
                cause,
                order_client_id: request.order_client_id.clone(),
                reason: reason.into(),
                timestamp: request.timestamp,
            }))
        }
    }
    events
}

/// **One consecutive run of orders through the native batch** (`POST <batch>`, `batchOrders` a
/// JSON-array string param), chunked at `max`; the per-order events are appended to `events`. Each
/// element of the venue's answer is matched to its order by INDEX: an `orderId` accepts it, a `code`
/// refuses it in its `msg` (read through [`FamilySignedRest::refusal`]; "batch order reject" when the
/// venue gave none) unless the code itself says status-unknown, and an element with neither — or no
/// element — asks the venue ([`resolve_ambiguous_submit`]). A whole-batch status-unknown asks about every
/// order of the chunk; any other whole-batch failure refuses each in its words, with no cause.
pub fn send_batch_run<C: FamilySignedRest>(
    client: &C,
    events: &mut Vec<Event>,
    run: &[&OrderRequest],
    max: usize,
) {
    for chunk in run.chunks(max) {
        let orders: Vec<Value> = chunk
            .iter()
            .map(|r| {
                Value::Object(
                    client
                        .order_params(r)
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), Value::String(v)))
                        .collect(),
                )
            })
            .collect();
        let params: Vec<(&str, String)> =
            vec![("batchOrders", serde_json::to_string(&orders).expect("array"))];
        match client.transport().signed(
            client.base_url(),
            C::PATHS.batch,
            "POST",
            &params,
            client.signer(),
        ) {
            Ok(response) => {
                let list = response.as_array().cloned().unwrap_or_default();
                for (i, r) in chunk.iter().enumerate() {
                    let element = list.get(i);
                    let id = element
                        .and_then(|o| o.get("orderId"))
                        .map(json_id)
                        .filter(|id| !id.is_empty());
                    let code = element.and_then(|o| o.get("code")).and_then(|c| c.as_i64());
                    match (element, id, code) {
                        (_, Some(id), _) => {
                            events.push(Event::OrderAccepted(OrderAccepted {
                                order_client_id: r.order_client_id.clone(),
                                venue_order_id: Some(id.into()),
                                timestamp: r.timestamp,
                            }));
                        }
                        // a refusal, unless the venue itself calls it status-unknown
                        (Some(o), None, Some(code))
                            if !crate::transport::error_codes::submit_outcome_unknown(code) =>
                        {
                            let msg = o
                                .get("msg")
                                .and_then(|m| m.as_str())
                                .unwrap_or("batch order reject");
                            let (cause, reason) = client.refusal(r, code, msg);
                            events.push(Event::OrderRejected(OrderRejected {
                                cause,
                                order_client_id: r.order_client_id.clone(),
                                reason: reason.into(),
                                timestamp: r.timestamp,
                            }));
                        }
                        // a 2xx that says nothing about this order (no element, or one with neither
                        // an id nor a code), or a status-unknown code: it may rest, so ask
                        _ => events.extend(resolve_ambiguous_submit(client, r)),
                    }
                }
            }
            // audit T1: an ambiguous whole-batch timeout may have been accepted — re-query each
            // order's status instead of rejecting the whole chunk (which could strand phantoms).
            Err(exc) if crate::transport::error_codes::submit_outcome_unknown(exc.code) => {
                for r in chunk {
                    events.extend(resolve_ambiguous_submit(client, r));
                }
            }
            Err(exc) => {
                for r in chunk {
                    // the words only: a whole-batch failure names no order's cause
                    let (_, reason) = client.refusal(r, exc.code, &exc.msg);
                    events.push(Event::OrderRejected(OrderRejected {
                        cause: None,
                        order_client_id: r.order_client_id.clone(),
                        reason: reason.into(),
                        timestamp: r.timestamp,
                    }));
                }
            }
        }
    }
}

/// Resolve an ambiguous (timed-out) submit into a NON-phantom event (audit T1): the by-order-client-id query
/// ([`FamilySignedRest::lookup_by_order_client_id`]) read by `super::confirm::resolve_submit` — found -> its
/// status's events, absent or a failed query -> nothing (the watchdog confirm decides).
pub fn resolve_ambiguous_submit<C: FamilySignedRest>(
    client: &C,
    request: &OrderRequest,
) -> Vec<Event> {
    super::confirm::resolve_submit(
        &request.order_client_id,
        request.timestamp,
        client.lookup_by_order_client_id(&request.order_client_id),
    )
}

/// **The active confirm** of one order by its client id, the body of each client's
/// `ConfirmByOrderClientId::confirm_when`: the by-order-client-id query read by `vike_bridge_core::confirm::resolve_confirm`
/// with the family's status words (`super::confirm::found_events`).
pub fn confirm_when<C: FamilySignedRest>(
    client: &C,
    order_client_id: &str,
    ts: i64,
    absent_rejects: bool,
) -> Vec<Event> {
    vike_bridge_core::confirm::resolve_confirm(
        order_client_id,
        ts,
        client.lookup_by_order_client_id(order_client_id),
        absent_rejects,
        |found| super::confirm::found_events(order_client_id, ts, &found),
    )
}

/// **Cancel by order client id**, `DELETE <order>` by the id as the venue stored it. "Unknown order"
/// (`-2011` with those words, `crate::transport::error_codes::cancel_says_order_unknown`) is "already gone", not a
/// failure (the R6 gate's "unknown ≠ rejection" rule); everything else propagates.
pub fn cancel_by_order_client_id<C: FamilySignedRest>(
    client: &C,
    order_client_id: &str,
) -> Result<OrderCancelAck, VenueApiError> {
    let orig = binance_broker_order_client_id(client.link_id(), order_client_id);
    let answer = rest_reads::delete_by_order_client_id(
        client.transport(),
        client.signer(),
        client.base_url(),
        C::PATHS.order,
        client.symbol(),
        &orig,
    );
    OrderCancelAck::from_venue_answer(
        C::VENUE,
        order_client_id,
        answer,
        crate::transport::error_codes::cancel_says_order_unknown,
    )
}

/// Audit A3 resync: the recent order states of `symbol` (an EXCHANGE symbol, the mounted one or an
/// admitted one) for the post-reconnect history replay: `GET <all_orders>` on the requery transport,
/// [`rest_reads::get_all_orders`].
pub fn get_all_orders_for<C: FamilySignedRest>(
    client: &C,
    symbol: &str,
    limit: u32,
) -> Result<Value, VenueApiError> {
    rest_reads::get_all_orders(
        client.transport(),
        client.signer(),
        client.base_url(),
        C::PATHS.all_orders,
        symbol,
        limit,
    )
}

/// Audit A3 resync: the recent fills of `symbol` (an EXCHANGE symbol), `GET <trades>` on the requery
/// transport. ⚠ On aster spot that path is `userTrades`: its `myTrades` is a plain 404, which the
/// replay's best-effort read used to swallow into an empty replay.
pub fn get_trades_for<C: FamilySignedRest>(
    client: &C,
    symbol: &str,
    limit: u32,
) -> Result<Value, VenueApiError> {
    let params: Vec<(&str, String)> =
        vec![("symbol", symbol.to_string()), ("limit", limit.to_string())];
    client.transport().signed_requery(
        client.base_url(),
        C::PATHS.trades,
        "GET",
        &params,
        client.signer(),
    )
}

/// `POST <leverage> {symbol, leverage}`, the figure cast to a whole number. The family's
/// change-leverage is idempotent (HTTP 200 even at the target), so no benign error is swallowed.
pub fn set_leverage<C: FamilySignedRest>(client: &C, leverage: f64) -> Result<(), VenueApiError> {
    let params: Vec<(&str, String)> =
        vec![("symbol", client.symbol().to_string()), ("leverage", format!("{}", leverage as i64))];
    client
        .transport()
        .signed(client.base_url(), C::PATHS.leverage, "POST", &params, client.signer())
        .map(|_| ())
}

/// The native perp amend ([`rest_reads::modify_perp_order`]) by the id as the venue stored it.
pub fn modify_perp<C: FamilySignedRest>(
    client: &C,
    spec: &PerpModifySpec,
    properties: &SymbolProperties,
    order: &OrderRequest,
    new_qty: Option<f64>,
    new_price: Option<f64>,
) -> Vec<Event> {
    let orig = binance_broker_order_client_id(client.link_id(), &order.order_client_id);
    rest_reads::modify_perp_order(
        client.transport(),
        client.signer(),
        client.base_url(),
        spec,
        client.symbol(),
        properties,
        &orig,
        order,
        new_qty,
        new_price,
    )
}
