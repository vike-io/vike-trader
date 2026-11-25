//! The single-socket combined data+exec start (F4). cTrader multiplexes market data AND order flow
//! over ONE authenticated protobuf/TLS socket; [`CtraderClient`] owns exactly one such connection
//! (the [`ActorHandle`] returned by [`transport::connection::connect_and_auth_execution`](crate::transport::connection::connect_and_auth_execution))
//! and hands out lightweight VIEWS — a `DataClient` over a [`DataClientBusSpecCtrader::view`] and a
//! [`ExecutionClientCtrader`] (`ExecutionClient`) — that both drive that ONE actor/socket via a cloned
//! [`ConnectionShared`](crate::transport::connection::ConnectionShared).
//!
//! ## Why this exists
//! The single-client entry points ([`crate::feed::connect`]/[`ExecutionClientCtrader::new`]) each OWN their own
//! [`ActorHandle`] → their own socket + actor thread. Starting both that way opens TWO sockets to
//! the same account for a venue that is designed around one. `CtraderClient` fixes that: it holds
//! the sole [`ActorHandle`] (and thus the sole socket + actor thread), and the views it vends do
//! NOT own the thread — dropping a view never closes the shared socket. The connection is torn down
//! in exactly ONE place: dropping (or [`shutdown`](CtraderClient::shutdown)-ing) the `CtraderClient`.
//!
//! ## Lifecycle
//! ```ignore
//! let client = CtraderClient::connect(cfg, sink, events)?; // one handshake, one socket
//! let mut data = client.data();  // DataClient view (shares the socket)
//! let mut exec = client.exec();  // ExecutionClient view (shares the socket)
//! // ... data.subscribe_quotes(...) and exec.submit(...) both ride the one connection ...
//! client.shutdown();             // the ONE place the socket closes
//! ```
//! Ports nothing — cTrader Open API (https://help.ctrader.com/open-api/).

pub mod connection;
pub mod error_codes;
pub mod framing;
pub mod oauth;
pub mod proto;
pub mod token_store;

use std::sync::Arc;

use vike_bridge_core::data_client::bus::{DataClientBus, DataClientBusSink};
use vike_data::DataClientSink;
use vike_execution::EventSender;

use crate::execution::ExecutionClientCtrader;
use crate::feed::DataClientBusSpecCtrader;
use crate::transport::connection::{
    ActorHandle, ConnectionConfig, ConnectionError, connect_and_auth_execution,
};

/// Owns ONE authenticated cTrader connection and vends `DataClient`/`ExecutionClient` views that
/// share it. See the module doc for the ownership/lifecycle contract. The `events` clone it holds
/// is the SAME lane wired into the actor at [`connect`](CtraderClient::connect), so an exec view's
/// synchronous `OrderSubmitted`/synthetic-reject emits and the actor's async venue events land on
/// one ingest lane, in order.
pub struct CtraderClient {
    handle: ActorHandle,
    events: EventSender,
    /// The fixed sink the actor delivers into; a data view attaches to it.
    relay: Arc<DataClientBusSink>,
    /// Where a data view's prices go.
    sink: Arc<dyn DataClientSink>,
}

impl CtraderClient {
    /// Open ONE authenticated socket (two-stage auth + symbol discovery, exec-wired) and wrap it.
    /// `sink` receives the live quotes/bars decoded on the actor thread, through the data view's
    /// engine; `events` is the ingest lane both the actor (async venue events) and any
    /// [`exec`](CtraderClient::exec) view (synchronous emits) push onto. Returns the same
    /// `ConnectionError`s as the underlying handshake; on failure NO socket/thread is left behind.
    pub fn connect(
        config: ConnectionConfig,
        sink: Arc<dyn DataClientSink>,
        events: EventSender,
    ) -> Result<Self, ConnectionError> {
        let relay = Arc::new(DataClientBusSink::default());
        let actor_sink = Arc::clone(&relay) as Arc<dyn DataClientSink>;
        let handle = connect_and_auth_execution(config, actor_sink, events.clone())?;
        Ok(Self { handle, events, relay, sink })
    }

    /// A `DataClient` VIEW over the shared socket. Does NOT own the actor thread — dropping the
    /// returned client leaves the socket up for a co-mounted exec view; only the `CtraderClient`
    /// closes it. Call once: the prices go to the latest view.
    pub fn data(&self) -> DataClientBus<DataClientBusSpecCtrader> {
        let view = DataClientBusSpecCtrader::view(self.handle.shared(), Arc::clone(&self.relay));
        DataClientBus::on(&Arc::new(view), Arc::clone(&self.sink))
    }

    /// An `ExecutionClient` VIEW over the shared socket, carrying a clone of the SAME `EventSender`
    /// wired into the actor at [`connect`](CtraderClient::connect) (so synchronous emits and async
    /// venue events stay on one ordered lane). Does NOT own the actor thread — see [`data`](Self::data).
    pub fn exec(&self) -> ExecutionClientCtrader {
        ExecutionClientCtrader::from_shared(self.handle.shared(), self.events.clone())
    }

    /// The resolved cTrader account id (`ctidTraderAccountId`) this connection authenticated.
    pub fn ctid(&self) -> i64 {
        self.handle.ctid
    }

    /// Graceful teardown: `Command::Shutdown` to the actor then join its thread. Consumes `self`, so
    /// any outstanding views become inert (their channel sends fail silently). Equivalent to simply
    /// dropping the `CtraderClient` (the owned [`ActorHandle`]'s `Drop` does the same) — provided for
    /// deterministic teardown at a known point.
    pub fn shutdown(self) {
        self.handle.shutdown();
    }
}
