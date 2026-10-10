//! Every directory resolved off the project root, and the state directory's dual read.

use std::path::{Path, PathBuf};

#[cfg(doc)]
use super::USER_DATA_DIR_ENV;
use super::project_root::{
    project_root, project_root_from, project_settings_dir, project_settings_dir_from,
    project_user_data_dir,
};
use super::{
    HIST_SUBDIR, IMPORTS_SUBDIR, INDICATORS_SUBDIR, LOGS_SUBDIR, PLUGINS_SUBDIR, PROJECT_BIN_DIR,
    PROJECT_DATA_DIR, PROJECT_TMP_DIR, PROJECT_USER_DATA_DIR, RESEARCH_SUBDIR, RHAI_SUBDIR,
    RUNS_SUBDIR, RUST_SUBDIR, SETTINGS_DIR_ENV, STATE_SUBDIR, STRATEGIES_SUBDIR, STUDIES_SUBDIR,
};

/// **THE data directory: `<project>/market_data`** — see [`PROJECT_DATA_DIR`] for what lives under it and
/// why it is a sibling of `settings/` rather than a child.
///
/// Resolved by the SAME walk as [`project_settings_dir`] and [`project_user_data_dir`], for the same
/// reason: "which project am I in" must have ONE answer, or a run could write tape into one
/// project while reading another's credentials. The walk's rules have been revised three times
/// (#1089, #1101, the deployment shape) and reusing it rather than restating it is what keeps that
/// true across the next revision.
///
/// Like `user_data/` and unlike `settings/`, this answers with where the directory BELONGS whether
/// or not it exists yet: `market_data/` is NOT a marker. A project with no `market_data/` is a fresh install that
/// has recorded nothing, not evidence of a mis-resolution, and the store creates its own root on
/// first write.
pub fn project_data_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_DATA_DIR))
}

/// [`project_data_dir`] under [`SETTINGS_DIR_ENV`]'s value — the data twin of
/// [`project_state_dir_from`], so ONE variable relocates a whole project rather than half of one.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_data_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_DATA_DIR))
}

/// **THE runtime-tool directory: `<project>/bin`** — see [`PROJECT_BIN_DIR`] for what lives under
/// it, why it is a sibling rather than a child, and why it is not `vendor/`.
///
/// Resolved by the SAME walk as every other sibling, for the reason that walk exists: "which
/// project am I in" must have ONE answer. A tool resolved by a second rule could be the one from
/// another project while the credentials came from this one.
///
/// Like `user_data/` and `market_data/` and unlike `settings/`, this answers with where the directory
/// BELONGS whether or not it exists yet: `bin/` is NOT a marker. A project that has never
/// installed a tool is an ordinary fresh install, and the caller's own "is it there" check is what
/// distinguishes that from a mis-resolution.
///
/// ⚠ **The answer is a DIRECTORY, and every tool under it owns a subdirectory rather than sitting
/// loose.** That is not tidiness: `bin/lightgbm/` holds the binary AND the `PROVENANCE` file
/// `vike_ml::train::cli::LightGbmCli::new` refuses to start without, so a tool is a directory's worth of
/// files even when only one of them is executable.
pub fn project_bin_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_BIN_DIR))
}

/// [`project_bin_dir`] under [`SETTINGS_DIR_ENV`]'s value — the tool twin of
/// [`project_data_dir_from`], so ONE variable relocates a whole project rather than part of one.
///
/// **This is the form a binary should call.** The bare walk is `$VIKE_SETTINGS_DIR`-BLIND, so a
/// unit whose service file relocates the project would find its settings in one place and its
/// tools in another — the exact split [`project_log_dir_from`] was added to close after a daemon
/// on the CI box wrote its log beside the executable while reading a relocated settings directory.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_bin_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_BIN_DIR))
}

