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

// --- A venue setting filed as a credential row (decision 0095, Task 7) ----------------------------
//
// This boot loads no credential map, so the stranded-setting refusal every other root gets from
// `vike_boot::boot` runs here over a scoped read of the declared legacy names. What only THIS root
// can get wrong is that read: a scope that came out empty would refuse nothing, and one too wide
// would hand this data server a venue key it has no business holding.

/// **A stranded venue setting refuses this server's start BY NAME, and the read holds no venue key.**
/// A file-store box (`secrets.env` answers; no settings database) is the shape that needs no
/// fixture writer, and it is the arm where the narrowing is a filter over a whole parsed file rather
/// than a bound query — so it is the arm that proves the scope. Every name is composed, not
/// spelled: a whole env-shaped name in any `src/` file is read by
/// `crates/vike-ops/tests/settings_registry.rs` as this crate reading that variable.
#[cfg(feature = "serve-datafusion")]
#[test]
fn the_stranded_setting_read_refuses_by_name_and_holds_no_venue_key() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let proxy_host = concat!("POLY", "_PROXY_HOST");
    let venue_key = concat!("BINANCE", "_LIVE_API_KEY");
    let file = dir.path().join("secrets.env");
    std::fs::write(
        &file,
        format!("{proxy_host}=stranded-value-marker\n{venue_key}=must-never-be-named\n"),
    )
    .expect("plant the store");
    #[cfg(unix)]
    {
        // 0600, so the permission finding (which this read logs) is not what the test exercises.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    let why = refuse_stranded_venue_settings_in(dir.path()).expect_err("a stranded row refuses");
    assert!(why.contains(proxy_host) && why.contains("venue.polymarket.proxy_host"), "{why}");
    assert!(why.contains("vike-cli secrets move-venue-config"), "{why}");
    assert!(!why.contains("stranded-value-marker"), "a value leaked: {why}");
    assert!(!why.contains(venue_key), "a credential is not a venue setting: {why}");
}

/// A store holding credentials and no venue setting refuses nothing; an ABSENT store is the ordinary
/// unconfigured box; and a store that EXISTS and cannot be read refuses nothing either — it is
/// logged, and the rows' own read reports the same store. A DIRECTORY where the file belongs is the
/// portable stand-in for an unreadable file (a `chmod 000` proves nothing as root, which CI is).
#[cfg(feature = "serve-datafusion")]
#[test]
fn only_a_stranded_setting_refuses_the_start() {
    let dir = tempfile::tempdir().expect("a temp dir");
    assert_eq!(refuse_stranded_venue_settings_in(dir.path()), Ok(()), "no store");
    let file = dir.path().join("secrets.env");
    std::fs::write(&file, format!("{}=k\n", concat!("BINANCE", "_LIVE_API_KEY"))).expect("store");
    assert_eq!(refuse_stranded_venue_settings_in(dir.path()), Ok(()), "a credential only");
    std::fs::remove_file(&file).expect("remove the store");
    std::fs::create_dir(&file).expect("a directory where the file goes");
    assert_eq!(refuse_stranded_venue_settings_in(dir.path()), Ok(()), "an unreadable store");
}

