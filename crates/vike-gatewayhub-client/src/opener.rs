//! `GatewayOpener` — the gatewayhub's [`Opener`]: how the tube's receiving engine
//! (`vike_market_stream::market::RemoteDataClient`) dials this daemon, authenticates under the `Read`
//! scope (the `Write` scope for the strategy lane's credentialed sessions) and opens a `stream` session. The engine owns reconnecting, replay and gap disclosure; this
//! owns the dial and the handshake (`crate::client::GatewayClient`), which stay with the client crate
//! (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
//!
//! The numbers the engine frames and waits under are this protocol's: the frame ceilings and the symbol
//! guard are `crate::proto`'s constants, the waits are the link's [`crate::client::GatewayTimings`].

use std::io;
use std::net::TcpStream;

use vike_hub_proto::auth::Scope;
use vike_market_stream::opener::{DataStream, Opener, StreamLimits, StreamTimings};

use crate::client::{GatewayClient, GatewayLink, StreamAsk, gateway_text};
use crate::proto::{
    GW_CREDENTIALED_FIRST_DIAL_WAIT, GW_DOWN_FRAME_CEILING_BYTES, GW_MAX_SYMBOL_BYTES,
    GW_UP_FRAME_CEILING_BYTES,
};

/// The gatewayhub behind one [`GatewayLink`], as an [`Opener`].
#[derive(Debug, Clone)]
pub struct GatewayOpener {
    link: GatewayLink,
    scope: Scope,
    ask: StreamAsk,
}

impl GatewayOpener {
    /// An opener that dials `link` for a keyless stream under the `Read` scope.
    pub fn new(link: GatewayLink) -> Self {
        Self { link, scope: Scope::Read, ask: StreamAsk::default() }
    }

    /// An opener for the STRATEGY LANE's venue clients: `network` names the venue's network (only
    /// Hyperliquid takes one), `credentialed` a `cs_venue=` venue (the `Write` scope, the datahub's
    /// control key) and `pump` a session that hosts a tick pump (`gw_pump` required).
    pub fn strategy(
        link: GatewayLink,
        network: Option<String>,
        credentialed: bool,
        pump: bool,
    ) -> Self {
        let scope = if credentialed { Scope::Write } else { Scope::Read };
        Self { link, scope, ask: StreamAsk { network, credentialed, pump } }
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

    /// A CREDENTIALED stream's open includes the daemon's handshake with the venue (ctrader's runs up to
    /// `GW_CREDENTIALED_OPEN_BUDGET` inside the open), so its first dial is waited for that long; a keyless
    /// stream keeps the engine's 2 s.
    fn first_dial_wait(&self, _venue: &str) -> Option<std::time::Duration> {
        self.ask.credentialed.then_some(GW_CREDENTIALED_FIRST_DIAL_WAIT)
    }

    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<DataStream> {
        GatewayClient::connect_observed(&self.link, self.scope, |stream| observe(stream))
            .and_then(|client| client.open_stream(venue, &self.ask))
    }
}

#[cfg(test)]
mod tests {
    use vike_hub_proto::auth::HubKeys;

    use super::*;
    use crate::proto::{GW_CREDENTIALED_OPEN_BUDGET, LOOKUP_REPLY_TIMEOUT};

    fn link() -> GatewayLink {
        GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new()))
    }

    /// **A credentialed stream's first dial is waited for as long as its open's handshake may take; a keyless
    /// one keeps the engine's default.** ctrader's handshake runs up to `GW_CREDENTIALED_OPEN_BUDGET` inside the
    /// open, so the datahub's first subscribe on it waits `GW_CREDENTIALED_FIRST_DIAL_WAIT` (past the budget, not past
    /// the open-reply wait); a keyless venue's dial is a connect and a lookup (`None`: the engine's 2 s).
    #[test]
    fn only_a_credentialed_streams_first_dial_is_waited_for_a_handshake() {
        let credentialed = GatewayOpener::strategy(link(), None, true, false);
        let wait =
            credentialed.first_dial_wait("ctrader").expect("a credentialed dial states its wait");
        assert_eq!(wait, GW_CREDENTIALED_FIRST_DIAL_WAIT);
        assert!(wait > GW_CREDENTIALED_OPEN_BUDGET && wait <= LOOKUP_REPLY_TIMEOUT, "{wait:?}");
        assert_eq!(
            GatewayOpener::strategy(link(), None, false, false).first_dial_wait("binance"),
            None
        );
        assert_eq!(GatewayOpener::new(link()).first_dial_wait("binance"), None);
    }
}
