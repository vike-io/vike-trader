//! What the daemon tells a client at `Hello`: the `Welcome.features` list, and the server's leg of
//! decision 0112 (a verb whose string is not advertised is refused BY NAME, on a connection that
//! survives).
//!
//! # ⚠ The advertisement is a fact about the BUILD, never about a venue's liveness
//!
//! [`served_features`] is a pure function of two lane declarations, `DataLane::venues` and
//! `RestLane::facts`, and `crate::server::accept::serve` calls it ONCE at start. A venue that is down
//! is a verb's own failure (a `SubscribeError`, a `FetchFailed`), never a missing string, so a client that
//! reads the handshake learns what the daemon can do, and a transient outage cannot make the
//! datahub's capability check refuse a lane it would have served a second later (drift register R5).
//! The golden in `features_tests.rs` pins the exact list.
//!
//! # The strings
//!
//! The vocabulary is `vike_gatewayhub_client::proto::KNOWN_FEATURES` (sentinel first). The daemon
//! advertises a string exactly when it serves the verb it gates:
//!
//! * the plane sentinel and `gateway_status` always (the server answers `GatewayStatus` itself), and
//!   `wire_msgpack` always: every connection of this build reads either body and answers in the request's
//!   (`crate::server::connection`'s module doc);
//! * `gateway_data` and one `market_data_venue=` per venue when a data lane serves any venue, and `stream_class`,
//!   `stream_funding` and `stream_binary_ticks` with them (the delivery class of a stream subscription, the funding
//!   frames and the binary tick bodies a session asks for are properties of the stream plane);
//! * `gateway_fetch` when klines or funding are served, `gateway_ticks`, `gateway_catalog`, `gateway_options` (and
//!   `gateway_options_listing` and `gateway_option_trades` with it), `gateway_family`, `gateway_market_rules`
//!   (and `gateway_market_rules_series` with it) and `gateway_presence` each when their lane is, and
//!   `gateway_fetch_credentialed` with one `credentialed_data_venue=` per credentialed venue;
//! * `gateway_fetch_progress` and `gateway_fetch_cancel` when ANY fetch lane is served: they are the contract
//!   of the fetch machinery itself (an immediate progress frame and a cadence; a cancel on the same
//!   connection), and a client refuses a fetch without the first.

use vike_gatewayhub_client::proto::{
    FEATURE_GATEWAY_CATALOG, FEATURE_GATEWAY_DATA, FEATURE_GATEWAY_FAMILY,
    FEATURE_GATEWAY_FEED_TICK, FEATURE_GATEWAY_FETCH, FEATURE_GATEWAY_FETCH_CANCEL,
    FEATURE_GATEWAY_FETCH_CREDENTIALED, FEATURE_GATEWAY_FETCH_PROGRESS,
    FEATURE_GATEWAY_MARKET_RULES, FEATURE_GATEWAY_MARKET_RULES_SERIES,
    FEATURE_GATEWAY_OPTION_TRADES, FEATURE_GATEWAY_OPTIONS, FEATURE_GATEWAY_OPTIONS_LISTING,
    FEATURE_GATEWAY_PRESENCE, FEATURE_GATEWAY_STREAM_BINARY_TICKS, FEATURE_GATEWAY_STREAM_CLASS,
    FEATURE_GATEWAY_STREAM_FUNDING, FEATURE_GATEWAY_STREAM_NETWORK, FEATURE_GATEWAY_TICKS,
    FetchWhat, GATEWAYHUB_PLANE_SENTINEL, KNOWN_TRADE_FEATURES, credentialed_data_venue_feature,
    cs_venue_feature, trade_venue_feature,
};
use vike_hub_protocol::features::{
    FEATURE_GATEWAY_STATUS, FEATURE_WIRE_MSGPACK, market_data_venue_feature,
};

use crate::data::DataLane;
use crate::rest::RestRosters;

