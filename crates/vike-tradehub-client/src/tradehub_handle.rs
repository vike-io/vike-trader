//! `TradehubCoreHandle` — the read-only, push-fed thin-client twin of `vike_core::CoreHandle`, plus
//! the one-shot reads (private child `reads`, re-exported here by name).
//!
//! The handle connects to a node's observe server, authenticates under [`Scope::Read`], subscribes,
//! and a background receive thread stores each [`Response::SnapshotFrame`] into a LOCAL arc-swap
//! cell (latest-wins) that [`TradehubCoreHandle::snapshot`] loads without blocking. Dropped frames on
//! a slow link are fine: the observer is a lossy downstream reader by contract, and it says when it has
//! read one so the node sends the newest next ([`crate::proto::FEATURE_OBSERVE_NEWEST`]) instead of
//! filling the link with frames it would read ever later. A node that predates that still fills it,
//! and [`TradehubCoreHandle::frame_age`] shows it. A node of this build is asked for what this reader can read beyond
//! that ([`crate::proto::Request::SubscribeWith`]): a compressed body, patches after a whole snapshot, and the bar series
//! it shows ([`crate::observe_stream`]). The GUI has no core of its own; this handle, run by
//! `vike-app-core`'s observe bridge, is how it sees one, and the REPL's and `vike-mcp`'s reads are the same handle.
//!
//! `snapshot()` returns `Arc<CoreSnapshotWire>`, NOT `Arc<vike_execution::CoreSnapshot>`: the decode is the
//! caller's to run, on its own thread, and a reader that only needs the wire (the CLI, MCP) never pays for it.
//! `crates/vike-app-core/src/backend/observe_bridge.rs`'s `spawn_bridge` connects this handle and converts each
//! frame through [`crate::observe_decode::wire_to_core`]; nothing presents this handle as a `CoreHandle`.
//!
//! Liveness: the stream is ONE-WAY, so against a node advertising
//! [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`] the read is deadlined at
//! [`crate::liveness::OBSERVE_READ_TIMEOUT`] (three missed `Pong`s) by default, or the window the
//! caller's `preferences.observe_stall_timeout_ms` raised it to, and the receive loop's existing
//! error arm ends it. This handle only observes; the write half is `crate::tradehub_control`.

use std::io;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;

use crate::handshake::hub_handshake;
use crate::liveness::OBSERVE_READ_TIMEOUT;
use crate::observe_stream::{ObserveReceiver, Received};
use crate::proto::{
    FEATURE_OBSERVE_COMPRESSED, FEATURE_OBSERVE_HEARTBEAT, FEATURE_OBSERVE_NEWEST,
    FEATURE_OBSERVE_PATCHES, FEATURE_OBSERVE_SERIES, ObserveSubscriptionWire, Request, Response,
    SeriesKeyWire, Topic,
};
use crate::wire::CoreSnapshotWire;
use vike_hub_protocol::access::Signer;
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::frame::{BodyCodec, read_frame_raw, write_frame_as};

mod exchange;
mod reads;

pub use exchange::exchange_once;
pub use reads::{
    directory, settings_show, snapshot_once, strategy_params, strategy_status,
    strategy_status_with_features, venue_read,
};

/// How old a frame in hand is ([`TradehubCoreHandle::frame_age`]), on two clocks, the node's and this box's: read a skew
/// between them into both figures. Negative readings are clamped to zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAge {
    /// Since the node framed it.
    pub age_ms: i64,
    /// How old it already was when it arrived. Large is a link behind. Small beside a large [`Self::age_ms`] is a node that
    /// has pushed nothing newer, which a quiet node does.
    pub arrived_after_ms: i64,
}

impl FrameAge {
    /// `None` for a frame with no stamp: the client's placeholder, or a node that predates it.
    pub fn of(published_ms: i64, received_ms: i64, now_ms: i64) -> Option<Self> {
        (published_ms > 0).then(|| FrameAge {
            age_ms: (now_ms - published_ms).max(0),
            arrived_after_ms: (received_ms - published_ms).max(0),
        })
    }
}

