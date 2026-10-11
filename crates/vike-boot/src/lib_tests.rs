use std::path::{Path, PathBuf};

use super::*;

fn spec<'a>(env: &'a HashMap<String, String>, cwd: &'a Path) -> BootSpec<'a> {
    BootSpec {
        env,
        cwd: Some(cwd),
        identity: Identity { name: "vike-test", version: "0.0.0" },
        removed_env: RemovedEnv::Refuse,
        settings: SettingsLoad::Load,
        credentials: Credentials::Deferred("the unit tests open no store"),
        log_home: LogHome::UnderSettings,
        disclosure: Disclosure::Render,
    }
}

/// The test process's working directory, where a walk from [`spec`] starts.
fn here() -> PathBuf {
    std::env::current_dir().unwrap()
}

/// Blank `$VIKE_SETTINGS_DIR` values, each of which must count as unset.
const BLANKS: [&str; 3] = ["", "   ", "\t"];

/// The state root is `<named>/state` and the log home `<state root>/logs`: both hang off the ONE
/// resolved directory, never a second walk.
fn assert_paths_hang_off(booted: &Booted, named: &Path) {
    let state = named.join(vike_model::paths::state_path::STATE_SUBDIR);
    assert_eq!(
        booted.state_dir.as_deref(),
        Some(state.as_path()),
        "the state root is <settings dir>/state, never a second walk"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(state.join(vike_model::paths::state_path::LOGS_SUBDIR).as_path()),
        "…and the log home is <state root>/logs, under it"
    );
}

/// The identity line is the SAME string `--version` prints — the two must not be able to
/// disagree about which commit a box is running.
#[test]
fn the_identity_line_is_the_version_line() {
    let env = HashMap::new();
    let cwd = here();
    let booted = boot(&spec(&env, &cwd)).expect("a clean env boots");
    assert_eq!(booted.identity_line, vike_buildinfo::version_line("vike-test", "0.0.0"));
}

/// A BLANK override falls through to the walk rather than resolving settings to the working
/// directory — `project_settings_dir_from`'s documented rule, restated here because this crate
/// is what decides which value the resolver ever sees.
#[test]
fn a_blank_settings_dir_override_is_ignored() {
    for blank in BLANKS {
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), blank.to_string())]);
        assert_eq!(settings_dir_override(&env), None, "{blank:?}");
    }
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), "  /srv/x  ".to_string())]);
    assert_eq!(settings_dir_override(&env).as_deref(), Some("/srv/x"));
}

/// **Every project-relative path hangs off the ONE resolved directory** — the property this
/// crate exists for, asserted where it can be seen rather than left to the roots.
///
/// Driven under `$VIKE_SETTINGS_DIR` pointing somewhere the working directory is not, because
/// that is the only configuration in which a second walk gives itself away: a blind
/// `project_state_dir(&cwd)`/`project_log_dir(&cwd)` answers with the CWD's project, and on
/// the CI box the two agreed only because `WorkingDirectory=` happened to equal the override.
#[test]
fn the_state_root_and_the_log_home_hang_off_the_one_resolved_directory() {
    let named = std::env::temp_dir().join("vike-boot-elsewhere").join("settings");
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), named.display().to_string())]);
    let cwd = here();
    let booted = boot(&spec(&env, &cwd)).expect("an override boots");

    assert_eq!(booted.settings_dir.as_deref(), Some(named.as_path()));
    assert_paths_hang_off(&booted, &named);
    assert!(
        !cwd.starts_with(&named),
        "precondition: the working directory is NOT under the overridden project, so a blind \
             walk could not have produced these answers"
    );
}

