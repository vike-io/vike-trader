//! `DatahubClient`'s strategy-lane verbs: the dial under [`Scope::Trade`] and `strategy_open`, the wire's
//! second mode switch (`Request::StrategyOpen`), which CONSUMES the client and hands back the raw socket.
//!
//! What follows the open reply is the TUBE's (`vike_market_stream::wire`), read and written by its
//! receiving engine; the dial-and-open as an `Opener` is `crate::strat::StreamOpenerStrategyLane`. These two methods
//! are in the `client` module because the socket and the handshake state are its private fields.

use super::*;
use crate::proto::{
    FEATURE_FUNDING_HISTORY, FEATURE_INPUT_BARS, FEATURE_STRATEGY_LANE, FEATURE_STREAM_CLASS,
    FEATURE_STREAM_FUNDING, advertised_strategy_venues,
};

/// What a successful [`DatahubClient::strategy_open`] learned: the positional answer, unpacked, so a caller
/// never matches `Response` again on a socket that has stopped carrying one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrategyLaneOpenedInfo {
    /// The daemon's name for this lane session (a log and diagnostics key, nothing else).
    pub session: u64,
    /// The daemon's own heartbeat period, UNCLAMPED: the caller clamps it into
    /// `STRATEGY_LANE_HEARTBEAT_MIN..=STRATEGY_LANE_HEARTBEAT_MAX` and arms `max(STRATEGY_LANE_READ_FLOOR, 3 x period)`.
    pub heartbeat_ms: u64,
    /// Whether the daemon advertised [`FEATURE_STREAM_CLASS`] (a `class` on a `Sub`, the `Delivery`
    /// frame): without it the daemon ignores a class it is sent.
    pub delivery_class: bool,
    /// Whether the daemon advertised [`FEATURE_STREAM_FUNDING`]: the tube then asks it for the venue's
    /// funding frames; without it nothing is asked, so a daemon that predates the frame never sees it.
    pub funding: bool,
}