/// **THE scratch directory: `<project>/tmp`** — see [`PROJECT_TMP_DIR`] for what belongs under it,
/// why it is a sibling rather than a child, why it carries no variable of its own, and — the half
/// that is not optional — why nothing may write here without the ownership and sweep in
/// [`crate::scratch`].
///
/// Resolved by the SAME walk as every other sibling, for the reason that walk exists: "which project
/// am I in" must have ONE answer. Scratch resolved by a second rule could land outside the mounted
/// volume while everything else landed inside it, which is the exact failure this directory replaces.
///
/// Like `user_data/`, `market_data/` and `bin/` and unlike `settings/`, this answers with where the
/// directory BELONGS whether or not it exists yet: `tmp/` is NOT a marker. A project that has never
/// staged anything is an ordinary fresh install, and [`crate::scratch::ScratchDir::create_in`]
/// creates the root on first use.
pub fn project_tmp_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_TMP_DIR))
}

/// [`project_tmp_dir`] under [`SETTINGS_DIR_ENV`]'s value — the scratch twin of
/// [`project_bin_dir_from`], so ONE variable relocates a whole project rather than part of one.
///
/// **This is the form a binary should call**, and here the bare walk's blindness is more than an
/// inconsistency. `$VIKE_SETTINGS_DIR` is how a deployment says where its project folder is — all
/// three shipped units set it precisely so the answer stops depending on `WorkingDirectory=` — and
/// a unit that relocated its project while its scratch kept resolving off the working directory
/// would stage gigabytes somewhere nobody mounted, backs up, or sweeps.
///
/// See [`project_root_from`] for the strip, the empty-parent refusal and the one residual.
pub fn project_tmp_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_root_from(override_dir, start)?.join(PROJECT_TMP_DIR))
}

/// `<project>/market_data/hist` — the historical-market-data store inside [`project_data_dir`], and the
/// PROJECT rung of [`crate::paths::store_path::resolve_store_root`].
///
/// The caller passes the result down as that function's `project_default` parameter: `store_path`
/// performs no walk and no environment read of its own, so the two modules stay independently
/// testable and the precedence stays a pure function of its arguments. In practice binaries reach
/// this through [`crate::paths::store_path::resolve_store_root_from`], which calls the
/// [`project_hist_store_dir_from`] twin below so the override is never forgotten at a call site.
///
/// ⚠ **No `VIKE_DATA_DIR` variable exists, deliberately.** The `config.store_root` row already
/// names this store outright and already wins over this rung, so a variable would be a second way
/// to say one thing — and the two would then need a documented precedence between them. `user_data/`
/// got its own [`USER_DATA_DIR_ENV`] precisely because it had no such existing override.
pub fn project_hist_store_dir(start: &Path) -> Option<PathBuf> {
    Some(project_data_dir(start)?.join(HIST_SUBDIR))
}

/// [`project_hist_store_dir`] under [`SETTINGS_DIR_ENV`]'s value — **the form every binary should
/// use**, and the one [`crate::paths::store_path::resolve_store_root_from`] calls.
///
/// `VIKE_SETTINGS_DIR` moves settings, credentials and state; it must move the store's default with
/// them, or an operator who relocated their project would read one project's credentials while
/// writing another project's tape. That defect shipped in the first cut of this rung and is what
/// this function exists to close.
pub fn project_hist_store_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_data_dir_from(override_dir, start)?.join(HIST_SUBDIR))
}

/// [`project_user_data_dir`] with [`USER_DATA_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// Same contract as [`project_settings_dir_from`]: the CALLER reads the variable and passes the
/// value, so this module still touches no environment, and a blank or whitespace-only value is
/// IGNORED rather than honoured — an empty `VIKE_USER_DATA_DIR=` line would otherwise resolve user
/// content to `""` and scan the working directory for strategies.
pub fn project_user_data_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => project_user_data_dir(start),
    }
}

