//! **The ONE socket of a market-data route** ([`RouteLink`]), shared by every lane of the client on that route.
//!
//! Alpaca allows one connection per key per stream (a second gets `406 connection limit exceeded`, the module doc of
//! [`crate::feed`]), so the lanes of a route cannot each dial: every subscription is a lane of the shared data engine
//! (`vike_bridge_core::data_client::ws`'s `SessionOn` plan, its own thread, its own session loop on the market pump),
//! and what a lane gets from [`RouteLink::attach`] is a read-only [`LaneStream`] over the route's one socket.
//!
//! - **One dial, the bounded one.** The first lane that finds no live socket dials it (`connect_market_socket`, under
//!   the pump row's `connect_timeout`), sends the OAuth auth frame and starts the socket's reader; the others wait for
//!   the dial in stop-aware naps and attach to what it opened.
//! - **One frame per key.** The reader writes a `subscribe` frame for the first lane of a `(verb, wire name)` and an
//!   `unsubscribe` frame when its last lane lets go; the route's last lane closes the socket. Each is decided and
//!   queued under the socket's one lane lock, so a key's frames reach the venue in the order its holders were counted
//!   (a key let go while another lane takes it is never left unsubscribed under a lane that holds it).
//! - **Every frame to every lane.** A text frame goes to each lane of the route, which keeps the messages of its own
//!   key; a venue `error` frame is said once, here, at `warn`.
//! - **The liveness is the socket's.** Alpaca's server pings and requires the pong (the reader answers each); any
//!   inbound frame, a ping included, is proof of life, and a lane's idle clock ([`LaneStream`]'s `since_last_frame`)
//!   reads the socket's, so a quiet symbol on a live socket never trips. A socket silent past the row's
//!   `idle_threshold` is ended by its reader.
//! - **No subscription vanishes silently.** A socket that ends (a transport error, a server close, the idle bound, a
//!   stop) tells every lane on it why, after the link has forgotten it, and a lane registers BEFORE it checks the
//!   socket is still the live one, so none can slip between the two. A lane that is told ends its session and comes
//!   back through the pump's backoff, onto a new socket the first of them dials.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::Message;
use vike_bridge_core::websocket_session::market::{
    WebsocketSessionMarketStream, connect_market_socket,
};
use vike_bridge_core::websocket_session::spec::WebsocketSessionKnobs;
use vike_bridge_core::websocket_session::user::WebsocketSessionUserError;
use vike_transport::websocket::{WsSocket, is_timeout};
use vike_utility::mutex::lock;

use super::decode::decode_control_errors;
use super::{StreamRoute, VENUE, Verb, auth_frame, sub_action};
use crate::refusals::AlpacaFeedRefusal;
use crate::transport::sign::TokenSource;

/// What the reader hands a lane.
enum Inbound {
    /// One text frame of the route.
    Text(String),
    /// The socket is over, and why.
    Closed(String),
}

/// What a lane asks the reader to do.
enum Command {
    /// Write this frame (a `subscribe` or an `unsubscribe`).
    Frame(String),
    /// The route's last lane let go: close the socket.
    Close,
}

/// The lanes on one socket, and how many of them hold each key.
#[derive(Default)]
struct Lanes {
    routes: HashMap<u64, Sender<Inbound>>,
    held: HashMap<(Verb, String), usize>,
    next: u64,
}

/// The live socket, as lanes see it.
#[derive(Clone)]
struct Connection {
    generation: u64,
    commands: Sender<Command>,
    lanes: Arc<Mutex<Lanes>>,
    /// When the socket last heard anything, a ping included.
    heard: Arc<Mutex<Instant>>,
}

/// The one socket of a route, dialled by the first lane that needs it (the module doc).
pub(super) struct RouteLink {
    route: StreamRoute,
    url: String,
    token: Arc<TokenSource>,
    knobs: WebsocketSessionKnobs,
    stop: Arc<AtomicBool>,
    live: Arc<Mutex<Option<Connection>>>,
    generations: AtomicU64,
    /// Held by the lane that is dialling.
    dial: Mutex<()>,
    readers: Mutex<Vec<JoinHandle<()>>>,
}

