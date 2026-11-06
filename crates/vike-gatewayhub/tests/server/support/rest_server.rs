//! A running gatewayhub SERVER on loopback with the REAL `VenueRest` lane, and the client links to it.
//!
//! `harness.rs` serves the SCRIPTED lanes of `fake_lanes.rs` to test the server; this serves the lane
//! under test (`vike_gatewayhub::rest::lane::VenueRest`, built by the test around scripted venue
//! sources) to test the lane THROUGH the server, with the client crate's real `RemoteKlines`,
//! `RemoteCatalog`, `RemoteOptions` and the rest as the peer. The data lane is an empty fake: nothing
//! here streams.
//!
//! Compiled once, in the `server` group binary beside `fake_lanes` and `harness` (`tests/server.rs`).

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::gateway::{ServedThread, fast_timings};
use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::HubKeys;

use crate::fake_lanes::FakeData;
use crate::harness::{read_only_keys, test_keys};

/// A server on an ephemeral loopback port, dispatching the REST verbs to a real `VenueRest`.
pub struct RestServer {
    served: ServedThread,
    /// Where it listens.
    pub addr: SocketAddr,
}

impl RestServer {
    /// Serve `lane` with the production configuration and the test keys.
    pub fn start(lane: VenueRest) -> Self {
        Self::start_with(lane, ServerConfig::PRODUCTION, test_keys())
    }

    /// Serve `lane` with `config` and `keys`.
    pub fn start_with(lane: VenueRest, config: ServerConfig, keys: HubKeys) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the assigned port");
        let lanes = Lanes { data: FakeData::new(&[]), rest: Arc::new(lane) };
        let served = ServedThread::spawn("test-gwh-rest-serve", move |stop| {
            serve(listener, keys, lanes, config, stop)
        });
        Self { served, addr }
    }

    /// A link with both keys and [`fast_timings`].
    pub fn link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), test_keys()).with_timings(fast_timings())
    }

    /// A link with the Read key only.
    pub fn read_link(&self) -> GatewayLink {
        GatewayLink::new(self.addr.to_string(), read_only_keys()).with_timings(fast_timings())
    }

    /// Raise the stop flag and wait for `serve` to return its result.
    pub fn stop(&mut self) -> io::Result<()> {
        self.served.stop()
    }
}