/// The `Welcome.features` of a daemon serving `data_venues` and `rest`, in `KNOWN_FEATURES`' order
/// followed by the `market_data_venue=` entries and the `credentialed_data_venue=` entries in lane order.
pub fn served_features(data_venues: &[String], rest: &RestRosters) -> Vec<String> {
    let mut f: Vec<String> = vec![GATEWAYHUB_PLANE_SENTINEL.to_string()];
    let mut push = |on: bool, feature: &str| {
        if on {
            f.push(feature.to_string());
        }
    };
    push(!data_venues.is_empty(), FEATURE_GATEWAY_DATA);
    push(!data_venues.is_empty(), FEATURE_GATEWAY_STREAM_CLASS);
    push(!data_venues.is_empty(), FEATURE_GATEWAY_STREAM_FUNDING);
    push(!data_venues.is_empty(), FEATURE_GATEWAY_STREAM_BINARY_TICKS);
    push(!rest.klines.is_empty() || !rest.funding.is_empty(), FEATURE_GATEWAY_FETCH);
    push(rest.serves_any_fetch(), FEATURE_GATEWAY_FETCH_PROGRESS);
    push(rest.serves_any_fetch(), FEATURE_GATEWAY_FETCH_CANCEL);
    push(!rest.credentialed.is_empty(), FEATURE_GATEWAY_FETCH_CREDENTIALED);
    push(!rest.ticks.is_empty(), FEATURE_GATEWAY_TICKS);
    push(!rest.catalog.is_empty(), FEATURE_GATEWAY_CATALOG);
    push(rest.options, FEATURE_GATEWAY_OPTIONS);
    push(rest.options, FEATURE_GATEWAY_OPTIONS_LISTING);
    push(rest.options, FEATURE_GATEWAY_OPTION_TRADES);
    push(!rest.family.is_empty(), FEATURE_GATEWAY_FAMILY);
    push(!rest.market_rules.is_empty(), FEATURE_GATEWAY_MARKET_RULES);
    push(!rest.market_rules.is_empty(), FEATURE_GATEWAY_MARKET_RULES_SERIES);
    push(rest.presence, FEATURE_GATEWAY_PRESENCE);
    push(true, FEATURE_GATEWAY_STATUS);
    push(true, FEATURE_WIRE_MSGPACK);
    f.extend(data_venues.iter().map(|v| market_data_venue_feature(v)));
    f.extend(rest.credentialed.iter().map(|v| credentialed_data_venue_feature(v)));
    f
}

/// The strategy lane's strings (wave 2, W10), appended AFTER [`served_features`]' list: `stream_network`
/// when any venue takes a network on `DataOpen`, `gateway_feed_tick` when a tick pump is hosted, then one
/// `cs_venue=` per credentialed-stream venue, in lane order. Build facts, like every other string here.
pub fn strategy_lane_features(data: &dyn DataLane) -> Vec<String> {
    let mut f: Vec<String> = Vec::new();
    if !data.network_venues().is_empty() {
        f.push(FEATURE_GATEWAY_STREAM_NETWORK.to_string());
    }
    if !data.feed_tick_venues().is_empty() {
        f.push(FEATURE_GATEWAY_FEED_TICK.to_string());
    }
    f.extend(data.credentialed_venues().iter().map(|v| cs_venue_feature(v)));
    f
}

/// The `Welcome.features` a daemon that serves the TRADE class appends to [`served_features`]: the eight
/// fixed strings of `KNOWN_TRADE_FEATURES` in their order, then one `trade_venue=<slug>` per venue whose
/// adapter the build carries, in lane order. Separate from [`served_features`] on purpose: that list is the
/// DATA vocabulary (`KNOWN_FEATURES`, which the client crate pins a full data build equal to), /// trade strings are advertised only when a trade lane is mounted AND the Trade key is held
/// (`crate::server::accept`).
///
/// The strings are armed TOGETHER: the lane serves the resume cursor, the by-order-client-id resolution, the wire-id
/// prefetch, the lease, the arming table, the adapter start and the preflight as one class, so a client
/// that finds `gateway_trade` finds the rest.
pub fn trade_features(trade_venues: &[String]) -> Vec<String> {
    let mut f: Vec<String> = KNOWN_TRADE_FEATURES.iter().map(|s| (*s).to_string()).collect();
    f.extend(trade_venues.iter().map(|v| trade_venue_feature(v)));
    f
}

