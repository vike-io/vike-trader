//! **ONE data connection per gateway session, shared by every consumer of `data_client_id`** (any-symbol
//! price lane, decision 0174).
//!
//! IB refuses a second API connection under an id in use (error 326), and the lookup of a new stock
//! (`crate::any_symbol_socket`) and the price stream of the same stock (`crate::market_feed::IbkrFeeds`) both
//! ride `data_client_id`. Before this module the lookup dialled its own connection and closed it inside the
//! gate's data turn; a price stream that LIVES under the id made that dial fail with 326. Now both take a
//! [`Lease`] from the [`Session`] of their gateway and id:
//!
//! * the first lease **dials once** (the caller's dial: `market_feed::dial_data_client`, which goes through
//!   `GatewayGate::dial_data` and so never delays an order-lane dial and spends the data lane's own budget);
//! * every later lease **joins that connection** and dials nothing, so a lookup beside a live stream costs no
//!   connect at all;
//! * the connection is **closed when the LAST lease is dropped** (a lookup alone closes it as it always did; a
//!   stream closes it at its shutdown), which wakes any pump parked in a blocking `ibapi` call;
//! * a connection that is **no longer up** is replaced by the next lease (and closed), never handed out. The
//!   leases of the dead one end with it. The asker that comes back for it is the price stream's supervisor
//!   (`crate::quote_handle`, policy in `crate::redial`): its re-dial takes a NEW lease before it lets the dead
//!   stream's go, so a connection `ibapi` reconnected by itself, or one a lookup dialled meanwhile, is JOINED with no
//!   dial and no budget, and only a really dead one is dialled (through the gate's re-dial lane).
//!
//! The session lock is held across the dial ON PURPOSE: a second asker waits for the first dial and then joins
//! its connection, which is what keeps N concurrent first asks at one dial. The dial is bounded by the gate's
//! deadline. [`Session::live`] never waits (it is for a caller that must not queue behind a dial, the grid
//! pre-fetch).
//!
//! The session is generic over the connection ([`Link`]) so the sharing is proven with a scripted double; the
//! production connection is the `ibapi` blocking client.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError, TryLockError};

use ibapi::client::blocking::Client;

use crate::config::IbkrConfig;
use crate::error::IbkrError;

/// What the session needs to know of a connection.
pub(crate) trait Link {
    /// Is the connection still up (the socket has not reported itself dead)?
    fn up(&self) -> bool;
    /// Close it. Idempotent.
    fn close(&self);
}

impl Link for Client {
    fn up(&self) -> bool {
        self.is_connected()
    }

    fn close(&self) {
        self.disconnect();
    }
}

/// The connection and how many leases hold it.
struct Held<C> {
    client: Arc<C>,
    users: usize,
}

/// See the module doc.
pub(crate) struct Session<C> {
    held: Mutex<Option<Held<C>>>,
}

impl<C> Default for Session<C> {
    fn default() -> Self {
        Self { held: Mutex::new(None) }
    }
}

/// A hold on the session's connection. Dropping it gives the hold back; the last one closes the connection.
pub(crate) struct Lease<C: Link> {
    session: Arc<Session<C>>,
    client: Arc<C>,
}

impl<C: Link> Lease<C> {
    /// The shared connection.
    pub(crate) fn client(&self) -> &Arc<C> {
        &self.client
    }
}

impl<C: Link> Drop for Lease<C> {
    fn drop(&mut self) {
        self.session.release(&self.client);
    }
}

