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
    let state = named.join(vike_paths::state_path::STATE_SUBDIR);
    assert_eq!(
        booted.state_dir.as_deref(),
        Some(state.as_path()),
        "the state root is <settings dir>/state, never a second walk"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(state.join(vike_paths::state_path::LOGS_SUBDIR).as_path()),
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
        booted.settings_dir.map(|d| d.join(vike_paths::state_path::STATE_SUBDIR)),
        "the state root is still resolved — a declining root must not have to walk for it"
    );
}

/// A store in a temporary project holding `rows`, written the one way a credential reaches a
/// store (`create_store`, then the journalled writer), and the environment that names it.
fn project_holding(rows: &[(&str, &str)]) -> (tempfile::TempDir, HashMap<String, String>) {
    use vike_change_journal::{Actor, Proc};
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    vike_secrets::create_store(settings.to_str()).expect("create the empty store");
    let rows: Vec<(String, String)> =
        rows.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let (_, journal_error) = vike_secrets::save_credentials_to_store(
        &settings,
        vike_secrets::Table::Credential,
        &rows,
        vike_secrets::UpsertIntent::Add,
        vike_secrets::CredentialJournal {
            actor: Actor::cli("vike-boot-test"),
            venue: "multi",
            tier: "",
            proc: Proc::new("vike-boot-test", 1, "0.0.0"),
            now_ms: 0,
        },
    )
    .expect("seed the store");
    assert!(journal_error.is_none(), "{journal_error:?}");
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())]);
    (dir, env)
}

/// A store in a temporary project holding one ACCOUNT per `(venue, tier)` with its `API_KEY` field
/// set — the account created first, with its tier, and the credential written against it by FIELD,
/// which is the one way an account's credential reaches a store (decision 0156 item 6). Returns the
/// environment that names it and the NAME each credential row was stored under, in the order given.
fn project_with_accounts(
    accounts: &[(&str, vike_model::AccountTier)],
) -> (tempfile::TempDir, HashMap<String, String>, Vec<String>) {
    use vike_change_journal::{Actor, Proc};
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    vike_secrets::create_store(settings.to_str()).expect("create the empty store");
    let journal = || vike_secrets::AccountJournal {
        actor: Actor::cli("vike-boot-test"),
        proc: Proc::new("vike-boot-test", 1, "0.0.0"),
        now_ms: 0,
    };
    let mut names = Vec::new();
    for &(venue, tier) in accounts {
        let (created, _) = vike_secrets::edit_account_in(
            &settings,
            vike_secrets::AccountEdit::Create { venue, tier, label: None },
            journal(),
        )
        .expect("create the account");
        let id = created.after.expect("a create returns its row").id;
        let (written, journal_error) = vike_secrets::save_owned_credentials(
            &settings,
            &vike_secrets::CredentialOwner::Account(id),
            &[("API_KEY".to_string(), format!("{venue}-{tier}"))],
            vike_secrets::UpsertIntent::Add,
            journal(),
        )
        .expect("write the account's key");
        assert!(journal_error.is_none(), "{journal_error:?}");
        names.extend(written.names);
    }
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())]);
    (dir, env, names)
}

/// A credential map holding a venue SETTING under its old credential name — `POLY_RATE_GATE`,
/// composed so the settings registry's literal sweep does not read a variable here. Nothing reads
/// such a row, and the boot does not stop on it: a retired name simply stops being read
/// (`docs/decisions/0117-there-are-no-migrations.md`).
#[test]
fn a_venue_setting_under_its_old_credential_name_does_not_stop_the_boot() {
    let old_name = concat!("POLY", "_RATE_GATE");
    let (_dir, env) = project_holding(&[(old_name, "1")]);
    let cwd = here();
    let booted = boot(&BootSpec { credentials: Credentials::Read, ..spec(&env, &cwd) })
        .expect("a row nothing reads does not refuse the boot");
    assert!(
        booted.settings.warnings.iter().all(|w| !w.contains("rate_gate")),
        "{:?}",
        booted.settings.warnings
    );
    let read = booted.credentials.expect("Credentials::Read reads the map");
    assert!(
        read.secrets.keys().any(|k| k == old_name),
        "the row is in the map: {:?}",
        read.secrets
    );
}

