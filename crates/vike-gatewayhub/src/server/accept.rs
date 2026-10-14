//! The listener: the accept loop, the connection cap, and the graceful stop.
//!
//! [`serve`] owns a bound loopback listener and runs until the stop flag the boot armed
//! (`vike_boot::stop::StopSignal::flag`, raised by SIGTERM, SIGINT or the stdio word) goes up. It is
//! deliberately unlike the datahub's `serve_inner`, whose loop "has no teardown at ALL" and whose
//! stop was always SIGKILL: this daemon holds venue sessions and credentials, and a stop must reach
//! every connection (a stream session writes its `Bye(ServerStopping)`, a running fetch is
//! cancelled), join their threads, and only then return, so the root can run its own teardown after.
//!
//! # How a blocked `accept` learns of the stop
//!
//! The std listener has no timeout and a signal handler may only store a bool, so a named watcher
//! thread (`gwh-stop`) polls the flag every `vike_boot::stop::STOP_POLL` and, when it goes up, wakes the
//! accept loop with a connection to the listener's own (loopback) address. The loop sees its internal
//! `stopping` flag, drops that connection and leaves. The watcher repeats the wake until the loop has
//! really left, so one refused connect cannot strand it.
//!
//! # The connection cap
//!
//! A slot is taken BEFORE the thread is spawned and released by `Drop`
//! (`crate::server::stats::Slot`), so a burst of accepts cannot race the count past the cap and a
//! panicking connection thread cannot leak a place. Past the cap a connection is dropped WITHOUT a
//! reply: writing an error frame would spend a thread and an allocation on the peer that is already
//! exhausting them, and would answer an unauthenticated stranger.
//!
//! # Refusing to start unsafe
//!
//! No keys, no start (the tradehub's rule): a daemon that holds credentials and offers no
//! authentication would let any local process spend them. And a listener bound off-loopback is
//! refused here as well as at `crate::server::bind::bind_loopback`, so a root that bound its own socket
//! cannot serve on a public one.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_boot::stop::STOP_POLL;
use vike_hub_proto::auth::{HubKeys, Scope};

use super::bind::check_listener;
use super::connection::handle_connection;
use super::ctl::ConnRegistry;
use super::features::served_features;
use super::lanes::Lanes;
use super::limits::ServerConfig;
use super::shared::Shared;
use super::stats::Stats;

/// How long a failed `accept` backs off, so a persistent fault (descriptor exhaustion) is a slow log
/// line and not a hot loop.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// Serve `listener` until `stop` is raised, then drain. Blocks the calling thread.
///
/// `keys` must hold at least one of the `Read` and `Write` keys. `lanes` are asked for their build
/// facts ONCE, here; the `Welcome` every connection gets is computed from them and never changes.
pub fn serve(
    listener: TcpListener,
    keys: HubKeys,
    lanes: Lanes,
    config: ServerConfig,
    stop: Arc<AtomicBool>,
) -> io::Result<()> {
    if !keys.has(Scope::Read) && !keys.has(Scope::Write) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "vike-gatewayhub: no hub key is configured, so the daemon refuses to start: it holds \
             venue credentials and has no key-less mode (decision 0164, verdict 6)",
        ));
    }
    check_listener(&listener).map_err(io::Error::from)?;
    let local = listener.local_addr()?;

    let data_venues = lanes.data.venues();
    let rest = lanes.rest.facts();
    let features = served_features(&data_venues, &rest);
    tracing::info!(
        addr = %local,
        keys = ?keys,
        features = ?features,
        "vike-gatewayhub: serving (AUTHENTICATION REQUIRED; the `Debug` of the keys reports presence only)"
    );
    let shared = Arc::new(Shared {
        keys,
        lanes,
        features,
        data_venues,
        rest,
        config,
        stats: Stats::new(),
        registry: ConnRegistry::new(),
    });

    let stopping = Arc::new(AtomicBool::new(false));
    let accept_done = Arc::new(AtomicBool::new(false));
    let watcher = spawn_watcher(local, stop, Arc::clone(&stopping), Arc::clone(&accept_done))?;

    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    for incoming in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let stream = match incoming {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!(error = %e, "vike-gatewayhub: accept failed, continuing");
                std::thread::sleep(ACCEPT_BACKOFF);
                continue;
            }
        };
        let slot = shared.stats.connections.enter();
        if shared.stats.connections.get() > shared.config.limits.max_connections {
            tracing::warn!(
                peer = ?stream.peer_addr().ok(),
                cap = shared.config.limits.max_connections,
                "vike-gatewayhub: connection REFUSED - already at the connection cap; dropping \
                 without a reply"
            );
            continue;
        }
        let socket = match stream.try_clone() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "vike-gatewayhub: could not clone an accepted socket; dropping it");
                continue;
            }
        };
        let Some((ctl, registration)) = shared.registry.register(socket) else {
            continue;
        };
        handles.retain(|h| !h.is_finished());
        let for_thread = Arc::clone(&shared);
        let spawned = std::thread::Builder::new().name("gwh-conn".into()).spawn(move || {
            let _slot = slot;
            let _registration = registration;
            handle_connection(&for_thread, stream, ctl);
        });
        match spawned {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                tracing::warn!(error = %e, "vike-gatewayhub: could not spawn a connection thread")
            }
        }
    }
    accept_done.store(true, Ordering::SeqCst);

    // The drain: tell every connection, wait for the threads, join what finished.
    tracing::info!(connections = shared.registry.len(), "vike-gatewayhub: stopping");
    shared.registry.stop_all();
    // Two steps inside the one budget: the polite interrupt gets the first half, and whatever is still
    // running (a writer blocked on a peer that stopped reading) has its socket cut for the second.
    let budget = shared.config.timings.stop_drain;
    let started = Instant::now();
    let mut cut = false;
    while !handles.iter().all(JoinHandle::is_finished) && started.elapsed() < budget {
        if !cut && started.elapsed() >= budget / 2 {
            shared.registry.hard_close_all();
            cut = true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut left_behind = 0usize;
    for handle in handles {
        if handle.is_finished() {
            let _ = handle.join();
        } else {
            left_behind += 1;
        }
    }
    if left_behind > 0 {
        tracing::warn!(
            left_behind,
            "vike-gatewayhub: connection threads still inside a venue call at the end of the stop \
             budget; they end when the call returns"
        );
    }
    let _ = watcher.join();
    tracing::info!("vike-gatewayhub: stopped");
    Ok(())
}

/// The stop watcher. See the module doc.
fn spawn_watcher(
    local: SocketAddr,
    stop: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    accept_done: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("gwh-stop".into()).spawn(move || {
        while !stop.load(Ordering::SeqCst) && !accept_done.load(Ordering::SeqCst) {
            std::thread::sleep(STOP_POLL);
        }
        if accept_done.load(Ordering::SeqCst) {
            return;
        }
        stopping.store(true, Ordering::SeqCst);
        // Wake the blocked accept, and keep waking it until it has left.
        while !accept_done.load(Ordering::SeqCst) {
            let _ = TcpStream::connect_timeout(&local, Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(20));
        }
    })
}

#[cfg(test)]
#[path = "accept_tests.rs"]
mod accept_tests;
