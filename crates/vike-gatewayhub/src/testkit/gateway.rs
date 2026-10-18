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

/// The Trade-scope key a trade-serving gateway verifies ([`LoopbackGateway::start_trade`]) and
/// [`LoopbackGateway::trade_link`] signs with.
#[cfg(feature = "trade")]
pub const TRADE_KEY: &[u8] = b"testkit-trade-key-not-a-hub-key";

/// Both test keys: what the server holds and what [`LoopbackGateway::link`] signs with.
pub fn keys() -> HubKeys {
    HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec())
}

/// The three test keys: what a trade-serving server holds.
#[cfg(feature = "trade")]
pub fn trade_keys() -> HubKeys {
    keys().with_trade(TRADE_KEY.to_vec())
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
    #[cfg(feature = "trade")]
    trade: Option<super::trade::ScriptedTrade>,
    #[cfg(feature = "trade")]
    budget: Option<Arc<crate::budget::ledger::Budget>>,
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
        Self {
            addr,
            data,
            rest,
            stop,
            thread: Some(thread),
            #[cfg(feature = "trade")]
            trade: None,
            #[cfg(feature = "trade")]
            budget: None,
        }
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

/// What a trade-serving [`LoopbackGateway`] is started with besides its lanes.
#[cfg(feature = "trade")]
#[derive(Debug, Clone, Default)]
pub struct TradeKit {
    /// The project's state directory (`LIVE-*.lock`, `gateway-id`). `None` = none resolved.
    pub state_dir: Option<std::path::PathBuf>,
    /// The HALT sentinel's path. `None` = no gate at the sessions' seam.
    pub halt_path: Option<std::path::PathBuf>,
}

#[cfg(feature = "trade")]
impl LoopbackGateway {
    /// Start a server that ALSO serves the trade class over `trade` (the production configuration, the
    /// test keys plus [`TRADE_KEY`], no state directory and no HALT path). Panics if the loopback socket
    /// cannot be bound or the thread cannot be spawned.
    pub fn start_trade(
        data: ScriptedData,
        rest: ScriptedRest,
        trade: super::trade::ScriptedTrade,
    ) -> Self {
        Self::start_trade_with(data, rest, trade, ServerConfig::PRODUCTION, TradeKit::default())
    }

    /// [`LoopbackGateway::start_trade`] with another server configuration and a [`TradeKit`].
    pub fn start_trade_with(
        data: ScriptedData,
        rest: ScriptedRest,
        trade: super::trade::ScriptedTrade,
        config: ServerConfig,
        kit: TradeKit,
    ) -> Self {
        let listener = bind_loopback("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let addr = listener.local_addr().expect("the assigned port");
        let stop = Arc::new(AtomicBool::new(false));
        let lanes = Lanes { data: Arc::new(data.clone()), rest: Arc::new(rest.clone()) };
        let budget = Arc::new(crate::budget::ledger::Budget::new());
        let setup = crate::server::trade::TradeSetup {
            lane: Arc::new(trade.clone()),
            state_dir: kit.state_dir,
            halt_path: kit.halt_path,
            budget: Arc::clone(&budget),
        };
        let thread = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("gwh-testkit".into())
                .spawn(move || {
                    crate::server::accept::serve_with_trade(
                        listener,
                        trade_keys(),
                        lanes,
                        config,
                        setup,
                        stop,
                    )
                })
                .expect("spawn the gatewayhub server thread")
        };
        Self {
            addr,
            data,
            rest,
            stop,
            thread: Some(thread),
            trade: Some(trade),
            budget: Some(budget),
        }
    }

    /// The scripted trade lane the server dispatches to, when it serves the trade class.
    pub fn trade(&self) -> Option<&super::trade::ScriptedTrade> {
        self.trade.as_ref()
    }

    /// The request budget the trade sessions count their orders against (shadow mode: it counts and
    /// refuses nothing).
    pub fn budget(&self) -> Option<&Arc<crate::budget::ledger::Budget>> {
        self.budget.as_ref()
    }

    /// A link with the Trade key and [`fast_timings`]: dial it with the client crate's trade seams.
    pub fn trade_link(&self) -> GatewayLink {
        self.link_with(trade_keys(), fast_timings())
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
