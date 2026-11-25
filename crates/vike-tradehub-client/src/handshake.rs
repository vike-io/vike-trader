//! The tradehub client's side of the ONE hub handshake (`vike_hub_protocol::handshake`, decision 0288) —
//! Hello → Welcome (version guard) → `Auth` or a device's `PersonAuth` → AuthOk — shared by every client
//! connection path to a tradehub, the invitation's registration ([`register_device`]), plus the one-shot
//! verbs' shared refusal ([`require_feature`]) and reply-mismatch ([`ReplyWords::mismatch`]) wording.
//!
//! Only the [`Scope`] and the signer differ between callers: the observe handle runs it under
//! [`Scope::Read`] and then subscribes; the control paths run it under [`Scope::Write`]. The answer to the
//! `Welcome` is `vike_hub_protocol::handshake::auth_answer`'s, the one choice every hub client makes.
//!
//! Every failure is a [`HubCallError`], its variant chosen here where the PHASE is known (decision 0301):
//! a dial, a handshake or a socket option that broke before any request is `NotSent` whatever the OS
//! said; the node's `AuthDenied` is `AccessDenied`; a `Welcome` or a reply this client cannot speak to is
//! `Protocol`; a capability the node does not advertise is `NotOffered`; the node's own no is `Refused`
//! or `NotNow`; anything after a request began to leave is `OutcomeUnknown`, a reply that does not
//! decode included.
//!
//! ⚠ The error TEXTS built here are pinned by tests (`tests/*_negotiation.rs`,
//! `tests/strategy_verbs.rs`, and the string table in `tradehub_handle/reads.rs`): the operator reads
//! them, so change one only on purpose.

use std::io;
use std::net::{TcpStream, ToSocketAddrs};

use crate::auth;
use crate::liveness::ONE_SHOT_REPLY_TIMEOUT;
use crate::proto::{NODE_PROTO_VERSION, Request, Response, node_codec, refusal_outcome};
use vike_hub_protocol::access::{DeviceKey, FEATURE_ACCESS_REGISTER, Signer, invite_proof};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_hub_protocol::frame::{connect_bounded, read_frame, write_frame, write_frame_as};
use vike_hub_protocol::handshake::{
    Admitted, HandshakeFrame, HandshakeReply, Registered, Registration, auth_answer, reads_answer,
};
use vike_hub_protocol::limits::CLIENT_HANDSHAKE_DEADLINE;
use vike_utility::errors::FailureOutcome;

/// Connect to `addr` and run the full handshake under `scope`, signing the server's nonce with
/// `key` (a shared key's bytes, or this device's key: [`Signer`]). Returns the AUTHED stream with nothing
/// further sent, plus the server's advertised `Welcome.features`, so a caller can refuse an unadvertised
/// verb CLIENT-SIDE ([`require_feature`]) instead of sending a frame an older server cannot decode, and who
/// the node's access list admitted ([`Admitted`]: `None` for a shared key or a node older than the field).
///
/// A protocol-version mismatch is [`HubCallError::Protocol`] naming both versions; `AuthDenied` is
/// [`HubCallError::AccessDenied`] with the server's reason; a device key against a node that does not
/// advertise `person-auth` is [`HubCallError::NotOffered`], nothing sent; every other desync is
/// [`HubCallError::Protocol`], and a dial or a link that broke is [`HubCallError::NotSent`].
///
/// ⚠ The dial is `vike_hub_protocol::frame::connect_bounded` (a black-holed host costs
/// `vike_hub_protocol::limits::CLIENT_CONNECT_TIMEOUT` per resolved address, not the OS SYN-retry minutes) and both reads run
/// under [`CLIENT_HANDSHAKE_DEADLINE`] (a half-open `ssh -L` tunnel accepts the local
/// connect and then never answers), and the deadline is CLEARED before the stream is handed back:
/// the session's read policy belongs to the caller.
pub(crate) fn hub_handshake<'k, A: ToSocketAddrs>(
    addr: A,
    key: impl Into<Signer<'k>>,
    scope: Scope,
) -> Result<(TcpStream, Vec<String>, Option<Admitted>), HubCallError> {
    hub_handshake_with_deadline(addr, key.into(), scope, CLIENT_HANDSHAKE_DEADLINE)
}

