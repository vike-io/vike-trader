use super::*;

fn spec<'a>(env: &'a HashMap<String, String>, cwd: &'a Path) -> BootSpec<'a> {
    BootSpec {
        env,
        cwd: Some(cwd),
        identity: Identity { name: "vike-test", version: "0.0.0" },
        removed_env: RemovedEnv::Refuse,
        settings: SettingsLoad::Load,
        ceilings: Ceilings::NotInterpreted("the unit tests read no ceiling"),

        credentials: Credentials::Deferred("the unit tests open no store"),
        log_home: LogHome::UnderSettings,
        disclosure: Disclosure::Render,
    }
}

/// The identity line is the SAME string `--version` prints — the two must not be able to
/// disagree about which commit a box is running.
#[test]
fn the_identity_line_is_the_version_line() {
    let env = HashMap::new();
    let cwd = std::env::current_dir().unwrap();
    let booted = boot(&spec(&env, &cwd)).expect("a clean env boots");
    assert_eq!(booted.identity_line, vike_buildinfo::version_line("vike-test", "0.0.0"));
}

/// A BLANK override falls through to the walk rather than resolving settings to the working
/// directory — `project_settings_dir_from`'s documented rule, restated here because this crate
/// is what decides which value the resolver ever sees.
#[test]
fn a_blank_settings_dir_override_is_ignored() {
    for blank in ["", "   ", "\t"] {
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
    let cwd = std::env::current_dir().unwrap();
    let booted = boot(&spec(&env, &cwd)).expect("an override boots");

    assert_eq!(booted.settings_dir.as_deref(), Some(named.as_path()));
    assert_eq!(
        booted.state_dir.as_deref(),
        Some(named.join(vike_model::state_path::STATE_SUBDIR).as_path()),
        "the state root is <settings dir>/state, never a second walk"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(
            named
                .join(vike_model::state_path::STATE_SUBDIR)
                .join(vike_model::state_path::LOGS_SUBDIR)
                .as_path()
        ),
        "…and the log home is <state root>/logs, under it"
    );
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
/// removed, unmounted or made unsearchable — an ordinary event for a long-lived deployment and
/// for a verification lane whose tree is replaced under it, and all three shipped `deploy/*.
/// service` units set `$VIKE_SETTINGS_DIR`. This boot used to resolve the directory as
/// `spec.cwd.and_then(..)`, so on such a box it answered `settings_dir: None` while still
/// returning `settings_dir_override: Some(..)` — and the two halves of the process then read
/// DIFFERENT projects:
///
/// * the CREDENTIALS still came from the named directory, because
///   `vike_secrets::resolve_project` -> `workspace_dotenv_path_from` honours the override with
///   no walk at all (`dotenv_path_for`'s no-CWD arm), while
/// * the POLICY CEILINGS, the state root, the log home and the disclosure all fell back to the
///   no-project answers — compiled-in defaults, no `alerts.json` home, and a banner reading
///   "settings dir: NONE" on a box whose settings directory was named outright.
///
/// A daemon holding live venue keys out of a file whose sibling `policy.toml` it never opened is
/// precisely the split this crate exists to make impossible, and `max_notional_per_order`
/// silently reverting to UNCAPPED is the sharp end of it.
#[test]
fn an_override_is_honoured_with_no_working_directory() {
    let named = std::env::temp_dir().join("vike-boot-no-cwd").join("settings");
    let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), named.display().to_string())]);
    let cwd = std::env::current_dir().unwrap();
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
    assert_eq!(
        booted.state_dir.as_deref(),
        Some(named.join(vike_model::state_path::STATE_SUBDIR).as_path()),
        "the state root hangs off it, exactly as it does with a working directory"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(
            named
                .join(vike_model::state_path::STATE_SUBDIR)
                .join(vike_model::state_path::LOGS_SUBDIR)
                .as_path()
        ),
        "…and so does the log home"
    );
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
    let cwd = std::env::current_dir().unwrap();
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
    for blank in ["", "   ", "\t"] {
        let env = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), blank.to_string())]);
        let cwd = std::env::current_dir().unwrap();
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
    let cwd = std::env::current_dir().unwrap();
    let mut s = spec(&env, &cwd);
    s.log_home = LogHome::Elsewhere("the test declines it");
    let booted = boot(&s).expect("a clean env boots");

    assert!(booted.log_home.is_none(), "declined");
    assert_eq!(
        booted.state_dir,
        booted.settings_dir.map(|d| d.join(vike_model::state_path::STATE_SUBDIR)),
        "the state root is still resolved — a declining root must not have to walk for it"
    );
}

fn pre_0095_store(live: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let arming = live
        .iter()
        .map(|v| vike_secrets::ArmingRow {
            venue: (*v).to_string(),
            label: None,
            mode: "live".to_string(),
            max_exposure: None,
        })
        .collect();
    vike_secrets::plant_settings_rows(
        d.path(),
        &vike_secrets::StoredSettings { arming, ..Default::default() },
    )
    .unwrap();
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(d.path());
    d
}

fn env_naming(dir: &Path) -> HashMap<String, String> {
    HashMap::from([("VIKE_SETTINGS_DIR".to_string(), dir.display().to_string())])
}

/// Review Focus 1: the daemon never reads a pre-0095 `live` row as mainnet.
#[test]
fn an_interpreting_root_migrates_before_it_reads_a_ceiling() {
    let d = pre_0095_store(&["binance", "aster"]);
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    let booted =
        boot(&BootSpec { ceilings: Ceilings::Interpret { now_ms: 1 }, ..spec(&env, &cwd) })
            .expect("a writable pre-0095 store boots, migrated");
    assert_eq!(booted.settings.policy.venues.get("binance"), vike_config::VenueMode::Demo);
    assert_eq!(booted.settings.policy.venues.get("aster"), vike_config::VenueMode::Live);
    assert!(
        booted.settings.warnings.iter().any(|w| w.contains("policy.venues.binance")),
        "the rewrite is disclosed: {:?}",
        booted.settings.warnings
    );
    assert!(!vike_secrets::live_means_mainnet::live_means_mainnet_pending(d.path()).unwrap());
}

/// Review Focus 1, the other half: no write, no boot — and the refusal names the command.
#[test]
fn a_root_that_cannot_write_refuses_an_unmigrated_store() {
    let d = pre_0095_store(&["hyperliquid"]);
    let _held = vike_secrets::hold_write_lock(d.path());
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    let err = boot(&BootSpec { ceilings: Ceilings::Interpret { now_ms: 1 }, ..spec(&env, &cwd) })
        .err()
        .expect("an interpreting root refuses a store it cannot migrate");
    assert!(err.contains("vike-cli config migrate-store"), "{err}");
    assert!(err.contains("0095"), "{err}");
}

#[test]
fn a_marking_root_starts_and_carries_the_refusal_as_a_store_mark() {
    let d = pre_0095_store(&["okx"]);
    let _held = vike_secrets::hold_write_lock(d.path());
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    let booted =
        boot(&BootSpec { ceilings: Ceilings::InterpretOrMark { now_ms: 1 }, ..spec(&env, &cwd) })
            .expect("a marking root keeps its repair verbs alive");
    let mark = booted.settings.store_refusal.expect("the mark");
    assert!(mark.contains("vike-cli config migrate-store"), "{mark}");
}

#[test]
fn a_root_that_reads_no_ceiling_leaves_the_store_alone() {
    let d = pre_0095_store(&["bybit"]);
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    boot(&spec(&env, &cwd)).expect("boots");
    assert!(vike_secrets::live_means_mainnet::live_means_mainnet_pending(d.path()).unwrap());
}
