//! **Settings that USED to configure something and no longer do** — refused by name, never ignored.
//!
//! Two of them: [`refuse_removed_env`] for the environment variables that carried a risk ceiling,
//! and [`refuse_removed_project_file`] for the whole per-project override FILE. Same argument in
//! both cases, and it is the only argument this module makes.
//!
//! Phase 5 of the settings-unification design
//! (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`) is the one phase that
//! changes behaviour: the risk ceilings move into [`crate::Policy`], and **their environment
//! overrides are deleted**. A ceiling you can raise from the environment is not a ceiling —
//! anyone who can set env on the box (a shell export, a stale systemd unit, a CI script, an
//! inherited parent process) silently widens the limit, with no file changed, no diff, no review,
//! and a run that looks completely normal.
//!
//! ## Why a removed variable must ERROR rather than be ignored
//!
//! Deleting the read is only half the job. The operator who set `VIKE_MAX_ORDER_NOTIONAL=250`
//! believes a 250-unit ceiling is active. If the new build simply stops reading it, that belief
//! becomes silently false and the process trades **uncapped** — strictly worse than either keeping
//! the variable or refusing to start. So a set-but-removed variable is a **startup refusal** that
//! names the exact file and key that replaces it, with the operator's own value rendered into a
//! copy-pasteable TOML line.
//!
//! This is the same argument [`crate::flags`] makes for rejecting a truthy typo, and the same one
//! [`crate::policy::PolicyPatch`]'s `deny_unknown_fields` makes for rejecting a mistyped key: a
//! setting that silently does nothing is the failure mode worth engineering against.
//!
//! ## What counts as "set"
//!
//! Present with a **non-empty** value after trimming. An empty (`VAR=`) or whitespace-only value
//! never configured anything under the old readers either — every one of them parsed the string as
//! `f64` and fell back to the permissive default — so nobody can believe a ceiling was active from
//! one, and refusing to start over a leftover blank line in a unit file would convert a harmless
//! artefact into an outage.
//!
//! ## I/O ownership
//!
//! The map is a PARAMETER, like everywhere else in this crate: the BINARY collects
//! `std::env::vars()` (and/or the workspace `.env`) and calls this before it starts anything. See
//! [`crate::load`].
//!
//! ## The removed FILE — `<project>/vike.toml`
//!
//! [`REMOVED_PROJECT_FILE`] was a fifth settings file, sitting at the project ROOT and overriding
//! `[config]` and `[preferences]` from `<project>/settings/*.toml`. It is gone, and the reason is
//! the reason `<project>/settings/` exists at all: twelve PRs consolidated every setting,
//! credential and state file into ONE directory with four files of clear ownership, and a fifth
//! file one level ABOVE that directory reintroduces the question the consolidation removed — *which
//! file won?* That question is where real defects hide, which this crate's own history demonstrates
//! twice over ([`crate::consumed`] for a key nothing reads, and the layer itself, which was
//! implemented, tested and printed in an operator-facing precedence header while no binary read
//! one).
//!
//! ⚠ **Removing the reader is the dangerous half, exactly as it is for a variable.** The file was
//! genuinely wired for the hour between the change that wired it and the change that removed the
//! layer, so an operator may have one on disk that TOOK EFFECT. Ignoring it would make their belief
//! silently false — the identical failure mode `VIKE_MAX_ORDER_NOTIONAL` gets refused for. So a
//! present one is a startup REFUSAL naming the file and both destinations.
//!
//! ⚠ **And a path that cannot be PROBED refuses too — but says so, rather than claiming the file
//! is there.** Fail-closed is right (absence must be established, never assumed); a fail-closed
//! verdict dressed up as a positive finding is not. [`RemovedFileProbe`] carries which of the two
//! happened, and [`removed_project_file_message`] writes a different message for each.
//!
//! ⚠ **Nothing here deletes, moves or rewrites it.** It is the operator's file, holding their
//! keys; a loader that "helpfully" migrated it would be making an edit nobody reviewed. The message
//! says what to move where, and stops.
//!
//! Unlike [`refuse_removed_env`], this one is NOT a call a composition root has to remember: it is
//! folded into [`crate::load_with_cli`], so every binary that loads settings at all performs it.
//! That placement is deliberate and it is the lesson of the layer being removed — a root that can
//! forget a parameter can equally forget a call, and there is no reason to leave a fourth chance to
//! forget lying around.

