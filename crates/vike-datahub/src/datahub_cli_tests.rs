use super::*;
use vike_util::cli::{CliExit, USAGE_EXIT};

fn argv(v: &[&str]) -> std::vec::IntoIter<String> {
    v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
}

/// The `Launch` a command line asks for, or a panic naming what came back instead.
fn launch(v: &[&str]) -> Launch {
    read_args(argv(v)).unwrap_or_else(|exit| panic!("{v:?} must run, got {exit:?}"))
}

/// The recording a command line asks for.
fn recording(v: &[&str]) -> RecordRequest {
    launch(v).record.unwrap_or_else(|| panic!("{v:?} must record"))
}

/// A refused command line: a usage error, exit 2, on stderr. Returns the text.
fn refusal(v: &[&str]) -> String {
    match read_args(argv(v)) {
        Ok(l) => panic!("{v:?} must be refused, got {l:?}"),
        Err(exit) => {
            assert_eq!(exit.code, USAGE_EXIT, "{v:?}: a usage error exits 2: {exit:?}");
            assert!(!exit.to_stdout, "{v:?}: a usage error is stderr: {exit:?}");
            exit.text
        }
    }
}

/// A question the reader answers itself (help, version): exit 0, on stdout.
fn question(v: &[&str]) -> CliExit {
    match read_args(argv(v)) {
        Ok(l) => panic!("{v:?} is a question, not a run: {l:?}"),
        Err(exit) => {
            assert_eq!((exit.code, exit.to_stdout), (0, true), "{v:?}: {exit:?}");
            exit
        }
    }
}

