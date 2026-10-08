use super::*;

fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A temp PROJECT root and its `settings/` child, both created.
///
/// ⚠ Two levels, deliberately, not one bare temp directory.
/// `vike_secrets::legacy_store_warning` probes the store's GRANDPARENT for a `<project>/.env`,
/// so a bare temp directory aims that probe at the SYSTEM temp root — where a file somebody
/// else left behind would flake this whole suite for reasons nobody could reproduce. Every path
/// these tests depend on is one they created.
fn project(tag: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "vike-cli-check-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let settings = root.join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    (root, settings)
}

/// Make a credential store holding `body`'s `KEY=value` lines — the settings DATABASE, the only
/// store, built the one way a store comes into being: the lines are CARRIED in by
/// `vike_secrets::migrate` (the library half of `vike-cli secrets migrate`) and the scratch file is
/// then retired, so no SHADOWED-file finding rides along with what a test asserts. The database is
/// created 0600 in a 0700 directory, so no permission finding does either. (This wrote
/// `secrets.env` itself until the credential FILE store was removed on 2026-10-07.)
fn write_store(settings: &Path, body: &str) {
    let path = settings.join(vike_secrets::SECRETS_FILE);
    std::fs::write(&path, body).unwrap();
    vike_secrets::migrate(
        settings.to_str(),
        vike_model::credential_keys::is_platform_key,
        &vike_bridge_core::credentials::classify_credential_name,
        vike_secrets::WhenNothingToCarry::CreateNothing,
    )
    .unwrap();
    std::fs::remove_file(&path).unwrap();
}

/// A credential store that EXISTS and cannot be read: bytes that are not a database where the
/// database belongs. Portable (a `chmod 000` proves nothing as root, which CI is).
fn plant_unreadable_store(settings: &Path) {
    let db = vike_secrets::db_path_in(settings);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, b"this is not a sqlite database, and it is not empty either").unwrap();
}