impl<C: Link> Session<C> {
    /// A lease on the live connection, DIALLING one with `dial` when there is none or the last one is down.
    /// Waits for a dial another asker has in progress and then joins it.
    pub(crate) fn lease(
        self: &Arc<Self>,
        dial: impl FnOnce() -> Result<C, IbkrError>,
    ) -> Result<Lease<C>, IbkrError> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(h) = held.as_mut()
            && h.client.up()
        {
            h.users += 1;
            return Ok(self.lease_of(&h.client));
        }
        if let Some(dead) = held.take() {
            dead.client.close();
        }
        let client = Arc::new(dial()?);
        *held = Some(Held { client: Arc::clone(&client), users: 1 });
        Ok(self.lease_of(&client))
    }

    /// A lease on the live connection if there is one RIGHT NOW; never dials and never waits for a dial in
    /// progress (that answers `None`: the caller falls back to what it did before the session existed).
    pub(crate) fn live(self: &Arc<Self>) -> Option<Lease<C>> {
        let mut held = match self.held.try_lock() {
            Ok(held) => held,
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
            Err(TryLockError::WouldBlock) => return None,
        };
        let h = held.as_mut().filter(|h| h.client.up())?;
        h.users += 1;
        Some(self.lease_of(&h.client))
    }

    fn lease_of(self: &Arc<Self>, client: &Arc<C>) -> Lease<C> {
        Lease { session: Arc::clone(self), client: Arc::clone(client) }
    }

    /// Give one hold on `client` back; the last hold closes it. A connection that was replaced meanwhile (it
    /// died) is not the session's any more and was closed when it was replaced.
    fn release(&self, client: &Arc<C>) {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let last = match held.as_mut() {
            Some(h) if Arc::ptr_eq(&h.client, client) => {
                h.users -= 1;
                h.users == 0
            }
            _ => false,
        };
        if last && let Some(h) = held.take() {
            h.client.close();
        }
    }
}

/// The sessions of this process, one per gateway address and data client id.
static SESSIONS: LazyLock<Mutex<HashMap<String, Arc<Session<Client>>>>> =
    LazyLock::new(Mutex::default);

