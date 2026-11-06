//! Where the settings directory and the settings database live — the path half of this crate.
//!
//! The settings database, `<project>/settings/db/vike.db`, is the ONLY credential store
//! (`docs/decisions/0054-settings-move-into-one-database.md`, `docs/decisions/0086-settings-live-only-in-the-database.md`).
//! This module resolves the directory it sits in and the database's path inside it. It READS
//! nothing: every read of the store is `crate::CredentialStore`'s, over a `crate::StoreLocation` a
//! root built from what this module resolved — or that [`open_credential_store`] built for a root
//! that runs no boot, out of the environment map the root hands in.
//!
//! ⚠ **Read-only, always.** No code path in this workspace deletes, moves or rewrites the store: it
//! may be the user's only copy of live venue credentials.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_paths::state_path::PROJECT_SETTINGS_DIR;

/// The sub-directory of the settings directory holding the settings DATABASE:
/// `<project>/settings/db`.
///
/// `docs/decisions/0054-settings-move-into-one-database.md`, constraint 1, and the name is the
/// owner's. Three things are decided by it and none is a detail:
///
/// * It is NOT under [`vike_paths::state_path::STATE_SUBDIR`]. `state/` is what the process WRITES
///   at runtime — `HALT`, the rolling logs, the change journal; settings are what it READS, and an
///   earlier draft put the database there for the sole reason that the directory was already
///   writable, which is the wrong reason to name a permanent thing.
/// * It is a DIRECTORY of its own rather than a file directly in `settings/`, because the deployed
///   unit mounts `settings/` read-only (`ProtectSystem=strict`) and grants exactly one narrow
///   `ReadWritePaths=<root>/settings/db`. Widening that grant to the whole of `settings/` was
///   refused: it hands the daemon write access to the kill switch's parent directory.
/// * It is `db/` and not `sqlite-db/`: the directory names its CONTENTS, so a change of engine does
///   not make the name a lie.
///
/// ⚠ **The DIRECTORY names around it are not spelled here, and that was the open question
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` left at this site.**
/// This module used to declare `SETTINGS_DIR` and `STATE_DIR`, second spellings of
/// [`vike_paths::state_path::PROJECT_SETTINGS_DIR`] and
/// [`vike_paths::state_path::STATE_SUBDIR`], for the one reason their docs gave — that this crate
/// declared no `vike-*` dependency, so the owner's copy could not be imported. 0072 made that
/// false, and on 2026-09-26 they collapsed: this file IMPORTS the first and needs the second
/// nowhere. It is the same disposition the same sweep reached for the account-label pair
/// (`crates/vike-bridge-core/tests/account_label_spellings.rs` records it), and
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` carries the argument in full.
pub const DB_DIR: &str = "db";
/// The settings database inside [`DB_DIR`]: `<project>/settings/db/vike.db`.
///
/// **ONE PER PROJECT, not one per box** — it sits under the `<project>` walk, so a second checkout
/// gets a second database.
pub const DB_FILE: &str = "vike.db";

