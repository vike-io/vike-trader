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
        Some(named.join(vike_model::paths::state_path::STATE_SUBDIR).as_path()),
        "the state root is <settings dir>/state, never a second walk"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(
            named
                .join(vike_model::paths::state_path::STATE_SUBDIR)
                .join(vike_model::paths::state_path::LOGS_SUBDIR)
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
/// A daemon holding live venue keys out of a store whose sibling `policy` rows it never read is
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
        Some(named.join(vike_model::paths::state_path::STATE_SUBDIR).as_path()),
        "the state root hangs off it, exactly as it does with a working directory"
    );
    assert_eq!(
        booted.log_home.as_deref(),
        Some(
            named
                .join(vike_model::paths::state_path::STATE_SUBDIR)
                .join(vike_model::paths::state_path::LOGS_SUBDIR)
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
        booted.settings_dir.map(|d| d.join(vike_model::paths::state_path::STATE_SUBDIR)),
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
    assert!(err.starts_with("REFUSING TO START"), "a root that refuses says so first: {err}");
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
    // Final review M9: the mark is printed by a root that STARTS — `vike-cli` and the desktop carry
    // it as a warning — so it may not announce a refusal that did not happen.
    assert!(
        !mark.contains("REFUSING TO START"),
        "a root that starts may not say it refuses: {mark}"
    );
}

/// A pending-0095 store on the shape the release before the venue flip wrote: a binance `live`
/// ceiling, so the ceiling step WRITES, and an `account` row naming no roster venue, so the store's
/// repair — which runs in that same write — refuses it by name. Planted through
/// `vike_secrets::venue_links::plant_pre_venue_link_store` because a CARRIED `account` cannot hold
/// that row at all (`venue_id` is `NOT NULL` there), and `pre_0095_store` plants through the write
/// funnel, which carries the store.
fn an_off_roster_store_pending_0095() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    vike_secrets::venue_links::plant_pre_venue_link_store(
        d.path(),
        "INSERT INTO venue_arming (venue, venue_id, label, mode) \
             VALUES ('binance', (SELECT id FROM venue WHERE name = 'binance'), NULL, 'live'); \
         INSERT INTO account (id, venue, venue_id, tier) \
             VALUES (7, 'no-such-venue', NULL, 'demo');",
    );
    vike_secrets::live_means_mainnet::unmark_live_means_mainnet(d.path());
    assert!(
        vike_secrets::live_means_mainnet::live_means_mainnet_pending(d.path()).unwrap(),
        "premise: decision 0095 is pending, so the ceiling step writes"
    );
    d
}

/// The row trap 7 names on [`an_off_roster_store_pending_0095`], as its refusal renders it.
const OFF_ROSTER_ROW: &str = "`account` id 7 (venue 'no-such-venue')";

/// **Final review M4: the pass-through is pinned at `boot()`, not only at the function.** A root
/// booted with `Ceilings::Interpret` over a store whose REPAIR refuses must refuse to start with the
/// repair's own rows and words — not with decision 0095's framing, which names `vike-cli config
/// migrate-store`, a verb that runs the same repair and is refused the same way.
#[test]
fn an_interpreting_root_refuses_a_repair_refusal_in_its_own_words() {
    let d = an_off_roster_store_pending_0095();
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    let err = boot(&BootSpec { ceilings: Ceilings::Interpret { now_ms: 1 }, ..spec(&env, &cwd) })
        .err()
        .expect("the store's repair refuses, so the interpreting root refuses to start");
    assert!(err.contains(OFF_ROSTER_ROW), "the refusal's own row rides through: {err}");
    assert!(!err.contains("predates decision 0095"), "…and it is not blamed on 0095: {err}");
    assert!(err.starts_with("REFUSING TO START"), "…and the root says it refuses: {err}");
}

/// A refusal from the store's REPAIR — not from decision 0095's rewrite — is passed through without
/// the 0095 framing. `vike-cli config migrate-store` runs the same repair and is refused the same
/// way, so offering it as the repair would send an operator in a circle. The vike-secrets half,
/// that the store's refusal arrives as `RepairRefused`, is
/// `crates/vike-secrets/tests/venue_links_by_number.rs`'s
/// `a_venue_links_refusal_at_boot_is_told_apart_from_decision_0095`.
///
/// The text this function returns is the refusal's AND the mark's, so it does not announce a
/// refusal itself: [`boot`] says *REFUSING TO START* where it refuses, and only there.
#[test]
fn a_repair_refusal_at_the_ceiling_step_is_not_blamed_on_decision_0095() {
    let d = an_off_roster_store_pending_0095();
    let repair = vike_secrets::live_means_mainnet::apply_live_means_mainnet(
        d.path(),
        Actor::Boot,
        Proc::current("0.0.0"),
        1,
    )
    .expect_err("the store's repair refuses the off-roster row");
    assert!(
        matches!(repair.kind, vike_secrets::DbErrorKind::RepairRefused { .. }),
        "premise: the store's repair refused: {repair}"
    );
    let why = ceiling_migration_refusal(d.path(), &repair);
    assert!(why.contains(OFF_ROSTER_ROW), "the refusal's own rows ride through: {why}");
    assert!(!why.contains("REFUSING TO START"), "the refusal is boot's to announce: {why}");
    assert!(!why.contains("predates decision 0095"), "…and it is not blamed on 0095: {why}");
    assert!(
        why.contains("`vike-cli config migrate-store` is not the repair"),
        "…nor is the verb that fails the same way offered as its repair: {why}"
    );

    // …while a failure of 0095's own step keeps its framing and its repair.
    let busy = vike_secrets::DbError {
        path: vike_secrets::db_path_in(d.path()),
        kind: vike_secrets::DbErrorKind::StoreBusy,
    };
    let why = ceiling_migration_refusal(d.path(), &busy);
    assert!(why.contains("predates decision 0095"), "{why}");
    assert!(why.contains("vike-cli config migrate-store"), "{why}");
    assert!(!why.contains("REFUSING TO START"), "the refusal is boot's to announce: {why}");
}

