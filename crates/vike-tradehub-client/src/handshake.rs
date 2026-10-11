//! The ONE node-client handshake — Hello → Welcome (version guard) → sign → Auth → AuthOk —
//! shared by every client connection path to a tradehub node, plus the one-shot verbs' shared
//! refusal ([`require_feature`]) and reply-mismatch ([`ReplyWords::mismatch`]) wording.
//!
//! Only the [`Scope`] (and so the signing key) differs between callers: the observe handle runs it
//! under [`Scope::Read`] and then subscribes; the control paths run it under [`Scope::Write`].
//! Crate-private: the handles are the surface, this is an implementation seam.
//!
//! ⚠ The error TEXTS built here are pinned by tests (`tests/*_negotiation.rs`,
//! `tests/strategy_verbs.rs`, and the string table in `remote_handle/reads.rs`): a caller's exit
//! ladder and the operator both read them, so change one only on purpose.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};

use crate::auth;
use crate::liveness::HANDSHAKE_REPLY_TIMEOUT;
use crate::proto::{NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame};

/// Connect to `addr` and run the full handshake under `scope`, signing the server's nonce with
/// `key`. Returns the AUTHED stream with nothing further sent, plus the server's advertised
/// `Welcome.features`, so a caller can refuse an unadvertised verb CLIENT-SIDE ([`require_feature`])
/// instead of sending a frame an older server cannot decode.
///
/// A protocol-version mismatch fails naming both versions; [`Response::AuthDenied`] surfaces as
/// [`io::ErrorKind::PermissionDenied`] with the server's reason; every other desync is
/// [`io::ErrorKind::InvalidData`].
///
/// ⚠ Both reads run under [`HANDSHAKE_REPLY_TIMEOUT`] (a half-open `ssh -L` tunnel accepts the local
/// connect and then never answers), and the deadline is CLEARED before the stream is handed back:
/// the session's read policy belongs to the caller.
pub(crate) fn node_handshake<A: ToSocketAddrs>(
    addr: A,
    key: &[u8],
    scope: Scope,
) -> io::Result<(TcpStream, Vec<String>)> {
    node_handshake_with_deadline(addr, key, scope, HANDSHAKE_REPLY_TIMEOUT)
}

/// [`node_handshake`] with the reply deadline as a PARAMETER — the seam the unit tests drive in
/// milliseconds. Single-caller in production: the deadline is a protocol fact, not a knob.
fn node_handshake_with_deadline<A: ToSocketAddrs>(
    addr: A,
    key: &[u8],
    scope: Scope,
    reply_deadline: std::time::Duration,
) -> io::Result<(TcpStream, Vec<String>)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(reply_deadline))?;
    // Nagle OFF on the dialled end (`vike_node_proto::frame::configure_node_stream`). A refusal
    // costs latency, never correctness, so it does not fail the connect.
    let _ = vike_node_proto::frame::configure_node_stream(&stream);

    // 1. Hello -> Welcome{ nonce } (with the protocol-version guard).
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION })?;
    let (nonce, server_version, features) = match read_frame::<_, Response>(&mut stream)? {
        Response::Welcome { proto_version, nonce, features } => {
            if proto_version != NODE_PROTO_VERSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "tradehub node protocol version mismatch: client speaks \
                         {NODE_PROTO_VERSION}, server speaks {proto_version}"
                    ),
                ));
            }
            (nonce, proto_version, features)
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub handshake: expected Welcome, got {}", resp_kind(&other)),
            ));
        }
    };

    // 2. Auth{ scope, mac } -> AuthOk / AuthDenied.
    let mac = auth::sign(key, &nonce, server_version, scope);
    write_frame(&mut stream, &Request::Auth { scope, mac })?;
    match read_frame::<_, Response>(&mut stream)? {
        Response::AuthOk { scope: granted } if granted == scope => {}
        Response::AuthOk { scope: granted } => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub auth: server granted unexpected scope {granted:?}"),
            ));
        }
        Response::AuthDenied { reason } => {
            // The lowercase scope name ("tradehub control auth denied: …") names the refused path.
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("tradehub {} auth denied: {reason}", scope.name()),
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub auth: expected AuthOk, got {}", resp_kind(&other)),
            ));
        }
    }

    // NO read policy on the way out: an inherited 10 s bound would kill an idle control connection.
    stream.set_read_timeout(None)?;
    Ok((stream, features))
}

