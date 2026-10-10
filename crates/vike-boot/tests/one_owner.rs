//! **The startup sequence has ONE owner, and that is a gate rather than a convention.**
//!
//! # Why a gate
//!
//! The ORDER is load-bearing and subtle (settings must load before `vike_log::init`, because the
//! log directory and both log levels are themselves settings), and the roots once got it right by
//! COPYING it. The walk happening in several places is what made the CI box's failure expensive: a
//! daemon that loaded no policy, no config and NO CREDENTIALS, every venue silently on paper, with
//! one place per root to fix and every pair of them able to disagree. `vike_boot::boot` hoists the
//! order once; nothing stops the next root, or a refactor of an existing one, from spelling it out
//! again, which is what this file is for. Prose rosters (`ci_crates`, `release.yml`'s crate list,
//! the settings registry) were all true on the day and silently false a month later, so every
//! roster here is DERIVED.
//!
//! # The mechanism
//!
//! Text-only over the real `crates/**/src/**.rs` tree, comments stripped (the walk lives in
//! `common/mod.rs`, shared with `boot_journal_wiring.rs`) — the same shape as
//! `crates/vike-buildinfo/tests/identity_adoption.rs`, `crates/vike-ops/tests/architecture/layer_gate.rs` and
//! `crates/vike-ops/tests/settings_secrets/settings_registry.rs`. THREE questions:
//!
//! 1. [`only_vike_boot_runs_the_startup_sequence`] — any call to a [`SEQUENCE`] entry point (the
//!    `vike_config::` steps) must be in `vike-boot` itself or a declared [`NOT_THE_ONE_OWNER`] row.
//! 2. [`a_crate_that_boots_may_not_walk_again`] — **a crate that calls `vike_boot::boot` may not
//!    also call a [`WALK`] entry point**, anywhere under its own `src/`. Question (1) cannot see
//!    this: the desktop's rolling log file once hung off its own `project_log_dir(&cwd)` walk, which
//!    ignores `$VIKE_SETTINGS_DIR`, so under the override the log file and the block describing it
//!    named two different projects.
//! 3. [`every_public_resolver_is_classified`] — **the [`WALK`] table's set is DERIVED from
//!    `crates/vike-model/src/paths/state_path.rs` and `crates/vike-secrets/src/store_locator.rs`**: every
//!    `pub fn` there must be a [`WALK`] row or a [`NOT_A_SECOND_ANSWER`] row, so (2) bans a resolver
//!    nobody remembered to list. [`the_gate_actually_sees_the_calls`] cannot answer "is the table
//!    complete": ONE needle matching keeps it alive, and exactly one does — `vike-boot`'s own
//!    `vike_secrets::project_settings_dir_for(`, the ONE walk. A floor catches a stripper that has
//!    stopped matching; only the resolver modules' own source catches a short table.
//!
//! **(2) is scoped to BOOTING crates, deliberately, not to the workspace.** Binaries and libraries
//! that run no boot walk legitimately (`vike-backfill`'s CLI glue, `vike-mount`'s `incident` bin,
//! `vike_bridge_core::halt`, `vike-studio`, `vike-app-core`'s workspace persist): there is no second
//! answer for them to disagree with, and a workspace-wide ban would need an exemption row arguing
//! that for each — a gate whose exemption table is longer than its findings is one people learn to
//! add rows to. The property is *this process resolved the project once and then again*, a
//! per-crate question.
//!
//! # Two declared limitations
//!
//! * **A file's trailing test module is cut** ([`production_half`]): the cut is the FIRST
//!   `#[cfg(test)]` whose next item is a `mod`, so a file with an earlier inline test module is
//!   under-scanned, and [`public_fns`] inherits the cut (a `pub fn` below a trailing test block
//!   joins no roster). Cutting at the TRAILING test module instead changes a shared gate's
//!   semantics and is owed its own PR.
//! * **It matches the QUALIFIED path** (`vike_config::load(`): a `use vike_config::load;` followed
//!   by a bare `load(...)` would slip through. No file in this tree does that, and matching a bare
//!   `load(` would match half the workspace.

mod common;

use common::{BOOT_CALL, booting_crates, crate_dir, production_half, sources};

