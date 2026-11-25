//! The data lane's VENUE TABLE — the ONE function in this daemon that constructs a venue market-data
//! client ([`build_client`]) — and the [`DataLane`] built over it ([`DataLaneBridge`]).
//!
//! The arms are the datahub's `build_client` arms, MOVED: the same constructors, the same
//! `venue_setting` reads (decision 0095), the same refusal for a venue outside the table. The datahub
//! builds no venue client of its own: its `build_client` (`crates/vike-datahub/src/feeds/venues.rs`)
//! makes a `vike_market_stream::market::DataClientStream` over its `GatewayLink`, one stream socket
//! to this lane per client. The sharing column (`EXPLICIT_SHARING`, `Shared`/`PerHolder`) is a
//! consumer-side rule and stays there, so the lane builds exactly ONE client per stream socket
//! whatever the venue (`crate::server::stream`).
//!
//! # The shape: one plain arm per venue
//!
//! The `data` feature links every bridge (`crates/vike-gatewayhub/Cargo.toml`), so unlike the
//! datahub's table there is no `#[cfg(feature)]` arm and no `#[cfg(not)]` twin: a venue is either in
//! the table or it is not. A venue the table does not know is refused naming the table
//! ([`crate::registry::RosterLane::Stream`]), never a feature to rebuild with, because there is none to name.
//! The arms and the roster are held equal both ways by this file's tests, so a venue cannot be built
//! here and left unadvertised (or advertised and unbuilt) by forgetting one of two lists.
use std::collections::BTreeMap;
use std::sync::Arc;
use vike_data::DataClientBuilder;

use vike_bridge_core::data_client::ws::DataClientWebsocket;
use vike_data::{DataClient, DataClientError, DataClientSink};
use vike_model::AccountTier;
// The one `mark_streams` reader, shared with the trading daemon's feed wiring.
use vike_secrets::venue_setting::{VenueSettings, mark_streams_setting};
use vike_venues::venue_urls::VenueUrls;

use vike_gatewayhub_client::history::network_venues;
use vike_model::VenueNetwork;

use super::credentialed::CredentialedStreamLane;
use super::{DataLane, DataLaneHolder, FeedTickSink, FeedTickSubscription};
use crate::data::feed_tick::{FeedTickHub, feed_tick_table};
use crate::registry::{RosterLane, owned, roster};

/// **`venue`'s address rows out of the boot's one read** (`venue_settings`, every venue's rows), for a
/// bridge built here or in the REST lane (decision 0265). A venue with no entry gets no row, so every
/// lookup of it is refused naming the row (`vike_venues::venue_urls::UrlRefusal`), never a default.
pub fn venue_urls(venue_settings: &BTreeMap<String, VenueSettings>, venue: &str) -> VenueUrls {
    venue_settings
        .get(venue)
        .map(|s| s.urls().clone())
        .unwrap_or_else(|| VenueUrls::from_rows(venue, std::iter::empty()))
}

/// The tier the aster feed is built on: `Live`, whose `data` rows are the mainnet public market-data
/// hosts (`vike_aster::transport::urls::AsterUrls::data`; owner ruling 7(a), decision 0164; the arm in
/// [`build_client`] says why). It names a NETWORK, not an account: no credential is read for it.
const ASTER_FEED_TIER: AccountTier = AccountTier::Live;

/// A venue's address rows refused its feed: the refusal's words, as `build_client`'s `Err`.
fn refused(venue: &str, why: vike_venues::venue_urls::UrlRefusal) -> String {
    format!("feed broker: venue `{venue}` is NOT RUNNING: {why}")
}

