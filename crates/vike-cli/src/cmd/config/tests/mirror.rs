use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse_args(args.iter().map(|s| (*s).to_string()))
}

#[test]
fn the_flag_parser_takes_dry_run_and_refuses_anything_else() {
    assert!(!parse_of(&["--profile", "p.toml"]).unwrap().dry_run);
    assert!(parse_of(&["--dry-run", "--profile", "p.toml"]).unwrap().dry_run);
    let err = parse_of(&["--force", "--profile", "p.toml"]).unwrap_err();
    assert!(err.contains("--force"), "{err}");
    // `--help` travels back through the `Err` channel as the shared sentinel, which
    // `args::exit_for_parse_error` turns into a SUCCESS printing the usage to stdout.
    assert!(parse_of(&["--help"]).is_err());
    assert!(
        parse_of(&["--dry-run=1", "--profile", "p.toml"]).is_err(),
        "a bare boolean takes no inline value"
    );
}

/// Each profile flag takes a value, may NOT repeat, and its `-name` sibling needs it.
///
/// ⚠ `--profile` used to repeat, and it does not any more: what it writes changed from a
/// `[risk]` disclosure row keyed by file name to a BODY an operator then activates BY NAME, and
/// one document at a time is the honest shape of that.
#[test]
fn each_profile_flag_takes_one_value_and_its_name_flag_needs_it() {
    for (flag, name_flag) in [
        ("--profile", "--profile-name"),
        ("--daemon", "--daemon-name"),
        ("--recorder", "--recorder-name"),
    ] {
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

/// **The sentence the help used to print is GONE, and the one that replaced it is there.**
///
/// ⚠ The old page said, of `--profile`: *"Nothing reads the rows: the profile FILE still judges
/// every order."* It was TRUE — that flag wrote a disclosure mirror with no `active` column
/// whose reader was forbidden by name — and it is the sentence an operator would have read
/// before deciding this migration was pointless. It must not survive the change that makes it
/// false, and a `const` cannot be checked by reading it, so it is checked here.
#[test]
fn the_help_page_names_every_flag_and_no_longer_speaks_of_settings_files() {
    for flag in [
        "--dry-run",
        "--profile",
        "--profile-name",
        "--daemon",
        "--daemon-name",
        "--recorder",
        "--recorder-name",
    ] {
        assert!(USAGE.contains(flag), "the help page does not name `{flag}`:\n{USAGE}");
    }
    assert!(
        !USAGE.contains("--no-settings"),
        "the retired flag survived the change that removed the half it gated:\n{USAGE}"
    );
    assert!(
        USAGE.contains("THE ROWS BIND"),
        "the help must say what the rows now DO, not merely stop saying what they did \
             not:\n{USAGE}"
    );
    assert!(
        USAGE.contains("config activate"),
        "…and name the separate act that makes a body bind:\n{USAGE}"
    );
}

/// With no profile flag at all there is nothing left for this verb to do — the settings-file
/// half is gone (0086), so a bare `config mirror` is a usage error rather than a silent no-op.
#[test]
fn no_profile_flag_at_all_is_refused() {
    let err = parse_of(&[]).unwrap_err();
    assert!(err.contains("--profile"), "{err}");
    assert!(err.contains("--recorder"), "{err}");
    assert!(parse_of(&["--profile", "p.toml"]).is_ok());
}

/// **The NAME defaults to the file STEM, not the file name** — a name is what an operator types
/// into `config activate`, and a `.toml` inside one reads as a path.
#[test]
fn the_default_profile_name_is_the_file_stem() {
    assert_eq!(default_name(Path::new("/srv/x/settings/run-live.toml")), "run-live");
    assert_eq!(default_name(Path::new("tradehub.toml")), "tradehub");
    // A path with no stem renders as whatever was typed rather than as an empty name.
    assert_eq!(default_name(Path::new("..")), "..");
}

#[test]
fn a_run_with_no_settings_directory_refuses_rather_than_guessing_one() {
    let args = Args { profile: Some(PathBuf::from("p.toml")), ..Args::default() };
    let err = execute(&args, None).unwrap_err();
    assert!(err.contains("VIKE_SETTINGS_DIR"), "{err}");
}

/// ⚠ The refusal that matters: a project with NO database is not served by creating one. The
/// existence of that file is what makes the database — rather than `secrets.env` — answer for
/// every credential on the box. `plan` reads only the file, so this refusal comes from the
/// WRITE half, once `--dry-run` is off.
#[test]
fn a_project_with_no_database_is_refused_and_none_is_created() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("run-live.toml");
    std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 2.0\n").unwrap();

    let err =
        execute(&Args { profile: Some(profile), ..Args::default() }, Some(tmp.path())).unwrap_err();
    assert!(err.contains("secrets migrate"), "the refusal must name the repair: {err}");
    assert!(
        !vike_secrets::db_path_in(tmp.path()).exists(),
        "no database may be left behind by a refused mirror"
    );
}

/// A dry run names the profile's rows and still writes nothing.
#[test]
fn a_dry_run_reports_the_profile_rows_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("run-live.toml");
    std::fs::write(
        &profile,
        "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 250.0\n\
             max_total_exposure = 1000.0\n",
    )
    .unwrap();

    let out = execute(
        &Args { dry_run: true, profile: Some(profile), ..Args::default() },
        Some(tmp.path()),
    )
    .unwrap();
    assert!(out.contains("run `run-live` (0 mount(s), 3 setting(s)"), "{out}");
    assert!(out.contains("risk.max_notional_per_order = 250.0"), "{out}");
    assert!(out.contains("No active run row was written"), "storing is not selecting: {out}");
    assert!(out.contains("config activate run"), "the report names the deliberate act: {out}");
    assert!(out.contains("NOTHING WAS WRITTEN"), "{out}");
    assert!(!vike_secrets::db_path_in(tmp.path()).exists());
}

