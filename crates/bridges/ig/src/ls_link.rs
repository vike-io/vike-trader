//! **The ONE Lightstreamer link of an IG market-data client**: one REST login and one WebSocket session carry
//! every subscription of a [`crate::market_feed::Feeds`], where each subscription used to log in and dial for
//! itself (a mount's quote lane, each bar interval and every extra epic the control door admitted were each a
//! `POST /session` and a Lightstreamer socket of the SAME API key).
//!
//! # Why
//!
//! IG's FAQ asks a client not to create multiple concurrent connections, since that may get the API key
//! suspended, and its streaming guide calls connecting more than once to get past the 40 subscriptions of a
//! connection a breach of its terms that may revoke API permissions (`docs/ops/known-limitations-2026-10.md`
//! carries the row). A busy key came to about six sessions or more by hand count. After this module the market
//! data of a client is one login and one socket however many epics it streams; the exec trade stream (an HTTP
//! streaming session, `crate::exec::stream`) and the REST sessions of the exec, reconcile and facts lanes are
//! other things of the same key and are not folded in.
//!
//! # The shape: lanes keep their loop, the link owns the socket
//!
//! Every subscription still has its own thread riding the shared market pump (`run_market_feed_on`), its own
//! failure ladder ([`crate::market_feed`]'s `Refusals`), its own silence and stall watch and its own gap
//! disclosure: none of that is shared state and none of it changed. What a lane no longer owns is the SOCKET.
//! [`Link::attach`] gives it a [`LaneStream`], a read-only [`MarketStream`] fed from a channel, and the link's one
//! reader thread demultiplexes the socket into those channels:
//!
//! * `U` / `SUBOK` / `EOS` / `CONF` / `UNSUB` of subscription `g` go to the lane that owns `g`, rewritten to the
//!   lane-local id [`SUB_ID`] (`1`, which the lane's parser has always expected), so a lane cannot tell its
//!   shared session from a private one;
//! * a `REQERR` goes to the lane whose `add` it answers (the request id of an `add` IS its subscription id; a
//!   `delete` rides ids above [`DELETE_REQ_BASE`], whose answers nobody reads);
//! * everything without a subscription id (`PROBE` above all, the stall watch's heartbeat) is broadcast;
//! * `CONERR`, `END`, `LOOP`, a transport error and the idle bound end the CONNECTION: every lane is told (the
//!   frame first, then a close), the link forgets the socket, and the next lane to ask dials a new one, once.
//!
//! A subscription that IG refuses (`REQERR`), that goes silent or that stalls therefore costs the lane a
//! `delete` and a new `add` on the same socket, where it used to cost a whole redial: a stalled epic no longer
//! tears down the quotes of the others. Subscription ids are the connection's own (a new socket numbers from `1`
//! again) and a lane that comes back gets a NEW id, so a late line of its old subscription finds no route.
//!
//! # No subscription vanishes silently
//!
//! A lane that asked is always told how it ended: a refused `add` is the lane's `REQERR`; a socket that ends takes
//! every lane with it by a `Closed` (the reader drains the routes AFTER it forgets the connection, and
//! [`Link::subscribe`] registers a route BEFORE it checks the connection is still the live one, so a lane cannot
//! slip between the two); a link at IG's 40 subscriptions refuses the 41st lane in words, into its ladder, instead
//! of queueing an `add` IG would refuse; a stop ends the reader, which tells every lane. A lane that is told ends
//! its session and redials through its own failure ladder ([`crate::market_feed`]'s `Refusals`) and the pump's
//! backoff, so nothing here retries behind a lane's back.
//!
//! # Locks, and what a lane waits for
//!
//! One dial at a time: a lane that finds no live socket takes the dial lock or waits for it in 25 ms stop-aware
//! naps (so `unsubscribe` is never held behind a stranger's dial longer than the nap), and a failed dial is
//! shared with the lanes that ask within [`FAILURE_SHARED_FOR`] of it instead of each paying the connect bound
//! in turn (a dial the dialing lane itself abandoned, because its own stop was raised, is not shared: it says
//! nothing about IG). The state lock is never held across I/O, and the state and route locks are never held
//! together.
//!
//! # What is NOT here
//!
//! * The refusal ladders and their wording stay per lane (`Refusals`): N lanes on a dead connection each warn
//!   once, as N private sessions did.
//! * One link per `Feeds`, not per key: two `Feeds` of the same account (the gatewayhub builds one per data
//!   session) are two links. The count that mattered, one per EPIC, is gone.
//! * A link with no subscription left is not closed: it idles on `PROBE`s until the client shuts down or IG ends
//!   the session.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vike_bridge_core::market_pump::MarketStream;
use vike_bridge_core::pump_spec::market_pump_spec;
use vike_bridge_core::user_data::StreamError;

