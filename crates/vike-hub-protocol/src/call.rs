//! What a failed call to a hub DID, as the ONE type a hub client returns where its caller decides (decision 0301:
//! decide on a type, never on an `io::ErrorKind` or the text; decision 0303's four outcomes).
//!
//! The variant is chosen where the PHASE is known: the dial and the handshake (`crate::conn::HubConnection`'s
//! constructors, `crate::handshake::auth_answer`), the exchange (`HubConnection::exchange_frame`: refused before the
//! first byte, or failed after the frame began to leave), and the client crate that decoded the hub's answer (its
//! `Response::Error`, its refusal frame, a reply of another verb). [`HubCallError::failure_outcome`] folds the seven
//! into the four outcomes, so the cure words and the outcome cannot disagree.
//!
//! A verb nobody decides on keeps `io::Result` and propagates with `?`: `From<HubCallError> for io::Error` keeps the
//! kind each variant was before and carries the variant. Nothing decides on that kind:
//! `crates/vike-ops/tests/hygiene/error_kind_match_gate.rs`'s `ERROR_KIND_MATCHES` holds every crate's count of
//! matches on a chosen `io::ErrorKind`.
//!
//! **A door's refusal has ONE id, and the CLIENT gives it** ([`HubAccessRefusal`], `hub.access.denied`): the
//! handshake's `AuthDenied { reason }` carries no id on the wire, since the variant already names the refusal and its
//! reason stays coarse by design (it never tells a missing key from a wrong one). Every reader maps it to
//! [`HubCallError::AccessDenied`], whose `Display` renders the id, so a surface shows `{e}` and never adds its own.

use std::{fmt, io};

use vike_utility::errors::FailureOutcome;

vike_utility::refusal_enum! {
    /// The hub's door refused this key or scope: [`HubCallError::AccessDenied`]'s one id (this module's doc). `why`
    /// is the client's sentence, the hub's own reason inside it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum HubAccessRefusal with ids HubAccessRefusalId {
        Denied { why: String } => "hub.access.denied": "{why}",
    }
}

/// A failed hub call, named for the caller's decision.
#[derive(Debug)]
pub enum HubCallError {
    /// NOT SENT: nothing of the request left (a dial nobody accepted or that could not route, a handshake that failed
    /// before the request, a connection already unusable, a socket option refused). Safe to send again.
    NotSent(io::Error),
    /// OUTCOME UNKNOWN: the request began to leave and no answer this client can read came back (a timeout, a reset,
    /// an end of stream, a reply that does not decode). Never resent: a write is resolved by a read; a read asks
    /// again.
    OutcomeUnknown(io::Error),
    /// NOT NOW: the hub answered "not yet" (a node still starting, a busy core). Nothing was done.
    NotNow { id: Option<String>, text: String },
    /// REFUSED: the hub answered no. `id` is its refusal frame's (`crate::refusal::HubRefusal`), `None` from an
    /// older hub's `Response::Error` or a refusal declared with no id.
    Refused { id: Option<String>, text: String },
    /// REFUSED at the door: the hub refused this key or scope (`AuthDenied`, a keyed hub and no key here, a connection
    /// that authenticated nothing). Cure: the access list or the key. Shown under [`HubAccessRefusal`]'s id.
    AccessDenied(String),
    /// REFUSED before sending: the hub does not offer `feature` (its `Welcome` lacks the word; an older hub cannot
    /// take a device key, `feature` = `person-auth`). Cure: upgrade the hub. Nothing was sent.
    NotOffered { feature: String, text: String },
    /// REFUSED: the two ends do not speak alike (a version or plane mismatch, a `Welcome` without a nonce on a keyed
    /// hub, a peer that is not a hub, a reply of another verb to a read, a request this client will not send). A
    /// `crate::conn::WrongDaemon` rides it when the peer is the other plane's daemon.
    Protocol(Box<dyn std::error::Error + Send + Sync>),
}

impl HubCallError {
    /// What the call DID, in the four words every caller decides on: the cure variants are all `Refused`.
    #[must_use]
    pub const fn failure_outcome(&self) -> FailureOutcome {
        match self {
            HubCallError::NotSent(_) => FailureOutcome::NotSent,
            HubCallError::OutcomeUnknown(_) => FailureOutcome::OutcomeUnknown,
            HubCallError::NotNow { .. } => FailureOutcome::NotNow,
            HubCallError::Refused { .. }
            | HubCallError::AccessDenied(_)
            | HubCallError::NotOffered { .. }
            | HubCallError::Protocol(_) => FailureOutcome::Refused,
        }
    }
}

/// The sentence a person reads: byte for byte what the `io::Error` said before this type, but a refusal the hub
/// declared with an id and the door's refusal ([`HubAccessRefusal`]), which read `error [<id>]: <text>`
/// (`vike_utility::errors::render_refusal`), so a person can look it up in the published `refusals.json`. This is the
/// ONE place a hub's refusal frame and a door's refusal are shown.
impl fmt::Display for HubCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HubCallError::NotSent(e) | HubCallError::OutcomeUnknown(e) => write!(f, "{e}"),
            HubCallError::NotNow { id: Some(id), text }
            | HubCallError::Refused { id: Some(id), text } => {
                f.write_str(&vike_utility::errors::render_refusal(id, text))
            }
            HubCallError::AccessDenied(why) => {
                f.write_str(&HubAccessRefusal::Denied { why: why.clone() }.rendered())
            }
            HubCallError::NotNow { text, .. }
            | HubCallError::Refused { text, .. }
            | HubCallError::NotOffered { text, .. } => f.write_str(text),
            HubCallError::Protocol(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HubCallError {}

/// For a verb nobody decides on, which keeps `io::Result`: the kind each variant had before this type, the variant
/// carried inside for its words.
impl From<HubCallError> for io::Error {
    fn from(error: HubCallError) -> Self {
        let kind = match &error {
            HubCallError::NotSent(e) | HubCallError::OutcomeUnknown(e) => e.kind(),
            HubCallError::NotNow { .. } | HubCallError::Protocol(_) => io::ErrorKind::InvalidData,
            HubCallError::Refused { .. } => io::ErrorKind::Other,
            HubCallError::AccessDenied(_) => io::ErrorKind::PermissionDenied,
            HubCallError::NotOffered { .. } => io::ErrorKind::Unsupported,
        };
        io::Error::new(kind, error)
    }
}

#[cfg(test)]
#[path = "call_tests.rs"]
mod call_tests;