/// A typo'd `[risk]` key refuses the WHOLE run — before anything is written, so a half-mirrored
/// store is not a state this verb can produce.
#[test]
fn a_bad_risk_key_refuses_the_run_by_name_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("run-live.toml");
    std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_levrage = 3.0\n").unwrap();

    let err =
        execute(&Args { profile: Some(profile), ..Args::default() }, Some(tmp.path())).unwrap_err();
    assert!(err.contains("max_levrage"), "{err}");
    assert!(
        !vike_secrets::db_path_in(tmp.path()).exists(),
        "the profile is lowered BEFORE any write, so nothing was created"
    );
}

/// **The three flags are compared pairwise, in WRITE ORDER, and every pair is covered.**
///
/// A loop that compared only neighbours would miss `run` against `recorder`, which is the pair
/// the measured defect actually used (`--profile-name default` plus `--recorder`'s own default
/// name), so the middle flag is left out of two of the three cases deliberately.
#[test]
fn every_pair_of_profile_names_is_compared_and_a_distinct_set_is_allowed() {
    for (run, daemon, recorder, first, second) in [
        (Some("x"), Some("x"), None, "run", "daemon"),
        (Some("x"), None, Some("x"), "run", "recorder"),
        (None, Some("x"), Some("x"), "daemon", "recorder"),
        (Some("x"), Some("x"), Some("x"), "run", "daemon"),
    ] {
        let err = refuse_one_name_under_two_kinds(run, daemon, recorder)
            .expect_err("one name under two kinds must be refused");
        assert!(err.contains("`x`"), "the refusal must name the name: {err}");
        assert!(err.contains(&format!("`{first}` profile")), "names the first kind: {err}");
        assert!(err.contains(&format!("`{second}` profile")), "names the second kind: {err}");
        assert!(err.contains("NOTHING WAS WRITTEN"), "and it is TRUE here: {err}");
    }
    // ...and three distinct names, or fewer than two flags, are not a collision.
    for (run, daemon, recorder) in [
        (Some("a"), Some("b"), Some("c")),
        (Some("a"), None, None),
        (None, None, Some("a")),
        (None, None, None),
    ] {
        assert!(refuse_one_name_under_two_kinds(run, daemon, recorder).is_ok());
    }
}

