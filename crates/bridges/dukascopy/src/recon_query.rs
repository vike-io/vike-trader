//! The reconcile QUERY's plumbing: ask the JForex sidecar what the VENUE holds now, and wait for the one answer.
//!
//! The reconcile client (`crate::recon_client`) is a pure reader of an [`ReconAnswer`]; this module is the one place a
//! `recon` command is written to the sidecar's stdin and its `recon` envelope is matched back to the asker.
//!
//! # Why a query at all (and why it replaced the snapshot)
//!
//! The first version of the client read a SNAPSHOT the reader thread filled from the sidecar's `position` push lines.
//! A `position` line follows only a fill this sidecar reported (and, since the jar that adopts, the start's census), so
//! for the case that matters, a position the venue closed without telling the node (a stop-loss that fired while no
//! sidecar listened, a close in the platform), the snapshot kept the node's own last belief and read "in sync": a
//! positions-only leg over it would not have seen the phantom (`crates/bridges/dukascopy/CLAUDE.md`'s phantom section).
//! A PULL, answered from `engine.getOrders()` at the moment of the question, is the venue's own present state.
//!
//! # One question, one answer, matched by id
//!
//! [`ReconLink::ask`] numbers the question, writes `Command::Recon { id }`, and parks on a channel until the reader
//! thread delivers the `Envelope::Recon` of that id ([`ReconLink::deliver`]) or the wait runs out. Many askers may wait
//! at once (the daemon's recon worker runs one fetch at a time, but nothing here assumes it) and an answer that nobody
//! waits for any more (a timed-out ask) is dropped, said at `debug`.
//!
//! # ⚠ An unreadable answer is an ERROR, never "no orders"
//!
//! Every way the question can fail is an `Err` in words, and none is an empty answer, because an empty answer reads as a
//! flat venue and the node would call every position it holds a phantom (and the restore planner would call every
//! resting order gone): the jar does not announce `recon`; no sidecar runs (it died and the supervisor is starting
//! another); the write failed; the sidecar did not answer in time; the sidecar ended with the question open; the sidecar
//! answered `error`; the answer carried neither lists nor an error.
//!
//! # The symbols that get a FLAT row
//!
//! The jar lists an instrument only when it has a FILLED order, so a position the venue closed has no row at all, and
//! `vike_exec::recon::diff` raises nothing for an absent row (its step 4 walks the venue's rows). A flat row for a symbol
//! the node may hold is what makes the phantom visible, so the link keeps the symbols it knows: the engine's mount (the
//! start's symbol), every symbol a fill or a `position` line named, and every symbol the engine admitted
//! (`ReconClient::admit_symbol`). Bounded by [`MAX_KNOWN_SYMBOLS`]; the mount is never forgotten.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::proto::{Command, ReconOrder, ReconPosition, encode_line};
use crate::quotes::LinePort;

/// How long one question waits for its answer. The sidecar answers on its strategy thread, behind whatever that thread is
/// doing (a submit waiting for an instrument's feed can hold it for seconds), so the bound is generous; a reconcile pass
/// is every minute and a query that is late is a failed pass, said so, not a stalled node.
pub const RECON_QUERY_TIMEOUT: Duration = Duration::from_secs(10);

/// The most symbols the link keeps a flat row for (the engine's own admission cap is smaller; this only bounds a
/// misbehaving stream of fills).
pub const MAX_KNOWN_SYMBOLS: usize = 64;

/// The words of a question put to a jar that does not announce `recon`.
pub const NO_RECON_WORDS: &str = "dukascopy: the installed JForex sidecar jar predates the reconcile query (its `ready` \
     line does not announce `recon`), so the venue's orders and positions cannot be read; install the jar of this \
     release (`just fetch-tools`) and restart";

/// What the sidecar answered: the venue's orders and its net position per instrument, as of `ts`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReconAnswer {
    /// Every order the venue lists in the state CREATED, OPENED or FILLED.
    pub orders: Vec<ReconOrder>,
    /// One row per instrument that has a FILLED order (an instrument with none has NO row).
    pub positions: Vec<ReconPosition>,
    /// How many of the orders are not ours (opened in the platform).
    pub foreign: u32,
    /// The sidecar's clock when it read the venue (epoch ms); `0` when it stated none.
    pub ts: i64,
}

