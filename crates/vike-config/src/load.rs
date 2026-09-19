//! [`Settings`] and [`load`] — the layered loader.
//!
//! ```text
//! code defaults -> <project>/settings/db/vike.db  (0057 Phase 1: MIRRORED, so it loses to the files)
//!               -> <project>/settings/policy.toml
//!               -> <project>/settings/config.toml
//!               -> <project>/settings/preferences.toml
//!               -> <project>/settings/flags.toml
//!               -> env                     (Config / Preferences / Flags only)
//!               -> CLI                     (Config / Preferences / Flags only)
//!               -> clamp preferences to policy
//! ```
//!
//! ⚠ **There is no fifth file.** A `<project>/vike.toml` override layer sat between the settings
//! directory and the environment for one hour; it is REMOVED, and a present one is refused at
//! startup rather than ignored — see [`crate::removed`], which owns both the reason and the
//! message. Four files with one authority each is the whole point of `<project>/settings/`, and a
//! file above that directory overriding two of them puts "which file won?" back into every
//! investigation.
//!
//! **The root is a DIRECTORY and it is a PARAMETER.** `settings_dir` is `<project>/settings`, the
//! one directory every setting, credential and state file lives in. The env map is a parameter too.
//! So the whole loader is a pure function of its arguments — CLAUDE.md's rule that libraries take
//! configuration as parameters and only binaries touch the process environment, and the reason
//! every test below runs against a `tempfile::TempDir` and can never see a real settings directory.
//!
//! This crate resolves NO directory of its own: it never walks, never expands `~` and never reads a
//! platform variable. `vike_model::state_path::project_settings_dir_from` (and its
//! no-`vike-*`-dependency twin `vike_secrets::project_settings_dir_from`) is where the walk lives, and the
//! BINARY performs it. (The one place a second path is named — the removed-file probe — derives it
//! with a single [`Path::parent`] call from the directory the caller supplied, which discovers
//! nothing: `<project>/settings`'s parent is `<project>` by construction.)
//!
//! File READING is this crate's job, though — a loader that took four pre-read strings would push
//! the "which file was that?" bookkeeping onto every caller, and that bookkeeping is precisely
//! what [`crate::ConfigError`] exists to get right.
//!
//! **A missing file is not an error.** A layer with no file contributes nothing and the layer
//! below stands; only a file that EXISTS and cannot be read or parsed fails the load. That is
//! what makes `load(None, &HashMap::new())` the pure code-default answer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConfigPatch};
use crate::error::{ConfigError, key_from_parse_message, redacted_parse_message};
use crate::flags::{Flags, FlagsPatch};
use crate::layers::{CliOverride, CliOverrides, EnvOverride};
use crate::policy::{Policy, PolicyPatch};
use crate::preferences::{Preferences, PreferencesPatch};
use vike_secrets::StoredSettings;

/// File name of the policy layer inside the settings directory.
pub const POLICY_FILE: &str = "policy.toml";
/// File name of the deployment-config layer inside the settings directory.
pub const CONFIG_FILE: &str = "config.toml";
/// File name of the preferences layer inside the settings directory.
pub const PREFERENCES_FILE: &str = "preferences.toml";
/// File name of the flags layer inside the settings directory.
pub const FLAGS_FILE: &str = "flags.toml";

