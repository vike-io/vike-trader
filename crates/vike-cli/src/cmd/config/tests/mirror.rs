use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse_args(args.iter().map(|s| (*s).to_string()))
}

/// A single-mount daemon profile that round-trips through the renderer unchanged.
const DAEMON: &str = "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\nsymbol = \"BTCUSDT\"\n";

#[test]
fn the_flag_parser_takes_dry_run_and_refuses_anything_else() {
    assert!(!parse_of(&["--daemon", "d.toml"]).unwrap().dry_run);
    assert!(parse_of(&["--dry-run", "--daemon", "d.toml"]).unwrap().dry_run);
    let err = parse_of(&["--force", "--daemon", "d.toml"]).unwrap_err();
    assert!(err.contains("--force"), "{err}");
    // `--help` travels back through the `Err` channel as the shared sentinel, which
    // `args::exit_for_parse_error` turns into a SUCCESS printing the usage to stdout.
    assert!(parse_of(&["--help"]).is_err());
    assert!(
        parse_of(&["--dry-run=1", "--daemon", "d.toml"]).is_err(),
        "a bare boolean takes no inline value"
    );
}

/// Each profile flag takes a value, may NOT repeat, and its `-name` sibling needs it.
#[test]
fn each_profile_flag_takes_one_value_and_its_name_flag_needs_it() {
    for (flag, name_flag) in [("--daemon", "--daemon-name"), ("--recorder", "--recorder-name")] {
        assert!(parse_of(&[flag, "a.toml"]).is_ok(), "{flag}");
        let err = parse_of(&[flag, "a.toml", flag, "b.toml"]).unwrap_err();
        assert!(err.contains("given twice"), "{flag}: {err}");
        assert!(err.contains(name_flag), "the refusal names the real answer: {err}");
        let err = parse_of(&[name_flag, "x"]).unwrap_err();
        assert!(err.contains(flag), "{name_flag} alone configures nothing: {err}");
        // The shared rule: a valued flag may not eat a following flag.
        assert!(parse_of(&[flag, "--dry-run"]).is_err(), "{flag}");
        assert!(parse_of(&[flag]).is_err(), "{flag}");
    }
}

/// **The RUN profile's flags are refused BY NAME** (decision 0111): no file is imported for the
/// run profile any more, and an operator following an old runbook must be told which command
/// replaced the flag rather than read "unknown option" for a flag that shipped for months.
#[test]
fn the_run_profile_flags_are_refused_by_name_naming_the_writer() {
    for line in [
        &["--profile", "run-live.toml"][..],
        &["--profile-name", "run-live"][..],
        &["--daemon", "d.toml", "--profile", "run-live.toml"][..],
    ] {
        let err = parse_of(line).unwrap_err();
        assert!(err.contains("--profile is REMOVED"), "{line:?}: {err}");
        assert!(err.contains("vike-cli config bootstrap-run"), "names the writer: {err}");
    }
}

/// The help page names every flag this verb takes, none it no longer takes, and the two other
/// writers an operator needs: the deliberate `config activate`, and `bootstrap-run` for the run
/// profile this verb no longer stores.
#[test]
fn the_help_page_names_every_flag_and_no_longer_speaks_of_settings_files() {
    for flag in ["--dry-run", "--daemon", "--daemon-name", "--recorder", "--recorder-name"] {
        assert!(USAGE.contains(flag), "the help page does not name `{flag}`:\n{USAGE}");
    }
    for gone in ["--no-settings", "--profile"] {
        assert!(!USAGE.contains(gone), "a retired flag is still on the help page:\n{USAGE}");
    }
    assert!(
        USAGE.contains("config activate"),
        "…and name the separate act that makes a body bind:\n{USAGE}"
    );
    assert!(USAGE.contains("bootstrap-run"), "…and where the run profile went:\n{USAGE}");
}

/// With no profile flag at all there is nothing left for this verb to do — the settings-file
/// half is gone (0086), so a bare `config mirror` is a usage error rather than a silent no-op.
#[test]
fn no_profile_flag_at_all_is_refused() {
    let err = parse_of(&[]).unwrap_err();
    assert!(err.contains("--daemon"), "{err}");
    assert!(err.contains("--recorder"), "{err}");
    assert!(err.contains("bootstrap-run"), "and names where the run profile went: {err}");
    assert!(parse_of(&["--daemon", "d.toml"]).is_ok());
}

