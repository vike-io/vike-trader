//! Order vocabulary: the request an order is, its client id, how it triggers and fills against a
//! bar, its barriers, whether HALT admits it, and the venue reports reconcile reads back.
//!
//! A module directory rather than a crate, for `docs/decisions/0003-vike-model-splits-by-module.md`'s
//! reason: the order vocabulary is named by the journaled command lane, so it cannot leave this crate.

pub mod barrier;
pub mod client_order_id;
pub mod fill_trigger;
pub mod halt_admit;
pub mod order;
pub mod own_book;
pub mod reports;
