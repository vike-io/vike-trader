//! `server::starting` — the door that listens BEFORE the engines are up, and says so.
//!
//! The observe/control server needs a running core (a snapshot cell, a command sink), and the core
//! exists only after every account's engine has started — one after another, each as slow as its
//! venue. A hung venue therefore kept the port closed, and the deploy helper's health window could
//! not tell "still starting" from "dead" (2026-10-17: the v0.1.64 deploy rolled back). This module
//! is the answer for the interval in between: it takes the SAME listener the real server will use
//! and answers every authenticated peer, for any verb, with [`starting_error`] — the node's honest
//! state and where the start has got to ([`StartProgress`]). Nothing here can reach the core,
//! because there is none yet.
//!
//! ## What it will NOT do (fail closed)
//!
//! * It serves no snapshot, no strategy status, no settings, no tearsheet and accepts no command of
//!   any kind: not an order, a cancel, a mount, a set-active, not even a halt over the wire. There
//!   is no sink to hand one to, so each is refused with the `starting:` text. A HALT is a FILE
//!   (the sentinel the order path polls), written by `vike-cli halt` and never by this server, and
//!   this module neither reads nor writes it: it keeps working exactly as before.
//! * It runs the SAME handshake as the real server ([`run_handshake`], the same keys), so an
//!   unauthenticated peer learns nothing — the progress text is shown only after `AuthOk`, and a
//!   scope the node does not hold a key for is denied here exactly as there.
//! * It cannot be read as READY. Ready is the stdout `{"kind":"ready"...}` line the deploy helper
//!   greps the journal for, and it is printed after the real server took this listener over
//!   ([`StartingDoor::hand_over`]); no verb here answers with a frame a client takes for state.
//!
//! ## Shape
//!
//! One accept thread on a CLONE of the listener, polling (the listener is switched non-blocking
//! while the door is up and back at the hand-over), one short-lived thread per connection, bounded
//! by [`MAX_CONNECTIONS`]. A connection answers ONE request and closes: the node's state is about
//! to change, so a link kept open here would keep saying `starting` after the node is ready. A
//! connection still being answered when the hand-over comes closes without a word and the client
//! dials again, to the real server.

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_hub_proto::auth::HubKeys;
use vike_tradehub_client::proto::{Response, read_frame_raw, starting_error, write_frame};

use super::MAX_CONNECTIONS;
use super::handshake::{HandshakeOutcome, run_handshake};
use super::log_read_end;

/// How often the accept thread looks at the listener (and at the stop flag) when nothing waits.
/// The hand-over costs at most this much; a connection that arrives meanwhile queues in the kernel
/// backlog of the listener the real server then takes over.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// The most one door connection may wait on its peer, for the handshake and for the one request:
/// a stranger cannot hold a thread, and an authenticated client that says nothing is let go (it
/// dials again). Short on purpose — nothing here is worth a long wait.
const CONNECTION_PATIENCE: Duration = Duration::from_secs(10);

/// What one account's start has reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountStart {
    /// Its engine is being started now.
    Starting,
    /// Its engine is up.
    Up,
    /// It does not run, and why (the operator reads this word).
    NotRunning(String),
}

#[derive(Default)]
struct Progress {
    /// What the node is doing now, in a few words.
    phase: String,
    /// When `phase` was set (the heartbeat's "N s" when no account is starting).
    phase_since: Option<Instant>,
    /// Accounts in the order their start was first reported, by route key.
    accounts: Vec<(String, AccountStart)>,
    /// When each account now `Starting` began, in that order (the heartbeat's "N s").
    starting_since: Vec<(String, Instant)>,
}

/// Where the node's start has got to, shared by whoever starts things (writers) and by the door
/// (the reader). Cheap to clone; every clone is the same record.
///
/// A report callback may be attached ([`StartProgress::with_reporter`]): it is called with the
/// rendered text after EVERY change, from the thread that made it, so the daemon can put each step
/// on its protocol stream where the journal and the deploy helper see it.
#[derive(Clone)]
pub struct StartProgress {
    state: Arc<Mutex<Progress>>,
    reporter: Option<Reporter>,
}

/// What a change is reported to: called with the rendered text.
type Reporter = Arc<dyn Fn(&str) + Send + Sync>;