/// [`project_user_data_dir_from`] over an **already-resolved settings directory** instead of a
/// fresh walk — the form a composition root must use, and the reason it exists is a
/// mis-resolution rather than tidiness.
///
/// [`project_user_data_dir_from`]'s own fallback is [`project_user_data_dir`], which WALKS, and that
/// walk is `$VIKE_SETTINGS_DIR`-BLIND. So a root that had already resolved `<project>/settings`
/// through [`project_settings_dir_from`] — honouring the override — then asked for the sibling and
/// silently got a DIFFERENT project's `user_data/` back, whenever the override and the working
/// directory disagreed. Which is the one case the override exists for: all three shipped units set
/// it precisely so the answer stops depending on `WorkingDirectory=`.
///
/// `settings_dir` is that already-resolved `<project>/settings`; the sibling is recovered by
/// stripping the trailing component, exactly as [`project_root_from`] does, with the same
/// empty-parent refusal (a bare relative `VIKE_SETTINGS_DIR=settings` has `""` for a parent, and
/// joining onto that would resolve user content against the WORKING DIRECTORY).
///
/// ⚠ **`override_dir` here is [`USER_DATA_DIR_ENV`], not [`SETTINGS_DIR_ENV`]** — the two are
/// deliberately separate variables, and the settings one has already been spent in producing
/// `settings_dir`. Passing the wrong one relocates user content to the settings directory itself.
///
/// Equal to [`project_user_data_dir_from`] whenever `settings_dir` came from a walk with no
/// settings override — pinned by `the_sibling_agrees_with_the_walk_when_nothing_overrides_it`, so
/// the two spellings cannot drift into two answers for the ordinary case.
pub fn user_data_dir_beside(
    override_dir: Option<&str>,
    settings_dir: Option<&Path>,
) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => settings_dir?
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|root| root.join(PROJECT_USER_DATA_DIR)),
    }
}

/// [`project_bin_dir`] over an **already-resolved settings directory** instead of a fresh walk —
/// the [`user_data_dir_beside`] twin, and it exists for that function's reason exactly.
///
/// A composition root has ALREADY resolved `<project>/settings` (through `vike_boot::boot`, which
/// honours `$VIKE_SETTINGS_DIR`), and `crates/vike-boot/tests/one_owner.rs` forbids it asking the
/// filesystem a second time: the bare walk is override-BLIND, so a unit whose service file
/// relocates the project would find its settings in one place and its runtime tools in another.
/// This resolves NOTHING — it strips a component off a directory the caller already has.
///
/// ⚠ **No `override_dir` parameter, and that is [`PROJECT_BIN_DIR`]'s own rule rather than an
/// omission**: that constant has no variable of its own "because every tool under it already has
/// one of its own". `$VIKE_SETTINGS_DIR` relocates the whole project and has already been spent in
/// producing `settings_dir`.
///
/// Same empty-parent refusal as [`user_data_dir_beside`] and [`project_root_from`]: a bare relative
/// `VIKE_SETTINGS_DIR=settings` has `""` for a parent, and joining onto that would resolve tools
/// against the WORKING DIRECTORY.
pub fn bin_dir_beside(settings_dir: Option<&Path>) -> Option<PathBuf> {
    settings_dir?
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|root| root.join(PROJECT_BIN_DIR))
}

/// `<project>/market_data/imports` ([`IMPORTS_SUBDIR`]) over an **already-resolved settings
/// directory** — the [`bin_dir_beside`] twin for the archive import root, and for that function's
/// reason exactly: the data daemon has already booted (`vike_boot::boot`, which honours
/// `$VIKE_SETTINGS_DIR`), and `crates/vike-boot/tests/one_owner.rs` forbids it asking the filesystem
/// a second time. This resolves NOTHING — it strips a component off a directory the caller already
/// has and joins two constant names.
///
/// `None` when there is no settings directory, or when it has no parent (a bare relative
/// `VIKE_SETTINGS_DIR=settings`, whose `""` parent would resolve the root against the WORKING
/// DIRECTORY) — the same refusal as [`bin_dir_beside`]. A daemon with no project above it therefore
/// has no imports root and mounts no import lane, which is the design's §2.3 rule rather than a
/// default to invent.
///
/// It does not say whether the directory EXISTS: an archive nobody has synced yet is an ordinary
/// state, and the import lane reports it per dataset rather than refusing to mount.
pub fn imports_dir_beside(settings_dir: Option<&Path>) -> Option<PathBuf> {
    settings_dir?
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|root| root.join(PROJECT_DATA_DIR).join(IMPORTS_SUBDIR))
}

