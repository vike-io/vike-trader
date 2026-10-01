use super::*;

// -- the ONE store the merge created --------------------------------------------------------

/// The agreeing case, including the shape an operator's existing profile actually has: a
/// RELATIVE `store` beside an absolute server root. Before the merge these were two processes
/// and this comparison did not exist; the relative form is what
/// `crates/vike-recorder/recorder.example.toml` ships, so it is the form that must pass.
#[test]
fn a_profile_naming_the_servers_own_root_is_accepted() {
    let cwd = Path::new("/srv/vike-<unit>");
    one_store_root(
        Path::new("/srv/vike-<unit>/data/hist"),
        Path::new("/srv/vike-<unit>/data/hist"),
        Some(cwd),
    )
    .expect("two absolute spellings of one directory agree");
    one_store_root(Path::new("data/hist"), Path::new("/srv/vike-<unit>/data/hist"), Some(cwd))
        .expect("a relative profile path resolves against the daemon's own working directory");
}

/// ⚠ The failure the merge introduces and this refusal exists for: a recording landing in one
/// store while every query is answered from another. Both silent answers produce a lie, so the
/// message must name BOTH paths and BOTH knobs — a refusal that says only "mismatch" leaves an
/// operator guessing which of two files to edit.
#[test]
fn a_disagreeing_profile_is_refused_and_the_message_names_both_sides() {
    let err = one_store_root(
        Path::new("/var/lib/vike/market_data/hist"),
        Path::new("/srv/vike-<unit>/data/hist"),
        Some(Path::new("/srv/vike-<unit>")),
    )
    .expect_err("two different roots in one process must be refused, not silently resolved");
    assert!(err.contains("/var/lib/vike/market_data/hist"), "{err}");
    assert!(err.contains("/srv/vike-<unit>/data/hist"), "{err}");
    assert!(err.contains("VIKE_DATAHUB_STORE"), "the message must name the server's knob: {err}");
    assert!(err.contains("`store`"), "…and the profile's: {err}");
}

/// No working directory (a process with no project above it) must not make two DIFFERENT
/// relative spellings look equal by dropping the base — the comparison degrades to the literals,
/// which still separates them.
#[test]
fn a_missing_working_directory_does_not_collapse_two_relative_roots() {
    assert!(one_store_root(Path::new("data/hist"), Path::new("market_data/hist"), None).is_err());
    one_store_root(Path::new("data/hist"), Path::new("data/hist"), None)
        .expect("the same relative spelling is the same store either way");
}

// -- the PRE-BIND refusals -------------------------------------------------------------------
//
// ⚠ These test `load_and_check_profile` rather than the pieces, because the property under test
// is WHERE the refusal happens. `one_store_root` above was already correct and already tested;
// what was wrong is that `datahub_cli` reached it only after the listener was bound, so the
// whole daemon crash-looped instead of refusing to start. A test of the pure comparison cannot
// see that, and could not have caught it.

/// Write a profile into a temp dir and return `(dir, path)` — the dir must outlive the path.
fn profile_file(body: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("recorder.toml");
    std::fs::write(&path, body).expect("write profile");
    (dir, path)
}

/// The happy path, so every refusal below is a refusal of something specific rather than of
/// everything. The venue is one every `record-*` build compiles.
#[test]
fn a_profile_that_agrees_with_the_server_is_accepted_before_anything_binds() {
    let (dir, path) = profile_file(
        "store = \"tape/hist\"\n\
             [[subscribe]]\nvenue = \"polymarket\"\nfamily = \"btc-updown-5m\"\n",
    );
    let root = dir.path().join("tape").join("hist");
    let profile = load_and_check_profile(&path, &root, Some(dir.path()))
        .expect("an agreeing profile naming a supported venue is accepted");
    assert_eq!(profile.subscribe.len(), 1);
}

