//! The venues x Sim/Demo/Live credential-status grid — the `vike-connections` tool: which venues
//! have API keys configured in the workspace `.env`, plus in-app editing of those keys, plus
//! (foundation, see [`vike_model::feed_status`]) a live per-venue connection-status column.
//!
//! `status` is the pure credential enumerator (unit-tested); the pure live connection-state model +
//! string parser ([`ConnectionState`]/[`parse_feed_status`], unit-tested; binance-only source today,
//! every other venue reports `Unknown` until it gets a producer wired in) MOVED DOWN to
//! [`vike_model::feed_status`] and is re-exported here under its historical
//! `vike_connections::{parse_feed_status, ConnectionState}` paths — its other consumer,
//! `vike_ops::reconcile_config::health_from_feed_status`, is called by the HEADLESS daemon, which
//! may not reach through this (egui) crate to get at it, and this crate in turn no longer reaches
//! through `vike-ops` (and so `vike-core`/`vike-data`/`vike-alerting`) to get at 142
//! dependency-free lines; `env_write` is the pure `.env` upsert +
//! atomic write (unit/integration-tested, no secrets in tests); `view` is the egui render (mounted
//! by vike-app as the Connections tool window) — grid + Status column + masked add/edit form. The
//! grid never displays an existing secret's plaintext (status stays ●/○); edit fields are masked
//! (`egui::TextEdit::password`) and start empty; no secret value is ever logged.

//!
//! # More than one account per venue
//!
//! [`status::credential_status_for_account`] is the per-account read, and [`status::AccountGrids`]
//! is the ENUMERATION built on it: the default account's grid plus one per labelled account the
//! store holds. `view` renders one of them at a time, picked in a chip strip above the grid, and
//! composes every key name it writes through `vike_model::account_keys::account_key`. On a box with
//! no labelled account the enumeration is empty, the strip offers one chip, and every name is
//! returned unchanged — so that box's panel is the one that shipped.

pub mod env_write;
pub mod status;
pub mod view;

pub use env_write::{CredentialHome, CredentialWrite, save_credentials_journalled};
pub use status::{
    AccountGrids, VENUES, VenueCredStatus, credential_status, credential_status_for_account,
};
pub use view::connections_ui;
pub use vike_model::feed_status::{ConnectionState, parse_feed_status};