/// The fully-resolved settings for this process.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Settings {
    /// Hard ceilings. Defaults + file only — see [`Policy`].
    pub policy: Policy,
    /// Deployment settings. Full chain.
    pub config: Config,
    /// Taste and tuning. Full chain.
    pub preferences: Preferences,
    /// Operator toggles. Full chain.
    pub flags: Flags,
    /// Non-fatal resolutions the loader made, in the order it made them.
    ///
    /// Returned as DATA rather than logged, for the same reason the env map is a parameter: this
    /// crate depends on `serde` + `toml` + `vike-model` and deliberately not on `tracing`, and a
    /// library that writes to stderr on its own initiative is a library that cannot be used by a
    /// binary whose stdout/stderr is a protocol (`vike-tradehub`, `vike-recorder`, the jforex
    /// sidecar). The BINARY logs these — it already owns logging init.
    ///
    /// ⚠ **TWO producers today** (2026-09-06), and they arrived in separate branches that each
    /// called itself the only one — so read this list, never a "the one producer" sentence:
    ///
    /// * [`NO_SETTINGS_DIRECTORY_WARNING`] — a load handed no settings directory at all.
    ///   `docs/ops/kill-switches.md` carried that silence as a register entry ("a missing settings
    ///   directory is silently uncapped"): a process that resolved no project gets
    ///   `Policy::default()`, which is every ceiling absent and every venue capped `paper`, and said
    ///   so nowhere. That is indistinguishable from a policy file that deliberately says nothing,
    ///   and the two want opposite reactions from an operator.
    /// * A WRITTEN reconcile refusal that S2 no longer honours — `reconcile = false` in
    ///   `flags.toml`, in a `flags` ROW of the settings database ([`crate::mirror::apply_rows`],
    ///   which performs the same read-before-apply for the same reason), `VIKE_RECONCILE=0` in the
    ///   environment, or `--reconcile false` on the command line. See
    ///   [`crate::flags::reconcile_refusal_ignored`], which carries the argument; this frame is the
    ///   LAST one that can tell an explicit `false` from an unset value, because the resolved field
    ///   is a `bool`. It goes when `reconcile` stops being a flag at all.
    ///
    /// The disposition is unchanged and applies to both: a warning is deleted WITH the defect it
    /// describes, never left to go stale. The most recent one to go that way was [`load_with_cli`]'s
    /// admission that an `[accounts]` table bound nothing yet — the mount now folds those ceilings
    /// per account (`vike_mount::account_ceiling`), so the admission went with it.
    ///
    /// The channel predates every producer it has had and was KEPT while it had none,
    /// deliberately: an empty
    /// warnings list PRINTS NOTHING and therefore claims nothing, where a dead SETTING is displayed
    /// as effective and attributed to the operator's own file — positive confirmation of something
    /// false. (Its ORIGINAL producer was the policy-clamps-preference edge, `rate.max_utilization`
    /// over `rate_utilization`, and both halves were removed as a ceiling bounding a value nothing
    /// read — see [`crate::preferences`]' module doc. Deleting the channel then would have meant
    /// this loader adjustment landing with three binaries having no way to surface it, which is
    /// exactly the state the clamp warning was in until Phase 6c: swallowed.)
    pub warnings: Vec<String>,
}

/// The no-settings-directory warning: [`load_with_cli`] was handed no settings directory, so layer 2
/// was skipped whole and every ceiling on this process is the compiled-in default.
///
/// ⚠ This said "The ONE warning [`load_with_cli`] produces" and was the THIRD instance of the same
/// defect in this file — it sat forty lines below [`Settings::warnings`]'s own "⚠ TWO producers
/// today … never a 'the one producer' sentence" note, which two branches had already earned. It then
/// said "four sites", and that went stale the moment the settings database gained a `flags` layer of
/// its own. **Count nothing here; the field doc is the list**, and the pushes are this one plus a
/// reconcile refusal per layer that can carry a written `false`.
///
/// **Why this is a warning and not an error.** A missing project is the ordinary state of a dev
/// checkout run from the wrong directory and of a container whose bind mount did not land, and
/// refusing to start over it would strand an operator with a daemon that will not boot instead of
/// one that boots capped at `paper` (`docs/decisions/0013-degrade-vs-refuse.md`). What it may not do
/// is stay SILENT: `Policy::default()` is a real, permissive-looking answer that an operator's own
/// `config show` then attributes to nothing at all, which is the "positive confirmation of something
/// false" this crate exists to prevent.
///
/// Held as a constant rather than formatted at the site because two binaries assert on it
/// (`crates/vike-tradehub/src/tradehub_cli.rs`'s `an_absent_policy_file_is_the_mount_default_and_arms_no_venue`
/// is the daemon-edge one) and a message they match on by substring is a message that must have one
/// spelling.
pub const NO_SETTINGS_DIRECTORY_WARNING: &str = "no settings directory resolved — every ceiling is the compiled-in default and every venue is \
     capped `paper`. Set VIKE_SETTINGS_DIR, or run from a project that has one.";