/// ⚠ THE BLOCKER: this refusal used to fire from inside `record`, i.e. after `TcpListener::bind`
/// and after the serve thread was spawned. The daemon therefore took the data wire UP, refused,
/// exited non-zero, and `Restart=on-failure` did it again every five seconds — a crash-loop
/// wearing a configuration error's message.
#[test]
fn a_store_root_disagreement_is_refused_by_the_pre_bind_check() {
    let (dir, path) = profile_file(
        "store = \"tape/hist\"\n[[subscribe]]\nvenue = \"polymarket\"\nfamily = \"btc-updown-5m\"\n",
    );
    let err = load_and_check_profile(&path, Path::new("/somewhere/else/hist"), Some(dir.path()))
        .expect_err("two roots in one process must be refused before the port is bound");
    assert!(err.contains("VIKE_DATAHUB_STORE"), "{err}");
}

/// A profile that names nothing to record would bind a port, serve every query and accumulate
/// nothing — the silent no-op class this whole daemon's design objects to. It was a startup
/// error already (`rt.feed_count() == 0`); what moved is that it is answered before the bind.
#[test]
fn a_profile_with_no_subscriptions_is_refused_before_anything_binds() {
    let (dir, path) = profile_file("store = \"tape/hist\"\n");
    let root = dir.path().join("tape").join("hist");
    let err = load_and_check_profile(&path, &root, Some(dir.path()))
        .expect_err("nothing to record must not start a daemon that looks healthy");
    assert!(err.contains("[[subscribe]]"), "the message names the missing table: {err}");
}

/// A venue with no feed in THIS build is the same refusal `crate::recording::build_recording_feed`
/// gives, asked earlier.
/// `kalshi` is unsupported in every build, so this test says the same thing under every feature
/// combination rather than only under one.
#[test]
fn a_venue_this_build_cannot_record_is_refused_before_anything_binds() {
    let (dir, path) = profile_file(
        "store = \"tape/hist\"\n[[subscribe]]\nvenue = \"kalshi\"\nsymbols = [\"X\"]\n",
    );
    let root = dir.path().join("tape").join("hist");
    let err = load_and_check_profile(&path, &root, Some(dir.path()))
        .expect_err("a venue with no feed compiled in must refuse, never record nothing");
    assert!(err.contains("kalshi"), "{err}");
    assert!(err.contains("Supported here"), "…and say what this build CAN record: {err}");
}

/// A `--record` typo names a file that is not there, and the message must name the path: this
/// error is usually read seconds after typing it.
#[test]
fn an_unreadable_profile_names_the_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("nope.toml");
    let err = load_and_check_profile(&missing, dir.path(), Some(dir.path()))
        .expect_err("a profile that is not there must be refused");
    assert!(err.contains("nope.toml"), "{err}");
}

// -- the ROW half of the same four questions -------------------------------------------------

/// **A `--recorder-profile NAME` naming a profile the store does not hold is a REFUSAL, and the
/// message names the STORE.**
///
/// ⚠ This is the one question that differs between the two loaders, and it is a deliberate
/// NARROWING of what the store layer answers. `vike_secrets::profile_store::read_profiles`
/// collapses *no database*, *no profile tables* and *no rows* to `Profiles::none()`, which is
/// right for a SELECTION — every box that has not migrated must behave as it did — and wrong
/// here, because `--recorder-profile NAME` names one OUTRIGHT. A daemon that treated the empty
/// answer as "record nothing" would bind its port, serve every query and accumulate nothing:
/// the silent no-op this whole file exists to refuse, arriving through the store instead of
/// through a file.
#[test]
fn a_profile_name_the_store_does_not_hold_is_refused_before_anything_binds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let settings = dir.path().join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    let err = load_and_check_profile_row(
        "default",
        settings.to_str(),
        &dir.path().join("tape").join("hist"),
        Some(dir.path()),
    )
    .expect_err("a named profile that is not in the store must refuse, never record nothing");
    assert!(err.contains("default"), "the message names the profile: {err}");
    assert!(
        err.contains("vike.db"),
        "…and the STORE that answered, which is the half a journal reader cannot \
             reconstruct: {err}"
    );
    assert!(
        err.contains("vike-cli"),
        "…and what to run about it, because `--recorder-profile` has no file to go and look \
             at: {err}"
    );
}

