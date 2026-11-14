//! Remote HISTORY sources — `KlineSource`, `FundingRateSource` and the dukascopy tick fetch, answered
//! by the gatewayhub over `Fetch`.
//!
//! The datahub keeps everything that touches its store (the commit key, the still-forming-candle
//! guard, the append, the chunk grid, the cancel registry); only the VENUE half of a backfill moved
//! to the daemon. These types are that half's remote handle:
//!
//! * [`KlineSourceGateway`] / [`FundingRateSourceGateway`] implement `vike_data::source::{KlineSource,
//!   FundingRateSource}`, so the datahub's backfill table folds them exactly as it folded the bridge
//!   statics — over the two rosters [`KLINE_VENUES`] and [`FUNDING_VENUES`], in the SAME ORDER.
//! * [`GatewayTicks`] is the dukascopy `.bi5` range fetch, shaped like the closure
//!   `vike_backfill::venues::dukascopy::backfill_quotes_then_bars` takes (`FnMut(&str, i64, i64) ->
//!   Result<Vec<TickQuote>, String>`); [`GatewayTicks::fetcher`] builds it around the request's stop
//!   probe.
//!
//! Beside the history rosters sit the strategy lane's two stream rosters, [`FEED_TICK_VENUES`] and
//! [`NETWORK_VENUE`]: the gatewayhub's lane roster names them, the datahub refuses by them, and neither
//! daemon may link the other, so this crate below both is their one home.
//!
//! # A source is a REQUEST: one connection for its life
//!
//! A source is built PER REQUEST (it may hold the request's stop probe) and holds ONE lazily dialled
//! connection across all of its fetches until it is dropped. That is the remote twin of the
//! in-process source the datahub builds around a read-once credential memo: the daemon keeps its
//! per-connection state (the OANDA practice token is read from the store once per CONNECTION and
//! dropped with it) exactly as long as the request lives, so a chunked ingest of thousands of day
//! fetches reads the store once, not once per day. A held connection the daemon reaped while it
//! idled is detected before any reply frame and repeated ONCE on a fresh one
//! ([`GatewayClient::heard_a_reply`]); a failure after the daemon began answering is the fetch's
//! answer and is never repeated, because the venue may have been spent, and neither is a TIMEOUT
//! ([`GatewayClient::timed_out`]): a daemon that went quiet on the request may be working on it.
//!
//! # The error contract (drift register R12)
//!
//! * `SourceError::{Fetch, Refused}` round-trip with IDENTICAL inner text: the daemon's failure
//!   frame carries the variant and the source's own words.
//! * **A transport failure is `Fetch("gatewayhub ...")`, NEVER `Refused`, and never an empty `Vec`.**
//!   `Refused` means "the request cannot be expressed, nothing was asked of the venue"; a dead link
//!   says nothing about that. An empty `Vec` would be stored as "the venue had no rows" and spend the
//!   commit key for good.
//!
//! # Cancellation (wire design B.6)
//!
//! The sources may hold the request's stop probe. While a fetch waits the client polls it every
//! `GatewayTimings::fetch_probe_slice`; on `true` it sends `FetchCancel`, drops the connection and
//! returns `Fetch("gatewayhub fetch cancelled ...")` — faster than a chunk boundary. ONLY the
//! stoppable lanes (the tick lane and the credentialed klines lane) should be handed a probe; the
//! one-batch lanes ignore it, which keeps backfill cancellation identical in wave 1. ⚠ A cancel that
//! lands inside a fetch surfaces as that chunk's fetch ERROR, where the in-process path would have
//! stopped cleanly at the next boundary: the caller that passes a probe owns that difference.

use std::sync::{Arc, Mutex, PoisonError};

use vike_data::source::{FundingRateSource, KlineSource, SourceError};
use vike_hub_protocol::auth::Scope;
use vike_marketdata::FundingRatePoint;
use vike_marketdata::{Bar, TickQuote};

use crate::client::{FetchError, FetchReply, GatewayClient, GatewayLink, gateway_text};
use crate::proto::FetchWhat;

/// A stop probe an owned source can hold: `true` means "stop now" and, once it has said so, keeps
/// saying so.
pub type StopProbe = Arc<dyn Fn() -> bool + Send + Sync>;