/// Build one venue's market-data client, or say WHY not.
///
/// ⚠ Every constructor is the one the datahub's market-data plane called before the broker existed,
/// verbatim — plus, since decision 0095, the venue's own `venue_setting` rows out of `venue_settings`
/// (Polymarket's socket batching, each CEX feed's mark-stream row), and since decision 0265 its
/// `venue_url` rows ([`venue_urls`]): a CEX feed whose rows name no address is refused here, naming the
/// row. The `|| {}` is the GUI repaint nudge — a headless caller passes an empty closure, exactly as the
/// recorder does.
pub fn build_client(
    venue: &str,
    sink: Arc<dyn DataClientSink>,
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Result<Box<dyn DataClient + Send>, String> {
    let urls = || venue_urls(venue_settings, venue);
    match venue {
        "binance" => Ok(Box::new(
            DataClientWebsocket::new(
                vike_binance::feed::DataClientSpecBinance::new(&urls())
                    .map_err(|why| refused(venue, why))?,
                sink,
                || {},
            )
            .with_mark_streams(mark_streams_setting(venue_settings, "binance")),
        )),

        // A symbol here is a USER name (decision 0290), routed by its kept row: `BTCUSDT.P` is the linear book.
        "bybit" => Ok(Box::new(
            DataClientWebsocket::new(
                vike_bybit::feed::DataClientSpecBybit::new(&urls())
                    .map_err(|why| refused(venue, why))?,
                sink,
                || {},
            )
            .with_mark_streams(mark_streams_setting(venue_settings, "bybit")),
        )),

        "okx" => Ok(Box::new(
            DataClientWebsocket::new(
                vike_okx::feed::DataClientSpecOkx::new(&urls())
                    .map_err(|why| refused(venue, why))?,
                sink,
                || {},
            )
            .with_mark_streams(mark_streams_setting(venue_settings, "okx")),
        )),

        // ⚠ OWNER RULING 7(a) of decision 0164, the one behaviour change of wave 1: this feed is built
        // on MAINNET. The feed once defaulted the tier to `AccountTier::Demo`, which is what the datahub's
        // aster feed was, while the datahub's aster backfill and catalog and the tradehub's aster feed are
        // mainnet and keyless market data is mainnet at every tier for the other CEXs. The tier picks
        // HOSTS only (`vike_aster::transport::urls::AsterUrls::data`): this arm reads no credential and the feed
        // touches no authenticated endpoint, so a box whose aster account is `live` is not asked for, and
        // does not spend, a key by serving this feed.
        "aster" => Ok(Box::new(
            DataClientWebsocket::new(
                vike_aster::feed::DataClientSpecAster::new(&urls(), ASTER_FEED_TIER)
                    .map_err(|why| refused(venue, why))?,
                sink,
                || {},
            )
            .with_mark_streams(mark_streams_setting(venue_settings, "aster")),
        )),

        // ⚠ MAINNET, on purpose and by the feed's default: this table takes no network, so a session that
        // names none (`DataOpen { network: None }`) is fed the main network, as the datahub's market-data
        // arm always was. The bridge maps a `demo` AND a `paper` account to the testnet, so a caller that
        // wants the test network MUST say so: `build_client_on` is the only door that builds it.
        // The symbology fills in the background so a SPOT pair (`HYPE/USDC`) resolves to its wire coin (`@107`):
        // without it no spot pair had a price (decision 0218).
        "hyperliquid" => {
            let (urls, net) = (venue_urls(venue_settings, "hyperliquid"), VenueNetwork::Mainnet);
            let spec = vike_hyperliquid::feed::DataClientSpecHyperliquid::new(net, &urls)
                .map_err(|e| e.to_string())?
                .with_background_symbology();
            Ok(Box::new(
                DataClientWebsocket::new(spec, sink, || {})
                    .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid")),
            ))
        }

        "polymarket" => {
            // The venue's rows: K — `venue.polymarket.ws_tokens_per_socket` (decision 0095), or the
            // bridge's default — and the addresses the feed dials (its `venue_url` rows). No entry
            // for the venue, or a missing address, builds no client.
            let settings = venue_settings
                .get("polymarket")
                .ok_or("polymarket: no venue settings were loaded, so the feed has no address")?;
            let k = vike_polymarket::tokens_per_socket(|field| {
                settings
                    .get(vike_secrets::venue_setting::SettingTier::Any, field)
                    .map(str::to_string)
            });
            let source = vike_polymarket::DataClientBusSpecPolymarket::new(|| {}, settings.urls())
                .map_err(|e| e.to_string())?
                .with_tokens_per_socket(k);
            Ok(Box::new(vike_bridge_core::data_client::bus::DataClientBus::on(
                &Arc::new(source),
                sink,
            )))
        }

        // Deribit reads NO `venue_setting` row (its marks stream chain-wide through the options
        // feed, not a per-bars companion socket), so unlike every arm above it hands the feed
        // nothing but the sink and its address rows (mainnet's, whatever any account's tier).
        // Its bar lane refuses an interval the venue has no resolution code for on the caller's thread
        // (the spec's `refuse_bars`, `vike_deribit::feed::refuse_bars_interval`).
        "deribit" => {
            let urls = venue_urls(venue_settings, "deribit");
            let spec =
                vike_deribit::feed::DataClientSpecDeribit::new(&urls).map_err(|e| e.to_string())?;
            Ok(Box::new(DataClientWebsocket::new(spec, sink, || {})))
        }

        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            roster(RosterLane::Stream).join(", ")
        )),
    }
}

