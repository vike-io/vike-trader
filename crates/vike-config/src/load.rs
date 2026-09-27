//! [`Settings`] and [`load`] — the layered loader.
//!
//! ```text
//! code defaults -> the settings database's rows -> env -> CLI
//! ```
//!
//! **`docs/decisions/0086`: settings live only in the database, and there are no settings files any
//! more — as source, fallback, export or way back.** This module used to apply four settings-
//! directory TOML files between the database and the environment (`policy.toml` / `config.toml` /
//! `preferences.toml` / `flags.toml`), each with its own `_FILE` constant, and a `WHICH SOURCE
//! answers` probe that chose between the files and the store. All three are gone: **a box either
//! has a row for a key, or it does not — there is no second place to look.** The `<project>/vike.toml`
//! per-project override this crate's history also removed (see [`crate::removed`]) is a DIFFERENT,
//! independently-decided refusal and is unaffected by this change.
//!
//! **The root is a DIRECTORY and it is a PARAMETER.** `settings_dir` is `<project>/settings`; this
//! crate resolves no directory of its own, never walks, never expands `~` and never reads a platform
//! variable — `vike_model::state_path::project_settings_dir_from` is where the walk lives, and the
//! BINARY performs it. `settings_dir` is still meaningful under 0086: it is where
//! `<project>/settings/db/vike.db` lives, and it is what [`crate::removed::refuse_removed_project_file`]
//! probes beside. The env map is a parameter too, so the whole loader is a pure function of its
//! arguments.
//!
//! **The Adoption/seal machinery survives 0086 unchanged in kind, changed in MEANING.** It is no
//! longer *"has an operator crossed over from files"* — every write now moves it
//! (`vike_secrets::write_setting_row_in`) — it is the store's own integrity check: do the counts a
//! past write sealed still match the tables now. See [`crate::source`] and
//! [`crate::mirror::apply_rows`].
//!
//! **A key with no row is not an error.** A store with no `setting` row for a key, or no database at
//! all, contributes nothing and the compiled-in default stands — that is what makes
//! `load(None, &HashMap::new())` the pure code-default answer.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{ConfigError, key_from_parse_message, redacted_parse_message};
use crate::flags::Flags;
use crate::layers::{CliOverride, CliOverrides, EnvOverride};
use crate::policy::Policy;
use crate::preferences::Preferences;
use crate::source::StoreLayer;