impl RouteLink {
    /// The link of `route` at `url`, authenticating with `token`, bounded by `knobs` (the venue's pump row).
    pub(super) fn new(
        route: StreamRoute,
        url: String,
        token: Arc<TokenSource>,
        knobs: WebsocketSessionKnobs,
    ) -> Self {
        RouteLink {
            route,
            url,
            token,
            knobs,
            stop: Arc::default(),
            live: Arc::default(),
            generations: AtomicU64::new(0),
            dial: Mutex::new(()),
            readers: Mutex::default(),
        }
    }

    /// Attach one lane for `verb` of the venue's `wire` name, dialling the socket first if no lane has. Dropping the
    /// stream lets the key go (and nothing else). `Err` is the attach's failure in words, the lane's session error.
    pub(super) fn attach(
        self: &Arc<Self>,
        lane_stop: &AtomicBool,
        verb: Verb,
        wire: &str,
    ) -> Result<LaneStream, String> {
        let conn = self.connection(lane_stop)?;
        let key = (verb, wire.to_string());
        let (tx, rx) = mpsc::channel();
        let (id, queued) = {
            let mut lanes = lock(&conn.lanes);
            let id = lanes.next;
            lanes.next += 1;
            // Registered BEFORE the socket is checked: a socket still current has not drained, and its drain tells
            // this lane.
            lanes.routes.insert(id, tx);
            let holders = lanes.held.entry(key.clone()).or_default();
            *holders += 1;
            // Queued under the lock (an unbounded send never blocks), so the reader writes a key's frames in the order
            // its holders were counted: a `detach` that let the key go cannot queue its `unsubscribe` after this.
            let queued = *holders > 1
                || conn
                    .commands
                    .send(Command::Frame(sub_action("subscribe", verb, wire).to_string()))
                    .is_ok();
            (id, queued)
        };
        let stream = LaneStream {
            rx,
            id,
            key,
            conn: conn.clone(),
            link: Arc::clone(self),
            poll: self.knobs.read_timeout,
        };
        if !self.is_current(conn.generation) {
            return Err("the market-data socket closed while subscribing".to_string());
        }
        if !queued {
            self.forget(conn.generation);
            return Err("the market-data socket's reader is gone".to_string());
        }
        Ok(stream)
    }

    /// Is `generation` the socket lanes are being given?
    fn is_current(&self, generation: u64) -> bool {
        lock(&self.live).as_ref().is_some_and(|c| c.generation == generation)
    }

    /// Forget the socket `generation`, so the next lane dials.
    fn forget(&self, generation: u64) {
        forget(&self.live, generation);
    }

