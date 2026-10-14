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
    /// * **the database exists** — the `hub_key` table answers ([`Source::Database`]);
    /// * **no location, or no database** — an EMPTY map ([`Source::None`]): no hub key, so the node
    ///   answers `bad mac` to this client.
    ///
    /// # `is_hub_key` is the SCOPE the read materialises
    ///
    /// Pass the FAMILY predicate — `vike_venues::credential_keys::is_tradehub_pair_key` /
    /// `is_datahub_pair_key` — and the map holds that service's pair and nothing else: a process that
    /// authenticates to ONE service never holds the other's key (blast radius, the same argument as
    /// the credential read's `KeyScope`). `crates/vike-ops/tests/settings_secrets/hub_key_store_gate.rs`
    /// pins every call site's predicate.
    ///
    /// ⚠ **The map holds ONLY the names the predicate admits, so a name it drops is invisible to the
    /// caller.** The tradehub daemon reads the admin key out of this map and therefore passes
    /// `vike_venues::credential_keys::is_tradehub_daemon_key` (pair plus admin key); the CLI and the
    /// desktop pass the pair-only `is_tradehub_pair_key`.
    ///
    /// The handle's [`CredentialScope`] applies here too, in the statement: no hub-key name carries
    /// a live token today, so under [`CredentialScope::NoLiveKeys`] this answers what it answers
    /// under [`CredentialScope::Full`], and a name that did carry one would be withheld.
    ///
    /// # Errors
    /// [`SecretsError`] when a database that exists will not open — never folded into "no hub key".
    pub fn hub_keys(&self, is_hub_key: impl Fn(&str) -> bool) -> Result<Resolved, SecretsError> {
        let mut resolved = match self.backend() {
            Backend::Database(db) => {
                let (secrets, withheld) =
                    crate::db::read_table(&db, crate::db::Table::HubKey, self.scope())?;
                Resolved {
                    secrets,
                    source: Source::Database(db.clone()),
                    warning: permission_warning(&db),
                    scope: self.scope(),
                    withheld,
                    health: StoreHealth::Readable,
                }
            }
            Backend::Absent => Resolved::absent(self.scope()),
        };
        let family = std::mem::take(&mut resolved.secrets)
            .into_map()
            .into_iter()
            .filter(|(name, _)| is_hub_key(name))
            .collect();
        resolved.secrets = SecretMap::from_map(family);
        Ok(resolved)
    }
}
