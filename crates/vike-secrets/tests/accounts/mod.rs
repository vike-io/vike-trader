//! The `accounts` integration-test binary: the `account` table's reader, its writers (book, lifecycle, tier, exposure ceiling) and the `venue` table beside it.

mod book;
mod fact;
mod leverage;
mod lifecycle;
mod max_exposure;
mod reader;
mod symbol;
mod tier;
mod venue_table;
mod venue_titles;
