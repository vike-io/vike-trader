//! The `reads` binary — what a caller reads out of a store: the scoped and demo-only credential reads, the venue-setting reader and its fold, the profile store, and the `live` -> `demo` ceiling migration.

#[path = "../support/mod.rs"]
mod support;

mod demo_only_scope;
mod live_means_mainnet;
mod profile_store;
mod scoped_read;
mod venue_setting_fold;
mod venue_settings;