/// [`hub_handshake`] with the reply deadline as a PARAMETER — the seam the unit tests drive in
/// milliseconds. Single-caller in production: the deadline is a protocol fact, not a knob.
fn hub_handshake_with_deadline<A: ToSocketAddrs>(
    addr: A,
    key: Signer<'_>,
    scope: Scope,
    reply_deadline: std::time::Duration,
) -> Result<(TcpStream, Vec<String>, Option<Admitted>), HubCallError> {
    let (mut stream, nonce, server_version, features) = hello(addr, reply_deadline)?;

    // 2. Auth | PersonAuth -> AuthOk / AuthDenied.
    let answer = auth_answer(key, auth::DOMAIN, &nonce, server_version, scope, &features)?;
    write_frame(&mut stream, &answer).map_err(HubCallError::NotSent)?;
    let admitted =
        match read_frame::<_, HandshakeReply>(&mut stream).map_err(handshake_read_failure)? {
            HandshakeReply::AuthOk { scope: granted, admitted } if granted == scope => admitted,
            HandshakeReply::AuthOk { scope: granted, .. } => {
                return Err(HubCallError::Protocol(
                    format!("tradehub auth: server granted unexpected scope {granted:?}").into(),
                ));
            }
            HandshakeReply::AuthDenied { reason } => {
                // The lowercase scope name ("tradehub control auth denied: …") names the refused path.
                return Err(HubCallError::AccessDenied(format!(
                    "tradehub {} auth denied: {reason}",
                    scope.name()
                )));
            }
            other => {
                return Err(HubCallError::Protocol(
                    format!("tradehub auth: expected AuthOk, got {}", other.kind()).into(),
                ));
            }
        };

    // 3. To a node that advertised the refusal frame, the word that this client reads it (no reply comes).
    if let Some(reads) = reads_answer(&features) {
        write_frame(&mut stream, &reads).map_err(HubCallError::NotSent)?;
    }

    // NO read or write policy on the way out: an inherited 10 s bound would kill an idle control
    // connection (read) or a slow-draining peer's next command (write).
    stream.set_read_timeout(None).map_err(HubCallError::NotSent)?;
    stream.set_write_timeout(None).map_err(HubCallError::NotSent)?;
    Ok((stream, features, admitted))
}

/// A handshake reply that could not be read, before any request left: a length over the frame ceiling
/// or a body that is not a hub's frame (the codec's `InvalidData`) is a peer that does not speak this
/// protocol; any other failure is the link's.
fn handshake_read_failure(e: io::Error) -> HubCallError {
    match e.kind() {
        io::ErrorKind::InvalidData => HubCallError::Protocol(e.into()),
        _ => HubCallError::NotSent(e),
    }
}

/// A request's write that failed: the codec's refusal BEFORE the first byte (`InvalidData`: a body
/// over the frame ceiling) sent nothing and is a request this client will not send; any other failure
/// may have sent part of it.
fn request_write_failure(e: io::Error) -> HubCallError {
    match e.kind() {
        io::ErrorKind::InvalidData => HubCallError::Protocol(e.into()),
        _ => HubCallError::OutcomeUnknown(e),
    }
}

/// The handshake's first half: dial, `Hello`, and the `Welcome` (with the protocol-version guard). Returns the
/// stream, the nonce, the node's version and its advertised features.
fn hello<A: ToSocketAddrs>(
    addr: A,
    reply_deadline: std::time::Duration,
) -> Result<(TcpStream, [u8; 32], u32, Vec<String>), HubCallError> {
    // `connect_bounded` also turned Nagle off and armed the handshake bound both ways; the read
    // bound is narrowed to the caller's `reply_deadline` (the test seam), the write bound stays.
    let mut stream = connect_bounded(addr).map_err(HubCallError::NotSent)?;
    stream.set_read_timeout(Some(reply_deadline)).map_err(HubCallError::NotSent)?;

    // 1. Hello -> Welcome{ nonce } (with the protocol-version guard).
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: NODE_PROTO_VERSION })
        .map_err(HubCallError::NotSent)?;
    match read_frame::<_, HandshakeReply>(&mut stream).map_err(handshake_read_failure)? {
        HandshakeReply::Welcome { proto_version, features, nonce } => {
            if proto_version != NODE_PROTO_VERSION {
                return Err(HubCallError::Protocol(
                    format!(
                        "tradehub protocol version mismatch: client speaks \
                         {NODE_PROTO_VERSION}, server speaks {proto_version}"
                    )
                    .into(),
                ));
            }
            // A tradehub is always keyed: a Welcome with no nonce is not one.
            let nonce = nonce.ok_or_else(|| {
                HubCallError::Protocol("tradehub handshake: the Welcome carried no nonce".into())
            })?;
            Ok((stream, nonce, proto_version, features))
        }
        other => Err(HubCallError::Protocol(
            format!("tradehub handshake: expected Welcome, got {}", other.kind()).into(),
        )),
    }
}

