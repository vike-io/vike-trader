//! `RemoteCoreHandle` — the read-only, push-fed thin-client twin of `vike_core::CoreHandle`
//! (headless two-layer plan, Layer 2, PR-11).
//!
//! A laptop GUI connects this handle to a live headless node's observe server, authenticates under
//! [`Scope::Observe`], subscribes, and thereafter reads the latest pushed [`WireSnapshot`] off a
//! LOCAL arc-swap cell — exactly the lossy, never-blocking read the in-process GUI already does over
//! `CoreHandle::snapshot()`. A background receive thread decodes [`Response::SnapshotFrame`]s and
//! stores each into the cell (latest-wins); [`RemoteCoreHandle::snapshot`] loads it. Dropped frames
//! on a slow link are fine — the observer is a lossy downstream reader by contract (steal S6/S9).
//!
//! # RESOLVED DESIGN — `snapshot()` returns `Arc<WireSnapshot>`, NOT `Arc<CoreSnapshot>`
//!
//! Returning `Arc<vike_core::CoreSnapshot>` would force this LIGHT client crate to depend on
//! `vike-core` (the whole live-runtime tree: tokio, the execution engines), defeating the entire
//! reason PR-10 made this crate DataFusion-free and dependency-light. So the client stays light and
//! exposes the wire type [`WireSnapshot`] directly. The "interface-identical to `CoreHandle` for the
//! GUI panels" adapter — the shim that presents a `RemoteCoreHandle` as if it were a `CoreHandle` —
//! is part of the DEFERRED `vike-app --observe` follow-up (that crate already links `vike-core`, so
//! it can bridge the two snapshot shapes there), not this crate.
//!
//! # No control path here (PR-11 is read-only)
//!
//! This handle only observes. A remote `try_command`/`send_command` (the write half, pre-minting the
//! coid client-side) lands in PR-13 on top of PR-12's authenticated control channel.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use arc_swap::ArcSwap;

use crate::handshake::node_handshake;
use crate::proto::{
    read_frame, write_frame, Request, Response, Scope, Topic, FEATURE_SETTINGS_SHOW,
    FEATURE_STRATEGY_VERBS,
};
use crate::wire::{WireSettingsShow, WireSnapshot, WireStrategyStatus};

/// A connected, authenticated, read-only observer of a remote headless node.
///
/// Construct with [`RemoteCoreHandle::connect`]. It owns a background receive thread that keeps a
/// local arc-swap cell current from the node's pushed frames; [`snapshot`](Self::snapshot) reads that
/// cell without ever blocking or back-pressuring the node. Drop it to tear down the connection and
/// join the receive thread.
pub struct RemoteCoreHandle {
    /// The latest snapshot the node pushed (latest-wins). Read by [`Self::snapshot`], written by the
    /// receive thread — a lossy hand-off, the wire twin of the core's own publish cell.
    cell: Arc<ArcSwap<WireSnapshot>>,
    /// `false` once the receive thread has exited (EOF / transport fault / local shutdown).
    connected: Arc<AtomicBool>,
    /// A second handle to the same socket, kept ONLY so [`Drop`] can `shutdown` it and thereby unblock
    /// the receive thread's blocking `read_frame`, so the thread exits promptly.
    shutdown_stream: TcpStream,
    /// The receive thread's join handle, taken and joined in [`Drop`].
    recv: Option<JoinHandle<()>>,
    /// The datahub dial address the node's `Welcome.features` advertised at connect
    /// (`datahub=<addr>`, split-plane REQ-2), parsed once by
    /// [`crate::proto::advertised_datahub`]. `None` from a node with no
    /// `datahub_advertise_addr` configured (or one that predates the advertisement). Fixed for
    /// this connection's lifetime — the handshake happens once.
    advertised_datahub: Option<String>,
}