/// Build the market-data client of a venue that takes a NETWORK, for the network a strategy-lane session
/// named, or say WHY not (wave 2, W10).
///
/// The venues that take one are those whose price feed follows the account tier, by their network rows
/// (`vike_gatewayhub_client::history::network_venues`: Hyperliquid alone; the CEX feeds take no network and aster is
/// pinned `Live`), and each has its arm here: `testnet` and `mainnet` are `VenueNetwork::Testnet` / `VenueNetwork::Mainnet`
/// through the spec's network-taking `DataClientSpecHyperliquid::new` (the network the tradehub's `wire_hyperliquid_feed` set,
/// MOVED), on that network's address rows, with the venue's `mark_streams` row. [`build_client`] is not touched: it is the datahub's market-data
/// arm and stays the main network. A network word outside the two is refused, never read as the main one, and a venue
/// whose row follows the tier but has no arm is refused in words, never built as another venue.
pub fn build_client_on(
    venue: &str,
    network: &str,
    sink: Arc<dyn DataClientSink>,
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Result<Box<dyn DataClient + Send>, String> {
    if !network_venues().contains(&venue) {
        return Err(format!(
            "gatewayhub: venue `{venue}` takes no network (asked for `{network}`)"
        ));
    }
    let Some(network) = VenueNetwork::from_word(network) else {
        return Err(format!(
            "gatewayhub: unknown network `{network}` for {venue} (the words are `{}`)",
            VenueNetwork::WORDS.join("` and `")
        ));
    };
    match venue {
        "hyperliquid" => {
            let urls = venue_urls(venue_settings, "hyperliquid");
            let spec = vike_hyperliquid::feed::DataClientSpecHyperliquid::new(network, &urls)
                .map_err(|e| e.to_string())?
                // On the account's own network (a `demo` account's spot pairs are the testnet's).
                .with_background_symbology();
            Ok(Box::new(
                DataClientWebsocket::new(spec, sink, || {})
                    .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid")),
            ))
        }
        other => Err(format!(
            "gatewayhub: venue `{other}` follows the account tier but has no network-taking client in this \
             process's venue table"
        )),
    }
}

/// The table [`DataLaneBridge::new`] takes in production — carrying every venue's `venue_setting` rows,
/// which the clients read (decision 0095).
pub fn real_client_table(venue_settings: BTreeMap<String, VenueSettings>) -> DataClientBuilder {
    Box::new(move |venue, sink| build_client(venue, sink, &venue_settings))
}

/// The [`DataLane`] over the venue bridges: [`DataLane::venues`] is the stream roster and
/// [`DataLane::open`] is [`build_client`] for the session the server opened.
///
/// The `venue_settings` map is the lane's constructor PARAMETER, not something it reads: a library
/// takes configuration as parameters and only the binary reads the store
/// (`vike_secrets::venue_setting::load_venue_settings`, the daemon's `run`).
pub struct DataLaneBridge {
    clients: DataClientBuilder,
    /// The network-taking venues' builder; `None` advertises no `stream_network`.
    networked: Option<NetworkBuilder>,
    /// The shared tick pumps; `None` advertises no `gateway_feed_tick`.
    pumps: Option<Arc<FeedTickHub>>,
    /// The credentialed streams; `None` advertises no `cs_venue=`.
    credentialed: Option<CredentialedStreamLane>,
}

/// How the lane builds a client for a venue on a named network.
pub type NetworkBuilder = Box<
    dyn Fn(&str, &str, Arc<dyn DataClientSink>) -> Result<Box<dyn DataClient + Send>, String>
        + Send
        + Sync,
>;

/// The network table [`DataLaneBridge::new`] takes in production: [`build_client_on`] carrying the venues'
/// `venue_setting` rows.
pub fn real_network_table(venue_settings: BTreeMap<String, VenueSettings>) -> NetworkBuilder {
    Box::new(move |venue, network, sink| build_client_on(venue, network, sink, &venue_settings))
}

impl DataLaneBridge {
    /// The production lane: every arm of [`build_client`], with `venue_settings` threaded into the
    /// feeds that read a row.
    ///
    /// It hosts the tick pumps and serves the network of Hyperliquid's feed; the credentialed streams are
    /// added by [`DataLaneBridge::with_credentialed`] (they need the credential store, which a lane takes as
    /// a parameter).
    pub fn new(venue_settings: BTreeMap<String, VenueSettings>) -> Self {
        Self::over(real_client_table(venue_settings.clone()))
            .with_networks(real_network_table(venue_settings.clone()))
            .with_feed_ticks(FeedTickHub::new(feed_tick_table(venue_settings)))
    }

    /// A lane over any client table. A loopback test passes a scripted one; the advertisement stays
    /// the stream roster, so the double stands in for a bridge and for nothing else.
    pub fn over(clients: DataClientBuilder) -> Self {
        Self { clients, networked: None, pumps: None, credentialed: None }
    }

    /// This lane serving the network-taking venues through `networked`.
    pub fn with_networks(mut self, networked: NetworkBuilder) -> Self {
        self.networked = Some(networked);
        self
    }

    /// This lane hosting tick pumps through `pumps`.
    pub fn with_feed_ticks(mut self, pumps: Arc<FeedTickHub>) -> Self {
        self.pumps = Some(pumps);
        self
    }

    /// This lane serving the credentialed streams through `credentialed`.
    pub fn with_credentialed(mut self, credentialed: CredentialedStreamLane) -> Self {
        self.credentialed = Some(credentialed);
        self
    }
}

impl DataLane for DataLaneBridge {
    fn venues(&self) -> Vec<String> {
        owned(RosterLane::Stream)
    }

    fn open(
        &self,
        venue: &str,
        holder: &DataLaneHolder,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        tracing::debug!(
            session = holder.session,
            venue,
            "vike-gatewayhub data: building the venue client"
        );
        (self.clients)(venue, sink)
    }

    fn network_venues(&self) -> Vec<String> {
        if self.networked.is_some() { owned(RosterLane::StreamNetwork) } else { Vec::new() }
    }

    fn open_on(
        &self,
        venue: &str,
        network: &str,
        holder: &DataLaneHolder,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let Some(networked) = &self.networked else {
            return Err(format!(
                "gatewayhub: venue `{venue}` takes no network (asked for `{network}`)"
            ));
        };
        tracing::debug!(
            session = holder.session,
            venue,
            network,
            "vike-gatewayhub data: building the venue client for a named network"
        );
        networked(venue, network, sink)
    }

    fn feed_tick_venues(&self) -> Vec<String> {
        if self.pumps.is_some() { owned(RosterLane::Pump) } else { Vec::new() }
    }

    fn open_feed_tick(
        &self,
        venue: &str,
        holder: &DataLaneHolder,
        symbol: &str,
        tick_size: f64,
        sink: FeedTickSink,
    ) -> Result<FeedTickSubscription, DataClientError> {
        let Some(pumps) = &self.pumps else {
            return Err(DataClientError::Subscribe(format!(
                "gatewayhub: no tick pump is hosted for `{venue}` by this build"
            )));
        };
        tracing::debug!(
            session = holder.session,
            venue,
            symbol,
            tick_size,
            "vike-gatewayhub data: subscribing a session to a tick pump"
        );
        pumps.join_with(venue, symbol, tick_size, sink)
    }

    fn credentialed_venues(&self) -> Vec<String> {
        self.credentialed.as_ref().map_or_else(Vec::new, CredentialedStreamLane::venues)
    }

    fn open_credentialed(
        &self,
        venue: &str,
        holder: &DataLaneHolder,
        sink: Arc<dyn DataClientSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        let Some(lane) = &self.credentialed else {
            return Err(format!(
                "gatewayhub: no credentialed stream is hosted for `{venue}` by this build"
            ));
        };
        tracing::debug!(
            session = holder.session,
            venue,
            "vike-gatewayhub data: building a credentialed venue client"
        );
        lane.open_in_session(venue, holder.session, sink)
    }

    fn credentialed_feed_account(&self, venue: &str) -> Option<i64> {
        self.credentialed.as_ref()?.feed_account(venue)
    }
}

#[cfg(test)]
#[path = "venues_tests.rs"]
mod venues_tests;
