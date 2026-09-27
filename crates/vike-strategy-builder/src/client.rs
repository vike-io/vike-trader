//! The CLIENT half of `builder.rs`'s wire protocol — the one call that asks this service for a
//! build and waits for its answer.
//!
//! # Why this lives here and not in the caller
//!
//! `builder.rs` owns the server's whole understanding of the exchange (`Hello` → `Welcome{nonce}`
//! → `Auth{scope, mac}` → `AuthOk` → `Build` → `BuildOk`/`BuildErr`) plus [`DOMAIN`],
//! [`PROTO_VERSION`] and [`required_scope`]. A client written anywhere else would be a SECOND
//! statement of that exchange, kept in step by nothing — the failure
//! `crates/vike-datahub-client/src/client.rs` avoids by living in the same crate as
//! `crates/vike-datahub-client/src/proto.rs`. So the request/response types are `pub` and this
//! module is the only thing that speaks them from the outside; [`build_remote`] signs under
//! `builder::DOMAIN` and presents `builder::required_scope()` by CALLING them rather than
//! restating either, so a change to the service's own auth cannot leave a client behind.
//!
//! # The timeout shape, and why the BUILD read is unbounded
//!
//! Mirrors `vike_datahub_client`'s client exactly, for its reasons: [`CONNECT_TIMEOUT`] and
//! [`HANDSHAKE_DEADLINE`] bound the two legs a dead host actually stalls on (the TCP connect and
//! the pre-auth exchange), and the read timeout is CLEARED before the `Build` frame goes out. The
//! clearing is load-bearing here in a way it is not even for the datahub: this service answers a
//! `Build` by RUNNING CARGO, so the legitimate wait is a compile — minutes on a cold dependency
//! tree — and any ceiling put here would eventually abort a healthy build and report it as a
//! network fault. What the caller gets instead of a timeout is a worker thread (see
//! `crates/vike-studio/src/plugin_build.rs`), which is where a wait this long belongs.
//!
//! # `toolchain_fp` is a PARAMETER, and today every caller has the same honest answer: nothing
//!
//! [`Request::Build`]'s own doc records that the field rides the wire and is CHECKED against
//! nothing. It is a parameter here rather than a value this module invents, because the value that
//! would eventually be correct is the BACKTEST SERVER's fingerprint — the host that will `dlopen`
//! the artifact — and this client runs on the operator's PC under a different toolchain, profile
//! and target. Sending this process's own `vike_strategy_plugin::fingerprint::FINGERPRINT` would
//! therefore be a confident wrong answer the day the check lands, which is worse than an empty one.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use vike_datahub_client::proto::{read_frame, write_frame};
use vike_node_proto::auth::{self, NodeKeys};

use crate::builder::{self, PROTO_VERSION, Request, Response};

/// Bound on the TCP connect, PER RESOLVED ADDRESS — the same value and the same per-address rule
/// `vike_datahub_client`'s `connect_bounded` uses, and for its reason: a name can resolve to both
/// an A and an AAAA record, and a client that tried only the first would fail against a dual-stack
/// host whose IPv6 route is dark.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Read/write bound for the `Hello`/`Welcome`/`Auth`/`AuthOk` legs only. Cleared (read) and
/// widened (write) before the `Build` frame — see this module's header.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// Write bound once the handshake is over. A `Build` body is a source file — kilobytes — so a
/// value large enough to matter is one that only ever fires on a peer that stopped reading.
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// Why a build request did not come back with a sha.
///
/// The variants are separated by WHOSE problem each one is, because the Studio renders them
/// differently: [`BuildRequestError::Compile`] is the user's own source and belongs in front of
/// them verbatim, while the other three are the operator's deployment.
#[derive(Debug, Clone)]
pub enum BuildRequestError {
    /// The socket could not be opened, or the address resolved to nothing.
    Connect(String),
    /// The peer answered, but not with what this protocol says comes next — a version skew, a
    /// truncated frame, a different service on that port.
    Protocol(String),
    /// The service refused this connection's `Auth`: a missing key, the wrong key, or a key minted
    /// for a sibling service's domain.
    AuthDenied(String),
    /// `cargo` ran and rustc reported a failure. Carries the service's `BuildErr` diagnostics
    /// VERBATIM — this is the one variant the user's own source produces.
    Compile(String),
}