    /// The live socket, dialling one if none is. One dialler at a time; the others wait in stop-aware naps.
    // shortcut: a failed dial is not shared, so the lanes behind it each pay their own bounded dial in turn; share the
    // failure for a moment (ig's `FAILURE_SHARED_FOR`) if a route ever carries many lanes.
    fn connection(&self, lane_stop: &AtomicBool) -> Result<Connection, String> {
        loop {
            if lane_stop.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed) {
                return Err("stop requested".to_string());
            }
            if let Some(conn) = lock(&self.live).clone() {
                return Ok(conn);
            }
            let _dialling = match self.dial.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
            };
            // Another lane may have finished its dial while this one waited for the lock.
            if let Some(conn) = lock(&self.live).clone() {
                return Ok(conn);
            }
            return self.open();
        }
    }

    /// Mint the Bearer, dial within the row's bound, authenticate and start the socket's reader.
    fn open(&self) -> Result<Connection, String> {
        let bearer = self.token.bearer().map_err(|e| format!("token mint failed: {e}"))?;
        let mut socket = connect_market_socket(
            &self.url,
            self.knobs.read_timeout,
            self.knobs.connect_timeout,
            None,
        )?;
        // The auth frame goes out without waiting for its ack: a refused key is the venue's `error` frame and its
        // close, which end the socket like any other.
        socket
            .send(Message::Text(auth_frame(&bearer).to_string().into()))
            .map_err(|e| format!("auth send failed: {e}"))?;
        let (commands, command_rx) = mpsc::channel();
        let conn = Connection {
            generation: self.generations.fetch_add(1, Ordering::Relaxed),
            commands,
            lanes: Arc::default(),
            heard: Arc::new(Mutex::new(Instant::now())),
        };
        let reader = Reader {
            route: self.route,
            commands: command_rx,
            conn: conn.clone(),
            live: Arc::clone(&self.live),
            stop: Arc::clone(&self.stop),
            idle: self.knobs.idle_threshold,
        };
        let thread = std::thread::Builder::new()
            .name("gw-alpaca-link".to_string())
            .spawn(move || {
                vike_execution::affinity::pin_current_thread(
                    vike_execution::affinity::Role::MarketData,
                    VENUE,
                );
                reader.run(socket);
            })
            .map_err(|e| format!("the OS refused the market-data reader thread: {e}"))?;
        {
            let mut readers = lock(&self.readers);
            readers.retain(|reader| !reader.is_finished());
            readers.push(thread);
        }
        *lock(&self.live) = Some(conn.clone());
        tracing::info!(venue = VENUE, route = ?self.route, generation = conn.generation, "market-data socket up (one for every subscription of this stream)");
        Ok(conn)
    }

    /// A lane let `key` go: the last lane of the key writes its `unsubscribe`, the last lane of the socket closes it. A
    /// socket that is over took every subscription with it. Decided and queued under the socket's lane lock, as
    /// [`Self::attach`]'s `subscribe` is, so the two reach the venue in the order they were decided.
    fn detach(&self, conn: &Connection, id: u64, key: &(Verb, String)) {
        let mut lanes = lock(&conn.lanes);
        lanes.routes.remove(&id);
        let key_gone = match lanes.held.get_mut(key) {
            Some(holders) if *holders > 1 => {
                *holders -= 1;
                false
            }
            Some(_) => {
                lanes.held.remove(key);
                true
            }
            None => false,
        };
        let socket_empty = lanes.routes.is_empty();
        if !self.is_current(conn.generation) {
            return;
        }
        if socket_empty {
            self.forget(conn.generation);
            let _ = conn.commands.send(Command::Close);
        } else if key_gone {
            let frame = sub_action("unsubscribe", key.0, &key.1).to_string();
            let _ = conn.commands.send(Command::Frame(frame));
        }
    }

    /// Raise the stop and return at once: the reader ends within one read poll.
    pub(super) fn raise_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop the reader and join it. A stopped link never dials again.
    pub(super) fn shutdown(&self) {
        self.raise_stop();
        let readers: Vec<_> = std::mem::take(&mut *lock(&self.readers));
        for reader in readers {
            let _ = reader.join();
        }
    }
}

impl Drop for RouteLink {
    /// The reader holds no `Arc<RouteLink>`, so a link let go without a `shutdown` must end it here.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Forget the socket `generation` in `live`, if it is still the one lanes are given.
fn forget(live: &Mutex<Option<Connection>>, generation: u64) {
    let mut live = lock(live);
    if live.as_ref().is_some_and(|c| c.generation == generation) {
        *live = None;
    }
}

/// A lane's view of its route's socket: a read-only [`WebsocketSessionMarketStream`] over the frames the reader hands
/// it, whose idle clock is the socket's. Dropping it lets its key go.
pub(super) struct LaneStream {
    rx: Receiver<Inbound>,
    id: u64,
    key: (Verb, String),
    conn: Connection,
    link: Arc<RouteLink>,
    poll: Duration,
}

impl WebsocketSessionMarketStream for LaneStream {
    fn read_frame(&mut self) -> Result<String, WebsocketSessionUserError> {
        match self.rx.recv_timeout(self.poll) {
            Ok(Inbound::Text(text)) => Ok(text),
            Ok(Inbound::Closed(why)) => Err(WebsocketSessionUserError::Closed(why)),
            Err(RecvTimeoutError::Timeout) => Err(WebsocketSessionUserError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(WebsocketSessionUserError::Closed("the market-data socket ended".to_string()))
            }
        }
    }