/// **Register this device with an invitation** (decision 0288 item 7.3), over the guest's tunnel: `Hello`, then
/// `Register` in `Auth`'s place, signed with the invitation's `secret` over this connection's nonce (the secret
/// never crosses the wire). `invite` and `secret` are the invitation's third and fourth fields, `device` the name
/// this app gives itself, `device_key` the key it just made (only its PUBLIC half is sent), `ssh_key` its new
/// permanent `ssh-ed25519 <base64>` public key. The node closes the connection after its answer; the device then
/// dials with `PersonAuth` like any other.
///
/// # Errors
/// [`HubCallError::NotOffered`] when the node does not advertise `access-register` (nothing sent);
/// [`HubCallError::AccessDenied`] with the node's reason when it refuses; [`HubCallError::Protocol`] for
/// a reply that is not a registration's; [`HubCallError::OutcomeUnknown`] when the link broke after the
/// `Register` began to leave; the handshake's own.
pub fn register_device<A: ToSocketAddrs>(
    addr: A,
    invite: &str,
    secret: &str,
    device: &str,
    device_key: &DeviceKey,
    ssh_key: &str,
) -> Result<Registered, HubCallError> {
    let (mut stream, nonce, server_version, features) = hello(addr, CLIENT_HANDSHAKE_DEADLINE)?;
    require_feature(&features, FEATURE_ACCESS_REGISTER, Refusal::OlderHub("the registration is"))?;
    let registration = Registration {
        invite: invite.to_string(),
        proof: invite_proof(secret, &nonce, server_version),
        device: device.to_string(),
        public_key: device_key.public_key().to_vec(),
        ssh_key: ssh_key.to_string(),
    };
    write_frame(&mut stream, &HandshakeFrame::Register(registration))
        .map_err(request_write_failure)?;
    match read_frame::<_, HandshakeReply>(&mut stream).map_err(HubCallError::OutcomeUnknown)? {
        HandshakeReply::Registered(registered) => Ok(registered),
        HandshakeReply::AuthDenied { reason } => {
            Err(HubCallError::AccessDenied(format!("tradehub registration refused: {reason}")))
        }
        other => Err(HubCallError::Protocol(
            format!("tradehub registration: expected Registered, got {}", other.kind()).into(),
        )),
    }
}

/// [`hub_handshake`] for a ONE-SHOT verb: the authed stream comes back with
/// [`ONE_SHOT_REPLY_TIMEOUT`] armed as its read bound, so the one reply the verb reads cannot park
/// its caller on a node that went silent after the handshake. The per-call verbs of
/// `tradehub_handle::reads` and `tradehub_control` open their connection through this; the
/// long-lived handles, which own their read policy, keep [`hub_handshake`].
pub(crate) fn one_shot_handshake<'k, A: ToSocketAddrs>(
    addr: A,
    key: impl Into<Signer<'k>>,
    scope: Scope,
) -> Result<(TcpStream, Vec<String>), HubCallError> {
    let (stream, features, _) = hub_handshake(addr, key, scope)?;
    stream.set_read_timeout(Some(ONE_SHOT_REPLY_TIMEOUT)).map_err(HubCallError::NotSent)?;
    Ok((stream, features))
}

/// Send ONE `request` on an authed stream in the form the node's `features` advertise
/// ([`crate::proto::node_codec`]: msgpack to a node of this build, JSON to one before it) and read its
/// reply, in whichever form it came: every one-shot verb's exchange. A write the codec refused before
/// its first byte is [`HubCallError::Protocol`] (nothing left); any later failure, a reply that does
/// not decode included, is [`HubCallError::OutcomeUnknown`]: the request may have reached the node.
pub(crate) fn ask(
    stream: &mut TcpStream,
    features: &[String],
    request: &Request,
) -> Result<Response, HubCallError> {
    write_frame_as(stream, node_codec(features), request).map_err(request_write_failure)?;
    read_frame(stream).map_err(HubCallError::OutcomeUnknown)
}

// How a client-side capability refusal names the verb it withheld — the words between
/// `capability` and `refused client-side, nothing was sent` in [`require_feature`]'s message.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Refusal {
    /// `… capability (an older vike-tradehub) — {what} refused client-side, nothing was sent`.
    OlderHub(&'static str),
}

