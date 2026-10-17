//! Hyperliquid's feed arm: one keyless feed (bars + quotes) through the `CoreLaneSink`.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, LiveDataSink};
use vike_tradehub_core::MakerConfig;

use super::FeedCtors;
use super::live::Tracked;
use crate::HlNetwork;

/// Hyperliquid's feed: ONE keyless feed through the venue-agnostic `CoreLaneSink`, with a bar
/// subscription per distinct `(symbol, interval)` and a quote subscription per distinct symbol
/// across this venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_hyperliquid_feed(
    network: &HlNetwork,
    cfg: &MakerConfig,
    cfgs: &[&MakerConfig],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn LiveDataSink>) -> Arc<dyn LiveDataSink>,
    make: &dyn FeedCtors,
) -> Result<Tracked, String> {
    // The venue-agnostic `CoreLaneSink` (LiveDataSink → core-lane bridge). `subscribe_bars`
    // drives the account-wide margin/drawdown sweeps (per CLOSED bar) + the mark lane;
    // `subscribe_quotes` drives the maker's `on_quote_tick` (the A-S maker prices on
    // quotes/book, NOT on_bar). KEYLESS-public feed — the exec creds gate is separate (inside
    // assemble_runtime).
    let sink = wrap(Arc::new(vike_tradehub_core::CoreLaneSink::new(
        handle.bar_sender(),
        handle.market_sender(),
        handle.tick_sender(),
    )) as Arc<dyn LiveDataSink>);
    // The venue's network is the one thing a Hyperliquid dial NAMES (`StratOpen{network}`): the datahub
    // keys its client by it and the gatewayhub builds the venue client for it, so a test-network
    // account is never priced off mainnet and a main-network one never off the test book. Its
    // `mark_streams` row (decision 0095) is the gatewayhub's to read.
    let mut feeds = Tracked::new(make.client("hyperliquid", network.strat_network(), sink)?);
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
        "LIVE assemble_runtime core up; Hyperliquid feed subscribed (bars + quotes)"
    );
    Ok(feeds)
}
