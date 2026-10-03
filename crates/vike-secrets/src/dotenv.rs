//! The `KEY=value` store reader, and where the store lives.
//!
//! It sits in this crate rather than in `vike-bridge-core` for one dependency reason: `vike-cli
//! secrets` reads the store, and `vike-cli` is DataFusion-free, transport-free and on the FAST CI
//! lane — linking `vike-bridge-core` for a 12-line `KEY=VALUE` parser would drag `ureq`,
//! `tungstenite` and `rustls` into it. The `vike-alerting` split made exactly this argument.
//!
//! `vike_bridge_core::credentials` re-exports these functions under their historical paths, so
//! every one of the ~179 existing call sites
//! (`vike_bridge_core::credentials::load_workspace_dotenv()`) resolves unchanged.
//!
//! ⚠ **Read-only, always.** No code path in this workspace deletes, moves or rewrites the store:
//! it is the user's only copy of live venue credentials.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_model::state_path::PROJECT_SETTINGS_DIR;

/// Minimal `KEY=VALUE` parser ('#' comments; optional surrounding quotes) — enough to read the
/// store without a dotenv dependency. Returns the map instead of mutating the process env
/// (`set_var` is unsafe under threads).
///
/// ⚠ Its expressive limits are real: the format cannot represent a value containing a newline, and
/// it strips surrounding quotes unconditionally.
pub fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// The credential file inside the settings directory: `<project>/settings/secrets.env`.
///
/// ⚠ **The DIRECTORY names around it are not spelled here any more, and that was the open question
/// `docs/decisions/0072-vike-secrets-takes-one-vike-edge-and-is-not-split.md` left at this site.**
/// This module used to declare `SETTINGS_DIR` and `STATE_DIR`, second spellings of
/// [`vike_model::state_path::PROJECT_SETTINGS_DIR`] and
/// [`vike_model::state_path::STATE_SUBDIR`], for the one reason their docs gave — that this crate
/// declared no `vike-*` dependency, so the owner's copy could not be imported. 0072 made that
/// false, and on 2026-09-26 they collapsed: this file IMPORTS the first and needs the second
/// nowhere. It is the same disposition the same sweep reached for the account-label pair
/// (`crates/vike-bridge-core/tests/account_label_spellings.rs` records it), and
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` carries the argument in full.
///
/// The FILE names below are this crate's own — `vike-model` declares none of them — so they stay.
pub const SECRETS_FILE: &str = "secrets.env";
/// The NODE-key file inside the settings directory, beside [`SECRETS_FILE`].
///
/// ⚠ **Two files, and this is NOT a precedence chain.** The rule "one store, no chain, no second
/// location" is about one KEY having one HOME — no ladder, no "it might be here or there" — and it
/// is intact: which file a name lives in is decided STATICALLY by
/// `vike_model::credential_keys::is_platform_key`, and no name is ever looked for in both. "Where is
/// my Binance key" and "where is my node key" each still answer in one line.
///
/// WHY THEY ARE SEPARATE, measured 2026-09-08: [`SECRETS_FILE`] holds **168** venue key names
/// (`VENUES × 4 tiers × 3 suffixes`, the product `vike_model::credential_keys`'s
/// `the_credential_grid_is_the_roster_times_the_tiers_times_the_suffixes` pins) plus the bespoke FX
/// shapes, and leaking one of those means somebody signs orders with real money. This file holds
/// **4** — two services × two scopes — and leaking one means somebody reaches a data service. They
/// also grow differently: a new venue adds twelve names there and NONE here.
///
/// ⚠ What this buys is BLAST RADIUS, not access control, and the difference matters. Every
/// subcommand runs as one user with one filesystem view, so anything that can read one file can read
/// the other. What it prevents is a `backtest` process — which needs a node key and no venue key —
/// holding 168 venue secrets in memory where a core dump, a panic payload or a future logging bug
/// could reach them.
pub const NODE_FILE: &str = "node.env";

/// The sub-directory of the settings directory holding the settings DATABASE:
/// `<project>/settings/db`.
///
/// `docs/decisions/0054-settings-move-into-one-database.md`, constraint 1, and the name is the
/// owner's. Three things are decided by it and none is a detail:
///
/// * It is NOT under [`vike_model::state_path::STATE_SUBDIR`]. `state/` is what the process WRITES
///   at runtime — `HALT`, the rolling logs, the change journal; settings are what it READS, and an
///   earlier draft put the database there for the sole reason that the directory was already
///   writable, which is the wrong reason to name a permanent thing.
/// * It is a DIRECTORY of its own rather than a file beside the TOMLs, because the deployed unit
///   mounts `settings/` read-only (`ProtectSystem=strict`) and grants exactly one narrow
///   `ReadWritePaths=<root>/settings/db`. Widening that grant to the whole of `settings/` was
///   refused: it hands the daemon write access to the kill switch's parent directory.
/// * It is `db/` and not `sqlite-db/`: the directory names its CONTENTS, so a change of engine does
///   not make the name a lie.
pub const DB_DIR: &str = "db";
/// The settings database inside [`DB_DIR`]: `<project>/settings/db/vike.db`.
///
/// **ONE PER PROJECT, not one per box** — it sits under the same `<project>` walk as
/// [`SECRETS_FILE`] and [`NODE_FILE`], so a second checkout gets a second database exactly as it
/// gets a second credential file today.
pub const DB_FILE: &str = "vike.db";

/// The variable that names the settings directory OUTRIGHT, skipping the walk: `VIKE_SETTINGS_DIR`.
///
/// ⚠ **The one constant in this module that is still a SECOND SPELLING of
/// [`vike_model::state_path::SETTINGS_DIR_ENV`], and the reason is neither the retired
/// zero-`vike-*` policy nor inertia.** The two directory NAMES beside it collapsed on 2026-09-26
/// (see [`SECRETS_FILE`]); this one did not, because it is an ENVIRONMENT-VARIABLE name and
/// `vike_ops::scan` resolves constants **crate-wide**. `vike_ops::settings::SETTINGS` is keyed on
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

/// The project's credential FILE: `<project>/settings/secrets.env` (the store only while the
/// project has no settings database — `crate::Backend` decides per run), resolved at RUNTIME by
/// walking UP from `start` for the workspace root.
///
/// **Resolved at RUNTIME, deliberately.** A compile-time path would bake in whichever checkout the
/// binary was built in, so a binary built in one worktree would read that worktree's credentials
/// when run from another, and every extra checkout would need its own copy of the live signing
/// keys. Walking up at runtime means one store per project, whichever build produced the binary.
///
/// `None` when neither project marker is above `start` (see [`project_settings_dir`]). The caller
/// reports that with the path it wanted; this never resolves a second location.
pub fn project_secrets_path(start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir(start)?.join(SECRETS_FILE))
}

/// [`project_secrets_path`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk.
pub fn project_secrets_path_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir_from(override_dir, start)?.join(SECRETS_FILE))
}

/// The project's settings DIRECTORY — `<project>/settings` — by walking UP from `start`.
///
/// ⚠ **This is a DELEGATION now, and the ~55 lines of rule that used to sit here are gone with the
/// walk they described.** `vike_model::state_path::project_settings_dir` is the one implementation
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
    vike_model::state_path::project_settings_dir(start)
}

/// [`project_settings_dir`] with [`SETTINGS_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// The override arrives as a PARAMETER — this crate reads no environment. A blank or
/// whitespace-only value falls through to the walk rather than resolving settings to `""` and
/// reading credentials out of the working directory.
pub fn project_settings_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    vike_model::state_path::project_settings_dir_from(override_dir, start)
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
/// It is the same law `dotenv_path_for` already applies one level down for the credential FILE, and
/// it is spelled ONCE — here — because the settings DIRECTORY and the store inside it disagreeing
/// about which project this process belongs to is the whole failure. `vike_boot::boot` wrote its own
/// copy of it as `spec.cwd.and_then(..)` with no override arm at all, and the two halves of a daemon
/// then read different projects: the credentials came out of the NAMED directory
/// (`crate::resolve_project` -> [`workspace_dotenv_path_from`] -> `dotenv_path_for`, the override
/// carried the whole way with no walk), while the policy CEILINGS, the state root, the log home and
/// the startup banner all fell back to the no-project answers. `nonblank`'s own doc carries what
/// this file already paid for writing one law out twice.
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