/// The refusal text for a verb the daemon does not serve: it names the verb and the string a client
/// would have seen advertised, and it is answered as a positional `Response::Error` on a connection
/// that stays open (0112's third leg).
pub fn not_served(verb: &str, feature: &str) -> String {
    format!(
        "gatewayhub: {verb} is not served by this daemon build ({feature:?} is not advertised in \
         its Welcome); nothing was asked of any venue"
    )
}

/// The refusal text for a lane that is not armed although the string gating its FAMILY is advertised
/// (a sibling lane arms it: klines and funding share `gateway_fetch`). It must not claim the string is
/// missing, because it is not.
pub fn lane_not_armed(verb: &str, feature: &str) -> String {
    format!(
        "gatewayhub: {verb} is not served by this daemon build: its lane is not armed (the \
         {feature:?} family serves other lanes); nothing was asked of any venue"
    )
}

/// Which lane's roster a `FetchWhat` is judged against, and the string that gates it.
pub fn fetch_gate(what: &FetchWhat, credentialed: bool) -> &'static str {
    if credentialed { FEATURE_GATEWAY_FETCH_CREDENTIALED } else { what.feature() }
}

/// Whether `rest` serves the lane `what` names (the string-level question: is the lane armed at
/// all), not whether it serves the venue.
pub fn fetch_lane_armed(rest: &RestRosters, what: &FetchWhat, credentialed: bool) -> bool {
    if credentialed {
        return !rest.credentialed.is_empty();
    }
    match what {
        FetchWhat::Klines { .. } => !rest.klines.is_empty(),
        FetchWhat::Funding { .. } => !rest.funding.is_empty(),
        FetchWhat::Ticks { .. } => !rest.ticks.is_empty(),
        FetchWhat::CatalogList { .. } | FetchWhat::CatalogSearch { .. } => !rest.catalog.is_empty(),
        FetchWhat::OptionIndex { .. } | FetchWhat::OptionTrades { .. } => rest.options,
        FetchWhat::ClosedMarketRules { .. }
        | FetchWhat::SeriesMarketRules { .. }
        | FetchWhat::MarketRulesOfToken { .. } => !rest.market_rules.is_empty(),
    }
}

/// The venue a fetch asks about and the roster it must be in (`None` for the option index and the option
/// trades, which name no venue: they are the options surface's). Exhaustive over `FetchWhat`.
pub fn fetch_roster<'a>(
    rest: &'a RestRosters,
    what: &'a FetchWhat,
    credentialed: bool,
) -> Option<(&'a str, &'a [String])> {
    if credentialed {
        // The operator's lookup rides the venues whose credential this daemon holds, the klines lane's roster.
        return match what {
            FetchWhat::Klines { venue, .. } | FetchWhat::CatalogSearch { venue, .. } => {
                Some((venue.as_str(), rest.credentialed.as_slice()))
            }
            _ => None,
        };
    }
    match what {
        FetchWhat::Klines { venue, .. } => Some((venue.as_str(), rest.klines.as_slice())),
        FetchWhat::Funding { venue, .. } => Some((venue.as_str(), rest.funding.as_slice())),
        FetchWhat::Ticks { venue, .. } => Some((venue.as_str(), rest.ticks.as_slice())),
        FetchWhat::CatalogList { venue } | FetchWhat::CatalogSearch { venue, .. } => {
            Some((venue.as_str(), rest.catalog.as_slice()))
        }
        FetchWhat::OptionIndex { .. } | FetchWhat::OptionTrades { .. } => None,
        FetchWhat::ClosedMarketRules { venue, .. }
        | FetchWhat::SeriesMarketRules { venue, .. }
        | FetchWhat::MarketRulesOfToken { venue, .. } => {
            Some((venue.as_str(), rest.market_rules.as_slice()))
        }
    }
}

#[cfg(test)]
#[path = "features_tests.rs"]
mod features_tests;