/// The variable that names the settings directory OUTRIGHT, skipping the walk: `VIKE_SETTINGS_DIR`.
///
/// ⚠ **The one constant in this module that is still a SECOND SPELLING of
/// [`vike_paths::state_path::SETTINGS_DIR_ENV`], and the reason is neither the retired
/// zero-`vike-*` policy nor inertia.** The two directory NAMES beside it collapsed on 2026-09-26
/// (see [`DB_DIR`]); this one did not, because it is an ENVIRONMENT-VARIABLE name and
/// `vike_model::scan` resolves constants **crate-wide**. `vike_config::settings::SETTINGS` is keyed on
/// the pair `(name, krate)` and carries a `("VIKE_SETTINGS_DIR", "vike-secrets")` row whose whole
/// evidence is the string literal below; importing the name instead would strand that row and
/// `every_declared_variable_is_read` would refuse the PR. Deleting the row with it is available and
/// is the wrong trade: the registry's job is that each crate's relationship to a variable is
/// declared where that crate lives, and this crate hands the name to its callers.
///
/// **A duplication that stays owes a pin, and this one finally has one**:
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs`'s
/// `the_settings_dir_variable_is_spelled_the_same_in_both_crates`. It CAN fail — two independent
/// `const` declarations, so editing one and not the other reddens it — unlike a comparison of the
/// merged WALK, which is one function and could not. ⚠ That test's home is no longer forced:
/// 0072's edge means THIS crate can see `vike-model` and could host the assertion itself. It lives
/// there because that file is where this duplication's history is written and where ~20 places in
/// the tree cite it.
///
/// Read by [`project_settings_dir_for`]'s CALLER, or looked up by [`open_credential_store`] in the
/// environment map ITS caller hands in: this module performs no environment read of its own, so
/// nothing here joins the settings registry's `Layer::Library` work-list.
pub const SETTINGS_DIR_ENV: &str = "VIKE_SETTINGS_DIR";

/// The project's settings DIRECTORY — `<project>/settings` — with [`SETTINGS_DIR_ENV`]'s value
/// winning over the walk, for a caller that may have **no working directory at all**: the start
/// arrives as an `Option`, because the WALK is the half that needs somewhere to start and
/// [`SETTINGS_DIR_ENV`] is not: it NAMES the directory.
///
/// The walk itself, and its rule (the strength-of-evidence precedence over the two project
/// markers), is `vike_paths::state_path::project_settings_dir_from`'s, and its doc is the one
/// statement of it. This crate's same-name twins of that function and of `project_settings_dir`
/// were deleted: they were one-line delegations, so a caller names `vike_model`'s directly.
///
/// The override arrives as a PARAMETER — this crate reads no environment. A blank or
/// whitespace-only value falls through to the walk rather than resolving settings to `""` and
/// reading credentials out of the working directory.
///
/// # `cwd: None` is a reachable state, not a curiosity
///
/// `std::env::current_dir()` fails whenever the directory a process was started in has been removed,
/// unmounted or made unsearchable — an ordinary event for a long-lived deployment and for a
/// verification lane whose tree is replaced under it. All three shipped `deploy/*.service` units set
/// `$VIKE_SETTINGS_DIR`, so the pairing *no working directory, override in hand* is exactly the one
/// a deployment reaches.
///
/// # Why the law lives HERE and not at the caller
///
/// It is the same law [`db_path_for`] applies one level down for the settings DATABASE, and it is
/// spelled ONCE — here — because the settings DIRECTORY and the store inside it disagreeing about
/// which project this process belongs to is the whole failure. `vike_boot::boot` once wrote its own
/// copy of it as `spec.cwd.and_then(..)` with no override arm at all, and the two halves of a daemon
/// then read different projects: the credentials came out of the NAMED directory while the policy
/// CEILINGS, the state root, the log home and the startup banner all fell back to the no-project
/// answers. `nonblank`'s own doc carries what this file already paid for writing one law out twice.
///
/// # The override is NOT probed, deliberately
///
/// Exactly as `vike_paths::state_path::project_settings_dir_from` does not probe it: a named directory that is not on disk
/// resolves to that path, and the layers above report it. That is what lets `vike-cli config check`
/// FAIL a set-but-unhonoured `$VIKE_SETTINGS_DIR` by name while merely warning about a walked
/// directory that is not there — a distinction an `is_dir` probe here would erase by handing the
/// walk's answer back for a directory that is only not mounted YET.
pub fn project_settings_dir_for(override_dir: Option<&str>, cwd: Option<&Path>) -> Option<PathBuf> {
    cwd.and_then(|cwd| vike_paths::state_path::project_settings_dir_from(override_dir, cwd))
        .or_else(|| nonblank(override_dir).map(PathBuf::from))
}

/// "A blank override is not an override" — ONE spelling of it, because this law decides which store
/// credentials come out of and it was once written out twice, in two functions, in this file. Both
/// copies agreed; a sweep deleted the `!` from the second and every test stayed green, which is the
/// standing evidence that agreement was never checked. The caller is [`project_settings_dir_for`].
fn nonblank(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// **The settings DIRECTORY every path in this module is joined onto** — the walk's answer, or the
/// relative last resort when there is nothing to walk from.
///
/// Spelled ONCE so a caller that holds a DIRECTORY and a caller that wants the database reach the
/// same answer: [`db_path_for`] is `db_path_in(&settings_dir_or_last_resort(o, cwd))` by
/// construction.
///
/// The last resort is neither a walk nor a name: a relative `settings/`, resolved against a working
/// directory this process does not have. It is a path, not an answer, and it is all that is left.
#[must_use]
fn settings_dir_or_last_resort(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    project_settings_dir_for(override_dir, cwd)
        .unwrap_or_else(|| PathBuf::from(PROJECT_SETTINGS_DIR))
}

/// The settings directory the walk answers (or the relative `settings/` last resort when nothing
/// does), with the working directory read here — the front door a BINARY that runs no boot calls
/// ([`open_credential_store`] builds its [`crate::StoreLocation`] from it).
///
/// This is what lets a caller holding only the override reach what takes a settings DIRECTORY (a
/// [`crate::StoreLocation`], [`crate::save_credentials_to_store`]) without inventing a second
/// derivation of it.
#[must_use]
pub fn workspace_settings_dir_from(override_dir: Option<&str>) -> PathBuf {
    settings_dir_or_last_resort(override_dir, std::env::current_dir().ok().as_deref())
}

/// **Open the credential store for a root that runs NO boot** (a one-shot tool, a vendor bin): the
/// same handle `vike_boot::boot` puts in `vike_boot::Booted::store`, from the one environment sweep
/// the binary owns.
///
/// Two facts are taken out of `env`, once: [`SETTINGS_DIR_ENV`] (the settings directory, which wins
/// over the walk; blank is unset) and [`crate::NO_LIVE_KEYS_ENV`] (the scope, parsed strictly by
/// [`crate::CredentialScope::parse`], UNTRIMMED). The walk is [`workspace_settings_dir_from`]'s,
/// from the process's working directory, with the relative `settings/` last resort when no project
/// is found — the resolution the whole-map loaders this replaces performed. Opening performs no
/// I/O.
///
/// A library function: the map arrives as a PARAMETER and this crate reads no process environment.
///
/// ⚠ **`env` is the REAL PROCESS ENVIRONMENT, never the credential map.**
///
/// # Errors
/// [`crate::ScopeError`] when `VIKE_NO_LIVE_KEYS` is set to anything but `true` or `false`; the
/// root refuses to start on it, as `vike_boot::boot` does.
pub fn open_credential_store(
    env: &HashMap<String, String>,
) -> Result<crate::CredentialStore, crate::ScopeError> {
    let scope =
        crate::CredentialScope::parse(env.get(crate::NO_LIVE_KEYS_ENV).map(String::as_str))?;
    let settings_dir = workspace_settings_dir_from(env.get(SETTINGS_DIR_ENV).map(String::as_str));
    Ok(crate::CredentialStore::open(Some(crate::StoreLocation::new(settings_dir)), scope))
}

/// The settings DATABASE inside a settings directory the caller already holds.
///
/// **The one spelling of the database's shape below the settings directory.** [`db_path_for`] is
/// this function over the walk's answer, and `crate::store`'s backend probe is this function over a
/// directory a caller handed in — so a reader and a writer cannot disagree about where the database
/// is, which is the property `crate::store::Backend` rests on.
#[must_use]
pub fn db_path_in(settings_dir: &Path) -> PathBuf {
    settings_dir.join(DB_DIR).join(DB_FILE)
}

/// The project's settings DATABASE — `<project>/settings/db/vike.db`, resolved through the ONE
/// walk, `settings_dir_or_last_resort`.
///
/// The override is a PARAMETER, deliberately: reading it here would be a library reading process
/// env its caller cannot see — a new `Layer::Library` row on a work-list
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` pins as may-only-shrink. The
/// composition roots pass it down instead.
pub(crate) fn workspace_db_path_from(override_dir: Option<&str>) -> PathBuf {
    db_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of `workspace_db_path_from` (the crate-private resolver `create_store` uses) — the working directory arrives as a parameter, so
/// every arm below — including the one that has no working directory at all — is reachable from a
/// test without `std::env::set_current_dir`, which is process-global and would race every other
/// test in this binary. The same shape `vike_boot::BootSpec`'s `cwd` field already uses.
///
/// ⚠ **`cwd: None` is where an override-blind caller and an override-holding one STOP agreeing.**
/// With no readable working directory, `db_path_for(Some("/srv/x/settings"), None)` is
/// `/srv/x/settings/db/vike.db` (a named directory needs no walk to reach it) while
/// `db_path_for(None, None)` is the relative last resort `settings/db/vike.db`.
/// `with_no_working_directory_the_override_still_answers` pins both halves — the disagreement is
/// what makes an override-blind call from a root that HAS an override a defect rather than a
/// spelling preference.
///
/// [`db_path_in`] is the shape below the directory and `settings_dir_or_last_resort` is the
/// directory, so the sub-directory costs no second copy of the walk and no second spelling of the
/// database's own location.
pub fn db_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    db_path_in(&settings_dir_or_last_resort(override_dir, cwd))
}

#[path = "store_locator_tests.rs"]
#[cfg(test)]
mod store_locator_tests;