// --- The OANDA history lane's ONE credential (docs/decisions/0097) --------------------------------
//
// What only THIS root can get wrong: the scope of the read, the three answers it gives, the startup
// line, and that nothing it builds holds the token. Every credential NAME is composed from the
// bridge's own declaration and every VALUE is an obviously fake, distinctive string — so "no value
// reached a log line" is an assertion that could actually fail. No name is spelled as a whole
// literal here: a `src/` file doing that is read by `crates/vike-ops/tests/settings_registry.rs` as
// this crate reading the variable.
#[cfg(feature = "backfill-serve")]
mod oanda_history {
    use std::fmt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber, span};
    use vike_bridge_core::credentials::Environment;
    use vike_data::source::{KlineSource, SourceError};
    use vike_model::Bar;
    use vike_oanda::{HistoryTokenError, HistoryTokenProvider};

    use super::super::{
        oanda_history_credentials, oanda_history_lane_line, oanda_history_presence,
        oanda_history_presence_probe, oanda_history_token_reader, read_oanda_history_token,
    };
    use vike_datahub_client::history::CredentialPresence;

    /// Every event's level and message text, in order — the hand-rolled capture
    /// `crates/vike-bridge-core/tests/shadowed_store_is_logged.rs` argues for, over `tracing`'s own
    /// `Subscriber` trait (this crate takes no `tracing-subscriber`).
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    struct Message<'a>(&'a mut String);

    impl Visit for Message<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            // Every field, not only `message`: an `error = %e` field is where a store error's text
            // lands, and a value there would be as much a leak as one in the message.
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }

    impl Subscriber for Capture {
        /// `sometimes`, so `tracing`'s per-callsite Interest cache cannot answer for this capture
        /// from an earlier test's subscriber — the ordering trap the precedent's own doc names.
        fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
            Interest::sometimes()
        }
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut text = String::new();
            event.record(&mut Message(&mut text));
            self.0
                .lock()
                .expect("capture lock")
                .push(format!("{}{text}", event.metadata().level()));
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    fn under_capture<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
        let capture = Capture::default();
        let out = tracing::subscriber::with_default(capture.clone(), f);
        let lines = capture.0.lock().expect("capture lock").clone();
        (out, lines)
    }

    /// The practice key — the ONE name the lane declares.
    fn practice_key() -> String {
        let names = vike_oanda::oanda_history_token_names();
        assert_eq!(names.len(), 1, "the lane declares ONE name: {names:?}");
        names[0].clone()
    }

    const PRACTICE_VALUE: &str = "fake-practice-token-7q4z";

    /// A store's worth of NEIGHBOURS the one-name scope must leave behind: the practice account's
    /// id, the live tier's pair in both spellings, the sim tier, a LABELLED account's practice key,
    /// and another venue's key. `(name, value)`, every value distinctive.
    fn neighbours() -> Vec<(String, String)> {
        let (_, practice_account) = vike_oanda::oanda_env_var_names(Environment::Demo);
        let (live_key, live_account) = vike_oanda::oanda_env_var_names(Environment::Live);
        let (sim_key, _) = vike_oanda::oanda_env_var_names(Environment::Sim);
        let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
        vec![
            (practice_account, "fake-account-id-2k9d".to_string()),
            (live_key, "fake-live-token-8m3x".to_string()),
            (live_account, "fake-live-account-5j1c".to_string()),
            (
                concat!("OANDA", "_MAINNET_API_KEY").to_string(),
                "fake-mainnet-token-6h2v".to_string(),
            ),
            (sim_key, "fake-sim-token-3w7n".to_string()),
            (
                vike_model::account_keys::account_key(&practice_key(), &alt),
                "fake-labelled-token-9p5r".to_string(),
            ),
            (concat!("BINANCE", "_LIVE_API_KEY").to_string(), "fake-binance-key-4t8y".to_string()),
        ]
    }

    /// Every planted value, the practice token included — what no log line may carry.
    fn every_value() -> Vec<String> {
        let mut values: Vec<String> = neighbours().into_iter().map(|(_, v)| v).collect();
        values.push(PRACTICE_VALUE.to_string());
        values
    }

    /// Write a FILE store (`secrets.env`) holding `rows`, 0600 on unix so the permission finding is
    /// only what a test asks for.
    fn write_file_store(settings: &std::path::Path, rows: &[(String, String)]) {
        let body: String = rows.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
        let file = settings.join("secrets.env");
        std::fs::write(&file, body).expect("plant the store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
    }

    /// The practice key plus every neighbour.
    fn full_store_rows() -> Vec<(String, String)> {
        let mut rows = neighbours();
        rows.push((practice_key(), PRACTICE_VALUE.to_string()));
        rows
    }

    /// **The lane's credential read RETURNS the one practice key and NOTHING else — over BOTH
    /// stores.** The FILE arm is a filter over a whole parsed file and the DATABASE arm a bound
    /// query, so each can fail in its own way; this plants the key among seven neighbours and reads
    /// through each. A read that went back to the whole store, or a scope that grew the account id
    /// or a live name, leaves a neighbour in the map.
    #[test]
    fn the_oanda_history_read_holds_the_one_practice_key_and_nothing_else() {
        let dir = tempfile::tempdir().expect("a temp dir");
        write_file_store(dir.path(), &full_store_rows());
        let want = vec![practice_key()];

        let from_file = oanda_history_credentials(dir.path(), false).expect("a readable file");
        let mut held: Vec<String> = from_file.keys().cloned().collect();
        held.sort_unstable();
        assert_eq!(held, want, "the FILE arm: exactly the one declared name");
        assert_eq!(from_file.get(&want[0]).map(String::as_str), Some(PRACTICE_VALUE));

        // The same rows, migrated into the settings DATABASE — which then answers WHOLLY.
        vike_secrets::migrate(
            Some(dir.path().to_str().expect("a utf-8 temp path")),
            |_| false,
            &vike_bridge_core::credentials::classify_credential_name,
        )
        .expect("the fixture migrates");
        assert!(
            dir.path().join("db").join("vike.db").is_file(),
            "the fixture must really be a database store, or the second half proves nothing"
        );
        let from_db = oanda_history_credentials(dir.path(), false).expect("a readable database");
        let mut held: Vec<String> = from_db.keys().cloned().collect();
        held.sort_unstable();
        assert_eq!(held, want, "the DATABASE arm: exactly the one declared name");
        assert_eq!(from_db.get(&want[0]).map(String::as_str), Some(PRACTICE_VALUE));
    }

    /// **Three states, three answers, and no value in any line logged on the way.** No settings
    /// directory and a store without the key are ABSENT; a store that exists and will not read is
    /// UNREADABLE (a directory where the file belongs — `chmod 000` proves nothing as root, which CI
    /// is) and says so in an `error!`; a present key is the value. The permission finding is logged
    /// by the announcing (startup) read and by no other.
    #[test]
    fn the_oanda_history_token_answers_three_states_and_no_line_carries_a_value() {
        let absent = tempfile::tempdir().expect("a temp dir");
        let without = tempfile::tempdir().expect("a temp dir");
        write_file_store(without.path(), &neighbours());
        let with = tempfile::tempdir().expect("a temp dir");
        write_file_store(with.path(), &full_store_rows());
        let unreadable = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir(unreadable.path().join("secrets.env"))
            .expect("a dir where a file goes");

        let (answers, lines) = under_capture(|| {
            [
                read_oanda_history_token(None, true),
                read_oanda_history_token(Some(absent.path()), true),
                read_oanda_history_token(Some(without.path()), true),
                read_oanda_history_token(Some(with.path()), false),
                read_oanda_history_token(Some(unreadable.path()), false),
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
        write_file_store(dir.path(), &full_store_rows());
        std::fs::set_permissions(
            dir.path().join("secrets.env"),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("chmod");

        let (_, silent) = under_capture(|| read_oanda_history_token(Some(dir.path()), false));
        assert!(silent.is_empty(), "a per-fetch read logged: {silent:?}");
        let (_, announced) = under_capture(|| read_oanda_history_token(Some(dir.path()), true));
        assert!(
            announced.iter().any(|l| l.starts_with("WARN")),
            "the startup read must report a store others can read: {announced:?}"
        );
        for value in every_value() {
            assert!(!announced.iter().any(|l| l.contains(&value)), "{announced:?}");
        }
    }

    /// The startup line names the KEY and what to do, and — taking a `Result<(), _>` — can carry no
    /// value by construction. Four states, four texts.
    #[test]
    fn the_startup_line_names_the_key_and_what_to_do() {
        let key = practice_key();
        let dir = std::path::Path::new("settings");
        let armed = oanda_history_lane_line(Some(dir), Ok(()));
        assert!(armed.contains("ARMED") && armed.contains(&key), "{armed}");
        assert!(armed.contains("no Observe verb"), "{armed}");
        let absent = oanda_history_lane_line(Some(dir), Err(HistoryTokenError::NotConfigured));
        assert!(absent.contains(&format!("`vike-cli secrets set {key}`")), "{absent}");
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
    /// file store, counted, behind the body production runs
    /// (`crate::backfill::credentialed_klines_row`) over a fake source: a two-day request reads the
    /// store once and hands both days the value; the next request reads again (two requests, two
    /// reads), so a key ROTATED between them is what it is handed; a key REMOVED makes the next
    /// request refuse with the teaching text. No line logged on the way carries any value.
    #[test]
    fn the_store_is_read_once_per_request_and_a_changed_key_is_what_the_next_one_sees() {
        let settings = tempfile::tempdir().expect("a temp dir");
        write_file_store(settings.path(), &full_store_rows());
        let hist = tempfile::tempdir().expect("a temp dir");
        let store = Arc::new(vike_data::DataFusionHist::open(hist.path()).expect("a store"));
        let root = oanda_history_token_reader(Some(settings.path().to_path_buf()));
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
            let mut rows = neighbours();
            rows.push((practice_key(), rotated.to_string()));
            write_file_store(settings.path(), &rows);
            let second = collect("EUR_USD", "5s", DAY0 + 2 * DAY, DAY0 + 2 * DAY + HOUR, &|| false);
            assert_eq!(reads.load(Ordering::SeqCst), 2, "two requests, two reads");
            assert!(nothing_held(), "the second request's provider outlived it");
            write_file_store(settings.path(), &neighbours());
            let third = collect("EUR_USD", "5s", DAY0 + 3 * DAY, DAY0 + 3 * DAY + HOUR, &|| false);
            (first, second, third)
        });
        assert_eq!(first, Ok(2), "two day chunks, one bar each");
        assert_eq!(second, Ok(1), "a key rotated between requests needs no restart");
        let third = third.expect_err("a removed key disarms the lane for the next request");
        assert!(third.contains(&format!("vike-cli secrets set {}", practice_key())), "{third}");
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

    /// **The history-channels read's presence word — every state its own answer, over BOTH stores,
    /// and no value in any line logged on the way** (the design's §2.4; 0097's verdict 6). No
    /// settings directory, a store without the key and a store holding it BLANK are `Absent` — the
    /// answers the lane's own token read gives — a stored key is `Present`, and a store that exists
    /// and will not read is `Unreadable`, logged once and never folded into `Absent`. The same
    /// fixtures migrated into the settings DATABASE answer the same, through the names-only query.
    #[test]
    fn the_presence_probe_answers_every_state_and_no_line_carries_a_value() {
        let absent = tempfile::tempdir().expect("a temp dir");
        let without = tempfile::tempdir().expect("a temp dir");
        write_file_store(without.path(), &neighbours());
        let blank = tempfile::tempdir().expect("a temp dir");
        let mut blank_rows = neighbours();
        blank_rows.push((practice_key(), "   ".to_string()));
        write_file_store(blank.path(), &blank_rows);
        let with = tempfile::tempdir().expect("a temp dir");
        write_file_store(with.path(), &full_store_rows());
        let unreadable = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir(unreadable.path().join("secrets.env"))
            .expect("a dir where a file goes");

        let (answers, lines) = under_capture(|| {
            [
                oanda_history_presence(None),
                oanda_history_presence(Some(absent.path())),
                oanda_history_presence(Some(without.path())),
                oanda_history_presence(Some(blank.path())),
                oanda_history_presence(Some(with.path())),
                oanda_history_presence(Some(unreadable.path())),
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
            let read = read_oanda_history_token(Some(dir), false);
            assert_eq!(
                read.is_ok(),
                oanda_history_presence(Some(dir)) == CredentialPresence::Present,
                "the presence word and the lane's read disagree about {}",
                dir.display()
            );
        }
        let errors: Vec<&String> = lines.iter().filter(|l| l.starts_with("ERROR")).collect();
        assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {lines:?}");
        assert!(errors[0].contains("NOT the same as the key being absent"), "{}", errors[0]);

        // The DATABASE arm: the same stores, migrated — which then answer WHOLLY.
        for (dir, want) in
            [(&with, CredentialPresence::Present), (&without, CredentialPresence::Absent)]
        {
            vike_secrets::migrate(
                Some(dir.path().to_str().expect("a utf-8 temp path")),
                |_| false,
                &vike_bridge_core::credentials::classify_credential_name,
            )
            .expect("the fixture migrates");
            assert!(dir.path().join("db").join("vike.db").is_file(), "guard: a database store");
            let (got, db_lines) = under_capture(|| oanda_history_presence(Some(dir.path())));
            assert_eq!(got, want, "the database arm answers as the file arm did");
            for value in every_value() {
                assert!(!db_lines.iter().any(|l| l.contains(&value)), "{db_lines:?}");
            }
        }
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
        use crate::backfill::{BackfillFn, BackfillLane, BackfillTable};
        use vike_datahub_client::proto::Response;

        let settings = tempfile::tempdir().expect("a temp dir");
        write_file_store(settings.path(), &full_store_rows());
        let store: Arc<dyn vike_data::HistStore + Send + Sync> =
            Arc::new(vike_data::MemHistStore::new());
        let panics: BackfillFn =
            Box::new(|_: &str, _: &str, _: i64, _: i64, _: &dyn Fn() -> bool| {
                panic!("the history-channels read called a collector")
            });
        let table = BackfillTable::new(Vec::new())
            .with("oanda", BackfillLane::CredentialedKlines, panics)
            .with_credential_probe(
                "oanda",
                oanda_history_presence_probe(Some(settings.path().to_path_buf())),
            );
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

        write_file_store(settings.path(), &neighbours());
        let second = crate::history::history_channels_verb(&store, Some(&table), 1_790_899_200_000);
        assert_eq!(
            oanda_word(&second),
            CredentialPresence::Absent,
            "the key was removed: the next answer says so, uncached"
        );
    }
}