impl std::fmt::Display for BuildRequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildRequestError::Connect(e) => {
                write!(f, "connect to the strategy builder failed: {e}")
            }
            BuildRequestError::Protocol(e) => write!(f, "strategy-builder protocol error: {e}"),
            BuildRequestError::AuthDenied(e) => write!(
                f,
                "the strategy builder refused this connection: {e}. It authenticates with \
                 {} under its OWN domain separator — a datahub or tradehub node key cannot \
                 stand in for it.",
                builder::BUILDER_KEY_ENV
            ),
            BuildRequestError::Compile(d) => write!(f, "{d}"),
        }
    }
}

impl std::error::Error for BuildRequestError {}

/// The service's default address — `127.0.0.1` plus [`builder::DEFAULT_PORT`], both read from
/// [`builder::bind_addr`] rather than spelled again here, so the client and the service cannot
/// disagree about where this protocol lives.
#[must_use]
pub fn default_addr() -> String {
    builder::bind_addr(builder::DEFAULT_PORT).to_string()
}

/// Ask the builder at `addr` to compile `source` as the strategy called `name`, and return the
/// sha256 that names the artifact it produced.
///
/// **BLOCKING, and legitimately for minutes** — the answer is on the far side of a `cargo build`.
/// Call it from a worker thread, never from a frame.
///
/// Performs the service's whole handshake, presenting [`builder::required_scope`] (the only scope
/// this protocol grants a key for) signed under [`builder::DOMAIN`] and the connection's own
/// nonce, so the tag cannot be replayed onto another connection or against a sibling service.
///
/// `Ok(sha)` is the design's Flow step 3/4 answer, and it is the whole of what travels onward: the
/// artifact stays on the builder's box and the caller sends the SHA to the backtest server, never
/// a file (the design's `Studio ──sha──> backtest server` diagram).
pub fn build_remote(
    addr: &str,
    keys: &NodeKeys,
    name: &str,
    source: &str,
    toolchain_fp: &str,
) -> Result<String, BuildRequestError> {
    let mut stream = connect_bounded(addr).map_err(|e| {
        BuildRequestError::Connect(format!(
            "{addr}: {e} — the service is `vike-strategy-builder` (default {})",
            default_addr()
        ))
    })?;

    let nonce = handshake(&mut stream)?;

    let scope = builder::required_scope();
    let mac = auth::sign(builder::DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    write_frame(&mut stream, &Request::Auth { scope, mac }).map_err(proto)?;
    match read_frame::<_, Response>(&mut stream).map_err(proto)? {
        Response::AuthOk => {}
        Response::AuthDenied { reason } => return Err(BuildRequestError::AuthDenied(reason)),
        other => {
            return Err(BuildRequestError::Protocol(format!(
                "expected AuthOk/AuthDenied, got {}",
                response_kind(&other)
            )));
        }
    }

    // The handshake is over: stop bounding the read, because what follows is a compile. See the
    // module header for why a ceiling here would abort healthy builds.
    arm_request_timeouts(&stream).map_err(proto)?;

    write_frame(
        &mut stream,
        &Request::Build {
            name: name.to_string(),
            source: source.to_string(),
            toolchain_fp: toolchain_fp.to_string(),
        },
    )
    .map_err(proto)?;

    match read_frame::<_, Response>(&mut stream).map_err(proto)? {
        Response::BuildOk { sha } => Ok(sha),
        Response::BuildErr { diagnostics } => Err(BuildRequestError::Compile(diagnostics)),
        other => Err(BuildRequestError::Protocol(format!(
            "expected BuildOk/BuildErr, got {}",
            response_kind(&other)
        ))),
    }
}

/// `Hello` → `Welcome`, returning the connection's nonce. A protocol-version skew fails HERE,
/// naming BOTH numbers, rather than desyncing on the first frame that does not decode.
fn handshake<S: Read + Write>(stream: &mut S) -> Result<[u8; 32], BuildRequestError> {
    write_frame(stream, &Request::Hello { proto_version: PROTO_VERSION }).map_err(proto)?;
    match read_frame::<_, Response>(stream).map_err(proto)? {
        Response::Welcome { proto_version, nonce } => {
            if proto_version != PROTO_VERSION {
                return Err(BuildRequestError::Protocol(format!(
                    "protocol version mismatch: this client speaks {PROTO_VERSION}, the builder \
                     speaks {proto_version} — one of the two binaries is stale"
                )));
            }
            Ok(nonce)
        }
        Response::AuthDenied { reason } => Err(BuildRequestError::AuthDenied(reason)),
        other => Err(BuildRequestError::Protocol(format!(
            "expected Welcome, got {}",
            response_kind(&other)
        ))),
    }
}

fn proto(e: io::Error) -> BuildRequestError {
    BuildRequestError::Protocol(e.to_string())
}

/// A name for a response that arrived out of turn — never its contents, which on this protocol
/// could be a whole rustc diagnostic dump in a message about frame ordering.
fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::AuthOk => "AuthOk",
        Response::BuildOk { .. } => "BuildOk",
        Response::BuildErr { .. } => "BuildErr",
    }
}