use std::collections::HashMap;
use std::path::Path;

use crate::error::ConfigError;
use crate::load::{CONFIG_FILE, FLAGS_FILE, POLICY_FILE, PREFERENCES_FILE};

/// The per-project override file that USED to sit above `<project>/settings/*.toml`.
///
/// Kept as a named constant even though nothing loads it any more: it is the string the refusal,
/// the gate in `crates/vike-config/tests/layers_are_reachable.rs` and
/// `crates/vike-cli/tests/settings_layers_reachable.rs` all have to agree on, and a tombstone
/// spelled once cannot drift from the message that names it.
pub const REMOVED_PROJECT_FILE: &str = "vike.toml";

/// Refuse to start when a `<project>/vike.toml` is present. `Ok(())` is the overwhelmingly common
/// case — nobody has one.
///
/// `settings_dir` is `<project>/settings`, so the project is its PARENT: one [`Path::parent`] call,
/// which resolves no directory of its own (this crate never walks, never expands `~`, never reads a
/// platform variable — see [`crate::load`]). A `None` settings directory means there is no project
/// to look in, and a settings directory at a filesystem root has no parent; both skip the check,
/// because there is no file that could be misleading anybody.
///
/// ⚠ The probe is [`std::fs::metadata`] and only `NotFound` counts as absent. Any other error means
/// we could not ESTABLISH absence, and a file we cannot see is precisely the one an operator would
/// believe is in force — so it refuses and names the path, the same reasoning `read_toml` uses for
/// preferring a failed read over a prior `Path::exists()`.
///
/// ⚠ **Which is why the outcome is CARRIED rather than collapsed.** Fail-closed is the right
/// verdict and it is not changing; asserting *"this file is present"* on the strength of it is not.
/// See [`RemovedFileProbe`].
pub(crate) fn refuse_removed_project_file(settings_dir: Option<&Path>) -> Result<(), ConfigError> {
    let Some(project) = settings_dir.and_then(Path::parent) else { return Ok(()) };
    let file = project.join(REMOVED_PROJECT_FILE);
    match std::fs::metadata(&file) {
        Ok(_) => Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Present }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => {
            Err(ConfigError::RemovedProjectFile { file, probe: RemovedFileProbe::Unestablished(e) })
        }
    }
}

/// **What the stat actually established.** The two answers that are not `NotFound`, kept apart
/// because only ONE of them is a statement about a file that exists.
///
/// Both REFUSE — [`refuse_removed_project_file`] is fail-closed and stays that way, for the reason
/// its own doc gives. What they must not share is the message. Under a systemd unit's
/// `ProtectHome=yes` a stat of any path below `/home`, `/root` or `/run/user` returns **`EACCES`,
/// not `ENOENT`** (measured on the CI box), so a project root inside a user's home produced a hard
/// startup refusal that named a `vike.toml` **which did not exist** and told the operator to delete
/// it. Neither half of that is a cosmetic defect: it asserts something false, it names none of the
/// causes that actually produce it, and — because this check is layer 0 of [`crate::load_with_cli`]
/// — it fires before anything else in boot, so a false one MASKS whatever the real problem was.
///
/// The shape is `vike_data`'s `unwritable_store_root`, one crate over: a sandbox errno is decorated
/// with the unit directive that causes it and the directive that fixes it, rather than surfacing as
/// a bare `os error 13` pointing at the disk.
#[derive(Debug)]
pub enum RemovedFileProbe {
    /// [`std::fs::metadata`] SUCCEEDED, so something really is there under that name — a file, or
    /// a directory somebody created by mistake, which is at least as confusing. This is the answer
    /// that has earned the right to say *is present* and *delete it*.
    Present,
    /// The stat failed with something other than `NotFound`: absence was not established, and
    /// neither was presence.
    ///
    /// Carries the [`std::io::Error`] verbatim rather than a rendered string — the errno is the
    /// operator's first clue, and it is what [`std::error::Error::source`] hands a programmatic
    /// caller.
    Unestablished(std::io::Error),
}

