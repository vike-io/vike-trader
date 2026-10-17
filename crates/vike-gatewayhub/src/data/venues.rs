//! The data lane's VENUE TABLE — the ONE function in this daemon that constructs a venue market-data
//! client ([`build_client`]) — and the [`DataLane`] built over it ([`BridgeData`]).
//!
//! The arms are the datahub's `build_client` arms, MOVED: the same constructors, the same
//! `venue_setting` reads (decision 0095), the same refusal for a venue outside the table. The datahub
//! still builds its own clients, from `EXPLICIT_SHARING` in `crates/vike-datahub/src/feeds/venues.rs`,
//! until it is rewired onto `vike_market_stream::market::RemoteDataClient`; the sharing column
//! (`Shared`/`PerHolder`) is a consumer-side rule and stays there, so the lane builds exactly ONE
//! client per stream socket whatever the venue (`crate::server::stream`).
//!
//! # The shape: one plain arm per venue
//!
//! The `data` feature links every bridge (`crates/vike-gatewayhub/Cargo.toml`), so unlike the
//! datahub's table there is no `#[cfg(feature)]` arm and no `#[cfg(not)]` twin: a venue is either in
//! the table or it is not. A venue the table does not know is refused naming the table
//! ([`crate::registry::Lane::Stream`]), never a feature to rebuild with, because there is none to name.
//! The arms and the roster are held equal both ways by this file's tests, so a venue cannot be built
//! here and left unadvertised (or advertised and unbuilt) by forgetting one of two lists.
use std::collections::BTreeMap;
use std::sync::Arc;

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, StreamLabel, SubscriptionId};
use vike_model::AccountTier;
// The one `mark_streams` reader, shared with the trading daemon's feed wiring.
use vike_secrets::venue_setting::{VenueSettings, mark_streams_setting};

use vike_gatewayhub_client::proto::{DATA_NETWORK_MAINNET, DATA_NETWORK_TESTNET};
use vike_hyperliquid::base::config::Network;

use super::credentialed::CredentialedLane;
use super::pump::{PumpHub, real_pump_table};
use super::{DataLane, Holder, PumpLease, PumpSink};
use crate::registry::{Lane, owned, roster};

/// The tier the aster feed is built on: `Live`, which `vike_aster::urls::urls_for` resolves to the
/// mainnet public market-data hosts (owner ruling 7(a), decision 0164; the arm in [`build_client`]
/// says why). It names a NETWORK, not an account: no credential is read for it.
const ASTER_FEED_TIER: AccountTier = AccountTier::Live;

/// Build one venue's market-data client, or say WHY not.
///
/// ⚠ Every constructor is the one the datahub's market-data plane called before the broker existed,
/// verbatim — plus, since decision 0095, the venue's own `venue_setting` rows out of `venue_settings`
/// (Polymarket's socket batching, each CEX feed's mark-stream row). The `|| {}` is the GUI repaint
/// nudge — a headless caller passes an empty closure, exactly as the recorder does.
pub fn build_client(
    venue: &str,
    sink: Arc<dyn LiveDataSink>,
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Result<Box<dyn DataClient + Send>, String> {
    match venue {
        "binance" => Ok(Box::new(
            vike_binance::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "binance")),
        )),

        "bybit" => Ok(Box::new(
            vike_bybit::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "bybit")),
        )),

        "okx" => Ok(Box::new(
            vike_okx::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "okx")),
        )),

        // ⚠ OWNER RULING 7(a) of decision 0164, the one behaviour change of wave 1: this feed is built
        // on MAINNET. `Feeds::new` defaults the tier to `AccountTier::Demo` (the testnet hosts), which
        // is what the datahub's aster feed was, while the datahub's aster backfill and catalog and
        // the tradehub's aster feed are mainnet and keyless market data is mainnet at every tier for
        // the other CEXs. The tier picks HOSTS only (`vike_aster::urls::urls_for`): this arm reads no
        // credential and the feed touches no authenticated endpoint, so a box whose aster account is
        // `live` is not asked for, and does not spend, a key by serving this feed.
        "aster" => Ok(Box::new(
            vike_aster::market_feed::Feeds::with_env(sink, || {}, ASTER_FEED_TIER)
                .with_mark_streams(mark_streams_setting(venue_settings, "aster")),
        )),

        // ⚠ MAINNET, on purpose and by the feed's default: this table takes no network, so a session that
        // names none (`DataOpen { network: None }`) is fed the main network, as the datahub's market-data
        // arm always was. The bridge maps a `demo` AND a `paper` account to the testnet, so a caller that
        // wants the test network MUST say so: `build_client_on` is the only door that builds it.
        "hyperliquid" => Ok(Box::new(
            vike_hyperliquid::feeds::market_feed::Feeds::new(sink, || {})
                .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid")),
        )),

        "polymarket" => {
            // K — `venue.polymarket.ws_tokens_per_socket` (decision 0095), or the bridge's default.
            let k = vike_polymarket::tokens_per_socket(|field| {
                venue_settings
                    .get("polymarket")
                    .and_then(|s| s.get(vike_secrets::venue_setting::SettingTier::Any, field))
                    .map(str::to_string)
            });
            Ok(Box::new(vike_polymarket::Feeds::new(sink, || {}).with_tokens_per_socket(k)))
        }

        // Deribit reads NO `venue_setting` row (its marks stream chain-wide through the options
        // feed, not a per-bars companion socket), so unlike every arm above it hands the feed
        // nothing but the sink.
        // ...but its bar lane refuses an interval the venue has no resolution code for, which its feed
        // itself does not (`BarIntervalGate`).
        "deribit" => Ok(Box::new(BarIntervalGate {
            inner: Box::new(vike_deribit::market_feed::Feeds::new(sink, || {})),
            check: deribit_interval,
        })),

        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            roster(Lane::Stream).join(", ")
        )),
    }
}

