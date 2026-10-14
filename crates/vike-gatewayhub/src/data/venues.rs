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

use vike_data::{DataClient, LiveDataSink};
use vike_model::AccountTier;
// The one `mark_streams` reader, shared with the trading daemon's feed wiring.
use vike_secrets::venue_setting::{VenueSettings, mark_streams_setting};

use super::{DataLane, Holder};
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
        "deribit" => Ok(Box::new(vike_deribit::market_feed::Feeds::new(sink, || {}))),

        other => Err(format!(
            "feed broker: venue `{other}` has no client in this process's venue table. Table: [{}]",
            roster(Lane::Stream).join(", ")
        )),
    }
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
}

impl BridgeData {
    /// The production lane: every arm of [`build_client`], with `venue_settings` threaded into the
    /// feeds that read a row.
    pub fn new(venue_settings: BTreeMap<String, VenueSettings>) -> Self {
        Self::over(real_client_table(venue_settings))
    }

    /// A lane over any client table. A loopback test passes a scripted one; the advertisement stays
    /// the stream roster, so the double stands in for a bridge and for nothing else.
    pub fn over(clients: ClientBuilder) -> Self {
        Self { clients }
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
}

#[cfg(test)]
#[path = "venues_tests.rs"]
mod venues_tests;
