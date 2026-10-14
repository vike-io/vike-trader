//! A RAW gatewayhub peer: the handshake and the frames, with no client library between the test and
//! the bytes. It exists for the properties a real client hides: that the daemon refuses a Read key a
//! credentialed verb (the client refuses it locally, before anything is sent), that a refusal is the
//! coarse `bad mac`, the exact frames and sequence numbers of a stream, and how a fetch's frames are
//! paced.
//!
//! Each caller `#[path]`-includes it.

use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vike_gatewayhub_client::proto::{
    GATEWAYHUB_DOMAIN, GATEWAYHUB_PROTO_VERSION, Request, Response, read_frame, write_frame,
};
use vike_hub_proto::auth::{self, HubKeys, Scope};
use vike_market_stream::wire::{StreamDown, StreamUp};

/// How long a raw read waits before the test calls the daemon dead.
pub const RAW_TIMEOUT: Duration = Duration::from_secs(10);

/// One raw connection.
pub struct Raw {
    /// The socket.
    pub stream: TcpStream,
    /// The features the `Welcome` carried (after [`Raw::hello`]).
    pub features: Vec<String>,
    nonce: [u8; 32],
}

impl Raw {
    /// Connect, with no handshake.
    pub fn connect(addr: SocketAddr) -> Raw {
        let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
        stream.set_read_timeout(Some(RAW_TIMEOUT)).expect("read timeout");
        stream.set_nodelay(true).expect("nodelay");
        Raw { stream, features: Vec::new(), nonce: [0; 32] }
    }

    /// Connect and authenticate under `scope` with `keys`, expecting `AuthOk`.
    pub fn open(addr: SocketAddr, keys: &HubKeys, scope: Scope) -> Raw {
        let mut raw = Raw::connect(addr);
        raw.hello();
        match raw.authenticate(keys.key_for(scope), scope) {
            Response::AuthOk { scope: granted } => assert_eq!(granted, scope),
            other => panic!("expected AuthOk, got {other:?}"),
        }
        raw
    }

    /// Send `Hello` and read the `Welcome`: `(version, features)`.
    pub fn hello(&mut self) -> (u32, Vec<String>) {
        self.hello_with_version(GATEWAYHUB_PROTO_VERSION)
    }

    /// [`Raw::hello`] announcing another protocol version.
    pub fn hello_with_version(&mut self, version: u32) -> (u32, Vec<String>) {
        self.send(&Request::Hello { proto_version: version });
        match self.recv::<Response>() {
            Response::Welcome { proto_version, features, nonce } => {
                self.nonce = nonce;
                self.features = features.clone();
                (proto_version, features)
            }
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    /// The challenge the last `Welcome` carried (after [`Raw::hello`]).
    pub fn nonce(&self) -> [u8; 32] {
        self.nonce
    }

    /// Answer the challenge signing with `key` (any bytes: a wrong key is the point of some tests).
    pub fn authenticate(&mut self, key: &[u8], scope: Scope) -> Response {
        self.authenticate_versioned(key, scope, GATEWAYHUB_PROTO_VERSION)
    }

    /// Answer the challenge with a ready-made `mac`: one signed elsewhere (another connection's
    /// nonce, another service's domain), which is the point of the replay and domain tests.
    pub fn authenticate_with_mac(&mut self, scope: Scope, mac: Vec<u8>) -> Response {
        self.send(&Request::Auth { scope, mac });
        self.recv()
    }

    /// [`Raw::authenticate`] signing under another protocol version.
    pub fn authenticate_versioned(&mut self, key: &[u8], scope: Scope, version: u32) -> Response {
        let mac = auth::sign(GATEWAYHUB_DOMAIN, key, &self.nonce, version, scope);
        self.send(&Request::Auth { scope, mac });
        self.recv()
    }

    /// Write one frame.
    pub fn send<T: Serialize>(&mut self, msg: &T) {
        write_frame(&mut self.stream, msg).expect("write a frame");
    }

    /// Write raw bytes (a torn or oversized frame).
    pub fn send_bytes(&mut self, bytes: &[u8]) {
        use std::io::Write;
        self.stream.write_all(bytes).expect("write bytes");
    }

    /// Read one frame, panicking on a fault or a timeout.
    pub fn recv<T: DeserializeOwned>(&mut self) -> T {
        self.try_recv().unwrap_or_else(|e| panic!("reading a frame: {e}"))
    }

    /// Read one frame.
    pub fn try_recv<T: DeserializeOwned>(&mut self) -> io::Result<T> {
        read_frame(&mut self.stream)
    }

    /// Read one frame with a custom wait.
    pub fn recv_within<T: DeserializeOwned>(&mut self, wait: Duration) -> io::Result<T> {
        self.stream.set_read_timeout(Some(wait))?;
        let r = read_frame(&mut self.stream);
        self.stream.set_read_timeout(Some(RAW_TIMEOUT))?;
        r
    }

    /// Whether the daemon has closed the connection (a read returns EOF or a reset) within `wait`.
    pub fn closed_within(&mut self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut byte = [0u8; 1];
        while Instant::now() < deadline {
            self.stream.set_read_timeout(Some(Duration::from_millis(50))).expect("timeout");
            match self.stream.read(&mut byte) {
                Ok(0) => return true,
                Ok(_) => {}
                Err(e)
                    if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
                Err(_) => return true,
            }
        }
        false
    }

    /// Open a stream for `venue` and return the `(session, epoch, heartbeat_ms)` of `DataOpened`.
    pub fn open_stream(&mut self, venue: &str) -> (u64, u64, u64) {
        self.send(&Request::DataOpen { venue: venue.to_string() });
        match self.recv::<Response>() {
            Response::DataOpened { session, epoch, heartbeat_ms } => (session, epoch, heartbeat_ms),
            other => panic!("expected DataOpened, got {other:?}"),
        }
    }

    /// Send one `StreamUp`.
    pub fn up(&mut self, frame: &StreamUp) {
        self.send(frame);
    }

    /// Read one `StreamDown`.
    pub fn down(&mut self) -> StreamDown {
        self.recv()
    }

    /// Read `StreamDown` frames until `pred` matches one, returning every frame read (the match last).
    /// Heartbeats are skipped.
    pub fn down_until(&mut self, mut pred: impl FnMut(&StreamDown) -> bool) -> Vec<StreamDown> {
        let mut out = Vec::new();
        loop {
            let frame = self.down();
            if matches!(frame, StreamDown::Heartbeat) {
                continue;
            }
            let hit = pred(&frame);
            out.push(frame);
            if hit {
                return out;
            }
        }
    }
}
