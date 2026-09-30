use super::*;

fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
    v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
}

/// The three outcomes, as PARSE results. The exit status and the stream each produces are
/// asserted over the shipped binary in `tests/help_cli.rs` — only a real run shows those.
#[test]
fn help_and_version_are_outcomes_not_errors_and_a_bare_invocation_runs() {
    for flag in ["-h", "--help"] {
        assert!(matches!(parse_args(argv(&[flag])), Ok(Parsed::Help)), "{flag}");
    }
    for flag in ["-V", "--version"] {
        assert!(matches!(parse_args(argv(&[flag])), Ok(Parsed::Version)), "{flag}");
    }
    assert!(
        matches!(parse_args(argv(&[])), Ok(Parsed::Run(None))),
        "no args is the server, recording nothing — the pre-merge behaviour, byte for byte"
    );
}

/// The five flags ruling 10 added, and the defaults they leave behind.
///
/// ⚠ **`--record PATH` is RETIRED (0086) and its path resolves to [`ProfileSource::ActiveRow`],
/// never [`ProfileSource::File`]** — that variant does not exist any more. The path is still
/// TAKEN (so the flag parses on an unedited unit) and reported back in the warning, never as the
/// profile source.
#[test]
fn the_record_flags_parse() {
    let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--record", "r.toml"])) else {
        panic!("`--record PATH` is a run that records");
    };
    assert_eq!(req.profile, ProfileSource::ActiveRow, "the path is IGNORED as a source");
    let w = req.retired_flag_warning.expect("`--record` must warn, never accept in silence");
    assert!(w.contains("--record r.toml"), "the warning names what was given: {w}");
    assert!(w.contains("RETIRED"), "…and that the flag itself is retiring: {w}");
    assert!(w.contains("--recorder-profile"), "…and what to write instead: {w}");
    assert_eq!(req.tick_secs, DEFAULT_TICK_SECS);
    assert_eq!(req.silent_secs, DEFAULT_SILENT_SECS);
    assert!(!req.once);
    assert!(
        !req.exit_on_silence,
        "the default reaction to a silent series is ALERT, never exit — and since the merge an \
             exit takes the data wire down with the feeds"
    );

    let Ok(Parsed::Run(Some(req))) = parse_args(argv(&[
        "--record",
        "r.toml",
        "--tick-secs",
        "5",
        "--silent-secs",
        "60",
        "--exit-on-silence",
        "--once",
    ])) else {
        panic!("a full recording command line parses");
    };
    assert_eq!(req.tick_secs, 5);
    assert_eq!(req.silent_secs, 60);
    assert!(req.exit_on_silence);
    assert!(req.once);
}

/// ⚠ A companion flag WITHOUT `--record` is refused rather than ignored, and the message names
/// the flag that needs the profile. Ignoring them would start a plain server for `--once` (a
/// dry run that never ends) and leave `--silent-secs 0` looking like a watchdog somebody turned
/// off — the "believes something is armed that is not" class the recorder's own parser already
/// refused within `--record`.
#[test]
fn a_recording_flag_without_a_profile_is_refused() {
    for flag in ["--once", "--exit-on-silence"] {
        let err = parse_args(argv(&[flag])).expect_err("{flag} alone must be a usage error");
        assert!(err.contains(flag), "the message names the offending flag: {err}");
        assert!(err.contains("--record"), "…and what it needs: {err}");
    }
    let err = parse_args(argv(&["--tick-secs", "5"])).expect_err("a value flag alone too");
    assert!(err.contains("--record"), "{err}");
}

/// The flag/value rules the recorder's parser carried, kept: a missing value is an error rather
/// than a silently-defaulted knob, a zero tick would spin the resolver flat out against the
/// venue's directory API, and `--exit-on-silence` with detection off is an exit that could
/// never fire.
#[test]
fn the_record_flags_reject_what_they_always_rejected() {
    assert!(parse_args(argv(&["--record"])).is_err(), "--record needs a path");
    assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs"])).is_err());
    assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs", "0"])).is_err());
    assert!(parse_args(argv(&["--record", "r.toml", "--tick-secs", "x"])).is_err());
    let err = parse_args(argv(&["--record", "r.toml", "--silent-secs", "0", "--exit-on-silence"]))
        .expect_err("--exit-on-silence with detection off must be a usage error");
    assert!(err.contains("--silent-secs"), "{err}");
}