impl Default for StartProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl StartProgress {
    /// An empty record, reporting nowhere.
    #[must_use]
    pub fn new() -> Self {
        StartProgress { state: Arc::new(Mutex::new(Progress::default())), reporter: None }
    }

    /// This record, calling `reporter` with the rendered text after every change.
    #[must_use]
    pub fn with_reporter(mut self, reporter: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.reporter = Some(Arc::new(reporter));
        self
    }

    /// Say what the node is doing now.
    pub fn phase(&self, text: &str) {
        self.update(|p| {
            p.phase = text.to_string();
            p.phase_since = Some(Instant::now());
        });
    }

    /// Record how far the account `route_key` has got (first report = it joins the list).
    pub fn account(&self, route_key: &str, start: AccountStart) {
        let starting = start == AccountStart::Starting;
        self.update(|p| {
            match p.accounts.iter_mut().find(|(key, _)| key == route_key) {
                Some((_, slot)) => *slot = start,
                None => p.accounts.push((route_key.to_string(), start)),
            }
            p.starting_since.retain(|(key, _)| key != route_key);
            if starting {
                p.starting_since.push((route_key.to_string(), Instant::now()));
            }
        });
    }

    /// Report [`Self::waiting_line`] every `interval` until the returned guard is dropped, so a
    /// start held by ONE slow account still moves the stream: the deploy helper
    /// (`deploy/sbin/vike-trader-ci-deploy`'s `healthy`) extends its window only when the count of
    /// `"kind":"starting"` lines GROWS, and `update` speaks only when something changes. The
    /// heartbeat changes no state, so the door's `starting:` answer is untouched. Reports nowhere
    /// when no reporter is attached.
    #[must_use = "the heartbeat stops when the guard is dropped"]
    pub fn heartbeat(&self, interval: Duration) -> Heartbeat {
        let (stop, ticks) = mpsc::channel::<()>();
        let progress = self.clone();
        let spawned =
            std::thread::Builder::new().name("vt-start-heartbeat".into()).spawn(move || {
                // `Timeout` = another interval went by; `Disconnected` = the guard was dropped.
                while let Err(RecvTimeoutError::Timeout) = ticks.recv_timeout(interval) {
                    if let Some(report) = &progress.reporter {
                        report(&progress.waiting_line(Instant::now()));
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "the start heartbeat could not spawn; the start reports changes only");
        }
        Heartbeat { _stop: stop }
    }

    /// The progress line plus what the start is waiting on and for how long, as of `now`: the
    /// first account still `Starting`, else the phase. `now` is a parameter so a test sets the age.
    #[must_use]
    pub fn waiting_line(&self, now: Instant) -> String {
        let p = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (what, since) = match p.starting_since.first() {
            Some((key, since)) => (key.as_str(), Some(*since)),
            None => (phase_text(&p), p.phase_since),
        };
        let secs = since.map_or(0, |since| now.saturating_duration_since(since).as_secs());
        format!("{}; waiting on {what} {secs}s", render(&p))
    }

    fn update(&self, change: impl FnOnce(&mut Progress)) {
        let text = {
            let mut p = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            change(&mut p);
            render(&p)
        };
        if let Some(report) = &self.reporter {
            report(&text);
        }
    }

    /// The record as one line an operator reads: the phase, then each account and its state.
    #[must_use]
    pub fn describe(&self) -> String {
        render(&self.state.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Dropping this stops the [`StartProgress::heartbeat`] (its thread sees the channel close).
pub struct Heartbeat {
    _stop: mpsc::Sender<()>,
}

fn phase_text(p: &Progress) -> &str {
    if p.phase.is_empty() { "the node is starting" } else { p.phase.as_str() }
}

fn render(p: &Progress) -> String {
    let phase = phase_text(p);
    if p.accounts.is_empty() {
        return phase.to_string();
    }
    let accounts = p
        .accounts
        .iter()
        .map(|(key, start)| match start {
            AccountStart::Starting => format!("{key} starting"),
            AccountStart::Up => format!("{key} up"),
            AccountStart::NotRunning(why) => format!("{key} NOT RUNNING ({why})"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("{phase}; accounts: {accounts}")
}

/// What a door connection needs, fixed when the door opens.
struct DoorContext {
    keys: HubKeys,
    datahub_advertise: Option<String>,
    account_admin: bool,
    progress: StartProgress,
}

/// The running door. Dropping it stops the accept thread; [`Self::hand_over`] stops it AND returns
/// the listener to blocking mode for the real server.
pub struct StartingDoor {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl StartingDoor {
    /// Start answering on `listener` (the accept thread works on a clone; the caller keeps the
    /// original for the real server). `keys`, `datahub_advertise` and `account_admin` are the
    /// values the real server will use, so the handshake admits exactly the same peers.
    ///
    /// # Errors
    /// The listener could not be cloned or switched to non-blocking mode; nothing was started and
    /// the listener is as it was (the caller goes without a door, as it did before this existed).
    pub fn open(
        listener: &TcpListener,
        keys: HubKeys,
        datahub_advertise: Option<String>,
        account_admin: bool,
        progress: StartProgress,
    ) -> io::Result<Self> {
        let context = Arc::new(DoorContext { keys, datahub_advertise, account_admin, progress });
        Self::open_with(listener, context, |accept, stop, context| {
            std::thread::Builder::new()
                .name("vt-starting-accept".into())
                .spawn(move || accept_loop(&accept, &stop, &context))
        })
    }

    /// [`Self::open`] with the thread spawner injected (a test makes it fail). The mode is shared by
    /// the clone, so EVERY error path after the switch gives the listener back blocking: the real
    /// server would otherwise accept on a non-blocking listener and spin.
    fn open_with(
        listener: &TcpListener,
        context: Arc<DoorContext>,
        spawn: impl FnOnce(TcpListener, Arc<AtomicBool>, Arc<DoorContext>) -> io::Result<JoinHandle<()>>,
    ) -> io::Result<Self> {
        let accept = listener.try_clone()?;
        accept.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        match spawn(accept, Arc::clone(&stop), context) {
            Ok(join) => Ok(StartingDoor { stop, join: Some(join) }),
            Err(e) => {
                if let Err(restore) = listener.set_nonblocking(false) {
                    tracing::error!(error = %restore, "the starting door could not give its listener back blocking");
                }
                Err(e)
            }
        }
    }

    /// Stop answering `starting` and give `listener` back to the real server: the accept thread is
    /// joined, and the listener (non-blocking while the door was up — the mode is shared with the
    /// clone) is made blocking again. A peer that connected meanwhile waits in the backlog.
    ///
    /// # Errors
    /// The listener could not be made blocking again. The caller must NOT serve on it (a
    /// non-blocking `accept` loop would spin): it logs and goes without a server.
    pub fn hand_over(mut self, listener: &TcpListener) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if self.join.take().is_some_and(|join| join.join().is_err()) {
            tracing::error!("the starting door's accept thread panicked; continuing to the server");
        }
        listener.set_nonblocking(false)
    }
}

impl Drop for StartingDoor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Counts live door connections and gives the slot back when the thread ends, even by a panic.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn accept_loop(listener: &TcpListener, stop: &Arc<AtomicBool>, context: &Arc<DoorContext>) {
    let live = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Some platforms hand the accepted socket the listener's non-blocking mode.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let taken = live.fetch_add(1, Ordering::AcqRel) + 1;
                let slot = Slot(Arc::clone(&live));
                if taken > MAX_CONNECTIONS {
                    // Dropped without a reply, like the real server: a stranger exhausting the
                    // cap is not worth a thread or an allocation.
                    continue;
                }
                let (stop, context) = (Arc::clone(stop), Arc::clone(context));
                let spawned =
                    std::thread::Builder::new().name("vt-starting-conn".into()).spawn(move || {
                        let _slot = slot;
                        answer(stream, &context, &stop);
                    });
                if let Err(e) = spawned {
                    tracing::warn!(error = %e, "the starting door could not spawn a connection thread");
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(ACCEPT_POLL),
            Err(e) => {
                tracing::warn!(error = %e, "the starting door: accept failed, continuing");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

/// One door connection: the real handshake, then ONE request answered with the node's state and the
/// connection closed. The request is not decoded: whatever it asked, the answer is the same.
fn answer(mut stream: TcpStream, context: &DoorContext, stop: &AtomicBool) {
    let peer: Option<SocketAddr> = stream.peer_addr().ok();
    if vike_hub_proto::frame::configure_hub_stream(&stream).is_err()
        || stream.set_read_timeout(Some(CONNECTION_PATIENCE)).is_err()
    {
        return;
    }
    let HandshakeOutcome::Authed(_scope) = run_handshake(
        &mut stream,
        &context.keys,
        peer,
        context.datahub_advertise.as_deref(),
        context.account_admin,
    ) else {
        return;
    };
    if let Err(e) = read_frame_raw(&mut stream) {
        log_read_end(&e, peer);
        return;
    }
    // The node may have come up while this peer was typing: say nothing, so it dials the real server.
    if stop.load(Ordering::Acquire) {
        return;
    }
    // Best-effort: a peer that went away has nothing to be told.
    let _ =
        write_frame(&mut stream, &Response::Error(starting_error(&context.progress.describe())));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The heartbeat line names the account the start is held on and how long it has waited, at the
    /// age the CLOCK argument says (no sleeping): the line the deploy helper's extension counts.
    #[test]
    fn the_waiting_line_names_the_first_starting_account_and_its_age() {
        let progress = StartProgress::new();
        progress.phase("starting the accounts");
        progress.account("binance", AccountStart::Up);
        progress.account("dukascopy", AccountStart::Starting);
        progress.account("okx", AccountStart::Starting);
        let line = progress.waiting_line(Instant::now() + Duration::from_secs(70));
        assert!(line.ends_with("waiting on dukascopy 70s"), "{line}");
        assert!(
            line.starts_with("starting the accounts; accounts: binance up, dukascopy starting")
        );
    }

    /// With no account starting the line waits on the PHASE; once an account is up it stops
    /// naming it (a finished account is not what the start waits on).
    #[test]
    fn the_waiting_line_falls_back_to_the_phase() {
        let progress = StartProgress::new();
        progress.phase("connecting to the gatewayhub");
        let line = progress.waiting_line(Instant::now() + Duration::from_secs(12));
        assert!(line.ends_with("waiting on connecting to the gatewayhub 12s"), "{line}");
        progress.account("dukascopy", AccountStart::Starting);
        progress.account("dukascopy", AccountStart::Up);
        let line = progress.waiting_line(Instant::now() + Duration::from_secs(12));
        assert!(line.contains("waiting on connecting to the gatewayhub"), "{line}");
    }

    /// The heartbeat speaks on its interval while the guard lives, changes no state (the door's
    /// answer is the same text), and is silent once the guard is dropped.
    #[test]
    fn the_heartbeat_repeats_while_held_and_stops_when_dropped() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&lines);
        let progress = StartProgress::new()
            .with_reporter(move |text| sink.lock().expect("lines").push(text.to_string()));
        progress.account("dukascopy", AccountStart::Starting);
        let described = progress.describe();
        let count = || lines.lock().expect("lines").len();
        assert_eq!(count(), 1, "only the change itself so far");
        let beat = progress.heartbeat(Duration::from_millis(30));
        std::thread::sleep(Duration::from_millis(600));
        let while_held = count();
        // ~20 are due; 4 leaves room for a starved CI thread without letting "never beats" pass.
        assert!(while_held > 4, "heartbeat too quiet: {while_held} lines");
        assert!(
            lines.lock().expect("lines")[1..].iter().all(|l| l.contains("waiting on dukascopy"))
        );
        assert_eq!(progress.describe(), described, "a heartbeat must not change the progress");
        drop(beat);
        std::thread::sleep(Duration::from_millis(100));
        let after_drop = count();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(count(), after_drop, "the heartbeat outlived its guard");
    }

    /// **A door whose accept thread cannot start leaves the listener BLOCKING.** The mode is shared with
    /// the clone: left non-blocking, the real server's `accept` loop would spin on `WouldBlock`.
    #[test]
    fn a_failed_spawn_gives_the_listener_back_blocking() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        let addr = listener.local_addr().expect("assigned port");
        let context = Arc::new(DoorContext {
            keys: HubKeys::new(b"observe".to_vec(), b"control".to_vec()),
            datahub_advertise: None,
            account_admin: false,
            progress: StartProgress::new(),
        });
        let opened = StartingDoor::open_with(&listener, context, |_, _, _| {
            Err(io::Error::other("no thread"))
        });
        assert!(opened.is_err(), "the injected spawn failure is the open's error");
        // A peer that dials 300 ms from now: a blocking `accept` waits for it, a non-blocking one
        // answers `WouldBlock` at once.
        let dial = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            TcpStream::connect(addr)
        });
        assert!(listener.accept().is_ok(), "the listener is blocking again");
        dial.join().expect("dial thread").expect("connect");
    }
}
