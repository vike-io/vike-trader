//! The ONE node-client handshake — Hello → Welcome (version guard) → sign → Auth → AuthOk —
//! shared by every client connection path to a tradehub node.
//!
//! The sequence is identical for observe and control; only the [`Scope`] (and therefore the signing
//! key) differs, so it lives in exactly one place: [`node_handshake`]. The observe handle
//! (`crate::remote_handle::RemoteCoreHandle::connect`) runs it under [`Scope::Observe`] and then
//! sends its `Subscribe`; the control paths (`crate::remote_control::RemoteControlHandle::connect`
//! and `crate::remote_control::preview_command`) run it under [`Scope::Control`] and stay in the
//! request/response loop. Crate-private: the public surface stays the handles — this module is an
//! implementation seam, not API.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};

use crate::auth;
use crate::liveness::HANDSHAKE_REPLY_TIMEOUT;
use crate::proto::{NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame};

/// Connect to a node at `addr` and run the full handshake under `scope`, signing the server's
/// challenge nonce with `key` (the raw bytes of the scope's `.env` key). Returns the AUTHED stream
/// with nothing further sent — the caller decides what the connection becomes (the observe handle
/// sends its `Subscribe`; control stays request/response) — PLUS the server's advertised
/// `Welcome.features` (the forward-compat capability strings, e.g.
/// [`crate::proto::FEATURE_STRATEGY_VERBS`]), so a caller can refuse an unadvertised verb
/// CLIENT-SIDE instead of sending a frame an older server cannot decode.
///
/// Send [`Request::Hello`], read the [`Response::Welcome`] challenge nonce (failing — naming both
/// versions — on a protocol-version mismatch), sign it with [`auth::sign`] under `scope`, send
/// [`Request::Auth`], and require [`Response::AuthOk`] echoing that scope. A
/// [`Response::AuthDenied`] (wrong/absent key, or the server refusing the scope) surfaces as
/// [`io::ErrorKind::PermissionDenied`] carrying the server's reason; every other desync is
/// [`io::ErrorKind::InvalidData`].
///
/// # The handshake is READ-DEADLINED; the session that follows is not
///
/// Both reads here run under [`HANDSHAKE_REPLY_TIMEOUT`], and the deadline is CLEARED before the
/// authed stream is handed back — the session's policy belongs to the caller (the observe handle
/// arms [`crate::liveness::OBSERVE_READ_TIMEOUT`] when the node heartbeats; the control worker and
/// the per-call request/response verbs keep the blocking read they have always had), so nothing
/// downstream changes behaviour by inheriting a bound written for the handshake.
///
/// The deadline exists because `TcpStream::connect` succeeding proves nothing about the far end
/// when a TUNNEL is in the way: `ssh -L` accepts on a LOCAL socket, so a connect to a forwarded
/// port completes long after the far side is gone, and this `Welcome` read then blocked FOREVER.
/// In `vike-cli mcp` — single-threaded — that blocked the whole MCP server with it, so an agent
/// asking for anything at all got nothing back.
pub(crate) fn node_handshake<A: ToSocketAddrs>(
    addr: A,
    key: &[u8],
    scope: Scope,
) -> io::Result<(TcpStream, Vec<String>)> {
    node_handshake_with_deadline(addr, key, scope, HANDSHAKE_REPLY_TIMEOUT)
}

/// [`node_handshake`] with the reply deadline as a PARAMETER — the seam the unit tests below drive,
/// so the half-open-tunnel property is proven against a scripted server in milliseconds instead of
/// [`HANDSHAKE_REPLY_TIMEOUT`]'s ten seconds. Crate-private and single-caller in production
/// ([`node_handshake`], which passes the constant): the deadline is a protocol fact, not a knob,
/// and nothing outside this module may choose it.
fn node_handshake_with_deadline<A: ToSocketAddrs>(
    addr: A,
    key: &[u8],
    scope: Scope,
    reply_deadline: std::time::Duration,
) -> io::Result<(TcpStream, Vec<String>)> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(reply_deadline))?;

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
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("tradehub {} auth denied: {reason}", scope_name(scope)),
            ));
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tradehub auth: expected AuthOk, got {}", resp_kind(&other)),
            ));
        }
    }

    // Hand the stream back with NO read policy: the handshake's deadline was for the handshake.
    // Each caller sets its own (see this function's doc), and a stream that silently inherited a
    // 10 s bound would turn an idle control connection into a dead one — the very defect the node
    // side of this change removes.
    stream.set_read_timeout(None)?;
    Ok((stream, features))
}

