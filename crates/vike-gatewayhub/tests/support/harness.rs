//! A running gatewayhub SERVER on loopback with fake lanes, and the client links to it.
//!
//! The server is the real `vike_gatewayhub::server::accept::serve` on a real listener, stopped the way
//! the daemon stops it (the shared stop flag); the lanes are the fakes of `fake_lanes`; the peer in
//! the tests is the client crate's own `RemoteDataClient`, `RemoteKlines` and the rest, or the raw
//! peer of `raw_peer` where a test needs to see the bytes.
//!
//! Each caller `#[path]`-includes it AFTER `fake_lanes` and `raw_peer` are declared in its own root
//! (the modules name each other through `crate::`).

use std::io;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_gatewayhub::rest::RestFacts;
use vike_gatewayhub::server::accept::serve;
use vike_gatewayhub::server::lanes::Lanes;
use vike_gatewayhub::server::limits::ServerConfig;
use vike_gatewayhub_client::client::{GatewayLink, GatewayTimings};
use vike_hub_proto::auth::HubKeys;

use crate::fake_lanes::{FakeData, FakeRest};

/// The observe/control keys the server verifies and the tests sign with.
pub fn test_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), b"control-key-of-the-tests".to_vec())
}

/// The keys of a client that holds only the Read key.
pub fn read_only_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), Vec::new())
}

/// The full wave-1 facts: klines (binance, bybit), funding (binance), ticks (dukascopy), catalog
/// (binance, dukascopy), credentialed klines (oanda), options, a polymarket family and presence.
pub fn full_facts() -> RestFacts {
    RestFacts {
        klines: vec!["binance".into(), "bybit".into()],
        funding: vec!["binance".into()],
        ticks: vec!["dukascopy".into()],
        catalog: vec!["binance".into(), "dukascopy".into()],
        credentialed: vec!["oanda".into()],
        options: true,
        family: vec!["polymarket".into()],
        presence: true,
    }
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

/// Timings short enough that a test watches a stall in milliseconds.
pub fn fast_timings() -> GatewayTimings {
    GatewayTimings {
        reply_idle: Duration::from_secs(5),
        lookup_reply: Duration::from_secs(5),
        fetch_stall_floor: Duration::from_millis(600),
        fetch_probe_slice: Duration::from_millis(20),
        stream_read_floor: Duration::from_millis(300),
        sub_reply: Duration::from_secs(2),
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(100),
    }
}

/// A server on an ephemeral loopback port.
pub struct Harness {
    /// Where it listens.
    pub addr: SocketAddr,
    /// The data lane the server dispatches to.
    pub data: Arc<FakeData>,
    /// The REST lane the server dispatches to.
    pub rest: Arc<FakeRest>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Harness {
    /// A server with the default lanes: stream venues `binance` and `polymarket`, [`full_facts`], the
    /// test keys and the production configuration.
    pub fn start() -> Self {
        Self::start_with(
            FakeData::new(&["binance", "polymarket"]),
            FakeRest::new(full_facts()),
            ServerConfig::PRODUCTION,
            test_keys(),
        )
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
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: data.clone(), rest: rest.clone() };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("test-gwh-serve".into())
                .spawn(move || serve(listener, keys, lanes, config, stop))
                .expect("spawn the server")
        };
        Self { addr, data, rest, stop, thread: Some(thread) }
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
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(t) => t.join().expect("the server thread panicked"),
            None => Ok(()),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