/// The venues the kline lane serves, in the order the datahub's backfill table folds them.
///
/// ⚠ **ORDER IS OBSERVABLE.** It is what the datahub's unknown-venue refusal prints and what the
/// `Welcome` advertisement preserves, and it is the order of the datahub's backfill
/// registry `KLINE_SOURCES` at the time the venue calls moved here: binance, bybit, okx, aster, deribit,
/// hyperliquid, then fxcm (appended: FXCM's keyless candle ARCHIVE, `vike_fxcm::history::KlineSourceFxcm`).
/// The gatewayhub's own `KLINE_SOURCES` must equal this in order (a test there pins it), and
/// `kline_roster_order_is_the_datahubs` below pins the order here.
// ⚠ The comment inside the literal is on purpose: it makes rustfmt keep `= [` on the declaration's line, which
// `crates/vike-ops/tests/venues/collector_dispatch_gate.rs`'s `roster_from` reads (rustfmt would otherwise
// wrap seven names onto the next line after the `=`, where the scan finds the NEXT roster's `= [` instead).
pub const KLINE_VENUES: [&str; 7] = [
    "binance",
    "bybit",
    "okx",
    "aster",
    "deribit",
    "hyperliquid",
    // appended last: FXCM's keyless candle archive
    "fxcm",
];

/// The venues the funding lane serves, in the order the datahub's backfill table folds them — the
/// order of the datahub's backfill registry `FUNDING_SOURCES` at the time the venue calls moved
/// (binance, hyperliquid), then bybit, okx, deribit and aster appended. ORDER IS OBSERVABLE; see
/// [`KLINE_VENUES`].
// ⚠ The comment inside the literal keeps `= [` on the declaration's line, as on `KLINE_VENUES` above.
pub const FUNDING_VENUES: [&str; 6] = [
    "binance",
    "hyperliquid",
    // appended: bybit, okx, deribit and aster
    "bybit",
    "okx",
    "deribit",
    "aster",
];

/// The venue of the tick lane: dukascopy's keyless `.bi5` archive.
pub const TICK_VENUE: &str = "dukascopy";

/// The venues whose feed is a CEX tick PUMP (`spawn_*_market_data`): the only ones the strategy lane's
/// `Ticks` op is asked of. The gatewayhub hosts a pump for each (its `Lane::Pump` roster names this const),
/// the datahub's strategy lane refuses `Ticks` for any other venue by this list, and the tradehub's
/// `StrategyLanePlanCex` is held equal to it. ORDER IS OBSERVABLE: the refusal prints the list.
pub const FEED_TICK_VENUES: [&str; 4] = ["binance", "bybit", "okx", "aster"];

/// The one venue whose feed follows the account tier, so the one whose stream takes a network on `DataOpen`
/// (`stream_network`; the gatewayhub's `Lane::StreamNetwork` roster names this const). The datahub refuses a
/// network for any other venue rather than ignore it.
pub const NETWORK_VENUE: &str = "hyperliquid";

/// The one connection a source holds for its life (the module doc's "a source is a request").
///
/// `Clone` makes a NEW, empty slot: a clone is a new request, and two sources never share a
/// connection (the daemon's per-connection state would then outlive the request that created it).
struct SourceConnection {
    link: GatewayLink,
    scope: Scope,
    slot: Mutex<Option<GatewayClient>>,
}

impl Clone for SourceConnection {
    fn clone(&self) -> Self {
        Self::new(self.link.clone(), self.scope)
    }
}

impl SourceConnection {
    fn new(link: GatewayLink, scope: Scope) -> Self {
        Self { link, scope, slot: Mutex::new(None) }
    }

    fn dial(&self) -> Result<GatewayClient, FetchError> {
        GatewayClient::connect(&self.link, self.scope)
            .map_err(|e| FetchError::Link(gateway_text(self.link.addr(), &e)))
    }

    /// One fetch on the held connection (dialled on first use). A link failure BEFORE any reply frame
    /// on a REUSED connection is a stale one (the daemon reaped it: the failure is an EOF, a reset or a
    /// failed write) and is repeated once on a fresh connection; everything else is the answer. ⚠ **A
    /// TIMEOUT is never stale** ([`GatewayClient::timed_out`]): a daemon that read the request and went
    /// quiet may be spending the venue on it, and repeating would double the wait and the venue
    /// request, so a stall is the fetch's answer even when no frame was ever heard.
    fn fetch(
        &self,
        what: &FetchWhat,
        credentialed: bool,
        should_stop: Option<&dyn Fn() -> bool>,
    ) -> Result<FetchReply, FetchError> {
        let held = self.slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        let reused = held.is_some();
        let mut client = match held {
            Some(client) => client,
            None => self.dial()?,
        };
        let mut result = client.fetch(what, credentialed, should_stop);
        if reused
            && matches!(result, Err(FetchError::Link(_)))
            && !client.heard_a_reply()
            && !client.timed_out()
        {
            client = self.dial()?;
            result = client.fetch(what, credentialed, should_stop);
        }
        if client.is_usable() {
            *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(client);
        }
        result
    }
}

