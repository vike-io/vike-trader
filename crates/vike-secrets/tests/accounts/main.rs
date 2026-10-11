//! The `accounts` integration-test binary: the `account` table's reader, its writers (book, lifecycle, tier, exposure ceiling) and the `venue` table beside it.

#[path = "../support/mod.rs"]
mod support;

mod book;
mod lifecycle;
mod max_exposure;
mod reader;
mod tier;
mod venue_table;
mod venue_titles;
