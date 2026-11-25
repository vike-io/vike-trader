//! **The shared market pump's knobs, READ from the venue tables**: [`row`] assembles one venue's
//! [`WebsocketSessionKnobs`] from its market row in `vike_venues::venue_connection` (whether the lane rides this
//! pump at all, its `driver`, and the `subscribe_frame` shape) and its market lane in `vike_venues::venue_timings`
//! (every duration, each with its venue's reason: decisions 0160 and 0298). Nothing here names a venue, so a tuning
//! change is one row edit in that table and a new pump venue is one row there, never an arm here.
//!
//! What stays here is the MECHANISM: the stop-flag poll [`READ_POLL`] and the bounded dial [`CONNECT_10S`], which no
//! venue has a reason for, and the one rule that turns a table lane into the driver's options
//! ([`WebsocketSessionKnobs::from_lanes`]). The venue keeps only its payloads (subscribe frame, keepalive text) and
//! its protocol closures, and builds its options through [`WebsocketSessionKnobs::opts`], so a timing has no second
//! copy to drift from.
//!
//! IBKR's Client Portal product is read by name ([`IBKR_CPAPI`]), as both tables read it, because [`row`] answers
//! `"ibkr"` with the socket product's row, which is not on the pump. (A venue's DEPTH ladder rides
//! [`crate::depth`]'s driver, whose knobs are its own `venue_timings` depth block, not these.)

use std::time::Duration;

use vike_venues::venue_connection::{VenueConnectionDriver, connection_for};
use vike_venues::venue_rate_limits::VenueRateLimitsMarket;
use vike_venues::venue_timings::{
    VenueTimingsLane, VenueTimingsReconnect, VenueTimingsValue, timings_for,
};

use crate::websocket_session::market::{
    WebsocketSessionMarketKeepalive, WebsocketSessionMarketOptions,
};

/// The driver knobs of one market-pump lane — the declarative half of [`WebsocketSessionMarketOptions`]
/// (everything but the venue's payload strings). See [`WebsocketSessionKnobs::opts`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebsocketSessionKnobs {
    /// `true`: the venue subscribes via a per-session frame (replayed verbatim each reconnect);
    /// `false`: the URL path itself carries the subscription (binance-style stream paths).
    pub subscribe_frame: bool,
    /// App-level keepalive cadence (`None` = the venue needs none on these streams). The payload
    /// TEXT stays in the venue crate — only the cadence is a knob.
    pub keepalive_every: Option<Duration>,
    /// Subscribe-ack watchdog window; `None` disables.
    pub ack_timeout: Option<Duration>,
    /// Silent-stall watchdog threshold; `None` disables.
    pub idle_threshold: Option<Duration>,
    /// Socket read timeout — the stop-flag poll cadence.
    pub read_timeout: Duration,
    /// The reconnect ladder after a session fault: the lane's row, as the table declares it.
    pub backoff: VenueTimingsReconnect,
    /// Bounded-TCP-connect dial window; `None` = plain `tungstenite::connect`.
    pub connect_timeout: Option<Duration>,
}

/// A declared timing's value.
const fn value_of(timing: Option<VenueTimingsValue>) -> Option<Duration> {
    match timing {
        Some(timing) => Some(timing.value),
        None => None,
    }
}

impl WebsocketSessionKnobs {
    /// **The one rule from a table lane to the driver's knobs**: the lane's keepalive, subscribe-acknowledgement
    /// watchdog, idle limit and reconnect as declared; its dial bound, else [`CONNECT_10S`]; and a read at
    /// [`READ_POLL`], except on a lane that declares a response-head wait (an HTTP body, oanda's pricing stream),
    /// whose per-read bound ureq also applies to that wait, so a 2 s poll would cut every slow open at 2 s: such a lane
    /// reads at its idle bound. `subscribe_frame` is the connection row's.
    #[must_use]
    pub const fn from_lanes(subscribe_frame: bool, lane: &VenueTimingsLane) -> Self {
        WebsocketSessionKnobs {
            subscribe_frame,
            keepalive_every: value_of(lane.keepalive_every),
            ack_timeout: value_of(lane.subscribe_acknowledgement_timeout),
            idle_threshold: value_of(lane.idle_threshold),
            read_timeout: if lane.response_timeout.is_some() {
                lane.idle_threshold()
            } else {
                READ_POLL
            },
            backoff: lane.reconnect(),
            connect_timeout: Some(match lane.dial_timeout {
                Some(dial) => dial.value,
                None => CONNECT_10S,
            }),
        }
    }