use crate::config::IgConfig;
use crate::lightstreamer::{subscribe_request, unsubscribe_request};
use crate::market_facts::login_refused_for_good;
use crate::market_feed::{HandshakeErr, LsAuth, LsStream, SUB_ID, ls_ws_url, open_ls_session};
use crate::rest::IgSession;

const VENUE: &str = "ig";

/// Request ids of a `delete` start here; an `add` uses its subscription id as its request id, so a `REQERR` below
/// this names the lane that asked.
const DELETE_REQ_BASE: u32 = 1 << 30;

/// The most subscriptions one Lightstreamer connection may hold at IG (its streaming guide: 40).
const MAX_SUBSCRIPTIONS: usize = 40;

/// The reader's socket poll: how soon a `control` a lane queued goes out, and how soon a stop is seen. A lane's own
/// read timeout (the pump row's 2 s) is a different thing: it paces the lane's tick hook.
const LINK_POLL: Duration = Duration::from_millis(250);

/// How long a failed dial is answered to the lanes that ask right behind it, without dialing again.
const FAILURE_SHARED_FOR: Duration = Duration::from_secs(1);

/// Why a lane could not attach: what its connect closure counts into its own run of failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AttachErr {
    /// The REST login failed. `for_good`: IG answered with a credentials error (the same details get the same
    /// answer, [`login_refused_for_good`]); otherwise a login that never reached IG, a 429 or a 5xx.
    Login { text: String, for_good: bool },
    /// The server answered `CONERR` to the session request.
    Conerr { code: String, message: String },
    /// Anything transport-shaped: a dial, a handshake, a refused thread, a stop.
    Transport(String),
}

/// What the reader hands a lane.
enum Inbound {
    /// One TLCP line, already rewritten to the lane-local subscription id.
    Line(String),
    /// The connection is over (the reason is the pump's message for the lane's failure ladder).
    Closed(String),
}

/// What a lane asks the reader to write.
enum Cmd {
    Send(String),
}

/// Subscription id to the lane that owns it.
type Routes = Arc<Mutex<HashMap<u32, Sender<Inbound>>>>;

/// The live connection, as lanes see it.
#[derive(Clone)]
struct ConnHandle {
    generation: u64,
    cmds: Sender<Cmd>,
    routes: Routes,
    /// The next subscription id of THIS connection (ids are a session's own: a new connection numbers from `1`).
    next_sub: Arc<AtomicU32>,
    /// The Lightstreamer user (the account id): `PRICE:{account}:{epic}` is named after it.
    account: String,
}

/// May a `CONERR` drop the cached login? Once per RUN of failures: the first is most likely a token that expired,
/// a second after a fresh login is not expiry and costs a WebSocket handshake, not a `POST /session`. But a run
/// only ends on a delivered update, so a link refused or silent for longer than a token lives would keep an
/// expired pair for ever: a login older than the guard may be dropped again, once per login (the replacement is
/// young).
#[derive(Debug, Default)]
pub(crate) struct ReloginGuard {
    spent: bool,
}

impl ReloginGuard {
    /// A `CONERR` arrived for a login `age` old: may it be dropped (`after` is the guard,
    /// [`crate::market_feed`]'s `RELOGIN_AFTER` in production)?
    pub(crate) fn allow(&mut self, age: Duration, after: Duration) -> bool {
        if age >= after {
            self.spent = true;
            return true;
        }
        !std::mem::replace(&mut self.spent, true)
    }

    /// A session delivered data: the run is over and the one free drop is back.
    pub(crate) fn delivered(&mut self) {
        self.spent = false;
    }
}