/// Load the settings, applying every layer in precedence order.
///
/// * `settings_dir` — `<project>/settings`, or `None` to skip that layer entirely (and say so on
///   [`Settings::warnings`] — see [`NO_SETTINGS_DIRECTORY_WARNING`]). Absent files inside it are
///   skipped individually.
/// * `env` — an already-loaded environment map. A caller passes `std::env::vars().collect()`,
///   the credential map, or a merge of both; that choice belongs to the binary, not here.
///
/// Returns [`ConfigError`] naming the offending file and key (or env variable and value) on the
/// first layer that fails — including a REMOVED key, which is refused by name rather than ignored
/// (`policy.toml`'s `max_total_exposure` and `[rate] max_utilization`, `preferences.toml`'s
/// `rate_utilization`), and a REMOVED FILE, the retired `<project>/vike.toml`. Non-fatal
/// resolutions ride [`Settings::warnings`], which has TWO producers today — a `settings_dir` of
/// `None`, and a written `reconcile = false` that is no longer honoured. That field enumerates
/// them, and is the only place that should: this sentence said "today's one producer" and was
/// already wrong when the second one merged beside it.
pub fn load(
    settings_dir: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<Settings, ConfigError> {
    load_with_cli(settings_dir, env, &CliOverrides::default())
}

/// [`load`] plus the CLI layer — the highest-precedence one.
///
/// Split out rather than folded into `load`'s signature because most callers have no flags to
/// pass and the design's canonical entry point is the two-argument one. Note there is no
/// `policy` field on [`CliOverrides`] to pass, and no way to add one from outside this crate.
pub fn load_with_cli(
    settings_dir: Option<&Path>,
    env: &HashMap<String, String>,
    cli: &CliOverrides,
) -> Result<Settings, ConfigError> {
    load_with_store(settings_dir, None, env, cli)
}

/// [`load_with_cli`] plus **the settings DATABASE**, applied BELOW the four files.
///
/// `docs/decisions/0057-the-seven-settings-files-answered-one-at-a-time.md`'s Phase 1 is MIRRORED:
/// the store is written and **the files still win**, which is what makes it safe to land on a live
/// box. The rows are derived from the files by [`crate::mirror::rows_from_files`] and applied here
/// underneath them, so a mirrored box resolves byte-identically to an unmirrored one — proven by
/// `crates/vike-config/tests/mirror.rs`'s `the_mirror_changes_no_effective_value`, not asserted.
///
/// `store` is a PARAMETER and is plain data: this crate never opens the store and must not (its own
/// manifest says so, and 0057 gives the reason under one database — a handle that reaches the
/// settings rows reaches the `credential` table too). The BINARY opens it, through
/// `vike_secrets::read_settings_in`, and hands the rows in exactly as it hands in the environment
/// map. `None` is the answer for a box with no database, for a store migrated before the settings
/// tables existed, and for a caller that has not been wired to read one — all three contribute no
/// layer, which is the same thing an absent file does.
///
/// ⚠ The rows go back through the SAME patch types the files do, so an unknown key written by a
/// hand `INSERT` is refused BY NAME and a tombstone keeps its own message. That round trip is the
/// whole of [`crate::mirror`]'s read half; it is not a convenience.
pub fn load_with_store(
    settings_dir: Option<&Path>,
    store: Option<&StoredSettings>,
    env: &HashMap<String, String>,
    cli: &CliOverrides,
) -> Result<Settings, ConfigError> {
    // Layer 0 — refuse a REMOVED layer before resolving a single value. It sits here, inside the
    // one function every composition root goes through, rather than beside each root's
    // `refuse_removed_env` call: a root that could forget a parameter can equally forget a call,
    // and that is precisely how the file came to be advertised-but-unread in the first place.
    crate::removed::refuse_removed_project_file(settings_dir)?;

    // Layer 1 — code defaults.
    let mut settings = Settings::default();

    // Layer 1.5 — the settings DATABASE, BELOW the files for the mirror period. `None` is an absent
    // layer, exactly as an absent file is. See this function's doc, and `crate::mirror`.
    if let Some(store) = store {
        crate::mirror::apply_rows(&mut settings, store)?;
    }

    // Layer 2 — the settings directory, one file per authority level.
    if let Some(dir) = settings_dir {
        let policy_file = dir.join(POLICY_FILE);
        if let Some(patch) = read_toml::<PolicyPatch>(&policy_file)? {
            settings.policy.apply(patch, &policy_file)?;
            // ⚠ There WAS a warning here, and its removal is the point rather than a side effect:
            // it said `[accounts]` was "validated and stored but NOT YET ENFORCED". The mount now
            // folds the per-account ceiling (`vike_mount::account_ceiling`, reached from
            // `make_engine_accounts`), so the admission has become false and a loader that kept
            // printing it would be the mirror image of the defect it was written for.
        }
        let config_file = dir.join(CONFIG_FILE);
        if let Some(patch) = read_toml::<ConfigPatch>(&config_file)? {
            settings.config.apply(patch, &config_file)?;
        }
        let preferences_file = dir.join(PREFERENCES_FILE);
        if let Some(patch) = read_toml::<PreferencesPatch>(&preferences_file)? {
            settings.preferences.apply(patch, &preferences_file)?;
        }
        let flags_file = dir.join(FLAGS_FILE);
        if let Some(patch) = read_toml::<FlagsPatch>(&flags_file)? {
            // ⚠ READ BEFORE THE APPLY, and this is the one place it can be read at all: the patch
            // is `Option<bool>` and the resolved field is a `bool`, so `Some(false)` and `None`
            // become the same value one line down. See `flags::reconcile_refusal_ignored`.
            if patch.reconcile == Some(false) {
                settings.warnings.push(crate::flags::reconcile_refusal_ignored(&format!(
                    "`reconcile = false` in {}",
                    flags_file.display()
                )));
            }
            settings.flags.apply(patch, &flags_file)?;
        }
    } else {
        // NO settings directory at all — layer 2 is skipped whole, so `policy` stays
        // `Policy::default()`: no notional ceiling, no dead-man, no `[venues]` table. Every one of
        // those is a REFUSAL rather than an arming, so this cannot leak a live order — but it is
        // also invisible, and that invisibility was `docs/ops/kill-switches.md`'s register entry.
        // ⚠ Deliberately NOT extended to "a directory with no `policy.toml` in it": that is the
        // ordinary shape of a configured box that has not written ceilings yet, and warning on it
        // every boot is how a warning stops being read. The distinction being drawn is between "you
        // told me nothing" and "I never found your project".
        settings.warnings.push(NO_SETTINGS_DIRECTORY_WARNING.to_string());
    }

    // Layer 3 — the environment. `settings.policy` is absent from this block, and cannot be added
    // to it: `Policy` implements neither `EnvOverride` nor any inherent `apply_env`, so the line
    // would not compile. See `crate::layers`.
    settings.config.apply_env(env)?;
    settings.preferences.apply_env(env)?;
    settings.flags.apply_env(env)?;

    // ...and the same written-refusal check for the environment layer, AFTER `apply_env` so a
    // malformed value is still the error it always was rather than a warning. `get` skips an empty
    // value exactly as the layer itself does, so `VIKE_RECONCILE=` warns about nothing — it
    // configured nothing under the old reader either.
    if crate::layers::get(env, crate::flags::RECONCILE_ENV) == Some("0") {
        settings
            .warnings
            .push(crate::flags::reconcile_refusal_ignored("`VIKE_RECONCILE=0` in the environment"));
    }

    // …and the DEAD-FLAG variables. Their FILE halves are refused by name in `Flags::apply`
    // (`flags::DEAD_FLAG_KEYS`); this is the other half — a variable whose only reader was
    // `apply_env`, which went with the field it set. It cannot be a refusal (`crate::REMOVED_ENV`
    // would stop a correct daemon dead over a spelling that never configured anything) and it must
    // not be silence (the operator who exported it believes something is recording). `get` skips an
    // empty value, exactly as the layer itself does.
    //
    // ⚠ Spelled ONE CONST PER LINE rather than as a loop over `DEAD_FLAG_KEYS`, deliberately: the
    // settings registry records where a variable is NAMED, and a loop variable names nothing it
    // could resolve (`vike_ops::settings::Naming::Dynamic`, which needs an allowlist entry). This
    // is the same `get(env, <CONST>)` shape the reconcile refusal above uses, and a second dead
    // flag joins it as one more line.
    if crate::layers::get(env, crate::flags::RECORD_DVOL_ENV).is_some() {
        settings.warnings.push(crate::flags::dead_flag_env_ignored(crate::flags::RECORD_DVOL_ENV));
    }

    // ...and the venue-catalog flip's own written-refusal check
    // (`docs/decisions/0066`'s decision 4). ONE origin, not four, and that is not an oversight:
    // `VIKE_DATAHUB_VENUE_CATALOG` was never a `flags.toml` key, never a `CliOverrides` field and
    // never a settings-database row, so the environment is the only place an operator can have
    // written it. `reconcile_refusal_ignored`'s four call sites exist because `reconcile` IS a key
    // at all four layers; copying the count rather than the rule would put three checks on
    // origins the value cannot arrive from.
    //
    // ⚠ The CONDITION is the inverse of the reconcile one. That grammar was *exact `"1"` is on*,
    // so `=1` still asks for what it now gets (silent — the belief is still true) and any OTHER
    // non-empty value meant OFF and now silently means ON (warned). `get` skips a blank, which is
    // the same rule the layer itself and `refuse_removed_env` follow.
    if crate::layers::get(env, crate::flags::VENUE_CATALOG_ENV).is_some_and(|v| v != "1") {
        // The origin names the variable and the SHAPE of the value, never the value itself: the
        // operator set exactly one thing, so "a non-`1` value" locates it, and echoing an
        // arbitrary environment value into a log line buys nothing this does not.
        settings.warnings.push(crate::flags::venue_catalog_refusal_ignored(&format!(
            "a non-`1` `{}` in the environment",
            crate::flags::VENUE_CATALOG_ENV
        )));
    }

    // Layer 4 — CLI. Same three types, same structural exclusion of policy.
    settings.config.apply_cli(cli)?;
    settings.preferences.apply_cli(cli)?;
    settings.flags.apply_cli(cli)?;
    // ...and its own written-refusal check, for the same reason and against the same `Option<bool>`
    // shape. A `--reconcile false` is the shortest-lived of the three origins and the one most
    // likely to be typed by somebody who believes it is the switch.
    if cli.reconcile == Some(false) {
        settings.warnings.push(crate::flags::reconcile_refusal_ignored(
            "`--reconcile false` on the command line",
        ));
    }

    // ⚠ `settings.clamp_to_policy()` stood here — the ONE policy-binds-preference edge in the
    // model, and it bound `policy.rate.max_utilization` onto `preferences.rate_utilization`. Both
    // are gone: the preference was read by nothing, so the ceiling bounded nothing and the clamp
    // resolved a disagreement about a number no code consumed, warning line and all. See
    // `crate::preferences`' module doc.
    //
    // The MECHANISM was not the problem and is not being repudiated — policy sets the bound, a
    // preference sets the value inside it, and clamping rather than erroring is still the right
    // resolution when two people wrote the two files. It simply has no instance today, and a
    // no-op pass over an empty set is the kind of code that keeps a dead concept looking alive.
    // The first real bound/value pair brings it back, ideally with the gate that would have caught
    // this one: `crates/vike-config/tests/policy_is_consumed.rs` now REFUSES a policy row whose
    // only claimed consumer is inside this crate, which is exactly what the clamp was.
    Ok(settings)
}

/// Read and parse one TOML file. `Ok(None)` when the file does not exist — an absent layer.
///
/// The `NotFound` check is on the READ result rather than a prior `Path::exists()` on purpose:
/// `exists()` then `read` is a TOCTOU race and, worse, `exists()` returns `false` for a file that
/// is present but unreadable — which would silently degrade a permissions bug into "using
/// defaults", exactly the class of silence this crate exists to remove.
fn read_toml<T: for<'de> Deserialize<'de>>(file: &Path) -> Result<Option<T>, ConfigError> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ConfigError::Read { file: file.to_path_buf(), source }),
    };
    parse_toml_str(file, &text).map(Some)
}