/// Does `vike-boot` itself call this entry point? The floor
/// ([`the_gate_actually_sees_the_calls`]) checks the `Called` ones, so a stripper that stopped
/// matching gives itself away; a `Reserved` one is a public function no root may reach for and
/// `boot` has no arm for today.
#[derive(PartialEq, Eq)]
enum Owned {
    Called,
    Reserved,
}

/// The entry points that MAKE UP the startup sequence, and what each one is.
///
/// `vike_config::load` is on the list even though it is the least dangerous of them: a second load
/// whose value is CONSUMED means two answers to "what is the ceiling".
const SEQUENCE: &[(&str, Owned, &str)] = &[
    (
        "vike_config::refuse_removed_env(",
        Owned::Called,
        "step 1 — refuse a REMOVED risk-ceiling variable",
    ),
    (
        "vike_config::refuse_credential_file_arming(",
        Owned::Called,
        "step 3 — refuse a credential file that ARMS REAL MONEY",
    ),
    (
        "vike_config::load_with_source(",
        Owned::Called,
        "step 4 — resolve the settings, the settings DATABASE's rows included (the boot reads the \
         store and hands the arm in). It enters the sequence here and at no root: a root that \
         opened the store privately would give one surface a set of layers the rest of the \
         process does not have, which is the whole failure this file exists for",
    ),
    (
        "vike_config::load(",
        Owned::Reserved,
        "step 4's TWO-ARGUMENT public loader, which consults no settings store: a root reaching for \
         it gets a `Settings` that silently omits the settings DATABASE every other surface applied",
    ),
    (
        "vike_config::load_with_cli(",
        Owned::Reserved,
        "step 4's CLI-overlay loader. `boot` has no CLI-overlay arm today, so a root reaching for \
         this is re-spelling step 4 with a different function and gets a second `Settings` nothing \
         else in the process can see. If a root genuinely needs the overlay, `BootSpec` grows an arm",
    ),
    ("vike_config::boot_lines(", Owned::Called, "step 6 — render the startup disclosure"),
];