/// What a waiting asker is handed: the answer, or the words of why there is none.
pub type Reply = Result<ReconAnswer, String>;

/// The link between the reconcile client and the sidecar: the pending questions and the symbols that get a flat row.
pub struct ReconLink {
    /// The sidecar's stdin (shared with the exec client and the price lane: one whole line per write).
    port: Arc<dyn LinePort>,
    /// Whether the CURRENT sidecar's `ready` announced `recon`; false while none runs.
    supported: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    timeout_ms: AtomicU64,
    /// The engine's mount: always has a row, never forgotten.
    mount: Option<String>,
    known: Mutex<BTreeSet<String>>,
}

impl ReconLink {
    /// A link over `port`; `mount` is the symbol the engine is mounted on (the start's), if it was told.
    pub fn new(port: Arc<dyn LinePort>, mount: Option<&str>) -> Self {
        Self {
            port,
            supported: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            timeout_ms: AtomicU64::new(
                u64::try_from(RECON_QUERY_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
            ),
            mount: mount.map(|m| m.trim().to_ascii_uppercase()).filter(|m| !m.is_empty()),
            known: Mutex::new(BTreeSet::new()),
        }
    }

    /// Note whether the sidecar that runs now can answer (its `ready` announced `recon`); `false` at its end.
    pub fn set_supported(&self, supported: bool) {
        self.supported.store(supported, Ordering::SeqCst);
    }

    /// Whether the sidecar that runs now can answer.
    pub fn supported(&self) -> bool {
        self.supported.load(Ordering::SeqCst)
    }

    /// Change how long a question waits (a test's seam: the production bound is [`RECON_QUERY_TIMEOUT`]).
    pub fn set_timeout(&self, timeout: Duration) {
        self.timeout_ms
            .store(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX), Ordering::SeqCst);
    }