/// The parse half of [`read_toml`], over an already-held string — shared with
/// [`crate::write::validate_settings_text`], which must refuse a WOULD-BE file with the exact
/// error the next boot's loader would raise (`file` names the file the message should blame).
pub(crate) fn parse_toml_str<T: for<'de> Deserialize<'de>>(
    file: &Path,
    text: &str,
) -> Result<T, ConfigError> {
    match toml::from_str::<T>(text) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            // ⚠ NOT `e.to_string()`: that renders the offending SOURCE LINE verbatim, and these
            // files sit beside `secrets.env`. See `error::redacted_parse_message`.
            let message = redacted_parse_message(text, e);
            Err(ConfigError::Parse {
                file: file.to_path_buf(),
                key: key_from_parse_message(&message),
                message,
            })
        }
    }
}

/// The four settings-directory file names, in application order — useful to a `config show` that
/// wants to state which files it looked for.
pub fn settings_files(settings_dir: &Path) -> [PathBuf; 4] {
    [
        settings_dir.join(POLICY_FILE),
        settings_dir.join(CONFIG_FILE),
        settings_dir.join(PREFERENCES_FILE),
        settings_dir.join(FLAGS_FILE),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_files_and_no_env_is_exactly_the_code_defaults() {
        let s = load(None, &HashMap::new()).unwrap();
        assert_eq!(s.policy, Policy::default());
        assert_eq!(s.config, Config::default());
        assert_eq!(s.preferences, Preferences::default());
        assert_eq!(s.flags, Flags::default());
        // ⚠ This line asserted `warnings.is_empty()` and that WAS the defect: the VALUES are the
        // code defaults, which is correct, and the operator was told nothing about why. The values
        // are unchanged; only the silence is.
        assert_eq!(s.warnings, vec![NO_SETTINGS_DIRECTORY_WARNING.to_string()]);
    }

    /// A load handed NO settings directory says so, and a load handed one does not.
    ///
    /// The pair is the whole point: a warning that fires on every load is noise an operator learns
    /// to skip, and one that fires on no load is the silence this closes. `docs/ops/kill-switches.md`
    /// carried it as a register entry — a process that resolved no project runs on
    /// `Policy::default()`, which is no ceiling anywhere, and nothing anywhere said so.
    ///
    /// ⚠ Mutation proof: delete the `else` arm in `load_with_cli` and the first half goes red; make
    /// it unconditional and the second half does.
    #[test]
    fn a_load_with_no_settings_directory_says_so_and_one_with_a_directory_does_not() {
        let none = load(None, &HashMap::new()).unwrap();
        assert!(
            none.warnings.iter().any(|w| w == NO_SETTINGS_DIRECTORY_WARNING),
            "a load that resolved no project must SAY that its ceilings are defaults: {none:?}"
        );

        // A real directory, and deliberately an EMPTY one: the distinction is "I found no project",
        // never "your project wrote no ceilings", so a settings directory holding not one of the
        // four files must still be silent here.
        let tmp = tempfile::tempdir().unwrap();
        let some = load(Some(tmp.path()), &HashMap::new()).unwrap();
        assert!(
            some.warnings.is_empty(),
            "a settings directory that exists resolves the project, whatever is in it: {some:?}"
        );
    }

    /// **A DEAD flag's two spellings get two DIFFERENT answers, and both are answers.**
    ///
    /// The file key is a hard refusal naming the key (`Flags::apply`, tested at its own site); the
    /// variable is a WARNING and the load succeeds. Pinned together in one test because the value
    /// of the pair is the asymmetry: make the variable a refusal and a correct daemon stops over a
    /// spelling that never configured anything, make it silent and the operator who exported it
    /// believes something is being recorded.
    ///
    /// ⚠ Mutation proof: delete the `dead_flag_env_ignored` push in `load_with_cli` and the second
    /// half goes red; delete the `DEAD_FLAG_KEYS` loop in `Flags::apply` and the first does.
    #[test]
    fn a_dead_flag_refuses_its_file_key_and_warns_about_its_variable() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(FLAGS_FILE), "record_dvol = true\n").unwrap();
        let err = load(Some(tmp.path()), &HashMap::new()).expect_err("the FILE key is refused");
        assert!(err.to_string().contains("record_dvol"), "{err}");

        // …and the variable, on a tree with no such file: the load SUCCEEDS and says so once.
        let clean = tempfile::tempdir().unwrap();
        let env = HashMap::from([(crate::flags::RECORD_DVOL_ENV.to_string(), "1".to_string())]);
        let s = load(Some(clean.path()), &env).expect("the VARIABLE must not fail a load");
        assert!(
            s.warnings.iter().any(|w| w.contains(crate::flags::RECORD_DVOL_ENV)),
            "a set dead variable must be named once: {:?}",
            s.warnings
        );
        // An UNSET variable says nothing — a warning on every load is a warning nobody reads.
        let quiet = load(Some(clean.path()), &HashMap::new()).unwrap();
        assert!(quiet.warnings.is_empty(), "{:?}", quiet.warnings);
    }

    /// **The venue-catalog flip warns the operator whose value meant OFF, and ONLY that one.**
    ///
    /// ⚠ The asymmetry is the whole test and it is the INVERSE of the reconcile one above: the old
    /// grammar was *exact `"1"` is on*, so a `=1` still asks for what it now gets and must be
    /// SILENT, while a `0` meant OFF and silently became ON and must be LOUD. Getting it backwards
    /// would warn every correctly-configured legacy box and say nothing to the only operator who
    /// is wrong — which is why the two directions are pinned in one test rather than separately.
    ///
    /// ⚠ Mutation proof: drop the `.is_some_and(|v| v != "1")` guard to a bare `.is_some()` and the
    /// `=1` half goes red; delete the push and the `=0` half does; invert the comparison and both
    /// do.
    #[test]
    fn the_old_venue_catalog_arming_warns_only_where_its_value_meant_off() {
        let dir = tempfile::tempdir().unwrap();
        let load_with = |v: Option<&str>| {
            let env = v.map_or_else(HashMap::new, |v| {
                HashMap::from([(crate::flags::VENUE_CATALOG_ENV.to_string(), v.to_string())])
            });
            load(Some(dir.path()), &env).expect("a stale variable must never fail a load")
        };
        let warned = |s: &Settings| s.warnings.iter().any(|w| w.contains("venue_catalog_off"));

        // The one operator who is WRONG: `0` turned the lane off and now turns nothing off.
        let off = load_with(Some("0"));
        assert!(warned(&off), "a `0` must be told: {:?}", off.warnings);
        assert!(
            off.warnings.iter().any(|w| w.contains(crate::flags::VENUE_CATALOG_ENV)),
            "…and the warning must name where they wrote it: {:?}",
            off.warnings
        );
        // Any other non-`1` value is the same case.
        assert!(
            warned(&load_with(Some("true"))),
            "a truthy typo also meant OFF under the old reader"
        );

        // The operator who is RIGHT: `=1` asked for a served lane and has one.
        let on = load_with(Some("1"));
        assert!(!warned(&on), "a `=1` must be silent: {:?}", on.warnings);
        // A blank is unset, the rule `crate::layers::get` and `refuse_removed_env` both follow.
        let blank = load_with(Some(""));
        assert!(!warned(&blank), "a blank configured nothing then either: {:?}", blank.warnings);
        // …and an unset variable is the ordinary state.
        assert!(!warned(&load_with(None)));
    }

    /// **The REFUSAL resolves from the file and from the environment, and defaults to serving.**
    ///
    /// The half `the_old_venue_catalog_arming_warns_only_where_its_value_meant_off` cannot see:
    /// that warning is about a DEAD name, and this is the live one.
    #[test]
    fn the_venue_catalog_refusal_resolves_from_both_layers_and_defaults_off() {
        let clean = tempfile::tempdir().unwrap();
        assert!(
            !load(Some(clean.path()), &HashMap::new()).unwrap().flags.venue_catalog_off,
            "the guarded state is `false` — the lane SERVES unless refused"
        );

        let file = tempfile::tempdir().unwrap();
        std::fs::write(file.path().join(FLAGS_FILE), "venue_catalog_off = true\n").unwrap();
        assert!(load(Some(file.path()), &HashMap::new()).unwrap().flags.venue_catalog_off);

        let env =
            HashMap::from([(crate::flags::VENUE_CATALOG_OFF_ENV.to_string(), "1".to_string())]);
        assert!(load(Some(clean.path()), &env).unwrap().flags.venue_catalog_off);
    }

    #[test]
    fn settings_files_are_listed_in_application_order() {
        let files = settings_files(Path::new("/p/settings"));
        let names: Vec<_> =
            files.iter().map(|p| p.file_name().unwrap().to_str().unwrap()).collect();
        assert_eq!(names, ["policy.toml", "config.toml", "preferences.toml", "flags.toml"]);
        for f in &files {
            assert_eq!(f.parent(), Some(Path::new("/p/settings")));
        }
    }

    /// **This crate resolves no directory of its own.** The four TOMLs come from the directory it
    /// was HANDED, so a caller that hands it a temp directory can never reach a real one — the
    /// property that makes every test here safe to run on a configured box, and the reason there is
    /// no resolver to get wrong.
    #[test]
    fn the_settings_directory_is_the_one_the_caller_passed_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(POLICY_FILE), "max_notional_per_order = 250.0\n").unwrap();

        let s = load(Some(tmp.path()), &HashMap::new()).unwrap();
        assert_eq!(s.policy.max_notional_per_order, Some(250.0));

        // `None` skips the layer entirely; nothing is walked to and nothing is guessed.
        let s = load(None, &HashMap::new()).unwrap();
        assert_eq!(s.policy.max_notional_per_order, Policy::default().max_notional_per_order);
    }

    /// **The removed layer is refused by the LOADER, not by each root.**
    ///
    /// The check is layer 0 of `load_with_cli`, so there is no entry point into this crate that
    /// resolves a value while a `<project>/vike.toml` sits unread beside the settings directory —
    /// which is the shape the layer was in for the hour it existed, and the shape a per-root
    /// `refuse_*` call could drift back into one root at a time.
    #[test]
    fn a_present_project_file_fails_the_load_rather_than_being_ignored() {
        let project = tempfile::tempdir().unwrap();
        let dir = project.path().join("settings");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join(CONFIG_FILE), "log_dir = \"/from/file\"\n").unwrap();

        // Without it, the settings load exactly as they always did.
        let ok = load(Some(&dir), &HashMap::new()).unwrap();
        assert_eq!(ok.config.log_dir.as_deref(), Some(Path::new("/from/file")));

        std::fs::write(
            project.path().join(crate::removed::REMOVED_PROJECT_FILE),
            "[config]\nlog_dir = \"/from/project\"\n",
        )
        .unwrap();

        let err = load(Some(&dir), &HashMap::new()).unwrap_err();
        assert!(matches!(err, ConfigError::RemovedProjectFile { .. }), "{err}");
        // …and the CLI entry point, which is a different function, refuses identically.
        assert!(load_with_cli(Some(&dir), &HashMap::new(), &CliOverrides::default()).is_err());
    }
}
