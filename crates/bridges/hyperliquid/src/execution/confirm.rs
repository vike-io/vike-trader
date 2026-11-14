//! The ACTIVE confirm (`ExecutionClient::confirm`), in the one form of `vike_bridge_core::confirm`: the
//! venue's `info` `orderStatus` by `cloid`, on a client of its own, off the exec thread and off the fold.
//!
//! An order whose `/exchange` answer was lost or says nothing (a post-send timeout, a reply with no
//! `statuses`, a short list, a status word this crate does not know) gets no event from the submit, so
//! only a WS `orderUpdates` row can move it. If the WS missed that row in the same outage, the stuck-order
//! watchdog rejected it about a minute later while it RESTED. This is what the watchdog's confirm asks:
//!
//! * **Found** is read by the order's STATE ([`found_events`]): open is an `OrderAccepted` carrying the
//!   oid; closed is that `OrderAccepted` and its terminal, through the WS row's own mapper; refused by the
//!   venue is the terminal `OrderRejected` alone.
//! * **Absent** (`unknownOid`) is the terminal `OrderRejected` ONLY once the exec thread finished the
//!   order's POST ([`DispatchLedger`], marked by `ExecutionState::submit`, read before the query). Before that
//!   the POST may still be in flight and it says nothing; the backstop decides.
//! * **A failed read, or an answer of a shape not known here**, says nothing.
//!
//! The cloid is a pure function of the order client id, so the lookup needs no state a gatewayhub restart loses: the
//! same confirm answers `ResolveOrders` for an order the previous process placed. The restart does empty
//! the ledger, so for such an order absent says nothing. The worker thread is the shared actor's
//! (`vike_bridge_core::exec_actor::ExecutionClientActor`'s `confirm`, wired in [`super::client::spawn_with_market_slippage`]).
//!
//! ⚠ What it does NOT do: teach the exec thread the oid. A modify of an order accepted this way is still
//! refused in words (`no resting venue order to modify`); a cancel goes by cloid and works.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use vike_bridge_core::confirm::{
    ConfirmByOrderClientId, DispatchLedger, confirm_closure, found_by_state, resolve_confirm,
};
use vike_bridge_core::exec_actor::ConfirmFn;
use vike_bridge_core::ratelimit::RateGate;
use vike_bridge_core::rest::{VenueRest, refused};
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};
use vike_model::OrderRequest;
use vike_model::events::Event;

use crate::execution::event_mapper::{cloid_from_order_client_id, map_order_update};
use crate::execution::user_data::{order_client_id_mut, remap_symbol};
use crate::instrument::properties::SharedInstruments;
use crate::instrument::symbology::Symbology;
use crate::transport::HyperliquidTransport;
use crate::transport::consts::VENUE;
use vike_model::VenueNetwork;

use super::order_wire::order_status_body;

/// How long one `orderStatus` read may take: the short bound the other venues' by-order-client-id re-query rides.
const ORDER_STATUS_TIMEOUT: Duration = Duration::from_secs(5);

/// The order row in an `orderStatus` reply: `Ok(Some(row))` the venue holds it, `Ok(None)` the venue's
/// `unknownOid`, `Err` any other shape (says nothing about the order).
fn read_status(response: Value) -> Result<Option<Value>, VenueApiError> {
    let row = match response.get("status").and_then(Value::as_str) {
        Some("unknownOid") => return Ok(None),
        Some("order") => response.get("order"),
        _ => None,
    };
    row.filter(|row| {
        row.get("status").is_some_and(Value::is_string)
            && row.get("order").and_then(|o| o.get("oid")).is_some_and(Value::is_u64)
    })
    .cloned()
    .map(Some)
    .ok_or_else(|| VenueApiError {
        code: E_TIMEOUT_AMBIGUOUS,
        msg: format!("orderStatus answered a shape this crate does not know: {response}"),
    })
}