/// `<project>/user_data/strategies/rhai` — user-authored interpreted strategies.
///
/// Present on every install, including a shipped binary: these are read at startup and need no
/// toolchain.
pub fn user_rhai_strategies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(STRATEGIES_SUBDIR).join(RHAI_SUBDIR))
}

/// `<project>/user_data/strategies/rust` — locally-authored COMPILED strategies.
///
/// ⚠ **Meaningful only in a source checkout.** These are picked up by a build script and compiled
/// into the binary; on an installed binary the directory is inert, and that is not a defect — a user
/// who installed a release was never going to run `cargo`. It is called out here, in the folder's own
/// README and in `vike-cli init`'s output, because "I dropped a file in and nothing happened" is
/// otherwise unanswerable.
pub fn user_rust_strategies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(STRATEGIES_SUBDIR).join(RUST_SUBDIR))
}

/// `<project>/user_data/indicators` — user-WRITTEN indicators, resolved by the same walk.
///
/// `None` when no project is found, exactly like its strategy siblings: an absent project is the
/// ordinary unconfigured state, not an error.
pub fn user_indicators_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(INDICATORS_SUBDIR))
}

/// `<project>/user_data/plugins` — the BUILT `cdylib` artifacts a runtime-loaded Rust strategy is
/// `dlopen`ed from, one `<name>-<sha256>.so` per build.
///
/// ⚠ **Not a source directory, and that is the whole reason it is a sibling of
/// [`user_rust_strategies_dir`] rather than a child of it.** The `.rs` a user writes lives under
/// `strategies/rust/<name>/`; this holds what the BUILDER SERVICE produced from it, named by the
/// sha256 of the source so a run can be bound to the exact code it reports on.
///
/// ⚠ **ONLY THE READER RESOLVES THROUGH THIS FUNCTION, and the asymmetry is the load-bearing
/// part.** This doc claimed "both halves of the runtime-plugin flow resolve it through this one
/// function — the builder writes here and the backtest server reads here", and the builder half
/// was simply false: `vike-strategy-builder` resolves nothing at all. It takes its output
/// directory as an environment variable its unit sets to a literal, deliberately (that binary
/// performs no project walk — see its own module doc's scope note), so the two halves are NOT one
/// resolution. What makes them meet is an operator substituting ONE project root into both units
/// while both name the same root-relative leaf, which is the same arrangement the hist store
/// already uses for the datahub/backtest pair — and because that is a deployment fact rather than
/// a code fact, it is held by a gate over the unit files rather than by this function:
/// `crates/vike-ops/tests/container_deploy/deploy_layout_gate/plugin_and_profile.rs`'s
/// `every_shipped_plugin_artifact_dir_is_one_spelling`.
///
/// `None` when no project is found, exactly like its siblings: an absent project is the ordinary
/// unconfigured state, and a checkout that has built no plugin simply has no such directory.
pub fn user_plugins_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(PLUGINS_SUBDIR))
}

