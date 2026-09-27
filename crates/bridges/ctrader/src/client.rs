//! The single-socket combined data+exec mount (F4). cTrader multiplexes market data AND order flow
//! over ONE authenticated protobuf/TLS socket; [`CtraderClient`] owns exactly one such connection
//! (the [`ActorHandle`] returned by [`conn::connect_and_auth_exec`](crate::conn::connect_and_auth_exec))
//! and hands out lightweight VIEWS — a [`CtraderData`] (`DataClient`) and a [`CtraderExec`]
//! (`ExecutionClient`) — that both drive that ONE actor/socket via a cloned [`ConnShared`]
//! (crate::conn::ConnShared).
//!
//! ## Why this exists
//! The single-client entry points ([`CtraderData::new`]/[`CtraderExec::new`]) each OWN their own
//! [`ActorHandle`] → their own socket + actor thread. Mounting both that way opens TWO sockets to
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

use std::sync::Arc;

use vike_data::LiveDataSink;
use vike_exec::EventSender;

use crate::conn::{ActorHandle, ConnConfig, ConnError, connect_and_auth_exec};
use crate::data::CtraderData;
use crate::exec::CtraderExec;

/// Owns ONE authenticated cTrader connection and vends `DataClient`/`ExecutionClient` views that
/// share it. See the module doc for the ownership/lifecycle contract. The `events` clone it holds
/// is the SAME lane wired into the actor at [`connect`](CtraderClient::connect), so an exec view's
/// synchronous `OrderSubmitted`/synthetic-reject emits and the actor's async venue events land on
/// one ingest lane, in order.
pub struct CtraderClient {
    handle: ActorHandle,
    events: EventSender,
}

impl CtraderClient {
    /// Open ONE authenticated socket (two-stage auth + symbol discovery, exec-wired) and wrap it.
    /// `sink` receives the live quotes/bars decoded on the actor thread; `events` is the ingest lane
    /// both the actor (async venue events) and any [`exec`](CtraderClient::exec) view (synchronous
    /// emits) push onto. Returns the same `ConnError`s as the underlying handshake; on failure NO
    /// socket/thread is left behind.
    pub fn connect(
        cfg: ConnConfig,
        sink: Arc<dyn LiveDataSink>,
        events: EventSender,
    ) -> Result<Self, ConnError> {
        let handle = connect_and_auth_exec(cfg, sink, events.clone())?;
        Ok(Self { handle, events })
    }

    /// A `DataClient` VIEW over the shared socket. Does NOT own the actor thread — dropping the
    /// returned [`CtraderData`] leaves the socket up for a co-mounted exec view; only the
    /// `CtraderClient` closes it. Cheap; call once per logical data consumer.
    pub fn data(&self) -> CtraderData {
        CtraderData::from_shared(self.handle.shared())
    }

    /// An `ExecutionClient` VIEW over the shared socket, carrying a clone of the SAME `EventSender`
    /// wired into the actor at [`connect`](CtraderClient::connect) (so synchronous emits and async
    /// venue events stay on one ordered lane). Does NOT own the actor thread — see [`data`](Self::data).
    pub fn exec(&self) -> CtraderExec {
        CtraderExec::from_shared(self.handle.shared(), self.events.clone())
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
