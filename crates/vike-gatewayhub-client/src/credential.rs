//! The credential-PRESENCE word — whether the credential a venue's credentialed lane reads is stored
//! on the gatewayhub's box. A word, never a value.
//!
//! `vike-datahub-client` has the same five-word type (`history::CredentialPresence`, which the
//! history-channels report renders); the two hub clients sit at one layer and neither may name the
//! other, so this crate RE-DECLARES it with the same variant names and the same serde tags, and the
//! datahub maps one to the other. (The root `CLAUDE.md`'s cure for two sides that must not disagree
//! is a shared crate BELOW both; none exists for a five-word enum without inverting the layers, so
//! the copy is held equal by the consumer's exhaustive `match`, which fails to compile on a sixth
//! word.)
//!
//! ⚠ **`Unreadable` is never collapsed into `Absent`**
//! (`docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md`,
//! verdict 6): reported as "not stored", it sends the operator to store a key they already stored.
//! And an UNREACHABLE gatewayhub answers [`CredentialPresence::NotChecked`] — nothing was read — never
//! `Absent`.

use serde::{Deserialize, Serialize};

use crate::client::{GatewayClient, GatewayLink, unexpected};
use crate::proto::{FEATURE_GW_PRESENCE, Request, Response};
use vike_hub_proto::auth::Scope;

/// Whether the credential a lane reads is STORED on the gatewayhub's box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialPresence {
    /// No lane on this server needs a credential for this venue: a keyless lane, or a venue no lane
    /// serves.
    NotNeeded,
    /// The credential is stored, with a non-blank value.
    Present,
    /// The store answered and does not hold it (or holds it blank), or the daemon resolved no store.
    Absent,
    /// A store exists and could not be read, so whether it is stored cannot be told.
    Unreadable,
    /// Nothing was read: the venue needs a credential and the daemon holds no way to look, or the
    /// daemon could not be asked. Not `Absent`.
    NotChecked,
}

impl CredentialPresence {
    /// Every word, in declaration order.
    pub const ALL: [CredentialPresence; 5] = [
        CredentialPresence::NotNeeded,
        CredentialPresence::Present,
        CredentialPresence::Absent,
        CredentialPresence::Unreadable,
        CredentialPresence::NotChecked,
    ];
}

/// Ask the gatewayhub whether `venue`'s credential is stored.
///
/// **Total, and never an error**: a dead or refusing gatewayhub, a daemon that does not advertise
/// `gw_presence`, a link without a read key and a reply that is not a presence word all answer
/// [`CredentialPresence::NotChecked`], because nothing was read — the history-channels report then
/// says "not checked", which is the truth, rather than "not stored", which would be a lie. Called
/// once per report and uncached, so a key stored or removed after start is what the next answer says.
pub fn credential_presence(link: &GatewayLink, venue: &str) -> CredentialPresence {
    let ask = || -> std::io::Result<CredentialPresence> {
        let mut client = GatewayClient::connect(link, Scope::Read)?;
        client.require_feature(FEATURE_GW_PRESENCE)?;
        match client.exchange(&Request::CredentialPresence { venue: venue.to_string() })? {
            Response::PresenceIs { presence } => Ok(presence),
            other => Err(unexpected(link.addr(), "PresenceIs", &other)),
        }
    };
    match ask() {
        Ok(presence) => presence,
        Err(e) => {
            tracing::debug!(venue, error = %e, "gatewayhub credential presence: not checked");
            CredentialPresence::NotChecked
        }
    }
}