/// **The store the boot opens is the store of the ONE resolved directory** — the property that
/// replaced the root's own loader re-resolving it: `Booted::store`'s location IS
/// `Booted::settings_dir`, and `Credentials::Read` reads that store.
#[test]
fn the_store_is_opened_over_the_one_resolved_directory() {
    let (_dir, env, names) = project_with_accounts(&[("binance", vike_model::AccountTier::Demo)]);
    let key = names[0].as_str();
    let cwd = here();
    let booted = boot(&BootSpec { credentials: Credentials::Read, ..spec(&env, &cwd) })
        .expect("a store boots");
    assert_eq!(
        booted.store.location().map(|l| l.settings_dir().to_path_buf()),
        booted.settings_dir,
        "the store and the settings cannot name two projects"
    );
    assert_eq!(booted.store.scope(), vike_secrets::CredentialScope::Full, "unset is Full");
    let read = booted.credentials.expect("read");
    assert!(read.secrets.keys().any(|k| k == key));
    assert!(read.health.is_readable());

    // Deferred: the handle is there, nothing was read.
    let deferred = boot(&spec(&env, &cwd)).expect("boots");
    assert!(deferred.credentials.is_none());
    assert_eq!(deferred.store, booted.store);
}

/// With no settings directory resolved, the store is a handle with NO location: every read is
/// empty (the live gate), never an error.
#[test]
fn with_no_project_the_store_has_no_location_and_reads_empty() {
    let env = HashMap::new();
    let cwd = here();
    let mut s = spec(&env, &cwd);
    s.cwd = None;
    s.credentials = Credentials::Read;
    let booted = boot(&s).expect("a clean env boots");
    assert!(booted.store.location().is_none());
    let read = booted.credentials.expect("read");
    assert!(read.secrets.is_empty());
    assert_eq!(read.source, vike_secrets::Source::None);
}

/// **`VIKE_NO_LIVE_KEYS` is STRICT, and a word outside `true`/`false` REFUSES the boot** — the
/// error names the variable. `true` (any case) is the no-live-keys scope, and it binds the read the
/// boot performs: the credential of an account whose ROW is at tier `live` is withheld and counted
/// (the tier the operator wrote on the account, never one read out of a name).
#[test]
fn the_no_live_keys_switch_is_strict_and_binds_the_boot_read() {
    let (_dir, env, names) = project_with_accounts(&[
        ("binance", vike_model::AccountTier::Live),
        ("binance", vike_model::AccountTier::Demo),
    ]);
    let demo = names[1].as_str();
    let cwd = here();

    for bad in ["1", "yes", "ture", "", "  ", " true", "demo"] {
        let mut env = env.clone();
        env.insert("VIKE_NO_LIVE_KEYS".to_string(), bad.to_string());
        let err = match boot(&spec(&env, &cwd)) {
            Ok(_) => panic!("{bad:?} must refuse the boot"),
            Err(e) => e,
        };
        assert!(err.contains("VIKE_NO_LIVE_KEYS"), "the refusal names the variable: {err}");
        assert!(err.contains("`true` or `false`"), "…and says what is accepted: {err}");
    }

    for (word, scope) in [
        ("true", vike_secrets::CredentialScope::NoLiveKeys),
        ("TRUE", vike_secrets::CredentialScope::NoLiveKeys),
        ("false", vike_secrets::CredentialScope::Full),
    ] {
        let mut env = env.clone();
        env.insert("VIKE_NO_LIVE_KEYS".to_string(), word.to_string());
        let booted = boot(&BootSpec { credentials: Credentials::Read, ..spec(&env, &cwd) })
            .unwrap_or_else(|e| panic!("{word:?} boots: {e}"));
        assert_eq!(booted.store.scope(), scope, "{word:?}");
        let read = booted.credentials.expect("read");
        let names: Vec<&str> = read.secrets.keys().collect();
        match scope {
            vike_secrets::CredentialScope::NoLiveKeys => {
                assert_eq!(names, vec![demo], "the LIVE name is withheld: {names:?}");
                assert_eq!(read.withheld, 1);
            }
            vike_secrets::CredentialScope::Full => {
                assert_eq!(names.len(), 2, "{names:?}");
                assert_eq!(read.withheld, 0);
            }
            vike_secrets::CredentialScope::NoVenueKeys => {
                panic!("no environment word selects NoVenueKeys ({word:?})")
            }
        }
    }
}