/// An ABSENT settings directory is the same refusal, not a different one — a box with no
/// project resolved still named a profile explicitly, so "nothing selected it" is not an
/// available answer here either.
#[test]
fn a_missing_store_is_the_same_refusal_rather_than_an_empty_recording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = load_and_check_profile_row(
        "default",
        dir.path().join("no-such-settings").to_str(),
        dir.path(),
        Some(dir.path()),
    )
    .expect_err("an absent store must refuse a NAMED profile");
    assert!(err.contains("default"), "{err}");
}

// -- the ACTIVE row, which `--recorder-profile` with no value resolves to --------------------

/// **A box with no settings database refuses the VALUELESS flag, and the message is the one
/// that names `secrets migrate`** — not the same string the other two noes get.
///
/// This is the only one of the three reachable from here without a writer: `read_profiles` on a
/// store with no profile tables answers `Profiles::none()`, i.e. `NoProfileStore`. The other two
/// messages are asserted directly below, through the named functions that produce them, because
/// building their store state would need `store_profile` and this crate is not one of
/// `crates/vike-ops/tests/profile_writer_gate.rs`'s declared writers.
#[test]
fn a_box_with_no_settings_database_refuses_the_valueless_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = load_and_check_active_profile_row(
        dir.path().join("no-such-settings").to_str(),
        &dir.path().join("tape").join("hist"),
        Some(dir.path()),
    )
    .expect_err("no database means no ACTIVE profile, which must refuse rather than record");
    assert!(
        err.contains("vike.db"),
        "the STORE that answered leads, exactly as the NAMED half's refusal does: {err}"
    );
    assert!(
        err.contains("secrets migrate"),
        "…and the command that CREATES the database, which is the one thing that separates \
             this no from the other two: {err}"
    );
}

/// **The three noes are three DIFFERENT refusals** — the property `ActiveProfile`'s own doc
/// claims and the reason this file matches on it rather than on an `Option`. A single "no
/// active recorder profile" would send an operator to the wrong command in two of three cases.
///
/// Each message is also checked for the ONE token that makes it actionable, so a future edit
/// cannot converge them by dropping the part that differs.
#[test]
fn each_no_names_a_different_next_command() {
    let store = "/srv/vike-<unit>/settings/db/vike.db";
    let none_db = no_settings_database(store);
    let none_stored = no_recorder_profile_stored(store);
    let none_active = no_recorder_profile_selected(store, 3);

    assert_ne!(none_db, none_stored);
    assert_ne!(none_stored, none_active);
    assert_ne!(none_db, none_active);
    for m in [&none_db, &none_stored, &none_active] {
        assert!(m.starts_with(store), "every refusal leads with the store: {m}");
    }

    // No database -> the one command that may CREATE one.
    assert!(none_db.contains("secrets migrate"), "{none_db}");
    // Database, no recorder profile -> the command that WRITES a first body (0086: FROM
    // ARGUMENTS, never from a file — `config mirror --recorder` is retiring).
    assert!(none_stored.contains("config bootstrap-recorder"), "{none_stored}");
    assert!(
        !none_stored.contains("secrets migrate"),
        "a store that answered does not need migrating: {none_stored}"
    );
    // Profiles stored, none selected -> name one, or re-run the bootstrap verb to activate it.
    assert!(none_active.contains("3 recorder profile"), "the COUNT is named: {none_active}");
    assert!(none_active.contains("--recorder-profile <name>"), "{none_active}");
    assert!(
        none_active.contains("config bootstrap-recorder"),
        "⚠ the operator must be sent to a verb that actually exists and actually activates a \
             row: {none_active}"
    );
}

// -- the stop path ---------------------------------------------------------------------------

