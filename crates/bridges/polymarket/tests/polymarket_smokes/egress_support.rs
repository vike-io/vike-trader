//! Shared by this crate's `#[ignore]`d live smokes: declare the process's Polymarket egress from the
//! settings database, exactly as a composition root does (decision 0095 — the bridge reads no
//! environment and opens no store). Turn the tunnel on with
//! `vike-cli config set venue.polymarket.proxy_enabled true` (its endpoint `… proxy_host <host>` and
//! `… proxy_port <port>`, default 127.0.0.1:1080), or `vike-cli config set venue.polymarket.socks_proxy -`
//! with the URL on stdin. A box with no settings database keeps the built-in default: no proxy (direct).
//!
//! The rows are read here, by the test, and handed to `vike_polymarket::declare_from_rows` —
//! the one entry every root calls, which owns the precedence and the store-error policy. A dev box
//! whose tunnel is not yet a row writes it first (`vike-cli config set`), which a live test should
//! not paper over.

/// Call first in every live test, before anything dials the venue. Returns the venue's
/// `venue.polymarket.*` rows, which a test that starts hands on as `AdapterInputs::settings` would be
/// (decision 0095).
pub fn declare_from_the_settings_database() -> vike_secrets::venue_setting::VenueSettings {
    let loaded = vike_paths::state_path::project_settings_dir_from(
        std::env::var("VIKE_SETTINGS_DIR").ok().as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| vike_secrets::venue_setting::load_venue_settings(&dir));
    // Two tests of one binary declare the same settings, which is `Ok`. A box with no settings
    // database reads the release's seed addresses, as `load_venue_settings` does with no table.
    vike_polymarket::declare_from_rows(loaded)
        .unwrap_or_else(|| vike_secrets::venue_setting::VenueSettings::default().with_urls(seed()))
}

/// The venue's addresses of the release's seed: what a smoke dials when it reads no store.
pub fn seed() -> vike_venues::venue_urls::VenueUrls {
    vike_venues::venue_urls::VenueUrls::seed("polymarket")
}

/// One `live` row of the release's seed (the venue has no other type): the CLOB is
/// `(Trade, Rest, "")`, the data-api `(Data, Rest, "data")`, the relayer `(Trade, Rest, "relayer")`.
pub fn seed_row(
    channel: vike_venues::venue_urls::UrlChannel,
    protocol: vike_venues::venue_urls::UrlProtocol,
    product: &str,
) -> String {
    let seed = seed();
    let row = seed.url(vike_venues::venue_urls::UrlType::Live, channel, protocol, product);
    row.expect("the seed holds the row").to_string()
}