/// How a client-side capability refusal names the verb it withheld — the words between
/// `capability` and `refused client-side, nothing was sent` in [`require_feature`]'s message.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    /// `… capability (an older vike-tradehub) — {what} refused client-side, nothing was sent`.
    OlderNode(&'static str),
    /// `… capability — {what} refused client-side, nothing was sent` (no claim about the peer's
    /// age: the tearsheet wording).
    Plain(&'static str),
}

/// Refuse a verb CLIENT-SIDE unless the node advertised `feature` in `Welcome.features`: an
/// [`io::ErrorKind::Unsupported`] naming the capability, returned BEFORE anything is written, so
/// against an older node nothing goes on the wire after the handshake. A capability check is
/// whole-string equality, so the value-carrying `datahub=<addr>` entry can never satisfy one.
pub(crate) fn require_feature(
    features: &[String],
    feature: &str,
    refusal: Refusal,
) -> io::Result<()> {
    if features.iter().any(|f| f == feature) {
        return Ok(());
    }
    let what = match refusal {
        Refusal::OlderNode(what) => format!(" (an older vike-tradehub) — {what}"),
        Refusal::Plain(what) => format!(" — {what}"),
    };
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "this node does not advertise the \"{feature}\" capability{what} refused client-side, \
             nothing was sent"
        ),
    ))
}

/// The words a one-shot verb's reply errors use, so every verb maps a wrong reply ONE way:
/// `tradehub {name} {error_word}: {msg}` for [`Response::Error`] and
/// `tradehub {name}: expected {expected}, got {kind}` for any other variant, both
/// [`io::ErrorKind::InvalidData`] (the node ANSWERED, which an exit ladder keys on). With
/// `auth_denied`, a [`Response::AuthDenied`] is instead [`io::ErrorKind::PermissionDenied`]
/// `tradehub {name} denied: {reason}` (the write-scope verbs).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplyWords {
    /// The verb's name in every message (`"strategy status"`).
    pub(crate) name: &'static str,
    /// The [`Response`] variant name the verb expects (`"StrategyStatus"`).
    pub(crate) expected: &'static str,
    /// The word after `name` for a node's [`Response::Error`]: `"error"`, or `"refused"`.
    pub(crate) error_word: &'static str,
    /// Map [`Response::AuthDenied`] to [`io::ErrorKind::PermissionDenied`].
    pub(crate) auth_denied: bool,
}

impl ReplyWords {
    /// A read verb's words: `error` for a node error, no `AuthDenied` arm.
    pub(crate) const fn read(name: &'static str, expected: &'static str) -> Self {
        ReplyWords { name, expected, error_word: "error", auth_denied: false }
    }

    /// The error for a reply that is not the verb's `expected` variant.
    pub(crate) fn mismatch(self, reply: Response) -> io::Error {
        let ReplyWords { name, expected, error_word, auth_denied } = self;
        match reply {
            Response::AuthDenied { reason } if auth_denied => io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("tradehub {name} denied: {reason}"),
            ),
            Response::Error(msg) => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub {name} {error_word}: {msg}"),
            ),
            other => io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub {name}: expected {expected}, got {}", resp_kind(&other)),
            ),
        }
    }
}

