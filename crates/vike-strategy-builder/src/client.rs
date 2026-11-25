//! The CLIENT half of `builder.rs`'s wire protocol — the one call that asks this service for a
//! build and waits for its answer.
//!
//! # Why this lives here and not in the caller
//!
//! `builder.rs` owns the server's whole understanding of the exchange (`Hello` → `Welcome{nonce}`
//! → `Auth{scope, mac}` → `AuthOk` → `Build` → `BuildOk`/`BuildErr`) plus [`DOMAIN`](crate::builder::DOMAIN),
//! [`PROTO_VERSION`](crate::builder::PROTO_VERSION) and [`required_scope`](crate::builder::required_scope).
//! A client written anywhere else would be a SECOND
//! statement of that exchange, kept in step by nothing — the failure
//! `crates/vike-datahub-client/src/client.rs` avoids by living in the same crate as
//! `crates/vike-datahub-client/src/proto.rs`. So the request/response types are `pub` and this
//! module is the only thing that speaks them from the outside; [`build_remote`](crate::client::build_remote) signs under
//! `builder::DOMAIN` and presents `builder::required_scope()` by CALLING them rather than
//! restating either, so a change to the service's own auth cannot leave a client behind. The codec is
//! the service's too: JSON for the `Hello`/`Welcome` pair, msgpack after it
//! (`vike_hub_protocol::child_pipe::write_message`).
//!
//! # The timeout shape, and why the BUILD read is unbounded
//!
//! Mirrors `vike_datahub_client`'s client exactly, for its reasons: `CLIENT_CONNECT_TIMEOUT` and
//! `CLIENT_HANDSHAKE_DEADLINE` bound the two legs a dead host actually stalls on (the TCP connect and
//! the pre-auth exchange), and the read timeout is CLEARED before the `Build` frame goes out. The
//! clearing is load-bearing here in a way it is not even for the datahub: this service answers a
//! `Build` by RUNNING CARGO, so the legitimate wait is a compile — minutes on a cold dependency
//! tree — and any ceiling put here would eventually abort a healthy build and report it as a
//! network fault. What the caller gets instead of a timeout is a worker thread (the backtesthub's
//! connection thread, and behind it Studio's own worker, `crates/vike-studio/src/backend/backtesthub/build.rs`),
//! which is where a wait this long belongs.
//!
//! # `toolchain_fp` is a PARAMETER, and today every caller has the same honest answer: nothing
//!
//! [`Request::Build`](crate::builder::Request::Build)'s own doc records that the field rides the wire and is CHECKED against
//! nothing. It is a parameter here rather than a value this module invents, because the value that
//! would eventually be correct is the BACKTEST SERVER's fingerprint — the host that will `dlopen`
//! the artifact — and this client runs in a daemon built under its own toolchain, profile and target,
//! not the sandboxed child that loads the artifact. Sending this process's own
//! `vike_rust_strategy::fingerprint::FINGERPRINT` would therefore be a confident wrong answer the day
//! the check lands, which is worse than an empty one.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use vike_hub_protocol::auth::{self, HubKeys};
use vike_hub_protocol::child_pipe::{read_message, write_message};
use vike_hub_protocol::frame::{connect_bounded, read_frame, write_frame};
use vike_hub_protocol::refusal::HubRefusal;

use vike_rust_strategy::sdk::Sdk;
use vike_utility::errors::CarriedRefusal;

use crate::builder::{self, BUILDER_KEY_NAME, PROTO_VERSION, Request, Response};

/// Write bound once the handshake is over. A `Build` body is a source file — kilobytes — so a
/// value large enough to matter is one that only ever fires on a peer that stopped reading.
const REQUEST_WRITE_TIMEOUT: Duration = Duration::from_secs(60);

/// What the builder answered for a build (`Response::BuildOk`, decision 0296): the sha256 naming the
/// artifact (a study's, `crate::render::study_sha`), the numbered SDK it was built against (`None`: SDK
/// dev, a checkout's) and its build folder. [`Built::artifact`] is where it lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub sha: String,
    pub sdk: Option<u32>,
    pub folder: String,
}