/// One remote kline source: `venue`'s `KlineSource`, answered by the gatewayhub.
///
/// `venue()` is the roster slug, so a table keyed by it (the backfill fold, the store partition) keys
/// on the same string the in-process source reported. Build ONE PER REQUEST: it holds a connection for
/// its life (module doc). Cloning makes a new source with its own connection.
#[derive(Clone)]
pub struct KlineSourceGateway {
    conn: SourceConnection,
    venue: String,
    credentialed: bool,
    stop: Option<StopProbe>,
}

impl std::fmt::Debug for KlineSourceGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KlineSourceGateway")
            .field("venue", &self.venue)
            .field("credentialed", &self.credentialed)
            .field("stop", &self.stop.is_some())
            .finish_non_exhaustive()
    }
}

impl KlineSourceGateway {
    /// A KEYLESS kline source for `venue` (`Request::Fetch`, `Scope::Write`: the window is the
    /// caller's, see [`crate::proto::required_scope`]). A link holding no Write key refuses locally,
    /// naming the missing key, and dials nothing.
    pub fn new(link: GatewayLink, venue: &str) -> Self {
        Self {
            conn: SourceConnection::new(link, Scope::Write),
            venue: venue.to_string(),
            credentialed: false,
            stop: None,
        }
    }

    /// A CREDENTIALED kline source for `venue` (`Request::FetchCredentialed`, `Scope::Write`): the
    /// daemon spends the credential it holds. The datahub never reads that credential. The daemon
    /// reads it once per CONNECTION, so build this source per request and keep it for the request.
    pub fn credentialed(link: GatewayLink, venue: &str) -> Self {
        Self { credentialed: true, ..Self::new(link, venue) }
    }

    /// The same source holding a request's stop probe ([`KlineSourceGateway::fetch_until`]'s, applied to
    /// every `fetch`).
    #[must_use]
    pub fn with_stop(mut self, stop: StopProbe) -> Self {
        self.stop = Some(stop);
        self
    }

    /// `fetch`, with a stop probe borrowed for this call — for a caller that holds the request's
    /// probe as a `&dyn Fn() -> bool` and cannot make it `'static`.
    pub fn fetch_until(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
        should_stop: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<Bar>, SourceError> {
        let what = FetchWhat::Klines {
            venue: self.venue.clone(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start_ms,
            end_ms,
        };
        let reply = self
            .conn
            .fetch(&what, self.credentialed, should_stop)
            .map_err(FetchError::into_source_error)?;
        reply
            .assembler
            .finish_bars(reply.pages, reply.rows)
            .map_err(|why| SourceError::Fetch(gateway_text(self.conn.link.addr(), &why)))
    }
}

impl KlineSource for KlineSourceGateway {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn fetch(
        &self,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<Bar>, SourceError> {
        let probe = self.stop.as_deref().map(|f| f as &dyn Fn() -> bool);
        self.fetch_until(symbol, interval, start_ms, end_ms, probe)
    }
}

/// One remote funding-rate source: `venue`'s `FundingRateSource`, answered by the gatewayhub. Build
/// one per request, like [`KlineSourceGateway`].
#[derive(Clone)]
pub struct FundingRateSourceGateway {
    conn: SourceConnection,
    venue: String,
    stop: Option<StopProbe>,
}

impl std::fmt::Debug for FundingRateSourceGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FundingRateSourceGateway")
            .field("venue", &self.venue)
            .field("stop", &self.stop.is_some())
            .finish_non_exhaustive()
    }
}

impl FundingRateSourceGateway {
    /// A funding source for `venue` (`Scope::Write`, like [`KlineSourceGateway::new`]).
    pub fn new(link: GatewayLink, venue: &str) -> Self {
        Self {
            conn: SourceConnection::new(link, Scope::Write),
            venue: venue.to_string(),
            stop: None,
        }
    }

    /// The same source holding a request's stop probe.
    #[must_use]
    pub fn with_stop(mut self, stop: StopProbe) -> Self {
        self.stop = Some(stop);
        self
    }
}

impl FundingRateSource for FundingRateSourceGateway {
    fn venue(&self) -> &str {
        &self.venue
    }