/// The variant NAME of a response (no payload), so an unexpected reply is reported without
/// stringifying a possibly large snapshot payload.
pub(crate) fn resp_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthOk { .. } => "AuthOk",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::SnapshotFrame(_) => "SnapshotFrame",
        Response::Ack { .. } => "Ack",
        Response::Preview { .. } => "Preview",
        Response::Error(_) => "Error",
        Response::Pong => "Pong",
        Response::StrategyStatus(_) => "StrategyStatus",
        Response::SettingsShow(_) => "SettingsShow",
        Response::Directory(_) => "Directory",
        Response::SettingsWritten { .. } => "SettingsWritten",
        Response::Tearsheet(_) => "Tearsheet",
        Response::AccountList(_) => "AccountList",
        Response::AccountWritten(_) => "AccountWritten",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use std::net::{SocketAddr, TcpListener};
    use std::thread;
    use std::time::{Duration, Instant};

    /// The one deterministic challenge nonce the scripted server issues (production mints a fresh
    /// CSPRNG nonce per connection; freshness is proven in `tests/handshake.rs`).
    const NONCE: [u8; 32] = [7u8; 32];

    /// A one-connection scripted node: `Welcome` with `version` + [`NONCE`], then verify the
    /// client's mac against `key` and answer `AuthOk`/`AuthDenied`. Exits quietly when the client
    /// hangs up early (the version-mismatch path).
    fn scripted_server(key: &'static [u8], version: u32) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Hello { .. }) => {}
                other => panic!("expected Hello, got {other:?}"),
            }
            write_frame(
                &mut stream,
                &Response::Welcome { proto_version: version, nonce: NONCE, features: vec![] },
            )
            .expect("write Welcome");
            let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
                return; // client bailed before Auth (e.g. on the version guard)
            };
            let verdict = if auth::verify(key, &NONCE, version, scope, &mac) {
                Response::AuthOk { scope }
            } else {
                Response::AuthDenied { reason: "bad mac".into() }
            };
            write_frame(&mut stream, &verdict).expect("write verdict");
        });
        addr
    }

    /// The ONE shared sequence authenticates under BOTH scopes.
    #[test]
    fn handshake_succeeds_for_both_scopes() {
        for scope in [Scope::Read, Scope::Write] {
            let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
            let result = node_handshake(addr, b"the-key", scope);
            assert!(result.is_ok(), "{scope:?} handshake failed: {:?}", result.err());
        }
    }

    /// The AUTHED stream carries `TCP_NODELAY`, so a control command written while the previous
    /// one is unacknowledged is not held for the node's delayed ACK.
    #[test]
    fn the_authed_stream_has_nagle_off() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let (stream, _) = node_handshake(addr, b"the-key", Scope::Write).expect("authenticate");
        assert!(stream.nodelay().expect("read TCP_NODELAY"), "the dialled stream has Nagle on");
    }

    /// A wrong key surfaces the server's `AuthDenied` as `PermissionDenied`, naming the scope.
    #[test]
    fn wrong_key_is_permission_denied() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let err = node_handshake(addr, b"wrong-key", Scope::Write).expect_err("must deny");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("control"), "message names the scope: {err}");
    }

    /// **THE HALF-OPEN TUNNEL.** A peer that ACCEPTS and then says nothing (`ssh -L` once the far
    /// end is gone) must fail the connect on the reply deadline, not block forever. Driven through
    /// the seam at 200 ms; asserts the elapsed time too, since the error alone would pass on a
    /// build without the deadline whose peer happened to close.
    #[test]
    fn a_peer_that_accepts_and_never_answers_fails_on_the_reply_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // Hold the accepted socket OPEN and silent: dropping it would send a FIN, a different path.
        let held = thread::spawn(move || {
            let (sock, _) = listener.accept().expect("accept");
            thread::sleep(Duration::from_secs(2));
            drop(sock);
        });

        let started = Instant::now();
        let err = node_handshake_with_deadline(
            addr,
            b"the-key",
            Scope::Write,
            Duration::from_millis(200),
        )
        .expect_err("a silent peer must not authenticate");
        let elapsed = started.elapsed();
        assert_matches!(
            err.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut,
            "the failure must be the read deadline, not something else: {err} ({:?})",
            err.kind()
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "it must give up on the deadline, not later: {elapsed:?}"
        );
        let _ = held.join();
    }

    /// A protocol-version skew fails BEFORE auth (no `Auth` frame is ever sent) with `InvalidData`
    /// naming both versions.
    #[test]
    fn version_mismatch_is_invalid_data() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION + 1);
        let err = node_handshake(addr, b"the-key", Scope::Read).expect_err("must reject skew");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains(&NODE_PROTO_VERSION.to_string()), "names the client version: {msg}");
        assert!(
            msg.contains(&(NODE_PROTO_VERSION + 1).to_string()),
            "names the server version: {msg}"
        );
    }
}