fn wall_clock_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

/// A connected, authenticated, read-only observer of a remote headless node.
///
/// Construct with [`TradehubCoreHandle::connect`]; [`snapshot`](Self::snapshot) reads the local cell
/// without ever blocking or back-pressuring the node. Drop it to close the connection and join the
/// receive thread.
pub struct TradehubCoreHandle {
    /// The latest snapshot the node pushed (latest-wins), written by the receive thread.
    cell: Arc<ArcSwap<CoreSnapshotWire>>,
    /// When the frame in `cell` arrived, on this box's clock.
    received_ms: Arc<AtomicI64>,
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
    /// The half the reader's words go back on (a frame read, a missed patch, the series shown), shared by the receive
    /// thread and [`Self::show_series`]; `None` when the node takes no word.
    says: Option<Arc<Mutex<TcpStream>>>,
    /// The node takes [`Request::ObserveSeries`] (`FEATURE_OBSERVE_SERIES`).
    series_heard: bool,
    /// What this connection's requests are written in: msgpack to a node advertising it, else JSON.
    codec: BodyCodec,
}

impl TradehubCoreHandle {
    /// Connect to a node's observe server at `addr`, run the shared handshake under [`Scope::Read`]
    /// with `observe_key`, send a [`Request::Subscribe`] for [`Topic::All`], and spawn the receive
    /// thread. A refused key is [`HubCallError::AccessDenied`] carrying the server's reason (the
    /// handshake's variants: `crate::handshake`'s module doc); a failure once the subscription began to
    /// leave is [`HubCallError::OutcomeUnknown`]. The silent-link window is the built-in
    /// [`OBSERVE_READ_TIMEOUT`]; a root that reads settings dials [`Self::connect_with_read_timeout`]
    /// with its resolved window.
    pub fn connect<'k, A: ToSocketAddrs>(
        addr: A,
        observe_key: impl Into<Signer<'k>>,
    ) -> Result<Self, HubCallError> {
        Self::connect_with_read_timeout(addr, observe_key, OBSERVE_READ_TIMEOUT)
    }

    /// [`Self::connect`] with the silent-link deadline as a PARAMETER: the operator's patience on
    /// this link ([`crate::liveness::LinkPatience::observe_stall`], resolved from
    /// `preferences.observe_stall_timeout_ms` by the BINARY that dials), and the seam the unit tests
    /// drive in milliseconds. Every production root passes its resolved patience here; `connect` is
    /// the default window for a caller with no setting to read.
    ///
    /// ⚠ The caller owes the floor: a window below [`OBSERVE_READ_TIMEOUT`] tears healthy loaded
    /// links down, which is why the settings layer refuses such a row at the write and the load and
    /// this function does not clamp.
    ///
    /// ⚠ The deadline is armed ONLY when the node advertises
    /// [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`], whatever is passed here: a node that does not
    /// heartbeat would have every healthy quiet stream ended each window and reconnected. Against
    /// such a node the read blocks until a reported close, as it always did.
    pub fn connect_with_read_timeout<'k, A: ToSocketAddrs>(
        addr: A,
        observe_key: impl Into<Signer<'k>>,
        read_timeout: Duration,
    ) -> Result<Self, HubCallError> {
        Self::connect_with_series(addr, observe_key, read_timeout, None)
    }

    /// [`Self::connect_with_read_timeout`] stating the bar SERIES this reader shows (`None`: every series), so a node
    /// advertising `FEATURE_OBSERVE_SERIES` sends the bars of those alone from the first frame; [`Self::show_series`]
    /// changes the set later. A node without the word sends every series, as it always did.
    pub fn connect_with_series<'k, A: ToSocketAddrs>(
        addr: A,
        observe_key: impl Into<Signer<'k>>,
        read_timeout: Duration,
        series: Option<Vec<SeriesKeyWire>>,
    ) -> Result<Self, HubCallError> {
        // Subscribe predates feature negotiation; the features are kept for the two values they
        // carry: the datahub advertisement and the heartbeat capability.
        let (mut stream, features, _) = hub_handshake(addr, observe_key, Scope::Read)?;
        let advertised_datahub = crate::proto::advertised_datahub(&features);
        // Every word of this connection goes in the node's form; the frames back are read in whichever arrives.
        let codec = crate::proto::node_codec(&features);

        // Subscribe to every block — the node pushes whole coalesced snapshots. A node that can hold this reader to one
        // frame on the way is asked to: what it then reads is the newest there was when it finished the last, whatever
        // the link (`FEATURE_OBSERVE_NEWEST`).
        let newest = features.iter().any(|f| f == FEATURE_OBSERVE_NEWEST);
        let topics = vec![Topic::All];
        // A node that can compress is asked to (`FEATURE_OBSERVE_COMPRESSED`); its own row decides whether it does, and
        // the receive loop reads either body. One that sends patches is asked for them (`FEATURE_OBSERVE_PATCHES`):
        // the receive loop rebuilds the same snapshot from them. A node without the words is subscribed to as before,
        // byte for byte.
        let shaped = features.iter().any(|f| f == FEATURE_OBSERVE_COMPRESSED);
        let patches = shaped && features.iter().any(|f| f == FEATURE_OBSERVE_PATCHES);
        let series_heard = shaped && features.iter().any(|f| f == FEATURE_OBSERVE_SERIES);
        let subscribe = if shaped {
            Request::SubscribeWith(ObserveSubscriptionWire {
                topics,
                newest,
                compressed: true,
                patches,
                series: series.filter(|_| series_heard),
            })
        } else if newest {
            Request::SubscribeNewest { topics }
        } else {
            Request::Subscribe { topics }
        };
        // From here the subscription has begun to leave: a failure is the link's, after the request.
        write_frame_as(&mut stream, codec, &subscribe).map_err(HubCallError::OutcomeUnknown)?;
        // The reader's words go back on this half: a frame read, a missed patch, the series shown.
        let says = (newest || patches || series_heard)
            .then(|| stream.try_clone().map(|half| Arc::new(Mutex::new(half))))
            .transpose()
            .map_err(HubCallError::OutcomeUnknown)?;

        // THE SILENT-DEATH DEADLINE: a link that dies without a packet (laptop sleep, Wi-Fi change,
        // VPN re-key under `ssh -L`) reports nothing, so without a deadline `is_connected` stayed
        // true forever. The deadline turns that silence into the loop's existing error arm.
        let heartbeats = features.iter().any(|f| f == FEATURE_OBSERVE_HEARTBEAT);
        if heartbeats {
            stream.set_read_timeout(Some(read_timeout)).map_err(HubCallError::OutcomeUnknown)?;
        }

        // Spawn the receive thread over one clone; keep the other for a Drop-time shutdown.
        let cell = Arc::new(ArcSwap::from_pointee(CoreSnapshotWire::empty()));
        let received_ms = Arc::new(AtomicI64::new(0));
        let connected = Arc::new(AtomicBool::new(true));
        let shutdown_stream = stream.try_clone().map_err(HubCallError::OutcomeUnknown)?;

        let recv = {
            let cell = Arc::clone(&cell);
            let received_ms = Arc::clone(&received_ms);
            let connected = Arc::clone(&connected);
            let says = says.clone();
            let mut read_stream = stream;
            std::thread::Builder::new()
                .name("remote-receive".into())
                .spawn(move || {
                    // Each SnapshotFrame (or the state a SnapshotPatch rebuilds) overwrites the cell. Any
                    // other reply is dropped, including the heartbeat `Pong`: it proves the link (the read
                    // returned, so the deadline did not fire), never a change. Error / EOF / deadline /
                    // shutdown ends it.
                    let mut receiver = ObserveReceiver::default();
                    loop {
                        let frame = read_frame_raw(&mut read_stream)
                            .and_then(|body| crate::observe_stream::decode_response(&body, codec));
                        let Ok(response) = frame else { break };
                        let is_frame = matches!(
                            response,
                            Response::SnapshotFrame(_) | Response::SnapshotPatch(_)
                        );
                        let mut words = Vec::new();
                        match receiver.receive(response) {
                            Received::Snapshot(wire) => {
                                received_ms.store(wall_clock_ms(), Ordering::Release);
                                cell.store(Arc::new(*wire));
                            }
                            Received::Resync => words.push(Request::ObserveResync),
                            Received::Nothing => {}
                        }
                        // Every frame is read, whether or not it held state: the node sends the next only then.
                        if newest && is_frame {
                            words.push(Request::FrameRead);
                        }
                        let said = says.as_ref().map_or(Ok(()), |back| {
                            let mut back = back.lock().unwrap_or_else(PoisonError::into_inner);
                            words
                                .iter()
                                .try_for_each(|word| write_frame_as(&mut *back, codec, word))
                        });
                        if said.is_err() {
                            break;
                        }
                    }
                    connected.store(false, Ordering::Release);
                })
                .expect("spawn remote-receive thread")
        };

        Ok(TradehubCoreHandle {
            cell,
            received_ms,
            connected,
            shutdown_stream,
            recv: Some(recv),
            advertised_datahub,
            says,
            series_heard,
            codec,
        })
    }

    /// **Tell the node which bar series this reader shows now** (a chart opened or closed; `None`: every series). `Ok`
    /// and nothing sent to a node without `FEATURE_OBSERVE_SERIES`, which sends every series whatever is said; an
    /// error is the link's, and the receive loop ends with it.
    pub fn show_series(&self, series: Option<Vec<SeriesKeyWire>>) -> io::Result<()> {
        match &self.says {
            Some(back) if self.series_heard => write_frame_as(
                &mut *back.lock().unwrap_or_else(PoisonError::into_inner),
                self.codec,
                &Request::ObserveSeries { series },
            ),
            _ => Ok(()),
        }
    }

    /// The datahub dial address this node's `Welcome.features` advertised at connect (the backend
    /// names where a client should dial its `vike-datahub` — advertisement, never proxying).
    /// `None` from a node with none configured or one that predates it. Only as live as this
    /// connection: a reconnect re-reads it from the next node's Welcome.
    pub fn advertised_datahub(&self) -> Option<&str> {
        self.advertised_datahub.as_deref()
    }

    /// The latest snapshot the node pushed — a lossy, never-blocking read off the local cell, like
    /// an in-process `CoreHandle::snapshot()`. [`CoreSnapshotWire::empty`] before the first frame.
    pub fn snapshot(&self) -> Arc<CoreSnapshotWire> {
        self.cell.load_full()
    }

    /// How old the frame [`Self::snapshot`] answers is now, `None` for one with no stamp ([`FrameAge::of`]).
    pub fn frame_age(&self) -> Option<FrameAge> {
        let published_ms = self.cell.load().published_ms;
        FrameAge::of(published_ms, self.received_ms.load(Ordering::Acquire), wall_clock_ms())
    }

    /// `true` while the receive thread is alive; `false` once the node closes the stream or a
    /// transport fault ends the loop — a supervisor probe, mirroring `CoreHandle::is_alive`.
    ///
    /// ⚠ Against a node advertising [`crate::proto::FEATURE_OBSERVE_HEARTBEAT`] it also flips when
    /// nothing arrives for the window it was dialled with ([`OBSERVE_READ_TIMEOUT`] by default), so
    /// a SILENT death is bounded by that window.
    /// Against a node that does NOT advertise it, only a REPORTED close flips it and a silent death
    /// is not bounded at all (no keepalive backstop, see `crate::liveness`) — say so when reporting
    /// on a mixed-version deployment.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }
}

impl Drop for TradehubCoreHandle {
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
