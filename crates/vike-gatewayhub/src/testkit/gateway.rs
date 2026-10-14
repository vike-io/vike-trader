//! A running gatewayhub SERVER on loopback over the scripted lanes, and the client links to it.
//!
//! [`LoopbackGateway::start`] is the real `crate::server::accept::serve`: a real listener bound by the
//! real loopback guard on an ephemeral port, the real handshake and verb loop, stopped the way the
//! daemon stops it (the shared stop flag, then a drain and a join). Only the lanes are scripted. So a
//! test in another crate dials it with the client crate's real seams and sees what the datahub will:
//!
//! ```text
//! let data = ScriptedData::new(&["binance"]);
//! let rest = ScriptedRest::new().with_klines("binance", Answer::rows(bars));
//! let gw = LoopbackGateway::start(data.clone(), rest.clone());
//! let klines = RemoteKlines::new(gw.link(), "binance");
//! ```
//!
//! # Keys and timings
//!
//! The server verifies, and [`LoopbackGateway::link`] signs with, the fixed test keys
//! ([`keys`]): never a real hub key. [`LoopbackGateway::read_link`] holds the Read key only (a
//! credentialed fetch and a klines, funding or ticks window fetch need the Write one, so they are
//! refused on that link). The links carry
//! [`fast_timings`], short enough that a stall is watched in milliseconds; they are the timings the
//! crate's own suites run with. A test that wants the production waits builds a link of its own with
//! [`LoopbackGateway::link_with`].
//!
//! # Stopping
//!
//! Dropping the gateway raises the stop flag and joins the server thread, which drains its
//! connections first (a stream session writes its `Bye`, a running fetch is cancelled). A
//! client still holding a connection delays the return until the drain budget, so a test drops its
//! clients before the gateway (declare the gateway first).

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use vike_gatewayhub_client::client::{GatewayLink, GatewayTimings};
use vike_hub_proto::auth::HubKeys;

use crate::server::accept::serve;
use crate::server::bind::bind_loopback;
use crate::server::lanes::Lanes;
use crate::server::limits::ServerConfig;

use super::data::ScriptedData;
use super::rest::ScriptedRest;

/// The Read-scope key the server verifies and the links sign with.
pub const OBSERVE_KEY: &[u8] = b"testkit-observe-key-not-a-hub-key";

/// The Write-scope key the server verifies and [`LoopbackGateway::link`] signs with.
pub const CONTROL_KEY: &[u8] = b"testkit-control-key-not-a-hub-key";

/// Both test keys: what the server holds and what [`LoopbackGateway::link`] signs with.
pub fn keys() -> HubKeys {
    HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// The Read key only: what [`LoopbackGateway::read_link`] signs with.
pub fn read_only_keys() -> HubKeys {
    HubKeys::new(OBSERVE_KEY.to_vec(), Vec::new())
}

/// Client waits short enough that a test watches a stall in milliseconds. They are floors in
/// production's mouth (the client crate's `GatewayTimings` says why), which is why only a test
/// narrows them.
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

/// A gatewayhub server on an ephemeral loopback port, over scripted lanes. See the module doc.
pub struct LoopbackGateway {
    addr: SocketAddr,
    data: ScriptedData,
    rest: ScriptedRest,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl LoopbackGateway {
    /// Start a server over `data` and `rest` with the production configuration and the test keys. The
    /// lanes' build facts are read NOW and never again, so script the REST lane before this call.
    /// Panics if the loopback socket cannot be bound or the thread cannot be spawned.
    pub fn start(data: ScriptedData, rest: ScriptedRest) -> Self {
        Self::start_with(data, rest, ServerConfig::PRODUCTION)
    }

    /// [`LoopbackGateway::start`] with another server configuration (a smaller cap, a shorter drain).
    pub fn start_with(data: ScriptedData, rest: ScriptedRest, config: ServerConfig) -> Self {
        let listener = bind_loopback("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the assigned port");
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: Arc::new(data.clone()), rest: Arc::new(rest.clone()) };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("gwh-testkit".into())
                .spawn(move || serve(listener, keys(), lanes, config, stop))
                .expect("spawn the gatewayhub server thread")
        };
        Self { addr, data, rest, stop, thread: Some(thread) }
    }

    /// Where the server listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The address as the `host:port` string a `GatewayLink` takes.
    pub fn addr_string(&self) -> String {
        self.addr.to_string()
    }

    /// The scripted data lane the server dispatches to (a handle on the one passed to `start`).
    pub fn data(&self) -> &ScriptedData {
        &self.data
    }

    /// The scripted REST lane the server dispatches to (a handle on the one passed to `start`).
    pub fn rest(&self) -> &ScriptedRest {
        &self.rest
    }

    /// A link with both keys and [`fast_timings`]: dial it with the client crate's real seams.
    pub fn link(&self) -> GatewayLink {
        self.link_with(keys(), fast_timings())
    }

    /// A link with the Read key only and [`fast_timings`].
    pub fn read_link(&self) -> GatewayLink {
        self.link_with(read_only_keys(), fast_timings())
    }

    /// A link to this server signing with `keys` and waiting by `timings`.
    pub fn link_with(&self, keys: HubKeys, timings: GatewayTimings) -> GatewayLink {
        GatewayLink::new(self.addr_string(), keys).with_timings(timings)
    }

    /// Raise the stop flag and wait for `serve` to drain and return its result. Idempotent; dropping the
    /// gateway does the same and discards the result.
    pub fn stop(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        match self.thread.take() {
            Some(t) => t.join().expect("the gatewayhub server thread panicked"),
            None => Ok(()),
        }
    }
}

impl std::fmt::Debug for LoopbackGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoopbackGateway").field("addr", &self.addr).finish_non_exhaustive()
    }
}

impl Drop for LoopbackGateway {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
