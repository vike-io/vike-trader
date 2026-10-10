//! Where the settings directory and the settings database live — the path half of this crate.
//!
//! The settings database, `<project>/settings/db/vike.db`, is the ONLY credential store
//! (`docs/decisions/0054-settings-move-into-one-database.md`, `docs/decisions/0086-settings-live-only-in-the-database.md`).
//! This module resolves the directory it sits in and the database's path inside it, and it holds
//! the one database-backed loader, [`load_project_secrets`]: the SILENT twin of
//! `vike_bridge_core::credentials::load_workspace_secrets_at` — the same map, with the store's
//! findings left unlogged because this crate carries no logging dependency.
//! `vike_bridge_core::credentials` re-exports none of this module's names.
//!
//! ⚠ **Read-only, always.** No code path in this workspace deletes, moves or rewrites the store: it
//! may be the user's only copy of live venue credentials.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_model::paths::state_path::PROJECT_SETTINGS_DIR;

/// The sub-directory of the settings directory holding the settings DATABASE:
/// `<project>/settings/db`.
///
/// `docs/decisions/0054-settings-move-into-one-database.md`, constraint 1, and the name is the
/// owner's. Three things are decided by it and none is a detail:
///
/// * It is NOT under [`vike_model::paths::state_path::STATE_SUBDIR`]. `state/` is what the process WRITES
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
/// [`vike_model::paths::state_path::PROJECT_SETTINGS_DIR`] and
/// [`vike_model::paths::state_path::STATE_SUBDIR`], for the one reason their docs gave — that this crate
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
/// [`vike_model::paths::state_path::SETTINGS_DIR_ENV`], and the reason is neither the retired
/// zero-`vike-*` policy nor inertia.** The two directory NAMES beside it collapsed on 2026-09-26
/// (see [`DB_DIR`]); this one did not, because it is an ENVIRONMENT-VARIABLE name and
/// `vike_model::scan` resolves constants **crate-wide**. `vike_ops::settings::SETTINGS` is keyed on
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
/// Read by [`project_settings_dir_from`]'s CALLER: this module performs no environment read of its
/// own, so nothing here joins the settings registry's `Layer::Library` work-list.
pub const SETTINGS_DIR_ENV: &str = "VIKE_SETTINGS_DIR";

/// The project's settings DIRECTORY — `<project>/settings` — by walking UP from `start`.
///
/// **Resolved at RUNTIME, deliberately.** A compile-time path would bake in whichever checkout the
/// binary was built in, so a binary built in one worktree would read that worktree's credentials
/// when run from another, and every extra checkout would need its own copy of the live signing
/// keys. Walking up at runtime means one store per project, whichever build produced the binary.
///
/// `None` when neither project marker is above `start`. The caller reports that with the path it
/// wanted; this never resolves a second location.
///
/// ⚠ **This is a DELEGATION now, and the ~55 lines of rule that used to sit here are gone with the
/// walk they described.** `vike_model::paths::state_path::project_settings_dir` is the one implementation
/// and its doc is the one statement of the rule: the strength-of-evidence precedence over the two
/// markers, why a declared `[workspace]` root decides alone, why an UNREADABLE manifest is also
/// decisive, and the accepted residual for a deployment installed inside a checkout. Read it there.
///
/// Until this crate took a `vike-model` dependency, that rule was spelled TWICE — once there and
/// once here — because this crate's manifest forbade any `vike-*` edge, so neither copy could see
/// the other. The test that held them equal had to live in a THIRD crate for the same reason, and
/// its own doc recorded that the pin both copies promised had never actually existed. Both copies
/// of the doc rotted independently too, which is the half a parity test could never have caught.
pub fn project_settings_dir(start: &Path) -> Option<PathBuf> {
    vike_model::paths::state_path::project_settings_dir(start)
}

/// [`project_settings_dir`] with [`SETTINGS_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// The override arrives as a PARAMETER — this crate reads no environment. A blank or
/// whitespace-only value falls through to the walk rather than resolving settings to `""` and
/// reading credentials out of the working directory.
pub fn project_settings_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    vike_model::paths::state_path::project_settings_dir_from(override_dir, start)
}

/// [`project_settings_dir_from`] for a caller that may have **no working directory at all** — the
/// start arrives as an `Option`, because the WALK is the half that needs somewhere to start and
/// [`SETTINGS_DIR_ENV`] is not: it NAMES the directory.
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
/// Exactly as [`project_settings_dir_from`] does not probe it: a named directory that is not on disk
/// resolves to that path, and the layers above report it. That is what lets `vike-cli config check`
/// FAIL a set-but-unhonoured `$VIKE_SETTINGS_DIR` by name while merely warning about a walked
/// directory that is not there — a distinction an `is_dir` probe here would erase by handing the
/// walk's answer back for a directory that is only not mounted YET.
pub fn project_settings_dir_for(override_dir: Option<&str>, cwd: Option<&Path>) -> Option<PathBuf> {
    cwd.and_then(|cwd| project_settings_dir_from(override_dir, cwd))
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
pub fn settings_dir_or_last_resort(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    project_settings_dir_for(override_dir, cwd)
        .unwrap_or_else(|| PathBuf::from(PROJECT_SETTINGS_DIR))
}

/// [`settings_dir_or_last_resort`] with the working directory read here — the front door a BINARY
/// calls.
///
/// This is what lets a caller holding only the override reach the pair of functions that take a
/// settings DIRECTORY ([`crate::resolve_store_in`], [`crate::save_credentials_to_store`]) without
/// inventing a second derivation of it.
#[must_use]
pub fn workspace_settings_dir_from(override_dir: Option<&str>) -> PathBuf {
    settings_dir_or_last_resort(override_dir, std::env::current_dir().ok().as_deref())
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
/// walk, [`settings_dir_or_last_resort`].
///
/// The override is a PARAMETER, deliberately: reading it here would be a library reading process
/// env its caller cannot see — a new `Layer::Library` row on a work-list
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs` pins as may-only-shrink. The
/// composition roots pass it down instead.
pub fn workspace_db_path_from(override_dir: Option<&str>) -> PathBuf {
    db_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_db_path_from`] — the working directory arrives as a parameter, so
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
/// [`db_path_in`] is the shape below the directory and [`settings_dir_or_last_resort`] is the
/// directory, so the sub-directory costs no second copy of the walk and no second spelling of the
/// database's own location.
pub fn db_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    db_path_in(&settings_dir_or_last_resort(override_dir, cwd))
}

