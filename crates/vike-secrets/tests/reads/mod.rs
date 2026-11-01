//! The `reads` group of the `secrets` binary — what a caller reads out of a store: the account-addressed credential reads (the credential book, the one-field read, the account directory's credentials), the scoped and demo-only credential reads (and the scoped loaders' log lines), the venue-setting reader and its fold, the profile store, and the root's door `vike_secrets::open_credential_store` (`credential_chain_roots`, `credential_scope`), which boot nothing.

mod credential_book;
mod credential_chain_roots;
mod credential_scope;
mod demo_only_scope;
mod no_venue_keys_scope;
mod profile_store;
mod scoped_loader_logs;
mod scoped_read;
mod venue_setting_fold;
mod venue_settings;