/// The PROJECT WALK's entry points — every public resolver that answers "where is this project",
/// across both SURFACES that expose one: `vike_model::paths::state_path`, which owns the walk, and
/// `vike_secrets::store_locator`, which delegates into it. Two surfaces, ONE implementation
/// (`crates/vike-bridge-core/tests/settings_dir_spellings.rs` pins that only one copy exists) — and
/// a delegation is still a second call that asks the filesystem again.
///
/// A crate that has already booted has the answer; calling one of these is asking the filesystem
/// again, and the `_from`-less spellings do not even honour `$VIKE_SETTINGS_DIR`, so the second
/// answer is not merely redundant — it is the one that ignores the override. Both spellings are
/// listed because the honouring one is no better here: two calls are two chances for the working
/// directory to have moved, and the point is that the process has ONE answer to hand.
///
/// ⚠ **The whole family, not just the members somebody got wrong.** The defects this gate was
/// written against each reached for a DIFFERENT member — `project_state_dir` (twice),
/// `project_log_dir`, `project_user_data_dir_from` — which a list of the observed ones would have
/// missed.
///
/// ⚠ **"Every" is GATED rather than asserted**: [`every_public_resolver_is_classified`] derives the
/// set from [`RESOLVER_MODULES`]' own source, so a new `pub fn` there reddens this file on the PR
/// that adds it and the author decides between a row here and a [`NOT_A_SECOND_ANSWER`] row. Read
/// this table as a CLASSIFICATION of a derived roster — not as a list anybody has to keep complete
/// by hand.
const WALK: &[(&str, &str)] = &[
    ("state_path::project_settings_dir(", "<project>/settings"),
    ("state_path::project_settings_dir_from(", "<project>/settings, honouring the override"),
    ("secrets::project_settings_dir(", "<project>/settings (the vike-secrets twin)"),
    ("secrets::project_settings_dir_from(", "<project>/settings (the vike-secrets twin)"),
    (
        "secrets::project_settings_dir_for(",
        "<project>/settings (the vike-secrets twin), with the working directory itself optional — \
         THE ONE WALK `vike_boot::boot` performs. A booting crate calling this is asking the \
         filesystem again just as surely as the two rows above it",
    ),
    ("state_path::project_state_dir(", "<project>/settings/state — `Booted::state_dir`"),
    ("state_path::project_state_dir_from(", "<project>/settings/state — `Booted::state_dir`"),
    (
        "state_path::project_state_dir_from_env(",
        "<project>/settings/state, the override out of a sweep — `Booted::state_dir`. Its caller \
         is a root that boots nothing (a bridge crate's one-shot tool); a booting crate already \
         holds the answer",
    ),
    ("state_path::project_log_dir(", "<project>/settings/state/logs — `Booted::log_home`"),
    ("state_path::project_log_dir_from(", "<project>/settings/state/logs — `Booted::log_home`"),
    ("state_path::project_user_data_dir(", "<project>/user_data — `user_data_dir_beside`"),
    ("state_path::project_user_data_dir_from(", "<project>/user_data — `user_data_dir_beside`"),
    ("state_path::project_data_dir(", "<project>/market_data"),
    ("state_path::project_data_dir_from(", "<project>/market_data"),
    ("state_path::project_bin_dir(", "<project>/bin — the RUNTIME tool directory"),
    ("state_path::project_bin_dir_from(", "<project>/bin, honouring the override"),
    ("state_path::project_tmp_dir(", "<project>/tmp — the scratch directory"),
    ("state_path::project_tmp_dir_from(", "<project>/tmp, honouring the override"),
    ("state_path::project_hist_store_dir(", "<project>/market_data/hist"),
    ("state_path::project_hist_store_dir_from(", "<project>/market_data/hist"),
    ("state_path::user_rhai_strategies_dir(", "<project>/user_data/strategies/rhai"),
    ("state_path::user_rust_strategies_dir(", "<project>/user_data/strategies/rust"),
    ("state_path::user_indicators_dir(", "<project>/user_data/indicators"),
    (
        "state_path::user_plugins_dir(",
        "<project>/user_data/plugins — the BUILT cdylib artifacts a runtime-loaded Rust strategy \
         is dlopened from. It walks: `project_user_data_dir` is two rows above. ⚠ Its one caller, \
         `crates/vike-studio-core/src/run.rs`'s `plugins_dir`, is a LIBRARY reached from a BOOTED \
         root (`vike-backend backtest`), which is a shape this file's `a_crate_that_boots_may_not_\
         walk_again` cannot see — that rule's roster is the crates that CALL `vike_boot::boot`, and \
         a second walk one crate down is outside it. The residual, its measured bound and the \
         reason the cure is not in the PR that added this row are named at that function",
    ),
    ("state_path::user_runs_dir(", "<project>/user_data/runs"),
    ("state_path::user_runs_dir_from(", "<project>/user_data/runs, honouring the override"),
    ("state_path::user_studies_dir(", "<project>/user_data/research/studies — both tiers"),
    ("state_path::user_rhai_studies_dir(", "<project>/user_data/research/studies/rhai"),
    ("state_path::user_rust_studies_dir(", "<project>/user_data/research/studies/rust"),
    ("state_path::user_logs_dir(", "<project>/user_data/logs"),
    ("state_path::deployed_settings_dir(", "the deployment marker, in isolation"),
    ("state_path::workspace_root(", "the manifest chain's own root"),
];

/// The modules [`WALK`] claims to cover, each with the PREFIX its rows spell.
///
/// This is what makes the table above a CLASSIFICATION rather than a roster:
/// [`every_public_resolver_is_classified`] derives the resolver set from these files' own source
/// and requires every `pub fn` in them to be in [`WALK`] or in [`NOT_A_SECOND_ANSWER`], so a new
/// resolver reddens this gate on the PR that adds it, rather than on the day somebody happens to
/// read the two files side by side.
///
/// A module whose functions live in several files (a root that only `pub use`s its children) is
/// one row per file under ONE prefix, and the gate judges each prefix as the union of its files.
const RESOLVER_MODULES: &[(&str, &str)] = &[
    ("crates/vike-model/src/paths/state_path.rs", "state_path::"),
    ("crates/vike-model/src/paths/state_path/project_dirs.rs", "state_path::"),
    ("crates/vike-model/src/paths/state_path/project_root.rs", "state_path::"),
    ("crates/vike-secrets/src/store_locator.rs", "secrets::"),
];

