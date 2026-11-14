//! Order vocabulary: the request an order is, its client id, how it triggers and fills against a
//! bar, whether HALT admits it, why a cancel is issued, the order under management (`record.rs`:
//! `OrderStatus` and the `OrderRecord` state machine), an order a gate dropped (`drop.rs`: `OrderDrop`), and
//! the order a strategy places through the portable order seam (`placement.rs`: `OrderPlacement`, with its one
//! translation into a request's terms). The venue reports reconcile reads back are `crate::venues`' (what a
//! venue tells us).
//!
//! A module directory rather than a crate, for `docs/decisions/0003-vike-model-splits-by-module.md`'s
//! reason: the order vocabulary is named by the journaled command lane, so it cannot leave this crate.

// Its one name, `OrderCancelIntent`, is spelled from the crate root.
pub(crate) mod cancel;
pub mod client_id;
// Its one name is spelled from the crate root (`vike_model::OrderDrop`).
pub(crate) mod drop;
pub mod fill_trigger;
pub mod halt_admit;
pub mod order;
pub mod own_order;
pub mod record;
// Its one name, `OrderSide`, is spelled from the crate root.
pub(crate) mod order_side;
// Its one name, `OrderPlacement`, is spelled from the crate root.
pub(crate) mod placement;
