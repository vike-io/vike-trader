//! Every schema-upgrade proof of the settings store, in one test binary.

#[path = "../support/mod.rs"]
mod support;

mod database;
mod dead_column_drop;
mod hot_rollback_journal;
mod paper_tier;
mod venue_links;
mod venue_setting_any_tier;
