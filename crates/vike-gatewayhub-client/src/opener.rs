//! `GatewayOpener` — the gatewayhub's [`Opener`]: how the tube's receiving engine
//! (`vike_market_stream::market::RemoteDataClient`) dials this daemon, authenticates under the `Read`
//! scope and opens a `stream` session. The engine owns reconnecting, replay and gap disclosure; this
//! owns the dial and the handshake (`crate::client::GatewayClient`), which stay with the client crate
//! (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
//!
//! The numbers the engine frames and waits under are this protocol's: the frame ceilings and the symbol
//! guard are `crate::proto`'s constants, the waits are the link's [`crate::client::GatewayTimings`].

use std::io;
use std::net::TcpStream;

use vike_hub_proto::auth::Scope;
use vike_market_stream::opener::{DataStream, Opener, StreamLimits, StreamTimings};

use crate::client::{GatewayClient, GatewayLink, gateway_text};
use crate::proto::{GW_DOWN_FRAME_CEILING_BYTES, GW_MAX_SYMBOL_BYTES, GW_UP_FRAME_CEILING_BYTES};

/// The gatewayhub behind one [`GatewayLink`], as an [`Opener`].
#[derive(Debug, Clone)]
pub struct GatewayOpener {
    link: GatewayLink,
}

impl GatewayOpener {
    /// An opener that dials `link`.
    pub fn new(link: GatewayLink) -> Self {
        Self { link }
    }
}

impl Opener for GatewayOpener {
    fn addr(&self) -> &str {
        self.link.addr()
    }

    fn limits(&self) -> StreamLimits {
        StreamLimits {
            up_frame_ceiling_bytes: GW_UP_FRAME_CEILING_BYTES,
            down_frame_ceiling_bytes: GW_DOWN_FRAME_CEILING_BYTES,
            max_symbol_bytes: GW_MAX_SYMBOL_BYTES,
        }
    }

    fn timings(&self) -> StreamTimings {
        let t = self.link.timings();
        StreamTimings {
            sub_reply: t.sub_reply,
            stream_read_floor: t.stream_read_floor,
            reconnect_base: t.reconnect_base,
            reconnect_max: t.reconnect_max,
        }
    }

    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        gateway_text(self.link.addr(), e)
    }

    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream> {
        GatewayClient::connect_observed(&self.link, Scope::Read, |stream| observe(stream))
            .and_then(|client| client.open_data(venue))
    }
}
