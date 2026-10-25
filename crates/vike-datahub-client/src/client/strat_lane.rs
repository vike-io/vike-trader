//! `DatahubClient`'s strategy-lane verbs: the dial under [`Scope::Trade`] and `strat_open`, the wire's
//! second mode switch (`Request::StratOpen`), which CONSUMES the client and hands back the raw socket.
//!
//! What follows the open reply is the TUBE's (`vike_market_stream::wire`), read and written by its
//! receiving engine; the dial-and-open as an `Opener` is `crate::strat::StratOpener`. These two methods
//! are in the `client` module because the socket and the handshake state are its private fields.

use super::*;
use crate::proto::{FEATURE_STRATEGY_LANE, FEATURE_STREAM_CLASS, advertised_strat_venues};

/// What a successful [`DatahubClient::strat_open`] learned: the positional answer, unpacked, so a caller
/// never matches `Response` again on a socket that has stopped carrying one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StratOpenedInfo {
    /// The daemon's name for this lane session (a log and diagnostics key, nothing else).
    pub session: u64,
    /// The daemon's own heartbeat period, UNCLAMPED: the caller clamps it into
    /// `SL_HEARTBEAT_MIN..=SL_HEARTBEAT_MAX` and arms `max(SL_READ_FLOOR, 3 x period)`.
    pub heartbeat_ms: u64,
    /// Whether the daemon advertised [`FEATURE_STREAM_CLASS`] (a `class` on a `Sub`, the `Delivery`
    /// frame): without it the daemon ignores a class it is sent.
    pub delivery_class: bool,
}

impl DatahubClient {
    /// **Does this daemon serve a strategy lane for `venue`?** Read off the features its `Welcome` advertised and
    /// nothing else (no byte is sent): `Err` of kind [`io::ErrorKind::Unsupported`] naming the string when it
    /// advertises no [`FEATURE_STRATEGY_LANE`] or no `strat_venue=` entry for the venue. [`Self::strat_open`] asks
    /// it before it sends anything, and the tradehub's feed gate asks it for every mounted venue BEFORE a core
    /// exists, so a daemon that cannot serve a mounted venue (an older or smaller build during a rolling deploy)
    /// is a held feed with the words, not a start that dies after its core is up.
    pub fn check_strat_lane(&self, venue: &str) -> io::Result<()> {
        if !self.features.iter().any(|f| f == FEATURE_STRATEGY_LANE) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "datahub does not advertise `{FEATURE_STRATEGY_LANE}` (advertised: {:?}) - it \
                     serves no strategy lane. Nothing was sent.",
                    self.features
                ),
            ));
        }
        if !advertised_strat_venues(&self.features).iter().any(|v| v == venue) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "datahub serves no strategy lane for {venue} (it serves {:?}). Nothing was sent.",
                    advertised_strat_venues(&self.features)
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
    /// connect that quietly degraded to unauthenticated would be a `StratOpen` the daemon must refuse.
    pub fn connect_strat_lane(
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
        let client =
            Self::connect_authed_inner(addr, keys, Scope::Trade, Some(Plane::Data), observe)?;
        if client.authed != Some(Scope::Trade) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "datahub {addr}: the server authenticated nothing (it holds no hub keys), so it \
                     serves no strategy lane"
                ),
            ));
        }
        Ok(client)
    }

    /// **Open a STRATEGY-LANE stream on this connection**, CONSUMING the client: on success the socket
    /// speaks the tube's frames and no positional verb may ever be sent on it again (see
    /// [`crate::proto::Request::StratOpen`]), so the raw [`TcpStream`] is handed back with its WRITE bound
    /// armed (a client frame is tens of bytes: a write blocked this long is a peer that stopped reading) and
    /// its read bound CLEARED for the tube's reader to arm from the stated heartbeat.
    ///
    /// Checked LOCALLY, naming the string, before a byte is sent: the daemon must advertise
    /// [`FEATURE_STRATEGY_LANE`] and the venue's `strat_venue=` entry ([`io::ErrorKind::Unsupported`]).
    /// A daemon-side refusal ([`Response::Error`]: an unserved venue, a network the venue does not take, a
    /// full lane) is an [`io::ErrorKind::Other`] error carrying the daemon's text unchanged. `reply` bounds
    /// the open reply (a pure lookup: nothing the daemon does there waits on a venue).
    pub fn strat_open(
        mut self,
        venue: &str,
        network: Option<&str>,
        reply: Duration,
        write_timeout: Duration,
    ) -> io::Result<(StratOpenedInfo, TcpStream)> {
        self.check_strat_lane(venue)?;
        self.set_reply_timeout(Some(reply));
        let request =
            Request::StratOpen { venue: venue.to_string(), network: network.map(str::to_string) };
        match self.exchange(&request)? {
            Response::StratOpened { session, heartbeat_ms } => {
                self.stream.set_read_timeout(None)?;
                self.stream.set_write_timeout(Some(write_timeout))?;
                let delivery_class = self.features.iter().any(|f| f == FEATURE_STREAM_CLASS);
                Ok((StratOpenedInfo { session, heartbeat_ms, delivery_class }, self.stream))
            }
            Response::Error(text) => Err(io::Error::other(text)),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("datahub: expected StratOpened, got {}", resp_kind(&other)),
            )),
        }
    }
}