/// `<project>/user_data/runs` — EVERY run the user has produced, of every kind. See
/// [`RUNS_SUBDIR`] for why there is one such directory rather than one per kind of run.
///
/// Resolved by the SAME walk as [`project_user_data_dir`] and its siblings, so "which project am I
/// in" keeps ONE answer: a run must not land in one project while the strategy that produced it was
/// read from another.
///
/// Like every `user_data/` resolver and unlike [`project_settings_dir`], this answers with where the
/// directory BELONGS whether or not it exists. `user_data/` is not a marker and neither is `runs/`:
/// a project that has run nothing is a fresh install, not a mis-resolution, and creating the
/// directory belongs to whoever writes the first run.
pub fn user_runs_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(RUNS_SUBDIR))
}

/// [`user_runs_dir`] with the `VIKE_USER_DATA_DIR` override applied — the twin of
/// [`project_user_data_dir_from`], so the runs directory and every other `user_data/` consumer
/// answer from ONE walk.
///
/// ⚠ It exists because they did not. `crates/vike-backtest/src/backtest_cli.rs`'s `persist_run`
/// carried this as a DECLARED RESIDUAL: an operator who redirected `user_data` got their indicators
/// from the override and their runs from the project walk, silently, in one process.
///
/// The value is a PARAMETER and nothing here reads the environment — the BINARY sweeps
/// `std::env::vars()` once and hands the answer down, which is the rule
/// `crates/vike-ops/tests/settings_secrets/settings_registry.rs`'s `LIBRARY_PIN` ratchets. A blank value is ignored
/// rather than honoured, exactly as [`project_user_data_dir_from`] already does.
pub fn user_runs_dir_from(explicit: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir_from(explicit, start)?.join(RUNS_SUBDIR))
}

/// `<project>/user_data/research/studies` — BOTH study tiers under one root, which is what a
/// listing over the tree needs. See [`STUDIES_SUBDIR`] for why the tiers are the strategy tree's
/// own, and [`RESEARCH_SUBDIR`] for why a study is filed apart from a strategy.
///
/// It exists as its own resolver, unlike its strategy twin, for a reason the strategy loader states
/// against itself: `crates/vike-studio-core/src/user_strategies/load.rs`'s `load_user_strategies`
/// takes the PARENT of the two tiers and its own doc has to describe that argument as the tier
/// resolvers "minus their leaf" — a caller stripping a component off a resolved path is one edit
/// away from stripping the wrong one. `crates/vike-studio-core/src/listing.rs`'s `list_studies`
/// takes this directly.
pub fn user_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(RESEARCH_SUBDIR).join(STUDIES_SUBDIR))
}

/// `<project>/user_data/research/studies/rhai` — user-authored interpreted studies.
///
/// Present on every install, including a shipped binary, for the same reason
/// [`user_rhai_strategies_dir`] is: an interpreted artifact needs no toolchain.
pub fn user_rhai_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(user_studies_dir(start)?.join(RHAI_SUBDIR))
}

/// `<project>/user_data/research/studies/rust` — locally-authored COMPILED studies.
///
/// ⚠ **Meaningful only in a source checkout**, exactly as [`user_rust_strategies_dir`] is and for
/// exactly its reason: a `.rs` file is an input to `cargo`, and a user who installed a release was
/// never going to run one. The tier still EXISTS on a binary install, and a listing still shows what
/// is in it — an empty directory is an answer, while a tier that silently disappears on half the
/// installs is the thing that makes "I dropped a file in and nothing happened" unanswerable.
pub fn user_rust_studies_dir(start: &Path) -> Option<PathBuf> {
    Some(user_studies_dir(start)?.join(RUST_SUBDIR))
}

