//! The pre-auth phase on a KEYED server: `Hello` -> `Welcome{nonce}` -> `Auth` -> verify — the steps
//! are `vike_hub_protocol::handshake`'s (the one spelling the backtesthub and the gatewayhub share), the
//! log lines this daemon's own.

use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::proto::request_kind;
use vike_hub_protocol::auth::{DATAHUB_DOMAIN, HubKeys};
use vike_hub_protocol::frame::MAX_FRAME_LEN;
use vike_hub_protocol::handshake::{
    HandshakeNote, HandshakeOutcome, HandshakeSpec, run_keyed_handshake,
};
use vike_hub_protocol::plane::Plane;

#[cfg(doc)]
use vike_hub_protocol::limits::{SERVER_HANDSHAKE_DEADLINE, SERVER_HANDSHAKE_MAX_FRAME_LEN};

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server. Mirrors
/// `vike_tradehub::server::handshake`'s `run_handshake`, including its refusal ordering.
///
/// Both frames are read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling —
/// an unauthenticated peer must not be able to name a large allocation — and the WHOLE phase under ONE
/// `deadline` ([`SERVER_HANDSHAKE_DEADLINE`] in production), so a peer that opens a socket and says
/// nothing, or trickles a byte inside every read timeout, frees its thread in seconds rather than
/// minutes.
///
/// The mac is verified against the REQUESTED scope's key, under [`DATAHUB_DOMAIN`]: every scope is
/// served, so a scope whose key is ABSENT is the one refused WITHOUT consulting a key (the closed-gate
/// shape), and otherwise `auth::verify` decides in constant time. Its replies are written under the
/// frame codec's own ceiling. Each step is logged here, under this module's target, in the words it
/// always had.
pub(super) fn run_handshake(
    stream: &mut TcpStream,
    keys: &HubKeys,
    features: &[String],
    peer: Option<SocketAddr>,
    deadline: Duration,
) -> HandshakeOutcome {
    let spec = HandshakeSpec {
        domain: DATAHUB_DOMAIN,
        proto_version: PROTO_VERSION,
        plane: Some(Plane::Data),
        features,
        deadline,
        reply_ceiling: MAX_FRAME_LEN,
    };
    run_keyed_handshake(
        stream,
        keys,
        spec,
        |_| true,
        request_kind,
        |note| match note {
            HandshakeNote::VerbRefused(verb) => {
                tracing::info!(
                    ?peer,
                    verb,
                    "vike-datahub: verb refused — connection is not authenticated"
                )
            }
            HandshakeNote::VersionDiffers(client_version) => tracing::debug!(
                ?peer,
                client_version,
                server_version = PROTO_VERSION,
                "vike-datahub: client protocol version differs; auth will fail on the signed version"
            ),
            HandshakeNote::NoKey(scope) => tracing::info!(
                ?peer,
                ?scope,
                "vike-datahub: auth refused — no key configured for this scope on this server"
            ),
            HandshakeNote::Authenticated(scope) => {
                tracing::info!(?peer, ?scope, "vike-datahub: authenticated")
            }
            HandshakeNote::BadMac(scope) => {
                tracing::info!(?peer, ?scope, "vike-datahub: auth denied (bad mac)")
            }
        },
    )
}