#[derive(Default)]
struct State {
    /// The Lightstreamer credentials of the last login, reused across reconnects until a `CONERR` proves them dead.
    auth: Option<LsAuth>,
    conn: Option<ConnHandle>,
    next_generation: u64,
    guard: ReloginGuard,
    /// The last failed dial and when, so the lanes right behind it share the answer.
    failed: Option<(Instant, AttachErr)>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The client's one login and one socket. See the module doc.
pub(crate) struct Link {
    config: IgConfig,
    /// [`crate::market_feed`]'s `RELOGIN_AFTER` (a test shortens it through `Feeds::with_test_watch_timing`).
    relogin_after: Duration,
    /// The pump row's idle bound: no frame of any kind for this long ends the connection.
    idle: Option<Duration>,
    connect_timeout: Option<Duration>,
    /// What a lane's `read_frame` waits for a frame (the pump row's read timeout).
    lane_poll: Duration,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    /// Held by the lane that is dialing.
    dial: Mutex<()>,
    readers: Mutex<Vec<JoinHandle<()>>>,
}

impl Link {
    pub(crate) fn new(config: IgConfig, relogin_after: Duration) -> Self {
        let knobs = market_pump_spec(VENUE).knobs();
        Link {
            config,
            relogin_after,
            idle: knobs.idle_threshold,
            connect_timeout: knobs.connect_timeout,
            lane_poll: knobs.read_timeout,
            stop: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(State::default())),
            dial: Mutex::new(()),
            readers: Mutex::new(Vec::new()),
        }
    }