/// `<project>/user_data/logs` — logs belonging to work the USER ran, distinct from the app's own
/// runtime log under [`project_log_dir`].
///
/// The split is by owner, which is how MetaTrader draws it too (platform `Logs/` vs the tester's own
/// tree): a daemon on a server has no `user_data/` at all, while a backtest trace is the user's to
/// read and delete.
///
/// ⚠ **The two files under it have deliberately OPPOSITE defaults.** `compile.log` is always
/// written — it is bounded by the number of strategies rather than by runtime, and a user whose
/// script failed to parse needs one obvious place to look. Per-run backtest traces are OPT-IN,
/// because their size scales with runtime and this workspace has already written 6.4 GB in an hour
/// and 341 GB on one long run. Freqtrade writes no log at all without `--logfile`, and that instinct
/// is right for anything unbounded.
///
/// ⚠ **`$VIKE_LOG_DIR` must NOT redirect this.** That variable moves the APP log; if a user's run
/// history followed it, setting it for a daemon would silently relocate their backtests.
pub fn user_logs_dir(start: &Path) -> Option<PathBuf> {
    Some(project_user_data_dir(start)?.join(LOGS_SUBDIR))
}

/// The project's STATE directory — `<project>/settings/state` — where program-written JSON lives
/// (`pace.json`, workspace state).
///
/// A sub-directory of [`project_settings_dir`], not a sibling, so everything the app owns is under
/// one folder. It is separate from the TOMLs beside it because these files are written by the
/// PROGRAM: a program rewriting a file a human is editing loses that human's work, and keeping the
/// two in one directory invites exactly that.
pub fn project_state_dir(start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir(start)?.join(STATE_SUBDIR))
}

/// [`project_state_dir`] under [`project_settings_dir_from`]'s override — the state twin, so a
/// binary that relocates settings relocates the state under them with the same one variable.
pub fn project_state_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir_from(override_dir, start)?.join(STATE_SUBDIR))
}