/// **Load the project's credentials** — the `credential` table of the settings database — under
/// [`SETTINGS_DIR_ENV`]'s value, which wins over the walk; `None` lets the walk answer.
///
/// Returns an empty map when there is no database — callers then hit the live gate (no creds → stay
/// paper). No file is read (decision 0086). It is [`crate::resolve_project`]'s map with the error
/// and the findings dropped, and the SILENT twin of
/// `vike_bridge_core::credentials::load_workspace_secrets_at`, which returns the same map and logs
/// the findings.
///
/// ⚠ It had a no-argument twin, which was this function under `None` and nothing else; it was
/// deleted on 2026-10-09 with the loaders' file-era names, so a caller with nothing to pass spells
/// `None`.
///
/// **The override is a PARAMETER, and that is the whole design.** Making this function
/// read the variable for itself would be a library reading process env its caller can neither see
/// nor substitute — a new `Layer::Library` row on the work-list
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` pins as may-only-shrink, so
/// `library_rows_do_not_grow` would refuse it. This function reads no environment. A caller that
/// has one hands the value down: a BINARY through
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env`, out of the single
/// `std::env::vars()` sweep it already owns; a TEST binary — itself a `main`, so the read scores
/// `Layer::TestOnly` rather than `Layer::Library` — at its own call site.
///
/// ⚠ **Its absence was a live defect, not an asymmetry.** `settings/` is gitignored, so a git
/// worktree or a the CI box verification lane checks out no store; every `#[ignore]`d venue smoke called
/// the override-blind twin, resolved the empty `settings/` beside it, and self-SKIPPED in silence —
/// "no creds → stay paper" is a legitimate state, so nothing was logged and nothing went red.
/// Measured 2026-08-19 in a lane whose `VIKE_SETTINGS_DIR` named a store holding the credentials:
/// `alpaca_reconcile_smoke` reported no creds and passed.
///
/// A blank or whitespace-only value falls through to the walk — the law
/// [`project_settings_dir_for`] spells once in `nonblank`. An absent store yields an empty map.
///
/// # It asks [`crate::store::resolve_store_in`] — the one store choice
///
/// The store choice is made in exactly one place for this reader too: [`crate::store::backend_in`],
/// on the same settings directory, through the same front door every composition root already uses.
/// [`workspace_settings_dir_from`] reads the working directory once, so this is one walk, not two.
///
/// **A box with no database answers an EMPTY map** — the live gate.
///
/// # ⚠ What a database that EXISTS and cannot be READ does here, stated because it is a loss
///
/// **It yields an EMPTY MAP, silently** — this function is infallible by signature and this crate
/// carries no logging dependency, so there is nowhere for the error to go. That is deliberate: these
/// are STARTUP paths that must not gain a new hard failure, and the infallible shape is the one every
/// one of the ~60 call sites was written against. `check_schema_version` refuses an unstamped or
/// wrong-version database, so the asymmetry is PINNED rather than assumed, by
/// `crates/vike-secrets/tests/store/database/read_path.rs`'s
/// `an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one`.
///
/// **The LOUD reader is [`crate::store::resolve_project`]**, which returns the error, and every
/// composition root uses it (`vike_bridge_core::credentials::try_load_workspace_secrets_at` logs the
/// finding; `vike-cli secrets` prints it). A caller that needs to tell "no credentials" from "the
/// store is broken" must use that one.
///
/// # ⚠ It returns NO `venue_setting` row — it folded them from 2026-09-22 until decision 0095's Task 7
///
/// [`crate::store::resolve_store_in`] folded ruling 10's rows into the credential map under their
/// legacy names, and this reader inherited it, because the gap was MEASURED:
/// `crates/bridges/vike-ibkr/tests/ibkr_mktdata_smoke.rs` read the store through this function,
/// found no `IBKR_DEMO_PORT` because ruling 10 had moved the row, fell back to its built-in demo
/// default against a box whose gateway listens elsewhere, and SELF-SKIPPED while printing
/// `test result: ok. 1 passed`. That smoke reads the gateway through
/// [`crate::venue_setting::load_venue_settings`] now, like every mount, and the fold is retired: a
/// venue setting reaches no credential map. The permission finding is still discarded here (this
/// function is infallible by signature and this crate carries no logging dependency);
/// `try_load_workspace_secrets_at` is where it is surfaced.
pub fn load_project_secrets(override_dir: Option<&str>) -> HashMap<String, String> {
    crate::store::resolve_store_in(
        &workspace_settings_dir_from(override_dir),
        crate::db::Table::Credential,
    )
    .map(|resolved| resolved.secrets.into_map())
    .unwrap_or_default()
}

#[path = "store_locator_tests.rs"]
#[cfg(test)]
mod store_locator_tests;