/// **The verb decision 0095's refusal names also CONTRACTS the store, and the text says so.** That
/// run executes the store's whole write path in 0095's transaction, which since the venue-links
/// plan's second release drops the text `venue` column — a step v0.1.40 cannot read back. So the
/// text may not promise that the run "changes nothing else" (it did until the plan's final fix
/// wave), and it names the rollback boundary an operator crosses by running it.
#[test]
fn the_0095_refusal_says_its_repair_also_contracts_the_store() {
    let busy = vike_secrets::DbError {
        path: vike_secrets::db_path_in(Path::new("settings")),
        kind: vike_secrets::DbErrorKind::StoreBusy,
    };
    let why = ceiling_migration_refusal(Path::new("settings"), &busy);
    assert!(why.contains("vike-cli config migrate-store"), "premise: the 0095 framing: {why}");
    assert!(!why.contains("changes nothing else"), "the run changes the store's shape: {why}");
    assert!(why.contains("text `venue` column"), "…and the text names what goes: {why}");
    assert!(why.contains("v0.1.41 or newer"), "…and the rollback boundary: {why}");
}

/// A credential map holding a venue SETTING under its old credential name — `POLY_RATE_GATE`,
/// composed so the settings registry's literal sweep does not read a variable here.
fn a_stranded_setting() -> HashMap<String, String> {
    HashMap::from([(concat!("POLY", "_RATE_GATE").to_string(), "1".to_string())])
}

/// Decision 0095, Task 7: the credential-map fold is gone, so a root that loads credentials
/// REFUSES a stranded venue setting rather than ignore it, naming the setting and the move verb.
#[test]
fn a_stranded_venue_setting_refuses_the_boot() {
    let env = HashMap::new();
    let cwd = std::env::current_dir().unwrap();
    let load = a_stranded_setting;
    let err = boot(&BootSpec { credentials: Credentials::LoadWith(&load), ..spec(&env, &cwd) })
        .err()
        .expect("a stranded venue setting refuses the boot");
    assert!(err.contains("venue.polymarket.rate_gate"), "{err}");
    assert!(err.contains("move-venue-config"), "{err}");
}

/// …while the declared REPORTING arm starts and carries the same finding as a warning — and still
/// refuses a credential row that ARMS real money, which it does not get to downgrade.
#[test]
fn a_reporting_root_starts_and_warns_on_a_stranded_venue_setting() {
    let env = HashMap::new();
    let cwd = std::env::current_dir().unwrap();
    let load = a_stranded_setting;
    let booted = boot(&BootSpec {
        credentials: Credentials::LoadReportingStrandedSettings(&load, "the test mounts nothing"),
        ..spec(&env, &cwd)
    })
    .expect("a reporting root starts");
    assert!(
        booted
            .settings
            .warnings
            .iter()
            .any(|w| w.contains("venue.polymarket.rate_gate") && w.contains("move-venue-config")),
        "{:?}",
        booted.settings.warnings
    );
    // …and the bare finding, for a root with no subscriber to carry the warning: names and
    // settings, no value (the planted value is `1`, which the report could not contain by name).
    let report = booted.stranded_venue_settings.as_deref().expect("the reporting arm hands it on");
    assert!(report.contains("venue.polymarket.rate_gate"), "{report}");
    assert!(report.contains("move-venue-config"), "{report}");

    // Nothing stranded, and every other arm, hand on nothing.
    let clean =
        || HashMap::from([(concat!("BINANCE", "_LIVE_API_KEY").to_string(), "k".to_string())]);
    let booted = boot(&BootSpec {
        credentials: Credentials::LoadReportingStrandedSettings(&clean, "the test mounts nothing"),
        ..spec(&env, &cwd)
    })
    .expect("a clean store starts");
    assert_eq!(booted.stranded_venue_settings, None);
    let booted =
        boot(&BootSpec { credentials: Credentials::Deferred("no store"), ..spec(&env, &cwd) })
            .expect("a deferred root starts");
    assert_eq!(booted.stranded_venue_settings, None);

    let arming = || HashMap::from([(concat!("POLY", "_EXEC").to_string(), "1".to_string())]);
    let err = boot(&BootSpec {
        credentials: Credentials::LoadReportingStrandedSettings(&arming, "the test mounts nothing"),
        ..spec(&env, &cwd)
    })
    .err()
    .expect("an arming credential row still refuses under the reporting arm");
    assert!(err.contains("REAL MONEY") || err.contains("0095"), "{err}");
}

#[test]
fn a_root_that_reads_no_ceiling_leaves_the_store_alone() {
    let d = pre_0095_store(&["bybit"]);
    let env = env_naming(d.path());
    let cwd = std::env::current_dir().unwrap();
    boot(&spec(&env, &cwd)).expect("boots");
    assert!(vike_secrets::live_means_mainnet::live_means_mainnet_pending(d.path()).unwrap());
}