/// ⚠ **The regression this daemon cannot afford.** Under systemd stdin is `/dev/null`, which
/// reads EOF the instant the daemon starts. If EOF meant "stop", the recorder would exit at
/// startup on every box, every start — so a non-tty EOF must leave the flag DOWN and the daemon
/// recording headless.
#[test]
fn a_non_tty_eof_does_not_stop_the_recorder() {
    let stop = AtomicBool::new(false);
    control_loop(std::io::Cursor::new(b"" as &[u8]), false, &stop);
    assert!(
        !stop.load(std::sync::atomic::Ordering::SeqCst),
        "a non-tty EOF must NOT stop the daemon — systemd wires stdin to /dev/null, so this \
             would exit at startup on every service box"
    );
}

/// …and the mirror image, so the rule above is not bought by ignoring EOF entirely: on a TTY,
/// Ctrl-D IS the operator saying stop.
#[test]
fn a_tty_eof_stops_the_recorder() {
    let stop = AtomicBool::new(false);
    control_loop(std::io::Cursor::new(b"" as &[u8]), true, &stop);
    assert!(stop.load(std::sync::atomic::Ordering::SeqCst), "Ctrl-D on a TTY is a stop");
}

/// Every stop word raises the flag, TTY or not — the word is explicit, so the channel's
/// tty-ness has nothing to add.
#[test]
fn every_control_word_stops_on_either_channel() {
    for word in ["quit", "shutdown", "stop", "exit"] {
        for is_tty in [true, false] {
            let stop = AtomicBool::new(false);
            let input = format!("{word}\n");
            control_loop(std::io::Cursor::new(input.as_bytes()), is_tty, &stop);
            assert!(
                stop.load(std::sync::atomic::Ordering::SeqCst),
                "`{word}` must stop the recorder (is_tty={is_tty})"
            );
        }
    }
}

/// A typo must not stop a daemon that is recording tape, and a blank line must not either.
#[test]
fn an_unknown_word_does_not_stop_the_recorder() {
    let stop = AtomicBool::new(false);
    control_loop(std::io::Cursor::new(b"\n  \nhalt\n" as &[u8]), false, &stop);
    assert!(
        !stop.load(std::sync::atomic::Ordering::SeqCst),
        "an unrecognised word is reported, never obeyed"
    );
}

/// The unit's declared stop timeout, read rather than restated so the two cannot drift.
///
/// ⚠ This read the separate `-record` template until the 2026-09-16 unit collapse folded the
/// recording shape into `deploy/vike-datahub.service` itself. The condition it depends on is
/// unchanged and is worth restating at the read: a serve-only EDIT of that file installs no
/// signal handler (`arm` is conditional on `--record`), so it has no teardown for this budget
/// to bound, and this number would then be sizing a flush against a unit that never flushes.
/// `crates/vike-ops/tests/deploy_layout_gate.rs`'s
/// `a_units_store_grant_follows_its_recording_mode` is what refuses a half-made serve-only
/// edit; nothing refuses a complete one, which is why the unit's own stop block says at the
/// line that the number stops being derived when `--record` goes.
fn unit_stop_timeout_secs() -> u64 {
    let unit = include_str!("../../../deploy/vike-datahub.service");
    unit.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("TimeoutStopSec="))
        .expect("the shipped recording unit must set TimeoutStopSec= explicitly")
        .trim()
        .parse()
        .expect("TimeoutStopSec= is a plain number of seconds")
}

