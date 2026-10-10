//! Every create, write and refusal proof of the settings store, and the shape proofs beside them,
//! in one test binary.

#[path = "../support/mod.rs"]
mod support;

mod database;
mod hot_rollback_journal;
mod venue_links;
mod venue_setting_any_tier;