/// The fully-resolved settings for this process.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Settings {
    /// Hard ceilings. Defaults + the settings database only — see [`Policy`].
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
    /// Producers today:
    ///
    /// * [`NO_SETTINGS_DIRECTORY_WARNING`] — a load handed no settings directory at all.
    ///   `docs/ops/kill-switches.md` carried that silence as a register entry ("a missing settings
    ///   directory is silently uncapped"): a process that resolved no project gets
    ///   `Policy::default()`, which is every ceiling absent and every venue capped `paper`, and said
    ///   so nowhere. That is indistinguishable from a store that deliberately holds no row, and the
    ///   two want opposite reactions from an operator.
    /// * A WRITTEN reconcile refusal that S2 no longer honours — `reconcile = false` in a `flags`
    ///   ROW ([`crate::mirror::apply_rows`], which performs the same read-before-apply for the same
    ///   reason), `VIKE_RECONCILE=0` in the environment, or `--reconcile false` on the command line.
    ///   See [`crate::flags::reconcile_refusal_ignored`], which carries the argument; this frame is
    ///   the LAST one that can tell an explicit `false` from an unset value, because the resolved
    ///   field is a `bool`. It goes when `reconcile` stops being a flag at all.
    ///
    /// The disposition is unchanged and applies to both: a warning is deleted WITH the defect it
    /// describes, never left to go stale.
    pub warnings: Vec<String>,
    /// **The settings store could not be read at all, and this says so** — set when and only when
    /// [`load_with_source`] met [`StoreLayer::Unreadable`].
    ///
    /// It is a SECOND channel rather than one more `warnings` line (the refusal is pushed there too)
    /// because a consumer has to be able to REFUSE on it: `vike-cli` keeps every `config`/`secrets`
    /// verb running in this state — that is the JSON incident's lesson, that a refusal must not brick
    /// the tool that diagnoses it — while `trade` and `mcp` refuse, and a scan of free-text warnings
    /// is not a thing a verb may gate on.
    pub store_refusal: Option<String>,

    /// **The store opened and said something ILLEGAL** — the seal's counts do not match its tables,
    /// a row will not parse, or a row names a key the section rejects. Set by
    /// [`crate::mirror::apply_rows`].
    ///
    /// ⚠ **A SEPARATE channel from [`Settings::store_refusal`], because the two mean opposite things
    /// about the same file and want opposite repairs.** `store_refusal` is *this layer could not be
    /// READ*, so the values below are compiled-in defaults and merely unverified. This one is *this
    /// layer WAS read and is not trustworthy*, and there is no file underneath to fall back to — so
    /// the values below are whatever the rows still resolved, possibly compiled-in defaults, which
    /// for `policy.max_notional_per_order` means NO CEILING.
    ///
    /// **Who must refuse on it, and who must not.** The diagnostic verbs — `config show`, every
    /// `secrets` verb — keep running, because a refusal that takes down its own diagnosis is the JSON
    /// incident. The surfaces that could ACT on an unsound ceiling refuse: `vike-cli trade`,
    /// `vike-cli mcp`, `vike-cli config check`, and `vike-tradehub` itself
    /// ([`docs/decisions/0069`](../../../docs/decisions/0069-an-unsound-seal-refuses-the-ordering-root-and-nothing-else.md)).
    /// By ruling, the ONLY repair for an unsound store is restoring `vike.db` from the box's nightly
    /// backup — no repair command is built (`docs/decisions/0086`).
    pub seal_refusal: Option<String>,
}

impl Settings {
    /// Record a seal refusal on BOTH channels — the gated one a verb refuses on, and `warnings`,
    /// which is what every root already prints at startup.
    ///
    /// ⚠ **The FIRST refusal wins and later ones append rather than replace.** A store can fail its
    /// integrity check AND then carry an unparsable row, and the first is usually the cause of the
    /// second; overwriting would report the symptom and hide it.
    pub fn mark_seal_refusal(&mut self, why: String) {
        self.warnings.push(why.clone());
        match &mut self.seal_refusal {
            Some(existing) => {
                existing.push_str("\n\nand also: ");
                existing.push_str(&why);
            }
            slot @ None => *slot = Some(why),
        }
    }
}

/// The no-settings-directory warning: [`load_with_cli`] was handed no settings directory, so every
/// ceiling on this process is the compiled-in default and no database could even be opened.
///
/// **Why this is a warning and not an error.** A missing project is the ordinary state of a dev
/// checkout run from the wrong directory and of a container whose bind mount did not land, and
/// refusing to start over it would strand an operator with a daemon that will not boot instead of one
/// that boots capped at `paper` (`docs/decisions/0013-degrade-vs-refuse.md`). What it may not do is
/// stay SILENT: `Policy::default()` is a real, permissive-looking answer that an operator's own
/// `config show` then attributes to nothing at all.
///
/// Held as a constant rather than formatted at the site because two binaries assert on it and a
/// message they match on by substring is a message that must have one spelling.
pub const NO_SETTINGS_DIRECTORY_WARNING: &str = "no settings directory resolved — every ceiling is the compiled-in default and every venue is \
     capped `paper`. Set VIKE_SETTINGS_DIR, or run from a project that has one.";

/// Load the settings, applying every layer in precedence order.
///
/// * `settings_dir` — `<project>/settings`, or `None` to skip that layer entirely (and say so on
///   [`Settings::warnings`] — see [`NO_SETTINGS_DIRECTORY_WARNING`]). Consulted only for the removed
///   `<project>/vike.toml` refusal — see [`crate::removed`] — since this crate opens no settings file
///   of its own any more.
/// * `env` — an already-loaded environment map. A caller passes `std::env::vars().collect()`,
///   the credential map, or a merge of both; that choice belongs to the binary, not here.
///
/// Returns [`ConfigError`] naming the offending key on the first layer that fails — including a
/// REMOVED key, which is refused by name rather than ignored, and a REMOVED FILE, the retired
/// `<project>/vike.toml`. Non-fatal resolutions ride [`Settings::warnings`].
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
    load_with_source(settings_dir, StoreLayer::NotConsulted(NOT_A_ROOT), env, cli)
}