    /// Subscribe one lane on the shared connection, dialing it first if no lane has. `item` names the lane's
    /// `(data adapter, LS_group)` once the account is known; `schema` is its field list. The returned stream is
    /// the lane's whole view of the session: dropping it ends the subscription (and nothing else).
    ///
    /// # Errors
    /// [`AttachErr`], which the lane counts into its own failure run exactly as it counted a private session's.
    pub(crate) fn attach(
        self: &Arc<Self>,
        lane_stop: &AtomicBool,
        item: &dyn Fn(&str) -> (&'static str, String),
        schema: &str,
    ) -> Result<LaneStream, AttachErr> {
        let conn = self.connection(lane_stop)?;
        self.subscribe(&conn, item, schema)
    }

    /// Register a lane on `conn` and queue its `add`. ⚠ A subscription never vanishes silently: the reader may
    /// have ended since the lane got `conn`, and what it drained is gone, so a lane that registers on a dead
    /// connection is refused here (the route goes in FIRST, then the connection is checked: a connection still
    /// current at that point has not drained yet, and its drain finds this lane and tells it).
    fn subscribe(
        self: &Arc<Self>,
        conn: &ConnHandle,
        item: &dyn Fn(&str) -> (&'static str, String),
        schema: &str,
    ) -> Result<LaneStream, AttachErr> {
        let closed =
            || AttachErr::Transport("the Lightstreamer link closed while subscribing".to_string());
        let sub = conn.next_sub.fetch_add(1, Ordering::Relaxed);
        let (lane_tx, lane_rx) = mpsc::channel();
        {
            let mut routes = lock(&conn.routes);
            if routes.len() >= MAX_SUBSCRIPTIONS {
                return Err(AttachErr::Transport(format!(
                    "IG allows {MAX_SUBSCRIPTIONS} subscriptions on one Lightstreamer connection and this client \
                     holds them all"
                )));
            }
            // Registered BEFORE the `add` goes out, so nothing the server answers can arrive unrouted.
            routes.insert(sub, lane_tx);
        }
        if !self.is_current(conn.generation) {
            lock(&conn.routes).remove(&sub);
            return Err(closed());
        }
        let (adapter, group) = item(&conn.account);
        let add = subscribe_request(sub, sub, adapter, "MERGE", &group, schema, true);
        if conn.cmds.send(Cmd::Send(add)).is_err() {
            lock(&conn.routes).remove(&sub);
            self.forget(conn.generation);
            return Err(closed());
        }
        Ok(LaneStream {
            rx: lane_rx,
            sub,
            generation: conn.generation,
            link: Arc::clone(self),
            poll: self.lane_poll,
            last_rx: Instant::now(),
        })
    }

    /// Is `generation` the connection lanes are being given?
    fn is_current(&self, generation: u64) -> bool {
        lock(&self.state).conn.as_ref().is_some_and(|c| c.generation == generation)
    }

    /// The live connection, dialing one if none is. One dialer at a time; the others wait in stop-aware naps.
    fn connection(&self, lane_stop: &AtomicBool) -> Result<ConnHandle, AttachErr> {
        loop {
            if lane_stop.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed) {
                return Err(AttachErr::Transport("stop requested".to_string()));
            }
            if let Some(conn) = lock(&self.state).conn.clone() {
                return Ok(conn);
            }
            let _dialing = match self.dial.try_lock() {
                Ok(guard) => guard,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(25));
                    continue;
                }
            };
            // Another lane may have finished while this one waited for the lock, or failed a moment ago.
            {
                let state = lock(&self.state);
                if let Some(conn) = state.conn.clone() {
                    return Ok(conn);
                }
                if let Some((at, why)) = &state.failed
                    && at.elapsed() < FAILURE_SHARED_FOR
                {
                    return Err(why.clone());
                }
            }
            return match self.open(lane_stop) {
                Ok(conn) => Ok(conn),
                Err(why) => {
                    // A dial the dialing lane abandoned itself (its stop) says nothing about IG: not shared.
                    if !lane_stop.load(Ordering::Relaxed) {
                        lock(&self.state).failed = Some((Instant::now(), why.clone()));
                    }
                    Err(why)
                }
            };
        }
    }

    /// Log in if no login is kept, dial, run the `create_session` handshake and start the reader. The handshake
    /// is cut short by the DIALING lane's stop (an `unsubscribe` is never held behind a slow dial); the reader
    /// that follows belongs to the link and ends with the link's own stop.
    fn open(&self, lane_stop: &AtomicBool) -> Result<ConnHandle, AttachErr> {
        let kept = lock(&self.state).auth.clone();
        let auth = match kept {
            Some(auth) => auth,
            None => self.log_in()?,
        };
        let stream = match open_ls_session(&auth, LINK_POLL, self.connect_timeout, lane_stop) {
            Ok(stream) => stream,
            // The cached CST/XST pair may be dead (a token that expired): `on_conerr` decides whether this
            // CONERR drops it, so the next dial logs in afresh.
            Err(HandshakeErr::Refused { code, message }) => {
                on_conerr(&self.state, self.relogin_after);
                return Err(AttachErr::Conerr { code, message });
            }
            Err(HandshakeErr::Transport(why)) => return Err(AttachErr::Transport(why)),
        };
        let (cmds, cmd_rx) = mpsc::channel();
        let routes: Routes = Arc::default();
        let generation = {
            let mut state = lock(&self.state);
            let generation = state.next_generation;
            state.next_generation += 1;
            generation
        };
        let env = ReaderEnv {
            cmds: cmd_rx,
            routes: Arc::clone(&routes),
            state: Arc::clone(&self.state),
            generation,
            stop: Arc::clone(&self.stop),
            idle: self.idle,
            relogin_after: self.relogin_after,
        };
        let reader = std::thread::Builder::new()
            .name("feed-ig-link".to_string())
            .spawn(move || {
                vike_exec::affinity::pin_current_thread(
                    vike_exec::affinity::Role::MarketData,
                    VENUE,
                );
                reader_main(stream, &env);
            })
            .map_err(|e| {
                AttachErr::Transport(format!("the OS refused the Lightstreamer reader thread: {e}"))
            })?;
        {
            let mut readers = lock(&self.readers);
            readers.retain(|h| !h.is_finished());
            readers.push(reader);
        }
        let conn = ConnHandle {
            generation,
            cmds,
            routes,
            next_sub: Arc::new(AtomicU32::new(1)),
            account: auth.user,
        };
        {
            let mut state = lock(&self.state);
            state.conn = Some(conn.clone());
            state.failed = None;
        }
        tracing::info!(target: "vike_ig::ls_link", generation, "IG market-data link: Lightstreamer session up (one socket for every market-data subscription of this client)");
        Ok(conn)
    }

    /// One `POST /session` for the Lightstreamer credentials, kept for every later dial until a `CONERR` drops them.
    fn log_in(&self) -> Result<LsAuth, AttachErr> {
        // IG ANSWERED with a credentials error (400, 401, a 403 that is not an allowance): the same credentials
        // get the same answer, and a documented lockout (`error.security.too-many-failed-attempts`) waits at the end
        // of repeated tries, so the lane's next dial waits the longest wait. A login that never reached IG, a 429
        // or a 5xx is an ordinary drop. The text carries IG's status and `errorCode`, never a credential.
        let session = IgSession::login(&self.config).map_err(|e| AttachErr::Login {
            for_good: login_refused_for_good(e.status, &e.message),
            text: e.to_string(),
        })?;
        if session.lightstreamer_endpoint.is_empty() {
            return Err(AttachErr::Transport(
                "login returned no lightstreamerEndpoint".to_string(),
            ));
        }
        let auth = LsAuth {
            ws_url: ls_ws_url(&session.lightstreamer_endpoint),
            user: session.account_id.clone(),
            password: session.ls_password(),
            logged_in: Instant::now(),
        };
        lock(&self.state).auth = Some(auth.clone());
        // The operator's count of IG logins and sockets (the journal is its surface): one line per login here,
        // one per session in `open`, one per ending in `end`; the 0.1.97 kit's `t97-ig-one-stream.sh` reads them.
        tracing::info!(target: "vike_ig::ls_link", "IG market-data link: logged in (one login for every market-data subscription of this client)");
        Ok(auth)
    }

    /// A lane's stream was dropped: end its subscription on the live connection (a connection that is gone took
    /// the subscription with it).
    fn detach(&self, sub: u32, generation: u64) {
        let conn = lock(&self.state).conn.clone();
        if let Some(conn) = conn.filter(|c| c.generation == generation) {
            lock(&conn.routes).remove(&sub);
            let _ = conn.cmds.send(Cmd::Send(unsubscribe_request(DELETE_REQ_BASE + sub, sub)));
        }
    }

    /// The connection `generation` is dead (a lane's write found its reader gone): forget it, so the next lane dials.
    fn forget(&self, generation: u64) {
        let mut state = lock(&self.state);
        if state.conn.as_ref().is_some_and(|c| c.generation == generation) {
            state.conn = None;
        }
    }

    /// Raise the stop flag and return at once: the reader ends within [`LINK_POLL`].
    pub(crate) fn raise_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stop the reader and join it. A link stays stopped: a client is shut down once.
    pub(crate) fn shutdown(&self) {
        self.raise_stop();
        let readers: Vec<_> = std::mem::take(&mut *lock(&self.readers));
        for reader in readers {
            let _ = reader.join();
        }
    }
}

