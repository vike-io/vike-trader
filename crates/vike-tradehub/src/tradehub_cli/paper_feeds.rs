//! **The paper-only node's LIVE prices** (the owner's "3 - yes!", 2026-11-16, on the design in the wave-6 PAPER-QUOTES
//! report): with `flags.tradehub_live` off the node runs only `paper` accounts, and it now prices the KEYLESS venues its
//! strategy mounts trade (`crate::feeds::keyless_feed`: binance, bybit, okx, aster, hyperliquid, deribit, polymarket) the
//! way the live arm does, gatewayhub > datahub > tradehub over the datahub's strategy lane, so their paper books fill on
//! the venue's quotes and bars (decision 0285). Nothing here logs in to a venue or signs anything.
//!
//! * **The live arm's own wiring**: each venue's plan is `crate::feeds::venue_feed_plan` over an arming table where
//!   nothing is armed (`ArmingTable::default`), on the REAL market ([`on_the_real_market`]: hyperliquid's main network,
//!   which an unarmed table would not pick), and its feed is `crate::feeds::wire_venue_feeds` onto the paper core's
//!   lanes, one arm per distinct venue, nothing teed.
//! * **The same feed gate**: the datahub's strategy lane is asked first (`StrategyLaneFactories::datahub_probe`). A
//!   datahub, gatewayhub or strategy-lane key that is missing wires nothing and is said at `error` as
//!   `PRICE FEEDS UNAVAILABLE` with its cause; the ready banner carries the same prefix (`banner.rs`'s
//!   `feeds_unavailable_mode`). A venue whose own feed is refused is said the same way and the others still wire. The
//!   start never ends over a feed: a paper account priced by nothing says so on every order and mount.
//! * **What is priced is a per-venue fact on the wire** ([`PaperFeeds::priced`], published as
//!   `HubIdentityWire::priced_venues`): an order or a mount on any other venue (a credentialed one: its prices need its
//!   demo login, which this mode does not run) is refused `no price: this node has no price feed for <venue> ...`.
//! * **The same teardown**: the feeds go into the run's `StrategyLanesLiveTeardown`, shut down before the core is.
//!
//! No restart is ordered when the price path comes back (the live arm's late arm needs a gatewayhub link this mode does
//! not hold): such a node is restarted by hand, which its banner and its error line say.

use std::collections::HashSet;
use std::sync::Arc;

use vike_core::CoreHandle;
use vike_data::DataClientSink;
use vike_model::VenueNetwork;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::{AdapterPolicy, ArmingTable, MountPlacement};

use crate::StrategyLanePlan;
use crate::feeds::{StrategyLaneFactories, StrategyLanesLive, venue_feed_plan, wire_venue_feeds};

/// What [`wire_paper_feeds`] built.
#[derive(Default)]
pub(super) struct PaperFeeds {
    /// The wired feeds, for the teardown.
    pub(super) feeds: Vec<StrategyLanesLive>,
    /// The venues whose feed was wired, in mount order: what the node prices.
    pub(super) priced: Vec<String>,
    /// Why a keyless venue is NOT priced (the first cause): the banner's `PRICE FEEDS UNAVAILABLE` prefix.
    pub(super) unavailable: Option<crate::feeds::FeedsDown>,
}

/// **Wire the feeds of the keyless venues `configs` mount onto the paper core `handle`** (the module doc). `make` is the
/// feed constructor (production: the datahub's strategy lane, `live_runtime.rs`'s `production_feed_ctors`).
pub(super) fn wire_paper_feeds(
    configs: &[MountPlacement],
    handle: &CoreHandle,
    make: &dyn StrategyLaneFactories,
) -> PaperFeeds {
    let mut venues: Vec<String> = Vec::new();
    for config in configs.iter().filter(|c| crate::feeds::keyless_feed(&c.venue)) {
        if !venues.contains(&config.venue) {
            venues.push(config.venue.clone());
        }
    }
    let mut out = PaperFeeds::default();
    if venues.is_empty() {
        return out;
    }
    if let Some(why) = make.datahub_probe(&venues).and_then(|probe| probe.check()) {
        tracing::error!(
            reason = %why,
            venues = %venues.join(", "),
            "{}: {}: the paper-only node wires no price feed for {}, so their paper accounts get no price and every \
             order on them is refused. Restart this tradehub once the datahub, its gatewayhub and the strategy-lane \
             key answer: {why}",
            crate::feed_gate::PRICE_FEEDS_UNAVAILABLE,
            crate::feeds::short_cause(&why),
            venues.join(", "),
        );
        out.unavailable = Some(why);
        return out;
    }
    let (table, policy) = (ArmingTable::default(), AdapterPolicy::default());
    let wrap = |base: Arc<dyn DataClientSink>| -> Arc<dyn DataClientSink> { base };
    for venue in venues {
        let venue_configs: Vec<&MountPlacement> =
            configs.iter().filter(|c| c.venue == venue).collect();
        let accounts: Vec<AccountLabel> = venue_configs
            .iter()
            .map(|c| c.account.clone().unwrap_or(AccountLabel::Default))
            .collect();
        let wired = venue_feed_plan(venue_configs[0], &table, &policy)
            .map(on_the_real_market)
            .map_err(crate::feeds::FeedRefusal::from)
            .and_then(|plan| {
                wire_venue_feeds(
                    &plan,
                    &venue_configs,
                    handle,
                    &HashSet::new(),
                    &[],
                    &accounts,
                    &HashSet::new(),
                    &wrap,
                    make,
                )
            });
        match wired {
            Ok(feed) => {
                tracing::info!(
                    venue = %venue,
                    "the paper-only node prices {venue} from its live feed (gatewayhub > datahub > tradehub): its \
                     paper accounts fill on the venue's quotes and bars"
                );
                out.feeds.push(feed);
                out.priced.push(venue);
            }
            Err(why) => {
                tracing::error!(
                    venue = %venue,
                    reason = %why,
                    "{}: {venue}'s price feed was refused, so its paper accounts get no price and every order on \
                     {venue} is refused; the other venues are priced. Restart this tradehub once it answers",
                    crate::feed_gate::PRICE_FEEDS_UNAVAILABLE,
                );
                out.unavailable.get_or_insert(why.text.into());
            }
        }
    }
    out
}

/// **A paper book prices on the REAL market.** Hyperliquid's feed follows its armed accounts' network, which with
/// nothing armed is its test network; the paper-only arm reads the main network's public feed instead, as binance's
/// paper reads binance.com's. Keyless and read-only: the network only names the datahub's data client
/// (`crates/vike-tradehub/src/feeds/hyperliquid.rs`'s `strategy_network`: the main network is the request that names
/// none), and this arm arms no account and opens no order path. Every other keyless feed is the main network already.
pub(super) fn on_the_real_market(plan: StrategyLanePlan) -> StrategyLanePlan {
    match plan {
        StrategyLanePlan::Hyperliquid(_) => StrategyLanePlan::Hyperliquid(VenueNetwork::Mainnet),
        other => other,
    }
}
