//! The loopback relay that stands in for the SSH tunnel in front of the node.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::*;

/// The relayed socket pairs a [`Tunnel`] currently holds: each pump's OWN stop flag beside its
/// join handle. Named so the two sites that spell it agree, and because clippy's
/// `type_complexity` (a merge gate on Linux CI, which this Windows box cannot run) refused the
/// inline spelling.
type Links = Arc<Mutex<Vec<(Arc<AtomicBool>, JoinHandle<()>)>>>;

/// A loopback relay standing in front of the node — the SSH tunnel of the remote-node setup.
///
/// Every accepted client socket is paired with a fresh connection to the node and pumped both
/// ways by two [`pump`] threads. The accept loop polls a non-blocking listener and the pumps poll
/// a read timeout, both against one stop flag, so [`Tunnel::cut`] can end every thread and let
/// each one CLOSE its sockets — the far-side close the client handles under test then observe.
/// Dropping the listener is what makes the port refuse the server's next dial (a preview, a
/// reconnect) rather than hang it.
pub(super) struct Tunnel {
    /// Where the [`Server`] under test is pointed.
    pub(super) addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// One entry per relayed socket pair: the pump's OWN stop flag and its join handle. Per-LINK
    /// rather than one shared flag, so [`Tunnel::drop_links`] can end the connections currently
    /// held through the tunnel while the accept loop keeps running and the next dial succeeds.
    links: Links,
    /// How many client connections the accept loop has taken — the one thing a test can observe
    /// about WHEN the server under test dials, without a clock: a fresh observe handle and every
    /// per-call verb (a preview's node dry-run) each open one.
    pub(super) accepted: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl Tunnel {
    /// Open a relay to `node`, bound at `bind` (`127.0.0.1:0` for any port, or a previous
    /// tunnel's address to come back on the SAME port, the way a restarted tunnel does).
    pub(super) fn open(node: SocketAddr, bind: &str) -> io::Result<Tunnel> {
        let listener = TcpListener::bind(bind)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let links: Links = Arc::default();
        let accepted = Arc::new(AtomicUsize::new(0));
        let accept = {
            let stop = Arc::clone(&stop);
            let links = Arc::clone(&links);
            let accepted = Arc::clone(&accepted);
            thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let client = match listener.accept() {
                        Ok((s, _)) => {
                            accepted.fetch_add(1, Ordering::AcqRel);
                            s
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let Ok(upstream) = TcpStream::connect(node) else { continue };
                    // An accepted socket may inherit the listener's non-blocking flag; the pumps
                    // want a blocking read bounded by a timeout, not a spinning one.
                    client.set_nonblocking(false).expect("blocking client side");
                    let (c2, u2) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
                    // ONE flag per relayed pair (both directions share it), raised either by the
                    // whole tunnel dying or by `drop_links` ending just this connection.
                    let link = Arc::new(AtomicBool::new(false));
                    let mut held = links.lock().unwrap();
                    held.push((
                        Arc::clone(&link),
                        thread::spawn({
                            let (stop, link) = (Arc::clone(&stop), Arc::clone(&link));
                            move || pump(client, upstream, &stop, &link)
                        }),
                    ));
                    held.push((
                        Arc::clone(&link),
                        thread::spawn({
                            let (stop, link) = (Arc::clone(&stop), Arc::clone(&link));
                            move || pump(u2, c2, &stop, &link)
                        }),
                    ));
                }
                // Dropping `listener` here is what closes the port.
            })
        };
        Ok(Tunnel { addr, stop, links, accepted, accept: Some(accept) })
    }

    /// The tunnel dies: the port stops accepting and every relayed socket is closed from this
    /// side, so the node sees its peers go and the server under test sees the node go. Joins
    /// everything it spawned, so nothing is still copying bytes when the caller continues.
    pub(super) fn cut(self) {
        drop(self);
    }

    /// Close every RELAYED SOCKET while the tunnel keeps accepting — the node is fine, the port is
    /// fine, but the connections held through it end CLEANLY (each pump shuts its writer, so both
    /// peers see EOF, never an RST).
    ///
    /// This is the "the node closed the idle link" failure, and it needs its own verb because
    /// [`Tunnel::cut`] cannot express it: cut also drops the listener, so the redial that must
    /// SUCCEED for the property under test would be refused. Nothing in `server::serve` lets a
    /// test reach the accepted streams the node holds (the harness doc says so), so closing them
    /// from the middle is how a test plants a close the node itself would have sent — which is
    /// also literally what an operator's SSH tunnel does when it reaps one forwarded connection.
    ///
    /// Joins the pumps, so every FIN is on the wire before this returns.
    pub(super) fn drop_links(&self) {
        for (flag, _) in self.links.lock().unwrap().iter() {
            flag.store(true, Ordering::Release);
        }
        for (_, pump) in self.links.lock().unwrap().drain(..) {
            let _ = pump.join();
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        for (_, p) in self.links.lock().unwrap().drain(..) {
            let _ = p.join();
        }
    }
}

/// Copy bytes one way until either side ends, `stop` (the whole tunnel) is raised, or `link` (this
/// one relayed connection) is, then shut the writer so the far end sees EOF, and let both sockets
/// close on return. A bounded read rather than `io::copy`, for the Windows reason the module doc
/// measures.
fn pump(mut from: TcpStream, mut to: TcpStream, stop: &AtomicBool, link: &AtomicBool) {
    from.set_read_timeout(Some(Duration::from_millis(20))).expect("bounded read");
    let mut buf = [0u8; 8192];
    while !stop.load(Ordering::Acquire) && !link.load(Ordering::Acquire) {
        match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    let _ = to.shutdown(Shutdown::Write);
}