/// The three outcomes, as PARSE results. The exit status and the stream each produces are
/// asserted over the shipped binary in `tests/help_cli.rs` — only a real run shows those.
#[test]
fn help_and_version_are_outcomes_not_errors_and_a_bare_invocation_runs() {
    for flag in ["-h", "--help"] {
        let exit = question(&[flag]);
        assert!(exit.text.contains("Usage:"), "{flag}: {}", exit.text);
        assert!(exit.text.contains("--recorder-profile"), "the page lists the flags: {flag}");
        assert!(
            exit.text.contains("config.datahub_bind_addr"),
            "…and the settings rows the daemon reads: {flag}"
        );
    }
    for flag in ["-V", "--version"] {
        let exit = question(&[flag]);
        assert!(exit.text.starts_with("vike-datahub "), "name and version lead: {}", exit.text);
    }
    assert_eq!(
        launch(&[]),
        Launch { store: None, record: None },
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
    let req = recording(&["--record", "r.toml"]);
    assert_eq!(req.profile, ProfileSource::ActiveRow, "the path is IGNORED as a source");
    let w = req.retired_flag_warning.expect("`--record` must warn, never accept in silence");
    assert!(w.contains("--record r.toml"), "the warning names what was given: {w}");
    assert!(w.contains("RETIRED"), "…and that the flag itself is retiring: {w}");
    assert!(w.contains("--recorder-profile"), "…and what to write instead: {w}");
    // The REQUEST holds what the command line said, and nothing it did not: the defaults (and the
    // box's rows) are folded in by `to_record_args`, held below.
    assert_eq!(req.tick_secs, None);
    assert_eq!(req.silent_secs, None);
    assert!(!req.once);
    assert!(
        !req.exit_on_silence,
        "the default reaction to a silent series is ALERT, never exit — and since the merge an \
             exit takes the data wire down with the feeds"
    );

    let req = recording(&[
        "--record",
        "r.toml",
        "--tick-secs",
        "5",
        "--silent-secs",
        "60",
        "--exit-on-silence",
        "--once",
    ]);
    assert_eq!(req.tick_secs, Some(5));
    assert_eq!(req.silent_secs, Some(60));
    assert!(req.exit_on_silence);
    assert!(req.once);
    // The `=` spelling of every valued flag reads too.
    let req = recording(&["--recorder-profile=prod", "--tick-secs=7", "--silent-secs=9"]);
    assert_eq!(req.profile, ProfileSource::Row("prod".to_string()));
    assert_eq!((req.tick_secs, req.silent_secs), (Some(7), Some(9)));
}

/// ⚠ A companion flag WITHOUT `--record` is refused rather than ignored, and the message names
/// the flag that needs the profile. Ignoring them would start a plain server for `--once` (a
/// dry run that never ends) and leave `--silent-secs 0` looking like a watchdog somebody turned
/// off — the "believes something is armed that is not" class the recorder's own parser already
/// refused within `--record`.
#[test]
fn a_recording_flag_without_a_profile_is_refused() {
    for flag in ["--once", "--exit-on-silence"] {
        let err = refusal(&[flag]);
        assert!(err.contains(flag), "the message names the offending flag: {err}");
        assert!(err.contains("--record"), "…and what it needs: {err}");
    }
    let err = refusal(&["--tick-secs", "5"]);
    assert!(err.contains("--tick-secs") && err.contains("--record"), "{err}");
    let err = refusal(&["--silent-secs", "0"]);
    assert!(err.contains("--silent-secs") && err.contains("--record"), "{err}");
}

/// The flag/value rules the recorder's parser carried, kept: a missing value is an error rather
/// than a silently-defaulted knob, a zero tick would spin the resolver flat out against the
/// venue's directory API, and `--exit-on-silence` with detection off is an exit that could
/// never fire. A malformed number is an error too (it is never read as a default).
#[test]
fn the_record_flags_reject_what_they_always_rejected() {
    refusal(&["--record"]);
    refusal(&["--record", "r.toml", "--tick-secs"]);
    refusal(&["--record", "r.toml", "--tick-secs", "x"]);
    refusal(&["--record", "r.toml", "--silent-secs", "x"]);
    refusal(&["--record", "r.toml", "--tick-secs", "-5"]);
    let err = refusal(&["--record", "r.toml", "--tick-secs", "0"]);
    assert!(err.contains("--tick-secs must be > 0"), "{err}");
    let err = refusal(&["--record", "r.toml", "--silent-secs", "0", "--exit-on-silence"]);
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
    let req = recording(&["--recorder-profile", "default"]);
    assert_eq!(req.profile, ProfileSource::Row("default".to_string()));

    let err = refusal(&["--record", "r.toml", "--recorder-profile", "default"]);
    assert!(err.contains("--recorder-profile"), "{err}");
    assert!(err.contains("ambiguous"), "the message says WHY it is ambiguous: {err}");
    // …and in the other order, so the refusal is not an artefact of which came first.
    refusal(&["--recorder-profile", "default", "--record", "r.toml"]);
    // ⚠ …and the VALUELESS form is a source too, so it collides with `--record` identically.
    // Without this the optional value would have opened a hole in the one rule this test is
    // about: `--record r.toml --recorder-profile` would parse as a file AND an active row.
    refusal(&["--record", "r.toml", "--recorder-profile"]);
    // The retired spelling is a source as well.
    refusal(&["--record-profile", "a", "--recorder-profile", "b"]);
    // A companion flag still needs A profile, and the message names BOTH ways to give one.
    let err = refusal(&["--once"]);
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
    assert_eq!(recording(&["--recorder-profile"]).profile, ProfileSource::ActiveRow);

    let then_flag = recording(&["--recorder-profile", "--once"]);
    assert_eq!(then_flag.profile, ProfileSource::ActiveRow);
    assert!(then_flag.once, "…and the flag it did not eat still took effect");

    // A name still wins when one is actually there, in either position.
    let named = recording(&["--recorder-profile", "prod", "--once"]);
    assert_eq!(named.profile, ProfileSource::Row("prod".to_string()));
    assert!(named.once);
    assert_eq!(
        recording(&["--once", "--recorder-profile=prod"]).profile,
        ProfileSource::Row("prod".to_string())
    );
}

/// **The RETIRED spelling is accepted for this release, loudly, and the message is the edit
/// instruction.**
///
/// ⚠ It stands on an `ExecStart=` line on two deployed boxes, so the warning an operator meets
/// has to name the new flag. And it is a MIGRATION WINDOW rather than an alias: the warning says
/// the window closes.
#[test]
fn the_retired_profile_flag_still_starts_this_release_and_says_it_will_not_next() {
    // ⚠ The MIGRATION WINDOW, and the assertion pair is the whole point: it must STILL START
    // (a refusal deadlocks the deploy — see `RETIRED_PROFILE_FLAG`) and it must NOT be quiet.
    let req = recording(&["--record-profile", "default"]);
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
    let bare = recording(&["--record-profile"]);
    assert_eq!(bare.profile, ProfileSource::ActiveRow);
    assert!(bare.retired_flag_warning.is_some(), "…and warn just the same");

    // ANTI-VACUITY: the NEW spelling must carry no warning at all, or the assertions above
    // would pass on a build that warned unconditionally.
    let new = recording(&["--recorder-profile", "default"]);
    assert!(new.retired_flag_warning.is_none(), "the new spelling is not deprecated");
}

/// The flag spellings the deploy gate reads from this file's constants are the ones `clap` parses.
#[test]
fn the_flag_constants_are_the_declared_long_names() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    for flag in [RECORDER_PROFILE_FLAG, RETIRED_PROFILE_FLAG] {
        let long = flag.trim_start_matches("--");
        assert!(
            cmd.get_arguments().any(|a| a.get_long() == Some(long)),
            "{flag} is a constant here but no `clap` argument is declared under it"
        );
    }
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
    let req = recording(&["--record", "r.toml"]);
    let recording_line = startup_mode_line(Some(&req));
    assert!(recording_line.contains("RECORDING"), "{recording_line}");
    assert!(
        recording_line.contains("ACTIVE"),
        "the path is ignored, so this reports the same active-row source a bare \
             `--recorder-profile` does: {recording_line}"
    );
    assert!(
        !recording_line.contains("r.toml"),
        "the path must never appear as if it had been read: {recording_line}"
    );
    assert!(
        !recording_line.contains("SERVE-ONLY"),
        "the two modes must not be confusable in a journal grep: {recording_line}"
    );

    // …and the ROW source names itself AS a row. A journal line saying only `default` would
    // leave an operator unable to tell which store, or whether a file was read at all — the
    // ambiguity `vike-cli secrets list`'s `source:` line exists to answer one layer down.
    let row = recording(&["--recorder-profile", "prod"]);
    let recording_line = startup_mode_line(Some(&row));
    assert!(recording_line.contains("RECORDING"), "{recording_line}");
    assert!(recording_line.contains("prod"), "names the profile: {recording_line}");
    assert!(recording_line.contains("row"), "…and says it came from a row: {recording_line}");

    // …and the ACTIVE row says it is the active row rather than naming a profile. At this
    // point in the startup sequence no store has been read, so a name here would be a guess —
    // and a journal line that guessed would be worse than one that says how the choice is
    // made.
    let active = recording(&["--recorder-profile"]);
    let recording_line = startup_mode_line(Some(&active));
    assert!(recording_line.contains("RECORDING"), "{recording_line}");
    assert!(recording_line.contains("ACTIVE"), "it says HOW the row was chosen: {recording_line}");
    assert!(
        !recording_line.contains("SERVE-ONLY"),
        "the two modes must not be confusable in a journal grep: {recording_line}"
    );
}

