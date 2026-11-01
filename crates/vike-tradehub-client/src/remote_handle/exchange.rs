//! `exchange_once` — ONE request, ONE reply, over a fresh connection authenticated under a scope the
//! CALLER names, after the capabilities that request owes have been checked.
//!
//! It is the dial half of the account plane's first client (the desktop's "Data providers" group),
//! and it is deliberately the half that knows nothing about that plane: it takes a built
//! [`Request`] and a scope and key as PARAMETERS, so this file names no account wire type and no
//! admin scope. `crates/vike-cli/src/cmd/mcp/tests/credential_fence.rs` partitions the client crate
//! into files that dial and files that name those types, and holds that no file is both; the
//! caller that builds the frame and presents the scope is `vike-app-core`'s provider-key module,
//! which the MCP surface does not link.
//!
//! ⚠ Not a read verb: it writes nothing itself and refuses nothing the node would not, but the
//! `needs` list is the CLIENT-SIDE half of feature negotiation (decision 0112) — a node that does
//! not advertise every capability named is sent NOTHING after the handshake, so a new variant never
//! reaches a server that cannot decode it.

use std::io;
use std::net::ToSocketAddrs;

use crate::handshake::{Refusal, one_shot_handshake, require_feature};
use crate::proto::{Request, Response, read_frame, write_frame};
use vike_hub_proto::auth::Scope;

/// Handshake under `scope` with `key`, refuse CLIENT-SIDE (nothing sent,
/// [`io::ErrorKind::Unsupported`]) unless the node advertised EVERY string in `needs`, send
/// `request` and return the node's reply verbatim.
///
/// The reply is returned as the node sent it, [`Response::Error`] included: a refusal is an
/// ANSWER the caller words for its operator, while a transport fault, a version mismatch or a
/// denied handshake is the [`io::Error`] (the one-shot verbs' shared ladder). The read is bounded
/// by `crate::liveness::ONE_SHOT_REPLY_TIMEOUT`.
///
/// # Errors
/// The handshake's errors; [`io::ErrorKind::Unsupported`] naming the missing capability; the
/// frame write or read.
pub fn exchange_once<A: ToSocketAddrs>(
    addr: A,
    key: &[u8],
    scope: Scope,
    needs: &[&str],
    request: &Request,
) -> io::Result<Response> {
    let (mut stream, features) = one_shot_handshake(addr, key, scope)?;
    for feature in needs {
        require_feature(&features, feature, Refusal::OlderHub("the credential verb is"))?;
    }
    write_frame(&mut stream, request)?;
    read_frame::<_, Response>(&mut stream)
}
