//! The NODE-key store: `resolve_node_keys`, over the settings database's `node_key` table.

use super::*;

/// **The NODE-key store: the `node_key` table of `<project>/settings/db/vike.db`, narrowed to the
/// caller's own service FAMILY.**
///
/// `docs/decisions/0051-node-keys-live-in-their-own-store.md` gave the node pairs a home of their
/// own and `docs/decisions/0054` made that home a TABLE, so "which store holds my node key" has one
/// answer. This is [`resolve_store_in`] over [`crate::db::Table::NodeKey`] and the project walk:
///
/// * **the database exists** — the `node_key` table answers ([`Source::Database`]);
/// * **no database** — an EMPTY map ([`Source::None`]): no node key, so the node answers `bad mac`
///   to this client.
///
/// # `is_node_key` is the SCOPE the read materialises
///
/// Pass the FAMILY predicate — `vike_model::credential_keys::is_tradehub_node_key` /
/// `is_datahub_node_key` — and the map holds that service's pair and nothing else: a process that
/// authenticates to ONE service never holds the other's key (blast radius, the same argument as the
/// credential read's `KeyScope`). `crates/vike-ops/tests/settings_secrets/node_key_store_gate.rs` pins
/// every call site's predicate.
///
/// ⚠ **The map holds ONLY the names the predicate admits, so a name it drops is invisible to the
/// caller.** The tradehub daemon reads the admin key out of this map and therefore passes
/// `vike_model::credential_keys::is_tradehub_daemon_key` (pair plus admin key); the CLI and the
/// desktop pass the pair-only `is_tradehub_node_key`.
///
/// # Errors
/// [`SecretsError`] when a database that exists will not open — never folded into "no node key".
pub fn resolve_node_keys(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<Resolved, SecretsError> {
    let mut resolved = resolve_store_in(
        &crate::store_locator::workspace_settings_dir_from(settings_dir),
        crate::db::Table::NodeKey,
    )?;
    let family = std::mem::take(&mut resolved.secrets)
        .into_map()
        .into_iter()
        .filter(|(name, _)| is_node_key(name))
        .collect();
    resolved.secrets = SecretMap::from_map(family);
    Ok(resolved)
}