/// Plant settings ROWS into a real database, then chmod it OWNER-ONLY — `plant_settings_rows`
/// creates the file at whatever the OS umask gives it (MEASURED group-readable on the the CI box
/// lane), which is a real exposed-permission finding no fixture in this module wants to
/// exercise by accident. The row-planting twin of `write_store`'s own chmod, for the same reason.
fn plant_rows(settings: &Path, stored: &vike_secrets::StoredSettings) {
    vike_secrets::plant_settings_rows(settings, stored).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let db = vike_secrets::db_path_in(settings);
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// Write ONE credential into the settings DATABASE — the row-store twin of `write_store`, for a
/// case that has already planted rows via `plant_settings_rows` (which is why `write_store`'s
/// FILE goes dead there: once a database exists at this directory, `vike_secrets::Backend`
/// answers `Database` for credentials too, and the file is never read again). Routed through the
/// production writer, `vike_secrets::save_credentials_to_store`, with the same classifier
/// `vike-cli secrets set` passes — so this test drives the real write path rather than a
/// hand-rolled one. Chmods the database 0600 afterwards for the same reason `write_store` chmods
/// the file: a mode `vike_secrets::permission_warning` would otherwise correctly report as
/// exposed, which is not the property either fixture exists to test.
fn write_credential_row(settings: &Path, key: &str, value: &str) {
    vike_secrets::save_credentials_to_store(
        settings,
        vike_secrets::Table::Credential,
        &[(key.to_string(), value.to_string())],
        Some(&vike_bridge_core::credentials::classify_credential_name),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let db = vike_secrets::db_path_in(settings);
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn find_of<'a>(report: &'a Report, subject: &str) -> &'a Finding {
    report
        .findings
        .iter()
        .find(|f| f.subject == subject)
        .unwrap_or_else(|| panic!("no `{subject}` finding in {report:#?}"))
}

fn level_of(report: &Report, subject: &str) -> Level {
    find_of(report, subject).level
}

// -- the origin rule -------------------------------------------------------------------------

#[test]
fn the_origin_words_are_pinned() {
    assert_eq!(DirOrigin::Named.as_str(), "named");
    assert_eq!(DirOrigin::Walk.as_str(), "walk");
    assert_eq!(DirOrigin::Unresolved.as_str(), "none");
}

#[test]
fn an_unresolved_directory_reports_unresolved_whatever_the_override_says() {
    assert_eq!(dir_origin(None, None), DirOrigin::Unresolved);
    assert_eq!(dir_origin(Some("/x/settings"), None), DirOrigin::Unresolved);
}

/// **The blank-override rule is `vike_secrets::project_settings_dir_from`'s, and this holds the
/// two to it.** A whitespace-only `VIKE_SETTINGS_DIR` falls THROUGH to the walk there — so
/// calling it [`DirOrigin::Named`] here would report a rung that did not answer AND take the
/// FAIL disposition on a directory nobody named. Driven through the REAL resolver, so the two
/// cannot drift apart with only this file's opinion to notice.
#[test]
fn a_blank_override_is_the_walk_in_both_the_resolver_and_the_report() {
    let cwd = std::env::current_dir().unwrap();
    let walked = vike_secrets::project_settings_dir_from(None, &cwd);
    for blank in ["", "  ", "\t"] {
        assert_eq!(
            vike_secrets::project_settings_dir_from(Some(blank), &cwd),
            walked,
            "the resolver ignores a blank override"
        );
        assert_eq!(
            dir_origin(Some(blank), walked.as_deref()),
            if walked.is_some() { DirOrigin::Walk } else { DirOrigin::Unresolved },
            "…and so must the report"
        );
    }
    assert_eq!(dir_origin(Some(" /x/settings "), Some(Path::new("/x/settings"))), DirOrigin::Named);
}

// -- the disposition table -------------------------------------------------------------------

#[test]
fn a_clean_tree_with_a_real_store_is_all_ok() {
    let (root, settings) = project("clean");
    plant_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".to_string(),
                key: "max_notional_per_order".to_string(),
                value: "250".to_string(),
            }],
            ..Default::default()
        },
    );
    write_credential_row(&settings, "BINANCE_LIVE_API_KEY", "k");

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    assert_eq!(r.worst(), Level::Ok, "{r:#?}");
    assert!(!r.failed(false) && !r.failed(true));
    assert!(
        !r.findings.iter().any(|f| f.subject == "settings rows" || f.subject == "settings seal"),
        "a freshly-planted, unbroken store names no rows/seal finding at all: {r:#?}"
    );
    assert_eq!(level_of(&r, "credential store"), Level::Ok);
    let _ = std::fs::remove_dir_all(&root);
}

/// **The live gate.** An absent store is the designed state of an unconfigured box, so it is
/// `Ok` and not even a warning — `crates/vike-secrets/src/store/backend.rs`'s `resolve_absent` documents that
/// arm as an ANSWER and is deliberately silent about it. This is also the row that decides
/// whether a shipped unit can run this verb at all: every shipped daemon is a PAPER deployment
/// with an empty store, and a failure here would make a correct fresh install unstartable.
#[test]
fn an_absent_credential_store_is_not_even_a_warning() {
    let (root, settings) = project("nostore");
    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    assert_eq!(level_of(&r, "credential store"), Level::Ok, "{r:#?}");
    assert_eq!(r.worst(), Level::Ok);
    assert!(!r.failed(true), "not even --strict may fail a paper deployment");
    let _ = std::fs::remove_dir_all(&root);
}