/// A venue client whose `subscribe_bars` first asks the venue's own interval table, so an interval the
/// venue has no series for is a refused subscription and not a bar lane that spawns, sets a status line and
/// returns (a silent dead lane: closed bars are what fire a strategy's per-bar watchdogs).
///
/// The tradehub made this refusal itself, at mount (`venue_plan.rs`'s `deribit_plan`, `oanda_plan`,
/// `ig_plan`); its feeds became strategy-lane dials in wave 2, so the refusal lives where the venue client
/// is built. OANDA's and IG's clients refuse inside `subscribe_bars` already; deribit's does not.
struct BarIntervalGate {
    inner: Box<dyn DataClient + Send>,
    /// `Err` is the refusal text for an interval the venue cannot serve.
    check: fn(&str) -> Result<(), String>,
}

impl DataClient for BarIntervalGate {
    fn subscribe_bars(
        &mut self,
        symbol: &str,
        interval: &str,
    ) -> Result<SubscriptionId, LiveDataError> {
        (self.check)(interval).map_err(LiveDataError::Subscribe)?;
        self.inner.subscribe_bars(symbol, interval)
    }

    fn subscribe_quotes(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.inner.subscribe_quotes(symbol)
    }

    fn subscribe_trades(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.inner.subscribe_trades(symbol)
    }

    fn subscribe_book(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.inner.subscribe_book(symbol)
    }

    fn subscribe_depth(&mut self, symbol: &str) -> Result<SubscriptionId, LiveDataError> {
        self.inner.subscribe_depth(symbol)
    }

    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.inner.unsubscribe(id);
    }

    fn begin_shutdown(&mut self) {
        self.inner.begin_shutdown();
    }

    fn set_delivery(
        &mut self,
        symbol: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.inner.set_delivery(symbol, lane, class)
    }

    fn is_dead(&self, id: SubscriptionId) -> bool {
        self.inner.is_dead(id)
    }

    fn shutdown(&mut self) {
        self.inner.shutdown();
    }
}

/// Deribit's refusal of an interval its `chart.trades` resolution table has no code for: the tradehub's
/// `deribit_plan` text, verbatim. The table has gaps (no 4h, no weekly, no monthly).
pub fn deribit_interval(interval: &str) -> Result<(), String> {
    vike_deribit::data::resolution_code(interval).map(|_| ()).map_err(|e| {
        format!(
            "deribit streams `chart.trades` by RESOLUTION code and has none for interval {interval:?} \
             ({e} — `vike_deribit::data::resolution_code` is the table, and its enum has gaps: \
             there is no 4h), so the bar lane's thread would set a status line and exit — and \
             closed bars are what fire the margin-call watchdog and the drawdown latch. Name an \
             interval this venue serves"
        )
    })
}

/// Build the market-data client of a venue that takes a NETWORK, for the network a strategy-lane session
/// named, or say WHY not (wave 2, W10).
///
/// Only Hyperliquid's feed follows the account tier (the CEX feeds take no network and aster is pinned
/// `Live`), so it is the one arm: `testnet` and `mainnet` are `Network::Testnet` / `Network::Mainnet`
/// through the same `Feeds::with_network` call the tradehub's `wire_hyperliquid_feed` made, MOVED, with
/// the venue's `mark_streams` row. [`build_client`] is not touched: it is the datahub's market-data
/// arm and stays the main network. A network word outside the two is refused, never read as the main one.
pub fn build_client_on(
    venue: &str,
    network: &str,
    sink: Arc<dyn LiveDataSink>,
    venue_settings: &BTreeMap<String, VenueSettings>,
) -> Result<Box<dyn DataClient + Send>, String> {
    if venue != "hyperliquid" {
        return Err(format!(
            "gatewayhub: venue `{venue}` takes no network (asked for `{network}`)"
        ));
    }
    let network = match network {
        DATA_NETWORK_MAINNET => Network::Mainnet,
        DATA_NETWORK_TESTNET => Network::Testnet,
        other => {
            return Err(format!(
                "gatewayhub: unknown network `{other}` for hyperliquid (the words are \
                 `{DATA_NETWORK_MAINNET}` and `{DATA_NETWORK_TESTNET}`)"
            ));
        }
    };
    Ok(Box::new(
        vike_hyperliquid::feeds::market_feed::Feeds::new(sink, || {})
            .with_network(network)
            .with_mark_streams(mark_streams_setting(venue_settings, "hyperliquid")),
    ))
}

