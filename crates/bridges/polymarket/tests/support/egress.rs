//! Shared by this crate's `#[ignore]`d live smokes: declare the process's Polymarket egress from the
//! settings database, exactly as a composition root does (decision 0095 — the bridge reads no
//! environment and opens no store). Configure the tunnel with
//! `vike-cli config set venue.polymarket.proxy_host <host>` and `… proxy_port <port>`, or
//! `vike-cli config set venue.polymarket.socks_proxy -` with the URL on stdin. A box with no settings
//! database keeps the built-in default: a SOCKS proxy at 127.0.0.1:1080.
//!
//! The rows are read here, by the test, and handed to `vike_polymarket::declare_from_rows` —
//! the one entry every root calls, which owns the precedence and the store-error policy. A dev box
//! whose tunnel is not yet a row writes it first (`vike-cli config set`), which a live test should
//! not paper over.

/// Call first in every live test, before anything dials the venue. Returns the venue's
/// `venue.polymarket.*` rows, which a test that mounts hands on as `MountInputs::settings` would be
/// (decision 0095).
pub fn declare_from_the_settings_database() -> vike_secrets::venue_setting::VenueSettings {
    let loaded = vike_model::paths::state_path::project_settings_dir_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| vike_secrets::venue_setting::load_venue_settings(&dir));
    // Two tests of one binary declare the same settings, which is `Ok`.
    vike_polymarket::declare_from_rows(loaded).unwrap_or_default()
}
