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
    assert_eq!(req.tick_secs, DEFAULT_TICK_SECS);
    assert_eq!(req.silent_secs, DEFAULT_SILENT_SECS);
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
    assert_eq!(req.tick_secs, 5);
    assert_eq!(req.silent_secs, 60);
    assert!(req.exit_on_silence);
    assert!(req.once);
    // The `=` spelling of every valued flag reads too.
    let req = recording(&["--recorder-profile=prod", "--tick-secs=7", "--silent-secs=9"]);
    assert_eq!(req.profile, ProfileSource::Row("prod".to_string()));
    assert_eq!((req.tick_secs, req.silent_secs), (7, 9));
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
    // The compute verbs moved to `vike-backend backtest --addr`; this daemon refuses them by name.
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
                        r.tick_secs,
                        r.silent_secs,
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
        // docs/ops/backtest-daemon.md, docs/ops/datahub-the CI box.md: serve a store, record nothing.
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

// --- The OANDA history lane's ONE credential (docs/decisions/0097) --------------------------------
//
// What only THIS root can get wrong: the address of the read, the three answers it gives, the
// startup line, and that nothing it builds holds the token. The token is an ACCOUNT's credential
// (decision 0156 item 6), so every fixture plants ACCOUNT ROWS and writes their FIELDS — no
// credential name is composed or spelled here — and every VALUE is an obviously fake, distinctive
// string, so "no value reached a log line" is an assertion that could actually fail.
#[cfg(feature = "backfill-serve")]
mod oanda_history {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use vike_data::source::{KlineSource, SourceError};
    use vike_marketdata::Bar;
    use vike_model::AccountTier;
    use vike_oanda::{HistoryTokenError, HistoryTokenProvider};

    use super::super::{
        oanda_history_credentials, oanda_history_lane_line, oanda_history_presence,
        oanda_history_presence_probe, oanda_history_token_reader, read_oanda_history_token,
    };
    use vike_datahub_client::history::CredentialPresence;