/// "A blank override is not an override" — ONE spelling of it, because this law decides which file
/// credentials come out of and it was written out twice, in two functions, in this file. Both
/// copies agreed; a sweep deleted the `!` from the second and every test stayed green, which is the
/// standing evidence that agreement was never checked. The two callers are
/// [`project_settings_dir_from`] and [`workspace_dotenv_path_from`].
fn nonblank(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// The credential store for the CURRENT working directory. See [`project_secrets_path`].
///
/// ⚠ **`$VIKE_SETTINGS_DIR`-BLIND**, which is the whole difference between this and
/// [`workspace_dotenv_path_from`], and it is NOT merely "the override is absent here": with no
/// readable working directory the two answer differently even though this one passes `None` to
/// the same resolver — see `dotenv_path_for` below, where that arm lives and is tested. A caller
/// holding an override must spell the `_from` twin.
pub fn workspace_dotenv_path() -> PathBuf {
    workspace_dotenv_path_from(None)
}

/// [`workspace_dotenv_path`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk.
///
/// The override is a PARAMETER, deliberately: making `workspace_dotenv_path` read it directly
/// would be a library reading process env its caller cannot see — a new `Layer::Library` row on a
/// work-list `crates/vike-ops/tests/settings_registry.rs` pins as may-only-shrink. The composition
/// roots pass it down instead (`vike_bridge_core::credentials::load_workspace_secrets_from_env`
/// pulls it out of the one `std::env::vars()` sweep they already own).
pub fn workspace_dotenv_path_from(override_dir: Option<&str>) -> PathBuf {
    dotenv_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_dotenv_path_from`]: the working directory arrives as a PARAMETER,
/// so every arm below — including the one that has no working directory at all — is reachable from
/// a test without `std::env::set_current_dir`, which is process-global and would race every other
/// test in this binary. The same shape `vike_boot::BootSpec`'s `cwd` field already uses.
///
/// ⚠ **`cwd: None` is where the two public spellings STOP agreeing, and it is a reachable state,
/// not a curiosity.** `std::env::current_dir()` fails when the directory a process was started in
/// has been removed, unmounted, or become unsearchable — an ordinary event for a long-lived
/// deployment and for a verification lane whose tree is replaced under it. In that state:
///
/// * `dotenv_path_for(Some("/srv/x/settings"), None)` -> `/srv/x/settings/secrets.env`, because a
///   named directory needs no walk to reach it, and
/// * `dotenv_path_for(None, None)` -> the relative last resort `settings/secrets.env`.
///
/// So `workspace_dotenv_path()` and `workspace_dotenv_path_from(Some(dir))` DISAGREE there, while
/// with a readable working directory the override short-circuits the walk and they agree by
/// construction. `with_no_working_directory_the_override_still_answers` pins both halves — the
/// disagreement is what makes an override-blind call from a root that HAS an override a defect
/// rather than a spelling preference.
///
/// ⚠ **The no-CWD arm is [`project_settings_dir_for`]'s, not a second copy of it.** This function
/// used to spell the "an override needs no walk" law inline, which left the settings DIRECTORY and
/// the store INSIDE it free to answer differently in exactly this state — and they did, through
/// `vike_boot::boot`. Adding the DIRECTORY resolver and writing this one in terms of it is what
/// makes them one law; the last resort below is the only thing left that is this function's own.
fn dotenv_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    settings_file_path_for(override_dir, cwd, SECRETS_FILE)
}

/// The path of one file inside the resolved settings directory.
///
/// ⚠ Extracted from [`dotenv_path_for`] when [`NODE_FILE`] joined [`SECRETS_FILE`], so the two
/// stores cannot resolve their directory differently. That is not tidiness: the settings walk has
/// been the source of three separate defects (#1089, #1101, and the deployment that read a
/// stranger's `settings/`), and a second copy of it would be a fourth place for the same bug to
/// live. One walk, one last-resort, two file names.
fn settings_file_path_for(override_dir: Option<&str>, cwd: Option<&Path>, file: &str) -> PathBuf {
    settings_dir_or_last_resort(override_dir, cwd).join(file)
}

/// **The settings DIRECTORY every path in this module is joined onto** — the walk's answer, or the
/// relative last resort when there is nothing to walk from.
///
/// ⚠ Extracted from [`settings_file_path_for`] so a caller that holds a DIRECTORY can reach the same
/// three names without re-deriving one of them, and so the last resort is spelled ONCE rather than
/// once per name. Nothing about the resolution changed: `settings_file_path_for(o, cwd, F)` is
/// `settings_dir_or_last_resort(o, cwd).join(F)` by construction, which is why every existing path
/// answer is byte-identical.
///
/// The last resort is neither a walk nor a name: a relative `settings/`, resolved against a working
/// directory this process does not have. It is a path, not an answer, and it is all that is left.
#[must_use]
pub fn settings_dir_or_last_resort(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    project_settings_dir_for(override_dir, cwd)
        .unwrap_or_else(|| PathBuf::from(PROJECT_SETTINGS_DIR))
}

/// [`settings_dir_or_last_resort`] with the working directory read here — the front door a BINARY
/// calls, the same shape [`workspace_dotenv_path_from`] has.
///
/// This is what lets a caller holding only the override reach the pair of functions that take a
/// settings DIRECTORY ([`crate::resolve_store_in`], [`crate::save_credentials_to_store`]) without
/// inventing a second derivation of it.
#[must_use]
pub fn workspace_settings_dir_from(override_dir: Option<&str>) -> PathBuf {
    settings_dir_or_last_resort(override_dir, std::env::current_dir().ok().as_deref())
}

/// The credential store inside a settings directory the caller already holds.
#[must_use]
pub fn secrets_path_in(settings_dir: &Path) -> PathBuf {
    settings_dir.join(SECRETS_FILE)
}

/// The node-key store inside a settings directory the caller already holds.
#[must_use]
pub fn node_path_in(settings_dir: &Path) -> PathBuf {
    settings_dir.join(NODE_FILE)
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

/// The settings directory a store FILE sits in — its parent, or the relative last resort for a bare
/// file name.
///
/// ⚠ For callers that were handed a `secrets.env`/`node.env` PATH by their own composition root and
/// never saw the directory it came out of (the cTrader token persister, the GUI's
/// `CredentialHome`). Both build that path as `<settings dir>/<file>`, so the parent IS the
/// directory the walk resolved, and taking it back is not a second walk.
#[must_use]
pub fn settings_dir_of_store(store_file: &Path) -> PathBuf {
    match store_file.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from(PROJECT_SETTINGS_DIR),
    }
}

/// The project's NODE-key store — `<project>/settings/node.env`, resolved exactly as
/// [`workspace_dotenv_path_from`] resolves the credential store.
///
/// The override is a PARAMETER for the same reason it is there: this crate reads no environment.
pub fn workspace_node_path_from(override_dir: Option<&str>) -> PathBuf {
    node_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_node_path_from`] — the working directory arrives as a parameter, so
/// every arm is reachable from a test without `set_current_dir`.
pub fn node_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    settings_file_path_for(override_dir, cwd, NODE_FILE)
}