/// ⚠ The two spellings of each default are one fact, and this is what holds them equal.
///
/// The parser above is FEATURE-FREE (a build that cannot record still describes its own command
/// line), so it cannot name `crate::recorder`'s constants; this test is compiled only in a
/// `record` build, which is exactly where both spellings exist. A drift would make
/// `--help` promise one cadence while the mount ran another.
/// **The two profile SOURCES are parsed, and naming both is a refusal.**
///
/// ⚠ The refusal is the assertion that matters. A precedence chain here — "the row wins, else
/// the file" — would make "where is my recorder profile" a question with two answers, which is
/// the defect `vike_secrets::store::Backend` was shaped to avoid one layer down, and it would
/// turn a typo'd profile NAME into a silent file read.
#[test]
fn the_two_profile_sources_are_named_never_chained() {
    let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--recorder-profile", "default"])) else {
        panic!("`--recorder-profile NAME` is a run that records");
    };
    assert_eq!(req.profile, ProfileSource::Row("default".to_string()));

    let err = parse_args(argv(&["--record", "r.toml", "--recorder-profile", "default"]))
        .expect_err("naming both sources must be refused, not resolved by precedence");
    assert!(err.contains("--recorder-profile"), "{err}");
    assert!(err.contains("ambiguous"), "the message says WHY it is ambiguous: {err}");
    // …and in the other order, so the refusal is not an artefact of which came first.
    assert!(
        parse_args(argv(&["--recorder-profile", "default", "--record", "r.toml"])).is_err(),
        "order must not decide an ambiguity"
    );
    // ⚠ …and the VALUELESS form is a source too, so it collides with `--record` identically.
    // Without this the optional value would have opened a hole in the one rule this test is
    // about: `--record r.toml --recorder-profile` would parse as a file AND an active row.
    assert!(
        parse_args(argv(&["--record", "r.toml", "--recorder-profile"])).is_err(),
        "the valueless flag is still a SOURCE, so naming it beside --record is ambiguous"
    );
    // A companion flag still needs A profile, and the message names BOTH ways to give one.
    let err = parse_args(argv(&["--once"])).expect_err("a companion alone is refused");
    assert!(err.contains("--recorder-profile"), "{err}");
}

/// **The VALUELESS flag means the ACTIVE row, and the next token decides whether there is a
/// name — not the flag.**
///
/// ⚠ The `--recorder-profile --once` case is the one that would bite silently. A parser that
/// took the next token unconditionally would record a profile named `--once`, refuse it at the
/// store read, and report a profile name the operator never typed. It must instead see a flag,
/// leave it alone, and resolve the active row.
#[test]
fn the_profile_flag_takes_an_optional_value() {
    let Ok(Parsed::Run(Some(bare))) = parse_args(argv(&["--recorder-profile"])) else {
        panic!("a bare `--recorder-profile` is a run that records the ACTIVE row");
    };
    assert_eq!(bare.profile, ProfileSource::ActiveRow);

    let Ok(Parsed::Run(Some(then_flag))) = parse_args(argv(&["--recorder-profile", "--once"]))
    else {
        panic!("a following FLAG is not a profile name");
    };
    assert_eq!(then_flag.profile, ProfileSource::ActiveRow);
    assert!(then_flag.once, "…and the flag it did not eat still took effect");

    // A name still wins when one is actually there, in either position.
    let Ok(Parsed::Run(Some(named))) = parse_args(argv(&["--recorder-profile", "prod", "--once"]))
    else {
        panic!("`--recorder-profile NAME` still names a row");
    };
    assert_eq!(named.profile, ProfileSource::Row("prod".to_string()));
    assert!(named.once);
}