/// An unrecognised flag is REJECTED rather than ignored. This binary used to parse no argv at
/// all, so a typo in a systemd `ExecStart=` line started the server as if nothing were wrong.
#[test]
fn an_unknown_argument_is_rejected_rather_than_ignored() {
    let err = refusal(&["--adr=1.2.3.4:9"]);
    assert!(err.contains("--adr"), "the error names the offending argument: {err}");
    assert!(!err.contains("1.2.3.4"), "…and only the flag, never its value: {err}");
    // `-v` is not a version request — lowercase is verbosity everywhere else on the box.
    refusal(&["-v"]);
    // The backtest verbs moved to `vike-backend backtesthub --addr`; this daemon refuses them by name.
    refusal(&["--addr", "127.0.0.1:7878"]);
    // A repeated flag is refused (it used to take the last value, silently).
    refusal(&["--store", "a", "--store", "b"]);
    refusal(&["--once", "--once"]);
}

/// `--store DIR` is a SERVE flag (no recording needed), and a bare `--store` is a usage error
/// rather than a silent fall-through to the `config.store_root` row.
#[test]
fn the_store_flag_needs_a_directory_and_serves_alone() {
    assert_eq!(
        launch(&["--store", "/x/hist"]),
        Launch { store: Some(std::path::PathBuf::from("/x/hist")), record: None }
    );
    assert_eq!(launch(&["--store=/x/hist"]).store, Some(std::path::PathBuf::from("/x/hist")));
    assert_eq!(launch(&[]).store, None);
    for bad in [&["--store"][..], &["--store", ""][..], &["--store", "--once"][..]] {
        let err = refusal(bad);
        assert!(err.contains("--store"), "{bad:?}: {err}");
    }
}

/// What a command line meant to the daemon, comparable: the `--store` it named (only meaningful in
/// a serving build, where it is read), and either the two questions that answer and exit 0 or the
/// run — serve-only (`record: None`) or recording `(source, tick, silent, exit_on_silence, once,
/// warned)`.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Help,
    Version,
    Run {
        store: Option<std::path::PathBuf>,
        record: Option<(ProfileSource, u64, u64, bool, bool, bool)>,
    },
}

/// The adapter from the parser to [`Seen`]; `None` is a REFUSED command line. This is the only part
/// of the pin that changes when the reader does.
fn seen(line: &[&str]) -> Option<Seen> {
    match read_args(argv(line)) {
        // `-h`/`-V` are answered by the reader (exit 0, stdout); only the help page has a `Usage:`.
        Err(exit) if exit.code == 0 && exit.to_stdout => {
            Some(if exit.text.contains("Usage:") { Seen::Help } else { Seen::Version })
        }
        Err(_) => None,
        Ok(Launch { store, record }) => {
            // `--store` is read by a serving build only.
            let store = if cfg!(feature = "datafusion-serve") { store } else { None };
            Some(Seen::Run {
                store,
                record: record.map(|r| {
                    (
                        r.profile,
                        r.tick_secs.unwrap_or(DEFAULT_TICK_SECS),
                        r.silent_secs.unwrap_or(DEFAULT_SILENT_SECS),
                        r.exit_on_silence,
                        r.once,
                        r.retired_flag_warning.is_some(),
                    )
                }),
            })
        }
    }
}