/// What [`load`] and [`load_with_cli`] declare when they hand [`load_with_source`] no store.
///
/// A named constant rather than a literal at two call sites, because it is the ONE thing a reader
/// of either signature has to be told: these two loaders resolve no store and are therefore not a
/// composition root's entry point. `crates/vike-boot/tests/one_owner.rs` marks both `Owned::Reserved`
/// for exactly this reason, and lists the three library callers that use them legitimately.
const NOT_A_ROOT: &str = "`vike_config::load`/`load_with_cli` resolve no settings store: a composition root calls \
     `vike_boot::boot`, which calls `load_with_source` with the arm it read";

/// **[`load_with_cli`] plus the settings DATABASE** — the whole of what a settings key resolves
/// from, per `docs/decisions/0086`.
///
/// `source` is DATA the BINARY already read (`vike_secrets::read_settings_in`), never opened by this
/// crate — its own manifest says so, and 0057/0086 both give the reason under one database: a handle
/// that reaches the settings rows reaches the `credential` table too.
///
/// # What it REFUSES, and what it hands back as data
///
/// A row that says something ILLEGAL — an unknown key, a broken bound, a tombstone, a `null`, an
/// integer outside the store's range — or a seal whose counts do not match is MARKED on
/// [`Settings::seal_refusal`] rather than returned as an `Err`: a refusal that reaches this
/// function's own return type would take `vike-cli config show` and every `secrets` verb down with
/// the box it is supposed to help diagnose. The surfaces that can ACT on a ceiling
/// (`vike-cli trade`/`mcp`/`config check`, `vike-tradehub` itself) refuse on that mark instead.
///
/// A store that **could not be read at all** is different again and is also NOT an error return: it
/// comes back as [`Settings::store_refusal`], every key falling back to its compiled-in default. The
/// measurement that settles it is at the arm itself: on this tree an unreadable store also means an
/// empty CREDENTIAL map and therefore an all-paper mount, so a hard refusal here would take a daemon
/// down without preventing anything. `vike-cli config check` answers `Level::Fail` for this state,
/// which puts the stop at a deploy pre-flight rather than at a running daemon.
pub fn load_with_source(
    settings_dir: Option<&Path>,
    source: StoreLayer<'_>,
    env: &HashMap<String, String>,
    cli: &CliOverrides,
) -> Result<Settings, ConfigError> {
    // Layer 0 — refuse a REMOVED layer before resolving a single value. A DIFFERENT, independently
    // decided refusal from the settings-file removal this module carries: `<project>/vike.toml` was
    // a fifth override file one level ABOVE `<project>/settings/`, and its refusal survives 0086
    // untouched — it has no row analogue and no file-authority question to answer.
    crate::removed::refuse_removed_project_file(settings_dir)?;

    // Layer 1 — code defaults.
    let mut settings = Settings::default();

    if settings_dir.is_none() {
        // NO settings directory at all: `policy` stays `Policy::default()` — no notional ceiling, no
        // dead-man, no `[venues]` table. Every one of those is a REFUSAL rather than an arming, so
        // this cannot leak a live order — but it is also invisible, and that invisibility was
        // `docs/ops/kill-switches.md`'s register entry. Deliberately NOT extended to "a directory
        // with no rows in it": that is the ordinary shape of a configured box that has not written
        // ceilings yet, and warning on it every boot is how a warning stops being read. The
        // distinction being drawn is between "you told me nothing" and "I never found your project".
        settings.warnings.push(NO_SETTINGS_DIRECTORY_WARNING.to_string());
    }

    // Layer 1.5 — the settings DATABASE. The ONLY settings layer below env/CLI; there is no file
    // underneath it to fall back to any more.
    match source {
        StoreLayer::Rows { rows, adopted } => {
            crate::mirror::apply_rows(&mut settings, rows, adopted);
        }
        StoreLayer::Unreadable(why) => {
            // ⚠ **NOT a refusal: resolve WITHOUT it, and MARK.** The measurement that settles this
            // arm: `crates/vike-secrets/src/store.rs`'s `Backend` decides which store answers for a
            // CREDENTIAL on the same file's existence, and a present-but-unopenable store returns an
            // EMPTY credential map. An empty credential map IS the live gate, so an unreadable
            // settings store always co-occurs with an all-paper mount. The hazard a refusal would
            // protect against — armed and uncapped — is unreachable from this arm, and the cost of
            // refusing here is real: it would take down `vike-cli config show` and every `secrets`
            // verb along with the daemon whose credential half already degrades and announces for
            // exactly this state. The ENFORCEMENT POINT is `vike-cli config check`'s `Level::Fail`.
            settings.store_refusal = Some(format!(
                "the settings database could not be read ({why}). Every key resolves to its \
                 compiled-in default until the store can be opened again — there are no settings \
                 files to fall back to. Restore it from the box's nightly backup if this persists."
            ));
            settings.warnings.push(settings.store_refusal.clone().unwrap_or_default());
        }
        StoreLayer::NoDatabase | StoreLayer::TablesAbsent | StoreLayer::NotConsulted(_) => {}
    }

    // Layer 2 — the environment. `settings.policy` is absent from this block, and cannot be added
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

    // …and the DEAD-FLAG variables. Their ROW halves are refused by name in `Flags::apply`
    // (`flags::DEAD_FLAG_KEYS`); this is the other half — a variable whose only reader was
    // `apply_env`, which went with the field it set. It cannot be a refusal (`crate::REMOVED_ENV`
    // would stop a correct daemon dead over a spelling that never configured anything) and it must
    // not be silence (the operator who exported it believes something is recording). `get` skips an
    // empty value, exactly as the layer itself does.
    if crate::layers::get(env, crate::flags::RECORD_DVOL_ENV).is_some() {
        settings.warnings.push(crate::flags::dead_flag_env_ignored(crate::flags::RECORD_DVOL_ENV));
    }

    // ...and the venue-catalog flip's own written-refusal check
    // (`docs/decisions/0066`'s decision 4). ONE origin, not four, and that is not an oversight:
    // `VIKE_DATAHUB_VENUE_CATALOG` was never a settings key or a settings-database row, so the
    // environment is the only place an operator can have written it.
    if crate::layers::get(env, crate::flags::VENUE_CATALOG_ENV).is_some_and(|v| v != "1") {
        settings.warnings.push(crate::flags::venue_catalog_refusal_ignored(&format!(
            "a non-`1` `{}` in the environment",
            crate::flags::VENUE_CATALOG_ENV
        )));
    }

    // Layer 3 — CLI. Same three types, same structural exclusion of policy.
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

    Ok(settings)
}