    fn send_text(&mut self, _s: &str) -> Result<(), WebsocketSessionUserError> {
        Err(WebsocketSessionUserError::Closed(
            "a lane's stream is read-only: the link owns the socket".to_string(),
        ))
    }

    fn since_last_frame(&self) -> Duration {
        lock(&self.conn.heard).elapsed()
    }
}

impl Drop for LaneStream {
    fn drop(&mut self) {
        self.link.detach(&self.conn, self.id, &self.key);
    }
}

/// Everything the reader thread owns of one socket.
struct Reader {
    route: StreamRoute,
    commands: Receiver<Command>,
    conn: Connection,
    live: Arc<Mutex<Option<Connection>>>,
    stop: Arc<AtomicBool>,
    idle: Option<Duration>,
}

impl Reader {
    /// Write what the lanes queued, read the socket, hand every text frame to every lane. Returns when the socket is
    /// over, after telling every lane why.
    fn run(self, mut socket: WsSocket) {
        let reason = 'socket: loop {
            if self.stop.load(Ordering::Relaxed) {
                break "stop requested".to_string();
            }
            loop {
                match self.commands.try_recv() {
                    Ok(Command::Frame(frame)) => {
                        if let Err(e) = socket.send(Message::Text(frame.into())) {
                            break 'socket format!("a subscribe frame could not be sent: {e}");
                        }
                    }
                    Ok(Command::Close) => {
                        break 'socket "no subscription is left on it".to_string();
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        break 'socket "the market-data link was dropped".to_string();
                    }
                }
            }
            match socket.read() {
                Ok(Message::Text(text)) => {
                    self.heard();
                    self.say_errors(text.as_str());
                    lock(&self.conn.lanes).routes.retain(|_, lane| {
                        lane.send(Inbound::Text(text.as_str().to_string())).is_ok()
                    });
                }
                Ok(Message::Ping(payload)) => {
                    self.heard();
                    if let Err(e) = socket.send(Message::Pong(payload)) {
                        break format!("a pong could not be sent: {e}");
                    }
                }
                Ok(Message::Close(_)) => break "server closed".to_string(),
                Ok(_) => self.heard(),
                Err(e) if is_timeout(&e) => {
                    if let Some(idle) = self.idle
                        && lock(&self.conn.heard).elapsed() >= idle
                    {
                        break AlpacaFeedRefusal::SilentStall { secs: idle.as_secs() }.to_string();
                    }
                }
                Err(e) => break e.to_string(),
            }
        };
        self.end(&reason);
    }

    fn heard(&self) {
        *lock(&self.conn.heard) = Instant::now();
    }

    /// A venue `error` frame is not data, and the lanes decode it to nothing: it is said here, once for the socket,
    /// in the venue's own words (no credential travels in them).
    fn say_errors(&self, text: &str) {
        for e in decode_control_errors(text) {
            tracing::warn!(
                venue = VENUE,
                route = ?self.route,
                code = ?e.code,
                msg = %e.msg,
                "market-data WS: the venue sent an error frame"
            );
        }
    }

    /// The socket is over: forget it, then tell every lane why.
    fn end(&self, reason: &str) {
        forget(&self.live, self.conn.generation);
        let lanes: Vec<_> = lock(&self.conn.lanes).routes.drain().collect();
        tracing::info!(venue = VENUE, route = ?self.route, generation = self.conn.generation, lanes = lanes.len(), reason, "market-data socket ended; every subscription on it is told");
        for (_, lane) in lanes {
            let _ = lane.send(Inbound::Closed(reason.to_string()));
        }
    }
}

#[path = "connection_tests.rs"]
#[cfg(test)]
mod connection_tests;