/// Every command line the data daemon ACCEPTS today, pinned as POSITIVE cases (decision 0155:
/// "everything an operator could WRITE and have accepted before stays accepted"). The case table
/// is the pin; only [`seen`] adapts when the reader changes. Each row names where the line is
/// written.
#[test]
fn every_accepted_command_line_stays_accepted() {
    let t = DEFAULT_TICK_SECS;
    let s = DEFAULT_SILENT_SECS;
    // `--store` is read only by a serving build, so the expectation is built the same way.
    let store = |p: &str| {
        if cfg!(feature = "datafusion-serve") { Some(std::path::PathBuf::from(p)) } else { None }
    };
    let row = |n: &str| ProfileSource::Row(n.to_string());
    let rec =
        |source, tick, silent, exit, once, warned| Some((source, tick, silent, exit, once, warned));
    let cases: Vec<(&str, Vec<&str>, Seen)> = vec![
        // deploy/vike-datahub.service: `datahub --store /srv/vike-<unit>/market_data/hist
        // --recorder-profile default`.
        (
            "the shipped unit",
            vec!["--store", "/srv/vike-<unit>/market_data/hist", "--recorder-profile", "default"],
            Seen::Run {
                store: store("/srv/vike-<unit>/market_data/hist"),
                record: rec(row("default"), t, s, false, false, false),
            },
        ),
        // docs/ops/backtesthub.md, docs/ops/datahub-the CI box.md: serve a store, record nothing.
        ("serve a store", vec!["--store", "DIR"], Seen::Run { store: store("DIR"), record: None }),
        ("serve with no flags", vec![], Seen::Run { store: None, record: None }),
        // docs/ops/recorder-deploy.md: the commissioning dry runs.
        (
            "a dry run of the active profile",
            vec!["--recorder-profile", "default", "--once"],
            Seen::Run { store: None, record: rec(row("default"), t, s, false, true, false) },
        ),
        (
            "a dry run into a store",
            vec!["--store", "T/hist", "--recorder-profile", "flip-dry-run", "--once"],
            Seen::Run {
                store: store("T/hist"),
                record: rec(row("flip-dry-run"), t, s, false, true, false),
            },
        ),
        (
            "a named probe profile",
            vec!["--recorder-profile", "mm-probe"],
            Seen::Run { store: None, record: rec(row("mm-probe"), t, s, false, false, false) },
        ),
        // The OPTIONAL value: bare means the ACTIVE row, and a following flag is not its name.
        (
            "a bare recorder profile",
            vec!["--recorder-profile"],
            Seen::Run {
                store: None,
                record: rec(ProfileSource::ActiveRow, t, s, false, false, false),
            },
        ),
        (
            "a bare recorder profile, then a flag",
            vec!["--recorder-profile", "--once"],
            Seen::Run {
                store: None,
                record: rec(ProfileSource::ActiveRow, t, s, false, true, false),
            },
        ),
        // The companion flags with a profile.
        (
            "every recording knob",
            vec![
                "--recorder-profile",
                "prod",
                "--tick-secs",
                "5",
                "--silent-secs",
                "60",
                "--exit-on-silence",
                "--once",
            ],
            Seen::Run { store: None, record: rec(row("prod"), 5, 60, true, true, false) },
        ),
        (
            "detection off",
            vec!["--recorder-profile", "prod", "--silent-secs", "0"],
            Seen::Run { store: None, record: rec(row("prod"), t, 0, false, false, false) },
        ),
        (
            "knobs before the profile",
            vec!["--once", "--tick-secs", "10", "--recorder-profile", "prod"],
            Seen::Run { store: None, record: rec(row("prod"), 10, s, false, true, false) },
        ),
        // The RETIRED spellings: accepted for this release, each behind a startup warning.
        (
            "retired --record PATH",
            vec!["--record", "r.toml"],
            Seen::Run {
                store: None,
                record: rec(ProfileSource::ActiveRow, t, s, false, false, true),
            },
        ),
        (
            "retired --record-profile NAME",
            vec!["--record-profile", "default"],
            Seen::Run { store: None, record: rec(row("default"), t, s, false, false, true) },
        ),
        (
            "retired --record-profile, bare",
            vec!["--record-profile"],
            Seen::Run {
                store: None,
                record: rec(ProfileSource::ActiveRow, t, s, false, false, true),
            },
        ),
        (
            "a store beside the retired path",
            vec!["--store", "D", "--record", "r.toml", "--tick-secs", "10"],
            Seen::Run {
                store: store("D"),
                record: rec(ProfileSource::ActiveRow, 10, s, false, false, true),
            },
        ),
        // The questions.
        ("-h", vec!["-h"], Seen::Help),
        ("--help", vec!["--help"], Seen::Help),
        ("-V", vec!["-V"], Seen::Version),
        ("--version", vec!["--version"], Seen::Version),
        ("help after a flag", vec!["--once", "--help"], Seen::Help),
        ("version after a value flag", vec!["--tick-secs", "5", "--version"], Seen::Version),
    ];
    for (what, line, want) in cases {
        assert_eq!(seen(&line), Some(want), "{what}: {line:?}");
    }
}