    /// Assemble the driver's [`WebsocketSessionMarketOptions`] from this row — the ONE way an on-driver venue
    /// builds its opts, so the row is consumed, never copied. The venue supplies only its payload
    /// strings: `subscribe` (the per-session frame, `None` for a URL-subscribed venue) and
    /// `keepalive_payload` (the ping text, `None` for a venue without one). Debug builds assert
    /// the payloads MATCH the row's declaration (a frame-subscribing row must get a frame; a
    /// keepalive cadence must get a payload and vice versa) so a venue cannot silently disagree
    /// with its own declared shape.
    #[must_use]
    pub fn opts<'a>(
        &self,
        subscribe: Option<&'a str>,
        keepalive_payload: Option<&'a str>,
    ) -> WebsocketSessionMarketOptions<'a> {
        debug_assert_eq!(
            subscribe.is_some(),
            self.subscribe_frame,
            "subscribe payload must match the row's subscribe_frame declaration"
        );
        debug_assert_eq!(
            keepalive_payload.is_some(),
            self.keepalive_every.is_some(),
            "keepalive payload must match the row's keepalive_every declaration"
        );
        WebsocketSessionMarketOptions {
            subscribe,
            keepalive: match (keepalive_payload, self.keepalive_every) {
                (Some(payload), Some(every)) => {
                    Some(WebsocketSessionMarketKeepalive { payload, every })
                }
                _ => None,
            },
            ack_timeout: self.ack_timeout,
            idle_threshold: self.idle_threshold,
            read_timeout: self.read_timeout,
            backoff: self.backoff,
            connect_timeout: self.connect_timeout,
            notice: None,
        }
    }
}

/// **The stop-flag poll cadence of a live socket read**: how long a feed thread can sit in one
/// `read()` before it looks at its stop flag again, and so how long a raised stop can wait on a quiet
/// socket. Every pump lane whose socket the driver reads uses it (2 s across the board); a lane that declares a
/// response-head wait (the one HTTP body on the driver, oanda's pricing stream) reads at its idle bound instead
/// ([`WebsocketSessionKnobs::from_lanes`] says why).
///
/// Public because it is a MECHANISM default and not a fact about any venue (decision 0160): the
/// seven per-venue `READ_TIMEOUT` copies in binance (two), bybit (two), okx (two) and alpaca's data
/// connection are this same number, and a bridge reads it from here rather than restating it. It is
/// therefore NOT a row of `vike_venues::venue_timings`, which holds only what has a venue's
/// reason behind it.
///
/// The recorder's stop budget is derived from it (see [`CONNECT_10S`]), so a LONGER poll here is a
/// direct debit against a `TimeoutStopSec=` on a live box.
pub const READ_POLL: Duration = Duration::from_secs(2);
/// **The bounded-dial window every pump lane runs with** — the knob that makes a feed
/// thread's mid-dial state answerable to the stop flag at all
/// ([`crate::websocket_session::market::WebsocketSessionMarketOptions::connect_timeout`]).
///
/// **Why every row.** `None` reads as "no special handling", but what it selects is plain
/// `tungstenite::connect`, which has no connect bound, so a feed thread whose SYNs are being
/// black-holed sits out the OS's own retransmit ladder — Linux's default `tcp_syn_retries = 6` is
/// SYNs at 0/1/3/7/15/31/63 s, so ~127 s before `connect()` even returns — with the stop flag
/// already raised behind it. That is a thread-liveness property, not a venue protocol quirk, so
/// there is nothing per-venue to decide and the answer is the same for all of them.
///
/// **WHAT IT COVERS: the whole TCP phase, and no more.** [`vike_transport::websocket_proxy::connect_websocket`]'s bounded
/// arm spends this ONE window across name resolution AND every address the name resolves to, in
/// order — deliberately, because a per-step bound would make the true ceiling `resolve + n × window`
/// for an `n` the venue's DNS picks (binance's stream hosts resolve to 6-8 addresses), and a
/// "ceiling" a DNS answer can multiply is not one. What it does NOT cover is the TLS + WS handshake
/// that follows — `connect_websocket` bounds that separately, each READ of it under this same window (so a
/// peer that accepts and then stalls costs one more window, and a trickling one several; the module
/// doc of `vike_transport::websocket_proxy` has the arithmetic) — nor anything a venue's own connect closure does
/// after the socket is up.
///
/// **Why ten seconds.** A healthy TCP handshake is one round trip — ~250 ms to binance's stream
/// hosts from the CI box. Ten seconds is ~40× that, and in SYN-ladder terms it admits the retransmits
/// at 1 s, 3 s and 7 s: a path that drops three consecutive SYNs still connects, and only a fourth
/// loss is judged dead. Sizing it near the healthy cost instead — say at the 2 s [`READ_POLL`] poll
/// cadence — would fail a single lost SYN, and a failed dial is disclosed, backed off and RETRIED,
/// so on a lossy path that trades a bounded wait for a reconnect storm against a venue that
/// rate-limits new connections.
///
/// **And the recorder's stop budget is derived from it.** `crates/vike-datahub/src/recorder.rs`'s
/// `FEED_STOP_BUDGET_SECS` is [`READ_POLL`] + the largest bounded dial on the roster. A LONGER window
/// here is not free — it is a direct debit against a `TimeoutStopSec=` on a live box.
///
/// **Public because the DEPTH driver spends it too.** [`crate::depth::run_depth`] is a separate
/// driver whose knobs this table does not declare — but a dial bound is not a protocol knob, it is
/// the same thread-liveness property with the same answer, and its venues are rows of the same
/// tables. One constant means the recorder's budget has ONE window to be derived from rather than
/// two that can drift apart.
pub const CONNECT_10S: Duration = Duration::from_secs(10);

