//! What every connection of one server shares, and the per-connection context built on it.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;

use vike_gatewayhub_client::proto::{GW_DOWN_FRAME_CEILING_BYTES, Response};
use vike_hub_proto::auth::{HubKeys, Scope};
use vike_market_stream::frame::write_capped;

use crate::rest::RestFacts;
use crate::session::ConnectionScope;

use super::ctl::{ConnCtl, ConnRegistry};
use super::lanes::Lanes;
use super::limits::ServerConfig;
use super::stats::Stats;

/// What a verb handler tells the connection loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum After {
    /// The connection is still positional: read the next request.
    Continue,
    /// The connection is over (a write fault, a mode switch that ran its course, a stop).
    Close,
}

/// Fixed for the life of the process: the keys, the lanes, the advertisement computed from them, the
/// configuration and the live counters.
pub struct Shared {
    /// The hub keys the handshake verifies against. Never printed (`HubKeys`' `Debug` shows
    /// presence only).
    pub keys: HubKeys,
    /// The venue half.
    pub lanes: Lanes,
    /// `Welcome.features`, computed once from the lanes' build facts.
    pub features: Vec<String>,
    /// The keyless stream venues (`md_venue=`), in advertisement order.
    pub data_venues: Vec<String>,
    /// The REST lanes' facts.
    pub rest: RestFacts,
    /// Timings and caps.
    pub config: ServerConfig,
    /// Live counters.
    pub stats: Stats,
    /// Every connection being served.
    pub registry: Arc<ConnRegistry>,
}

/// One authenticated connection: its socket, who it is and what it may do.
pub struct Conn {
    /// The server-wide state.
    pub shared: Arc<Shared>,
    /// The socket.
    pub stream: TcpStream,
    /// The peer's address, when the socket still knows it.
    pub peer: Option<SocketAddr>,
    /// The capability ceiling it authenticated under.
    pub scope: Scope,
    /// The non-secret fingerprint of the key that authenticated.
    pub key_id: Option<String>,
    /// What the venue lanes keep for THIS connection (dropped when it closes).
    pub memo: Arc<ConnectionScope>,
    /// The handle the daemon's stop reaches this connection through.
    pub ctl: Arc<ConnCtl>,
    /// Frames the connection thread read while a fetch ran that were not a `FetchCancel`: requests the
    /// client pipelined. They are answered, in order, after the fetch's terminal frame.
    pub stash: VecDeque<Vec<u8>>,
}

impl Conn {
    /// Write one response under the client's frame ceiling.
    pub fn reply(&mut self, response: &Response) -> io::Result<()> {
        write_capped(&mut self.stream, response, GW_DOWN_FRAME_CEILING_BYTES)
    }

    /// Write an `Error` reply: a refusal of one verb, on a connection that stays open.
    pub fn reply_error(&mut self, text: impl Into<String>) -> io::Result<()> {
        self.reply(&Response::Error(text.into()))
    }
}