/// The whole operator-facing refusal for a [`REMOVED_PROJECT_FILE`], ready to print — one message
/// per [`RemovedFileProbe`] answer.
///
/// Split from the error variant so the text lives beside the argument it makes, and so a test can
/// assert the message without constructing a filesystem.
pub(crate) fn removed_project_file_message(file: &Path, probe: &RemovedFileProbe) -> String {
    match probe {
        RemovedFileProbe::Present => present_project_file_message(file),
        RemovedFileProbe::Unestablished(e) => unestablished_project_file_message(file, e),
    }
}

/// The refusal for a file that IS there. Says so, and says what to do with it.
fn present_project_file_message(file: &Path) -> String {
    format!(
        "{path} is present, but the per-project override layer is NO LONGER READ — removed because \
         a fifth settings file above <project>/settings/ reintroduces the `which file won?` \
         question that directory exists to answer.\n\
         Move its [config] keys into <project>/settings/{CONFIG_FILE} and its [preferences] keys \
         into <project>/settings/{PREFERENCES_FILE} (drop the table headers — those files are \
         already scoped to one table each), then delete {path}.\n\
         [policy] and [flags] tables were never accepted in it; they belong in \
         <project>/settings/{POLICY_FILE} and <project>/settings/{FLAGS_FILE}.\n\
         Nothing has been moved or deleted for you: it is your file.",
        path = file.display(),
    )
}

/// The refusal for a path that could not be PROBED. Same fail-closed verdict, and it claims
/// nothing at all about the file — because nothing is known about it.
///
/// The likeliest causes are named because the errno alone sends an operator hunting for a file that
/// is very probably not there: `ProtectHome=` and `ProtectSystem=` are the two unit directives that
/// turn "nothing here" into `Permission denied`, and `VIKE_SETTINGS_DIR` is the way to point the
/// process at a project it can actually see. The relocation instructions from
/// [`present_project_file_message`] are still offered, but conditionally — *if* it really is there.
fn unestablished_project_file_message(file: &Path, error: &std::io::Error) -> String {
    format!(
        "{path} could not be STATTED: {error} — this is NOT the same as absent, and it is NOT \
         evidence that the file is there. The per-project override layer is NO LONGER READ, and a \
         path whose absence cannot be ESTABLISHED refuses rather than being assumed empty: a file \
         nobody can see is precisely the one an operator would believe is in force.\n\
         ⚠ It may not exist at all. Under a systemd unit the likeliest cause is the sandbox rather \
         than a leftover file: ProtectHome=yes makes /home, /root and /run/user unreachable, so a \
         stat below them fails with `Permission denied` whether or not anything is there, and \
         ProtectSystem=strict does the same outside what ReadWritePaths= / ReadOnlyPaths= name. \
         Give the unit access to the project directory {project}, relax ProtectHome= to read-only, \
         or name a project the process can see with VIKE_SETTINGS_DIR. Otherwise it is ordinary \
         permissions: a parent directory this process cannot search (no `x` bit) fails identically.\n\
         If {path} really is there, it is the removed layer: move its [config] keys into \
         <project>/settings/{CONFIG_FILE} and its [preferences] keys into \
         <project>/settings/{PREFERENCES_FILE}, then delete it.\n\
         Nothing has been moved or deleted for you.",
        path = file.display(),
        project = file.parent().unwrap_or(Path::new(".")).display(),
    )
}

