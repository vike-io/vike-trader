//! `RemoteCoreHandle` — the read-only, push-fed thin-client twin of `vike_core::CoreHandle`, plus
//! the one-shot reads (private child `reads`, re-exported here by name).
//!
//! The handle connects to a node's observe server, authenticates under [`Scope::Read`], subscribes,
//! and a background receive thread stores each [`Response::SnapshotFrame`] into a LOCAL arc-swap
//! cell (latest-wins) that [`RemoteCoreHandle::snapshot`] loads without blocking. Dropped frames on
//! a slow link are fine: the observer is a lossy downstream reader by contract. The GUI has no core
//! of its own; this handle, run by `vike-app-core`'s observe bridge, is how it sees one.
//!
//! `snapshot()` returns `Arc<WireSnapshot>`, NOT `Arc<vike_exec::CoreSnapshot>`, so this LIGHT crate
//! never links `vike-exec`. `crates/vike-app-core/src/backend/observe_bridge.rs`'s `spawn_bridge`
//! connects this handle and converts each frame through that file's `wire_to_core`; nothing
//! presents this handle as a `CoreHandle`.
//!
//! Liveness: the stream is ONE-WAY, so against a node advertising
//! [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`] the read is deadlined at
//! [`crate::liveness::OBSERVE_READ_TIMEOUT`] (three missed `Pong`s) and the receive loop's existing
//! error arm ends it. This handle only observes; the write half is `crate::remote_control`.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;

use crate::handshake::node_handshake;
use crate::liveness::OBSERVE_READ_TIMEOUT;
use crate::proto::{
    FEATURE_OBSERVE_HEARTBEAT, Request, Response, Scope, Topic, read_frame, write_frame,
};
use crate::wire::WireSnapshot;

mod reads;

pub use reads::{
    directory, settings_show, snapshot_once, strategy_params, strategy_status,
    strategy_status_with_features, tearsheet,
};

/// A connected, authenticated, read-only observer of a remote headless node.
///
/// Construct with [`RemoteCoreHandle::connect`]; [`snapshot`](Self::snapshot) reads the local cell
/// without ever blocking or back-pressuring the node. Drop it to close the connection and join the
/// receive thread.
pub struct RemoteCoreHandle {
    /// The latest snapshot the node pushed (latest-wins), written by the receive thread.
    cell: Arc<ArcSwap<WireSnapshot>>,
    /// `false` once the receive thread has exited (EOF / transport fault / deadline / shutdown).
    connected: Arc<AtomicBool>,
    /// A second handle to the same socket, kept ONLY so [`Drop`] can `shutdown` it and unblock the
    /// receive thread's `read_frame`.
    shutdown_stream: TcpStream,
    /// The receive thread's join handle, taken and joined in [`Drop`].
    recv: Option<JoinHandle<()>>,
    /// The `datahub=<addr>` advertisement from `Welcome.features`, parsed once by
    /// [`crate::proto::advertised_datahub`]; fixed for this connection's lifetime.
    advertised_datahub: Option<String>,
}

impl RemoteCoreHandle {
    /// Connect to a node's observe server at `addr`, run the shared handshake under [`Scope::Read`]
    /// with `observe_key`, send a [`Request::Subscribe`] for [`Topic::All`], and spawn the receive
    /// thread. A [`Response::AuthDenied`] surfaces as [`io::ErrorKind::PermissionDenied`] carrying
    /// the server's reason.
    pub fn connect<A: ToSocketAddrs>(addr: A, observe_key: &[u8]) -> io::Result<Self> {
        Self::connect_with_read_timeout(addr, observe_key, OBSERVE_READ_TIMEOUT)
    }

