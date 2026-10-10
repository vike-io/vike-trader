//! The `accounts` integration-test binary: the `account` table's reader, its writers (book, lifecycle, tier) and the `venue` table beside it.

#[path = "../support/mod.rs"]
mod support;

mod book;
mod lifecycle;
mod reader;
mod tier;
mod venue_table;
mod venue_titles;