/// Public functions in those modules that are NOT a second answer to "where is this project", each
/// with the reason — the other half of the classification, so a new `pub fn` cannot be silently
/// neither.
///
/// TWO families live here and the reason column says which one a row is:
///
/// * **It resolves nothing.** It takes an ALREADY-RESOLVED directory. These are the shapes a booted
///   crate is supposed to reach for — `user_data_dir_beside` exists precisely so a root can derive
///   a sibling from `Booted::settings_dir` instead of walking, and the panic message in
///   [`a_crate_that_boots_may_not_walk_again`] names it as answer (2).
/// * **It resolves the CREDENTIAL STORE** (or a path in the same settings directory), which
///   genuinely walks and which a booted crate may still call, because that is the architecture's
///   ONE accepted second resolution: `vike_boot::Credentials::LoadWith` takes the ROOT's own loader
///   as a function rather than opening the store itself, so that `vike-cli` can defer the read and
///   avoid linking `vike_bridge_core`'s ureq/tungstenite/rustls stack
///   (`crates/vike-boot/tests/dependency_floor.rs` is the gate on that). It cannot disagree with
///   the boot's answer: it is the same pure resolver under the same override.
const NOT_A_SECOND_ANSWER: &[(&str, &str)] = &[
    (
        "state_path::user_data_dir_beside",
        "resolves NOTHING — it takes the already-resolved `<project>/settings` and strips a \
         component. This is the worked example the walk panic recommends, and the reason it exists",
    ),
    (
        "state_path::bin_dir_beside",
        "resolves NOTHING — the `user_data_dir_beside` twin for `<project>/bin`, taking the \
         already-resolved `<project>/settings` and stripping a component. The desktop reaches the \
         shipped venue-baseline artifact through it (`docs/decisions/0066` decision 8) rather than \
         through `project_bin_dir_from`, which WALKS",
    ),
    (
        "state_path::imports_dir_beside",
        "resolves NOTHING — the `bin_dir_beside` twin for `<project>/market_data/imports`, the \
         archive import root, taking the already-resolved `<project>/settings` and stripping a \
         component. The data daemon mounts its import lane through it \
         (`docs/decisions/0100` verdict 2: the root is resolved once at startup from the boot's \
         settings directory) rather than walking a second time",
    ),
    (
        "state_path::read_path",
        "resolves NOTHING — `state_dir` is a parameter; it picks between that directory and a \
         legacy path for one named file",
    ),
    (
        "state_path::write_path",
        "resolves NOTHING — `state_dir` is a parameter; it creates the directory and names the file",
    ),
    (
        "secrets::load_project_secrets",
        "the CREDENTIAL STORE's LOADER, honouring `$VIKE_SETTINGS_DIR` — the settings database of \
         the directory `workspace_settings_dir_from` resolves, or an EMPTY map on a box with none \
         (the live gate); what every root's `Credentials::LoadWith` closure bottoms out in",
    ),
    // ── the settings DATABASE, `docs/decisions/0054`'s credential half ──────────────────────────
    (
        "secrets::workspace_db_path_from",
        "the CREDENTIAL STORE's DATABASE home — `<project>/settings/db/vike.db`, honouring \
         `$VIKE_SETTINGS_DIR`. It MUST be the boot's resolution (the same \
         `project_settings_dir_for` walk under the same override), because a database that \
         resolved its directory differently would serve another project's credentials — the \
         #1089/#1101 family wearing a new artifact. A second FILE, not a second ANSWER",
    ),
    (
        "secrets::db_path_for",
        "the PURE half of the row above: the working directory is a PARAMETER rather than read off \
         the process. Same resolver, same accepted case — exactly the pairing \
         `settings_dir_or_last_resort` has with `workspace_settings_dir_from`",
    ),
    (
        "secrets::settings_dir_or_last_resort",
        "resolves NOTHING NEW — it IS the shared half every path row above performs (the walk, or \
         the relative last resort), spelled once so no two of them can resolve their DIRECTORY \
         differently. The working directory is a PARAMETER, like `db_path_for`'s",
    ),
    (
        "secrets::workspace_settings_dir_from",
        "the row above with the working directory read here — the front door a BINARY calls, and \
         the same shape `workspace_db_path_from` has. A booting crate reaches it ONLY when its \
         boot resolved no settings directory at all — in which case this resolver, the same \
         `project_settings_dir_for` the boot itself called under the same `None`, cannot answer \
         differently",
    ),
    (
        "secrets::db_path_in",
        "resolves NOTHING — it takes the already-resolved `<project>/settings` and joins the \
         database's path, and it is the ONE spelling of the database's shape below the \
         settings directory. `db_path_for` is this function over the walk's answer and \
         `crate::store::backend_in` is it over a directory a caller handed in, which is what makes \
         the reader and the writer structurally incapable of disagreeing about where the database is",
    ),
];