/// The session of `cfg`'s gateway and `data_client_id`: the ONE place a connection under that id is made.
pub(crate) fn session_for(cfg: &IbkrConfig) -> Arc<Session<Client>> {
    let key = format!("{}:{}/{}", cfg.host, cfg.port, cfg.data_client_id);
    let mut sessions = SESSIONS.lock().unwrap_or_else(PoisonError::into_inner);
    Arc::clone(sessions.entry(key).or_default())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// A connection double: whether it is up, and how many times it was closed.
    struct Fake {
        up: AtomicBool,
        closed: AtomicUsize,
    }

    impl Fake {
        fn new() -> Fake {
            Fake { up: AtomicBool::new(true), closed: AtomicUsize::new(0) }
        }
    }

    impl Link for Fake {
        fn up(&self) -> bool {
            self.up.load(Ordering::SeqCst)
        }

        fn close(&self) {
            self.closed.fetch_add(1, Ordering::SeqCst);
            self.up.store(false, Ordering::SeqCst);
        }
    }

    fn session() -> Arc<Session<Fake>> {
        Arc::default()
    }

    /// **The lookup and the stream share ONE connection**: the second lease dials nothing and is the same
    /// client, however many ask.
    #[test]
    fn a_second_consumer_joins_the_first_ones_connection() {
        let session = session();
        let dials = AtomicUsize::new(0);
        let dial = || {
            dials.fetch_add(1, Ordering::SeqCst);
            Ok(Fake::new())
        };
        let stream = session.lease(dial).expect("dialled");
        let lookup = session.lease(|| panic!("a live connection is never dialled again")).unwrap();
        let more = session.live().expect("live");
        assert_eq!(dials.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(stream.client(), lookup.client()));
        assert!(Arc::ptr_eq(stream.client(), more.client()));
    }

    /// The connection is closed by the LAST lease only: a lookup that ends beside a live stream does not
    /// close the stream's connection, and a stream that ends beside a lookup leaves it to the lookup.
    #[test]
    fn the_last_lease_closes_the_connection() {
        let session = session();
        let stream = session.lease(|| Ok(Fake::new())).unwrap();
        let lookup = session.lease(|| panic!("joined")).unwrap();
        let client = Arc::clone(stream.client());
        drop(lookup);
        assert!(client.up(), "the stream still holds it");
        assert_eq!(client.closed.load(Ordering::SeqCst), 0);
        drop(stream);
        assert!(!client.up(), "the last hold closed it");
        assert_eq!(client.closed.load(Ordering::SeqCst), 1);
        assert!(session.live().is_none(), "nothing live once closed");
    }

    /// A connection that is no longer up is replaced (and closed), never handed out; the dead one's late
    /// lease does not touch the replacement.
    #[test]
    fn a_dead_connection_is_replaced_and_its_late_release_is_harmless() {
        let session = session();
        let old = session.lease(|| Ok(Fake::new())).unwrap();
        let dead = Arc::clone(old.client());
        dead.up.store(false, Ordering::SeqCst);
        assert!(session.live().is_none(), "a dead connection is not live");
        let fresh = session.lease(|| Ok(Fake::new())).expect("redialled");
        assert!(!Arc::ptr_eq(old.client(), fresh.client()));
        assert_eq!(dead.closed.load(Ordering::SeqCst), 1, "the dead one was closed on replacement");
        drop(old);
        assert!(fresh.client().up(), "the replacement keeps its hold");
        assert_eq!(fresh.client().closed.load(Ordering::SeqCst), 0);
    }

    /// **A re-dial after a lookup already healed the connection dials nothing**: the stream's supervisor takes a NEW
    /// lease before it lets the dead stream's go, finds the replacement a lookup dialled, and joins it; the dead
    /// stream's late release leaves the replacement alone.
    #[test]
    fn a_redial_joins_the_connection_a_lookup_replaced_the_dead_one_with() {
        let session = session();
        let stream = session.lease(|| Ok(Fake::new())).unwrap();
        let dead = Arc::clone(stream.client());
        dead.up.store(false, Ordering::SeqCst);
        let lookup = session.lease(|| Ok(Fake::new())).expect("the lookup replaced it");
        let redial = session
            .lease(|| panic!("what a lookup dialled is joined, not dialled again"))
            .expect("joined");
        assert!(Arc::ptr_eq(lookup.client(), redial.client()));
        assert!(!Arc::ptr_eq(stream.client(), redial.client()));
        drop(stream);
        drop(lookup);
        assert!(redial.client().up(), "the re-dial's hold keeps the replacement");
        assert_eq!(redial.client().closed.load(Ordering::SeqCst), 0);
    }

    /// A dial that fails leaves nothing behind: the next ask dials again (the gate's backoff and budget are
    /// what bound the retries, not this session).
    #[test]
    fn a_failed_dial_is_not_kept() {
        let session = session();
        let first = session.lease(|| Err::<Fake, _>(IbkrError::Connect("refused".into())));
        assert!(matches!(first, Err(IbkrError::Connect(_))));
        assert!(session.live().is_none());
        assert!(session.lease(|| Ok(Fake::new())).is_ok());
    }

    /// `live` never waits for a dial in progress: it answers `None` while another asker dials.
    #[test]
    fn live_does_not_wait_for_a_dial_in_progress() {
        let session = session();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let dialling = {
            let session = Arc::clone(&session);
            std::thread::spawn(move || {
                session
                    .lease(|| {
                        started_tx.send(()).unwrap();
                        go_rx.recv().unwrap();
                        Ok(Fake::new())
                    })
                    .map(|lease| lease.client().up())
            })
        };
        started_rx.recv().unwrap();
        assert!(session.live().is_none(), "a dial holds the session");
        go_tx.send(()).unwrap();
        assert_eq!(dialling.join().unwrap().ok(), Some(true));
    }

    /// Both production consumers go through the session of their gateway, and so does the pre-fetch's look:
    /// a call site that dialled `Client::connect` under `data_client_id` itself would meet error 326 beside a
    /// live stream.
    #[test]
    fn the_production_consumers_take_their_lease_from_the_session() {
        for (what, src) in [
            ("the price stream", include_str!("market_feed/mod.rs")),
            ("the any-symbol lookup", include_str!("any_symbol_socket.rs")),
            ("the grid pre-fetch", include_str!("properties.rs")),
        ] {
            assert!(src.contains("session_for("), "{what} must lease from the shared session");
        }
    }
}