/// The project's settings DATABASE — `<project>/settings/db/vike.db`, resolved through the SAME
/// walk as [`workspace_dotenv_path_from`] and [`workspace_node_path_from`].
///
/// One walk, one last resort, now three names. The database must not resolve its directory
/// differently from the two files it is draining, or a box could migrate one project's credentials
/// into another project's database — the #1089/#1101 family wearing a new artifact.
pub fn workspace_db_path_from(override_dir: Option<&str>) -> PathBuf {
    db_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_db_path_from`] — the working directory arrives as a parameter.
///
/// [`db_path_in`] is the shape below the directory and [`settings_dir_or_last_resort`] is the
/// directory, so the sub-directory costs no second copy of the walk and no second spelling of the
/// database's own location.
pub fn db_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    db_path_in(&settings_dir_or_last_resort(override_dir, cwd))
}

/// Load and parse the project's credential store.
///
/// Returns an empty map when the file is absent — callers then hit the live gate (no creds → stay
/// paper).
pub fn load_workspace_dotenv() -> HashMap<String, String> {
    load_workspace_dotenv_from(None)
}

/// [`load_workspace_dotenv`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk — the
/// LOADER half of [`workspace_dotenv_path_from`], which shipped without one.
///
/// **The override is a PARAMETER, and that is the whole design.** Making [`load_workspace_dotenv`]
/// read the variable for itself would be a library reading process env its caller can neither see
/// nor substitute — a new `Layer::Library` row on the work-list
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` pins as may-only-shrink, so
/// `library_rows_do_not_grow` would refuse it. This function reads no environment. A caller that
/// has one hands the value down: a BINARY through
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env`, out of the single
/// `std::env::vars()` sweep it already owns; a TEST binary — itself a `main`, so the read scores
/// `Layer::TestOnly` rather than `Layer::Library` — at its own call site.
///
/// ⚠ **Its absence was a live defect, not an asymmetry.** `settings/` is gitignored, so a git
/// worktree or a the CI box verification lane checks out `settings/*.toml` and never `secrets.env`;
/// every `#[ignore]`d venue smoke called the override-blind twin, resolved the empty `settings/`
/// beside it, and self-SKIPPED in silence — "no creds → stay paper" is a legitimate state, so
/// nothing was logged and nothing went red. Measured 2026-08-19 in a lane whose `VIKE_SETTINGS_DIR`
/// named a store holding the credentials: `alpaca_reconcile_smoke` reported no creds and passed.
///
/// A blank or whitespace-only value falls through to the walk — the law
/// [`workspace_dotenv_path_from`] inherits from `nonblank`. An absent store still yields an empty
/// map, exactly as the no-override twin does.
///
/// # ⚠ It asks [`crate::store::resolve_store_in`], because it was a SECOND STORE CHOICE
///
/// Until `docs/decisions/0054`'s credential half this function was a `read_to_string` of the
/// credential FILE, and after it that was a second, file-only credential reader sitting beside the
/// backend-aware one — the ladder 0051 forbids, wearing the other artifact. On a migrated box it
/// opened the retired file and answered with whatever was left in it while every other reader used
/// the database: `crates/vike-run/src/bin/ibkr_mount.rs`'s `main` and
/// `crates/vike-backfill/src/bin/ibkr_backfill.rs`'s `main` (both bins are DELETED now, measured
/// unused — the second by docs/decisions/0094 — but both were live callers of this function when
/// the bug above was found) silently lost IBKR's account, host, port
/// and client id, and `crates/bridges/polymarket/src/egress.rs`'s now-deleted `dotenv_proxy_vars`
/// and `dotenv_rate_gate`, plus the chain watcher's `dotenv_chain_vars` (in the polymarket
/// settlement cluster's `chain.rs`, deleted by decision 0095 too), silently lost their settings.
/// Nothing errored, because an absent key IS the live gate.
///
/// ⚠ Those three polymarket readers cached in a `OnceLock`, so the store choice was made at their
/// FIRST call and held for the process. That was unchanged by this fix and was not a second choice —
/// `backend_in` was consulted inside the same `get_or_init`, so what they cached was the answer the
/// rest of the process already had. Decision 0095 then deleted all three: the bridge's proxy
/// settings are declared rows now (`vike_polymarket::declare_egress`), and the chain watcher —
/// started by nothing — takes its settings as a `ChainRpcSettings` parameter; none is read through
/// this path.
///
/// So the store choice is made in exactly one place for this reader too: [`crate::store::backend_in`],
/// on the same settings directory, through the same front door every composition root already uses.
/// [`workspace_settings_dir_from`] reads the working directory once and
/// `settings_file_path_for(o, cwd, F) == settings_dir_or_last_resort(o, cwd).join(F)` by
/// construction, so this is the SAME path derivation it performed before — one walk, not two.
///
/// **A box with no database is byte-identical to before.** `Backend::Files` sends this straight to
/// [`crate::store::resolve`], whose arms are the two this function already had: a present file is
/// [`parse_dotenv`] of the same bytes, an absent one is an empty map. The file arm additionally
/// computes the store's permission and legacy findings, which this function has no channel to report
/// and discards — the same silence it kept before, not a new one.
///
/// # ⚠ What a database that EXISTS and cannot be READ does here, stated because it is a loss
///
/// **It yields an EMPTY MAP, silently** — this function is infallible by signature and this crate
/// carries no logging dependency, so there is nowhere for the error to go. That is deliberate: these
/// are STARTUP paths (two `main`s and a proxy resolver) that must not gain a new hard failure, and
/// the infallible shape is the one every one of the ~60 call sites was written against.
///
/// It is also not a NEW silence, and that is the honest limit of the claim: a present-but-unreadable
/// FILE has always returned an empty map here too. What the database changes is the *likelihood* —
/// `check_schema_version` refuses an unstamped or wrong-version database that a file reader would
/// never have rejected — so the asymmetry is PINNED rather than assumed, by
/// `crates/vike-secrets/tests/database_migration.rs`'s
/// `an_unreadable_database_is_empty_to_the_infallible_reader_and_loud_to_the_fallible_one`.
///
/// **The LOUD reader is [`crate::store::resolve_project`]**, which returns the error, and every
/// composition root uses it (`vike_bridge_core::credentials::try_load_workspace_secrets_at` logs the
/// finding; `vike-cli secrets` prints it). A caller that needs to tell "no credentials" from "the
/// store is broken" must use that one — as it had to before 0054, for the same reason.
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
/// venue setting reaches no credential map. The permission and shadowed findings are still
/// discarded here (this function is infallible by signature and this crate carries no logging
/// dependency); `try_load_workspace_secrets_at` is where they are surfaced.
pub fn load_workspace_dotenv_from(override_dir: Option<&str>) -> HashMap<String, String> {
    crate::store::resolve_store_in(
        &workspace_settings_dir_from(override_dir),
        crate::db::Table::Credential,
    )
    .map(|resolved| resolved.secrets.into_map())
    .unwrap_or_default()
}

#[path = "dotenv_tests.rs"]
#[cfg(test)]
mod dotenv_tests;
