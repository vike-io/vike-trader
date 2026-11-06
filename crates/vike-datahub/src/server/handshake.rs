//! The pre-auth phase on a KEYED server: `Hello` -> `Welcome{nonce}` -> `Auth` -> verify — the steps
//! are `vike_datahub_client::handshake`'s (the one spelling the backtesthub shares), the log lines
//! this daemon's own.

use std::net::{SocketAddr, TcpStream};

use vike_datahub_client::PROTO_VERSION;
use vike_datahub_client::handshake::{HandshakeNote, HandshakeOutcome, run_keyed_handshake};
use vike_hub_proto::auth::HubKeys;

#[cfg(doc)]
use vike_hub_proto::limits::{SERVER_HANDSHAKE_DEADLINE, SERVER_HANDSHAKE_MAX_FRAME_LEN};

/// Run `Hello` → `Welcome{nonce}` → `Auth` → verify on a KEYED server. Mirrors
/// `vike_tradehub::server::handshake`'s `run_handshake`, including its refusal ordering.
///
/// Both frames are read under [`SERVER_HANDSHAKE_MAX_FRAME_LEN`] rather than the shared 64 MiB ceiling —
/// an unauthenticated peer must not be able to name a large allocation — and the caller has already
/// armed the handshake deadline ([`SERVER_HANDSHAKE_DEADLINE`] in production) as the read timeout, so a
/// peer that opens a socket and says nothing frees its thread in seconds rather than minutes.
///
/// The mac is verified against the REQUESTED scope's key: a scope whose key is ABSENT is refused
/// WITHOUT consulting it (the closed-gate shape), and otherwise `auth::verify` decides in constant
/// time. Each step is logged here, under this module's target, in the words it always had.
pub(super) fn run_handshake(
    stream: &mut TcpStream,
    keys: &HubKeys,
    features: &[String],
    peer: Option<SocketAddr>,
) -> HandshakeOutcome {
    run_keyed_handshake(stream, keys, features, |note| match note {
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
    })
}
