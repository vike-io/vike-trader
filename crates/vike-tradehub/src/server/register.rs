//! `server::register` — the invitation's REGISTRATION door (decision 0288 item 7.3), the one place a peer that holds
//! no access row is given one, and the tradehub is the ONE hub that has it.
//!
//! A guest's app, on the tunnel its one-time ssh key opened, sends `Register` in `Auth`'s place: the invitation's
//! id, a proof of its secret over this connection's nonce (`vike_hub_protocol::access::invite_proof`: the secret
//! never crosses the wire), the device name it chose, its new device key's public half and its new permanent ssh
//! key. The door answers `Registered` or `invitation refused`, and the connection closes either way; the device
//! then dials with `PersonAuth` like any other.
//!
//! The order, every step checked at the moment of use (no timer):
//! 1. **Loopback only.** The hub wire is plaintext, so only the ssh tunnel may carry a registration, even on a box
//!    that consented to a public bind.
//! 2. **The shapes, here, before the store** — `vike-secrets` sits on this crate's floor beside
//!    `vike-hub-protocol` and names neither `valid_name`, `valid_ssh_key` nor `key_fingerprint`: the invitation id is
//!    8 lower-case hex, the device a [`valid_name`], the ssh key a [`valid_ssh_key`] (it lands in `authorized_keys`
//!    at the swap), the public key 32 bytes, and the key id is computed HERE from those bytes, never taken from the
//!    peer.
//! 3. **ONE transaction in the store** (`vike_secrets::access::register_access`): the invitation exists, is not
//!    registered, has not expired, the proof verifies, no live row holds the device; then the row is in.
//!
//! Every refusal says `invitation refused` on the wire and its cause at `warn` in the log; a grant is a `warn` line
//! too, because it is the moment a person gained access and a box's file level is `warn`.

use std::net::SocketAddr;
use std::path::Path;

use vike_hub_protocol::access::{invite_proof_verifies, valid_name, valid_ssh_key};
use vike_hub_protocol::auth::key_fingerprint;
use vike_hub_protocol::handshake::{Registered, Registration};
use vike_secrets::access::{RegisterOutcome, register_access};
use vike_tradehub_client::proto::NODE_PROTO_VERSION;

/// The wire's answer to every refused registration.
pub(super) const INVITATION_REFUSED: &str = "invitation refused";

/// An invitation id: 8 lower-case hex characters (decision 0288 item 7.1).
fn valid_invite_id(id: &str) -> bool {
    id.len() == 8 && id.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Log a refusal's cause and answer the wire's one text. `invite` is logged only once its shape is known.
fn refused(peer: Option<SocketAddr>, invite: Option<&str>, why: &str) -> &'static str {
    tracing::warn!(?peer, invite, why, "vike-tradehub: an invitation's registration refused");
    INVITATION_REFUSED
}

/// The grant's `access` record in the change journal beside the store (decision 0288 item 5): verb `register`, under
/// the wire actor of the device that registered (no scope: it authenticated as none). A failed append is an `error`
/// and never undoes the grant, which has landed.
fn journal_registration(
    settings_dir: &Path,
    peer: Option<SocketAddr>,
    person: &str,
    device: &str,
    role: &str,
    key_id: &str,
    now_ms: i64,
) {
    use vike_change_journal::{Change, ChangeActor, ChangeJournal, ChangeOutcome, ChangeProcess};
    let journal = ChangeJournal::in_state_dir(
        &settings_dir.join(vike_paths::state_path::STATE_SUBDIR),
        ChangeProcess::current(env!("CARGO_PKG_VERSION")),
    );
    let actor = ChangeActor::wire(
        peer.map(|p| p.to_string()).as_deref(),
        None,
        Some(key_id),
        Some((person, device)),
    );
    let change = Change::access(
        ChangeOutcome::Applied,
        actor,
        "register",
        person,
        Some(device),
        Some(role),
        Some(key_id),
    );
    if let Err(e) = journal.append(now_ms, &change) {
        tracing::error!(
            error = %e,
            person,
            device,
            "vike-tradehub: a registration NOT recorded to the change journal (the grant DID land)"
        );
    }
}

/// The door: `registration` from `peer` over the connection whose nonce is `nonce`, against the store under
/// `settings_dir`, at `now_ms`. The module doc carries the order.
///
/// # Errors
/// [`INVITATION_REFUSED`], whatever the cause (the log names it).
pub(super) fn register(
    settings_dir: &Path,
    peer: Option<SocketAddr>,
    registration: Registration,
    nonce: &[u8; 32],
    now_ms: i64,
) -> Result<Registered, &'static str> {
    if !peer.is_some_and(|p| p.ip().is_loopback()) {
        return Err(refused(peer, None, "not a loopback peer: only the ssh tunnel may carry one"));
    }
    let Registration { invite, proof, device, public_key, ssh_key } = registration;
    if !valid_invite_id(&invite) {
        return Err(refused(peer, None, "the invitation id is not 8 lower-case hex characters"));
    }
    let invite_id = invite.as_str();
    let invite = Some(invite_id);
    if !valid_name(&device) {
        return Err(refused(peer, invite, "the device name is not a lower-case name of 1-32"));
    }
    if !valid_ssh_key(&ssh_key) {
        return Err(refused(peer, invite, "the ssh key is not one `ssh-ed25519 <base64>`"));
    }
    if public_key.len() != 32 {
        return Err(refused(peer, invite, "the device key is not 32 bytes"));
    }
    let key_id = key_fingerprint(&public_key);
    let outcome = register_access(
        settings_dir,
        invite_id,
        &device,
        &public_key,
        &key_id,
        &ssh_key,
        now_ms,
        |auth_secret| invite_proof_verifies(auth_secret, nonce, NODE_PROTO_VERSION, &proof),
    );
    match outcome {
        Ok(RegisterOutcome::Registered { person, role }) => {
            tracing::warn!(
                ?peer,
                invite,
                person = person.as_str(),
                device = device.as_str(),
                role = role.as_str(),
                key_id = key_id.as_str(),
                "vike-tradehub: a device registered with an invitation (decision 0288)"
            );
            journal_registration(settings_dir, peer, &person, &device, &role, &key_id, now_ms);
            Ok(Registered { person, device, role, key_id })
        }
        Ok(RegisterOutcome::Unknown) => Err(refused(peer, invite, "no such invitation")),
        Ok(RegisterOutcome::Expired) => Err(refused(peer, invite, "the invitation expired")),
        Ok(RegisterOutcome::AlreadyRegistered) => {
            Err(refused(peer, invite, "the invitation was used already"))
        }
        Ok(RegisterOutcome::WrongProof) => Err(refused(peer, invite, "the proof did not verify")),
        Ok(RegisterOutcome::DeviceTaken) => {
            Err(refused(peer, invite, "a live row holds this device or this key"))
        }
        Err(e) => {
            tracing::error!(
                ?peer,
                invite,
                error = %e,
                "vike-tradehub: an invitation's registration could not reach the store; nothing was granted"
            );
            Err(INVITATION_REFUSED)
        }
    }
}
