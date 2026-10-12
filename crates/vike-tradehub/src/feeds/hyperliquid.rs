//! Hyperliquid's feed arm: one keyless `Feeds` (bars + quotes) through the `CoreLaneSink`.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_mount::MakerMountConfig;

use super::mark_streams_setting;

/// Hyperliquid's feed: ONE keyless `Feeds` through the venue-agnostic `CoreLaneSink`, with a bar
/// subscription per distinct `(symbol, interval)` and a quote subscription per distinct symbol
/// across this venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_hyperliquid_feed(
    network: &vike_hyperliquid::config::Network,
    cfg: &MakerMountConfig,
    cfgs: &[&MakerMountConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    venue_settings: &std::collections::BTreeMap<String, vike_secrets::venue_setting::VenueSettings>,
) -> Result<vike_hyperliquid::market_feed::Feeds, String> {
    // The venue-agnostic `CoreLaneSink` (LiveDataSink → core-lane bridge). `subscribe_bars`
    // drives the account-wide margin/drawdown sweeps (per CLOSED bar) + the mark lane;
    // `subscribe_quotes` drives the maker's `on_quote_tick` (the A-S maker prices on
    // quotes/book, NOT on_bar). KEYLESS-public feed — the exec creds gate is separate (inside
    // build_node).
    let sink = wrap(Arc::new(vike_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    let mut feeds = vike_hyperliquid::market_feed::Feeds::new(sink, || {})
        .with_network(*network)
        .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid"));
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts, one
    // QUOTE subscription per distinct symbol — two mounts on one series subscribe once
    // (split-plane I10; a single mount is one of each, exactly as before).
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in cfgs {
        if !bar_keys.contains(&(c.token_id.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.token_id, &c.interval));
            feeds
                .subscribe_bars(&c.token_id, &c.interval)
                .map_err(|e| format!("subscribe_bars {}@{}: {e:?}", c.token_id, c.interval))?;
        }
        if !quote_keys.contains(&c.token_id.as_str()) {
            quote_keys.push(&c.token_id);
            feeds
                .subscribe_quotes(&c.token_id)
                .map_err(|e| format!("subscribe_quotes {}: {e:?}", c.token_id))?;
        }
    }
    tracing::info!(
        venue = %cfg.venue, symbol = %cfg.token_id, interval = %cfg.interval,
        mounts = cfgs.len(),
        network = ?network, live_venues = ?live_venues,
        "LIVE build_node core up; Hyperliquid feed subscribed (bars + quotes)"
    );
    Ok(feeds)
}
