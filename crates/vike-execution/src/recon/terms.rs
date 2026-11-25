//! The order-TERMS leg of a reconcile pass: a live local order whose venue row states another limit
//! price, trigger price or size (`ReconcileDivergence::OrderTermsDrift`). DELIBERATELY SEPARATE from
//! [`super::diff`], whose signature and golden fixtures stay untouched, like [`super::diff_balance`]:
//! the caller pushes what this returns into the same `divergences` vec before `resolve`, and only when
//! the pass's policy compares terms (`ReconcilePolicy::order_terms`).
//!
//! What is compared, per venue row whose order client id names a local order:
//!
//! - only an order that can take an `OrderModified` on both sides: the local one is modifiable
//!   (`OrderStatus::is_modifiable`, the FSM's own test for the adoption event) and the venue row is
//!   not terminal (a terminal row is `MissingTerminal`'s);
//! - not while the order's amend is SETTLING (`settling`): an amend still in flight, or answered within
//!   the last pass, races the venue's row (the row may be read before the venue took the amend, or
//!   before its answer folded here), so a difference then is no drift;
//! - a PRICE only where the venue STATES it (`None` is "not stated", never zero), the local order rests
//!   on it, and an `OrderModified` lands it (`vike_model::modified_prices` is the authority, so a confirm
//!   adopts exactly what was compared). That leaves a TRAILING kind's prices alone: its `trigger_price`
//!   is the ACTIVATION price, its stop moves at the venue, and no amend moves either;
//! - the SIZE only where the caller says the venue's row states the order's TOTAL size
//!   (`quantity_is_total`: `vike_venues::venue_amend::report_quantity_is_total`, read from each bridge's parser),
//!   never for a combo (its size is in combo units) and not while a fill is on its way: the two sides'
//!   filled parts differ, so the row and the book were read on either side of an execution;
//! - on the instrument's GRID: a price on its tick, a size on its lot. Two values are the same when they
//!   are equal once both are put on the step, or within half a step of each other. A symbol with no
//!   tick (lot) has no price (size) compared: a raw float compare is not a verdict.
//!
//! Pure: no clock (the caller's `settling` reads it), no I/O.

use vike_model::{Grid, OrderStatusReport, modified_prices, round_to_step};

use super::types::{ReconcileDivergence, ReconcileLocalView};

/// The [`ReconcileDivergence::OrderTermsDrift`]s of one pass: `orders` (the venue's open-order rows) against
/// `local`'s registry. `grid_of` is a symbol's grid in the engine's spelling (its tick and lot);
/// `quantity_is_total` says the venue's rows state each order's TOTAL size; `settling` says a client order
/// id's amend is in flight or was just answered.
pub fn diff_order_terms(
    orders: &[OrderStatusReport],
    local: &ReconcileLocalView,
    grid_of: &dyn Fn(&str) -> Grid,
    quantity_is_total: bool,
    settling: &dyn Fn(&str) -> bool,
) -> Vec<ReconcileDivergence> {
    let mut out = Vec::new();
    for o in orders {
        let Some(order_client_id) = o.order_client_id.as_deref() else { continue };
        let Some(mo) = local.orders.get(order_client_id) else { continue };
        if o.status.is_terminal() || !mo.status.is_modifiable() || settling(order_client_id) {
            continue;
        }
        let grid = grid_of(&mo.request.symbol);
        // Where an `OrderModified` would land each price on this kind: compare only those.
        let (lands_price, lands_trigger) =
            modified_prices(&mo.request.order_type, Some(1.0), Some(1.0));
        let price = lands_price.and_then(|_| drift(mo.request.price, o.price, grid.tick_size));
        let trigger_price = lands_trigger
            .and_then(|_| drift(mo.request.trigger_price, o.trigger_price, grid.tick_size));
        let fill_in_flight =
            drift(Some(mo.filled_qty), Some(o.filled_qty), grid.lot_size).is_some();
        let qty = (quantity_is_total && mo.request.combo_legs.is_empty() && !fill_in_flight)
            .then(|| drift(Some(mo.request.qty), Some(o.qty).filter(|q| *q > 0.0), grid.lot_size))
            .flatten();
        if price.is_some() || trigger_price.is_some() || qty.is_some() {
            out.push(ReconcileDivergence::OrderTermsDrift {
                venue: o.venue.clone(),
                symbol: o.symbol.clone(),
                order_client_id: order_client_id.to_string(),
                price,
                trigger_price,
                qty,
                timestamp: o.timestamp,
            });
        }
    }
    out
}

/// `(local, venue)` when both are stated, finite and different on the `step` grid; `None` when they
/// agree, either is unstated, or there is no usable step.
fn drift(local: Option<f64>, venue: Option<f64>, step: Option<f64>) -> Option<(f64, f64)> {
    let (l, v, s) = (local?, venue?, step.filter(|s| s.is_finite() && *s > 0.0)?);
    (l.is_finite() && v.is_finite() && differs_on_step(l, v, s)).then_some((l, v))
}

/// `a` and `b` are different values on a `step` grid: not equal once both are put on the step, and
/// further apart than half a step (either agreement is the same value).
fn differs_on_step(a: f64, b: f64, step: f64) -> bool {
    round_to_step(a, step) != round_to_step(b, step) && (a - b).abs() > step / 2.0
}

#[path = "terms_tests.rs"]
#[cfg(test)]
mod terms_tests;