/// The variant NAME of a response (no payload) — for handshake-desync error messages, so an
/// unexpected reply is reported without stringifying a whole (possibly large) snapshot payload.
/// Shared with the post-handshake loops (the control worker's Ack reads, `preview_command`).
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
        Response::SettingsWritten { .. } => "SettingsWritten",
        Response::Tearsheet(_) => "Tearsheet",
    }
}

/// The lowercase scope name for error messages ("tradehub control auth denied: …"), so a denial
/// names WHICH connection path was refused without leaning on the `Debug` casing.
fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Observe => "observe",
        Scope::Control => "control",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{SocketAddr, TcpListener};
    use std::thread;
    use std::time::{Duration, Instant};

    /// The one deterministic challenge nonce the scripted server issues (production mints a fresh
    /// CSPRNG nonce per connection; freshness is proven in `tests/handshake.rs`).
    const NONCE: [u8; 32] = [7u8; 32];

    /// Spawn a one-connection scripted node on loopback: answer `Hello` with a `Welcome` carrying
    /// `version` + [`NONCE`], then verify the client's `Auth` mac against `key` for whatever scope
    /// the client claimed and answer `AuthOk`/`AuthDenied`. Returns the bound address. The thread
    /// exits quietly when the client hangs up early (the version-mismatch path).
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

    /// The ONE shared sequence authenticates under BOTH scopes — the only inter-caller difference
    /// is the scope (and key) passed in.
    #[test]
    fn handshake_succeeds_for_both_scopes() {
        for scope in [Scope::Observe, Scope::Control] {
            let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
            let result = node_handshake(addr, b"the-key", scope);
            assert!(result.is_ok(), "{scope:?} handshake failed: {:?}", result.err());
        }
    }

    /// A wrong key surfaces the server's `AuthDenied` as `PermissionDenied`, naming the scope.
    #[test]
    fn wrong_key_is_permission_denied() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let err = node_handshake(addr, b"wrong-key", Scope::Control).expect_err("must deny");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("control"), "message names the scope: {err}");
    }

    /// **THE HALF-OPEN TUNNEL.** A peer that ACCEPTS and then says nothing — what `ssh -L` looks
    /// like from this side once the far end is gone, since the forwarded port is a local socket
    /// that keeps accepting — must fail the connect on the reply deadline rather than blocking
    /// forever. Before the deadline existed this read never returned, and in `vike-cli mcp` it
    /// held the single-threaded MCP server with it.
    ///
    /// Driven through the deadline SEAM at 200 ms; production reads
    /// [`crate::liveness::HANDSHAKE_REPLY_TIMEOUT`] (10 s, argued there). The assertion is on the
    /// error and on the elapsed time — a test that only checked the error would pass on a build
    /// where the deadline was removed but the peer happened to close.
    #[test]
    fn a_peer_that_accepts_and_never_answers_fails_on_the_reply_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        // Hold the accepted socket OPEN and silent for the duration of the test: dropping it would
        // send a FIN, which is the ordinary reported-close path and NOT the failure under test.
        let held = thread::spawn(move || {
            let (sock, _) = listener.accept().expect("accept");
            thread::sleep(Duration::from_secs(2));
            drop(sock);
        });

        let started = Instant::now();
        let err = node_handshake_with_deadline(
            addr,
            b"the-key",
            Scope::Control,
            Duration::from_millis(200),
        )
        .expect_err("a silent peer must not authenticate");
        let elapsed = started.elapsed();
        assert!(
            matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
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
        let err = node_handshake(addr, b"the-key", Scope::Observe).expect_err("must reject skew");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains(&NODE_PROTO_VERSION.to_string()), "names the client version: {msg}");
        assert!(
            msg.contains(&(NODE_PROTO_VERSION + 1).to_string()),
            "names the server version: {msg}"
        );
    }
}