impl RemoteCoreHandle {
    /// Connect to a node's observe server at `addr`, run the full handshake under [`Scope::Observe`]
    /// using `observe_key` (the raw bytes of `VIKE_TRADEHUB_OBSERVE_KEY`), subscribe to every block,
    /// and spawn the receive thread.
    ///
    /// The handshake is the shared `crate::handshake` sequence run under [`Scope::Observe`]: send
    /// [`Request::Hello`], read the [`Response::Welcome`] challenge nonce (failing — naming both
    /// versions — on a protocol-version mismatch), sign the nonce, send [`Request::Auth`], and
    /// require [`Response::AuthOk`]. A [`Response::AuthDenied`] (wrong key, or the server refusing the
    /// scope) surfaces as [`io::ErrorKind::PermissionDenied`] carrying the server's reason. On success
    /// a [`Request::Subscribe`] for [`Topic::All`] opens the push stream.
    pub fn connect<A: ToSocketAddrs>(addr: A, observe_key: &[u8]) -> io::Result<Self> {
        // 1-2. The shared node handshake (Hello -> Welcome version guard -> sign -> Auth -> AuthOk)
        //      under Observe — one sequence for every connection path (see `crate::handshake`).
        //      Every verb this handle SENDS (Subscribe) predates feature negotiation and every
        //      server speaks it; the features are kept for the one value they CARRY — the
        //      `datahub=<addr>` advertisement (split-plane REQ-2), parsed here once and exposed
        //      via [`Self::advertised_datahub`].
        let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
        let advertised_datahub = crate::proto::advertised_datahub(&features);

        // 3. Subscribe to every block — the node pushes whole coalesced snapshots.
        write_frame(&mut stream, &Request::Subscribe { topics: vec![Topic::All] })?;

        // 4. Spawn the receive thread over one clone; keep the other for a Drop-time shutdown.
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
                    // Blocking decode loop: each SnapshotFrame overwrites the cell (latest-wins).
                    // Any other reply (a Pong, etc.) is ignored — PR-11 only ever receives pushes.
                    // A read error / EOF / a Drop-triggered socket shutdown ends the loop.
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

    /// The datahub dial address this node's `Welcome.features` advertised at connect
    /// (split-plane REQ-2: the backend fronts a `vike-datahub` and names where a client should
    /// dial it — advertisement, never proxying). `None` from a node with no
    /// `datahub_advertise_addr` configured or one that predates the advertisement. Captured once
    /// at the handshake, so it is only as live as this connection: a reconnect (a fresh
    /// `connect`) re-reads it from the next node's Welcome.
    pub fn advertised_datahub(&self) -> Option<&str> {
        self.advertised_datahub.as_deref()
    }

    /// The latest snapshot the node pushed — a lossy, never-blocking read off the local arc-swap
    /// cell, exactly like the in-process GUI's `CoreHandle::snapshot()`. Before the first frame
    /// arrives (or after a disconnect with no newer frame) this is [`WireSnapshot::empty`].
    pub fn snapshot(&self) -> Arc<WireSnapshot> {
        self.cell.load_full()
    }

    /// `true` while the receive thread is alive (the connection is delivering frames). Flips to
    /// `false` once the node closes the stream or a transport fault ends the receive loop — a
    /// supervisor probe for the GUI, mirroring `CoreHandle::is_alive`.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

impl Drop for RemoteCoreHandle {
    fn drop(&mut self) {
        // Unblock the receive thread's blocking `read_frame` by shutting the shared socket, then join
        // it so no thread outlives the handle. A best-effort shutdown — the socket may already be
        // closed by the peer.
        let _ = self.shutdown_stream.shutdown(Shutdown::Both);
        if let Some(join) = self.recv.take() {
            let _ = join.join();
        }
    }
}

/// Ask a node WHAT IT IS RUNNING — the STRATEGY-level read verb (split-plane B4), the read-only
/// sibling of [`crate::remote_control::preview_command`]'s per-call shape: open a fresh short-lived
/// [`Scope::Observe`] connection (the same handshake [`RemoteCoreHandle::connect`] runs, minus the
/// `Subscribe`), send [`Request::StrategyStatus`], read the [`Response::StrategyStatus`] payload,
/// and drop the connection. A per-call connection is correct for the same reason it is for a
/// preview: a single request/response with no ordering concern.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: the request is sent ONLY when the server's
/// `Welcome.features` advertises [`FEATURE_STRATEGY_VERBS`]. Against an older node the verb does
/// not exist — its serde cannot decode the frame — so this fails with
/// [`io::ErrorKind::Unsupported`] naming the missing capability, and NOTHING is sent after the
/// handshake. A handshake failure (wrong/absent observe key, version skew, transport fault)
/// surfaces as its usual [`io::Error`]; a server-side [`Response::Error`] (e.g. a node publishing
/// no identity block) surfaces as [`io::ErrorKind::InvalidData`] carrying the server's text.
pub fn strategy_status<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireStrategyStatus> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
    if !features.iter().any(|f| f == FEATURE_STRATEGY_VERBS) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{FEATURE_STRATEGY_VERBS}\" capability \
                 (an older vike-tradehub) — StrategyStatus refused client-side, nothing was sent"
            ),
        ));
    }
    write_frame(&mut stream, &Request::StrategyStatus)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::StrategyStatus(status) => Ok(*status),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub strategy status error: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tradehub strategy status: expected StrategyStatus, got {}",
                crate::handshake::resp_kind(&other)
            ),
        )),
    }
}

/// Ask a node for its EFFECTIVE SETTINGS — the REQ-7 read verb, [`strategy_status`]'s exact
/// per-call shape: open a fresh short-lived [`Scope::Observe`] connection (the same handshake
/// [`RemoteCoreHandle::connect`] runs, minus the `Subscribe`), send [`Request::SettingsShow`],
/// read the [`Response::SettingsShow`] payload, and drop the connection. A per-call connection is
/// correct for the same reason it is there: a single request/response with no ordering concern.
///
/// **Feature-negotiated, refused CLIENT-SIDE**: the request is sent ONLY when the server's
/// `Welcome.features` advertises [`FEATURE_SETTINGS_SHOW`]. Against an older node the verb does
/// not exist — its serde cannot decode the frame — so this fails with
/// [`io::ErrorKind::Unsupported`] naming the missing capability, and NOTHING is sent after the
/// handshake (the GUI renders that as "server predates settings-show"). A handshake failure
/// surfaces as its usual [`io::Error`]; a server-side [`Response::Error`] (a node started without
/// a settings source, or settings that no longer load) surfaces as
/// [`io::ErrorKind::InvalidData`] carrying the server's text.
pub fn settings_show<A: ToSocketAddrs>(
    addr: A,
    observe_key: &[u8],
) -> io::Result<WireSettingsShow> {
    let (mut stream, features) = node_handshake(addr, observe_key, Scope::Observe)?;
    if !features.iter().any(|f| f == FEATURE_SETTINGS_SHOW) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!(
                "this node does not advertise the \"{FEATURE_SETTINGS_SHOW}\" capability \
                 (an older vike-tradehub) — SettingsShow refused client-side, nothing was sent"
            ),
        ));
    }
    write_frame(&mut stream, &Request::SettingsShow)?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::SettingsShow(show) => Ok(*show),
        Response::Error(msg) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("tradehub settings show error: {msg}"),
        )),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "tradehub settings show: expected SettingsShow, got {}",
                crate::handshake::resp_kind(&other)
            ),
        )),
    }
}