impl Drop for Link {
    /// The last holder of a link (the client and every lane let go of it without a `shutdown`) must not leave
    /// its reader and its socket behind: the reader holds no `Arc<Link>`, so this is the only thing that ends it.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A lane's view of the shared session: a read-only [`MarketStream`] over the frames the reader routed to it.
/// Dropping it ends the lane's subscription on the link.
pub(crate) struct LaneStream {
    rx: Receiver<Inbound>,
    sub: u32,
    generation: u64,
    link: Arc<Link>,
    poll: Duration,
    last_rx: Instant,
}

impl MarketStream for LaneStream {
    fn read_frame(&mut self) -> Result<String, StreamError> {
        match self.rx.recv_timeout(self.poll) {
            Ok(Inbound::Line(line)) => {
                self.last_rx = Instant::now();
                Ok(line)
            }
            Ok(Inbound::Closed(why)) => Err(StreamError::Closed(why)),
            Err(RecvTimeoutError::Timeout) => Err(StreamError::Timeout),
            Err(RecvTimeoutError::Disconnected) => {
                Err(StreamError::Closed("the Lightstreamer link ended".to_string()))
            }
        }
    }

    fn send_text(&mut self, _s: &str) -> Result<(), StreamError> {
        Err(StreamError::Closed(
            "a lane's stream is read-only: the link owns the socket".to_string(),
        ))
    }

    fn since_last_frame(&self) -> Duration {
        self.last_rx.elapsed()
    }
}

impl Drop for LaneStream {
    fn drop(&mut self) {
        self.link.detach(self.sub, self.generation);
    }
}

/// A `CONERR` arrived: drop the cached login if [`ReloginGuard`] allows, so the next dial logs in afresh.
fn on_conerr(state: &Mutex<State>, relogin_after: Duration) {
    let mut state = lock(state);
    let age = state.auth.as_ref().map_or(Duration::ZERO, |a| a.logged_in.elapsed());
    if state.guard.allow(age, relogin_after) {
        state.auth = None;
    }
}

/// Everything the reader thread owns of one connection.
struct ReaderEnv {
    cmds: Receiver<Cmd>,
    routes: Routes,
    state: Arc<Mutex<State>>,
    generation: u64,
    stop: Arc<AtomicBool>,
    idle: Option<Duration>,
    relogin_after: Duration,
}

/// The reader: write what the lanes queued, read the socket, route every line. Returns when the connection is over
/// (a stop, a transport error, an `END`/`LOOP`/`CONERR`, the idle bound), after telling every lane.
fn reader_main(mut stream: LsStream, env: &ReaderEnv) {
    let mut delivered = false;
    let reason = 'session: loop {
        if env.stop.load(Ordering::Relaxed) {
            break "stop requested".to_string();
        }
        loop {
            match env.cmds.try_recv() {
                Ok(Cmd::Send(text)) => {
                    if let Err(e) = stream.send_text(&text) {
                        break 'session format!("control send failed: {e:?}");
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    break 'session "the Lightstreamer link was dropped".to_string();
                }
            }
        }
        match stream.read_frame() {
            Ok(text) => {
                for line in text.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
                    if let Some(over) = route_line(line, env, &mut delivered) {
                        break 'session over;
                    }
                }
            }
            Err(StreamError::Timeout) => {
                if let Some(idle) = env.idle
                    && stream.since_last_frame() >= idle
                {
                    break format!("no frames within {}s (silent stall)", idle.as_secs());
                }
            }
            Err(StreamError::Closed(why)) => break why,
        }
    };
    end(env, &reason);
}