/// **The RETIRED spelling is refused BY NAME, and the message is the edit instruction.**
///
/// ⚠ It stands on an `ExecStart=` line on two deployed boxes, so the refusal an operator meets
/// has to name the new flag. A plain `unknown argument: --record-profile` would be true and
/// useless — they would have to come and read this file to learn a rename happened.
///
/// ⚠ And it must be a REFUSAL rather than an alias: a second accepted spelling is a second name
/// that rots, which is this repository's own no-alias rule.
#[test]
fn the_retired_profile_flag_still_starts_this_release_and_says_it_will_not_next() {
    // ⚠ The MIGRATION WINDOW, and the assertion pair is the whole point: it must STILL START
    // (a refusal deadlocks the deploy — see `RETIRED_PROFILE_FLAG`) and it must NOT be quiet.
    let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--record-profile", "default"])) else {
        panic!("the retired spelling must still start a daemon for one release");
    };
    assert_eq!(req.profile, ProfileSource::Row("default".into()), "…naming the same row");
    let w = req.retired_flag_warning.expect("…and it must never be accepted in SILENCE");
    assert!(w.contains("--record-profile"), "the warning names what was given: {w}");
    assert!(w.contains("--recorder-profile"), "…and what to write instead: {w}");
    assert!(w.contains("RENAMED"), "…that this is a rename, not a flag that never was: {w}");
    assert!(
        w.contains("next release") || w.contains("NEXT RELEASE"),
        "…and that the window CLOSES, which is what makes it a migration and not an alias: {w}"
    );

    // The value stays OPTIONAL through the retired spelling too: an operator mid-edit must not
    // meet a different rule depending on which half of the rename they have reached.
    let Ok(Parsed::Run(Some(bare))) = parse_args(argv(&["--record-profile"])) else {
        panic!("the retired spelling with no value must behave like the new one");
    };
    assert_eq!(bare.profile, ProfileSource::ActiveRow);
    assert!(bare.retired_flag_warning.is_some(), "…and warn just the same");

    // ANTI-VACUITY: the NEW spelling must carry no warning at all, or the assertions above
    // would pass on a build that warned unconditionally.
    let Ok(Parsed::Run(Some(new))) = parse_args(argv(&["--recorder-profile", "default"])) else {
        panic!("the new spelling parses");
    };
    assert!(new.retired_flag_warning.is_none(), "the new spelling is not deprecated");
}

#[cfg(feature = "record")]
#[test]
fn the_parser_defaults_match_the_recorders() {
    assert_eq!(DEFAULT_TICK_SECS, crate::recorder::DEFAULT_TICK_SECS);
    assert_eq!(DEFAULT_SILENT_SECS, crate::recorder::DEFAULT_SILENT_SECS);
}

/// ⚠ The mode is SAID, in both directions, and the serve-only line names what it is NOT doing.
///
/// The failure this guards is asymmetric and that is why the wording is: a unit that forgot
/// `--record` produces a daemon with no symptom whatsoever — active, healthy, answering — while
/// recording nothing, so "SERVE-ONLY" has to be as loud in the journal as "RECORDING" is. The
/// profile PATH is in the recording line because a box may run two of these.
#[test]
fn the_mode_line_says_which_mode() {
    let serve_only = startup_mode_line(None);
    assert!(serve_only.contains("SERVE-ONLY"), "{serve_only}");
    assert!(
        serve_only.contains("--record"),
        "the serve-only line must name the flag that would change it: {serve_only}"
    );

    // ⚠ `--record PATH` is RETIRED (0086): the path is IGNORED as a source, so the mode line
    // reports the ACTIVE row here too — identically to a bare `--recorder-profile` below — never
    // the path the operator typed, which this daemon never opens.
    let Ok(Parsed::Run(Some(req))) = parse_args(argv(&["--record", "r.toml"])) else {
        panic!("`--record PATH` is a run that records");
    };
    let recording = startup_mode_line(Some(&req));
    assert!(recording.contains("RECORDING"), "{recording}");
    assert!(
        recording.contains("ACTIVE"),
        "the path is ignored, so this reports the same active-row source a bare \
             `--recorder-profile` does: {recording}"
    );
    assert!(
        !recording.contains("r.toml"),
        "the path must never appear as if it had been read: {recording}"
    );
    assert!(
        !recording.contains("SERVE-ONLY"),
        "the two modes must not be confusable in a journal grep: {recording}"
    );

    // …and the ROW source names itself AS a row. A journal line saying only `default` would
    // leave an operator unable to tell which store, or whether a file was read at all — the
    // ambiguity `vike-cli secrets list`'s `source:` line exists to answer one layer down.
    let Ok(Parsed::Run(Some(row))) = parse_args(argv(&["--recorder-profile", "prod"])) else {
        panic!("`--recorder-profile NAME` is a run that records");
    };
    let recording = startup_mode_line(Some(&row));
    assert!(recording.contains("RECORDING"), "{recording}");
    assert!(recording.contains("prod"), "names the profile: {recording}");
    assert!(recording.contains("row"), "…and says it came from a row: {recording}");

    // …and the ACTIVE row says it is the active row rather than naming a profile. At this
    // point in the startup sequence no store has been read, so a name here would be a guess —
    // and a journal line that guessed would be worse than one that says how the choice is
    // made.
    let Ok(Parsed::Run(Some(active))) = parse_args(argv(&["--recorder-profile"])) else {
        panic!("a bare `--recorder-profile` is a run that records");
    };
    let recording = startup_mode_line(Some(&active));
    assert!(recording.contains("RECORDING"), "{recording}");
    assert!(recording.contains("ACTIVE"), "it says HOW the row was chosen: {recording}");
    assert!(
        !recording.contains("SERVE-ONLY"),
        "the two modes must not be confusable in a journal grep: {recording}"
    );
}

