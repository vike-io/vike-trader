//! A running gatewayhub SERVER on loopback with fake lanes, and the client links to it.
//!
//! The server is the real `vike_gatewayhub::server::accept::serve` on a real listener, stopped the way
//! the daemon stops it (the shared stop flag); the lanes are the fakes of `fake_lanes`; the peer in
//! the tests is the client crate's own `RemoteDataClient`, `RemoteKlines` and the rest, or the raw
//! peer of `raw_peer` where a test needs to see the bytes.
//!
//! Compiled once, in the `server` group binary beside `fake_lanes` (`tests/server.rs`); the raw peer is the
//! lib testkit's (`vike_gatewayhub::testkit::raw_peer`), which the `trade` group shares.

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;

use vike_gatewayhub::rest::RestFacts;
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub::testkit::fixtures::full_facts;
use vike_gatewayhub::testkit::gateway::{ServedThread, fast_timings};
use vike_gatewayhub_client::client::GatewayLink;
use vike_hub_proto::auth::HubKeys;

use crate::fake_lanes::{FakeData, FakeRest};

/// The production config with small session queues (128 KiB, 256 frames), so a flood fills them
/// quickly. A tight producer loop outruns even a healthy writer, so these tests would trip the
/// production lapse budget (8 gaps a minute) on their own pace: the budget is raised out of the way
/// here and the one test about the budget lowers it.
pub fn flood_config() -> ServerConfig {
    let mut c = ServerConfig::PRODUCTION;
    c.limits.queue_bytes = 128 * 1024;
    c.limits.queue_frames = 256;
    c.limits.lapse_budget = 1_000_000;
    c
}

/// The observe/control keys the server verifies and the tests sign with.
pub fn test_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), b"control-key-of-the-tests".to_vec())
}

/// The keys of a client that holds only the Read key.
pub fn read_only_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), Vec::new())
}

/// The facts of a daemon built FROM the registry shell: every lane serves exactly the roster the
/// registry declares for it.
pub fn registry_facts() -> RestFacts {
    use vike_gatewayhub::registry::{Lane, owned};
    RestFacts {
        klines: owned(Lane::Klines),
        funding: owned(Lane::Funding),
        ticks: owned(Lane::Ticks),
        catalog: owned(Lane::Catalog),
        credentialed: owned(Lane::CredentialedKlines),
        options: !owned(Lane::Options).is_empty(),
        family: owned(Lane::Family),
        presence: true,
    }
}

/// `n` one-minute bars from `ts = 0`, every field a distinct non-round fraction (and a funding rate), so a
/// field crossed with its neighbour or rounded on the wire shows.
pub fn bars(n: usize) -> Vec<vike_marketdata::Bar> {
    (0..n)
        .map(|i| {
            let x = i as f64;
            vike_marketdata::Bar {
                ts: i as i64 * 60_000,
                open: 0.1 * x,
                high: 0.1 * x + 1.0 / 3.0,
                low: 0.1 * x - 1.0 / 7.0,
                close: 0.1 * x + 1.0 / 9.0,
                volume: x / 7.0,
                funding: Some(x * 1e-7),
                bid: None,
                ask: None,
                symbol: None,
            }
        })
        .collect()
}

/// A server on an ephemeral loopback port.
pub struct Harness {
    served: ServedThread,
    /// Where it listens.
    pub addr: SocketAddr,
    /// The data lane the server dispatches to.
    pub data: Arc<FakeData>,
    /// The REST lane the server dispatches to.
    pub rest: Arc<FakeRest>,
}

impl Harness {
    /// A server with the default lanes: stream venues `binance` and `polymarket`, [`full_facts`], the
    /// test keys and the production configuration.
    pub fn start() -> Self {
        Self::start_config(ServerConfig::PRODUCTION)
    }

    /// A server whose lanes are exactly what the registry declares: the stream roster for the data
    /// lane and [`registry_facts`] for the REST lane.
    pub fn start_from_registry() -> Self {
        let streams = vike_gatewayhub::registry::roster(vike_gatewayhub::registry::Lane::Stream);
        Self::start_with(
            FakeData::new(streams),
            FakeRest::new(registry_facts()),
            ServerConfig::PRODUCTION,
            test_keys(),
        )
    }

    /// A server with the default lanes and `config`.
    pub fn start_config(config: ServerConfig) -> Self {
        Self::start_with(
            FakeData::new(&["binance", "polymarket"]),
            FakeRest::new(full_facts()),
            config,
            test_keys(),
        )
    }

    /// A server with the given lanes, configuration and server-side keys.
    pub fn start_with(
        data: Arc<FakeData>,
        rest: Arc<FakeRest>,
        config: ServerConfig,
        keys: HubKeys,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("the assigned port");
        let lanes = Lanes { data: data.clone(), rest: rest.clone() };
        let served = ServedThread::spawn("test-gwh-serve", move |stop| {
            serve(listener, keys, lanes, config, stop)
        });
        Self { served, addr, data, rest }
    }

    /// The address as the string a link takes.
    pub fn addr_string(&self) -> String {
        self.addr.to_string()
    }

    /// A link with both keys and [`fast_timings`].
    pub fn link(&self) -> GatewayLink {
        GatewayLink::new(self.addr_string(), test_keys()).with_timings(fast_timings())
    }

    /// A link with the Read key only.
    pub fn read_link(&self) -> GatewayLink {
        GatewayLink::new(self.addr_string(), read_only_keys()).with_timings(fast_timings())
    }

    /// Raise the stop flag and wait for `serve` to return its result.
    pub fn stop(&mut self) -> io::Result<()> {
        self.served.stop()
    }
}
