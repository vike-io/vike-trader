//! The ONE fake peer of this crate's suites: a scripted gatewayhub on loopback that speaks this crate's
//! own protocol, verifies the handshake with the real keys, and RECORDS every frame the client sends,
//! so "nothing was sent" is an empty slice and "the client replayed its held set" is a list of `Sub`
//! frames. A real daemon cannot prove either: it answers the frame either way.
//!
//! What it can do, by the tests that need it:
//!
//! * **rpc** — [`PeerConfig::rpc`] maps a request to [`Step`]s: frames to send, sleeps (during which a
//!   `FetchCancel` is noticed), or a hang. Paging, progress and failures are all steps.
//! * **stream** — [`PeerConfig::sub`] answers each `Sub` (ok with a deterministic key per symbol, an
//!   error, or silence); a test then pushes any [`StreamDown`] frame through the [`StreamHandle`] it
//!   gets from [`FakePeer::wait_stream`], or [`StreamHandle::kill`]s the socket to simulate a lost link.
//! * **feed** — the same, through [`FeedHandle`], for the options feeds.
//!
//! Each caller `#[path]`-includes it (a bare `mod` would resolve beside the binary root); a file under
//! `tests/support/` is no test target of its own. Every includer uses a different part of it, so the
//! includer's `mod` declaration carries the one `#[allow(dead_code, clippy::allow_attributes)]` (a
//! shared tests module is exactly the case the workspace lint policy names for it).

use std::collections::HashMap;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use vike_gatewayhub_client::client::{GatewayLink, GatewayTimings};
use vike_gatewayhub_client::proto::{
    GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, KNOWN_FEATURES, OptionsFeedDown, Request,
    Response, cd_venue_feature, md_venue_feature, read_frame, read_frame_raw, write_frame,
};
use vike_hub_proto::auth::{HubKeys, Scope, fresh_nonce};
use vike_market_stream::wire::{ByeReason, StreamDown, StreamUp, SubErrKind, SubOp};

/// One thing the peer saw, in arrival order.
#[derive(Debug, Clone)]
pub enum Seen {
    /// The opening `Hello`, with the version the client sent.
    Hello(u32),
    /// An `Auth`: the scope the client claimed and whether its mac verified.
    Auth(Scope, bool),
    /// A post-auth request on connection `conn`.
    Req { conn: usize, req: Request },
    /// A `stream`-class frame on connection `conn`.
    Up { conn: usize, frame: StreamUp },
}

/// What to do for one request.
#[derive(Debug, Clone)]
pub enum Step {
    /// Write this response.
    Send(Response),
    /// Wait; a `FetchCancel` arriving meanwhile ends the script.
    Sleep(Duration),
    /// Say nothing more, keep the socket open, and wait for a cancel or a hang-up.
    Hang,
    /// Close the connection.
    Close,
    /// Write the first bytes of a frame - a length prefix promising a body that never comes - and carry
    /// on with the script: a daemon that BEGAN an answer and wedged.
    Torn,
}

/// How to answer a `Sub`.
#[derive(Debug, Clone)]
pub enum SubAnswer {
    /// `SubOk` with this session's key for the symbol (assigned 1, 2, 3 … in first-seen order).
    Ok,
    /// `SubErr`.
    Err(SubErrKind),
    /// No reply at all.
    Silent,
}

/// A script for the rpc class: request in, steps out.
pub type RpcScript = Arc<dyn Fn(&Request) -> Vec<Step> + Send + Sync>;

/// A script for the stream class: the client's `sid` and op in, the answer out.
pub type SubScript = Arc<dyn Fn(u64, &SubOp) -> SubAnswer + Send + Sync>;

/// How the peer behaves.
pub struct PeerConfig {
    /// The `Welcome.features`.
    pub features: Vec<String>,
    /// The keys it verifies against.
    pub keys: HubKeys,
    /// The `Welcome.proto_version` (a mismatch test changes it).
    pub version: u32,
    /// The heartbeat period it states in `DataOpened` / `OptionsFeedOpened`.
    pub heartbeat_ms: u64,
    /// Close every connection straight after accepting it (a daemon that dies on contact).
    pub close_on_accept: bool,
    /// Request → steps, for the rpc class.
    pub rpc: RpcScript,
    /// `Sub` → answer, for the stream class.
    pub sub: SubScript,
}