// --- The per-kind retention row (`config.recorder_retention_days_by_kind`) -----------------------

/// An absent row, and the empty string, are today's behaviour: no kind has a retention of its own, so
/// the profile's scalar decides every kind.
#[test]
fn an_absent_or_empty_retention_row_is_todays_behaviour() {
    assert!(retention_by_kind_of(None).unwrap().is_empty());
    assert!(retention_by_kind_of(Some("")).unwrap().is_empty());
}

/// The documented row becomes one entry per kind named, keyed by the store's own kind type.
#[test]
fn a_retention_row_becomes_a_map_of_store_kinds_to_days() {
    use vike_data::StoreKindId;
    let map = retention_by_kind_of(Some("book:14,depth:14,trade:90")).unwrap();
    assert_eq!(
        map.into_iter().collect::<Vec<_>>(),
        vec![(StoreKindId::Book, 14), (StoreKindId::Depth, 14), (StoreKindId::Trade, 90)]
    );
}

/// A refused row reaches the operator as a sentence naming the ROW and the broken rule, from the
/// grammar's own parser — including the execution kinds, refused by name.
#[test]
fn a_refused_retention_row_names_the_row_and_the_rule() {
    for (row, rule) in [
        ("trade:0", "older than NOW"),
        ("exec_fill:30", "execution history"),
        ("exec_order:30", "execution history"),
        ("exec_funding:30", "execution history"),
        ("bok:14", "not a store kind"),
        ("book:14,book:30", "named twice"),
        ("book:14, trade:90", "whitespace"),
    ] {
        let msg = retention_by_kind_of(Some(row)).unwrap_err();
        assert!(msg.starts_with("config.recorder_retention_days_by_kind is refused: "), "{msg}");
        assert!(msg.contains(rule), "{row}: {msg}");
    }
}

/// **The grammar's kind lists and the store's kind enum say the same thing.** `vike-config` cannot
/// name `StoreKindId`, so it spells the retainable words and the refused ones as data; this holds
/// them to the enum. A new kind that reaches `StoreKindId::ALL` without being put on one list
/// reddens here, and so does a kind that moves between the lists: the execution kinds are the
/// refused set, and nothing else is.
#[test]
fn the_retention_grammar_and_the_store_kinds_agree() {
    use vike_config::retention::{RETENTION_KINDS, RETENTION_REFUSED_KINDS};
    use vike_data::StoreKindId;

    for kind in StoreKindId::ALL {
        let word = kind.as_str();
        let retained = RETENTION_KINDS.contains(&word);
        let refused = RETENTION_REFUSED_KINDS.contains(&word);
        assert!(
            retained ^ refused,
            "{word}: on exactly one of the two lists (retained {retained})"
        );
        assert_eq!(refused, word.starts_with("exec_"), "{word}: the refused set is the exec kinds");

        let row = format!("{word}:5");
        match retention_by_kind_of(Some(&row)) {
            Ok(map) => {
                assert!(retained, "{word}: accepted but not on the retainable list");
                assert_eq!(map.into_iter().collect::<Vec<_>>(), vec![(kind, 5)], "{word}");
            }
            Err(msg) => {
                assert!(refused, "{word}: refused but not on the refused list: {msg}");
                assert!(msg.contains(word), "{word}: the refusal names the kind: {msg}");
            }
        }
    }
    assert_eq!(
        RETENTION_KINDS.len() + RETENTION_REFUSED_KINDS.len(),
        StoreKindId::ALL.len(),
        "every kind is on a list, and no list names a word the enum does not spell"
    );
    for word in RETENTION_KINDS.iter().chain(&RETENTION_REFUSED_KINDS) {
        assert!(word.parse::<StoreKindId>().is_ok(), "{word} is not a StoreKindId word");
    }
}