/// Refuse a verb CLIENT-SIDE unless the node advertised `feature` in `Welcome.features`: a
/// [`HubCallError::NotOffered`] naming the capability, returned BEFORE anything is written, so
/// against an older node nothing goes on the wire after the handshake. A capability check is
/// whole-string equality, so the value-carrying `datahub=<addr>` entry can never satisfy one.
pub(crate) fn require_feature(
    features: &[String],
    feature: &str,
    refusal: Refusal,
) -> Result<(), HubCallError> {
    if features.iter().any(|f| f == feature) {
        return Ok(());
    }
    let Refusal::OlderHub(what) = refusal;
    let what = format!(" (an older vike-tradehub) — {what}");
    Err(HubCallError::NotOffered {
        feature: feature.to_string(),
        text: format!(
            "this node does not advertise the \"{feature}\" capability{what} refused client-side, \
             nothing was sent"
        ),
    })
}

/// The words a one-shot verb's reply errors use, so every verb maps a wrong reply ONE way:
/// `tradehub {name} {error_word}: {msg}` for the node's answer ([`Response::Error`],
/// [`Response::Refused`]), which is [`HubCallError::Refused`], or [`HubCallError::NotNow`] for a node
/// still starting or a busy core (never a final no); `tradehub {name}: expected {expected}, got {kind}`
/// for any other variant ([`HubCallError::Protocol`]). With `auth_denied`, a [`Response::AuthDenied`] is
/// instead [`HubCallError::AccessDenied`] `tradehub {name} denied: {reason}` (the write-scope verbs).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReplyWords {
    /// The verb's name in every message (`"strategy status"`).
    pub(crate) name: &'static str,
    /// The [`Response`] variant name the verb expects (`"StrategyStatus"`).
    pub(crate) expected: &'static str,
    /// The word after `name` for a node's [`Response::Error`]: `"error"`, or `"refused"`.
    pub(crate) error_word: &'static str,
    /// Map [`Response::AuthDenied`] to [`HubCallError::AccessDenied`].
    pub(crate) auth_denied: bool,
}

impl ReplyWords {
    /// A read verb's words: `error` for a node error, no `AuthDenied` arm.
    pub(crate) const fn read(name: &'static str, expected: &'static str) -> Self {
        ReplyWords { name, expected, error_word: "error", auth_denied: false }
    }

    /// The error for a reply that is not the verb's `expected` variant.
    pub(crate) fn mismatch(self, reply: Response) -> HubCallError {
        let ReplyWords { name, expected, error_word, auth_denied } = self;
        match reply {
            Response::AuthDenied { reason } if auth_denied => {
                HubCallError::AccessDenied(format!("tradehub {name} denied: {reason}"))
            }
            // An older node's words: a node still starting or a busy core is NOT NOW, never a final no.
            Response::Error(msg) => node_answer(
                refusal_outcome(&msg),
                None,
                format!("tradehub {name} {error_word}: {msg}"),
            ),
            // The refusal frame: the same words, its outcome read off its id (a node of this build).
            Response::Refused(refusal) => node_answer(
                crate::refusals::frame_outcome(&refusal),
                Some(refusal.id),
                format!("tradehub {name} {error_word}: {}", refusal.text),
            ),
            other => HubCallError::Protocol(
                format!("tradehub {name}: expected {expected}, got {}", response_kind(&other))
                    .into(),
            ),
        }
    }
}

/// The node's no to a verb, as `outcome` reads it: NOT NOW, or its final refusal.
fn node_answer(outcome: FailureOutcome, id: Option<String>, text: String) -> HubCallError {
    match outcome {
        FailureOutcome::NotNow => HubCallError::NotNow { id, text },
        _ => HubCallError::Refused { id, text },
    }
}

