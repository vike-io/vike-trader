//! The NODE-key read: `CredentialStore::hub_keys`, over the settings database's `hub_key` table.

use super::*;

impl CredentialStore {
    /// **The NODE-key store: the `hub_key` table of `<project>/settings/db/vike.db`, narrowed to
    /// the caller's own service FAMILY.**
    ///
    /// `docs/decisions/0051-node-keys-live-in-their-own-store.md` gave the node pairs a home of their
    /// own and `docs/decisions/0054` made that home a TABLE, so "which store holds my hub key" has
    /// one answer. This is the `hub_key` table through the same backend decision every read of this
    /// handle makes, under the same scope:
    ///
    /// * **the database exists** — the `hub_key` table answers ([`CredentialSource::Database`]);
    /// * **no location, or no database** — an EMPTY map ([`CredentialSource::None`]): no hub key, so the node
    ///   answers `bad mac` to this client.
    ///
    /// # `is_hub_key` is the SCOPE the read materialises
    ///
    /// Pass the narrowest predicate that names the keys the caller uses — a daemon's family
    /// (`vike_venues::credential_keys::is_gatewayhub_daemon_key`) or one client name
    /// (`is_tradehub_gatewayhub_trade_client_key`) — and the map holds those and nothing else: a
    /// process never holds another service's key (blast radius, the same argument as the credential
    /// read's `CredentialKeyScope`). `crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs`
    /// pins every call site's predicate.
    ///
    /// ⚠ **The map holds ONLY the names the predicate admits, so a name it drops is invisible to the
    /// caller.** Every key here is a DAEMON's (decision 0164's class): a person signs with its own
    /// device key, and a retired person row left in the table (decision 0288) is read by no caller.
    ///
    /// The handle's [`CredentialScope`] applies here too, in the statement: no hub-key name carries
    /// a live token today, so under [`CredentialScope::NoLiveKeys`] this answers what it answers
    /// under [`CredentialScope::Full`], and a name that did carry one would be withheld. Under
    /// [`CredentialScope::NoVenueKeys`] it answers in full: a hub key is no account's and no
    /// venue's, and the hubs that take that scope read nothing else.
    ///
    /// # Errors
    /// [`StoreReadError`] when a database that exists will not open — never folded into "no hub key".
    pub fn hub_keys(
        &self,
        is_hub_key: impl Fn(&str) -> bool,
    ) -> Result<CredentialRead, StoreReadError> {
        let mut resolved = self.read_resolved(crate::db::StoreTable::HubKey)?;
        let family = std::mem::take(&mut resolved.secrets)
            .into_map()
            .into_iter()
            .filter(|(name, _)| is_hub_key(name))
            .collect();
        resolved.secrets = CredentialMap::from_map(family);
        Ok(resolved)
    }
}