impl Built {
    /// The artifact under `builds` (`<builds>/sdk-N/<folder>/<sha>.so`,
    /// `vike_rust_strategy::artifact::artifact_path`); `builds` empty gives it relative, as a parent hands
    /// a sandboxed child (`vike_strategy_sandbox::artifact_of` checks its shape).
    #[must_use]
    pub fn artifact(&self, builds: &Path) -> PathBuf {
        let sdk = self.sdk.map_or(Sdk::Dev, Sdk::Number);
        vike_rust_strategy::artifact::artifact_path(builds, sdk, &self.folder, &self.sha)
    }

    /// **The hub's check of the answer** (decision 0296): `Ok` when this build is of the SDK `wanted`
    /// names (a run's explicit override, else the file's `[strategy] sdk` pin, `crate::render::pin_of`), or
    /// `wanted` names none, or the builder built a development checkout ("SDK dev", which ignores a pin
    /// and says so on its stderr). Otherwise the refusal, naming both: a build of another SDK compiles,
    /// loads and runs that SDK's code, so it is never run in place of the one asked for, whatever builder
    /// answered (one started on another SDK's tree, an older one).
    ///
    /// # Errors
    /// [`BuildRequestError::OtherSdkBuilt`], carried: a carrier hands it on, its id kept.
    pub fn of_the_sdk(&self, wanted: Option<u32>) -> Result<(), CarriedRefusal> {
        match (wanted, self.sdk) {
            (Some(asked), Some(built)) if asked != built => {
                Err(BuildRequestError::OtherSdkBuilt { asked, built }.carried())
            }
            _ => Ok(()),
        }
    }

    /// [`Built::artifact`] relative to the builds directory, `/`-separated whatever the host's
    /// separator: the argument a hub hands its sandboxed child.
    #[must_use]
    pub fn relative_artifact(&self) -> String {
        let path = self.artifact(Path::new(""));
        path.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join("/")
    }
}

vike_utility::refusal_enum! {
    /// Why a build request did not come back with a usable build, each with its id
    /// (`vike_utility::refusal_enum!`, decision 0301).
    ///
    /// The variants are separated by WHOSE problem each one is, because the Studio renders them
    /// differently: [`BuildRequestError::Compile`] is the builder's answer about the user's own source and
    /// belongs in front of them verbatim, while the other three are the operator's deployment.
    /// [`BuildFileError`]'s `Display` is the bare words a hub hands on.
    #[derive(Debug, Clone)]
    pub enum BuildRequestError with ids BuildRequestErrorId {
        /// The socket could not be opened, or the address resolved to nothing.
        Connect { why: String } => "strategy.builder.unreachable":
            "connect to the strategy builder failed: {why}",
        /// The peer answered, but not with what this protocol says comes next — a version skew, a
        /// truncated frame, a different service on that port.
        Protocol { why: String } => "strategy.builder.protocol_mismatch": "strategy-builder protocol error: {why}",
        /// The service refused this connection's `Auth`: a missing key, the wrong key, or a key minted
        /// for a sibling service's domain. `reason` is the service's own word.
        AuthDenied { reason: String } => "strategy.builder.auth_denied":
            "the strategy builder refused this connection: {reason}. It authenticates with \
             the {BUILDER_KEY_NAME} row of this box's settings database (the daemon key the builder reads \
             too) under its OWN domain separator — a datahub or tradehub hub key cannot \
             stand in for it.",
        /// The builder refused the Build (`Response::BuildErr`). Carries its refusal frame VERBATIM — the
        /// compiler's words for the user's own source, or another refusal (`crate::render::BuildError`'s id and
        /// bare words, the builder's answer): this variant only forwards it, so the builder's id shows.
        Compile { refusal: HubRefusal } => carries refusal,
            "strategy.builder.build_refused": "{refusal}",
        /// The builder built another SDK than the one asked for ([`Built::of_the_sdk`]): never run in its place.
        OtherSdkBuilt { asked: u32, built: u32 } => "strategy.builder.other_sdk_built":
            "SDK {asked} was asked for (a run's `sdk`, or the file's `[strategy] sdk`), and the \
             strategy builder built SDK {built}: it is not run in its place. Start the builder on \
             the installed SDKs (`--sdk-root <project>/vendor/vike-strategy-source`), or move the \
             file to SDK {built} (`vike-cli strategy migrate <FILE> --to {built}`)",
    }
}

impl std::error::Error for BuildRequestError {}