/// **The NAME defaults to the file STEM, not the file name** — a name is what an operator types
/// into `config activate`, and a `.toml` inside one reads as a path.
#[test]
fn the_default_profile_name_is_the_file_stem() {
    assert_eq!(default_name(Path::new("/srv/x/settings/tradehub.toml")), "tradehub");
    assert_eq!(default_name(Path::new("rec.toml")), "rec");
    // A path with no stem renders as whatever was typed rather than as an empty name.
    assert_eq!(default_name(Path::new("..")), "..");
}

#[test]
fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
    let args = Args { daemon: Some(PathBuf::from("d.toml")), ..Args::default() };
    let err = execute(&args, None).unwrap_err();
    assert!(err.contains("VIKE_SETTINGS_DIR"), "{err}");
}

/// ⚠ The refusal that matters: a project with NO database is not served by creating one. The
/// database is created by `vike-cli secrets init` and nothing else. `plan` reads only the file, so
/// this refusal comes from the WRITE half, once `--dry-run` is off.
#[test]
fn a_project_with_no_database_is_refused_and_none_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("tradehub.toml");
    std::fs::write(&profile, DAEMON).unwrap();

    let err =
        execute(&Args { daemon: Some(profile), ..Args::default() }, Some(tmp.path())).unwrap_err();
    assert!(err.contains("secrets init"), "the refusal must name the repair: {err}");
    assert!(
        !vike_secrets::db_path_in(tmp.path()).exists(),
        "no database may be left behind by a refused mirror"
    );
}

/// A dry run names the profile's rows and still writes nothing.
#[test]
fn a_dry_run_reports_the_profile_rows_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("tradehub.toml");
    std::fs::write(&profile, DAEMON).unwrap();

    let out = execute(
        &Args { dry_run: true, daemon: Some(profile), ..Args::default() },
        Some(tmp.path()),
    )
    .unwrap();
    assert!(out.contains("daemon `tradehub` (1 mount(s), 0 setting(s)"), "{out}");
    assert!(out.contains("mount 0 bybit/BTCUSDT [CryptoPerp]"), "{out}");
    assert!(out.contains("No active daemon row was written"), "storing is not selecting: {out}");
    assert!(out.contains("config activate daemon"), "the report names the deliberate act: {out}");
    assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
    assert!(!vike_secrets::db_path_in(tmp.path()).exists());
}

/// A typo'd mount key refuses the WHOLE run — before anything is written, so a half-mirrored
/// store is not a state this verb can produce.
#[test]
fn a_bad_key_refuses_the_run_by_name_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("tradehub.toml");
    std::fs::write(&profile, format!("{DAEMON}qtyy = 1.0\n")).unwrap();

    let err =
        execute(&Args { daemon: Some(profile), ..Args::default() }, Some(tmp.path())).unwrap_err();
    assert!(err.contains("qtyy"), "{err}");
    assert!(
        !vike_secrets::db_path_in(tmp.path()).exists(),
        "the profile is lowered BEFORE any write, so nothing was created"
    );
}

/// **The two flags are compared in WRITE ORDER**, and a distinct pair, or fewer than two flags, is
/// not a collision.
#[test]
fn the_pair_of_profile_names_is_compared_and_a_distinct_pair_is_allowed() {
    let err = refuse_one_name_under_two_kinds(Some("x"), Some("x"))
        .expect_err("one name under two kinds must be refused");
    assert!(err.contains("`x`"), "the refusal must name the name: {err}");
    assert!(err.contains("`daemon` profile"), "names the first kind: {err}");
    assert!(err.contains("`recorder` profile"), "names the second kind: {err}");
    assert!(err.contains("NOTHING WAS WRITTEN"), "and it is TRUE here: {err}");
    for (daemon, recorder) in [(Some("a"), Some("b")), (Some("a"), None), (None, Some("a"))] {
        assert!(refuse_one_name_under_two_kinds(daemon, recorder).is_ok());
    }
    assert!(refuse_one_name_under_two_kinds(None, None).is_ok());
}

/// **A `--recorder` that is not given may not collide**, which is the one case the report's
/// unconditional `recorder_name` makes easy to get wrong: that name is resolved for every run
/// so the dry-run can print it, and comparing it without the FLAG would refuse
/// `--daemon-name default` on a box that never asked for a recorder body.
#[test]
fn a_recorder_name_with_no_recorder_flag_is_not_a_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("tradehub.toml");
    std::fs::write(&profile, DAEMON).unwrap();
    let out = execute(
        &Args {
            dry_run: true,
            daemon: Some(profile),
            daemon_name: Some(crate::cmd::config::mirror_recorder::DEFAULT_PROFILE_NAME.into()),
            ..Args::default()
        },
        Some(tmp.path()),
    )
    .expect("no recorder was asked for, so `default` is free");
    assert!(out.contains("daemon `default`"), "{out}");
}

