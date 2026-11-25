//! Every create, write and refusal proof of the settings store, and the shape proofs beside them,
//! in one test binary.

mod access;
mod database;
mod hot_rollback_journal;
mod ibkr_cash_settled_root;
mod journalled_writers;
mod operator_instrument;
mod operator_instrument_list;
mod probe_copy;
mod read_only_store;
mod stock_split;
mod store_backup;
mod strategy_code;
mod symbol_set;
mod venue_links;
mod venue_setting_any_tier;
mod write_open;