/// One environment variable that has been REMOVED, and where its value lives now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedSetting {
    /// The variable that is no longer read.
    pub var: &'static str,
    /// The file that replaces it, inside `<project>/settings/` (e.g. `policy.toml`).
    pub file: &'static str,
    /// The key inside that file (e.g. `max_notional_per_order`), when the VALUE moved.
    ///
    /// `None` when the variable configured something that no longer exists at all: [`Self::file`]
    /// still names where the answer comes from, but there is no line for the operator to paste.
    pub key: Option<&'static str>,
    /// Which change removed it — so the message dates itself and can be searched for.
    pub removed_in: &'static str,
    /// One sentence on WHY it was removed, printed with the refusal. An operator who is told only
    /// "this moved" will re-add the variable somewhere else; one who is told a ceiling must not be
    /// env-settable will not.
    pub why: &'static str,
    /// Whether the variable's VALUE may appear in the refusal.
    ///
    /// `false` for anything that is ITSELF a secret. A refusal that echoes one turns a startup
    /// error into a credential in every log that captured stderr, which is a worse outcome than the
    /// misconfiguration it is reporting. A number the operator has to re-type into a TOML line is
    /// the case for `true`.
    pub echo_value: bool,
}

/// The removed variables. Every entry is refused at startup by [`refuse_removed_env`].
///
/// The first two carried the SAME idea — a per-order notional ceiling — under two names, one for
/// the GUI (`vike-app`, via `vike_app_core::order_entry::OrderLimits`, plus `vike-cli`'s advisory
/// client-side guardrail) and one for the headless daemon (`vike-tradehub`'s server-edge
/// `ControlLimitsConfig`). Both now read [`crate::Policy::max_notional_per_order`], which is the
/// point: one key, one file, one authority.
pub const REMOVED_ENV: &[RemovedSetting] = &[
    RemovedSetting {
        var: "VIKE_MAX_ORDER_NOTIONAL",
        file: "policy.toml",
        key: Some("max_notional_per_order"),
        removed_in: "Phase 5 (settings unification)",
        why: "an order-size ceiling any exported variable can raise is not a ceiling",
        echo_value: true,
    },
    RemovedSetting {
        var: "VIKE_TRADEHUB_MAX_ORDER_NOTIONAL",
        file: "policy.toml",
        key: Some("max_notional_per_order"),
        removed_in: "Phase 5 (settings unification)",
        why: "an order-size ceiling any exported variable can raise is not a ceiling",
        echo_value: true,
    },
    RemovedSetting {
        var: "VIKE_SECRETS_PASSPHRASE",
        file: "secrets.env",
        key: None,
        removed_in: "the one-store change (settings unification)",
        why: "nothing consumes it — credentials are read from the project's own store, in plaintext",
        echo_value: false,
    },
];

/// Refuse to start when a REMOVED variable is set. `Ok(())` is the overwhelmingly common case.
///
/// The `Err` string is the whole operator-facing message, ready to print: one block per offending
/// variable, each naming the variable, the file, the key, and the exact line to write. Every
/// offender is reported in ONE pass — fixing a stale unit file one restart at a time is a worse
/// experience than being handed the full list.
///
/// `String` rather than [`crate::ConfigError`] on purpose: every `ConfigError` variant is shaped to
/// name a file that was read or a layer that failed, and this failure is neither — nothing was
/// read, and the thing to fix is the process environment.
pub fn refuse_removed_env(vars: &HashMap<String, String>) -> Result<(), String> {
    let offenders: Vec<(&RemovedSetting, &str)> = REMOVED_ENV
        .iter()
        .filter_map(|r| {
            let raw = vars.get(r.var)?.trim();
            if raw.is_empty() {
                return None;
            }
            Some((r, raw))
        })
        .collect();
    if offenders.is_empty() {
        return Ok(());
    }

    let mut out = String::new();
    for (r, raw) in &offenders {
        if !out.is_empty() {
            out.push('\n');
        }
        let (var, file) = (r.var, r.file);
        // The value is echoed only where the row says it may be — see `RemovedSetting::echo_value`.
        let setting = if r.echo_value { format!("{var}={raw}") } else { var.to_string() };
        out.push_str(&format!(
            "{setting} is set, but {var} is NO LONGER READ — removed in {removed_in}, \
             because {why}.\n",
            removed_in = r.removed_in,
            why = r.why,
        ));
        match r.key {
            Some(key) => {
                out.push_str(&format!("Set it in <project>/settings/{file} instead:\n\n"));
                out.push_str(&format!("    {key} = {}\n\n", toml_value(raw)));
            }
            None => {
                out.push_str(&format!(
                    "<project>/settings/{file} is the only file consulted for this.\n\n"
                ));
            }
        }
        out.push_str(&format!("then unset {var}.\n"));
    }
    Err(out)
}