/// The service's default address — `127.0.0.1` plus [`builder::DEFAULT_PORT`], both read from
/// [`builder::bind_address`] rather than spelled again here, so the client and the service cannot
/// disagree about where this protocol lives.
#[must_use]
pub fn default_address() -> String {
    builder::bind_address(builder::DEFAULT_PORT).to_string()
}

/// Ask the builder at `addr` to compile the strategy FILE at `path` (relative to the project's
/// `strategies/` folder, `trend/momentum.rs`) whose text is `text`, and return what it built: the sha256
/// of that text, which names the artifact, the SDK and the build folder ([`Built`]).
///
/// **BLOCKING, and legitimately for minutes** — the answer is on the far side of a `cargo build`.
/// Call it from a worker thread, never from a frame.
///
/// Performs the service's whole handshake, presenting [`builder::required_scope`] (the only scope
/// this protocol grants a key for) signed under [`builder::DOMAIN`] and the connection's own
/// nonce, so the tag cannot be replayed onto another connection or against a sibling service.
///
/// `Ok` is the design's Flow step 3/4 answer, and it is the whole of what travels onward: the
/// artifact stays on the builder's box and the caller names it to the sandboxed child by its path
/// ([`Built::artifact`]), never sending a file.
///
/// `sdk` is an explicit SDK override (`Request::Build`'s `sdk`: a backtest profile's `[strategy] sdk`, a
/// Studio's pick); `None` builds as the text says.
pub fn build_remote(
    addr: &str,
    keys: &HubKeys,
    path: &str,
    text: &str,
    sdk: Option<u32>,
    toolchain_fp: &str,
) -> Result<Built, BuildRequestError> {
    let request = Request::Build {
        path: path.to_string(),
        text: text.to_string(),
        sdk,
        toolchain_fp: toolchain_fp.to_string(),
    };
    ask(addr, keys, &request)
}

/// [`build_remote`] for a research STUDY file (owner's rule, 2026-11-15: a study is a FILE, built like
/// a strategy file): the study's folder name and its `.rs` files as `(file name, text)`; what it
/// built, its sha the one its artifact is named by (`crate::render::study_sha`).
pub fn build_study_remote(
    addr: &str,
    keys: &HubKeys,
    name: &str,
    files: &[(String, String)],
) -> Result<Built, BuildRequestError> {
    let request = Request::BuildStudy {
        name: name.to_string(),
        files: files.to_vec(),
        // Unchecked, as `Build`'s (the module doc's last section).
        toolchain_fp: String::new(),
    };
    ask(addr, keys, &request)
}

/// One build request, the whole handshake first; what was built, or the refusal.
fn ask(addr: &str, keys: &HubKeys, request: &Request) -> Result<Built, BuildRequestError> {
    let mut stream = connect_bounded(addr).map_err(|e| BuildRequestError::Connect {
        why: format!(
            "{addr}: {e} — the service is `vike-strategy-builder` (default {})",
            default_address()
        ),
    })?;

    let nonce = handshake(&mut stream)?;

    let scope = builder::required_scope();
    let mac = auth::sign(builder::DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
    // Past the `Welcome` (its version is this client's, `handshake` checked): msgpack frames.
    write_message(&mut stream, &Request::Auth { scope, mac }).map_err(proto)?;
    match read_message::<_, Response>(&mut stream).map_err(proto)? {
        Response::AuthOk => {}
        Response::AuthDenied { reason } => return Err(BuildRequestError::AuthDenied { reason }),
        other => {
            return Err(BuildRequestError::Protocol {
                why: format!("expected AuthOk/AuthDenied, got {}", response_kind(&other)),
            });
        }
    }

    // The handshake is over: stop bounding the read, because what follows is a compile. See the
    // module header for why a ceiling here would abort healthy builds.
    arm_request_timeouts(&stream).map_err(proto)?;

    write_message(&mut stream, request).map_err(proto)?;

    match read_message::<_, Response>(&mut stream).map_err(proto)? {
        Response::BuildOk { sha, sdk, folder } => Ok(Built { sha, sdk, folder }),
        Response::BuildErr(refusal) => Err(BuildRequestError::Compile { refusal }),
        other => Err(BuildRequestError::Protocol {
            why: format!("expected BuildOk/BuildErr, got {}", response_kind(&other)),
        }),
    }
}

/// Why [`build_file`] came back without a sha: the builder's key could not be read here, or the request
/// failed ([`BuildRequestError`], whose [`BuildRequestError::Compile`] is the file's own fault). `Display` is the
/// refusal's words, the same either way.
#[derive(Debug, Clone)]
pub enum BuildFileError {
    /// [`builder::resolve_keys`] refused: no builder key in the settings database, or a store that will not open.
    Key(builder::KeyRefusal),
    /// The request itself failed.
    Request(BuildRequestError),
}

/// The BARE words a hub hands on (the refusal's `Display`, never its `rendered()`): the surface that shows them adds
/// the one id (`vike_utility::errors`'s module doc).
impl std::fmt::Display for BuildFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildFileError::Key(refusal) => write!(f, "{refusal}"),
            BuildFileError::Request(refusal) => write!(f, "{refusal}"),
        }
    }
}

