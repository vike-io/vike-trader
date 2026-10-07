//! The NODE-key store: `resolve_node_keys`, `NodeKeySource` and the migration notice.

use super::*;

/// Where a node key was actually found, so a caller can WARN about the legacy home without
/// re-deriving the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKeySource {
    /// The `node_key` table of `<project>/settings/db/vike.db` — the home since
    /// `docs/decisions/0054`. When this is the answer, NO file was consulted at all.
    Database,
    /// `<project>/settings/node.env` — the home. Nothing to say.
    NodeFile,
    /// `<project>/settings/secrets.env` — the LEGACY home, still read, warned about.
    LegacyCredentialStore,
    /// Neither file carries a node key. The ordinary unconfigured state; silent.
    Absent,
}

/// The NODE-key store: `<project>/settings/node.env`, falling back to the credential store for keys
/// that have not moved yet.
///
/// ⚠ **This is the ONE fallback in the two-store design, it is a MIGRATION and it is temporary.**
/// The rule the split obeys is that a name has one home decided statically — no ladder. This arm
/// exists because the tradehub node keys were written into `secrets.env` by every `node setup` run
/// before 2026-09-08 and a live daemon is holding a pair there right now; deleting the read outright
/// would take a running node's authentication away on the next deploy. It returns
/// [`NodeKeySource::LegacyCredentialStore`] so the caller can say so, once, by name.
///
/// ⚠ It is DELIBERATELY not a merge. Whichever file answers FIRST answers wholly: a pair split
/// across the two files is a half-migrated box, and merging would hide that while producing a
/// mismatched pair — an opaque `bad mac` at the node, which is the exact symptom
/// `crates/vike-cli/tests/node_cli.rs` records as the expensive one. `node.env` existing with a
/// non-empty node key is the whole test.
///
/// ⚠ **`is_node_key` is what "wholly" is scoped OVER, and passing a predicate WIDER than the pair
/// you are about to read is a defect, not a convenience.** This function's answer is *which file*,
/// and the caller then reads its own names out of that file; so a probe matching a name the caller
/// does not use lets ANOTHER service's migration decide this one's. Measured: with the four-name
/// `vike_model::credential_keys::is_platform_key`, a `node.env` holding only the DATAHUB pair —
/// what `vike-cli datahub setup` writes — answered [`NodeKeySource::NodeFile`] for a TRADEHUB
/// caller, whose working pair in `secrets.env` was then dropped, producing a silent `bad mac` (no
/// migration notice fires, because the source was not the legacy one). Pass the FAMILY predicate:
/// `vike_model::credential_keys::is_tradehub_node_key` / `is_datahub_node_key`, whose disjointness
/// and exhaustiveness over the table are pinned by that module's
/// `the_two_service_families_partition_the_platform_table`.
///
/// Nothing here writes, moves or deletes either file.
///
/// ⚠ **The database does NOT stack a third level on that fallback — it REPLACES both.**
/// `docs/decisions/0054` fixes the order explicitly: *"retire 0051's fallback first, or in the same
/// PR that adds the database read, so the depth never exceeds one"*, because a node key resolvable
/// from the database, from `node.env` AND from legacy `secrets.env` is two levels deep and makes
/// 0051's own retirement condition unsatisfiable. So the branches are disjoint, not nested:
///
/// * **[`Backend::Database`]** — the `node_key` table answers WHOLLY. No file is opened, so the
///   legacy arm below is not merely unreached, it is unreachable, and the answer is
///   [`NodeKeySource::Database`].
/// * **[`Backend::Files`]** — byte-identical to the behaviour before 0054, legacy fallback and all.
///
/// What discharges 0051 rather than deferring it is the MIGRATION, not this function: `crate::db`'s
/// `migrate` classifies a node key found in the credential store into the `node_key` table, so a box
/// that never moved its pair by hand arrives in the one-home state by migrating.
pub fn resolve_node_keys(
    settings_dir: Option<&str>,
    is_node_key: impl Fn(&str) -> bool,
) -> Result<(Resolved, NodeKeySource), SecretsError> {
    if let Backend::Database(db) = workspace_backend_from(settings_dir) {
        let file = crate::dotenv::workspace_node_path_from(settings_dir);
        let resolved = resolve_database(&db, crate::db::Table::NodeKey, &file)?;
        // Deliberately NOT probed with `is_node_key`: the table IS the namespace, so "which store
        // carries this family" has already been answered by the schema. Probing would re-introduce
        // the choice the two tables exist to remove.
        return Ok((resolved, NodeKeySource::Database));
    }
    let node = resolve(&crate::dotenv::workspace_node_path_from(settings_dir))?;
    let carries_one = node.secrets.keys().any(&is_node_key);
    if carries_one {
        return Ok((node, NodeKeySource::NodeFile));
    }
    let legacy = resolve_project(settings_dir)?;
    let source = if legacy.secrets.keys().any(&is_node_key) {
        NodeKeySource::LegacyCredentialStore
    } else {
        NodeKeySource::Absent
    };
    Ok((legacy, source))
}

/// The sentence a caller prints when [`resolve_node_keys`] answered
/// [`NodeKeySource::LegacyCredentialStore`] — one place, so five binaries cannot word the same
/// migration five ways.
#[must_use]
pub fn legacy_node_key_notice(settings_dir_display: &str) -> String {
    format!(
        "node keys are still in {settings_dir_display}/{} — the file that also holds every venue \
         key. Move the `VIKE_*_OBSERVE_KEY` / `VIKE_*_CONTROL_KEY` lines to \
         {settings_dir_display}/{}, which holds node keys and nothing else; they are read from \
         there first. This fallback is a migration and will be removed.",
        crate::dotenv::SECRETS_FILE,
        crate::dotenv::NODE_FILE
    )
}
