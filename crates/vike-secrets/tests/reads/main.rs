//! The `reads` binary — what a caller reads out of a store: the account-addressed credential reads (the credential book, the one-field read, the account directory's credentials), the scoped and demo-only credential reads (and the scoped loaders' log lines), the venue-setting reader and its fold, and the profile store.

#[path = "../support/mod.rs"]
mod support;

mod credential_book;
mod demo_only_scope;
mod profile_store;
mod scoped_loader_logs;
mod scoped_read;
mod venue_setting_fold;
mod venue_settings;