    /// [`Self::connect`] with the silent-link deadline as a PARAMETER — the seam the unit tests
    /// drive in milliseconds. Single-caller in production: the window is a protocol fact paired
    /// with the node's heartbeat cadence, not a caller's choice.
    ///
    /// ⚠ The deadline is armed ONLY when the node advertises
    /// [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`], whatever is passed here: a node that does not
    /// heartbeat would have every healthy quiet stream ended each window and reconnected. Against
    /// such a node the read blocks until a reported close, as it always did.
    pub(crate) fn connect_with_read_timeout<A: ToSocketAddrs>(
        addr: A,
        observe_key: &[u8],
        read_timeout: Duration,
    ) -> io::Result<Self> {
        // Subscribe predates feature negotiation; the features are kept for the two values they
        // carry: the datahub advertisement and the heartbeat capability.
        let (mut stream, features) = node_handshake(addr, observe_key, Scope::Read)?;
        let advertised_datahub = crate::proto::advertised_datahub(&features);

        // Subscribe to every block — the node pushes whole coalesced snapshots.
        write_frame(&mut stream, &Request::Subscribe { topics: vec![Topic::All] })?;

        // THE SILENT-DEATH DEADLINE: a link that dies without a packet (laptop sleep, Wi-Fi change,
        // VPN re-key under `ssh -L`) reports nothing, so without a deadline `is_connected` stayed
        // true forever. The deadline turns that silence into the loop's existing error arm.
        let heartbeats = features.iter().any(|f| f == FEATURE_OBSERVE_HEARTBEAT);
        if heartbeats {
            stream.set_read_timeout(Some(read_timeout))?;
        }

        // Spawn the receive thread over one clone; keep the other for a Drop-time shutdown.
        let cell = Arc::new(ArcSwap::from_pointee(WireSnapshot::empty()));
        let connected = Arc::new(AtomicBool::new(true));
        let shutdown_stream = stream.try_clone()?;

        let recv = {
            let cell = Arc::clone(&cell);
            let connected = Arc::clone(&connected);
            let mut read_stream = stream;
            std::thread::Builder::new()
                .name("vt-remote-recv".into())
                .spawn(move || {
                    // Each SnapshotFrame overwrites the cell. Any other reply is dropped, including
                    // the heartbeat `Pong`: it proves the link (the read returned, so the deadline
                    // did not fire), never a change. Error / EOF / deadline / shutdown ends it.
                    loop {
                        match read_frame::<_, Response>(&mut read_stream) {
                            Ok(Response::SnapshotFrame(wire)) => cell.store(Arc::new(*wire)),
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                    connected.store(false, Ordering::Release);
                })
                .expect("spawn vt-remote-recv thread")
        };

        Ok(RemoteCoreHandle {
            cell,
            connected,
            shutdown_stream,
            recv: Some(recv),
            advertised_datahub,
        })
    }

    /// The datahub dial address this node's `Welcome.features` advertised at connect (the backend
    /// names where a client should dial its `vike-datahub` — advertisement, never proxying).
    /// `None` from a node with none configured or one that predates it. Only as live as this
    /// connection: a reconnect re-reads it from the next node's Welcome.
    pub fn advertised_datahub(&self) -> Option<&str> {
        self.advertised_datahub.as_deref()
    }

    /// The latest snapshot the node pushed — a lossy, never-blocking read off the local cell, like
    /// an in-process `CoreHandle::snapshot()`. [`WireSnapshot::empty`] before the first frame.
    pub fn snapshot(&self) -> Arc<WireSnapshot> {
        self.cell.load_full()
    }

    /// `true` while the receive thread is alive; `false` once the node closes the stream or a
    /// transport fault ends the loop — a supervisor probe, mirroring `CoreHandle::is_alive`.
    ///
    /// ⚠ Against a node advertising [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`] it also flips when
    /// nothing arrives for [`OBSERVE_READ_TIMEOUT`], so a SILENT death is bounded by that window.
    /// Against a node that does NOT advertise it, only a REPORTED close flips it and a silent death
    /// is not bounded at all (no keepalive backstop, see `crate::liveness`) — say so when reporting
    /// on a mixed-version deployment.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

impl Drop for RemoteCoreHandle {
    fn drop(&mut self) {
        // Shut the shared socket to unblock the receive thread's read, then join it (best-effort:
        // the peer may already have closed it).
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(join) = self.recv.take() {
            let _ = join.join();
        }
    }
}

/// The SILENT-DEATH property against a scripted node that completes the real handshake, pushes one
/// frame, then holds the socket open and SILENT (no FIN, no RST: what no cutting relay can plant).
/// Both directions are pinned, since the fix is a NEGOTIATION: a heartbeating node gets a
/// deadlined read, a node that does not keeps the blocking read it always had.
#[path = "silent_link_tests.rs"]
#[cfg(test)]
mod silent_link_tests;
