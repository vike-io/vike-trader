//! A running gatewayhub SERVER on loopback with the REAL `VenueRest` lane, and the client links to it.
//!
//! `harness.rs` serves the SCRIPTED lanes of `fake_lanes.rs` to test the server; this serves the lane
//! under test (`vike_gatewayhub::rest::lane::VenueRest`, built by the test around scripted venue
//! sources) to test the lane THROUGH the server, with the client crate's real `RemoteKlines`,
//! `RemoteCatalog`, `RemoteOptions` and the rest as the peer. The data lane is an empty fake: nothing
//! here streams.
//!
//! Each caller `#[path]`-includes `fake_lanes` and `harness` first (this file names them through
//! `crate::`) and then this one, and declares `#[allow(dead_code, clippy::allow_attributes)]` on the
//! `mod` line, because every includer uses a different part.

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use vike_gatewayhub::rest::lane::VenueRest;
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::HubKeys;

use crate::fake_lanes::FakeData;
use crate::harness::{fast_timings, read_only_keys, test_keys};

/// A server on an ephemeral loopback port, dispatching the REST verbs to a real `VenueRest`.
pub struct RestServer {
    /// Where it listens.
    pub addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: FakeData::new(&[]), rest: Arc::new(lane) };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("test-gwh-rest-serve".into())
                .spawn(move || serve(listener, keys, lanes, config, stop))
                .expect("spawn the server")
        };
        Self { addr, stop, thread: Some(thread) }
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
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(t) => t.join().expect("the server thread panicked"),
            None => Ok(()),
        }
    }
}

impl Drop for RestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
