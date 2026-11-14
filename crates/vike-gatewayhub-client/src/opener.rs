//! `StreamOpenerGateway` — the gatewayhub's [`StreamOpener`]: how the tube's receiving engine
//! (`vike_market_stream::market::DataClientStream`) dials this daemon, authenticates under the `Read`
//! scope (the `Write` scope for the strategy lane's credentialed sessions) and opens a `stream` session. The engine owns reconnecting, replay and gap disclosure; this
//! owns the dial and the handshake (`crate::client::GatewayClient`), which stay with the client crate
//! (`docs/decisions/0167-one-live-data-tube-vike-market-stream-on-a-data-plane-tier.md`).
//!
//! The numbers the engine frames and waits under are this protocol's: the frame ceilings and the symbol
//! guard are `crate::proto`'s constants, the waits are the link's [`crate::client::GatewayTimings`].

use std::io;
use std::net::TcpStream;

use vike_hub_protocol::auth::Scope;
use vike_market_stream::opener::{StreamConnection, StreamLimits, StreamOpener, StreamTimings};

use crate::client::{GatewayClient, GatewayLink, StreamAsk, gateway_text};
use crate::proto::{
    GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT, GATEWAY_DOWN_FRAME_CEILING_BYTES,
    GATEWAY_MAX_SYMBOL_BYTES, GATEWAY_UP_FRAME_CEILING_BYTES,
};

/// The gatewayhub behind one [`GatewayLink`], as a [`StreamOpener`].
#[derive(Debug, Clone)]
pub struct StreamOpenerGateway {
    link: GatewayLink,
    scope: Scope,
    ask: StreamAsk,
}

impl StreamOpenerGateway {
    /// An opener that dials `link` for a keyless stream under the `Read` scope.
    pub fn new(link: GatewayLink) -> Self {
        Self { link, scope: Scope::Read, ask: StreamAsk::default() }
    }

    /// An opener for the STRATEGY LANE's venue clients: `network` names the venue's network (only
    /// Hyperliquid takes one), `credentialed` a `cs_venue=` venue (the `Write` scope, the datahub's
    /// control key) and `pump` a session that hosts a tick pump (`gateway_feed_tick` required).
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

impl StreamOpener for StreamOpenerGateway {
    fn addr(&self) -> &str {
        self.link.addr()
    }

    fn peer(&self) -> &'static str {
        "gatewayhub"
    }

    fn limits(&self) -> StreamLimits {
        StreamLimits {
            up_frame_ceiling_bytes: GATEWAY_UP_FRAME_CEILING_BYTES,
            down_frame_ceiling_bytes: GATEWAY_DOWN_FRAME_CEILING_BYTES,
            max_symbol_bytes: GATEWAY_MAX_SYMBOL_BYTES,
        }
    }

    fn timings(&self) -> StreamTimings {
        self.link.timings().stream
    }

    fn failure_text(&self, e: &dyn std::fmt::Display) -> String {
        gateway_text(self.link.addr(), e)
    }

    /// A CREDENTIALED stream's open includes the daemon's handshake with the venue (ctrader's runs up to
    /// `GATEWAY_CREDENTIALED_OPEN_BUDGET` inside the open), so its first dial is waited for that long; a keyless
    /// stream keeps the engine's 2 s.
    fn first_dial_wait(&self, _venue: &str) -> Option<std::time::Duration> {
        self.ask.credentialed.then_some(GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT)
    }

    fn open(&self, venue: &str, observe: &dyn Fn(&TcpStream)) -> io::Result<StreamConnection> {
        GatewayClient::connect_observed(&self.link, self.scope, |stream| observe(stream))
            .and_then(|client| client.open_stream(venue, &self.ask))
    }
}

#[cfg(test)]
mod tests {
    use vike_hub_protocol::auth::HubKeys;

    use super::*;
    use crate::proto::{GATEWAY_CREDENTIALED_OPEN_BUDGET, LOOKUP_REPLY_TIMEOUT};

    fn link() -> GatewayLink {
        GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new()))
    }

    /// A one-connection gatewayhub from before the P4.5c renames: it answers the `Hello` with a
    /// version-1 `Welcome`, and no more.
    fn a_version_one_daemon() -> GatewayLink {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().expect("accept");
            let _hello: crate::proto::Request = crate::proto::read_frame(&mut s).expect("Hello");
            let welcome = crate::proto::Response::Welcome {
                proto_version: 1,
                features: vec![],
                nonce: [0; 32],
            };
            crate::proto::write_frame(&mut s, &welcome).expect("Welcome");
        });
        GatewayLink::new(addr.to_string(), HubKeys::new(b"observe".to_vec(), b"control".to_vec()))
    }

    /// The P4.5c renames changed the gatewayhub wire's bytes, so the version stepped 1 -> 2: a daemon
    /// from before them is refused at the handshake, naming both numbers, never at a decode later.
    #[test]
    fn a_gatewayhub_from_before_the_phase_four_renames_is_refused_at_the_handshake() {
        assert_eq!(crate::proto::GATEWAYHUB_PROTO_VERSION, 2);
        let Err(err) = GatewayClient::connect(&a_version_one_daemon(), Scope::Read) else {
            panic!("a version-1 daemon must be refused")
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("client speaks 2, the daemon speaks 1"), "{err}");
    }

    /// The `stream` class opens through the same handshake and carries no version of its own: a
    /// stream to a gatewayhub from before the renames is refused before its first frame.
    #[test]
    fn a_stream_from_before_the_phase_four_renames_is_refused_at_the_handshake() {
        let opener = StreamOpenerGateway::new(a_version_one_daemon());
        let Err(err) = opener.open("binance", &|_| {}) else {
            panic!("a version-1 daemon must be refused")
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("client speaks 2, the daemon speaks 1"), "{err}");
    }

    /// **A credentialed stream's first dial is waited for as long as its open's handshake may take; a keyless
    /// one keeps the engine's default.** ctrader's handshake runs up to `GATEWAY_CREDENTIALED_OPEN_BUDGET` inside the
    /// open, so the datahub's first subscribe on it waits `GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT` (past the budget, not past
    /// the open-reply wait); a keyless venue's dial is a connect and a lookup (`None`: the engine's 2 s).
    #[test]
    fn only_a_credentialed_streams_first_dial_is_waited_for_a_handshake() {
        let credentialed = StreamOpenerGateway::strategy(link(), None, true, false);
        let wait =
            credentialed.first_dial_wait("ctrader").expect("a credentialed dial states its wait");
        assert_eq!(wait, GATEWAY_CREDENTIALED_FIRST_DIAL_WAIT);
        assert!(
            wait > GATEWAY_CREDENTIALED_OPEN_BUDGET && wait <= LOOKUP_REPLY_TIMEOUT,
            "{wait:?}"
        );
        assert_eq!(
            StreamOpenerGateway::strategy(link(), None, false, false).first_dial_wait("binance"),
            None
        );
        assert_eq!(StreamOpenerGateway::new(link()).first_dial_wait("binance"), None);
    }
}