/// **RULE 3: a keyed `secrets.env` with NO database is SAID — loudly, by name — and still never
/// read.** A WARN (the box must still start on paper: absent credentials are the live gate), so
/// `--strict` refuses it, and the sentence names the file, says NOT READ and names
/// `vike-cli secrets migrate`. Never a value.
#[test]
fn a_keyed_credential_file_with_no_database_is_a_warning_naming_the_carry() {
    let (root, settings) = project("unread-file");
    std::fs::write(settings.join(vike_secrets::SECRETS_FILE), "BINANCE_LIVE_API_KEY=never-shown\n")
        .unwrap();
    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    let unread = r
        .findings
        .iter()
        .find(|f| f.subject == "credential store" && f.detail.contains("NOT READ"))
        .unwrap_or_else(|| panic!("the unread file must be a finding: {r:#?}"));
    assert_eq!(unread.level, Level::Warn, "{unread:?}");
    assert!(unread.detail.contains("vike-cli secrets migrate"), "{unread:?}");
    assert!(!unread.detail.contains("never-shown"), "a VALUE reached the report: {unread:?}");
    assert!(!r.failed(false), "the box still starts — on paper");
    assert!(r.failed(true), "…and --strict refuses it");
    let _ = std::fs::remove_dir_all(&root);
}

/// …but an absent store with the PRE-ONE-STORE `<project>/.env` sitting beside it is a WARNING,
/// and the pair is what makes `--strict` mean something. It cannot be a refusal: a `.env` is
/// also a legitimate systemd `EnvironmentFile` (the CI box's recorder ships one), which is exactly
/// the argument `crates/vike-secrets/src/store/warnings.rs`'s `LegacyStoreWarning` makes.
#[test]
fn a_legacy_dotenv_beside_an_absent_store_warns_without_failing() {
    let (root, settings) = project("legacy");
    std::fs::write(root.join(".env"), "POLY_PROXY_ENABLED=false\n").unwrap();

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    assert_eq!(r.worst(), Level::Warn, "{r:#?}");
    assert!(!r.failed(false), "a finding is never a refusal");
    assert!(r.failed(true), "…and --strict is the audience that wants it to be one");
    let _ = std::fs::remove_dir_all(&root);
}

/// …and its opposite, which is the ONE row whose level is computed. On a box with nothing armed
/// for live, a store that exists and cannot be opened is the DEGRADE
/// `docs/decisions/0013-degrade-vs-refuse.md` records for the daemon itself — every venue was
/// going to be paper anyway — so it warns and `--strict` counts it. Refusing here
/// unconditionally would stop a working paper daemon over a file it never opens.
///
/// ⚠ Since the credential FILE store was removed (2026-10-07) the credential store and the
/// settings rows are ONE file, so an unreadable credential store is an unreadable settings
/// database too — and THAT row refuses on its own, armed or not (see
/// `a_genuinely_unreadable_store_degrades_settings_and_credentials_together`). What this test
/// still pins is the credential row's OWN computed level: `Warn`, never a `Fail` it cannot justify.
#[test]
fn an_unreadable_credential_store_degrades_when_nothing_is_armed_for_live() {
    let (root, settings) = project("badstore");
    plant_unreadable_store(&settings);
    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    assert_eq!(level_of(&r, "credential store"), Level::Warn, "{r:#?}");
    assert!(r.failed(true), "…and --strict is the audience that wants it counted");
    let _ = std::fs::remove_dir_all(&root);
}

