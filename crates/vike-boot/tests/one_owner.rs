//! **The startup sequence has ONE owner, and that is a gate rather than a convention.**
//!
//! # Why a gate
//!
//! Five composition roots ran the same ordered steps from five copies. That is not a tidiness
//! complaint: the ORDER is load-bearing and subtle (settings must load before `vike_log::init`,
//! because the log directory and both log levels are themselves settings), every root got it right
//! by COPYING, and the walk happening in five places is precisely what made the CI box's failure
//! expensive — a daemon that loaded no policy, no config and NO CREDENTIALS, every venue silently
//! on paper, with five places to fix and five chances for two of them to disagree.
//!
//! `crates/vike-desktop/src/main.rs` was already living that: its rolling log file hung off a
//! SECOND `project_log_dir(&cwd)` walk, which does not honour `$VIKE_SETTINGS_DIR`, so under the
//! override the log file and the block describing it named two different projects.
//!
//! Hoisting the order fixes it once. Nothing stops the sixth root — or a refactor of one of the
//! five — from spelling it out again, which is what this file is for. The repo's own experience is
//! the argument: `ci_crates`, `release.yml`'s crate list and the settings registry were all rosters
//! written in prose, true on the day and silently false a month later.
//!
//! # The mechanism
//!
//! Text-only over the real `crates/**/src/**.rs` tree, comments stripped — the same shape as
//! `crates/vike-buildinfo/tests/identity_adoption.rs`, `crates/vike-ops/tests/layer_gate.rs` and
//! `crates/vike-ops/tests/settings_registry.rs`. THREE questions — two because the property
//! above has two halves and the first cut of this file only asked one of them, and a third
//! because the second half is aimed by a TABLE, and a hand-written table of "every resolver"
//! is itself a thing this repo has watched rot:
//!
//! 1. [`only_vike_boot_runs_the_startup_sequence`] — any call to a [`SEQUENCE`] entry point (the
//!    `vike_config::` steps) must be in `vike-boot` itself or a declared [`NOT_THE_ONE_OWNER`] row.
//! 2. [`a_crate_that_boots_may_not_walk_again`] — **a crate that calls `vike_boot::boot` may not
//!    also call a [`WALK`] entry point**, anywhere under its own `src/`.
//! 3. [`every_public_resolver_is_classified`] — **the [`WALK`] table's own set is DERIVED from
//!    `crates/vike-model/src/state_path.rs` and `crates/vike-secrets/src/dotenv.rs`**, so (2) bans
//!    a resolver nobody remembered to list. Every `pub fn` in those modules must be a [`WALK`] row
//!    or a [`NOT_A_SECOND_ANSWER`] row: the table is a CLASSIFICATION of a derived set, not a
//!    roster.
//!
//! ⚠ **(2) is the half that was MISSING, and its absence was measured.** `SEQUENCE` listed only the
//! four `vike_config::` functions, so restoring `vike-app`'s original defect verbatim — the rolling
//! log file hanging off its own `project_log_dir(&cwd)` walk — left this file at `3 passed;
//! 0 failed`. The module doc above calls the walk the load-bearing property and the gate could not
//! see it.
//!
//! ⚠ **(3) exists because (2)'s table was SHORT, and the shortness was measured.** [`WALK`] said in
//! its own doc that it listed "every public resolver that answers 'where is this project'", and by
//! 2026-08-24 it was missing EIGHT of the twenty-six in `state_path.rs`: `project_bin_dir` and its
//! `_from` twin (#1457), `project_tmp_dir` and its twin (#1487), `user_runs_dir`, `user_studies_dir`
//! and its two tier resolvers. Four separate branches added a resolver to that module and none of
//! them touched this file, which is the ordinary outcome — nothing pointed here.
//!
//! ⚠ **Nothing in the gate could have caught it, and the reason is worth stating**, because it is
//! the shape a floor takes when it is mistaken for a completeness check.
//! [`the_gate_actually_sees_the_calls`] asserts `WALK.iter().any(…)` over `vike-boot`'s own source:
//! ONE needle matching keeps the whole table alive — and exactly one does, `vike-boot`'s own
//! `vike_secrets::project_settings_dir_from(`, the ONE walk. So the table could have been cut
//! to that single row and this file would still have reported `4 passed; 0 failed`. That
//! assertion is doing its stated job — it is a floor against a stripper that has stopped
//! matching anything — and a floor cannot answer "is this list complete". Only the resolver
//! module's own source can, which is what (3) reads.
//!
//! ⚠ **(2) is scoped to BOOTING crates, deliberately, and not to the workspace.** Ten binaries and
//! libraries walk perfectly legitimately (`vike-backfill`'s CLI glue, `vike-run`'s bins,
//! `vike_bridge_core::halt`, `vike-studio`, `vike-app-core`'s workspace persist): they run no boot,
//! so there is no second answer for them to disagree with. A workspace-wide ban would need ten
//! exemption rows arguing that, and a gate whose exemption table is longer than its findings is one
//! people learn to add rows to. The property is specifically *this process resolved the project
//! once and then resolved it again* — which is a per-crate question.
//!
//! # Two declared limitations
//!
//! * **A file's trailing test module is cut** ([`production_half`]). Four files drive the REAL
//!   `vike_config::load` from a `#[cfg(test)] mod tests` — which is exactly right, and demanding a
//!   row for each would teach people to write exceptions rather than to read them. The cut is the
//!   FIRST `#[cfg(test)]` whose next item is a `mod`, so a file with an earlier inline test module
//!   is under-scanned; [`the_gate_actually_sees_the_calls`] is the floor that notices a stripper
//!   which has stopped seeing anything at all. ⚠ [`public_fns`] inherits the same cut, so a `pub fn`
//!   added BELOW a module's trailing test block would not join (3)'s derived roster. Measured
//!   2026-08-24: a probe appended to the END of `state_path.rs` was invisible, the same probe
//!   inserted above the test module was named. Production code does not live under a trailing
//!   `mod tests`, and reading the whole file instead would put every `pub fn` test helper on the
//!   roster and teach people to write exception rows — which is the trade this cut already made.
//! * **It matches the QUALIFIED path** (`vike_config::load(`). A `use vike_config::load;` followed
//!   by a bare `load(...)` would slip through. No file in this tree does that, and matching a bare
//!   `load(` would match half the workspace.