impl PeerConfig {
    /// A healthy daemon: the full wave-1 feature set for `binance`/`polymarket`/`hyperliquid`
    /// streams and an `oanda` credentialed lane, the test keys, every `Sub` acknowledged and every
    /// rpc answered with `Response::Error` until a test scripts it.
    pub fn new() -> Self {
        Self {
            features: features(&["binance", "polymarket", "hyperliquid"], &["oanda"]),
            keys: test_keys(),
            version: GATEWAYHUB_PROTO_VERSION,
            heartbeat_ms: 1_000,
            close_on_accept: false,
            rpc: Arc::new(|_| vec![Step::Send(Response::Error("fake peer: no script".into()))]),
            sub: Arc::new(|_, _| SubAnswer::Ok),
        }
    }

    /// Script the rpc class.
    #[must_use]
    pub fn with_rpc(mut self, f: impl Fn(&Request) -> Vec<Step> + Send + Sync + 'static) -> Self {
        self.rpc = Arc::new(f);
        self
    }

    /// Script the `Sub` answers.
    #[must_use]
    pub fn with_sub(
        mut self,
        f: impl Fn(u64, &SubOp) -> SubAnswer + Send + Sync + 'static,
    ) -> Self {
        self.sub = Arc::new(f);
        self
    }

    /// Replace the advertised features.
    #[must_use]
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }
}

impl Default for PeerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// The observe/control keys the fake verifies and the tests sign with.
pub fn test_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), b"control-key-of-the-tests".to_vec())
}

/// Keys with a Read key only — a datahub that holds no control key.
pub fn read_only_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-the-tests".to_vec(), Vec::new())
}

/// The full wave-1 feature set with the given stream and credentialed venues.
pub fn features(md: &[&str], cd: &[&str]) -> Vec<String> {
    let mut f: Vec<String> = KNOWN_FEATURES.iter().map(|s| (*s).to_string()).collect();
    f.extend(md.iter().map(|v| md_venue_feature(v)));
    f.extend(cd.iter().map(|v| cd_venue_feature(v)));
    f
}

/// Timings short enough that a test watches a stall in milliseconds. (A stream deadline cannot go
/// under three seconds: the stated heartbeat is clamped to its one-second minimum.)
pub fn fast_timings() -> GatewayTimings {
    GatewayTimings {
        reply_idle: Duration::from_secs(5),
        lookup_reply: Duration::from_secs(5),
        fetch_stall_floor: Duration::from_millis(600),
        fetch_probe_slice: Duration::from_millis(20),
        stream_read_floor: Duration::from_millis(300),
        sub_reply: Duration::from_millis(600),
        reconnect_base: Duration::from_millis(20),
        reconnect_max: Duration::from_millis(100),
    }
}

/// An address nothing listens on: a connect is refused at once.
pub fn dead_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("its address");
    drop(listener);
    addr.to_string()
}

/// A link to `addr` with the test keys and [`fast_timings`].
pub fn link_to(addr: &str) -> GatewayLink {
    GatewayLink::new(addr, test_keys()).with_timings(fast_timings())
}

struct StreamSession {
    writer: Arc<Mutex<TcpStream>>,
    keys: Arc<Mutex<HashMap<String, u32>>>,
}

struct Shared {
    log: Mutex<Vec<Seen>>,
    streams: Mutex<Vec<StreamSession>>,
    feeds: Mutex<Vec<Arc<Mutex<TcpStream>>>>,
    conns: AtomicUsize,
    sockets: Mutex<Vec<TcpStream>>,
    stop: AtomicBool,
    epoch: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running fake gatewayhub.
pub struct FakePeer {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    acceptor: Option<JoinHandle<()>>,
    handlers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

/// A handle on one open stream session: push frames at the client, or cut the socket.
#[derive(Clone)]
pub struct StreamHandle {
    writer: Arc<Mutex<TcpStream>>,
    keys: Arc<Mutex<HashMap<String, u32>>>,
}

impl StreamHandle {
    /// Write one frame to the client.
    pub fn send(&self, frame: &StreamDown) {
        let mut w = lock(&self.writer);
        write_frame(&mut *w, frame).expect("the client is still reading");
    }