/// …and the SAME store on a box ARMED FOR LIVE is a refusal: the operator asked for a real
/// venue, the process cannot read the file that venue authenticates from, so it would place no
/// orders while every surface reads live. ADR 0013 question 2 — set-but-unhonoured.
///
/// ⚠ The variable is taken FROM `vike_config::CREDENTIAL_FILE_ARMING_REFUSED` rather than
/// spelled, for the two reasons `a_removed_variable_is_a_failure_carrying_its_replacement`
/// gives: it keys the assertion on the MECHANISM (whatever the arming table holds is what
/// counts as live) instead of one venue's flag, and a bare env-shaped literal in a `src/` file
/// is harvested as a READ by `crates/vike-model/src/scan.rs`'s literal sweep, which would demand
/// a `vike-cli` `SETTINGS` row for a variable this crate does not read.
#[test]
fn the_same_unreadable_store_fails_when_the_box_is_armed_for_live() {
    let (root, settings) = project("badstore-armed");
    plant_unreadable_store(&settings);

    for s in vike_config::CREDENTIAL_FILE_ARMING_REFUSED {
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[(s.var, "1")]));
        let f = find_of(&r, "credential store");
        assert_eq!(f.level, Level::Fail, "{} must arm the refusal: {r:#?}", s.var);
        assert!(f.detail.contains(s.var), "the refusal must NAME what armed it: {f:?}");
        assert!(r.failed(false), "an armed box must refuse WITHOUT --strict");
    }
    // …and a DISARMING value is not armed, so the same tree degrades again — the grammar is
    // `vike_config::armed_settings_in`'s, not a second opinion held here.
    let disarmed = vike_config::CREDENTIAL_FILE_ARMING_REFUSED[0].var;
    for v in ["0", "", "true"] {
        let r = inspect(Some(&settings), DirOrigin::Named, &map(&[(disarmed, v)]));
        assert_eq!(level_of(&r, "credential store"), Level::Warn, "{v:?} arms nothing: {r:#?}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// The disposition itself, driven directly — no filesystem, every branch, and the property that
/// makes the FAIL branch actionable: it names every arming source in one pass.
#[test]
fn the_unreadable_store_disposition_is_decided_by_the_arming_verdict_alone() {
    use vike_config::LiveArmingVerdict as V;
    assert_eq!(unreadable_store_finding("boom", &V::Unarmed).level, Level::Warn);

    let all = V::Armed(
        vike_config::CREDENTIAL_FILE_ARMING_REFUSED
            .iter()
            .map(vike_config::LiveArming::of_setting)
            .chain(std::iter::once(vike_config::TRADEHUB_LIVE_ARMING))
            .collect(),
    );
    let f = unreadable_store_finding("boom", &all);
    assert_eq!(f.level, Level::Fail);
    for s in vike_config::CREDENTIAL_FILE_ARMING_REFUSED {
        assert!(f.detail.contains(s.var), "every offender in ONE pass: {f:?}");
    }
    assert!(
        f.detail.contains(vike_config::TRADEHUB_LIVE_ARMING.source),
        "…including the NODE-scoped source, which is the one a switchless venue has: {f:?}"
    );
    assert!(f.detail.contains("boom"), "the store layer's own message survives: {f:?}");

    // ⚠ …and the third state. "Could not tell" refuses like an arm, and says WHY rather than
    // borrowing the armed wording, which would name a live venue nobody established.
    let u = unreadable_store_finding("boom", &V::Undetermined);
    assert_eq!(u.level, Level::Fail, "unknown must not degrade: {u:?}");
    assert!(u.detail.contains("could not be determined"), "{u:?}");
    assert!(!u.detail.contains("ARMED FOR LIVE ("), "it must not claim evidence: {u:?}");
}

/// **THE RESIDUAL THIS CHANGE CLOSED is now structurally UNREACHABLE, and this proves the
/// stronger property that replaced it.** The residual: a box armed ONLY by `flags.tradehub_live`
/// — no `{VENUE}_MAINNET` anywhere, the shape of every box live on one of the nine SWITCHLESS
/// venues — with its credential store unreadable, used to WARN and start all-paper instead of
/// refusing, because the node-scoped flag was invisible to the disposition that decided.
///
/// ⚠ **`docs/decisions/0086` retired the fixture that reproduced it, not the property.** The
/// scenario required `flags.tradehub_live` to be READABLE (a row) while the credential store was
/// UNREADABLE (a broken `secrets.env`) — two INDEPENDENT files under the old design. Under one
/// database (0054/0086) they are the SAME file: the settings rows and the credential table sit
/// in one `vike.db`, opened by one connection, so "the flag row reads fine but the credential
/// read fails" cannot be constructed at all any more — a store that will not open loses BOTH at
/// once: the `tradehub_live` row that would have proven the box armed is exactly as unreadable
/// as the credential it would have armed. The credential disposition therefore degrades to its
/// SAFE default (no provable arming ⇒ `Warn`, never a `Fail` it cannot actually justify) — and
/// the box still refuses to start, but for the honest reason: the settings database itself could
/// not be opened, a problem no box may run past, armed or not.
#[test]
fn a_genuinely_unreadable_store_degrades_settings_and_credentials_together() {
    let (root, settings) = project("badstore-node-armed");
    plant_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "flags".to_string(),
                key: "tradehub_live".to_string(),
                value: "true".to_string(),
            }],
            ..Default::default()
        },
    );
    // Corrupt the ONE file AFTER the plant closed it — the write already landed; this
    // simulates a box whose database has since gone bad (disk corruption, a botched restore),
    // not a fixture that never wrote one.
    std::fs::write(vike_secrets::db_path_in(&settings), b"not a sqlite file").unwrap();

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    // The settings half fails outright — the store genuinely could not be opened, and that is
    // unconditional: no armed-state computation can rescue it.
    let rows = find_of(&r, "settings database");
    assert_eq!(rows.level, Level::Fail, "{r:#?}");
    assert!(r.failed(false), "an unopenable settings database must refuse, armed or not");
    // The credential half's OWN disposition is COMPUTED from the armed-state, and the
    // armed-state is unknowable now — the very row that would have proven it is behind the
    // same unopenable file. It degrades to the safe default rather than claiming evidence it
    // does not have.
    let cred = find_of(&r, "credential store");
    assert_eq!(cred.level, Level::Warn, "{r:#?}");
    // …and, crucially, nothing here can be misread as the row's `true` having been honoured:
    // the row is unreadable, not merely absent, and no finding claims otherwise.
    assert!(
        !r.findings.iter().any(|f| f.detail.contains("tradehub_live")),
        "an unopenable store names no row from it: {r:#?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// …and the flag arms the disposition ONLY when the store is unreadable. An armed box whose
/// store is fine is a correctly configured live node, not a finding — this verb detects a false
/// belief, it does not object to going live.
#[test]
fn the_node_scoped_flag_is_not_itself_a_finding() {
    let (root, settings) = project("node-armed-ok");
    plant_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "flags".to_string(),
                key: "tradehub_live".to_string(),
                value: "true".to_string(),
            }],
            ..Default::default()
        },
    );
    write_credential_row(&settings, "BYBIT_DEMO_API_KEY", "k");

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    assert_eq!(r.worst(), Level::Ok, "arming is a configuration, not a defect: {r:#?}");
    assert!(!r.failed(true), "not even --strict");
    let _ = std::fs::remove_dir_all(&root);
}