impl std::error::Error for BuildFileError {}

impl BuildFileError {
    /// The refusal handed on, its one id kept (a refused Build's, the builder's own): what a hub's builder seam returns.
    #[must_use]
    pub fn carried(&self) -> CarriedRefusal {
        match self {
            BuildFileError::Key(refusal) => refusal.carried(),
            BuildFileError::Request(refusal) => refusal.carried(),
        }
    }
}

/// **A hub's whole ask** (plan 2b §3: the process that needs a file built asks the builder): the
/// builder's key read from the settings database `vars` locates ([`builder::resolve_keys`], the
/// service's own read), then [`build_remote`] at [`default_address`]. `Ok` is what was built: the sha256,
/// the SDK and the build folder naming the artifact ([`Built`]); `Err` is the refusal ([`BuildFileError`]:
/// a missing key, an unreachable service, the compiler's diagnostics, an SDK the file cannot build against).
/// `sdk` is [`build_remote`]'s override. Blocking for as long as the build takes.
pub fn build_file(
    vars: &std::collections::HashMap<String, String>,
    path: &str,
    text: &str,
    sdk: Option<u32>,
) -> Result<Built, BuildFileError> {
    let keys = builder::resolve_keys(vars).map_err(BuildFileError::Key)?;
    // The fingerprint rides the wire unchecked (the module doc's last section): nothing true to send yet.
    build_remote(&default_address(), &keys, path, text, sdk, "").map_err(BuildFileError::Request)
}

/// [`build_file`] for a research STUDY file: the builder's key read the same way, then
/// [`build_study_remote`] at [`default_address`]. `Ok` is what was built ([`Built`]).
pub fn build_study(
    vars: &std::collections::HashMap<String, String>,
    name: &str,
    files: &[(String, String)],
) -> Result<Built, BuildFileError> {
    let keys = builder::resolve_keys(vars).map_err(BuildFileError::Key)?;
    build_study_remote(&default_address(), &keys, name, files).map_err(BuildFileError::Request)
}

/// `Hello` → `Welcome`, returning the connection's nonce. A protocol-version skew fails HERE,
/// naming BOTH numbers, rather than desyncing on the first frame that does not decode.
fn handshake<S: Read + Write>(stream: &mut S) -> Result<[u8; 32], BuildRequestError> {
    write_frame(stream, &Request::Hello { proto_version: PROTO_VERSION }).map_err(proto)?;
    match read_frame::<_, Response>(stream).map_err(proto)? {
        Response::Welcome { proto_version, nonce } => {
            if proto_version != PROTO_VERSION {
                return Err(BuildRequestError::Protocol {
                    why: format!(
                        "protocol version mismatch: this client speaks {PROTO_VERSION}, the builder \
                         speaks {proto_version} — one of the two binaries is stale"
                    ),
                });
            }
            Ok(nonce)
        }
        Response::AuthDenied { reason } => Err(BuildRequestError::AuthDenied { reason }),
        other => Err(BuildRequestError::Protocol {
            why: format!("expected Welcome, got {}", response_kind(&other)),
        }),
    }
}

fn proto(e: io::Error) -> BuildRequestError {
    BuildRequestError::Protocol { why: e.to_string() }
}