impl DatahubClient {
    /// **Does this daemon serve a strategy lane for `venue`?** Read off the features its `Welcome` advertised and
    /// nothing else (no byte is sent): `Err` of kind [`io::ErrorKind::Unsupported`] naming the string when it
    /// advertises no [`FEATURE_STRATEGY_LANE`] or no `strategy_venue=` entry for the venue. [`Self::strategy_open`] asks
    /// it before it sends anything, and the tradehub's feed gate asks it for every mounted venue BEFORE a core
    /// exists, so a daemon that cannot serve a mounted venue (an older or smaller build during a rolling deploy)
    /// is a held feed with the words, not a start that dies after its core is up.
    pub fn check_strategy_lane(&self, venue: &str) -> io::Result<()> {
        if !self.features().iter().any(|f| f == FEATURE_STRATEGY_LANE) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "datahub does not advertise `{FEATURE_STRATEGY_LANE}` (advertised: {:?}) - it \
                     serves no strategy lane. Nothing was sent.",
                    self.features()
                ),
            ));
        }
        if !advertised_strategy_venues(self.features()).iter().any(|v| v == venue) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "datahub serves no strategy lane for {venue} (it serves {:?}). Nothing was sent.",
                    advertised_strategy_venues(self.features())
                ),
            ));
        }
        Ok(())
    }

    /// Dial `addr` and authenticate under [`Scope::Trade`] to the DATA daemon, handing `observe` the
    /// freshly dialled socket BEFORE the handshake reads from it.
    ///
    /// Refused BEFORE anything is dialled with [`io::ErrorKind::PermissionDenied`] when `keys` holds no
    /// Trade key (a mac signed with the empty key would otherwise be sent), and after the handshake with
    /// the same kind when the daemon authenticated nothing: a key-less datahub serves no lane, and a
    /// connect that quietly degraded to unauthenticated would be a `StrategyOpen` the daemon must refuse.
    pub fn connect_strategy_lane(
        addr: &str,
        keys: &HubKeys,
        observe: impl FnOnce(&TcpStream),
    ) -> io::Result<Self> {
        if !keys.has(Scope::Trade) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "datahub {addr}: no trade hub key is configured for this link, so a strategy \
                     lane cannot be opened (nothing was dialled)"
                ),
            ));
        }
        let conn = HubConnection::connect_authed_observed(
            addr,
            keys,
            Scope::Trade,
            Some(Plane::Data),
            PROTO_VERSION,
            observe,
        )?;
        if conn.authenticated_scope() != Some(Scope::Trade) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "datahub {addr}: the server authenticated nothing (it holds no hub keys), so it \
                     serves no strategy lane"
                ),
            ));
        }
        Ok(Self { conn })
    }

    /// **The stored funding settlements of `(venue, symbol)` in `[start, end]`** (epoch-ms, inclusive) on this
    /// strategy-lane connection ([`crate::proto::Request::StrategyFundingHistory`]): the store's `interval=funding`
    /// bars, `funding` the rate and `close` the price (`0.0` for none). Refused LOCALLY, nothing sent, by a
    /// daemon that does not advertise [`FEATURE_FUNDING_HISTORY`]; a daemon-side refusal is its text.
    pub fn strategy_funding_history(
        &mut self,
        venue: &str,
        symbol: &str,
        start: i64,
        end: i64,
    ) -> Result<Vec<Bar>, String> {
        if !self.features().iter().any(|f| f == FEATURE_FUNDING_HISTORY) {
            return Err(format!(
                "datahub does not advertise `{FEATURE_FUNDING_HISTORY}` (an older build, or no strategy lane): \
                 its stored funding cannot be read on the lane. Nothing was sent."
            ));
        }
        let request = Request::StrategyFundingHistory {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            start,
            end,
        };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", response_kind(&other))),
        }
    }

    /// **The stored bars of `(venue, symbol, interval)` in `[start, end]`** (epoch-ms, inclusive) on this
    /// strategy-lane connection ([`crate::proto::Request::StrategyInputBars`]): the venue's own candles, or the
    /// `source` lane of them (a user's `user-<label>` import), the read a live strategy mount's input makes
    /// (decision 0271's live half). Refused LOCALLY, nothing sent, by a daemon that does not advertise
    /// [`FEATURE_INPUT_BARS`]; a daemon-side refusal is its text.
    pub fn strategy_input_bars(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        source: Option<&str>,
        start: i64,
        end: i64,
    ) -> Result<Vec<Bar>, String> {
        if !self.features().iter().any(|f| f == FEATURE_INPUT_BARS) {
            return Err(format!(
                "datahub does not advertise `{FEATURE_INPUT_BARS}` (an older build, or no strategy lane): \
                 a strategy input's stored bars cannot be read on the lane. Nothing was sent."
            ));
        }
        let request = Request::StrategyInputBars {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            source: source.map(str::to_string),
            start,
            end,
        };
        match self.exchange(&request).map_err(|e| e.to_string())? {
            Response::Bars(bars) => Ok(bars),
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected Bars, got {}", response_kind(&other))),
        }
    }

    /// **Open a STRATEGY-LANE stream on this connection**, CONSUMING the client: on success the socket
    /// speaks the tube's frames and no positional verb may ever be sent on it again (see
    /// [`crate::proto::Request::StrategyOpen`]), so the raw [`TcpStream`] is handed back with its WRITE bound
    /// armed (a client frame is tens of bytes: a write blocked this long is a peer that stopped reading) and
    /// its read bound CLEARED for the tube's reader to arm from the stated heartbeat.
    ///
    /// Checked LOCALLY, naming the string, before a byte is sent: the daemon must advertise
    /// [`FEATURE_STRATEGY_LANE`] and the venue's `strategy_venue=` entry ([`io::ErrorKind::Unsupported`]).
    /// A daemon-side refusal ([`Response::Error`]: an unserved venue, a network the venue does not take, a
    /// full lane) is an [`io::ErrorKind::Other`] error carrying the daemon's text unchanged. `reply` bounds
    /// the open reply (a pure lookup: nothing the daemon does there waits on a venue).
    pub fn strategy_open(
        mut self,
        venue: &str,
        network: Option<&str>,
        reply: Duration,
        write_timeout: Duration,
    ) -> io::Result<(StrategyLaneOpenedInfo, TcpStream)> {
        self.check_strategy_lane(venue)?;
        self.set_reply_timeout(Some(reply));
        let request = Request::StrategyOpen {
            venue: venue.to_string(),
            network: network.map(str::to_string),
        };
        match self.exchange(&request)? {
            Response::StrategyOpened { session, heartbeat_ms } => {
                self.stream().set_read_timeout(None)?;
                self.stream().set_write_timeout(Some(write_timeout))?;
                let delivery_class = self.features().iter().any(|f| f == FEATURE_STREAM_CLASS);
                let funding = self.features().iter().any(|f| f == FEATURE_STREAM_FUNDING);
                Ok((
                    StrategyLaneOpenedInfo { session, heartbeat_ms, delivery_class, funding },
                    self.conn.into_stream(),
                ))
            }
            Response::Error(text) => Err(io::Error::other(text)),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub: expected StrategyOpened, got {}", response_kind(&other)),
            )),
        }
    }
}
