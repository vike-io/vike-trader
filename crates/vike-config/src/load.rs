//! [`Settings`] and [`load`] — the layered loader.
//!
//! ```text
//! code defaults -> <project>/settings/policy.toml
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
//! zero-dependency twin `vike_secrets::project_settings_dir_from`) is where the walk lives, and the
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
    /// ⚠ **NO producer today, again — and getting back here is the good outcome.** Its most recent
    /// one was [`load_with_cli`]'s admission that an `[accounts]` table bound nothing yet; the mount
    /// now folds those ceilings per account (`vike_mount::account_ceiling`), so the admission was
    /// deleted with the defect it described rather than left to go quietly stale.
    ///
    /// The channel predates that producer and was KEPT while it had none, deliberately: an empty
    /// warnings list PRINTS NOTHING and therefore claims nothing, where a dead SETTING is displayed
    /// as effective and attributed to the operator's own file — positive confirmation of something
    /// false. (Its ORIGINAL producer was the policy-clamps-preference edge, `rate.max_utilization`
    /// over `rate_utilization`, and both halves were removed as a ceiling bounding a value nothing
    /// read — see [`crate::preferences`]' module doc. Deleting the channel then would have meant
    /// this loader adjustment landing with three binaries having no way to surface it, which is
    /// exactly the state the clamp warning was in until Phase 6c: swallowed.)
    pub warnings: Vec<String>,
}

/// Load the settings, applying every layer in precedence order.
///
/// * `settings_dir` — `<project>/settings`, or `None` to skip that layer entirely. Absent files
///   inside it are skipped individually.
/// * `env` — an already-loaded environment map. A caller passes `std::env::vars().collect()`,
///   the credential map, or a merge of both; that choice belongs to the binary, not here.
///
/// Returns [`ConfigError`] naming the offending file and key (or env variable and value) on the
/// first layer that fails — including a REMOVED key, which is refused by name rather than ignored
/// (`policy.toml`'s `max_total_exposure` and `[rate] max_utilization`, `preferences.toml`'s
/// `rate_utilization`), and a REMOVED FILE, the retired `<project>/vike.toml`. Non-fatal
/// resolutions ride [`Settings::warnings`]; there is no producer today (see that field).
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
    // Layer 0 — refuse a REMOVED layer before resolving a single value. It sits here, inside the
    // one function every composition root goes through, rather than beside each root's
    // `refuse_removed_env` call: a root that could forget a parameter can equally forget a call,
    // and that is precisely how the file came to be advertised-but-unread in the first place.
    crate::removed::refuse_removed_project_file(settings_dir)?;

    // Layer 1 — code defaults.
    let mut settings = Settings::default();

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
            settings.flags.apply(patch);
        }
    }

    // Layer 3 — the environment. `settings.policy` is absent from this block, and cannot be added
    // to it: `Policy` implements neither `EnvOverride` nor any inherent `apply_env`, so the line
    // would not compile. See `crate::layers`.
    settings.config.apply_env(env)?;
    settings.preferences.apply_env(env)?;
    settings.flags.apply_env(env)?;

    // Layer 4 — CLI. Same three types, same structural exclusion of policy.
    settings.config.apply_cli(cli)?;
    settings.preferences.apply_cli(cli)?;
    settings.flags.apply_cli(cli)?;

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
        assert!(s.warnings.is_empty());
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