/// The events for an order the venue HAS, under `order_client_id`, by its status word (see the module doc). A word
/// [`map_order_update`] does not know (`replaced`, one the venue adds) proves the order exists and
/// nothing more: an `OrderAccepted` alone, the rest rides the WS and reconcile.
fn found_events(order_client_id: &str, ts: i64, row: &Value, symbology: &Symbology) -> Vec<Event> {
    let oid = row.get("order").and_then(|o| o.get("oid")).and_then(Value::as_u64);
    let oid = oid.map(|oid| oid.to_string());
    let Some(mut event) = map_order_update(row, VENUE) else {
        return found_by_state(order_client_id, ts, oid, Vec::new());
    };
    // The row names the order by its hash, which nothing can invert after a restart; the order client id is known.
    if let Some(slot) = order_client_id_mut(&mut event) {
        *slot = order_client_id.to_string();
    }
    if let Event::OrderFilled(wrap) = &mut event {
        wrap.fill.order_client_id = order_client_id.to_string();
    }
    remap_symbol(&mut event, symbology);
    found_by_state(order_client_id, ts, oid, vec![event])
}

/// One confirm's verdict over what the read answered. `absent_rejects`: the exec thread finished this
/// order client id's POST ([`DispatchLedger`], read before the query).
fn confirm_events(
    order_client_id: &str,
    ts: i64,
    read: Result<Value, VenueApiError>,
    absent_rejects: bool,
    symbology: &Symbology,
) -> Vec<Event> {
    resolve_confirm(order_client_id, ts, read.and_then(read_status), absent_rejects, |row| {
        found_events(order_client_id, ts, &row, symbology)
    })
}

/// The read behind the confirm: its own transport (a short timeout, the venue's one per-IP weight window
/// `ip_gate`), the MASTER address the read is scoped to, the universe a fill's coin is named through.
struct OrderStatusRead {
    transport: HyperliquidTransport,
    master: String,
    instruments: SharedInstruments,
}

/// `ConfirmByOrderClientId` is a `VenueRest`, and this client only reads: nothing calls these two, the order lane
/// is the exec thread's. They answer as a client that sends nothing.
impl VenueRest for OrderStatusRead {
    fn submit_order(&self, request: &OrderRequest) -> Vec<Event> {
        refused(request, "the hyperliquid confirm client sends no orders".to_string())
    }
    fn cancel_order(
        &self,
        _order_client_id: &str,
    ) -> Result<vike_bridge_core::rest::OrderCancelAck, VenueApiError> {
        Err(VenueApiError { code: 0, msg: "the hyperliquid confirm client sends nothing".into() })
    }
}

impl ConfirmByOrderClientId for OrderStatusRead {
    // None: the venue states no age limit on `orderStatus` and none has been measured (decision 0171's table).
    const LOOKUP_WINDOW: Option<Duration> = None;

    fn confirm_when(&self, order_client_id: &str, ts: i64, absent_rejects: bool) -> Vec<Event> {
        let cloid = cloid_from_order_client_id(order_client_id);
        let read = self.transport.info(&order_status_body(&self.master, &cloid));
        confirm_events(
            order_client_id,
            ts,
            read,
            absent_rejects,
            self.instruments.current().symbology(),
        )
    }
}

/// The `with_confirm` closure of one exec client: bounded in flight and backed off per order client id by the shared
/// [`confirm_closure`]. `ledger` is the one the exec thread marks after each POST (`ExecutionState::submit`).
pub(super) fn order_status_confirm(
    network: VenueNetwork,
    rest: &str,
    master: String,
    ip_gate: RateGate,
    instruments: SharedInstruments,
    ledger: Arc<DispatchLedger>,
) -> ConfirmFn {
    let agent = vike_bridge_core::http::blocking_agent_with_timeout(ORDER_STATUS_TIMEOUT);
    let transport = HyperliquidTransport::with_agent(network, rest, agent).with_rate_gate(ip_gate);
    confirm_closure(OrderStatusRead { transport, master, instruments }, ledger, |_| true)
}

#[path = "confirm_tests.rs"]
#[cfg(test)]
mod confirm_tests;