/// **An override needs NO WALK to honour, so a process with no readable working directory must
/// still resolve one.** `$VIKE_SETTINGS_DIR` NAMES the directory; the walk is the thing that
/// needs somewhere to start.
///
/// `std::env::current_dir()` fails whenever the directory a process was started in has been
/// removed, unmounted or made unsearchable — an ordinary event for a long-lived deployment, and
/// the shipped `deploy/*.service` units set `$VIKE_SETTINGS_DIR`. Dropping the name there splits
/// the process: the CREDENTIALS still come from the named store, while the policy CEILINGS, the
/// state root, the log home and the disclosure fall back to no project — a daemon holding live
/// keys with `max_notional_per_order` silently UNCAPPED.
#[test]
fn an_override_is_honoured_with_no_working_directory() {
    let named = std::env::temp_dir().join("vike-boot-no-cwd").join("settings");
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), named.display().to_string())]);
    let cwd = here();
    let mut s = spec(&env, &cwd);
    s.cwd = None;
    let booted = boot(&s).expect("no working directory is a legitimate boot, not a failure");

    assert_eq!(
        booted.settings_dir.as_deref(),
        Some(named.as_path()),
        "a NAMED settings directory needs no walk to reach it"
    );
    assert_eq!(
        booted.settings_dir_override.as_deref(),
        Some(named.display().to_string().as_str()),
        "…and it is still reported as the rung that answered"
    );
    assert_paths_hang_off(&booted, &named);
    assert!(
        booted.boot_lines.iter().any(|l| l.contains(&named.display().to_string())),
        "the disclosure must NAME the directory this process resolved, never the \
             no-project line: {:?}",
        booted.boot_lines
    );
}

/// …and the other half of the same law: with no working directory AND no override there is
/// genuinely nothing to answer with, so `None` stays `None`.
///
/// This is the arm that keeps the fix above from being "always return something": the walk is
/// the only other rung, and it has no start. Every path in [`Booted`] is then `None` together,
/// which is the state the disclosure's `settings dir: NONE` line describes honestly.
#[test]
fn with_neither_a_working_directory_nor_an_override_nothing_resolves() {
    let env = HashMap::new();
    let cwd = here();
    let mut s = spec(&env, &cwd);
    s.cwd = None;
    let booted = boot(&s).expect("a clean env boots");

    assert_eq!(booted.settings_dir, None, "no name and no walk is no answer");
    assert_eq!(booted.settings_dir_override, None);
    assert_eq!(booted.state_dir, None, "and nothing may be derived from an absent project");
    assert_eq!(booted.log_home, None);
}

/// A BLANK override is not an override on this arm either — it must not resolve the settings
/// directory to `""`, which is the working directory a process without one does not have.
#[test]
fn a_blank_override_resolves_nothing_with_no_working_directory() {
    for blank in BLANKS {
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), blank.to_string())]);
        let cwd = here();
        let mut s = spec(&env, &cwd);
        s.cwd = None;
        let booted = boot(&s).expect("a blank override boots");
        assert_eq!(booted.settings_dir, None, "{blank:?}");
        assert_eq!(booted.state_dir, None, "{blank:?}");
    }
}

/// `LogHome::Elsewhere` withholds the log home and NOTHING else. A root with its own state-root
/// variable (`vike-tradehub`'s `$VIKE_STATE_ROOT`) still needs this rung underneath it — that
/// is what stopped its `state_dir` from having to walk again.
#[test]
fn declining_the_log_home_does_not_withhold_the_state_root() {
    let env = HashMap::new();
    let cwd = here();
    let mut s = spec(&env, &cwd);
    s.log_home = LogHome::Elsewhere("the test declines it");
    let booted = boot(&s).expect("a clean env boots");

    assert!(booted.log_home.is_none(), "declined");
    assert_eq!(
        booted.state_dir,
        booted.settings_dir.map(|d| d.join(vike_model::paths::state_path::STATE_SUBDIR)),
        "the state root is still resolved — a declining root must not have to walk for it"
    );
}

/// A credential map holding a venue SETTING under its old credential name — `POLY_RATE_GATE`,
/// composed so the settings registry's literal sweep does not read a variable here. Nothing reads
/// such a row, and the boot does not stop on it: a retired name simply stops being read
/// (`docs/decisions/0117-there-are-no-migrations.md`).
#[test]
fn a_venue_setting_under_its_old_credential_name_does_not_stop_the_boot() {
    let env = HashMap::new();
    let cwd = here();
    let load = || HashMap::from([(concat!("POLY", "_RATE_GATE").to_string(), "1".to_string())]);
    let booted = boot(&BootSpec { credentials: Credentials::LoadWith(&load), ..spec(&env, &cwd) })
        .expect("a row nothing reads does not refuse the boot");
    assert!(
        booted.settings.warnings.iter().all(|w| !w.contains("rate_gate")),
        "{:?}",
        booted.settings.warnings
    );
}