    /// Write one frame, ignoring a client that has gone.
    pub fn try_send(&self, frame: &StreamDown) -> bool {
        let mut w = lock(&self.writer);
        write_frame(&mut *w, frame).is_ok()
    }

    /// The key this session assigned to `symbol`, once a `Sub` for it was acknowledged.
    pub fn key_of(&self, symbol: &str) -> Option<u32> {
        lock(&self.keys).get(symbol).copied()
    }

    /// Cut the socket: the client sees a lost link.
    pub fn kill(&self) {
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
    }

    /// End the session the way a daemon does.
    pub fn bye(&self, reason: ByeReason) {
        self.send(&StreamDown::Bye(reason));
        self.kill();
    }
}

/// A handle on one open options feed.
#[derive(Clone)]
pub struct FeedHandle {
    writer: Arc<Mutex<TcpStream>>,
}

impl FeedHandle {
    /// Write one frame to the client.
    pub fn send(&self, frame: &OptionsFeedDown) {
        let mut w = lock(&self.writer);
        write_frame(&mut *w, frame).expect("the client is still reading");
    }

    /// Cut the socket.
    pub fn kill(&self) {
        let _ = lock(&self.writer).shutdown(Shutdown::Both);
    }
}

enum Msg {
    Req(Request),
    Up(StreamUp),
}

impl FakePeer {
    /// Start a peer and begin accepting.
    pub fn start(config: PeerConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("resolve the assigned port");
        listener.set_nonblocking(true).expect("non-blocking accept");
        let shared = Arc::new(Shared {
            log: Mutex::new(Vec::new()),
            streams: Mutex::new(Vec::new()),
            feeds: Mutex::new(Vec::new()),
            conns: AtomicUsize::new(0),
            sockets: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            epoch: AtomicU64::new(0xE0_0001),
        });
        let config = Arc::new(config);
        let handlers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let (sh, cf, hs) = (Arc::clone(&shared), Arc::clone(&config), Arc::clone(&handlers));
        let acceptor = thread::Builder::new()
            .name("fake-gw-accept".into())
            .spawn(move || {
                while !sh.stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = stream.set_nonblocking(false);
                            if let Ok(clone) = stream.try_clone() {
                                lock(&sh.sockets).push(clone);
                            }
                            let conn = sh.conns.fetch_add(1, Ordering::SeqCst);
                            let (sh2, cf2) = (Arc::clone(&sh), Arc::clone(&cf));
                            let h = thread::Builder::new()
                                .name("fake-gw-conn".into())
                                .spawn(move || handle(conn, stream, &cf2, &sh2))
                                .expect("spawn a connection handler");
                            lock(&hs).push(h);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn the acceptor");
        Self { addr, shared, acceptor: Some(acceptor), handlers }
    }

    /// The address as the string a link takes.
    pub fn addr_string(&self) -> String {
        self.addr.to_string()
    }

    /// A link to this peer: the test keys and [`fast_timings`].
    pub fn link(&self) -> GatewayLink {
        link_to(&self.addr_string())
    }

    /// Every frame seen so far.
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.shared.log).clone()
    }

