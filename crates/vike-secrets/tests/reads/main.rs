//! The `reads` binary — what a caller reads out of a store: the scoped and demo-only credential reads, the venue-setting reader and its fold, and the profile store.

#[path = "../support/mod.rs"]
mod support;

mod demo_only_scope;
mod profile_store;
mod scoped_read;
mod venue_setting_fold;
mod venue_settings;