/// **The `NoVenueKeys` arms are the ONLY way to that scope, and the environment cannot lift them.**
/// A root declaring `Credentials::ReadNoVenueKeys` or `::DeferredNoVenueKeys` gets a handle under
/// `CredentialScope::NoVenueKeys` whatever `VIKE_NO_LIVE_KEYS` says (`true`, `false` or unset), the
/// boot read holds no account's credential, and a malformed word still REFUSES the boot (the strict
/// parse is not skipped for such a root). The `Read` and `Deferred` arms keep their scopes.
#[test]
fn the_no_venue_keys_arms_open_the_stricter_scope_whatever_the_environment() {
    let (_dir, env, _names) = project_with_accounts(&[
        ("binance", vike_model::AccountTier::Demo),
        ("binance", vike_model::AccountTier::Live),
    ]);
    let cwd = here();
    for word in [None, Some("true"), Some("false")] {
        let mut env = env.clone();
        if let Some(word) = word {
            env.insert("VIKE_NO_LIVE_KEYS".to_string(), word.to_string());
        }
        let read =
            boot(&BootSpec { credentials: Credentials::ReadNoVenueKeys, ..spec(&env, &cwd) })
                .unwrap_or_else(|e| panic!("{word:?} boots: {e}"));
        assert_eq!(read.store.scope(), vike_secrets::CredentialScope::NoVenueKeys, "{word:?}");
        let map = read.credentials.expect("ReadNoVenueKeys reads the map");
        assert!(map.secrets.is_empty(), "an account's credential reached the boot read: {word:?}");
        assert_eq!(map.scope, vike_secrets::CredentialScope::NoVenueKeys);
        assert_eq!(map.withheld, 2, "both accounts' rows are withheld and counted: {word:?}");

        let deferred = boot(&BootSpec {
            credentials: Credentials::DeferredNoVenueKeys("the unit tests read through the handle"),
            ..spec(&env, &cwd)
        })
        .unwrap_or_else(|e| panic!("{word:?} boots: {e}"));
        assert_eq!(deferred.store.scope(), vike_secrets::CredentialScope::NoVenueKeys, "{word:?}");
        assert!(deferred.credentials.is_none(), "Deferred reads nothing at boot");
    }
    // The strict parse still runs: a malformed word refuses even a NoVenueKeys root.
    let mut bad = env.clone();
    bad.insert("VIKE_NO_LIVE_KEYS".to_string(), "yes".to_string());
    let refused = boot(&BootSpec {
        credentials: Credentials::DeferredNoVenueKeys("the unit tests read through the handle"),
        ..spec(&bad, &cwd)
    });
    assert!(refused.is_err_and(|e| e.contains("VIKE_NO_LIVE_KEYS")), "a bad word must refuse");
    // The other arms are unchanged.
    let full =
        boot(&BootSpec { credentials: Credentials::Read, ..spec(&env, &cwd) }).expect("boots");
    assert_eq!(full.store.scope(), vike_secrets::CredentialScope::Full);
}

/// The literal this crate reads is the store's own name for the switch: the registry sees the read
/// through the literal, and the store documents the switch under its constant, so the two must be
/// one spelling.
#[test]
fn the_scope_variable_is_spelled_as_the_store_spells_it() {
    let env = HashMap::from([(vike_secrets::NO_LIVE_KEYS_ENV.to_string(), "true".to_string())]);
    assert_eq!(no_live_keys(&env), Some("true"));
}