    /// Ask the sidecar what the venue holds now and wait for the answer (see the module doc: every failure is an `Err`).
    pub fn ask(&self) -> Reply {
        if !self.supported() {
            return Err(NO_RECON_WORDS.to_string());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending().insert(id, tx);
        if !self.port.send(&encode_line(&Command::Recon { id })) {
            self.pending().remove(&id);
            return Err("dukascopy: the reconcile query was NOT sent: the write to the JForex sidecar failed (it has \
                        exited and the node is restarting it)"
                .to_string());
        }
        let wait = Duration::from_millis(self.timeout_ms.load(Ordering::SeqCst));
        match rx.recv_timeout(wait) {
            Ok(reply) => reply,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.pending().remove(&id);
                Err(format!(
                    "dukascopy: the JForex sidecar did not answer the reconcile query within {wait:?}; the venue's \
                     orders and positions were NOT read"
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.pending().remove(&id);
                Err("dukascopy: the reconcile query was dropped before it was answered".to_string())
            }
        }
    }

    /// The reader thread hands over the `recon` envelope of question `id`. `false` = nobody waits for it any more.
    pub fn deliver(
        &self,
        id: u64,
        orders: Option<Vec<ReconOrder>>,
        positions: Option<Vec<ReconPosition>>,
        foreign: u32,
        ts: i64,
        error: Option<String>,
    ) -> bool {
        let reply = judge(orders, positions, foreign, ts, error);
        let Some(waiter) = self.pending().remove(&id) else {
            return false;
        };
        waiter.send(reply).is_ok()
    }

    /// The sidecar ended with questions open: every one gets `why` as its error.
    pub fn fail_all(&self, why: &str) {
        let waiting: Vec<_> = self.pending().drain().map(|(_, waiter)| waiter).collect();
        for waiter in waiting {
            let _ = waiter.send(Err(format!(
                "dukascopy: {why}; the venue's orders and positions were NOT read"
            )));
        }
    }

    /// A symbol the node may hold a position in (a fill or a `position` line named it, or the engine admitted it): it
    /// gets a flat row when the venue lists none. Bounded; an empty or already-known symbol is a no-op.
    pub fn know(&self, symbol: &str) {
        let symbol = symbol.trim().to_ascii_uppercase();
        if symbol.is_empty() {
            return;
        }
        let mut known = self.known();
        if known.len() < MAX_KNOWN_SYMBOLS {
            known.insert(symbol);
        }
    }

    /// The engine GAVE `symbol` back: it no longer gets a flat row (the mount is never forgotten).
    pub fn forget(&self, symbol: &str) {
        let symbol = symbol.trim().to_ascii_uppercase();
        if self.mount.as_deref() != Some(symbol.as_str()) {
            self.known().remove(&symbol);
        }
    }

    /// The symbols that get a flat row, sorted: the mount and everything [`Self::know`] was told.
    pub fn known_symbols(&self) -> Vec<String> {
        let mut all: BTreeSet<String> = self.known().clone();
        all.extend(self.mount.clone());
        all.into_iter().collect()
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, mpsc::Sender<Reply>>> {
        self.pending.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn known(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// An `Envelope::Recon` as a reply: the sidecar's own `error` wins, a missing list is a malformed answer (never an empty
/// one), and anything else is the answer.
pub fn judge(
    orders: Option<Vec<ReconOrder>>,
    positions: Option<Vec<ReconPosition>>,
    foreign: u32,
    ts: i64,
    error: Option<String>,
) -> Reply {
    if let Some(error) = error {
        return Err(format!("dukascopy: the JForex sidecar could not read the venue: {error}"));
    }
    match (orders, positions) {
        (Some(orders), Some(positions)) => Ok(ReconAnswer { orders, positions, foreign, ts }),
        _ => Err("dukascopy: the JForex sidecar's reconcile answer carried neither its lists nor an error, so it is \
                  not read as an empty venue"
            .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// A port that records what it was sent and answers a scripted delivery for each question.
    struct Port {
        sent: Mutex<Vec<String>>,
        alive: AtomicBool,
        writes: AtomicUsize,
    }

    impl LinePort for Port {
        fn send(&self, line: &str) -> bool {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.sent.lock().unwrap().push(line.to_string());
            self.alive.load(Ordering::SeqCst)
        }
    }

    fn port(alive: bool) -> Arc<Port> {
        Arc::new(Port {
            sent: Mutex::new(Vec::new()),
            alive: AtomicBool::new(alive),
            writes: AtomicUsize::new(0),
        })
    }

    fn link(port: &Arc<Port>) -> Arc<ReconLink> {
        let link = Arc::new(ReconLink::new(Arc::clone(port) as Arc<dyn LinePort>, Some("eurusd")));
        link.set_supported(true);
        link.set_timeout(Duration::from_millis(300));
        link
    }

    fn order(symbol: &str) -> ReconOrder {
        ReconOrder {
            order_id: "1".into(),
            coid: Some("c1".into()),
            symbol: symbol.into(),
            side: 1,
            state: "OPENED".into(),
            command: "BUYLIMIT".into(),
            amount: 0.0,
            requested: 1000.0,
            price: 1.1,
            stop_loss: 0.0,
            take_profit: 0.0,
            commission: 0.0,
            created: 1,
            filled: 0,
        }
    }

    /// Answer the question that was just written, from another thread, the way the reader does.
    fn answer_later(
        link: &Arc<ReconLink>,
        port: &Arc<Port>,
        reply: impl FnOnce(&ReconLink, u64) + Send + 'static,
    ) {
        let (link, port) = (Arc::clone(link), Arc::clone(port));
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while port.sent.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            let line = port.sent.lock().unwrap().last().cloned().expect("a question was written");
            let id = match crate::proto::parse_command(&line) {
                Some(Command::Recon { id }) => id,
                other => panic!("a recon command was expected, got {other:?}"),
            };
            reply(link.as_ref(), id);
        });
    }

    #[test]
    fn a_question_is_written_with_its_id_and_the_answer_with_that_id_is_returned() {
        let port = port(true);
        let link = link(&port);
        answer_later(&link, &port, |link, id| {
            assert!(link.deliver(id, Some(vec![order("EURUSD")]), Some(vec![]), 0, 7, None));
        });
        let answer = link.ask().expect("answered");
        assert_eq!(answer.orders.len(), 1);
        assert_eq!(answer.ts, 7);
        assert_eq!(port.sent.lock().unwrap().len(), 1);
        assert!(port.sent.lock().unwrap()[0].contains(r#""cmd":"recon""#));
    }

    /// An empty answer is an answer: the venue holds nothing. It is not the same as no answer.
    #[test]
    fn an_empty_answer_is_an_answer_and_not_an_error() {
        let port = port(true);
        let link = link(&port);
        answer_later(&link, &port, |link, id| {
            link.deliver(id, Some(vec![]), Some(vec![]), 0, 1, None);
        });
        let answer = link.ask().expect("a flat venue is an answer");
        assert!(answer.orders.is_empty() && answer.positions.is_empty());
    }

    /// Every way the question can fail is an Err in words. None is an empty answer.
    #[test]
    fn a_jar_without_the_feature_a_dead_pipe_a_silence_and_an_error_answer_are_all_errors() {
        // (1) the jar does not announce `recon`: nothing is written at all
        let port1 = port(true);
        let l1 = link(&port1);
        l1.set_supported(false);
        let e = l1.ask().unwrap_err();
        assert!(e.contains("predates the reconcile query"), "{e}");
        assert_eq!(
            port1.writes.load(Ordering::SeqCst),
            0,
            "nothing is sent to a jar that cannot answer"
        );

        // (2) the write fails: the sidecar is gone
        let port2 = port(false);
        let l2 = link(&port2);
        let e = l2.ask().unwrap_err();
        assert!(e.contains("was NOT sent"), "{e}");
        assert!(l2.pending().is_empty(), "a refused question leaves nothing pending");

        // (3) the sidecar says nothing within the bound
        let port3 = port(true);
        let l3 = link(&port3);
        let e = l3.ask().unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
        assert!(l3.pending().is_empty(), "a timed-out question is forgotten");

        // (4) the sidecar answers with an error
        let port4 = port(true);
        let l4 = link(&port4);
        answer_later(&l4, &port4, |link, id| {
            link.deliver(
                id,
                None,
                None,
                0,
                0,
                Some("the venue's orders could not be read: no session".into()),
            );
        });
        let e = l4.ask().unwrap_err();
        assert!(e.contains("no session"), "{e}");

        // (5) the answer carries neither lists nor an error: malformed, never an empty venue
        let port5 = port(true);
        let l5 = link(&port5);
        answer_later(&l5, &port5, |link, id| {
            link.deliver(id, None, None, 0, 0, None);
        });
        let e = l5.ask().unwrap_err();
        assert!(e.contains("neither its lists nor an error"), "{e}");

        // (6) one list present and the other absent is malformed too
        let port6 = port(true);
        let l6 = link(&port6);
        answer_later(&l6, &port6, |link, id| {
            link.deliver(id, Some(vec![]), None, 0, 0, None);
        });
        assert!(l6.ask().is_err());
    }

    /// The sidecar's death with a question open fails it at once, and a late answer to a forgotten question is dropped.
    #[test]
    fn a_death_fails_the_open_question_and_a_late_answer_is_dropped() {
        let port = port(true);
        let link = link(&port);
        answer_later(&link, &port, |link, _| link.fail_all("the JForex sidecar ended"));
        let e = link.ask().unwrap_err();
        assert!(e.contains("the JForex sidecar ended") && e.contains("NOT read"), "{e}");
        assert!(
            !link.deliver(999, Some(vec![]), Some(vec![]), 0, 0, None),
            "nobody waits for id 999"
        );
    }

    /// The symbols with a flat row: the mount (upper-cased, never forgotten), what was named, minus what was given back.
    #[test]
    fn the_known_symbols_are_the_mount_and_what_was_named_and_the_mount_stays() {
        let port = port(true);
        let link = link(&port);
        assert_eq!(link.known_symbols(), ["EURUSD"]);
        link.know("usdjpy");
        link.know("");
        link.know("USDJPY");
        assert_eq!(link.known_symbols(), ["EURUSD", "USDJPY"]);
        link.forget("USDJPY");
        link.forget("eurusd");
        assert_eq!(link.known_symbols(), ["EURUSD"], "the mount is never forgotten");
        for n in 0..(MAX_KNOWN_SYMBOLS + 10) {
            link.know(&format!("SYM{n}"));
        }
        assert_eq!(link.known_symbols().len(), MAX_KNOWN_SYMBOLS + 1, "bounded (plus the mount)");
    }
}