    /// The post-auth requests seen so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Req { req, .. } => Some(req),
                _ => None,
            })
            .collect()
    }

    /// The `stream`-class frames seen so far, in order, across every session.
    pub fn ups(&self) -> Vec<StreamUp> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Up { frame, .. } => Some(frame),
                _ => None,
            })
            .collect()
    }

    /// How many `Hello`s arrived.
    pub fn hellos(&self) -> usize {
        self.seen().iter().filter(|s| matches!(s, Seen::Hello(_))).count()
    }

    /// The `Auth` frames seen: `(claimed scope, mac verified)`.
    pub fn auths(&self) -> Vec<(Scope, bool)> {
        self.seen()
            .into_iter()
            .filter_map(|s| match s {
                Seen::Auth(scope, ok) => Some((scope, ok)),
                _ => None,
            })
            .collect()
    }

    /// How many TCP connections were accepted.
    pub fn connections(&self) -> usize {
        self.shared.conns.load(Ordering::SeqCst)
    }

    /// Wait until stream session number `index` (0-based) has opened, and return its handle.
    pub fn wait_stream(&self, index: usize, within: Duration) -> StreamHandle {
        let deadline = Instant::now() + within;
        loop {
            if let Some(s) = lock(&self.shared.streams).get(index) {
                return StreamHandle { writer: Arc::clone(&s.writer), keys: Arc::clone(&s.keys) };
            }
            assert!(Instant::now() < deadline, "stream session {index} never opened");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Wait until feed number `index` (0-based) has opened, and return its handle.
    pub fn wait_feed(&self, index: usize, within: Duration) -> FeedHandle {
        let deadline = Instant::now() + within;
        loop {
            if let Some(w) = lock(&self.shared.feeds).get(index) {
                return FeedHandle { writer: Arc::clone(w) };
            }
            assert!(Instant::now() < deadline, "feed {index} never opened");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Poll `cond` until it holds or `within` passes; whether it held.
    pub fn wait_until(within: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for FakePeer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(a) = self.acceptor.take() {
            let _ = a.join();
        }
        for s in lock(&self.shared.sockets).iter() {
            let _ = s.shutdown(Shutdown::Both);
        }
        let handlers: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.handlers));
        for h in handlers {
            let _ = h.join();
        }
    }
}

fn note(shared: &Shared, seen: Seen) {
    lock(&shared.log).push(seen);
}

/// One connection. However it ends, the socket is shut: the acceptor keeps a clone of every accepted
/// stream (so a dropped peer can cut its sessions), and a clone alone would keep a refused connection
/// open until the client's handshake deadline.
fn handle(conn: usize, stream: TcpStream, cfg: &Arc<PeerConfig>, shared: &Arc<Shared>) {
    let closer = stream.try_clone().ok();
    handle_connection(conn, stream, cfg, shared);
    if let Some(c) = closer {
        let _ = c.shutdown(Shutdown::Both);
    }
}

/// The handshake, then a reader thread feeding a loop that answers.
fn handle_connection(
    conn: usize,
    mut stream: TcpStream,
    cfg: &Arc<PeerConfig>,
    shared: &Arc<Shared>,
) {
    let _ = stream.set_nodelay(true);
    if cfg.close_on_accept {
        return;
    }
    let Ok(Request::Hello { proto_version }) = read_frame::<_, Request>(&mut stream) else {
        return;
    };
    note(shared, Seen::Hello(proto_version));
    let nonce = fresh_nonce();
    let welcome =
        Response::Welcome { proto_version: cfg.version, features: cfg.features.clone(), nonce };
    if write_frame(&mut stream, &welcome).is_err() {
        return;
    }
    // The client may hang up here (it refused the version or the plane BEFORE signing).
    let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
        return;
    };
    let ok = cfg.keys.verify(GATEWAYHUB_DOMAIN, scope, &nonce, GATEWAYHUB_PROTO_VERSION, &mac);
    note(shared, Seen::Auth(scope, ok));
    if !ok {
        let _ = write_frame(&mut stream, &Response::AuthDenied { reason: "bad mac".into() });
        return;
    }
    if write_frame(&mut stream, &Response::AuthOk { scope }).is_err() {
        return;
    }

    let writer = Arc::new(Mutex::new(stream.try_clone().expect("clone the socket")));
    let stream_mode = Arc::new(AtomicBool::new(false));
    let (tx, rx): (Sender<Msg>, Receiver<Msg>) = mpsc::channel();
    let (sh, mode) = (Arc::clone(shared), Arc::clone(&stream_mode));
    let reader = thread::Builder::new()
        .name("fake-gw-read".into())
        .spawn(move || {
            let mut stream = stream;
            while let Ok(body) = read_frame_raw(&mut stream) {
                if mode.load(Ordering::SeqCst) {
                    match serde_json::from_slice::<StreamUp>(&body) {
                        Ok(frame) => {
                            note(&sh, Seen::Up { conn, frame: frame.clone() });
                            if tx.send(Msg::Up(frame)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                } else {
                    match serde_json::from_slice::<Request>(&body) {
                        Ok(req) => {
                            note(&sh, Seen::Req { conn, req: req.clone() });
                            if tx.send(Msg::Req(req)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        })
        .expect("spawn the connection reader");

    serve(conn, cfg, shared, &writer, &stream_mode, &rx);
    let _ = lock(&writer).shutdown(Shutdown::Both);
    let _ = reader.join();
}

fn send(writer: &Arc<Mutex<TcpStream>>, response: &Response) -> bool {
    let mut w = lock(writer);
    write_frame(&mut *w, response).is_ok()
}

fn serve(
    _conn: usize,
    cfg: &Arc<PeerConfig>,
    shared: &Arc<Shared>,
    writer: &Arc<Mutex<TcpStream>>,
    stream_mode: &Arc<AtomicBool>,
    rx: &Receiver<Msg>,
) {
    let keys: Arc<Mutex<HashMap<String, u32>>> = Arc::new(Mutex::new(HashMap::new()));
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Req(Request::FetchCancel { .. }) => return,
            Msg::Req(Request::Ping) => {
                if !send(writer, &Response::Pong) {
                    return;
                }
            }
            Msg::Req(Request::DataOpen { .. }) => {
                stream_mode.store(true, Ordering::SeqCst);
                let epoch = shared.epoch.load(Ordering::SeqCst);
                let opened =
                    Response::DataOpened { session: 1, epoch, heartbeat_ms: cfg.heartbeat_ms };
                if !send(writer, &opened) {
                    return;
                }
                lock(&shared.streams)
                    .push(StreamSession { writer: Arc::clone(writer), keys: Arc::clone(&keys) });
            }
            Msg::Req(Request::OptionsFeedOpen { .. }) => {
                let epoch = shared.epoch.load(Ordering::SeqCst);
                let opened = Response::OptionsFeedOpened { epoch, heartbeat_ms: cfg.heartbeat_ms };
                if !send(writer, &opened) {
                    return;
                }
                lock(&shared.feeds).push(Arc::clone(writer));
            }
            Msg::Req(req) => {
                if !run_steps((cfg.rpc)(&req), writer, rx) {
                    return;
                }
            }
            Msg::Up(StreamUp::Sub { sid, op, .. }) => match (cfg.sub)(sid, &op) {
                SubAnswer::Ok => {
                    let k = {
                        let mut table = lock(&keys);
                        let next = u32::try_from(table.len()).expect("few keys") + 1;
                        *table.entry(op.symbol().to_string()).or_insert(next)
                    };
                    let frame = StreamDown::SubOk { sid, k };
                    let mut w = lock(writer);
                    if write_frame(&mut *w, &frame).is_err() {
                        return;
                    }
                }
                SubAnswer::Err(err) => {
                    let frame = StreamDown::SubErr { sid, err };
                    let mut w = lock(writer);
                    if write_frame(&mut *w, &frame).is_err() {
                        return;
                    }
                }
                SubAnswer::Silent => {}
            },
            Msg::Up(StreamUp::Ping) => {
                let mut w = lock(writer);
                if write_frame(&mut *w, &StreamDown::Heartbeat).is_err() {
                    return;
                }
            }
            Msg::Up(StreamUp::Close) => {
                let mut w = lock(writer);
                let _ = write_frame(&mut *w, &StreamDown::Bye(ByeReason::ClientClosed));
                return;
            }
            Msg::Up(
                StreamUp::Unsub { .. } | StreamUp::Delivery { .. } | StreamUp::BeginShutdown,
            ) => {}
        }
    }
}

/// Run a script; `false` when the connection should end (a cancel, a close, a hang-up).
fn run_steps(steps: Vec<Step>, writer: &Arc<Mutex<TcpStream>>, rx: &Receiver<Msg>) -> bool {
    for step in steps {
        match step {
            Step::Send(response) => {
                if !send(writer, &response) {
                    return false;
                }
            }
            Step::Close => return false,
            Step::Torn => {
                use std::io::Write as _;
                let mut w = lock(writer);
                if w.write_all(&256u32.to_be_bytes()).and_then(|()| w.flush()).is_err() {
                    return false;
                }
            }
            Step::Sleep(d) => {
                let deadline = Instant::now() + d;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(left) {
                        Ok(Msg::Req(Request::FetchCancel { .. })) => return false,
                        Ok(_) => {}
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => return false,
                    }
                }
            }
            Step::Hang => loop {
                match rx.recv() {
                    Ok(Msg::Req(Request::FetchCancel { .. })) | Err(_) => return false,
                    Ok(_) => {}
                }
            },
        }
    }
    true
}