/// A name for a response that arrived out of turn — never its contents, which on this protocol
/// could be a whole rustc diagnostic dump in a message about frame ordering.
fn response_kind(r: &Response) -> &'static str {
    match r {
        Response::Welcome { .. } => "Welcome",
        Response::AuthDenied { .. } => "AuthDenied",
        Response::AuthOk => "AuthOk",
        Response::BuildOk { .. } => "BuildOk",
        Response::BuildErr(_) => "BuildErr",
    }
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
    use vike_hub_protocol::auth::Scope;

    /// The client presents the scope the SERVICE requires by calling it, so the two cannot drift.
    #[test]
    fn the_client_presents_the_scope_the_service_requires() {
        assert_eq!(builder::required_scope(), Scope::Write);
    }

    /// The hub's check of an answer (`Built::of_the_sdk`): a build of another SDK than the one asked for
    /// (a run's override, else the file's pin) is refused naming both and never run; a build of the SDK
    /// asked for, an answer to a file that asked for none, and a development checkout's build (which
    /// ignores a pin) pass.
    #[test]
    fn a_build_of_another_sdk_than_asked_for_is_refused_at_the_hub() {
        let built = |sdk| Built { sha: "ab".repeat(32), sdk, folder: "0123abcd".to_string() };
        let refused = built(Some(3)).of_the_sdk(Some(5)).expect_err("another SDK");
        let id = vike_utility::errors::Declared::declared_id(&refused);
        assert_eq!(id, Some("strategy.builder.other_sdk_built"), "carried with its id");
        let why = refused.to_string();
        assert!(why.contains("SDK 5 was asked for") && why.contains("built SDK 3"), "{why}");
        assert!(
            why.contains("--sdk-root") && why.contains("migrate <FILE> --to 3"),
            "the fix: {why}"
        );
        assert_eq!(built(Some(3)).of_the_sdk(Some(3)), Ok(()));
        assert_eq!(built(Some(3)).of_the_sdk(None), Ok(()));
        assert_eq!(built(None).of_the_sdk(Some(5)), Ok(()), "SDK dev ignores a pin");
    }

    /// The default address is the service's own bind address, not a second literal.
    #[test]
    fn the_default_address_is_the_services_own_bind_address() {
        assert_eq!(default_address(), builder::bind_address(builder::DEFAULT_PORT).to_string());
        assert!(default_address().starts_with("127.0.0.1:"), "{}", default_address());
    }

    /// A mac this client produces is exactly the one [`builder::verify_auth`] accepts — the whole
    /// point of both halves living in one crate. Proved against the REAL verifier rather than by
    /// re-deriving the signature here.
    #[test]
    fn a_mac_this_client_signs_is_one_the_service_verifies() {
        let key = b"a-real-builder-key";
        let keys = HubKeys::new(Vec::new(), key.to_vec());
        let nonce = [9u8; 32];
        let scope = builder::required_scope();
        let mac = auth::sign(builder::DOMAIN, keys.key_for(scope), &nonce, PROTO_VERSION, scope);
        assert!(builder::verify_auth(&keys, &nonce, PROTO_VERSION, scope, &mac));
    }

    /// A version skew is reported with BOTH numbers and never silently tolerated: the deployed
    /// builder of `PROTO_VERSION` 4 (a deploy's restart window, whose `BuildErr` this build could not
    /// decode) and a newer one alike. Driven over an in-memory pair of frames rather than a socket: the
    /// decision is pure.
    #[test]
    fn a_protocol_version_skew_names_both_numbers() {
        assert_eq!(PROTO_VERSION, 5);
        for theirs in [4, PROTO_VERSION + 7] {
            let mut framed = Vec::new();
            write_frame(
                &mut framed,
                &Response::Welcome { proto_version: theirs, nonce: [0u8; 32] },
            )
            .expect("frame the planted Welcome");
            let mut s = Scripted { out: Vec::new(), inbox: std::io::Cursor::new(framed) };
            match handshake(&mut s) {
                Err(BuildRequestError::Protocol { why: msg }) => {
                    assert!(msg.contains("protocol version mismatch"), "{msg}");
                    assert!(msg.contains(&PROTO_VERSION.to_string()), "{msg}");
                    assert!(msg.contains(&theirs.to_string()), "{msg}");
                }
                other => panic!("expected a protocol error naming both versions, got {other:?}"),
            }
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
        let out_of_turn =
            Response::BuildErr(HubRefusal::new("strategy.build.compile_failed", &diagnostics));
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