/// A broken settings file is the same refusal the daemon performs, and the message names the
/// file — a check that passed here would contradict the binary it is checking.
#[test]
fn a_broken_settings_row_fails_and_names_the_key() {
    let (root, settings) = project("brokenrow");
    plant_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".to_string(),
                key: "max_leverage".to_string(),
                value: "\"not a number\"".to_string(),
            }],
            ..Default::default()
        },
    );
    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    // ⚠ A row that will not parse is MARKED, never a hard load error — see the enforcement point
    // this test now proves: `describe_with_source` never `Err`s for it, so the ONLY place this
    // verb can catch it is the explicit `seal_refusal` check above, as a `"settings rows"` FAIL.
    let f = find_of(&r, "settings rows");
    assert_eq!(f.level, Level::Fail, "{r:#?}");
    assert!(f.detail.contains("max_leverage"), "the offending key must be named: {f:?}");
    assert!(r.failed(false));
    let _ = std::fs::remove_dir_all(&root);
}

/// **Set-but-unhonoured is a REFUSAL** (ADR 0013 question 2): the operator named the directory,
/// it is not there, and everything downstream degrades to defaults with no error anywhere.
#[test]
fn a_named_settings_directory_that_is_not_there_fails() {
    let (root, settings) = project("named-missing");
    std::fs::remove_dir_all(&settings).unwrap();

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    let f = find_of(&r, "settings directory");
    assert_eq!(f.level, Level::Fail, "{r:#?}");
    assert!(r.failed(false));
    assert!(
        f.detail.contains(SETTINGS_DIR_ENV),
        "the refusal must name the variable that promised it: {f:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// …and the SAME missing directory reached by the WALK is only a warning. Nobody claimed it was
/// there: a checkout that has never configured anything is the ordinary state of this repo.
#[test]
fn the_same_directory_reached_by_the_walk_is_only_a_warning() {
    let (root, settings) = project("walk-missing");
    std::fs::remove_dir_all(&settings).unwrap();

    let r = inspect(Some(&settings), DirOrigin::Walk, &map(&[]));
    assert_eq!(level_of(&r, "settings directory"), Level::Warn, "{r:#?}");
    assert!(!r.failed(false), "an unconfigured checkout is not a failure");
    assert!(r.failed(true), "…but --strict is exactly the audience that wants it to be");
    let _ = std::fs::remove_dir_all(&root);
}

/// No project at all: a warning, and no credential-store row — the directory row already said
/// every venue stays paper, and a second warning for the same fact teaches people to scroll.
#[test]
fn no_settings_directory_warns_once() {
    let r = inspect(None, DirOrigin::Unresolved, &map(&[]));
    assert_eq!(level_of(&r, "settings directory"), Level::Warn, "{r:#?}");
    assert!(!r.findings.iter().any(|f| f.subject == "credential store"), "{r:#?}");
    assert!(!r.failed(false));
    assert!(r.failed(true));
}

/// The removed-variable row. ⚠ Unreachable through the shipped binary — `crate::run` refuses
/// first — so it is driven directly here, and the END-TO-END refusal is gated in
/// `crates/vike-cli/tests/config_check_cli.rs`.
///
/// ⚠ The variable is taken FROM the real table rather than spelled, for two reasons. It keys the
/// assertion on the MECHANISM (whatever is removed is refused, and the refusal carries the line
/// to paste) rather than on one name that a later phase will retire — and a bare `VIKE_*`
/// literal in a `src/` file is harvested as a READ by `crates/vike-model/src/scan.rs`'s literal
/// sweep, which would demand a `vike-cli` `SETTINGS` row for a variable this crate does not read.
#[test]
fn a_removed_variable_is_a_failure_carrying_its_replacement() {
    let removed = vike_config::REMOVED_ENV
        .iter()
        .find(|r| r.key.is_some() && r.echo_value)
        .expect("a removed variable whose value moved to a named key");

    let r = inspect(None, DirOrigin::Unresolved, &map(&[(removed.var, "250")]));
    assert_eq!(level_of(&r, "removed environment"), Level::Fail, "{r:#?}");
    let detail = &r.findings[0].detail;
    // ⚠ NOT `removed.file` (`"policy.toml"`) — that field is internal data the renderer derives
    // the SECTION word from (`docs/decisions/0086`: there is no settings file to name any more),
    // and the printed remedy is a `vike-cli config set` row write.
    let section = removed.file.trim_end_matches(".toml");
    assert!(detail.contains(&format!("vike-cli config set {section}.")), "{detail}");
    assert!(detail.contains(removed.key.unwrap()), "…and the key: {detail}");
    assert!(detail.contains("250"), "…with the operator's own value pasted in: {detail}");
}

// -- the exit-code rule ------------------------------------------------------------------------

#[test]
fn strict_promotes_warnings_and_nothing_promotes_an_ok() {
    let ok = Report {
        settings_dir: None,
        origin: DirOrigin::Unresolved,
        findings: vec![finding("a", Level::Ok, "x")],
    };
    assert!(!ok.failed(false) && !ok.failed(true));

    let warn = Report { findings: vec![finding("a", Level::Warn, "x")], ..ok.clone() };
    assert!(!warn.failed(false));
    assert!(warn.failed(true));

    let fail = Report { findings: vec![finding("a", Level::Fail, "x")], ..ok.clone() };
    assert!(fail.failed(false) && fail.failed(true));
}

#[test]
fn the_level_words_are_pinned() {
    assert_eq!(Level::Ok.as_str(), "ok");
    assert_eq!(Level::Warn.as_str(), "warn");
    assert_eq!(Level::Fail.as_str(), "fail");
    assert!(Level::Ok < Level::Warn && Level::Warn < Level::Fail, "worst() depends on this");
}

// -- redaction ---------------------------------------------------------------------------------

/// No credential VALUE reaches a finding, whatever the store holds — the store is disclosed by
/// COUNT, and no key NAME is printed either.
#[test]
fn no_finding_carries_a_credential() {
    const LEAK: &str = "sk-do-not-print-me";
    let (root, settings) = project("redact");
    write_store(
        &settings,
        &format!("BINANCE_LIVE_API_KEY={LEAK}\nOKX_DEMO_API_PASSPHRASE={LEAK}\n"),
    );

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    let rendered = format!("{r:#?}");
    assert!(!rendered.contains(LEAK), "a credential VALUE leaked: {rendered}");
    assert!(!rendered.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked: {rendered}");
    assert!(rendered.contains("2 key(s)"), "…and the count is what IS disclosed: {rendered}");
    // …and the same property on the SERIALIZED document, which is what a monitor scrapes.
    let json = serde_json::to_string(&report_json(&r, false)).unwrap();
    assert!(!json.contains(LEAK) && !json.contains("BINANCE_LIVE_API_KEY"), "{json}");
    let _ = std::fs::remove_dir_all(&root);
}

// -- the stranded venue settings (decision 0095, Task 7) -----------------------------------------

/// The finding's subject — what a reader of the report sees in the second column.
const STRANDED: &str = "stranded venue settings";

/// **A credential row under a venue setting's OLD name fails the check, and the check names it.**
///
/// Until this finding existed the daemons refused such a store at their own boot
/// (`vike_boot::boot`'s step 3) and this verb passed it — so a container entrypoint whose whole
/// pre-flight is `config check` started the daemon and let IT refuse, while the check had said the
/// box was fine. The finding is `vike_config::refuse_stranded_venue_settings`'s own refusal, so
/// the rows it names are the rows the daemon names. Names only: the value planted here must reach
/// neither the report nor its JSON, and neither may the name of the credential beside it.
#[test]
fn a_stranded_venue_setting_fails_the_check_and_names_the_row_not_its_value() {
    const LEAK: &str = "sk-do-not-print-me";
    let stranded = concat!("POLY", "_RATE_GATE");
    let (root, settings) = project("stranded");
    write_store(&settings, &format!("{stranded}={LEAK}\nBINANCE_LIVE_API_KEY={LEAK}\n"));

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    let f = find_of(&r, STRANDED);
    assert_eq!(f.level, Level::Fail, "{r:#?}");
    assert!(r.failed(false), "the daemon refuses this store, so the check must too: {r:#?}");
    assert!(f.detail.contains(stranded), "the row must be named: {}", f.detail);
    assert!(f.detail.contains("venue.polymarket.rate_gate"), "…and its setting: {}", f.detail);
    assert!(f.detail.contains("vike-cli secrets move-venue-config"), "…and the verb: {}", f.detail);

    let rendered = format!("{r:#?}");
    let json = serde_json::to_string(&report_json(&r, false)).unwrap();
    for text in [&rendered, &json] {
        assert!(!text.contains(LEAK), "a credential VALUE leaked: {text}");
        assert!(!text.contains("BINANCE_LIVE_API_KEY"), "a credential NAME leaked: {text}");
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// **It is the daemons' set, not a list of its own.** A labelled spelling of a TIER-scoped field is
/// stranded (ruling 10) and a credential that merely looks like a gateway row is not; the report
/// names exactly what `vike_secrets::venue_setting::stranded_venue_setting_names` returns for the
/// same names, so a field added to the catalog is judged here the moment the daemons judge it.
#[test]
fn the_check_names_exactly_the_set_the_daemons_refuse() {
    let names = [
        concat!("IBKR", "_DEMO_HOST__HEDGE"),
        concat!("IBKR", "_DEMO_PORT"),
        concat!("DUKASCOPY", "_DEMO1_SERVER"),
        concat!("OKX", "_DEMO_API_PASSPHRASE"),
        concat!("IBKR", "_DEMO_API_KEY"),
    ];
    let (root, settings) = project("stranded-set");
    write_store(&settings, &names.map(|n| format!("{n}=v\n")).concat());

    let daemon_set: Vec<String> = vike_secrets::venue_setting::stranded_venue_setting_names(names)
        .into_iter()
        .map(|(name, _setting)| name)
        .collect();
    assert!(daemon_set.len() >= 3 && daemon_set.len() < names.len(), "the grid is not trivial");

    let r = inspect(Some(&settings), DirOrigin::Named, &map(&[]));
    let detail = &find_of(&r, STRANDED).detail;
    for name in names {
        assert_eq!(
            detail.contains(&format!("  {name}  is the setting")),
            daemon_set.iter().any(|d| d == name),
            "`{name}` is named by the check iff the daemons refuse it: {detail}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

// -- arg parsing + the printers ----------------------------------------------------------------

#[test]
fn flags_parse_and_default_to_the_human_non_strict_view() {
    assert_eq!(parse_args(std::iter::empty()).unwrap(), Args::default());
    let a = parse_args(["--json", "--strict"].map(String::from).into_iter()).unwrap();
    assert!(a.json && a.strict);
}

#[test]
fn a_bad_flag_is_a_clean_error() {
    assert!(parse_args(["--nope".to_string()].into_iter()).unwrap_err().contains("--nope"));
    assert!(
        parse_args(["--json=1".to_string()].into_iter()).unwrap_err().contains("takes no value")
    );
    assert_eq!(parse_args(["-h".to_string()].into_iter()).unwrap_err(), "help requested");
}

#[test]
fn usage_documents_both_flags_and_the_exit_contract() {
    for needle in ["usage:", "--json", "--strict", "exit 0", "exit 1"] {
        assert!(USAGE.contains(needle), "USAGE must mention {needle}");
    }
}

#[test]
fn both_printers_render_every_level_without_panicking() {
    let r = Report {
        settings_dir: Some(PathBuf::from("/srv/x/settings")),
        origin: DirOrigin::Named,
        findings: vec![
            finding("ok row", Level::Ok, "fine"),
            finding("warn row", Level::Warn, "a degrade"),
            // a MULTI-LINE detail, the shape `refuse_removed_env` returns
            finding("fail row", Level::Fail, "first line\nsecond line\n"),
        ],
    };
    print_human(&r, false);
    print_human(&r, true);
    let doc = report_json(&r, false);
    for field in
        ["settings_dir", "settings_dir_origin", "strict", "ok", "failures", "warnings", "findings"]
    {
        assert!(doc.get(field).is_some(), "missing {field}");
    }
    assert_eq!(doc["ok"], serde_json::Value::Bool(false));
    assert_eq!(doc["failures"], serde_json::json!(1));
    assert_eq!(doc["warnings"], serde_json::json!(1));
    // …and the empty-directory header path.
    print_human(
        &Report { settings_dir: None, origin: DirOrigin::Unresolved, findings: vec![] },
        false,
    );
}
