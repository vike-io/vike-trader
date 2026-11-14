//! What every connection of one server shares, and the per-connection context built on it.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use vike_gatewayhub_client::proto::{GATEWAY_DOWN_FRAME_CEILING_BYTES, Response};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_market_stream::frame::write_capped;

use crate::rest::RestRosters;
use crate::session::ConnectionMemos;

use super::ctl::{ConnectionControl, ConnectionRegistry};
use super::lanes::ServerLanes;
use super::limits::ServerConfig;
use super::stats::ServerStatistics;

/// What a verb handler tells the connection loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerAfter {
    /// The connection is still positional: read the next request.
    Continue,
    /// The connection is over (a write fault, a mode switch that ran its course, a stop).
    Close,
}

/// Fixed for the life of the process: the keys, the lanes, the advertisement computed from them, the
/// configuration and the live counters.
pub struct ServerShared {
    /// The hub keys the handshake verifies against. Never printed (`HubKeys`' `Debug` shows
    /// presence only).
    pub keys: HubKeys,
    /// The venue half.
    pub lanes: ServerLanes,
    /// `Welcome.features`, computed once from the lanes' build facts.
    pub features: Vec<String>,
    /// The keyless stream venues (`market_data_venue=`), in advertisement order.
    pub data_venues: Vec<String>,
    /// The REST lanes' facts.
    pub rest: RestRosters,
    /// Timings and caps.
    pub config: ServerConfig,
    /// Live counters.
    pub stats: ServerStatistics,
    /// Every connection being served.
    pub registry: Arc<ConnectionRegistry>,
    /// The trade lane's state, when one is mounted AND the Trade key is held (`crate::server::accept`).
    #[cfg(feature = "trade")]
    pub trade: Option<Arc<crate::trade::session::AccountSessions>>,
}

/// One authenticated connection: its socket, who it is and what it may do.
pub struct Connection {
    /// The server-wide state.
    pub shared: Arc<ServerShared>,
    /// The socket.
    pub stream: TcpStream,
    /// The peer's address, when the socket still knows it.
    pub peer: Option<SocketAddr>,
    /// The capability ceiling it authenticated under.
    pub scope: Scope,
    /// The non-secret fingerprint of the key that authenticated.
    pub key_id: Option<String>,
    /// What the venue lanes keep for THIS connection (dropped when it closes).
    pub memo: Arc<ConnectionMemos>,
    /// The handle the daemon's stop reaches this connection through.
    pub control: Arc<ConnectionControl>,
    /// Frames the connection thread read while a fetch ran that were not a `FetchCancel`: requests the
    /// client pipelined. They are answered, in order, after the fetch's terminal frame.
    pub stash: VecDeque<Vec<u8>>,
}

impl Connection {
    /// Write one response under the client's frame ceiling.
    pub fn reply(&mut self, response: &Response) -> io::Result<()> {
        write_capped(&mut self.stream, response, GATEWAY_DOWN_FRAME_CEILING_BYTES)
    }

    /// Write an `Error` reply: a refusal of one verb, on a connection that stays open.
    pub fn reply_error(&mut self, text: impl Into<String>) -> io::Result<()> {
        self.reply(&Response::Error(text.into()))
    }
}

/// A positional refusal of a class-opening verb (`DataOpen`, `OptionsFeedOpen`): the `Error` reply, and
/// the socket stays positional; a reply that cannot be written ends the connection.
pub fn refuse(conn: &mut Connection, text: String) -> ServerAfter {
    match conn.reply_error(text) {
        Ok(()) => ServerAfter::Continue,
        Err(_) => ServerAfter::Close,
    }
}