use std::path::{Path, PathBuf};

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
/// whose value is CONSUMED means two answers to "what is the ceiling", which is the class of bug
/// the ceilings are file-only to avoid.
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
    ("vike_config::load(", Owned::Called, "step 4 — load <project>/settings/*.toml"),
    (
        "vike_config::load_with_cli(",
        Owned::Reserved,
        "step 4's OTHER public loader — `vike_config`'s second entry point, which `load` itself is \
         written in terms of. `boot` has no CLI-overlay arm today, so a root reaching for this is \
         re-spelling step 4 with a different function and gets a second `Settings` nothing else in \
         the process can see. If a root genuinely needs the overlay, `BootSpec` grows an arm",
    ),
    ("vike_config::boot_lines(", Owned::Called, "step 6 — render the startup disclosure"),
];

/// The PROJECT WALK's entry points — every public resolver that answers "where is this project",
/// across both copies of the resolver (`vike_model::state_path` and the zero-dependency
/// `vike_secrets` twin, pinned equal by
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs`).
///
/// A crate that has already booted has the answer; calling one of these is asking the filesystem
/// again, and the `_from`-less spellings do not even honour `$VIKE_SETTINGS_DIR`, so the second
/// answer is not merely redundant — it is the one that ignores the override. Both spellings are
/// listed because the honouring one is no better here: two calls are two chances for the working
/// directory to have moved, and the point is that the process has ONE answer to hand.
///
/// ⚠ **The whole family, not just the members somebody got wrong.** Each of the four defects this
/// PR fixed reached for a DIFFERENT member — `project_state_dir` (twice), `project_log_dir`,
/// `project_user_data_dir_from` — which is exactly what a list of the observed ones would have
/// missed.
///
/// ⚠ **"Every" is now GATED rather than asserted**, because between #1226 and 2026-08-24 it was
/// simply false: eight resolvers had joined `state_path.rs` across four branches and none of them
/// joined this table. [`every_public_resolver_is_classified`] derives the set from
/// [`RESOLVER_MODULES`]' own source, so a new `pub fn` there reddens this file on the PR that adds
/// it and the author decides between a row here and a [`NOT_A_SECOND_ANSWER`] row. Read this table
/// as a CLASSIFICATION of a derived roster — not as a list anybody has to keep complete by hand.
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
    ("state_path::user_runs_dir(", "<project>/user_data/runs"),
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
const RESOLVER_MODULES: &[(&str, &str)] = &[
    ("crates/vike-model/src/state_path.rs", "state_path::"),
    ("crates/vike-secrets/src/dotenv.rs", "secrets::"),
];

/// Public functions in those modules that are NOT a second answer to "where is this project", each
/// with the reason — the other half of the classification, so a new `pub fn` cannot be silently
/// neither.
///
/// TWO families live here and the reason column says which one a row is:
///
/// * **It resolves nothing.** It takes an ALREADY-RESOLVED directory, or it parses text. These are
///   the shapes a booted crate is supposed to reach for — `user_data_dir_beside` exists precisely so
///   a root can derive a sibling from `Booted::settings_dir` instead of walking, and the panic
///   message in [`a_crate_that_boots_may_not_walk_again`] names it as answer (2).
/// * **It resolves the CREDENTIAL STORE**, which genuinely walks and which a booted crate may still
///   call, because that is the architecture's ONE accepted second resolution:
///   `vike_boot::Credentials::LoadWith` takes the ROOT's own loader as a function rather than
///   opening the store itself, so that `vike-cli` can defer the read and avoid linking
///   `vike_bridge_core`'s ureq/tungstenite/rustls stack (`crates/vike-boot/tests/dependency_floor.rs`
///   is the gate on that). It cannot disagree with the boot's answer: it is the same pure resolver
///   under the same override, pinned equal by
///   `crates/vike-bridge-core/tests/settings_dir_spellings.rs`.
const NOT_A_SECOND_ANSWER: &[(&str, &str)] = &[
    (
        "state_path::user_data_dir_beside",
        "resolves NOTHING — it takes the already-resolved `<project>/settings` and strips a \
         component. This is the worked example the walk panic recommends, and the reason it exists",
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
    ("secrets::parse_dotenv", "resolves NOTHING — a pure `KEY=VALUE` parser over a &str"),
    (
        "secrets::project_secrets_path",
        "the CREDENTIAL STORE — `<project>/settings/secrets.env`, the read `Credentials::LoadWith` \
         leaves to the root's own loader",
    ),
    (
        "secrets::project_secrets_path_from",
        "the CREDENTIAL STORE, honouring `$VIKE_SETTINGS_DIR` — the same accepted second resolution",
    ),
    (
        "secrets::workspace_dotenv_path",
        "the CREDENTIAL STORE from the working directory. ⚠ The override-BLIND spelling, and \
         `crates/vike-cli/src/cmd/secrets.rs`'s `store_path` is the one booting-crate call site: it \
         is reached ONLY when the dispatcher's boot resolved no settings directory at all, and in \
         that case this resolver — the same function the boot itself called, under the same \
         `None` — cannot answer differently",
    ),
    (
        "secrets::workspace_dotenv_path_from",
        "the CREDENTIAL STORE from the working directory, honouring `$VIKE_SETTINGS_DIR` — the \
         form `vike_bridge_core::credentials::load_workspace_secrets_from_env` calls with the \
         value out of the root's own `std::env::vars()` sweep",
    ),
    (
        "secrets::workspace_node_path_from",
        "the NODE-KEY store — `<project>/settings/node.env`, honouring `$VIKE_SETTINGS_DIR`. The \
         credential-store row above with one filename changed: the SAME `project_settings_dir_for` \
         walk through the same `settings_file_path_for`, the same override PARAMETER, and every \
         caller hands it the root's own `settings_dir_override` — so like its twin it cannot answer \
         differently from the boot that resolved that value. ⚠ It is a second FILE, not a second \
         ANSWER, and the distinction is the whole of \
         `docs/decisions/0051-node-keys-live-in-their-own-store.md`: two disjoint, statically-known \
         name sets (`vike_model::credential_keys::is_platform_key` decides which) live in two \
         files, and no name is ever looked for in both. What this table exists to catch — one \
         question with two possible answers — is what that design forbids too",
    ),
    (
        "secrets::node_path_for",
        "the PURE half of the row above: the working directory is a PARAMETER rather than read off \
         the process, so a test reaches every arm without `set_current_dir`. Same resolver, same \
         accepted case",
    ),
    (
        "secrets::load_workspace_dotenv",
        "the CREDENTIAL STORE's LOADER — `workspace_dotenv_path` plus the parse",
    ),
    (
        "secrets::load_workspace_dotenv_from",
        "the CREDENTIAL STORE's LOADER, honouring `$VIKE_SETTINGS_DIR` — what every root's \
         `Credentials::LoadWith` closure bottoms out in",
    ),
];

/// The call that MAKES a crate a booting one — the anchor [`a_crate_that_boots_may_not_walk_again`]
/// keys on, so the roster is derived from the tree rather than written down here (this repo's
/// rosters-in-prose all rotted; the root `CLAUDE.md` keeps a list of which).
const BOOT_CALL: &str = "vike_boot::boot(";

/// Crates that BOOT and still walk, each with its reason. Empty today, and that is the finding: the
/// four sites that stood here — `vike-app`'s `state_dir_path` and `install_user_indicators`,
/// `vike-tradehub`'s `state_dir`, `vike-cli`'s and `vike-datahub`'s `user_data` resolution — were
/// FIXED rather than exempted, which is what the panic message below asks for first.
const WALKS_ANYWAY: &[(&str, &str)] = &[];

/// Files outside `vike-boot` that legitimately call a [`SEQUENCE`] entry point, each with its
/// reason.
///
/// A row is an argued exemption, not a debt: unlike `crates/vike-ops/tests/settings_registry.rs`'s
/// `LIBRARY_PIN`, there is no expectation that this list drains.
const NOT_THE_ONE_OWNER: &[(&str, &str)] = &[
    (
        "crates/vike-cli/src/cmd/config_check.rs",
        "the validating PRE-FLIGHT (`vike-cli config check`, what the shipped `deploy/*.service` units \
     put in their `ExecStartPre=`). It re-runs the refusal in order to REPORT it as one finding \
     with an exit code, not in order to obey it — the dispatcher above it has already booted \
     through `vike_boot::boot` and a real run would have exited there first. A checker that could \
     not call the thing it checks would be checking a re-implementation.",
    ),
    (
        "crates/vike-tradehub/src/hot_reload.rs",
        "the RUNTIME hot-apply path (REQ-7 write, split-plane): a `SetSetting` that names a hot-classed \
     key re-reads the settings from disk ON THE DAEMON'S SUMMARY TICK, long after the boot, in \
     order to APPLY the operator's edit — that is the feature, not a second startup. It re-walks \
     nothing: the settings DIRECTORY it reads is the one `vike_boot::boot` already resolved and \
     handed it (`SettingsShowSource`'s `settings_dir`), so the one-walk property this gate exists \
     to protect is untouched — what would violate it is resolving the directory again, which this \
     file must never do. `policy` is refused before the table is consulted, so no ceiling can ever \
     ride this path.",
    ),
    (
        "crates/vike-app-core/src/tool_views/venues.rs",
        "the Data Manager's arming SCREEN, reading `policy.toml` to RENDER it. `reload_venue_ceilings` \
     re-reads the ceilings every frame that tab is visible so the Mode column shows what the FILE \
     says now rather than what this process loaded at boot — without it, an operator who flips a \
     switch watches the row not change and concludes the screen is broken. It re-walks NOTHING: \
     the settings directory is a `&Path` PARAMETER, threaded from `vike-app`'s `SETTINGS_DIR` \
     (i.e. `vike_boot::Booted::settings_dir`), so this function structurally CANNOT produce a \
     second answer to `where is the project` — which is the property this gate exists to protect. \
     ⚠ It reads the CEILINGS, which `hot_reload.rs`'s row above is careful to say it never does — \
     and that difference is the point: this path DISPLAYS them and applies nothing. Every policy \
     key is `HotClass::Restart`, the mount reads a venue's ceiling once at `make_engine`, and the \
     row renders that restart requirement in words beside the value. Nothing here changes what the \
     RUNNING process is doing.",
    ),
];

/// Workspace root, resolved from `CARGO_MANIFEST_DIR` (never CWD) — the idiom every text gate in
/// this repo uses.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Every `.rs` file under `crates/**/src/`, as `(repo-relative path, text)`.
///
/// The two vendored trees the root manifest EXCLUDES from the workspace are skipped: neither is
/// ours and neither could call a vike function.
fn sources() -> Vec<(String, String)> {
    let root = repo_root();
    let mut out = Vec::new();
    walk(&root.join("crates"), &root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            // `target/` in a crate directory, and the two vendored trees.
            if matches!(name, "target" | "vendor" | "protogen") {
                continue;
            }
            walk(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            if !rel.contains("/src/") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                out.push((rel, text));
            }
        }
    }
}

/// Line comments removed — so a `//!` module doc or a `//` note NAMING one of these functions is
/// prose, not a call. The same stripper `crates/vike-buildinfo/tests/identity_adoption.rs` uses.
fn strip_comments(text: &str) -> String {
    text.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The file with its trailing `#[cfg(test)] mod …` block cut off — see the module doc's first
/// declared limitation.
fn production_half(text: &str) -> String {
    let clean = strip_comments(text);
    let lines: Vec<&str> = clean.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() != "#[cfg(test)]" {
            continue;
        }
        let next = lines[i + 1..].iter().find(|l| !l.trim().is_empty());
        if next.is_some_and(|l| l.trim_start().starts_with("mod ")) {
            return lines[..i].join("\n");
        }
    }
    clean
}

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
         every venue silently on paper; in `vike-app` a second walk put the rolling log file under \
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

