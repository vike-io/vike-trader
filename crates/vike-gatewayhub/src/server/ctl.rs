//! Per-connection control: how the daemon's stop reaches a thread parked in a read.
//!
//! A connection thread spends its life blocked in a socket read, a venue call or a queue wait, and none
//! of those looks at a flag. So every connection registers a [`ConnCtl`] with the [`ConnRegistry`], and
//! the stop (`ConnRegistry::stop_all`) calls each one's `interrupt`: it raises the connection's
//! `stopping` flag and runs its WAKE, which is whatever makes THAT connection's blocked call return.
//! The default wake shuts the read half of the socket (an idle `rpc` connection reads EOF and ends);
//! a stream session replaces it with "end the session with `Bye(ServerStopping)`", because shutting the
//! socket would cut off the very `Bye` it owes; an options feed the same.
//!
//! Registration and the stop are ordered by one lock: a connection that registers after the stop began
//! is refused ([`ConnRegistry::register`] returns `None`) and one that registered before it is
//! interrupted, so no connection slips between the two.

use std::collections::HashMap;
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

type Wake = Arc<dyn Fn() + Send + Sync>;

/// One connection's handle on the stop.
pub struct ConnCtl {
    id: u64,
    socket: TcpStream,
    stopping: AtomicBool,
    wake: Mutex<Option<Wake>>,
}

impl std::fmt::Debug for ConnCtl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnCtl").field("id", &self.id).finish_non_exhaustive()
    }
}

impl ConnCtl {
    /// Whether the daemon is stopping.
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Replace the default wake (shut the read half of the socket) with `wake`. If the stop already
    /// began, `wake` runs now: a connection must not install its wake after the interrupt passed and
    /// then sleep through the stop.
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let wake: Wake = Arc::new(wake);
        *self.wake.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(&wake));
        if self.is_stopping() {
            wake();
        }
    }

    /// Raise the flag and run the wake. Idempotent.
    pub fn interrupt(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        let wake = self.wake.lock().unwrap_or_else(PoisonError::into_inner).clone();
        match wake {
            Some(wake) => wake(),
            None => {
                let _ = self.socket.shutdown(Shutdown::Read);
            }
        }
    }
}

/// Every connection the daemon is serving.
#[derive(Debug, Default)]
pub struct ConnRegistry {
    next: AtomicU64,
    state: Mutex<RegistryState>,
}

#[derive(Debug, Default)]
struct RegistryState {
    stopping: bool,
    conns: HashMap<u64, Arc<ConnCtl>>,
}

/// A connection's place in the registry; removed on drop (also on a panic).
pub struct Registration {
    id: u64,
    registry: Arc<ConnRegistry>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.registry.state.lock().unwrap_or_else(PoisonError::into_inner).conns.remove(&self.id);
    }
}

impl ConnRegistry {
    /// An empty registry.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Register a connection (`socket` is a clone the stop can shut down). `None` once the stop began.
    pub fn register(self: &Arc<Self>, socket: TcpStream) -> Option<(Arc<ConnCtl>, Registration)> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.stopping {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let ctl = Arc::new(ConnCtl {
            id,
            socket,
            stopping: AtomicBool::new(false),
            wake: Mutex::new(None),
        });
        state.conns.insert(id, Arc::clone(&ctl));
        Some((ctl, Registration { id, registry: Arc::clone(self) }))
    }

    /// Begin the stop: refuse new registrations and interrupt every registered connection.
    pub fn stop_all(&self) {
        let conns: Vec<Arc<ConnCtl>> = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.stopping = true;
            state.conns.values().cloned().collect()
        };
        for ctl in conns {
            ctl.interrupt();
        }
    }

    /// Cut every registered connection's socket, both halves. The second step of the stop, for a
    /// connection the polite interrupt did not move: a thread blocked in a write to a peer that stopped
    /// reading wakes only when its socket is shut.
    pub fn hard_close_all(&self) {
        let conns: Vec<Arc<ConnCtl>> = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .conns
            .values()
            .cloned()
            .collect();
        for ctl in conns {
            let _ = ctl.socket.shutdown(Shutdown::Both);
        }
    }

    /// How many connections are registered.
    pub fn len(&self) -> usize {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).conns.len()
    }

    /// Whether none is.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::time::Duration;

    use super::*;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (server, client)
    }

    #[test]
    fn the_default_wake_ends_a_blocked_read() {
        let registry = ConnRegistry::new();
        let (server, _client) = pair();
        let (ctl, _registration) =
            registry.register(server.try_clone().expect("clone")).expect("registered");
        let reader = std::thread::Builder::new()
            .name("ctl-test-rd".into())
            .spawn(move || {
                let mut server = server;
                let mut byte = [0u8; 1];
                server.read(&mut byte)
            })
            .expect("spawn");
        std::thread::sleep(Duration::from_millis(50));
        registry.stop_all();
        assert!(ctl.is_stopping());
        assert_eq!(reader.join().expect("join").expect("read"), 0, "the read did not see EOF");
    }

    #[test]
    fn a_connection_registering_after_the_stop_is_refused() {
        let registry = ConnRegistry::new();
        registry.stop_all();
        let (server, _client) = pair();
        assert!(registry.register(server).is_none());
    }

    #[test]
    fn a_wake_installed_after_the_stop_runs_at_once_and_a_drop_deregisters() {
        let registry = ConnRegistry::new();
        let (server, _client) = pair();
        let (ctl, registration) = registry.register(server).expect("registered");
        assert_eq!(registry.len(), 1);
        registry.stop_all();
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        ctl.set_wake(move || flag.store(true, Ordering::SeqCst));
        assert!(ran.load(Ordering::SeqCst), "a late wake was never run");
        drop(registration);
        assert!(registry.is_empty());
    }

    #[test]
    fn a_custom_wake_replaces_the_socket_shutdown() {
        let registry = ConnRegistry::new();
        let (server, mut client) = pair();
        let (ctl, _registration) = registry.register(server).expect("registered");
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        ctl.set_wake(move || flag.store(true, Ordering::SeqCst));
        registry.stop_all();
        assert!(ran.load(Ordering::SeqCst));
        // The socket was NOT shut down by the interrupt: the owner of the wake decides when.
        client.set_read_timeout(Some(Duration::from_millis(50))).expect("timeout");
        let mut byte = [0u8; 1];
        let err = client.read(&mut byte).expect_err("nothing was sent and the socket is open");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ));
    }
}