/// **THE MEASURED DEFECT, at the rung that decides it.** A profile name beside a `--recorder`
/// whose own default name is `default` refused NOTHING at plan time, reported *would mirror* in
/// `--dry-run` and exited 0, then wrote the first body and had the store refuse the recorder half
/// — printing `NOTHING WAS WRITTEN` over a committed row. (Measured with the run plane's
/// `--profile-name default`, which decision 0111 removed; `--daemon-name default` is the same
/// collision between the two halves that are left.)
///
/// Asserted through [`execute`] rather than through the helper so the DRY RUN is covered: it is
/// the half that reported a write the store was going to refuse.
#[test]
fn one_name_under_two_kinds_is_refused_before_any_plan_dry_run_included() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("tradehub.toml");
    std::fs::write(&profile, DAEMON).unwrap();
    let rec = tmp.path().join("rec.toml");
    std::fs::write(&rec, "store = \"market_data/hist\"\n").unwrap();

    for dry_run in [true, false] {
        let outcome = execute(
            &Args {
                dry_run,
                daemon: Some(profile.clone()),
                daemon_name: Some("default".to_string()),
                recorder: Some(rec.clone()),
                ..Args::default()
            },
            Some(tmp.path()),
        );
        let Err(err) = outcome else {
            panic!("one name under two kinds must be refused (dry_run: {dry_run})")
        };
        assert!(err.contains("`daemon` profile") && err.contains("`recorder` profile"), "{err}");
        assert!(err.contains("NOTHING WAS WRITTEN"), "{err}");
        assert!(!err.contains("would mirror"), "a rehearsal may not promise it: {err}");
        assert!(
            !vike_secrets::db_path_in(tmp.path()).exists(),
            "refused ABOVE the write phase, so not even a store was reached"
        );
    }

    // ⚠ …and ABOVE THE PLANS: with something also wrong with the file, the operator reads the
    // COLLISION, not whatever `mirror_profile::plan` says about a file it cannot open.
    let err = execute(
        &Args {
            daemon: Some(tmp.path().join("not-here.toml")),
            daemon_name: Some("default".to_string()),
            recorder: Some(rec),
            ..Args::default()
        },
        Some(tmp.path()),
    )
    .expect_err("the collision is decided before the file is opened");
    assert!(err.contains("`recorder` profile"), "the COLLISION, not the file: {err}");
    assert!(!err.contains("not-here.toml"), "the file was never opened: {err}");
}

/// **A failure with nothing on disk passes the store's words through UNCHANGED** — they are
/// true at that point, and re-scoping a true sentence would be its own small lie.
#[test]
fn a_failure_before_any_write_inherits_the_stores_own_words() {
    let err = write_phase_error(&[], "the daemon profile `x`", "boom. NOTHING WAS WRITTEN.");
    assert_eq!(err, "boom. NOTHING WAS WRITTEN.");
}

/// **…and a failure AFTER a half has committed may not print the run-scope claim.**
///
/// Every spelling the tree actually writes is covered, because the store messages reachable
/// from this verb use all three: `vike_secrets::DbError`'s `NoSettingsDatabase` shouts it,
/// `ForeignKeys` capitalises the sentence, `Unclassified` lower-cases it.
#[test]
fn a_failure_after_a_half_committed_rescopes_the_claim_and_names_what_landed() {
    for raw in [
        "refused. NOTHING WAS WRITTEN.",
        "refused. Nothing was written.",
        "refused. nothing was written.",
    ] {
        let err = write_phase_error(
            &["the daemon profile `tradehub`".to_string()],
            "the recorder profile `x`",
            raw,
        );
        assert!(
            !err.contains("NOTHING WAS WRITTEN.")
                && !err.contains("Nothing was written.")
                && !err.contains("nothing was written."),
            "the run-scope claim is FALSE here and may not print: {err}"
        );
        assert!(err.contains("BY THAT ONE WRITE") || err.contains("by that one write"), "{err}");
        assert!(err.contains("PART OF THIS RUN WAS ALREADY WRITTEN"), "{err}");
        assert!(err.contains("the daemon profile `tradehub`"), "it names what landed: {err}");
        assert!(err.contains("the recorder profile `x`"), "…and what failed: {err}");
    }
    // Two landings read as English, which is the only thing the plural arm decides.
    let two = write_phase_error(&["a".to_string(), "b".to_string()], "c", "boom");
    assert!(two.contains("a, b have already committed"), "{two}");
}