/// The variant NAME of a response (no payload), so an unexpected reply is reported without
/// stringifying a possibly large snapshot payload.
pub(crate) fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::AuthDenied { .. } => "AuthDenied",
        Response::SnapshotFrame(_) => "SnapshotFrame",
        Response::SnapshotPatch(_) => "SnapshotPatch",
        Response::Ack { .. } => "Ack",
        Response::Preview { .. } => "Preview",
        Response::Error(_) => "Error",
        Response::Pong => "Pong",
        Response::StrategyStatus(_) => "StrategyStatus",
        Response::SettingsShow(_) => "SettingsShow",
        Response::Directory(_) => "Directory",
        Response::SettingsWritten { .. } => "SettingsWritten",
        Response::AccountList(_) => "AccountList",
        Response::AccountWritten(_) => "AccountWritten",
        Response::CredentialsHeld(_) => "CredentialsHeld",
        Response::VenueRead(_) => "VenueRead",
        Response::Refused(_) => "Refused",
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
            match read_frame::<_, HandshakeFrame>(&mut stream) {
                Ok(HandshakeFrame::Hello { .. }) => {}
                other => panic!("expected Hello, got {other:?}"),
            }
            write_frame(
                &mut stream,
                &HandshakeReply::Welcome {
                    proto_version: version,
                    features: vec![],
                    nonce: Some(NONCE),
                },
            )
            .expect("write Welcome");
            let Ok(HandshakeFrame::Auth { scope, mac }) =
                read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                return; // client bailed before Auth (e.g. on the version guard)
            };
            let verdict = if auth::verify(key, &NONCE, version, scope, &mac) {
                HandshakeReply::AuthOk { scope, admitted: None }
            } else {
                HandshakeReply::AuthDenied { reason: "bad mac".into() }
            };
            write_frame(&mut stream, &verdict).expect("write verdict");
        });
        addr
    }

    /// A device key never signs for a node that does not advertise `person-auth`: refused before any
    /// answer is written (decision 0288).
    #[test]
    fn a_device_key_against_a_node_without_person_auth_sends_nothing() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let (device, _) = DeviceKey::generate().expect("a device key");
        let err = hub_handshake(addr, &device, Scope::Read).expect_err("no person-auth advertised");
        assert_matches!(
            &err,
            HubCallError::NotOffered { feature, .. }
                if feature == vike_hub_protocol::access::FEATURE_PERSON_AUTH,
            "{err}"
        );
    }

    /// The ONE shared sequence authenticates under BOTH scopes.
    #[test]
    fn handshake_succeeds_for_both_scopes() {
        for scope in [Scope::Read, Scope::Write] {
            let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
            let result = hub_handshake(addr, b"the-key", scope);
            assert!(result.is_ok(), "{scope:?} handshake failed: {:?}", result.err());
        }
    }

    /// The handshake hands back NO read bound (a long-lived handle owns its policy), and the
    /// one-shot variant hands back [`ONE_SHOT_REPLY_TIMEOUT`], so a per-call verb's one reply read
    /// cannot park its caller on a node that went silent after `AuthOk`.
    #[test]
    fn a_one_shot_stream_comes_back_with_its_reply_bound_armed() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let (stream, _, _) = hub_handshake(addr, b"the-key", Scope::Read).expect("authenticate");
        assert_eq!(stream.read_timeout().expect("read_timeout"), None);
        // The dial armed a write bound for the handshake; the session's write policy belongs to the
        // caller, so it must not leak out (the old unbounded connect left none).
        assert_eq!(stream.write_timeout().expect("write_timeout"), None);

        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let (stream, _) = one_shot_handshake(addr, b"the-key", Scope::Read).expect("authenticate");
        assert_eq!(stream.read_timeout().expect("read_timeout"), Some(ONE_SHOT_REPLY_TIMEOUT));
    }

    /// The AUTHED stream carries `TCP_NODELAY`, so a control command written while the previous
    /// one is unacknowledged is not held for the node's delayed ACK.
    #[test]
    fn the_authed_stream_has_nagle_off() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let (stream, _, _) = hub_handshake(addr, b"the-key", Scope::Write).expect("authenticate");
        assert!(stream.nodelay().expect("read TCP_NODELAY"), "the dialled stream has Nagle on");
    }

    /// A wrong key surfaces the server's `AuthDenied` as `AccessDenied`, naming the scope, shown under the client's
    /// one access id.
    #[test]
    fn wrong_key_is_access_denied() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION);
        let err = hub_handshake(addr, b"wrong-key", Scope::Write).expect_err("must deny");
        assert_matches!(&err, HubCallError::AccessDenied(_), "{err}");
        assert_eq!(
            err.to_string(),
            "error [hub.access.denied]: tradehub control auth denied: bad mac"
        );
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
        let err = hub_handshake_with_deadline(
            addr,
            b"the-key".into(),
            Scope::Write,
            Duration::from_millis(200),
        )
        .expect_err("a silent peer must not authenticate");
        let elapsed = started.elapsed();
        assert_matches!(
            &err,
            HubCallError::NotSent(e)
                if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut),
            "the failure must be the read deadline, before any request, not something else: {err:?}"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "it must give up on the deadline, not later: {elapsed:?}"
        );
        let _ = held.join();
    }

    /// A protocol-version skew fails BEFORE auth (no `Auth` frame is ever sent) as `Protocol`
    /// naming both versions.
    #[test]
    fn version_mismatch_is_a_protocol_refusal() {
        let addr = scripted_server(b"the-key", NODE_PROTO_VERSION + 1);
        let err = hub_handshake(addr, b"the-key", Scope::Read).expect_err("must reject skew");
        assert_matches!(&err, HubCallError::Protocol(_), "{err}");
        let msg = err.to_string();
        assert!(msg.contains(&NODE_PROTO_VERSION.to_string()), "names the client version: {msg}");
        assert!(
            msg.contains(&(NODE_PROTO_VERSION + 1).to_string()),
            "names the server version: {msg}"
        );
    }

    /// The P4.5c renames changed the node wire's bytes, so the version stepped 4 -> 5: a node from
    /// before them is refused at the handshake, naming both numbers, never at a decode later.
    #[test]
    fn a_node_from_before_the_phase_four_renames_is_refused_at_the_handshake() {
        assert_eq!(NODE_PROTO_VERSION, 5);
        let addr = scripted_server(b"the-key", 4);
        let err = hub_handshake(addr, b"the-key", Scope::Read).expect_err("a v4 node is refused");
        assert_matches!(&err, HubCallError::Protocol(_), "{err}");
        assert_eq!(
            err.to_string(),
            "tradehub protocol version mismatch: client speaks 5, server speaks 4"
        );
    }

    /// **A node that answers "starting" reads NOT NOW** by its older words, keeping them; any other node
    /// error stays the node's final refusal. Neither carries an id: an older node sends none.
    #[test]
    fn a_starting_nodes_answer_reads_not_now_and_another_error_reads_refused() {
        let words = ReplyWords::read("snapshot", "SnapshotFrame");
        let starting =
            words.mismatch(Response::Error(crate::proto::starting_error("the accounts")));
        assert_matches!(&starting, HubCallError::NotNow { id: None, .. }, "{starting}");
        assert_eq!(starting.to_string(), "tradehub snapshot error: starting: the accounts");
        let refused = words.mismatch(Response::Error("no identity block".into()));
        assert_matches!(&refused, HubCallError::Refused { id: None, .. }, "{refused}");
        assert_eq!(refused.to_string(), "tradehub snapshot error: no identity block");
    }

    /// **The refusal frame is decided by its ID, never its words**: a starting frame reads NOT NOW with the same
    /// text an older node's `Error` gives and keeps its id, and a frame whose id this build does not know reads
    /// as the node's refusal even when its words say "starting".
    #[test]
    fn a_refusal_frame_is_read_by_its_id() {
        use vike_hub_protocol::refusal::HubRefusal;

        let words = ReplyWords::read("snapshot", "SnapshotFrame");
        let starting = words.mismatch(Response::Refused(
            crate::refusals::TradehubRefusal::Starting { progress: "the accounts".into() }.into(),
        ));
        assert_matches!(
            &starting,
            HubCallError::NotNow { id: Some(id), .. } if id == "tradehub.starting",
            "{starting}"
        );
        // The frame's id is shown in front; an older node's `Error` (above) reads as it always did.
        assert_eq!(
            starting.to_string(),
            "error [tradehub.starting]: tradehub snapshot error: starting: the accounts"
        );
        let unknown =
            words.mismatch(Response::Refused(HubRefusal::new("tradehub.newer", "starting: x")));
        assert_matches!(
            &unknown,
            HubCallError::Refused { id: Some(id), .. } if id == "tradehub.newer",
            "{unknown}"
        );
    }

    /// **After the request left, nothing this client cannot read is a refusal** (decision 0303): a reply
    /// that does not decode and a node that hangs up are both OUTCOME UNKNOWN, never "the node said no".
    #[test]
    fn a_reply_that_does_not_decode_or_never_comes_is_outcome_unknown() {
        use vike_hub_protocol::frame::{read_frame_raw, write_frame_raw};

        for garbage in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let addr = listener.local_addr().expect("local addr");
            let node = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                read_frame_raw(&mut stream).expect("the request arrives");
                if garbage {
                    write_frame_raw(&mut stream, b"not a reply").expect("write garbage");
                }
            });
            let mut stream = TcpStream::connect(addr).expect("dial");
            let err = ask(&mut stream, &[], &Request::Snapshot).expect_err("no readable reply");
            assert_matches!(&err, HubCallError::OutcomeUnknown(_), "{err:?}");
            node.join().expect("the scripted node ends");
        }
    }
}