/// An unrecognised flag is REJECTED rather than ignored. This binary used to parse no argv at
/// all, so a typo in a systemd `ExecStart=` line started the server as if nothing were wrong.
#[test]
fn an_unknown_argument_is_rejected_rather_than_ignored() {
    let err = parse_args(argv(&["--adr=1.2.3.4:9"])).expect_err("a typo must not be ignored");
    assert!(err.contains("--adr"), "the error names the offending argument: {err}");
    // `-v` is not a version request — lowercase is verbosity everywhere else on the box.
    assert!(parse_args(argv(&["-v"])).is_err(), "-v must stay unknown");
}

// --- Polymarket's egress credentials (decision 0095's review) ------------------------------------
//
// The bridge's `declare_from_rows` is tested where it lives, over a credential map it is handed.
// What only THIS root can get wrong is building that map: this boot loads none, so it does a scoped
// read of five names, and a scope that came out empty or too wide would either strand a box's
// egress on the default proxy (the failure the fallback exists to prevent) or hand a venue key
// this data server has no business holding on to the egress code.

/// **The datahub's credential read for Polymarket's egress RETURNS the five names and NOTHING else.**
/// A file-store box (`secrets.env` answers; no settings database) is the shape that needs no
/// fixture writer, and it is the arm where the narrowing is a filter over a whole parsed file rather
/// than a bound query — so it is the arm that proves the scope. The venue key is composed, not
/// spelled: a whole env-shaped name in any `src/` file is read by
/// `crates/vike-ops/tests/settings_registry.rs` as this crate reading that variable.
#[cfg(all(
    feature = "serve-datafusion",
    any(feature = "venue-polymarket", feature = "catalog-serve")
))]
#[test]
fn the_egress_credential_read_holds_the_five_names_and_no_venue_key() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let names = vike_polymarket::egress_legacy_names();
    assert!(!names.is_empty(), "an empty scope would read nothing and so prove nothing");

    let venue_key = concat!("BINANCE", "_LIVE_API_KEY");
    let mut body: String = names.iter().map(|n| format!("{n}=value-of-{n}\n")).collect();
    body.push_str(&format!("{venue_key}=must-never-be-materialised\n"));
    let file = dir.path().join("secrets.env");
    std::fs::write(&file, body).expect("plant the store");
    #[cfg(unix)]
    {
        // 0600, so the permission finding (which this read logs) is not what the test exercises.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    let got = polymarket_egress_credentials(dir.path()).expect("a readable store");
    let mut held: Vec<&str> = got.keys().map(String::as_str).collect();
    held.sort_unstable();
    let mut want: Vec<&str> = names.iter().map(String::as_str).collect();
    want.sort_unstable();
    assert_eq!(held, want, "exactly the five declared names, all of them");
    assert!(!got.contains_key(venue_key), "a venue key must never be materialised here");
    assert_eq!(
        got.get(&names[0]).map(String::as_str),
        Some(format!("value-of-{}", names[0]).as_str()),
        "the value comes through untouched"
    );
}

/// An ABSENT store is the ordinary unconfigured box — no credentials, not an error — while a store
/// that EXISTS and cannot be read is `None` (and logged): the two must never look the same. A
/// DIRECTORY where the file belongs is the portable stand-in for an unreadable file (a `chmod 000`
/// proves nothing as root, which CI is).
#[cfg(all(
    feature = "serve-datafusion",
    any(feature = "venue-polymarket", feature = "catalog-serve")
))]
#[test]
fn an_absent_store_holds_no_credentials_and_an_unreadable_one_is_none() {
    let dir = tempfile::tempdir().expect("a temp dir");
    assert_eq!(
        polymarket_egress_credentials(dir.path()),
        Some(std::collections::HashMap::new()),
        "no store is the unconfigured box, not a fault"
    );
    std::fs::create_dir(dir.path().join("secrets.env")).expect("a directory where the file goes");
    assert_eq!(
        polymarket_egress_credentials(dir.path()),
        None,
        "a store that exists and will not read is NOT 'no credentials'"
    );
}