/// **A `--recorder` that is not given may not collide**, which is the one case the report's
/// unconditional `recorder_name` makes easy to get wrong: that name is resolved for every run
/// so the dry-run can print it, and comparing it without the FLAG would refuse
/// `--profile-name default` on a box that never asked for a recorder body.
#[test]
fn a_recorder_name_with_no_recorder_flag_is_not_a_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("run-live.toml");
    std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 10.0\n").unwrap();
    let out = execute(
        &Args {
            dry_run: true,
            profile: Some(profile),
            profile_name: Some(crate::cmd::config::mirror_recorder::DEFAULT_PROFILE_NAME.into()),
            ..Args::default()
        },
        Some(tmp.path()),
    )
    .expect("no recorder was asked for, so `default` is free");
    assert!(out.contains("run `default`"), "{out}");
}

/// **THE MEASURED DEFECT, at the rung that decides it.** `--profile-name default` beside a
/// `--recorder` whose own default name is `default` refused NOTHING at plan time, reported
/// *would mirror* in `--dry-run` and exited 0, then wrote the run body and had the store refuse
/// the recorder half — printing `NOTHING WAS WRITTEN` over a committed row.
///
/// Asserted through [`execute`] rather than through the helper so the DRY RUN is covered: it is
/// the half that reported a write the store was going to refuse, and a check placed one line
/// lower (after the plans, before the writes) would pass the helper's own test and still
/// promise it.
#[test]
fn one_name_under_two_kinds_is_refused_before_any_plan_dry_run_included() {
    let tmp = tempfile::tempdir().unwrap();
    let profile = tmp.path().join("run-live.toml");
    std::fs::write(&profile, "mode = \"live\"\n\n[risk]\nmax_notional_per_order = 10.0\n").unwrap();
    let rec = tmp.path().join("rec.toml");
    std::fs::write(&rec, "store = \"market_data/hist\"\n").unwrap();

    for dry_run in [true, false] {
        let outcome = execute(
            &Args {
                dry_run,
                profile: Some(profile.clone()),
                profile_name: Some("default".to_string()),
                recorder: Some(rec.clone()),
                ..Args::default()
            },
            Some(tmp.path()),
        );
        let Err(err) = outcome else {
            panic!("one name under two kinds must be refused (dry_run: {dry_run})")
        };
        assert!(err.contains("`run` profile") && err.contains("`recorder` profile"), "{err}");
        assert!(err.contains("NOTHING WAS WRITTEN"), "{err}");
        assert!(!err.contains("would mirror"), "a rehearsal may not promise it: {err}");
        assert!(
            !vike_secrets::db_path_in(tmp.path()).exists(),
            "refused ABOVE the write phase, so not even a store was reached"
        );
    }

    // ⚠ …and ABOVE THE PLANS, which the assertions so far cannot see: a plan writes nothing
    // either, so "no database exists" is true of both placements. What distinguishes them is
    // WHICH refusal an operator reads when there is also something wrong with the file. With
    // the check above the plans it is the collision; one line lower it is whatever
    // `mirror_profile::plan` says about a file it cannot open — and the operator then
    // fixes the file and meets the collision on the next run instead of this one.
    let err = execute(
        &Args {
            profile: Some(tmp.path().join("not-here.toml")),
            profile_name: Some("default".to_string()),
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
    let err = write_phase_error(&[], "the four settings files", "boom. NOTHING WAS WRITTEN.");
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
            &["the `setting` and `venue_arming` tables".to_string()],
            "the run profile `x`",
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
        assert!(err.contains("`setting` and `venue_arming`"), "it names what landed: {err}");
        assert!(err.contains("the run profile `x`"), "…and what failed: {err}");
    }
    // Two landings read as English, which is the only thing the plural arm decides.
    let two = write_phase_error(&["a".to_string(), "b".to_string()], "c", "boom");
    assert!(two.contains("a, b have already committed"), "{two}");
}
