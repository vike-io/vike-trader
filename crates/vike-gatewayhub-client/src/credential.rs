//! The credential-PRESENCE question: whether the credential a venue's credentialed lane reads is
//! stored on the gatewayhub's box. A word, never a value.
//!
//! The word is `vike_model::CredentialPresence`, ONE type for both hub protocols:
//! `Response::PresenceIs` carries it here, and the datahub forwards it unchanged into its
//! history-channels report.
//!
//! ⚠ **`Unreadable` is never collapsed into `Absent`**
//! (`docs/decisions/0097-the-datahub-reads-one-practice-token-for-a-credentialed-history-lane.md`,
//! verdict 6): reported as "not stored", it sends the operator to store a key they already stored.
//! And an UNREACHABLE gatewayhub answers [`CredentialPresence::NotChecked`] — nothing was read — never
//! `Absent`.

use crate::client::{GatewayClient, GatewayLink, unexpected};
use crate::proto::{FEATURE_GATEWAY_PRESENCE, Request, Response};
use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_model::CredentialPresence;

/// Ask the gatewayhub whether `venue`'s credential is stored.
///
/// **Total, and never an error**: a dead or refusing gatewayhub, a daemon that does not advertise
/// `gateway_presence`, a link without a read key and a reply that is not a presence word all answer
/// [`CredentialPresence::NotChecked`], because nothing was read — the history-channels report then
/// says "not checked", which is the truth, rather than "not stored", which would be a lie. Called
/// once per report and uncached, so a key stored or removed after start is what the next answer says.
pub fn credential_presence(link: &GatewayLink, venue: &str) -> CredentialPresence {
    let ask = || -> Result<CredentialPresence, HubCallError> {
        let mut client = GatewayClient::connect(link, Scope::Read)?;
        client.require_feature(FEATURE_GATEWAY_PRESENCE)?;
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
