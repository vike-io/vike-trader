//! Hyperliquid's feed arm: one keyless feed (bars + quotes) through the `CoreLaneSink`, behind a price tap.

use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::{DataClient, DataClientSink};
use vike_tradehub_core::MountPlacement;

use super::StrategyLaneFactories;
use super::live::{DataClientTracking, StrategyLaneQuote};
use super::price_tap::PriceTap;
use vike_model::VenueNetwork;

/// Hyperliquid's feed: ONE keyless feed through the venue-agnostic `CoreLaneSink`, with a bar
/// subscription per distinct `(symbol, interval)` and a quote subscription per distinct symbol
/// across this venue's mounts. Any subscription failure refuses the mount.
pub(super) fn wire_hyperliquid_feed(
    network: &VenueNetwork,
    config: &MountPlacement,
    configs: &[&MountPlacement],
    handle: &CoreHandle,
    live_venues: &std::collections::HashSet<String>,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, String> {
    let (mut feeds, tap) = dial_hyperliquid(network, handle, wrap, make)?;
    // One BAR subscription per distinct (symbol, interval) across this venue's mounts, one
    // QUOTE subscription per distinct symbol — two mounts on one series subscribe once
    // (split-plane I10; a single mount is one of each, exactly as before).
    let mut bar_keys: Vec<(&str, &str)> = Vec::new();
    let mut quote_keys: Vec<&str> = Vec::new();
    for c in configs {
        if !bar_keys.contains(&(c.symbol.as_str(), c.interval.as_str())) {
            bar_keys.push((&c.symbol, &c.interval));
            feeds
                .subscribe_bars(&c.symbol, &c.interval)
                .map_err(|e| format!("subscribe_bars {}@{}: {e:?}", c.symbol, c.interval))?;
        }
        if !quote_keys.contains(&c.symbol.as_str()) {
            quote_keys.push(&c.symbol);
            feeds
                .subscribe_quotes(&c.symbol)
                .map_err(|e| format!("subscribe_quotes {}: {e:?}", c.symbol))?;
        }
    }
    tracing::info!(
        venue = %config.venue, symbol = %config.symbol, interval = %config.interval,
        mounts = configs.len(),
        network = ?network, live_venues = ?live_venues,
        "LIVE assemble_runtime core up; Hyperliquid feed subscribed (bars + quotes)"
    );
    Ok(StrategyLaneQuote::new(feeds, tap))
}

/// Hyperliquid's PRICE-ONLY feed, for a box that mounts no Hyperliquid market: the dial on `network` and the
/// price tap, nothing subscribed. The control door subscribes a coin's quote lane through its
/// [`StrategyLaneQuote::watcher`] when an order names a coin the account was not started on (decision 0174).
pub(super) fn wire_hyperliquid_price_feed(
    network: &VenueNetwork,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<StrategyLaneQuote, String> {
    let (feeds, tap) = dial_hyperliquid(network, handle, wrap, make)?;
    Ok(StrategyLaneQuote::new(feeds, tap))
}

/// The dial both Hyperliquid arms start from, and the price tap in front of its sink.
fn dial_hyperliquid(
    network: &VenueNetwork,
    handle: &CoreHandle,
    wrap: &dyn Fn(Arc<dyn DataClientSink>) -> Arc<dyn DataClientSink>,
    make: &dyn StrategyLaneFactories,
) -> Result<(DataClientTracking, Arc<PriceTap>), String> {
    // The shared `feeds.rs` `dial`, plain tap. `subscribe_bars` drives the account-wide margin/drawdown
    // sweeps (per CLOSED bar) + the mark lane; `subscribe_quotes` drives the maker's `on_quote_tick` (the
    // A-S maker prices on quotes/book, NOT on_bar). KEYLESS-public feed — the exec creds gate is separate
    // (inside assemble_runtime).
    //
    // The venue's network is the one thing a Hyperliquid dial NAMES (`StrategyOpen{network}`): the datahub
    // keys its client by it and the gatewayhub builds the venue client for it, so a test-network
    // account is never priced off mainnet and a main-network one never off the test book. Its
    // `mark_streams` row (decision 0095) is the gatewayhub's to read.
    super::dial("hyperliquid", strategy_network(*network), PriceTap::default, handle, wrap, make)
}

/// The word the datahub's `StrategyOpen` carries for Hyperliquid's feed network (decision 0095: the one
/// venue whose feed follows the account tier; the strategy lane keys its client by it,
/// `StreamOpenerStrategyLane::on_network`): `None` for the main network (the request names no network, and the
/// datahub's one main-network client serves it), `testnet` for the other.
pub(super) fn strategy_network(network: VenueNetwork) -> Option<&'static str> {
    match network {
        VenueNetwork::Mainnet => None,
        VenueNetwork::Testnet => Some(vike_datahub_client::proto::STRATEGY_NETWORK_TESTNET),
    }
}