/// The settings-file reading this module used to do (read-whole-file, then parse) is GONE with the
/// four `_FILE` constants it read — there are no settings files to open any more (this module's
/// doc). What survives is the PARSE half, shared with `crate::mirror`'s raw-table probes and
/// `crate::profile_risk`'s run-profile reader, since a run/daemon PROFILE remains a document read
/// from disk under 0086 (its *Phasing* leaves the profile plane as a going concern for a separate
/// migration).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_env_and_no_store_is_exactly_the_code_defaults() {
        let s = load(None, &HashMap::new()).unwrap();
        assert_eq!(s.policy, Policy::default());
        assert_eq!(s.config, Config::default());
        assert_eq!(s.preferences, Preferences::default());
        assert_eq!(s.flags, Flags::default());
        assert_eq!(s.warnings, vec![NO_SETTINGS_DIRECTORY_WARNING.to_string()]);
    }

    /// A load handed NO settings directory says so, and a load handed one does not.
    ///
    /// The pair is the whole point: a warning that fires on every load is noise an operator learns
    /// to skip, and one that fires on no load is the silence this closes. `docs/ops/kill-switches.md`
    /// carried it as a register entry — a process that resolved no project runs on
    /// `Policy::default()`, which is no ceiling anywhere, and nothing anywhere said so.
    #[test]
    fn a_load_with_no_settings_directory_says_so_and_one_with_a_directory_does_not() {
        let none = load(None, &HashMap::new()).unwrap();
        assert!(
            none.warnings.iter().any(|w| w == NO_SETTINGS_DIRECTORY_WARNING),
            "a load that resolved no project must SAY that its ceilings are defaults: {none:?}"
        );

        // A real directory, empty of everything: the distinction is "I found no project", never
        // "your project wrote no ceilings", so a settings directory with no rows must still be
        // silent here.
        let tmp = tempfile::tempdir().unwrap();
        let some = load(Some(tmp.path()), &HashMap::new()).unwrap();
        assert!(
            some.warnings.is_empty(),
            "a settings directory that exists resolves the project, whatever it holds: {some:?}"
        );
    }

    /// **A DEAD flag's two spellings get two DIFFERENT answers, and both are answers.**
    ///
    /// The row key is a hard refusal naming the key (`Flags::apply`, tested at its own site); the
    /// variable is a WARNING and the load succeeds.
    #[test]
    fn a_dead_flag_refuses_its_row_key_and_warns_about_its_variable() {
        let tmp = tempfile::tempdir().unwrap();
        let rows = vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "flags".to_string(),
                key: "record_dvol".to_string(),
                value: "true".to_string(),
            }],
            ..Default::default()
        };
        let resolved = load_with_source(
            Some(tmp.path()),
            StoreLayer::Rows { rows: &rows, adopted: None },
            &HashMap::new(),
            &CliOverrides::default(),
        )
        .expect("a row problem MARKS rather than hard-erroring — see `crate::mirror::apply_rows`");
        let refusal = resolved.seal_refusal.expect("the ROW key must be marked illegal");
        assert!(refusal.contains("record_dvol"), "{refusal}");

        // …and the variable, on a store with no such row: the load SUCCEEDS and says so once.
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

    /// **The REFUSAL resolves from the environment, and defaults to serving.**
    #[test]
    fn the_venue_catalog_refusal_resolves_from_the_environment_and_defaults_off() {
        let clean = tempfile::tempdir().unwrap();
        assert!(
            !load(Some(clean.path()), &HashMap::new()).unwrap().flags.venue_catalog_off,
            "the guarded state is `false` — the lane SERVES unless refused"
        );

        let env =
            HashMap::from([(crate::flags::VENUE_CATALOG_OFF_ENV.to_string(), "1".to_string())]);
        assert!(load(Some(clean.path()), &env).unwrap().flags.venue_catalog_off);
    }

    /// **This crate resolves no directory of its own** for settings resolution: with no store
    /// consulted, a settings directory changes nothing about the resolved ceilings — it is consulted
    /// only for the removed-project-file refusal.
    #[test]
    fn a_settings_directory_alone_resolves_nothing_without_a_store() {
        let tmp = tempfile::tempdir().unwrap();
        let s = load(Some(tmp.path()), &HashMap::new()).unwrap();
        assert_eq!(s.policy.max_notional_per_order, Policy::default().max_notional_per_order);
    }

    /// **The removed layer is refused by the LOADER, not by each root.**
    ///
    /// The check is layer 0 of `load_with_cli`, so there is no entry point into this crate that
    /// resolves a value while a `<project>/vike.toml` sits unread beside the settings directory.
    #[test]
    fn a_present_project_file_fails_the_load_rather_than_being_ignored() {
        let project = tempfile::tempdir().unwrap();
        let dir = project.path().join("settings");
        std::fs::create_dir(&dir).unwrap();

        // Without it, the settings load exactly as they always did.
        assert!(load(Some(&dir), &HashMap::new()).is_ok());

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