/// **The journal materializer's arming decision**: OFF unless `flags.journal_materializer` is on —
/// a `config.journal_dir` row alone arms nothing, because that row is the TRADE daemon's switch for
/// writing a journal — and on without a directory is a refusal whose line names both rows.
#[test]
fn the_journal_materializer_arms_only_on_its_flag_and_a_directory() {
    let dir = std::path::Path::new("/srv/journal");
    assert_eq!(materializer_arming(false, None), MaterializerArming::Off);
    assert_eq!(
        materializer_arming(false, Some(dir)),
        MaterializerArming::Off,
        "a journal directory alone arms nothing"
    );
    assert_eq!(materializer_arming(true, Some(dir)), MaterializerArming::Spawn(dir.to_path_buf()));
    assert_eq!(materializer_arming(true, None), MaterializerArming::RefusedNoDir);
    let line = materializer_refusal_line();
    assert!(line.contains("flags.journal_materializer"), "the refusal names the flag: {line}");
    assert!(line.contains("config.journal_dir"), "...and the missing row: {line}");
    assert!(line.contains("NOT started"), "...and says nothing runs: {line}");
}

/// **...and the phase acts on it**: flag off (a directory present) and flag on with no directory
/// both spawn nothing; flag on with a MISSING directory is refused by the write probe; flag on with
/// a writable directory spawns the thread, which a shutdown drains and joins.
#[test]
fn the_journal_materializer_phase_spawns_only_when_armed() {
    let store: std::sync::Arc<dyn vike_data::HistStore + Send + Sync> =
        std::sync::Arc::new(vike_data::MemHistStore::new());
    let journal = tempfile::tempdir().unwrap();
    let mut settings = vike_config::Settings::default();

    settings.config.journal_dir = Some(journal.path().to_path_buf());
    assert!(
        start_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag off: not spawned, whatever the directory"
    );

    settings.flags.journal_materializer = true;
    settings.config.journal_dir = None;
    assert!(
        start_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag on, no directory: refused, not spawned"
    );

    settings.config.journal_dir = Some(journal.path().join("not-created"));
    assert!(
        start_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag on, the directory missing: the write probe refuses, not spawned"
    );
    assert!(
        !journal.path().join("not-created").exists(),
        "...and the probe does not create the trade daemon's directory"
    );

    settings.config.journal_dir = Some(journal.path().to_path_buf());
    let handle = start_journal_materializer(&settings, std::sync::Arc::clone(&store))
        .expect("flag on and a writable directory: spawned");
    handle.shutdown();
}

/// **The chain recorder's arming decision**: OFF whenever `flags.record_chains` is off (compaction
/// or not — the lane is unchanged), ARMED only when it is on AND this process compacts its store,
/// and on without compaction a refusal whose line names the row, the missing prerequisite and both
/// fixes.
#[test]
fn the_chain_recorder_arms_only_on_its_flag_and_a_compacting_store() {
    assert_eq!(chain_recorder_arming(false, false), ChainRecorderArming::Off);
    assert_eq!(chain_recorder_arming(false, true), ChainRecorderArming::Off);
    assert_eq!(chain_recorder_arming(true, true), ChainRecorderArming::Armed);
    assert_eq!(chain_recorder_arming(true, false), ChainRecorderArming::RefusedNoCompaction);
    let line = chain_recorder_refusal_line();
    assert!(line.contains("flags.record_chains"), "the refusal names the flag: {line}");
    assert!(line.contains("[maintenance].interval_secs"), "...the missing prerequisite: {line}");
    assert!(line.contains("--recorder-profile"), "...the fix that supplies it: {line}");
    assert!(line.contains("config set flags.record_chains false"), "...and the other fix: {line}");
    assert!(line.contains("NOT started"), "...and says nothing records: {line}");
}

/// **...and the phase acts on it**: the recorder the lane is handed exists exactly in the ARMED
/// case, and the refusal is logged (once) exactly in the refused one. `options_chain_recorder`
/// builds nothing that touches the network, so this runs over the in-memory store.
#[cfg(feature = "options-serve")]
#[test]
fn the_chain_recorder_phase_builds_a_recorder_only_when_armed() {
    let store = || -> std::sync::Arc<dyn vike_data::HistStore + Send + Sync> {
        std::sync::Arc::new(vike_data::MemHistStore::new())
    };
    let refusals = |events: &[vike_log::capture::CapturedEvent]| {
        events.iter().filter(|e| e.message.contains("flags.record_chains")).count()
    };

    let cadence = vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS;

    let (rec, events) =
        vike_log::capture::captured(|| options_chain_recorder(true, false, store(), cadence));
    assert!(rec.is_none(), "flag on, no compaction: refused, no recorder");
    assert_eq!(refusals(&events), 1, "...and the refusal is logged once: {events:?}");
    assert!(events.iter().any(|e| e.level == tracing::Level::WARN), "...as a warning");

    let (rec, events) =
        vike_log::capture::captured(|| options_chain_recorder(true, true, store(), cadence));
    assert!(rec.is_some_and(|r| r.enabled()), "flag on and a compacting store: armed");
    assert_eq!(refusals(&events), 0, "...and nothing is refused: {events:?}");

    for compaction_runs in [false, true] {
        let (rec, events) = vike_log::capture::captured(|| {
            options_chain_recorder(false, compaction_runs, store(), cadence)
        });
        assert!(rec.is_none(), "flag off (compaction {compaction_runs}): no recorder, as before");
        assert!(events.is_empty(), "...and no line about it: {events:?}");
    }
}