/// Crates that BOOT and still walk, each with its reason. Empty today, and that is the finding: every
/// site that stood here was FIXED rather than exempted, which is what the panic message below asks
/// for first.
const WALKS_ANYWAY: &[(&str, &str)] = &[];

/// Files outside `vike-boot` that legitimately call a [`SEQUENCE`] entry point, each with its
/// reason.
///
/// A row is an argued exemption, not a debt: unlike `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s
/// `LIBRARY_PIN`, there is no expectation that this list drains.
const NOT_THE_ONE_OWNER: &[(&str, &str)] = &[
    (
        "crates/vike-cli/src/cmd/config/check.rs",
        "the validating PRE-FLIGHT (`vike-cli config check`, what the shipped `deploy/*.service` units \
     put in their `ExecStartPre=`). It re-runs the refusal in order to REPORT it as one finding \
     with an exit code, not in order to obey it — the dispatcher above it has already booted \
     through `vike_boot::boot` and a real run would have exited there first. A checker that could \
     not call the thing it checks would be checking a re-implementation.",
    ),
    (
        "crates/vike-tradehub/src/hot_reload.rs",
        "the RUNTIME hot-apply path (REQ-7 write, split-plane): a `SetSetting` that names a hot-classed \
     key re-reads the settings ON THE DAEMON'S SUMMARY TICK, long after the boot, in \
     order to APPLY the operator's edit — that is the feature, not a second startup. It re-walks \
     nothing: the settings DIRECTORY it reads is the one `vike_boot::boot` already resolved and \
     handed it (`SettingsShowSource`'s `settings_dir`), so the one-walk property this gate exists \
     to protect is untouched — what would violate it is resolving the directory again, which this \
     file must never do. `policy` is refused before the table is consulted, so no ceiling can ever \
     ride this path. It reads the settings DATABASE like the boot (`load_with_source`): an \
     exemption argued on the WALK says nothing about the LAYER SET.",
    ),
    (
        "crates/vike-app-core/src/ui/tool_views/venues.rs",
        "the Data Manager's arming SCREEN, reading the `policy.venues.<venue>` rows to RENDER it. \
     `reload_venue_ceilings` re-reads the ceilings every frame that tab is visible so the Mode \
     column shows what the STORE says now rather than what this process loaded at boot — \
     without it, an operator who flips a switch watches the row not change and concludes the \
     screen is broken. It re-walks NOTHING: the settings directory is a `&Path` PARAMETER, \
     threaded from the desktop's `SETTINGS_DIR` (i.e. `vike_boot::Booted::settings_dir`), so this \
     function structurally CANNOT produce a second answer to `where is the project` — which is the \
     property this gate exists to protect. \
     ⚠ It reads the CEILINGS, which `hot_reload.rs`'s row above is careful to say it never does — \
     and that difference is the point: this path DISPLAYS them and applies nothing. Every policy \
     key is `HotClass::Restart`, the mount reads a venue's ceiling once at `make_engine`, and the \
     row renders that restart requirement in words beside the value. Nothing here changes what the \
     RUNNING process is doing. It reads the settings DATABASE like the boot (`load_with_source`).",
    ),
    (
        "crates/vike-cli/src/cmd/config/retired_env.rs",
        "the deploy pre-flight's JUDGE (`vike-cli config retired-env`, what \
     `deploy/sbin/vike-trader-ci-deploy`'s `retired_env_report` feeds a roster unit's MERGED \
     environment through before a release is installed). It calls `vike_config::refuse_removed_env` \
     on a map it read off STDIN — a systemd unit's declared `Environment=`, `EnvironmentFile=` \
     contents and `/proc/<pid>/environ`, none of which is THIS PROCESS's own environment — and \
     prints the verdict rather than obeying it: there is no settings load, no window, no venue \
     mount on either side of the call, only a pure judgement of somebody else's prospective \
     environment. A verb whose entire job is to answer 'would the boot sequence refuse this' must \
     be able to call the function the boot sequence calls, or it is judging a re-implementation.",
    ),
    (
        "crates/vike-backtest/src/backtest_cli/serve.rs",
        "the COMPUTE daemon's `--addr` arm (`daemon_before_logging`, through \
     `load_backtest_settings`, before its log subscriber) — a \
     composition root of its own, and one that runs no `vike_boot::boot`: it performs its own \
     `project_settings_dir_from` walk, resolves its settings, and opens the socket it serves on. \
     It reads the settings DATABASE like the boot (`load_with_source`).",
    ),
];