/// Walk every resolved address, taking the first that answers within [`CONNECT_TIMEOUT`], and arm
/// [`HANDSHAKE_DEADLINE`] on the stream that did.
fn connect_bounded<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
    let mut last_err: Option<io::Error> = None;
    for candidate in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&candidate, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(HANDSHAKE_DEADLINE))?;
                stream.set_write_timeout(Some(HANDSHAKE_DEADLINE))?;
                return Ok(stream);
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the address resolved to no socket addresses")
    }))
}

/// Clear the read bound and widen the write one, once the handshake has completed.
fn arm_request_timeouts(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(None)?;
    stream.set_write_timeout(Some(REQUEST_WRITE_TIMEOUT))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_node_proto::auth::Scope;

    /// The client presents the scope the SERVICE requires by calling it, so the two cannot drift.
    #[test]
    fn the_client_presents_the_scope_the_service_requires() {
        assert_eq!(builder::required_scope(), Scope::Write);
    }

    /// The default address is the service's own bind address, not a second literal.
    #[test]
    fn the_default_address_is_the_services_own_bind_address() {
        assert_eq!(default_addr(), builder::bind_addr(builder::DEFAULT_PORT).to_string());
        assert!(default_addr().starts_with("127.0.0.1:"), "{}", default_addr());
    }

    /// A mac this client produces is exactly the one [`builder::verify_auth`] accepts — the whole
    /// point of both halves living in one crate. Proved against the REAL verifier rather than by
    /// re-deriving the signature here.
    #[test]
    fn a_mac_this_client_signs_is_one_the_service_verifies() {
        let key = b"a-real-builder-key";
        let keys = NodeKeys::new(Vec::new(), key.to_vec());
        let nonce = [9u8; 32];
        let scope = builder::required_scope();
        let mac = auth::sign(builder::DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
        assert!(builder::verify_auth(&keys, &nonce, PROTO_VERSION, scope, &mac));
    }

    /// A version skew is reported with BOTH numbers and never silently tolerated. Driven over an
    /// in-memory pair of frames rather than a socket: the decision is pure.
    #[test]
    fn a_protocol_version_skew_names_both_numbers() {
        let mut framed = Vec::new();
        write_frame(
            &mut framed,
            &Response::Welcome { proto_version: PROTO_VERSION + 7, nonce: [0u8; 32] },
        )
        .expect("frame the planted Welcome");
        let mut s = Scripted { out: Vec::new(), inbox: std::io::Cursor::new(framed) };
        match handshake(&mut s) {
            Err(BuildRequestError::Protocol(msg)) => {
                assert!(msg.contains(&PROTO_VERSION.to_string()), "{msg}");
                assert!(msg.contains(&(PROTO_VERSION + 7).to_string()), "{msg}");
            }
            other => panic!("expected a protocol error naming both versions, got {other:?}"),
        }
    }

    /// An out-of-turn frame is named by KIND and never rendered — a `BuildErr` arriving where an
    /// `AuthOk` was expected must not put a rustc dump inside a frame-ordering message.
    ///
    /// ⚠ **This test has now been vacuous in TWO different shapes, and the second one is the more
    /// instructive.** Version one asserted `!response_kind(..).contains(diagnostics)`, which could
    /// not fail because `response_kind` returns `&'static str` — vacuous by TYPE. Version two
    /// built the error message itself, inside the test body, by copying [`handshake`]'s own arm:
    /// it exercised the copy, so mutating the REAL arm to render `{other:?}` left it green.
    ///
    /// A test that performs the behaviour cannot witness the behaviour; it witnesses its own copy.
    /// That is the same defect [0082](../../../docs/decisions/0082-the-plugin-mechanism-lands-without-the-feature.md)
    /// records at the scale of a whole join, and it is worth one comment here at its smallest
    /// scale, because at this size it reads like ordinary test setup.
    ///
    /// So this calls [`handshake`] — the production function — over a planted stream, exactly as
    /// the skew test below does. PROVED by mutation: rendering `{other:?}` in place of
    /// `response_kind(&other)` in that arm reddens it.
    #[test]
    fn an_out_of_turn_frame_is_named_by_kind_and_never_rendered() {
        let diagnostics = "error[E0425]: cannot find value `x`".to_string();
        let out_of_turn = Response::BuildErr { diagnostics: diagnostics.clone() };
        assert_eq!(response_kind(&out_of_turn), "BuildErr");

        // The real site: a `BuildErr` where a `Welcome` was expected. `handshake` writes its
        // `Hello` into `out` and reads this planted answer back, so the whole path that BUILDS the
        // message is the production one.
        let mut framed = Vec::new();
        write_frame(&mut framed, &out_of_turn).expect("frame the planted answer");
        let mut s = Scripted { out: Vec::new(), inbox: std::io::Cursor::new(framed) };
        let err = handshake(&mut s).expect_err("the planted frame is out of turn");
        let rendered = err.to_string();
        assert!(rendered.contains("BuildErr"), "it must name the KIND: {rendered}");
        assert!(
            !rendered.contains(&diagnostics),
            "a frame-ordering message must not carry the frame's CONTENTS: {rendered}"
        );
        // ...and the production function genuinely RAN rather than being short-circuited by a
        // read error: it wrote its `Hello` before it read anything. Without this, a `handshake`
        // that failed on the WRITE would produce a `Protocol` error too and the assertions above
        // could pass for a reason that has nothing to do with frame ordering.
        assert!(!s.out.is_empty(), "handshake must send its Hello before reading the answer");
    }

    /// A `Read`/`Write` pair over planted bytes — no socket, no timing. Shared by the two frame
    /// tests, both of which drive the production [`handshake`] over it rather than re-implementing
    /// what it does.
    struct Scripted {
        /// What `handshake` WROTE — its `Hello`. Read by both tests, as the witness that the
        /// production function got as far as its read at all.
        out: Vec<u8>,
        inbox: std::io::Cursor<Vec<u8>>,
    }
    impl Write for Scripted {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.out.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl Read for Scripted {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.inbox.read(b)
        }
    }
}