/// [`project_state_dir_from`] with the override taken out of a composition root's own
/// `std::env::vars()` sweep — [`SETTINGS_DIR_ENV`] and nothing else, the one fact
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env` takes out of the same map, so a
/// root handing its sweep to both gets ONE answer to "which project".
///
/// For a root that owns a sweep and boots nothing: a one-shot tool in a bridge crate, where a
/// lookup of `VIKE_SETTINGS_DIR` by name in the tool itself would be a `Layer::Binary` row read by
/// a bridge, which `crates/vike-ops/tests/settings_secrets/settings_registry/library_ratchet.rs`'s
/// `no_venue_setting_is_read_from_the_environment` refuses. PURE by this module's rule: `env` is a
/// parameter, and nothing here reads the process environment.
pub fn project_state_dir_from_env(
    env: &std::collections::HashMap<String, String>,
    start: &Path,
) -> Option<PathBuf> {
    project_state_dir_from(env.get(SETTINGS_DIR_ENV).map(String::as_str), start)
}

/// The project's LOG directory — `<project>/settings/state/logs`, [`project_state_dir`] plus
/// [`LOGS_SUBDIR`].
///
/// A rolling trace file is program-written state by this module's own test (delete it and the
/// program writes a new one; nothing about behaviour changes permanently), so it belongs under the
/// state root with everything else the program owns. It had ended up in `<exe_dir>/logs` instead —
/// `target/debug/logs/vike-tradehub.<date>` in a checkout, a directory `cargo clean` deletes, and
/// on a deployment a directory beside the binary an operator has no reason to open.
///
/// A DIRECTORY rather than a file, because `tracing_appender` rolls a file per day per binary
/// inside it (`vike-tradehub.2026-08-05`, `vike-recorder.2026-08-05`, …).
///
/// PURE, like everything here: `start` is the caller's working directory, and the BINARY hands the
/// result to `vike_log::LogConfig::project_dir`. vike-log cannot compute this itself — it is
/// deliberately the bottom layer and depends on no crate at all — and `$VIKE_LOG_DIR` still wins
/// over whatever this returns.
pub fn project_log_dir(start: &Path) -> Option<PathBuf> {
    Some(project_state_dir(start)?.join(LOGS_SUBDIR))
}

/// [`project_log_dir`] under [`project_settings_dir_from`]'s override — the log twin of
/// [`project_state_dir_from`], so ONE variable relocates a whole project rather than most of one.
///
/// It exists because [`project_log_dir`] was the one member of this family with no `_from` sibling,
/// and the daemon that needed it (`vike-recorder`) therefore resolved its rolling log purely from
/// the working directory while its shipped unit set `VIKE_SETTINGS_DIR` and its runbook claimed
/// that made the answer "independent of WorkingDirectory". Measured: with the variable set to one
/// directory and the CWD under no project marker at all, the log landed in `<exe_dir>/logs`.
pub fn project_log_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_state_dir_from(override_dir, start)?.join(LOGS_SUBDIR))
}

/// The path to READ `name` from: `<state_dir>/<name>` when that file EXISTS, else `legacy`.
///
/// Half one of the dual read. Deliberately an EXISTENCE test on the new file, not on the
/// directory: a state dir that exists because some OTHER file was already migrated must not make
/// this file's own un-migrated `legacy` copy invisible.
pub fn read_path(state_dir: Option<&Path>, name: &str, legacy: PathBuf) -> PathBuf {
    match state_dir {
        Some(dir) => {
            let new = dir.join(name);
            if new.exists() { new } else { legacy }
        }
        None => legacy,
    }
}

/// The path to WRITE `name` to: `<state_dir>/<name>`, creating `<state_dir>` lazily.
///
/// Half two of the dual read — a write ALWAYS lands here, which is what silently migrates an
/// install. `Err` (never a panic) when there is no resolvable state dir, when it cannot be
/// created (a read-only `HOME`, or a plain file sitting where the directory should be), or when
/// the target file is a SYMLINK (below): the caller logs it and keeps writing to the file's legacy
/// path, so an unwritable state root degrades to pre-Phase-2 behaviour instead of losing the save.
///
/// # A symlinked state FILE is refused; a symlinked state DIRECTORY is not
///
/// Every caller takes this path and `fs::write`s it, which TRUNCATES whatever a link points at and
/// replaces its contents with something the program chose. So the refusal is aimed at exactly
/// that: the WRITE TARGET.
///
/// * **The file: refused.** Nothing under `settings/state` has a reason to be redirected
///   file-by-file. These files are program-written and re-derivable by definition (that is the test
///   that put them here rather than beside the TOMLs), their names are constants in the calling
///   crates, and a link at one of them buys an operator nothing they cannot get by relocating the
///   directory. What it DOES buy anyone else is a truncate-and-overwrite primitive aimed wherever
///   they point it.
/// * **The directory: allowed.** Pointing `settings/` or `settings/state` at shared or larger
///   storage is a legitimate setup — the filesystem's spelling of the same intent
///   [`SETTINGS_DIR_ENV`] and `VIKE_STATE_ROOT` serve — and the write still lands inside a
///   directory the operator chose, under that directory's own permissions. Refusing it would break
///   real deployments and prevent nothing: the target would still be a plain file.
///
/// ⚠ **This is a CHECK, not an enforcement.** The path is returned and opened later, so a symlink
/// planted in between is not caught — closing that needs `O_NOFOLLOW` at the caller's own `open`
/// (which is what `crates/bridges/ctrader/src/bin/ctrader_authorize.rs`'s `write_private` does for
/// the file with the worst payload). What this DOES close is the realistic case: a link that is
/// already sitting there when the program starts.
pub fn write_path(state_dir: Option<&Path>, name: &str) -> std::io::Result<PathBuf> {
    let dir = state_dir.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no state directory: no state-root override and no per-user data/home directory is set",
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(name);
    // `symlink_metadata`, so the link itself is what is inspected rather than whatever it points
    // at. An `Err` here is "nothing is there yet", the normal first-write case.
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "{} is a symlink: refusing to write program state through it, because the write \
                 would truncate whatever it points at. Remove the link, or relocate the whole \
                 state directory with {SETTINGS_DIR_ENV} (a symlinked DIRECTORY is fine).",
                path.display()
            ),
        ));
    }
    Ok(path)
}