/// The connection is over: forget it, then tell every lane why (a lane reads the frame that ended it first, when
/// there was one).
fn end(env: &ReaderEnv, reason: &str) {
    {
        let mut state = lock(&env.state);
        if state.conn.as_ref().is_some_and(|c| c.generation == env.generation) {
            state.conn = None;
        }
    }
    let lanes: Vec<_> = lock(&env.routes).drain().collect();
    tracing::info!(target: "vike_ig::ls_link", generation = env.generation, lanes = lanes.len(), reason, "IG market-data link: Lightstreamer session ended; every subscription on it is told");
    for (_, lane) in lanes {
        let _ = lane.send(Inbound::Closed(reason.to_string()));
    }
}

/// Route one TLCP line. `Some(reason)` = the connection is over.
fn route_line(line: &str, env: &ReaderEnv, delivered: &mut bool) -> Option<String> {
    let (tag, rest) = line.split_once(',').unwrap_or((line, ""));
    match tag {
        // `<tag>,<subscription id>[,<rest>]`: to the lane that owns the id, as its lane-local id.
        "U" | "SUBOK" | "EOS" | "CONF" | "CS" | "OV" | "UNSUB" => {
            let (id, tail) = match rest.split_once(',') {
                Some((id, tail)) => (id, Some(tail)),
                None => (rest, None),
            };
            let Ok(sub) = id.parse::<u32>() else { return None };
            if tag == "U" && !*delivered {
                // A session that DELIVERS ends the run of failures (a SUBOK alone does not).
                *delivered = true;
                lock(&env.state).guard.delivered();
            }
            let lane_line = match tail {
                Some(tail) => format!("{tag},{SUB_ID},{tail}"),
                None => format!("{tag},{SUB_ID}"),
            };
            send_to(&env.routes, sub, lane_line);
            None
        }
        // `REQERR,<request id>,<code>,<message>`: the lane whose `add` it answers (a `delete`'s is nobody's).
        "REQERR" => {
            let request = rest.split(',').next().and_then(|r| r.parse::<u32>().ok());
            if let Some(sub) = request.filter(|r| *r < DELETE_REQ_BASE) {
                send_to(&env.routes, sub, line.to_string());
            }
            None
        }
        "REQOK" => None,
        "CONERR" => {
            on_conerr(&env.state, env.relogin_after);
            broadcast(&env.routes, line);
            Some(line.to_string())
        }
        "END" | "LOOP" => {
            broadcast(&env.routes, line);
            Some(line.to_string())
        }
        // `PROBE`, `SERVNAME`, `CLIENTIP`, `CONS`, `SYNC`, `NOOP`, ...: no subscription id, every lane's heartbeat.
        _ => {
            broadcast(&env.routes, line);
            None
        }
    }
}

/// Hand `line` to the lane that owns `sub`; a lane that is gone loses its route.
fn send_to(routes: &Routes, sub: u32, line: String) {
    let mut routes = lock(routes);
    if routes.get(&sub).is_some_and(|lane| lane.send(Inbound::Line(line)).is_err()) {
        routes.remove(&sub);
    }
}

/// Hand `line` to every lane.
fn broadcast(routes: &Routes, line: &str) {
    lock(routes).retain(|_, lane| lane.send(Inbound::Line(line.to_string())).is_ok());
}

#[cfg(test)]
#[path = "ls_link_tests.rs"]
mod tests;