/// Render the operator's own value into the suggested TOML line when it is a value the key would
/// actually accept, else a placeholder.
///
/// Echoing garbage back (`max_notional_per_order = nope`) would hand over a line that fails the
/// loader with a *second*, unrelated error — and a non-positive number is rejected by
/// [`crate::Policy::apply`] as "a ceiling of 0 denies every order". Both cases get the placeholder
/// so the suggestion is always a line that works.
fn toml_value(raw: &str) -> String {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() && v > 0.0 => raw.to_string(),
        _ => "<a positive number, in quote currency>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn an_environment_with_none_of_them_starts_normally() {
        assert_eq!(refuse_removed_env(&HashMap::new()), Ok(()));
        assert_eq!(refuse_removed_env(&env(&[("VIKE_RECONCILE", "1")])), Ok(()));
    }

    #[test]
    fn a_set_variable_is_refused_naming_the_file_the_key_and_the_replacement_line() {
        let err = refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "250")])).unwrap_err();
        assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL"), "{err}");
        assert!(err.contains("policy.toml"), "{err}");
        // The operator's own value, rendered as the line they can paste.
        assert!(err.contains("max_notional_per_order = 250"), "{err}");
        assert!(err.contains("NO LONGER READ"), "{err}");
    }

    #[test]
    fn the_daemon_variable_is_refused_the_same_way() {
        let err =
            refuse_removed_env(&env(&[("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000")])).unwrap_err();
        assert!(err.contains("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL"), "{err}");
        assert!(err.contains("max_notional_per_order = 1000"), "{err}");
    }

    /// One pass, every offender — not one restart per variable.
    #[test]
    fn both_variables_are_reported_together() {
        let err = refuse_removed_env(&env(&[
            ("VIKE_MAX_ORDER_NOTIONAL", "250"),
            ("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL", "1000"),
        ]))
        .unwrap_err();
        assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL="), "{err}");
        assert!(err.contains("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL="), "{err}");
    }

    /// A blank value configured nothing before, so nobody believes a ceiling is active from one.
    #[test]
    fn an_empty_or_whitespace_value_is_not_a_belief_worth_refusing() {
        assert_eq!(refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "")])), Ok(()));
        assert_eq!(refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", "   ")])), Ok(()));
    }

    /// An unparseable or non-positive value still REFUSES (it is set, and someone believes it) —
    /// but the suggested line must be one that actually loads, not an echo of the garbage.
    #[test]
    fn a_garbage_value_still_refuses_but_suggests_a_usable_line() {
        for bad in ["nope", "0", "-5", "NaN"] {
            let err = refuse_removed_env(&env(&[("VIKE_MAX_ORDER_NOTIONAL", bad)])).unwrap_err();
            assert!(err.contains("VIKE_MAX_ORDER_NOTIONAL"), "{bad}: {err}");
            assert!(
                err.contains("max_notional_per_order = <a positive number"),
                "{bad} must not be echoed into the suggested line: {err}"
            );
        }
    }

    /// Every row that names a KEY must point at one the policy file actually has — a refusal that
    /// names a key `policy.toml` would reject on sight (`deny_unknown_fields`) sends the operator in
    /// a circle. A row with no key configures nothing that survives and has nothing to verify.
    #[test]
    fn every_row_that_names_a_key_names_one_policy_toml_accepts() {
        for r in REMOVED_ENV {
            let Some(key) = r.key else { continue };
            assert_eq!(r.file, "policy.toml", "{} names a file this check cannot verify", r.var);
            let patch: crate::PolicyPatch = toml::from_str(&format!("{key} = 123.0"))
                .unwrap_or_else(|e| {
                    panic!("{} names key `{key}`, which policy.toml rejects: {e}", r.var)
                });
            crate::Policy::default()
                .apply(patch, std::path::Path::new("policy.toml"))
                .expect("123.0 must be an accepted value for the named key");
        }
    }

    /// **A removed variable whose value is itself a secret is refused WITHOUT printing it.**
    ///
    /// The refusal goes to stderr, which is captured by every service manager and every CI log, so
    /// echoing the value would turn a startup diagnostic into a credential leak — a worse outcome
    /// than the misconfiguration being reported.
    #[test]
    fn a_secret_valued_variable_is_refused_without_echoing_it() {
        let err = refuse_removed_env(&env(&[("VIKE_SECRETS_PASSPHRASE", "hunter2-correct-horse")]))
            .unwrap_err();
        assert!(err.contains("VIKE_SECRETS_PASSPHRASE"), "{err}");
        assert!(err.contains("NO LONGER READ"), "{err}");
        assert!(
            !err.contains("hunter2-correct-horse"),
            "the refusal must never print the value: {err}"
        );
        // It names where credentials come from, and offers no TOML line to paste (nothing moved).
        assert!(err.contains("settings/secrets.env"), "{err}");
        assert!(!err.contains(" = "), "there is no key to set: {err}");
    }

    // -- the removed FILE -----------------------------------------------------------------------

    /// `<project>/settings` -> the project is its parent, and that is the only place looked at.
    #[test]
    fn a_project_file_beside_the_settings_directory_is_refused_by_full_path() {
        let project = tempfile::tempdir().unwrap();
        let settings = project.path().join("settings");
        std::fs::create_dir(&settings).unwrap();
        let file = project.path().join(REMOVED_PROJECT_FILE);
        std::fs::write(&file, "[config]\nlog_dir = \"/from/project\"\n").unwrap();

        let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
        assert!(matches!(err, ConfigError::RemovedProjectFile { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains(&file.display().to_string()), "names the exact file: {msg}");
        assert!(msg.contains("NO LONGER READ"), "{msg}");
        // Both destinations, so the operator never has to guess which table goes where.
        assert!(msg.contains("settings/config.toml"), "{msg}");
        assert!(msg.contains("settings/preferences.toml"), "{msg}");

        // ⚠ THE rule: refuse and instruct, never act on somebody else's file.
        assert!(file.is_file(), "the refusal must not delete, move or rewrite the operator's file");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "[config]\nlog_dir = \"/from/project\"\n",
            "nor edit it"
        );
    }

    /// Absent is the normal case, and it must cost nothing and say nothing.
    #[test]
    fn no_project_file_starts_normally() {
        let project = tempfile::tempdir().unwrap();
        let settings = project.path().join("settings");
        std::fs::create_dir(&settings).unwrap();
        assert!(refuse_removed_project_file(Some(&settings)).is_ok());
        // No settings directory at all ⇒ no project ⇒ nothing that could be misleading anybody.
        assert!(refuse_removed_project_file(None).is_ok());
    }

    /// A directory named `vike.toml` still refuses: `metadata` succeeds, and the operator who made
    /// one is at least as confused as the one who wrote a file. Only `NotFound` is absence.
    #[test]
    fn only_not_found_counts_as_absent() {
        let project = tempfile::tempdir().unwrap();
        let settings = project.path().join("settings");
        std::fs::create_dir(&settings).unwrap();
        std::fs::create_dir(project.path().join(REMOVED_PROJECT_FILE)).unwrap();
        let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
        // `metadata` SUCCEEDED, so this really is the "it is there, delete it" answer.
        assert!(
            matches!(err, ConfigError::RemovedProjectFile { probe: RemovedFileProbe::Present, .. }),
            "{err}"
        );
    }

    // -- the probe that could not answer -----------------------------------------------------
    //
    // ⚠ The refusal below is the SAME refusal. Nothing here relaxes it: absence must be
    // established, and it was not. What is under test is the CLAIM the message makes.

    /// **A stat that failed must not be reported as a file that exists.**
    ///
    /// This is the the CI box defect: under `ProtectHome=yes` the stat of a project root inside `$HOME`
    /// returns `EACCES`, and the old single message told the operator a `vike.toml` was present and
    /// to delete it — a file that was not there, with none of the causes that produce that errno
    /// named. Asserted on the message function directly, with a synthesized error, so the claim is
    /// pinned without needing a filesystem that can produce one (the wired end-to-end proof is
    /// `a_path_that_cannot_be_probed_refuses_without_claiming_the_file_exists` below).
    #[test]
    fn an_unprobeable_path_says_so_instead_of_asserting_the_file_is_present() {
        let file = Path::new("/home/the operator/vike-trader-rust").join(REMOVED_PROJECT_FILE);
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let errno = e.to_string();
        let msg = removed_project_file_message(&file, &RemovedFileProbe::Unestablished(e));

        // ⚠ THE assertion: it never claims the file is there, and never orders a deletion of it.
        assert!(
            !msg.contains("is present"),
            "the message asserts something it does not know: {msg}"
        );
        assert!(msg.contains("could not be STATTED"), "{msg}");
        assert!(msg.contains("NOT the same as absent"), "{msg}");
        assert!(msg.contains("may not exist at all"), "the operator must be told this: {msg}");
        // The errno itself, verbatim — the first clue, and the thing to search for.
        assert!(msg.contains(&errno), "the os error `{errno}` must be quoted: {msg}");
        // …and the causes that actually produce it, in the vocabulary `unwritable_store_root` uses.
        assert!(msg.contains("ProtectHome="), "the measured cause must be named: {msg}");
        assert!(msg.contains("ProtectSystem=strict"), "the sibling cause must be named: {msg}");
        assert!(msg.contains("VIKE_SETTINGS_DIR"), "the way out must be named: {msg}");
        // Still the full path, and still the promise that nothing was touched.
        assert!(msg.contains(&file.display().to_string()), "{msg}");
        assert!(msg.contains("Nothing has been moved or deleted"), "{msg}");
    }

    /// …and the answer that DID see something is untouched: it still says present, still names both
    /// destinations, still orders the deletion. A fix that blurred the two messages into one
    /// hedge would have replaced a false claim with no claim.
    #[test]
    fn a_file_that_was_actually_seen_still_gets_the_delete_it_message() {
        let file = Path::new("/srv/vike-<unit>").join(REMOVED_PROJECT_FILE);
        let msg = removed_project_file_message(&file, &RemovedFileProbe::Present);
        assert!(msg.contains("is present"), "{msg}");
        assert!(msg.contains("NO LONGER READ"), "{msg}");
        assert!(msg.contains(&format!("then delete {}", file.display())), "{msg}");
        assert!(!msg.contains("could not be STATTED"), "the two answers must not blur: {msg}");
    }

    /// **The wired proof, through the real `std::fs::metadata`.**
    ///
    /// A regular FILE standing where `<project>` should be: every probe below it fails with
    /// `ENOTDIR`, which is **uid-independent** — the trick `vike-secrets`' own store tests use,
    /// because `chmod 000` proves nothing when the suite runs as root, which CI does.
    ///
    /// ⚠ What it does and does not stand in for. It is a genuine non-`NotFound` stat failure
    /// reaching the real function, so it proves the arm is WIRED and that the message never claims
    /// presence. It is not `EACCES`, so it does not reproduce `ProtectHome=` itself — that is
    /// `a_genuine_eacces_is_reported_the_same_way`'s job when the process is not root, and the
    /// the CI box measurement's otherwise. Unix-only: on Windows a path through a regular file resolves
    /// to `ERROR_PATH_NOT_FOUND`, which maps to `NotFound` — i.e. Windows answers "absent" and the
    /// fixture cannot be built there at all.
    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_probed_refuses_without_claiming_the_file_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::write(&project, "a regular file, not a directory").unwrap();
        let settings = project.join("settings");

        let err = refuse_removed_project_file(Some(&settings)).unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::RemovedProjectFile { probe: RemovedFileProbe::Unestablished(_), .. }
            ),
            "a failed stat is not established absence, and not established presence either: {err}"
        );
        let msg = err.to_string();
        assert!(!msg.contains("is present"), "{msg}");
        assert!(msg.contains("could not be STATTED"), "{msg}");
        assert!(msg.contains(&project.join(REMOVED_PROJECT_FILE).display().to_string()), "{msg}");
        // The errno survives as a `source`, so a caller can match on the kind, not the prose.
        let source = std::error::Error::source(&err).expect("the io error must be carried");
        assert!(!source.to_string().is_empty(), "{source}");
    }

    /// The real thing when the environment allows it: a project directory the process genuinely
    /// cannot search, which is `EACCES` — the exact errno `ProtectHome=yes` produces.
    ///
    /// Self-skips as root (and on any filesystem ignoring the mode), where `0o000` denies nothing;
    /// the ENOTDIR fixture above is what always runs. Same shape as `vike-data`'s
    /// `open_under_an_unwritable_parent_reports_the_sandbox_diagnosis`.
    #[cfg(unix)]
    #[test]
    fn a_genuine_eacces_is_reported_the_same_way() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let settings = project.join("settings");
        std::fs::create_dir_all(&settings).unwrap();
        // No `x` bit: nothing inside can be resolved, exactly as under `ProtectHome=yes`.
        std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o000)).unwrap();

        let outcome = refuse_removed_project_file(Some(&settings));

        // Restore first, so a failing assertion cannot leave an unremovable directory behind.
        let _ = std::fs::set_permissions(&project, std::fs::Permissions::from_mode(0o755));

        match outcome {
            Err(ConfigError::RemovedProjectFile {
                probe: RemovedFileProbe::Unestablished(e),
                ..
            }) => {
                assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied, "{e}");
            }
            // Root, or a filesystem ignoring the mode: the fixture proved nothing, so say so.
            other => eprintln!(
                "skipped: this process can stat through a 0o000 directory (got {other:?})"
            ),
        }
    }

    /// A settings directory with no parent has no project, so there is nothing to probe. The
    /// loader's behaviour there is "skip the check", not "guess a directory".
    #[test]
    fn a_parentless_settings_directory_is_skipped_rather_than_guessed_at() {
        let root = if cfg!(windows) { Path::new("C:\\") } else { Path::new("/") };
        assert!(refuse_removed_project_file(Some(root)).is_ok());
    }

    /// Every row that forbids echoing must be one where echoing would actually matter, and every
    /// row that permits it must be one where the operator needs the number back. Stated as a test
    /// because `echo_value` is a one-word field that a copy-pasted row gets wrong silently.
    #[test]
    fn only_rows_with_a_key_to_paste_echo_their_value() {
        for r in REMOVED_ENV {
            assert_eq!(
                r.echo_value,
                r.key.is_some(),
                "{}: a row with no key has no line to paste, so it has no reason to echo",
                r.var
            );
        }
    }
}