    /// Every event's level, then every field — `message` first — as ` name=value`, one line per
    /// event: the shape the assertions below read (`starts_with("ERROR")`, and "no line carries a
    /// value", which needs EVERY field — an `error = %e` field is where a store error's text lands,
    /// and a value there would be as much a leak as one in the message). Projected from
    /// `vike_log::capture::captured`, the workspace's one scoped capture. ⚠ The hand-rolled
    /// subscriber this replaced answered `sometimes` from `register_callsite` on the theory that the
    /// per-callsite Interest cache could then not answer for it; while a capture is the only live
    /// dispatcher tracing-core never asks it, and the shared capture's module doc carries the
    /// mechanism and the anchor that does hold.
    fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
        let (out, events) = vike_log::capture::captured(f);
        let mut lines = Vec::with_capacity(events.len());
        for e in &events {
            let mut line = format!("{} message={}", e.level, e.message);
            for (k, v) in &e.fields {
                line.push_str(&format!(" {k}={v}"));
            }
            lines.push(line);
        }
        (out, lines)
    }

    /// The token's FIELD on the practice account.
    const TOKEN_FIELD: &str = vike_model::credential_keys::API_KEY;
    /// The oanda account's other field — the neighbour on the SAME account the read must leave.
    const ACCOUNT_ID_FIELD: &str = "ACCOUNT_ID";

    const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

    /// One planted account: its venue, tier and label, and the `(field, value)` pairs filed
    /// against it.
    type Planted = (&'static str, AccountTier, Option<&'static str>, Vec<(String, String)>);

    /// A store's worth of accounts around the practice account, with the practice account's token
    /// set to `token` (`None` = the account holds no token). The NEIGHBOURS the one-field read must
    /// leave behind: the practice account's own account id, the LIVE oanda account's pair, the
    /// PAPER oanda account's key, a LABELLED demo oanda account's key, and another venue's key.
    /// Every value distinctive.
    fn store_rows(token: Option<&str>) -> Vec<Planted> {
        let pair = |field: &str, value: &str| (field.to_string(), value.to_string());
        let mut practice = vec![pair(ACCOUNT_ID_FIELD, "fake-account-id-2k9d")];
        if let Some(token) = token {
            practice.push(pair(TOKEN_FIELD, token));
        }
        vec![
            ("oanda", AccountTier::Demo, None, practice),
            (
                "oanda",
                AccountTier::Live,
                None,
                vec![
                    pair(TOKEN_FIELD, "fake-live-token-8m3x"),
                    pair(ACCOUNT_ID_FIELD, "fake-live-account-5j1c"),
                ],
            ),
            ("oanda", AccountTier::Paper, None, vec![pair(TOKEN_FIELD, "fake-sim-token-3w7n")]),
            (
                "oanda",
                AccountTier::Demo,
                Some("ALT"),
                vec![pair(TOKEN_FIELD, "fake-labelled-token-9p5r")],
            ),
            ("binance", AccountTier::Live, None, vec![pair(TOKEN_FIELD, "fake-binance-key-4t8y")]),
        ]
    }

    /// Every neighbour, the practice account holding NO token.
    fn neighbours() -> Vec<Planted> {
        store_rows(None)
    }

    /// Every planted value, the practice token included — what no log line may carry.
    fn every_value() -> Vec<String> {
        let mut values: Vec<String> = store_rows(None)
            .into_iter()
            .flat_map(|(_, _, _, fields)| fields.into_iter().map(|(_, v)| v))
            .collect();
        values.push(PRACTICE_VALUE.to_string());
        values
    }

    /// Make the settings DATABASE — the only credential store — hold exactly `accounts`, 0600 on
    /// unix so the permission finding is only what a test asks for. The store is created the one way
    /// a store comes into being (`vike-cli secrets init`'s library half); each account is created
    /// with its tier FIRST and its fields are then written against it by owner and field — the one
    /// way an account's credential reaches a store. A store being REPLACED (a rotation, a removal)
    /// is rebuilt in this test's own temp directory: the writer adds and replaces fields and never
    /// removes one.
    fn write_store(settings: &std::path::Path, accounts: &[Planted]) {
        let db_dir = settings.join("db");
        if db_dir.exists() {
            std::fs::remove_dir_all(&db_dir).expect("drop this test's own previous store");
        }
        vike_secrets::create_store(Some(settings.to_str().expect("a utf-8 temp path")))
            .expect("create the empty store");
        for &(venue, tier, label, ref fields) in accounts {
            let created = vike_secrets::edit_account_in_unjournalled(
                settings,
                vike_secrets::AccountEdit::Create { venue, tier, label },
            )
            .expect("create the account");
            let id = created.after.expect("a create returns its row").id;
            if !fields.is_empty() {
                vike_secrets::save_owned_credentials_unjournalled(
                    settings,
                    &vike_secrets::CredentialOwner::Account(id),
                    fields,
                    vike_secrets::UpsertIntent::Add,
                )
                .expect("write the account's fields");
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                vike_secrets::db_path_in(settings),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("chmod");
        }
    }

    /// Bytes that are not a database where the database belongs: a store that EXISTS and will not
    /// open (`chmod 000` proves nothing as root, which CI is).
    fn plant_unreadable_store(settings: &std::path::Path) {
        std::fs::create_dir_all(settings.join("db")).expect("the db directory");
        std::fs::write(vike_secrets::db_path_in(settings), b"not a sqlite database at all")
            .expect("plant the garbage");
    }

    /// The handle the boot would hand this root for a settings directory `dir` (the full scope: no
    /// test here sets `VIKE_NO_LIVE_KEYS`).
    fn at(dir: &std::path::Path) -> vike_secrets::CredentialStore {
        vike_secrets::CredentialStore::open(
            Some(vike_secrets::StoreLocation::new(dir)),
            vike_secrets::CredentialScope::Full,
        )
    }

    /// The handle a boot that resolved NO settings directory hands this root: no location.
    fn nowhere() -> vike_secrets::CredentialStore {
        vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full)
    }

    /// The practice token plus every neighbour.
    fn full_store_rows() -> Vec<Planted> {
        store_rows(Some(PRACTICE_VALUE))
    }

    /// **The lane's credential read RETURNS the practice account's one field and NOTHING else —
    /// from the one store.** The settings DATABASE answers through a bound query on one account's
    /// one field; this plants the token among the practice account's own account id, its live,
    /// paper and labelled siblings and another venue's key, and reads it back. A read that went
    /// back to the whole store, or an address that drifted to another account or field, answers a
    /// neighbour's value.
    #[test]
    fn the_oanda_history_read_holds_the_one_practice_key_and_nothing_else() {
        let dir = tempfile::tempdir().expect("a temp dir");
        write_store(dir.path(), &full_store_rows());
        assert!(
            dir.path().join("db").join("vike.db").is_file(),
            "the fixture must really be a database store, or this half proves nothing"
        );
        let from_db =
            oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
        assert_eq!(
            from_db.as_ref().map(|token| token.expose().as_str()),
            Some(PRACTICE_VALUE),
            "the DATABASE arm: exactly the practice account's token"
        );
        // …and with the practice account holding NO token, no neighbour answers in its place.
        write_store(dir.path(), &neighbours());
        let without =
            oanda_history_credentials(&at(dir.path()), false).expect("a readable database");
        assert!(without.is_none(), "a neighbour answered for the practice token");
    }

    /// **Three states, three answers, and no value in any line logged on the way.** No settings
    /// directory, no store and a store without the key are ABSENT; a store that exists and will not read is
    /// UNREADABLE (bytes that are not a database where the database belongs) and says so in an
    /// `error!`; a present key is the value. The permission finding is logged by the announcing
    /// (startup) read and by no other.
    #[test]
    fn the_oanda_history_token_answers_three_states_and_no_line_carries_a_value() {
        let absent = tempfile::tempdir().expect("a temp dir");
        let without = tempfile::tempdir().expect("a temp dir");
        write_store(without.path(), &neighbours());
        let with = tempfile::tempdir().expect("a temp dir");
        write_store(with.path(), &full_store_rows());
        let unreadable = tempfile::tempdir().expect("a temp dir");
        plant_unreadable_store(unreadable.path());

        let (answers, lines) = under_capture(|| {
            [
                read_oanda_history_token(&nowhere(), true),
                read_oanda_history_token(&at(absent.path()), true),
                read_oanda_history_token(&at(without.path()), true),
                read_oanda_history_token(&at(with.path()), false),
                read_oanda_history_token(&at(unreadable.path()), false),
            ]
        });
        assert_eq!(
            answers,
            [
                Err(HistoryTokenError::NotConfigured),
                Err(HistoryTokenError::NotConfigured),
                Err(HistoryTokenError::NotConfigured),
                Ok(PRACTICE_VALUE.to_string()),
                Err(HistoryTokenError::StoreUnreadable),
            ]
        );
        let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
        assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
        assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);
        for value in every_value() {
            assert!(
                !lines.iter().any(|l| l.contains(&value)),
                "a value reached the log: {lines:?}"
            );
        }
    }

    /// The finding a group- or world-readable store earns is logged by the ANNOUNCING read and by
    /// no other — the provider's own reads run once per day chunk and must stay silent.
    #[cfg(unix)]
    #[test]
    fn only_the_announcing_read_logs_the_stores_permission_finding() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a temp dir");
        write_store(dir.path(), &full_store_rows());
        std::fs::set_permissions(
            vike_secrets::db_path_in(dir.path()),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");

        let (_, silent) = under_capture(|| read_oanda_history_token(&at(dir.path()), false));
        assert!(silent.is_empty(), "a per-fetch read logged: {silent:?}");
        let (_, announced) = under_capture(|| read_oanda_history_token(&at(dir.path()), true));
        assert!(
            announced.iter().any(|l| l.starts_with("WARN")),
            "the startup read must report a store others can read: {announced:?}"
        );
        for value in every_value() {
            assert!(!announced.iter().any(|l| l.contains(&value)), "{announced:?}");
        }
    }

    /// The startup line names the token by its ACCOUNT and FIELD and says what to do, and — taking
    /// a `Result<(), _>` — can carry no value by construction. Four states, four texts.
    #[test]
    fn the_startup_line_names_the_key_and_what_to_do() {
        let key =
            format!("the {TOKEN_FIELD} of the ACTIVE unlabelled oanda account at tier `demo`");
        let dir = std::path::Path::new("settings");
        let armed = oanda_history_lane_line(Some(dir), Ok(()));
        assert!(armed.contains("ARMED") && armed.contains(&key), "{armed}");
        assert!(armed.contains("no Observe verb"), "{armed}");
        let absent = oanda_history_lane_line(Some(dir), Err(HistoryTokenError::NotConfigured));
        assert!(absent.contains(&key), "{absent}");
        assert!(
            absent.contains(&format!("`vike-cli secrets set --account <ID> {TOKEN_FIELD}`")),
            "the fix writes the field onto the account: {absent}"
        );
        assert!(
            absent.contains("`vike-cli secrets account add --venue oanda --tier demo --no-label`"),
            "…and names how the account is created: {absent}"
        );
        assert!(absent.contains("nothing to restart"), "{absent}");
        let unknown = oanda_history_lane_line(Some(dir), Err(HistoryTokenError::StoreUnreadable));
        assert!(unknown.contains("UNKNOWN") && unknown.contains("not the same"), "{unknown}");
        let no_dir = oanda_history_lane_line(None, Err(HistoryTokenError::NotConfigured));
        assert!(no_dir.contains("no settings directory"), "{no_dir}");
    }

    /// The provider type behind [`HistoryTokenProvider`]'s `Arc`, for a `Weak` to it.
    type ProviderFn = dyn Fn() -> Result<String, HistoryTokenError> + Send + Sync;

    /// A source that asks its provider on every fetch, the way `vike_oanda::OandaKlines` does, and
    /// records what it was handed — so the test can see the VALUE flow and see it change — plus a
    /// `Weak` to the provider it fetched through, so the test can see that provider (and the memo
    /// holding the value inside it) GONE once the request returns.
    struct TokenEcho {
        token: HistoryTokenProvider,
        seen: Arc<Mutex<Vec<String>>>,
        providers: Arc<Mutex<Vec<std::sync::Weak<ProviderFn>>>>,
    }

    impl KlineSource for TokenEcho {
        fn venue(&self) -> &str {
            "oanda"
        }
        fn fetch(
            &self,
            _symbol: &str,
            _interval: &str,
            start_ms: i64,
            _end_ms: i64,
        ) -> Result<Vec<Bar>, SourceError> {
            self.providers.lock().expect("providers lock").push(Arc::downgrade(&self.token));
            let token = (self.token)().map_err(|e| SourceError::Refused(e.to_string()))?;
            self.seen.lock().expect("seen lock").push(token);
            Ok(vec![Bar {
                ts: start_ms,
                open: 1.25,
                high: 1.5,
                low: 1.0,
                close: 1.375,
                volume: 7.0,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            }])
        }
    }

    /// Tuesday 2024-01-02T00:00:00Z — long settled, so every chunk below is fetched.
    const DAY0: i64 = 1_704_153_600_000;
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;

    /// **The store is read ONCE per request however many days it fetches, and nothing holds the
    /// token between requests** — proved by what a real store does to the lane, and by the
    /// per-request provider being GONE once its request returns. The root's own reader over a real
    /// settings database, counted, behind the body production runs
    /// (`crate::backfill::credentialed_klines_row`) over a fake source: a two-day request reads the
    /// store once and hands both days the value; the next request reads again (two requests, two
    /// reads), so a key ROTATED between them is what it is handed; a key REMOVED makes the next
    /// request refuse with the teaching text. No line logged on the way carries any value.
    #[test]
    fn the_store_is_read_once_per_request_and_a_changed_key_is_what_the_next_one_sees() {
        let settings = tempfile::tempdir().expect("a temp dir");
        write_store(settings.path(), &full_store_rows());
        let hist = tempfile::tempdir().expect("a temp dir");
        let store = Arc::new(vike_store::DataFusionHist::open(hist.path()).expect("a store"));
        let root = oanda_history_token_reader(at(settings.path()));
        let reads = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&reads);
        let counted: HistoryTokenProvider = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            root()
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let providers = Arc::new(Mutex::new(Vec::new()));
        let (seen_by_source, providers_by_source) = (Arc::clone(&seen), Arc::clone(&providers));
        let (venue, collect) = crate::backfill::credentialed_klines_row(
            Arc::clone(&store),
            counted,
            move |token| TokenEcho {
                token,
                seen: Arc::clone(&seen_by_source),
                providers: Arc::clone(&providers_by_source),
            },
            |_, _, _, _| None,
        );
        assert_eq!(venue, "oanda");
        assert_eq!(reads.load(Ordering::SeqCst), 0, "building the row reads nothing");
        // Every provider a request fetched through must be dropped — memo, token and all — by the
        // time that request has returned. A `Weak` that still upgrades is a value held on.
        let nothing_held = || {
            let providers = providers.lock().expect("providers lock");
            assert!(!providers.is_empty(), "guard: the source fetched through a provider");
            providers.iter().all(|p| p.upgrade().is_none())
        };

        let rotated = "fake-rotated-token-1b6k";
        let ((first, second, third), lines) = under_capture(|| {
            let first = collect("EUR_USD", "5s", DAY0 + HOUR, DAY0 + DAY + HOUR, &|| false);
            assert_eq!(reads.load(Ordering::SeqCst), 1, "two days, ONE read of the store");
            assert!(nothing_held(), "the first request's provider outlived it");
            write_store(settings.path(), &store_rows(Some(rotated)));
            let second = collect("EUR_USD", "5s", DAY0 + 2 * DAY, DAY0 + 2 * DAY + HOUR, &|| false);
            assert_eq!(reads.load(Ordering::SeqCst), 2, "two requests, two reads");
            assert!(nothing_held(), "the second request's provider outlived it");
            write_store(settings.path(), &neighbours());
            let third = collect("EUR_USD", "5s", DAY0 + 3 * DAY, DAY0 + 3 * DAY + HOUR, &|| false);
            (first, second, third)
        });
        assert_eq!(first, Ok(2), "two day chunks, one bar each");
        assert_eq!(second, Ok(1), "a key rotated between requests needs no restart");
        let third = third.expect_err("a removed key disarms the lane for the next request");
        assert!(third.contains(&HistoryTokenError::NotConfigured.to_string()), "{third}");
        assert_eq!(reads.load(Ordering::SeqCst), 3, "one read per request, three requests");
        assert_eq!(
            *seen.lock().expect("seen lock"),
            [PRACTICE_VALUE, PRACTICE_VALUE, rotated],
            "each request's days share its one read, and the next request reads afresh"
        );
        let mut values = every_value();
        values.push(rotated.to_string());
        for value in values {
            assert!(
                !lines.iter().any(|l| l.contains(&value)),
                "a value reached the log: {lines:?}"
            );
        }
    }

    /// **The history-channels read's presence word — every state its own answer, and no value in
    /// any line logged on the way** (the design's §2.4; 0097's verdict 6). No settings directory, no
    /// store, a store without the key and a store holding it BLANK are `Absent` — the answers the lane's own token
    /// read gives — a stored key is `Present`, and a store that exists and will not read is
    /// `Unreadable`, logged once and never folded into `Absent`. The store is the settings
    /// DATABASE, answered through the names-only query.
    #[test]
    fn the_presence_probe_answers_every_state_and_no_line_carries_a_value() {
        let absent = tempfile::tempdir().expect("a temp dir");
        let without = tempfile::tempdir().expect("a temp dir");
        write_store(without.path(), &neighbours());
        let blank = tempfile::tempdir().expect("a temp dir");
        write_store(blank.path(), &store_rows(Some("   ")));
        let with = tempfile::tempdir().expect("a temp dir");
        write_store(with.path(), &full_store_rows());
        let unreadable = tempfile::tempdir().expect("a temp dir");
        plant_unreadable_store(unreadable.path());

        let (answers, lines) = under_capture(|| {
            [
                oanda_history_presence(&nowhere()),
                oanda_history_presence(&at(absent.path())),
                oanda_history_presence(&at(without.path())),
                oanda_history_presence(&at(blank.path())),
                oanda_history_presence(&at(with.path())),
                oanda_history_presence(&at(unreadable.path())),
            ]
        });
        assert_eq!(
            answers,
            [
                CredentialPresence::Absent,
                CredentialPresence::Absent,
                CredentialPresence::Absent,
                CredentialPresence::Absent,
                CredentialPresence::Present,
                CredentialPresence::Unreadable,
            ]
        );
        // The word agrees with the lane's own read on every one of those stores.
        for dir in [absent.path(), without.path(), blank.path(), with.path()] {
            let read = read_oanda_history_token(&at(dir), false);
            assert_eq!(
                read.is_ok(),
                oanda_history_presence(&at(dir)) == CredentialPresence::Present,
                "the presence word and the lane's read disagree about {}",
                dir.display()
            );
        }
        let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
        assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
        assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);

        for value in every_value() {
            assert!(
                !lines.iter().any(|l| l.contains(&value)),
                "a value reached the log: {lines:?}"
            );
        }
    }

    /// **The history-channels reply carries a WORD for OANDA's token and never the token** — not in
    /// its wire bytes, not in its `Debug`, not in a line logged while it was built — and it is asked
    /// afresh per request, so removing the key changes the next answer with no restart. The table's
    /// collector PANICS: the read must answer from a lookup and the probe, never by running a lane.
    #[test]
    fn the_history_channels_reply_carries_a_word_and_never_the_token() {
        use crate::backfill::{BackfillFn, BackfillTable};
        use vike_catalog::HistoryLane;
        use vike_datahub_client::proto::Response;

        let settings = tempfile::tempdir().expect("a temp dir");
        write_store(settings.path(), &full_store_rows());
        let store: Arc<dyn vike_data::HistStore + Send + Sync> =
            Arc::new(vike_data::MemHistStore::new());
        let panics: BackfillFn =
            Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
                panic!("the history-channels read called a collector")
            });
        let table = BackfillTable::new(Vec::new())
            .with("oanda", HistoryLane::CredentialedKlines, panics)
            .with_credential_probe("oanda", oanda_history_presence_probe(at(settings.path())));
        let oanda_word = |response: &Response| match response {
            Response::HistoryChannels(report) => {
                let row = &report.venue("oanda").expect("oanda is on the roster").channels[0];
                assert_eq!(row.mounted, Some(true), "the planted row is mounted");
                row.credential
            }
            other => panic!("expected HistoryChannels, got {other:?}"),
        };

        let (first, lines) = under_capture(|| {
            crate::history::history_channels_verb(&store, Some(&table), 1_790_899_200_000)
        });
        assert_eq!(oanda_word(&first), CredentialPresence::Present);
        let bytes = serde_json::to_string(&first).expect("the reply encodes");
        let debug = format!("{first:?}");
        for value in every_value() {
            assert!(!bytes.contains(&value), "a value reached the reply's bytes");
            assert!(!debug.contains(&value), "a value reached the reply's Debug");
            assert!(!lines.iter().any(|l| l.contains(&value)), "a value reached the log");
        }

        write_store(settings.path(), &neighbours());
        let second = crate::history::history_channels_verb(&store, Some(&table), 1_790_899_200_000);
        assert_eq!(
            oanda_word(&second),
            CredentialPresence::Absent,
            "the key was removed: the next answer says so, uncached"
        );
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
        mount_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag off: not spawned, whatever the directory"
    );

    settings.flags.journal_materializer = true;
    settings.config.journal_dir = None;
    assert!(
        mount_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag on, no directory: refused, not spawned"
    );

    settings.config.journal_dir = Some(journal.path().join("not-created"));
    assert!(
        mount_journal_materializer(&settings, std::sync::Arc::clone(&store)).is_none(),
        "flag on, the directory missing: the write probe refuses, not spawned"
    );
    assert!(
        !journal.path().join("not-created").exists(),
        "...and the probe does not create the trade daemon's directory"
    );

    settings.config.journal_dir = Some(journal.path().to_path_buf());
    let handle = mount_journal_materializer(&settings, std::sync::Arc::clone(&store))
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

    let (rec, events) =
        vike_log::capture::captured(|| options_chain_recorder(true, false, store()));
    assert!(rec.is_none(), "flag on, no compaction: refused, no recorder");
    assert_eq!(refusals(&events), 1, "...and the refusal is logged once: {events:?}");
    assert!(events.iter().any(|e| e.level == tracing::Level::WARN), "...as a warning");

    let (rec, events) = vike_log::capture::captured(|| options_chain_recorder(true, true, store()));
    assert!(rec.is_some_and(|r| r.enabled()), "flag on and a compacting store: armed");
    assert_eq!(refusals(&events), 0, "...and nothing is refused: {events:?}");

    for compaction_runs in [false, true] {
        let (rec, events) =
            vike_log::capture::captured(|| options_chain_recorder(false, compaction_runs, store()));
        assert!(rec.is_none(), "flag off (compaction {compaction_runs}): no recorder, as before");
        assert!(events.is_empty(), "...and no line about it: {events:?}");
    }
}