/// **The `config.datahub_chain_cadence_secs` row reaches the recorder the lane is handed, in
/// milliseconds, and no row is the default cadence** — the box that wrote nothing records exactly
/// the one snapshot per expiry per minute it always did. (The conversion itself, and its range, are
/// pinned in `crates/vike-datahub/tests/chain_recording.rs`; this holds the hop through the mount's
/// own function.)
#[cfg(feature = "options-serve")]
#[test]
fn the_chain_cadence_row_reaches_the_recorder_the_mount_builds() {
    use crate::options::deribit::chain_cadence_ms;

    let store = || -> std::sync::Arc<dyn vike_data::HistStore + Send + Sync> {
        std::sync::Arc::new(vike_data::MemHistStore::new())
    };
    let built = |row: Option<u32>| {
        options_chain_recorder(true, true, store(), chain_cadence_ms(row))
            .expect("armed: a recorder is built")
            .cadence_ms()
    };
    assert_eq!(built(None), vike_data::rec::chain_rec::DEFAULT_CHAIN_CADENCE_MS);
    assert_eq!(built(Some(60)), 60_000, "writing the default is the default");
    assert_eq!(built(Some(300)), 300_000, "the row is seconds, the recorder milliseconds");
}

// -- the `config.recorder_*` rows, folded with the flags --------------------------------------------

/// A `Config` with the recorder rows a test names set, everything else the box's default.
#[cfg(feature = "record")]
fn config_of(set: impl FnOnce(&mut vike_config::Config)) -> vike_config::Config {
    let mut config = vike_config::Config::default();
    set(&mut config);
    config
}

/// **A box with no recorder row records exactly as it did before the rows existed**: the shipped
/// unit's line, a dry run, and every flag spelling fold to the arguments the parser alone used to
/// produce, the flush policy is left unset (so `RecorderConfig`'s own defaults apply), and nothing
/// new is said at start.
#[cfg(feature = "record")]
#[test]
fn a_box_without_the_recorder_rows_records_exactly_as_it_did_before_them() {
    use std::time::Duration;

    let config = vike_config::Config::default();
    let resolved =
        to_record_args(&recording(&["--recorder-profile", "default"]), &config).expect("folds");
    assert_eq!(
        resolved.args,
        crate::recorder::RecordArgs {
            profile: "profile row `default`".to_string(),
            tick: Duration::from_secs(30),
            once: false,
            silent_secs: 300,
            exit_on_silence: false,
            flush_max_rows: None,
            flush_max_age: None,
        },
        "the shipped unit's recording, byte for byte what the parser alone made of it"
    );
    assert_eq!(resolved.disclosure, None, "a box that wrote no row hears nothing new at start");
    assert!(resolved.warnings.is_empty(), "{:?}", resolved.warnings);

    // The flags fold as they did: they say what they say, whatever the (empty) rows are.
    let flags =
        recording(&["--recorder-profile=prod", "--tick-secs=5", "--silent-secs=0", "--once"]);
    let resolved = to_record_args(&flags, &config).expect("folds");
    assert_eq!(resolved.args.tick, Duration::from_secs(5));
    assert_eq!((resolved.args.silent_secs, resolved.args.once), (0, true));
    assert_eq!(resolved.disclosure, None);
    assert!(resolved.warnings.is_empty(), "a flag value is the operator's own, not a row's");
}