    fn fetch(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<Vec<FundingRatePoint>, SourceError> {
        let what = FetchWhat::Funding {
            venue: self.venue.clone(),
            symbol: symbol.to_string(),
            start_ms,
            end_ms,
        };
        let probe = self.stop.as_deref().map(|f| f as &dyn Fn() -> bool);
        let reply = self.conn.fetch(&what, false, probe).map_err(FetchError::into_source_error)?;
        reply
            .assembler
            .finish_funding(reply.pages, reply.rows)
            .map_err(|why| SourceError::Fetch(gateway_text(self.conn.link.addr(), &why)))
    }
}

/// The remote dukascopy tick fetch: `vike_dukascopy::DukascopyTicks::fetch_quotes_range`'s shape, answered by the
/// gatewayhub. Its errors are `String`s (the seam's), carrying the daemon's text unchanged for a
/// daemon failure and `gatewayhub <addr>: ...` for a link failure — never an empty `Vec`. Build one
/// per request: it holds a connection for its life, like [`KlineSourceGateway`].
#[derive(Clone)]
pub struct GatewayTicks {
    conn: SourceConnection,
}

impl std::fmt::Debug for GatewayTicks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GatewayTicks").finish_non_exhaustive()
    }
}

impl GatewayTicks {
    /// A tick fetch over `link` (`Scope::Write`, like [`KlineSourceGateway::new`]).
    pub fn new(link: GatewayLink) -> Self {
        Self { conn: SourceConnection::new(link, Scope::Write) }
    }

    /// Fetch `symbol`'s quotes over `[start_ms, end_ms]`, polling `should_stop` while it waits.
    pub fn fetch_quotes_range(
        &self,
        symbol: &str,
        start_ms: i64,
        end_ms: i64,
        should_stop: Option<&dyn Fn() -> bool>,
    ) -> Result<Vec<TickQuote>, String> {
        let what = FetchWhat::Ticks {
            venue: TICK_VENUE.to_string(),
            symbol: symbol.to_string(),
            start_ms,
            end_ms,
        };
        let reply = self.conn.fetch(&what, false, should_stop).map_err(FetchError::into_text)?;
        reply
            .assembler
            .finish_quotes(reply.pages, reply.rows)
            .map_err(|why| gateway_text(self.conn.link.addr(), &why))
    }

    /// **The closure `backfill_quotes_then_bars` takes**, built around the request's stop probe:
    /// `FnMut(&str, i64, i64) -> Result<Vec<TickQuote>, String>`. The probe is the one the ingest
    /// itself asks between chunks; handing it here as well stops a fetch that is IN FLIGHT, which a
    /// dukascopy day (minutes) makes worth having.
    pub fn fetcher<'a>(
        &'a self,
        should_stop: &'a dyn Fn() -> bool,
    ) -> impl FnMut(&str, i64, i64) -> Result<Vec<TickQuote>, String> + 'a {
        move |symbol, start_ms, end_ms| {
            self.fetch_quotes_range(symbol, start_ms, end_ms, Some(should_stop))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kline roster is `KLINE_SOURCES`' order: binance, bybit, okx, aster, deribit, hyperliquid,
    /// fxcm (the datahub's backfill registry). The datahub folds this roster, so reordering it
    /// reorders the unknown-venue refusal and the advertisement — an observable change.
    #[test]
    fn kline_roster_order_is_the_datahubs() {
        assert_eq!(
            KLINE_VENUES,
            ["binance", "bybit", "okx", "aster", "deribit", "hyperliquid", "fxcm"],
            "KLINE_SOURCES' order is observable; a reorder is a decision, not an edit"
        );
    }

    /// The funding roster is `FUNDING_SOURCES`' order: binance, hyperliquid, then the three appended.
    #[test]
    fn funding_roster_order_is_the_datahubs() {
        assert_eq!(FUNDING_VENUES, ["binance", "hyperliquid", "bybit", "okx", "deribit", "aster"]);
    }

    #[test]
    fn every_roster_venue_is_a_real_slug_listed_once() {
        for roster in [&KLINE_VENUES[..], &FUNDING_VENUES[..], &FEED_TICK_VENUES[..]] {
            for (i, v) in roster.iter().enumerate() {
                assert!(vike_model::VENUES.contains(v), "`{v}` is not in vike_model::VENUES");
                assert!(!roster[..i].contains(v), "`{v}` is listed twice");
            }
        }
        assert!(vike_model::VENUES.contains(&TICK_VENUE));
        assert!(vike_model::VENUES.contains(&NETWORK_VENUE));
    }
}