/// **The knobs of `venue`'s market lane on the shared pump**, read from its rows (the module doc): one row for both
/// markets, which the tables declare alike for every pump venue (`spec_tests.rs` holds them equal). `venue` is the
/// canonical lowercase venue id (each crate's `VENUE` const).
///
/// # Panics
/// When the venue's market lane is not on the shared market pump (an own loop, a native session, none, or an unknown
/// venue): code that asks for a row its venue does not have is a static misclassification any test run catches.
#[must_use]
pub fn row(venue: &str) -> WebsocketSessionKnobs {
    let market = connection_for(venue, VenueRateLimitsMarket::Spot).market;
    assert!(
        market.driver == VenueConnectionDriver::MarketPump,
        "{venue}: its market lane is not on the shared market pump ({:?})",
        market.driver
    );
    WebsocketSessionKnobs::from_lanes(
        market.subscribe_frame,
        &timings_for(venue, VenueRateLimitsMarket::Spot).market(),
    )
}

/// **IBKR's Client Portal backend** (`vike_venues::IbkrBackend::Cpapi`, not deployed): its market lane's knobs, read by
/// name as the two tables read the product (`vike_venues::venue_timings::IBKR_CPAPI`,
/// `vike_venues::venue_connection::IBKR_CPAPI`), because [`row`] answers `"ibkr"` with the socket product's.
///
/// vike-ibkr feed/cpapi/lane.rs's `run`: ONE gateway socket per data client, every subscription the `smd` topic of
/// its conId, on `run_market_on` with the lane's own stream. Every knob is the rows' through
/// [`WebsocketSessionKnobs::from_lanes`] except the read timeout, the lane's 200 ms command and stop poll (a mechanism
/// value, as [`READ_POLL`] is, kept as the lane's own loop ran it: an `unsubscribe` waits 2 s for its answer).
pub const IBKR_CPAPI: WebsocketSessionKnobs = WebsocketSessionKnobs {
    read_timeout: Duration::from_millis(200),
    ..WebsocketSessionKnobs::from_lanes(
        vike_venues::venue_connection::IBKR_CPAPI.market.subscribe_frame,
        &vike_venues::venue_timings::IBKR_CPAPI.market(),
    )
};

#[path = "spec_tests.rs"]
#[cfg(test)]
mod spec_tests;