/// **A row fills what the command line leaves, the command line beats its row, and the flush rows
/// (which have no flag) reach the arguments** — and the startup line names exactly the knobs a row
/// supplied.
#[cfg(feature = "record")]
#[test]
fn a_row_fills_what_the_flags_leave_and_a_flag_beats_its_row() {
    use std::time::Duration;

    let config = config_of(|c| {
        c.recorder_tick_secs = Some(90);
        c.recorder_silent_secs = Some(600);
        c.recorder_flush_max_rows = Some(20_000);
        c.recorder_flush_max_age_secs = Some(60);
    });

    let by_row = to_record_args(&recording(&["--recorder-profile=prod"]), &config).expect("folds");
    assert_eq!(by_row.args.tick, Duration::from_secs(90));
    assert_eq!(by_row.args.silent_secs, 600);
    assert_eq!(by_row.args.flush_max_rows, Some(20_000));
    assert_eq!(by_row.args.flush_max_age, Some(Duration::from_secs(60)));
    let line = by_row.disclosure.expect("rows supplied values, so the start says so");
    for key in [
        "config.recorder_tick_secs",
        "config.recorder_silent_secs",
        "config.recorder_flush_max_rows",
        "config.recorder_flush_max_age_secs",
    ] {
        assert!(line.contains(key), "the line names {key}: {line}");
    }

    let flags = recording(&["--recorder-profile=prod", "--tick-secs=10", "--silent-secs=120"]);
    let by_flag = to_record_args(&flags, &config).expect("folds");
    assert_eq!(by_flag.args.tick, Duration::from_secs(10), "--tick-secs beats its row");
    assert_eq!(by_flag.args.silent_secs, 120, "--silent-secs beats its row");
    assert_eq!(by_flag.args.flush_max_rows, Some(20_000), "the flush rows have no flag to lose to");
    let line = by_flag.disclosure.expect("the flush rows still supplied values");
    assert!(
        !line.contains("recorder_tick_secs") && !line.contains("recorder_silent_secs"),
        "{line}"
    );
    assert!(line.contains("recorder_flush_max_rows"), "{line}");
}

/// The startup warnings `--recorder-profile=prod` earns on a box whose rows `set` writes.
#[cfg(feature = "record")]
fn warned(set: fn(&mut vike_config::Config)) -> Vec<String> {
    to_record_args(&recording(&["--recorder-profile=prod"]), &config_of(set))
        .expect("folds")
        .warnings
}

/// **Each value a row makes costly says so, once, and a value the flag or the default decided says
/// nothing**: a long tick, a threshold under the default, the watchdog off, a bigger flush window.
#[cfg(feature = "record")]
#[test]
fn a_row_that_weakens_a_detection_or_widens_the_loss_window_warns() {
    let one = |w: Vec<String>, needle: &str| {
        assert_eq!(w.len(), 1, "exactly one line for one costly row: {w:?}");
        assert!(w[0].contains(needle), "{needle}: {w:?}");
    };
    one(warned(|c| c.recorder_tick_secs = Some(120)), "config.recorder_tick_secs");
    one(warned(|c| c.recorder_silent_secs = Some(120)), "false alarms");
    one(warned(|c| c.recorder_silent_secs = Some(0)), "OFF");
    one(warned(|c| c.recorder_flush_max_rows = Some(6_000)), "config.recorder_flush_max_rows");
    one(warned(|c| c.recorder_flush_max_age_secs = Some(31)), "config.recorder_flush_max_age_secs");

    // The shipped defaults and the cheaper directions are legal rows that cost nothing to say.
    let quiet: [fn(&mut vike_config::Config); 6] = [
        |c| c.recorder_tick_secs = Some(60),
        |c| c.recorder_silent_secs = Some(300),
        |c| c.recorder_silent_secs = Some(900),
        |c| c.recorder_flush_max_rows = Some(5_000),
        |c| c.recorder_flush_max_rows = Some(500),
        |c| c.recorder_flush_max_age_secs = Some(5),
    ];
    for set in quiet {
        assert!(warned(set).is_empty(), "{:?}", warned(set));
    }
}

/// **`--exit-on-silence` with the silence row at `0` is refused before anything binds**, as it is
/// with `--silent-secs 0` — an exit that could never fire — and a `--silent-secs` above 0 beats
/// the row, so the pair is a legal command line there. Only a settings load can see this one, which
/// is why the parser (which reads none) cannot refuse it.
#[cfg(feature = "record")]
#[test]
fn exit_on_silence_with_the_silence_row_at_zero_is_refused() {
    let row_off = config_of(|c| c.recorder_silent_secs = Some(0));
    let err =
        to_record_args(&recording(&["--recorder-profile=prod", "--exit-on-silence"]), &row_off)
            .expect_err("an exit that could never fire must be refused");
    assert!(
        err.contains("--exit-on-silence") && err.contains("config.recorder_silent_secs"),
        "{err}"
    );

    let resolved = to_record_args(
        &recording(&["--recorder-profile=prod", "--exit-on-silence", "--silent-secs=60"]),
        &row_off,
    )
    .expect("the flag beats the row, so detection is on");
    assert_eq!(resolved.args.silent_secs, 60);
    assert!(resolved.args.exit_on_silence);

    // ...and with the row on (or unset) the exit is armed as it always was.
    let row_on = config_of(|c| c.recorder_silent_secs = Some(120));
    assert!(
        to_record_args(&recording(&["--recorder-profile=prod", "--exit-on-silence"]), &row_on)
            .is_ok()
    );
}