/// The `crates/<name>` directory prefix of a repo-relative source path.
fn crate_dir(rel: &str) -> String {
    // `crates/vike-desktop/src/…` and `crates/bridges/aster/src/…` — the bridges tree is one
    // level deeper, and either way the CRATE is everything above `/src/`.
    match rel.split_once("/src/") {
        Some((dir, _)) => dir.to_string(),
        None => rel.to_string(),
    }
}

/// **A crate that runs the boot may not resolve the project again.** The half
/// [`only_vike_boot_runs_the_startup_sequence`] cannot see — see this module's doc for the
/// measurement that proved it blind.
#[test]
fn a_crate_that_boots_may_not_walk_again() {
    let all = sources();
    let exempt: Vec<&str> = WALKS_ANYWAY.iter().map(|(c, _)| *c).collect();

    // The roster is DERIVED: whichever crates call `vike_boot::boot`, including the sixth root.
    let booting: Vec<String> = all
        .iter()
        .filter(|(rel, text)| {
            !rel.starts_with("crates/vike-boot/") && production_half(text).contains(BOOT_CALL)
        })
        .map(|(rel, _)| crate_dir(rel))
        .collect();

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
         directory, and in `vike-app` the rolling log file landed under one project while the \
         block describing it named another.\n\n\
         Three answers, in order of preference:\n  \
         1. use what the boot returned — `Booted::settings_dir`, `Booted::state_dir`, \
         `Booted::log_home`;\n  \
         2. derive from it with a function that takes the RESOLVED directory instead of walking \
         (`vike_model::state_path::user_data_dir_beside` is the worked example, and the reason it \
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
    // `a_crate_that_boots_may_not_walk_again` pass by seeing nothing at all — which is the exact
    // failure mode that let the missing half of this gate ship green.
    assert!(
        WALK.iter().any(|(n, _)| code.contains(n)),
        "vike-boot does not appear to perform the project walk at all — the WALK needles no longer \
         match the resolver's real spelling, so the second half of this gate is inert"
    );
    // …and the roster it keys on is non-empty: every composition root must be found by BOOT_CALL.
    let booting: Vec<String> = all
        .iter()
        .filter(|(rel, text)| {
            !rel.starts_with("crates/vike-boot/") && production_half(text).contains(BOOT_CALL)
        })
        .map(|(rel, _)| crate_dir(rel))
        .collect();
    assert!(
        booting.len() >= 5,
        "only {} crate(s) found calling `{BOOT_CALL}` ({booting:?}) — five composition roots run \
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
/// short.** The half this gate was missing — see the module doc's measurement of what the omission
/// cost.
///
/// Both directions, because a table can be wrong either way: a `pub fn` in neither table is an
/// UNCLASSIFIED resolver (the drift), and a row naming a `pub fn` that is no longer there is a DEAD
/// needle that silently bans nothing (the rot a rename leaves behind).
#[test]
fn every_public_resolver_is_classified() {
    let all = sources();
    let mut unclassified: Vec<String> = Vec::new();
    let mut dead: Vec<String> = Vec::new();
    for (path, prefix) in RESOLVER_MODULES {
        let (_, text) = all.iter().find(|(p, _)| p == path).unwrap_or_else(|| {
            panic!("{path} is not walked — RESOLVER_MODULES names a file that has moved or gone")
        });
        let names = public_fns(text);
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