/// ⚠ **The WHOLE stop must fit inside the unit's `TimeoutStopSec=`** — every step SIGTERM starts,
/// not just the capped suffix.
///
/// This test replaces one that compared `SHUTDOWN_DEADLINE_SECS` alone against the unit and
/// whose failure message claimed that established "SIGKILL does not cut the final flush in
/// half". It did not: the feed unsubscribe runs FIRST and outside that cap, so the assertion
/// bounded a suffix while naming the total — the repo's single most-repeated defect shape, in
/// the gate written to prevent the consequence.
///
/// MUTATION PROOF: raise `FEED_STOP_BUDGET_SECS` past the unit's headroom (e.g. 12 → 25) and
/// this goes red; the deleted version stayed green through exactly that change, because the
/// number it read never moved.
#[test]
fn the_whole_stop_fits_inside_the_units_stop_timeout() {
    let timeout = unit_stop_timeout_secs();
    assert!(
        TOTAL_STOP_BUDGET_SECS < timeout,
        "the TOTAL stop budget ({TOTAL_STOP_BUDGET_SECS}s = {FEED_STOP_BUDGET_SECS}s \
             unsubscribing the feeds + {SHUTDOWN_DEADLINE_SECS}s flushing, stopping alert delivery \
             and joining compaction) \
             must be strictly under the unit's TimeoutStopSec={timeout}s. Only the second half is \
             hard-capped; the first is budgeted, measured and warned on (a non-Send VenueFeed cannot \
             move onto the orchestration thread), so the unit is what actually protects the flush \
             from SIGKILL."
    );
}

/// The arithmetic that makes the budgeted half count: what the feed stop actually took comes
/// OUT of the bounded half, so the total keeps aiming at the same number the unit was sized
/// against.
#[test]
fn the_feed_stop_overrun_is_taken_out_of_the_flush_budget() {
    let flush = Duration::from_secs(SHUTDOWN_DEADLINE_SECS);
    let total = Duration::from_secs(TOTAL_STOP_BUDGET_SECS);

    // Fast feeds: the flush keeps its whole cap (it is a CAP, not a target — the extra seconds
    // the feeds did not use are not handed to it).
    assert_eq!(remaining_teardown_budget(Duration::ZERO), flush);
    assert_eq!(remaining_teardown_budget(Duration::from_secs(FEED_STOP_BUDGET_SECS)), flush);

    // Over budget: the overrun is subtracted, so feed_stop + budget stays at the total.
    let over = Duration::from_secs(FEED_STOP_BUDGET_SECS + 5);
    assert_eq!(over + remaining_teardown_budget(over), total);

    // Far over: the floor wins, and the daemon has already warned that the total is blown.
    let way_over = Duration::from_secs(TOTAL_STOP_BUDGET_SECS + 60);
    assert_eq!(
        remaining_teardown_budget(way_over),
        Duration::from_secs(MIN_FLUSH_BUDGET_SECS),
        "a slow feed stop must never leave the flush with nothing — that is the loss the bound \
             exists to prevent"
    );
}

/// The status is a DISTINCT non-zero, not the generic failure: `1` already means "the profile
/// was bad" or "the store would not open", and a supervisor should be able to tell "the daemon
/// worked and the data did not" apart from those.
#[test]
fn the_silence_exit_status_is_distinct_from_a_generic_failure() {
    assert_eq!(EXIT_SILENT, 3);
    assert_ne!(EXIT_SILENT, 0, "it must be non-zero for Restart=on-failure to see it");
    assert_ne!(EXIT_SILENT, 1, "…and distinguishable from a startup failure");
    assert_ne!(EXIT_SILENT, 2, "…and from the usage error --help/argv parsing uses");
}

/// The same rule for the dry run, and the reason it needed its own status: a `--once` run that
/// resolved NOTHING used to exit **0**, so `--once && systemctl enable --now` green-lit a
/// daemon that would record nothing. Measured on the CI box against the shipped binary, with the two
/// halves of an A/B on one variable both exiting 0.
#[test]
fn the_dry_run_exit_status_is_distinct_from_every_other_status() {
    assert_eq!(EXIT_DRY_RUN, 4);
    assert_ne!(EXIT_DRY_RUN, 0, "a dry run that proved nothing must NOT look like success");
    assert_ne!(EXIT_DRY_RUN, 1, "…nor like a startup failure (bad profile, unopenable store)");
    assert_ne!(EXIT_DRY_RUN, 2, "…nor like a usage error");
    assert_ne!(EXIT_DRY_RUN, EXIT_SILENT, "…nor like --exit-on-silence");
}