/// How the lane obtains one venue client. Same shape as the datahub's broker table, so a loopback
/// test hands the lane a scripted client for a bridge.
pub type ClientBuilder = Box<
    dyn Fn(&str, Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, String> + Send + Sync,
>;

/// The table [`BridgeData::new`] takes in production — carrying every venue's `venue_setting` rows,
/// which the clients read (decision 0095).
pub fn real_client_table(venue_settings: BTreeMap<String, VenueSettings>) -> ClientBuilder {
    Box::new(move |venue, sink| build_client(venue, sink, &venue_settings))
}

/// The [`DataLane`] over the venue bridges: [`DataLane::venues`] is the stream roster and
/// [`DataLane::open`] is [`build_client`] for the session the server opened.
///
/// The `venue_settings` map is the lane's constructor PARAMETER, not something it reads: a library
/// takes configuration as parameters and only the binary reads the store
/// (`vike_secrets::venue_setting::load_venue_settings`, the daemon's `run`).
pub struct BridgeData {
    clients: ClientBuilder,
    /// The network-taking venues' builder; `None` advertises no `stream_network`.
    networked: Option<NetworkBuilder>,
    /// The shared tick pumps; `None` advertises no `gw_pump`.
    pumps: Option<Arc<PumpHub>>,
    /// The credentialed streams; `None` advertises no `cs_venue=`.
    credentialed: Option<CredentialedLane>,
}

/// How the lane builds a client for a venue on a named network.
pub type NetworkBuilder = Box<
    dyn Fn(&str, &str, Arc<dyn LiveDataSink>) -> Result<Box<dyn DataClient + Send>, String>
        + Send
        + Sync,
>;

/// The network table [`BridgeData::new`] takes in production: [`build_client_on`] carrying the venues'
/// `venue_setting` rows.
pub fn real_network_table(venue_settings: BTreeMap<String, VenueSettings>) -> NetworkBuilder {
    Box::new(move |venue, network, sink| build_client_on(venue, network, sink, &venue_settings))
}

impl BridgeData {
    /// The production lane: every arm of [`build_client`], with `venue_settings` threaded into the
    /// feeds that read a row.
    ///
    /// It hosts the tick pumps and serves the network of Hyperliquid's feed; the credentialed streams are
    /// added by [`BridgeData::with_credentialed`] (they need the credential store, which a lane takes as
    /// a parameter).
    pub fn new(venue_settings: BTreeMap<String, VenueSettings>) -> Self {
        Self::over(real_client_table(venue_settings.clone()))
            .with_networks(real_network_table(venue_settings))
            .with_pumps(PumpHub::new(real_pump_table()))
    }

    /// A lane over any client table. A loopback test passes a scripted one; the advertisement stays
    /// the stream roster, so the double stands in for a bridge and for nothing else.
    pub fn over(clients: ClientBuilder) -> Self {
        Self { clients, networked: None, pumps: None, credentialed: None }
    }

    /// This lane serving the network-taking venues through `networked`.
    pub fn with_networks(mut self, networked: NetworkBuilder) -> Self {
        self.networked = Some(networked);
        self
    }

    /// This lane hosting tick pumps through `pumps`.
    pub fn with_pumps(mut self, pumps: Arc<PumpHub>) -> Self {
        self.pumps = Some(pumps);
        self
    }

    /// This lane serving the credentialed streams through `credentialed`.
    pub fn with_credentialed(mut self, credentialed: CredentialedLane) -> Self {
        self.credentialed = Some(credentialed);
        self
    }
}

impl DataLane for BridgeData {
    fn venues(&self) -> Vec<String> {
        owned(Lane::Stream)
    }

    fn open(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        tracing::debug!(
            session = holder.session,
            venue,
            "vike-gatewayhub data: building the venue client"
        );
        (self.clients)(venue, sink)
    }

    fn network_venues(&self) -> Vec<String> {
        if self.networked.is_some() { owned(Lane::StreamNetwork) } else { Vec::new() }
    }

    fn open_on(
        &self,
        venue: &str,
        network: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
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

    fn pump_venues(&self) -> Vec<String> {
        if self.pumps.is_some() { owned(Lane::Pump) } else { Vec::new() }
    }

    fn open_pump(
        &self,
        venue: &str,
        holder: &Holder,
        symbol: &str,
        tick_size: f64,
        sink: PumpSink,
    ) -> Result<PumpLease, String> {
        let Some(pumps) = &self.pumps else {
            return Err(format!("gatewayhub: no tick pump is hosted for `{venue}` by this build"));
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
        self.credentialed.as_ref().map_or_else(Vec::new, CredentialedLane::venues)
    }

    fn open_credentialed(
        &self,
        venue: &str,
        holder: &Holder,
        sink: Arc<dyn LiveDataSink>,
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
        lane.open(venue, sink)
    }
}

#[cfg(test)]
#[path = "venues_tests.rs"]
mod venues_tests;