#[test]
fn only_vike_boot_runs_the_startup_sequence() {
    let exempt: Vec<&str> = NOT_THE_ONE_OWNER.iter().map(|(p, _)| *p).collect();
    let mut bad: Vec<String> = Vec::new();
    for (rel, text) in sources() {
        if rel.starts_with("crates/vike-boot/") || exempt.contains(&rel.as_str()) {
            continue;
        }
        let code = production_half(&text);
        for (needle, _, what) in SEQUENCE {
            if code.contains(needle) {
                bad.push(format!("  {rel}\n      calls {needle}…)  — {what}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a composition root is re-spelling the startup sequence instead of running \
         `vike_boot::boot`:\n{}\n\n\
         The ORDER is load-bearing and subtle — the settings must load BEFORE `vike_log::init`, \
         because the log directory and both log levels are themselves settings — and it is not the \
         order that goes wrong: it is the SETTINGS WALK happening more than once. On the CI box the walk \
         answered with an unrelated directory and a daemon ran with no policy and NO CREDENTIALS, \
         every venue silently on paper; in the desktop a second walk put the rolling log file under \
         one project while the block describing it named another.\n\n\
         Two legitimate responses:\n  \
         1. call `vike_boot::boot` and declare how this root DIFFERS — every departure is an enum \
         arm carrying its reason (`RemovedEnv::Ignore`, `SettingsLoad::Skip`, \
         `Credentials::Deferred`, `LogHome::Elsewhere`, `Disclosure::Skip`);\n  \
         2. if this genuinely is not a startup — a checker that must call what it checks, say — add \
         the path to NOT_THE_ONE_OWNER with the argument.\n\
         Widening `vike-boot` to make the call disappear is neither.",
        bad.join("\n")
    );
}

/// **A crate that runs the boot may not resolve the project again.** The half
/// [`only_vike_boot_runs_the_startup_sequence`] cannot see — this module's doc, question (2).
#[test]
fn a_crate_that_boots_may_not_walk_again() {
    let all = sources();
    let exempt: Vec<&str> = WALKS_ANYWAY.iter().map(|(c, _)| *c).collect();

    // The roster is DERIVED: whichever crates call `vike_boot::boot`, the next root included.
    let booting = booting_crates(&all);

    let mut bad: Vec<String> = Vec::new();
    for (rel, text) in &all {
        let dir = crate_dir(rel);
        if !booting.contains(&dir) || exempt.contains(&dir.as_str()) {
            continue;
        }
        let code = production_half(text);
        for (needle, what) in WALK {
            if code.contains(needle) {
                bad.push(format!("  {rel}\n      calls …{needle}…)  — {what}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "a crate that runs `vike_boot::boot` resolves the project a SECOND time:\n{}\n\n\
         The boot already answered. The `_from`-less spellings do not even honour \
         `$VIKE_SETTINGS_DIR`, so the second answer is the one that ignores the override — which is \
         the whole failure: on the CI box the daemon's state root, log home and `alerts.json` agreed \
         with its settings only because `WorkingDirectory=` happened to equal the overridden \
         directory, and in the desktop the rolling log file landed under one project while the \
         block describing it named another.\n\n\
         Three answers, in order of preference:\n  \
         1. use what the boot returned — `Booted::settings_dir`, `Booted::state_dir`, \
         `Booted::log_home`;\n  \
         2. derive from it with a function that takes the RESOLVED directory instead of walking \
         (`vike_model::paths::state_path::user_data_dir_beside` is the worked example, and the reason it \
         exists);\n  \
         3. if this genuinely must resolve for itself, add the crate to WALKS_ANYWAY with the \
         argument — and say in the CODE, at the site, why one process needs two answers.\n\
         Deleting the boot call to make the finding disappear is none of the three.",
        bad.join("\n")
    );
}

/// A row that no longer applies is deleted, not left to rot — the same both-directions discipline
/// `crates/vike-buildinfo/tests/identity_adoption.rs`'s `no_stale_exemptions` applies.
#[test]
fn no_stale_exemptions() {
    let all = sources();
    let mut stale: Vec<String> = Vec::new();
    for (rel, why) in NOT_THE_ONE_OWNER {
        let Some((_, text)) = all.iter().find(|(p, _)| p == rel) else {
            stale.push(format!("  {rel} — the file no longer exists ({why})"));
            continue;
        };
        let code = production_half(text);
        if !SEQUENCE.iter().any(|(needle, _, _)| code.contains(needle)) {
            stale.push(format!("  {rel} — no longer calls any sequence entry point"));
        }
    }
    // …and the same for the WALK half, which is keyed on a crate DIRECTORY rather than a file.
    for (dir, why) in WALKS_ANYWAY {
        let files: Vec<&(String, String)> =
            all.iter().filter(|(p, _)| crate_dir(p) == *dir).collect();
        if files.is_empty() {
            stale.push(format!("  {dir} — the crate no longer exists ({why})"));
            continue;
        }
        let walks = files
            .iter()
            .any(|(_, t)| WALK.iter().any(|(needle, _)| production_half(t).contains(needle)));
        if !walks {
            stale.push(format!("  {dir} — no longer walks; delete the WALKS_ANYWAY row ({why})"));
        }
    }
    assert!(
        stale.is_empty(),
        "NOT_THE_ONE_OWNER names files that no longer need the exemption — delete these \
         lines:\n{}",
        stale.join("\n")
    );
}

/// A floor, not a count: this gate is textual, so a stripper or a walker that quietly stopped
/// matching anything would pass both assertions above by seeing nothing at all.
#[test]
fn the_gate_actually_sees_the_calls() {
    let all = sources();
    assert!(all.len() >= 400, "only {} source files walked — the walker is broken", all.len());
    let (_, boot) = all
        .iter()
        .find(|(p, _)| p == "crates/vike-boot/src/lib.rs")
        .expect("vike-boot's lib must be walked");
    let code = production_half(boot);
    let missing: Vec<&str> = SEQUENCE
        .iter()
        .filter(|(_, owned, _)| *owned == Owned::Called)
        .map(|(n, _, _)| *n)
        .filter(|n| !code.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "vike-boot does not appear to call {missing:?} — either the sequence changed (update \
         SEQUENCE) or the comment stripper is eating real code"
    );
    // The WALK half's floor: `vike-boot` performs the ONE walk, so at least one WALK needle must
    // match inside it. A needle table that matched nothing anywhere would let
    // `a_crate_that_boots_may_not_walk_again` pass by seeing nothing at all.
    assert!(
        WALK.iter().any(|(n, _)| code.contains(n)),
        "vike-boot does not appear to perform the project walk at all — the WALK needles no longer \
         match the resolver's real spelling, so the second half of this gate is inert"
    );
    // …and the roster it keys on is non-empty: every composition root must be found by BOOT_CALL.
    let booting = booting_crates(&all);
    // ⚠ It counts CRATES (`booting_crates` dedups), from production code only: four today —
    // vike-cli, vike-datahub, vike-desktop, vike-tradehub. The floor is a `>=` so a FIFTH root
    // joining needs no edit here; only a root LEAVING does, and that edit has to be deliberate
    // precisely because "a root retired" and "the anchor stopped matching real code" look
    // identical from inside this assertion.
    assert!(
        booting.len() >= 4,
        "only {} crate(s) found calling `{BOOT_CALL}` ({booting:?}) — four composition roots run \
         the sequence, so a smaller number means the anchor no longer matches and the walk gate \
         checks nobody",
        booting.len()
    );
    // …and the exemption is genuinely exercised, so the exempt path is not silently dead.
    let (_, check) = all
        .iter()
        .find(|(p, _)| p == NOT_THE_ONE_OWNER[0].0)
        .expect("the declared exemption's file must be walked");
    assert!(
        SEQUENCE.iter().any(|(n, _, _)| production_half(check).contains(n)),
        "the one declared exemption calls nothing — see `no_stale_exemptions`"
    );
}

/// Every top-level `pub fn` NAME in a resolver module — the DERIVED roster
/// [`every_public_resolver_is_classified`] checks the tables against.
///
/// Comments stripped and the trailing test module cut, like every other read in this file. It takes
/// `trim_start()`, so an inherent method would join the roster too: neither module has one today,
/// and being made to classify one costs a row while missing a free function costs the property.
fn public_fns(text: &str) -> Vec<String> {
    production_half(text)
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("pub fn "))
        .map(|rest| rest.split(['(', '<']).next().unwrap_or_default().trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// **[`WALK`]'s set is DERIVED from the resolver modules' own source, so the table cannot be
/// short.** This module's doc, question (3).
///
/// Both directions, because a table can be wrong either way: a `pub fn` in neither table is an
/// UNCLASSIFIED resolver (the drift), and a row naming a `pub fn` that is no longer there is a DEAD
/// needle that silently bans nothing (the rot a rename leaves behind).
#[test]
fn every_public_resolver_is_classified() {
    let all = sources();
    let mut unclassified: Vec<String> = Vec::new();
    let mut dead: Vec<String> = Vec::new();
    for (i, (_, prefix)) in RESOLVER_MODULES.iter().enumerate() {
        if RESOLVER_MODULES[..i].iter().any(|(_, p)| p == prefix) {
            continue;
        }
        let files: Vec<&str> =
            RESOLVER_MODULES.iter().filter(|(_, p)| p == prefix).map(|(f, _)| *f).collect();
        let path = files.join(" + ");
        let mut names = Vec::new();
        for file in &files {
            let (_, text) = all.iter().find(|(p, _)| p == file).unwrap_or_else(|| {
                panic!(
                    "{file} is not walked — RESOLVER_MODULES names a file that has moved or gone"
                )
            });
            names.extend(public_fns(text));
        }
        // A floor of the same kind as `the_gate_actually_sees_the_calls`: an extractor that stopped
        // matching would report an EMPTY roster, and an empty roster is classified by construction.
        assert!(
            names.len() >= 5,
            "only {} public functions found in {path} — `public_fns` no longer matches the \
             module's spelling, so this gate would pass by seeing nothing",
            names.len()
        );
        for name in &names {
            let banned = format!("{prefix}{name}(");
            let excused = format!("{prefix}{name}");
            if WALK.iter().any(|(n, _)| banned == *n)
                || NOT_A_SECOND_ANSWER.iter().any(|(n, _)| excused == *n)
            {
                continue;
            }
            unclassified.push(format!("  {prefix}{name}   ({path})"));
        }
        for (needle, _) in WALK.iter().filter(|(n, _)| n.starts_with(prefix)) {
            let name = needle.strip_prefix(prefix).unwrap_or(needle).trim_end_matches('(');
            if !names.iter().any(|n| n == name) {
                dead.push(format!("  WALK row `{needle}` — {path} has no `pub fn {name}`"));
            }
        }
        for (row, _) in NOT_A_SECOND_ANSWER.iter().filter(|(n, _)| n.starts_with(prefix)) {
            let name = row.strip_prefix(prefix).unwrap_or(row);
            if !names.iter().any(|n| n == name) {
                dead.push(format!(
                    "  NOT_A_SECOND_ANSWER row `{row}` — {path} has no `pub fn {name}`"
                ));
            }
        }
    }
    assert!(
        unclassified.is_empty(),
        "a public resolver is in NEITHER table, so `a_crate_that_boots_may_not_walk_again` does \
         not ban it:\n{}\n\n\
         `WALK` says it lists every public resolver that answers \"where is this project\", and a \
         list somebody has to remember to extend is exactly the shape this repo has watched rot — \
         four separate PRs added a resolver here and none of them touched the table. Classify each \
         name:\n  \
         1. it resolves the project ⇒ add a `WALK` row. Adding it may turn \
         `a_crate_that_boots_may_not_walk_again` red, and that finding is the point of the row;\n  \
         2. it takes an already-resolved directory, or it is the CREDENTIAL STORE (the one second \
         resolution `vike_boot::Credentials::LoadWith` accepts by design) ⇒ add a \
         `NOT_A_SECOND_ANSWER` row saying which.\n\
         Deleting the module from RESOLVER_MODULES to make the finding go away is neither.",
        unclassified.join("\n")
    );
    assert!(
        dead.is_empty(),
        "a table row names a function that no longer exists — a needle that matches nothing bans \
         nothing, which is how half a ban list goes quiet after a rename:\n{}",
        dead.join("\n")
    );
}
