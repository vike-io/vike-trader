use super::*;
use crate::cmd::args::HELP_SENTINEL;

fn parsed(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

fn full() -> Args {
    Args {
        study: "cohort".to_string(),
        recipe_path: "r.toml".to_string(),
        from: "2026-04-07T05".to_string(),
        to: "2026-08-05T05".to_string(),
        addr: None,
        json: false,
    }
}

// ---- the grammar ----

#[test]
fn the_four_required_flags_parse_and_the_address_is_left_unset() {
    let a = parsed(&[
        "--study",
        "cohort",
        "--recipe",
        "r.toml",
        "--from",
        "2026-04-07T05",
        "--to",
        "2026-08-05T05",
    ])
    .unwrap();
    assert_eq!(a, full());
}

#[test]
fn the_inline_form_parses_and_the_address_is_overridable() {
    let a = parsed(&[
        "--study=cohort",
        "--recipe=r.toml",
        "--from=1785906000",
        "--to=1788584400",
        "--addr=<host>:9999",
    ])
    .unwrap();
    assert_eq!(a.addr.as_deref(), Some("<host>:9999"));
    assert_eq!(a.from, "1785906000");
}

// ---- the address ladder ----

/// The three rungs, in order, over one table — `--addr` beats the settings key, the settings
/// key beats the compiled-in default, and the default is `vike-config`'s rather than a literal
/// of this module's.
#[test]
fn the_address_ladder_is_flag_then_setting_then_the_shared_default() {
    assert_eq!(resolve_addr(Some("<host>:1"), Some("<host>:2")), "<host>:1");
    assert_eq!(resolve_addr(None, Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(None, None), vike_config::DEFAULT_BACKTEST_ADDR);
}

/// A blank rung is ABSENT, not an address: an unset shell variable and an empty TOML string
/// both arrive here as `""`, and dialing that would report a failure naming nothing.
#[test]
fn a_blank_rung_falls_through_rather_than_being_dialled() {
    assert_eq!(resolve_addr(Some("   "), Some("<host>:2")), "<host>:2");
    assert_eq!(resolve_addr(Some(""), None), vike_config::DEFAULT_BACKTEST_ADDR);
    assert_eq!(resolve_addr(None, Some(" ")), vike_config::DEFAULT_BACKTEST_ADDR);
    // …and a value that IS an address is trimmed rather than dialled with its whitespace.
    assert_eq!(resolve_addr(Some(" <host>:1 "), None), "<host>:1");
}

/// ⚠ The default is the COMPUTE daemon's, and this test is a THREE-WAY separation because two
/// of the three numbers are other people's daemons: `127.0.0.1:7879` is the LIVE ORDER-SIGNING
/// daemon (`ss -ltnp` on the production box, 2026-09-10) and `127.0.0.1:7878` is the data
/// server ruling 7 empties. A default equal to either would aim this verb at a process that
/// does not serve it — one of them the one that signs orders.
#[test]
fn the_default_address_is_the_compute_daemons_and_neither_neighbours() {
    let dialled = resolve_addr(None, None);
    assert_eq!(dialled, "127.0.0.1:7880");
    assert_ne!(dialled, "127.0.0.1:7879", "that is the LIVE order-signing daemon");
    assert_ne!(dialled, "127.0.0.1:7878", "that is the DATA server ruling 7 empties");
}

/// Every required flag is refused BY NAME when absent — one test over all four, because a
/// message that names the wrong flag is the defect worth catching.
#[test]
fn each_missing_required_flag_is_named() {
    let base = ["--study=c", "--recipe=r.toml", "--from=a", "--to=b"];
    for (i, missing) in ["--study", "--recipe", "--from", "--to"].iter().enumerate() {
        let args: Vec<&str> =
            base.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, s)| *s).collect();
        let err = parsed(&args).unwrap_err();
        assert!(err.contains(missing), "expected {missing} to be named: {err}");
    }
}

/// An EMPTY value is refused too — it would otherwise reach the backend as a string it cannot
/// parse, spending a round trip on something visible here.
#[test]
fn an_empty_required_value_is_refused_here_not_on_the_backend() {
    let err = parsed(&["--study=", "--recipe=r.toml", "--from=a", "--to=b"]).unwrap_err();
    assert!(err.contains("--study"), "{err}");
    assert!(err.contains("empty"), "{err}");
}

/// The two BACKEND-side paths are refused by name with the reason, never dropped into the
/// generic unknown-argument arm: an operator passing one believes they are steering a machine
/// this side cannot see.
#[test]
fn the_backend_side_paths_are_refused_with_a_reason() {
    for flag in ["--store", "--lightgbm"] {
        let err =
            parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b", flag, "/somewhere"])
                .unwrap_err();
        assert!(err.contains(flag), "{err}");
        assert!(err.contains("BACKEND"), "it says whose box the path is on: {err}");
    }
}

/// `--json` is ACCEPTED now. It was refused BY NAME with "the flag arrives with the request
/// variant that returns an answer" — and this IS that variant, so a refusal that outlived its
/// reason would be the `Policy::max_total_exposure` shape: positive confirmation of something
/// false.
#[test]
fn json_is_accepted_now_that_there_is_an_answer_to_shape() {
    let a = parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b", "--json"])
        .expect("--json parses");
    assert!(a.json, "the flag sets the field");
    let without = parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b"])
        .expect("and its absence is the rendered default");
    assert!(!without.json);
}

#[test]
fn help_short_circuits_even_without_the_required_flags() {
    for spelling in ["-h", "--help"] {
        assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
    }
}

#[test]
fn an_unknown_argument_is_rejected_by_name() {
    let err = parsed(&["--study=c", "--recipe=r.toml", "--from=a", "--to=b", "--scratch=/tmp/x"])
        .unwrap_err();
    assert!(err.contains("--scratch"), "{err}");
}

// ---- the negotiation's two sides ----

/// The refusal a peer that ANSWERS produces: it names the capability, says nothing was sent,
/// confirms the recipe was readable (so a reader knows which half failed), and carries the
/// invocation that works today with this run's own arguments in it.
///
/// ⚠ These are the WORDS only. That this path is REACHED at all — a real server of this
/// protocol, a real handshake, and no frame after it — is
/// `crates/vike-cli/tests/study_report_refusal_cli.rs`, because a unit test over a pure
/// formatter cannot tell the difference between a message an invocation produces and one
/// nothing can reach.
#[test]
fn a_backend_without_the_capability_is_told_what_works_today() {
    let lines = no_capability_lines(&full(), "127.0.0.1:7880", 412);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains(FEATURE_STUDY), "{}", lines[0]);
    assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
    assert!(lines[0].contains("412 bytes"), "the recipe read is reported: {}", lines[0]);
    assert!(lines[2].contains("vike-backend study"), "{}", lines[2]);
    assert!(lines[2].contains("--study cohort"), "{}", lines[2]);
    assert!(lines[2].contains("--from 2026-04-07T05"), "the window is carried: {}", lines[2]);
    assert!(lines[2].contains("--to 2026-08-05T05"), "the window is carried: {}", lines[2]);
}

/// The fallback names no path this side cannot know: the store root and the trainer stay
/// placeholders, because inventing either would be this machine describing another one's
/// filesystem.
#[test]
fn the_fallback_leaves_the_backend_only_paths_as_placeholders() {
    let line = backend_fallback(&full());
    assert!(line.contains("<the backend's hist store>"), "{line}");
    assert!(line.contains("<the pinned trainer>"), "{line}");
    assert!(line.contains("--recipe r.toml"), "the LOCAL recipe path is real: {line}");
}

// ---- the dial failure: the words, and the rung ----

/// The RUNG, pinned kind by kind against the rule `crate::cmd::report`'s `failure_exit`
/// states: **the backend ANSWERED** ⇒ the pre-existing rung, and only a socket that never got
/// an answer is the retry rung. The two ANSWERED rows are the load-bearing ones — an auth
/// refusal and a protocol-version mismatch are permanent configuration facts about a REACHABLE
/// box, and on `Exit::Connect` a wrapper would back off and re-dial forever.
#[test]
fn the_rung_follows_whether_the_backend_answered() {
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
        let err = io::Error::new(kind, "the backend said something");
        assert_eq!(connect_failure_exit(&err), Exit::Failed, "{kind:?}: it ANSWERED");
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::ConnectionAborted,
    ] {
        let err = io::Error::new(kind, "no answer");
        assert_eq!(connect_failure_exit(&err), Exit::Connect, "{kind:?} never answered");
    }
}

/// …and the WORDS follow the same split: an auth refusal points at the keys, a protocol
/// mismatch says the backend answered with something else, and everything else is an honest
/// transport report naming the address. EVERY arm ends with the invocation that works — the
/// transport one above all, because a box with nothing listening on the compute address is the
/// ordinary box today.
#[test]
fn a_refused_dial_says_which_half_failed_and_still_names_what_works() {
    let addr = "127.0.0.1:7880";
    for (kind, needle) in [
        (io::ErrorKind::PermissionDenied, "did not verify"),
        (io::ErrorKind::InvalidData, "answered"),
        (io::ErrorKind::ConnectionRefused, "cannot connect"),
    ] {
        let err = io::Error::new(kind, "something");
        let lines = connect_failure_lines(&full(), addr, &err);
        let text = lines.join("\n");
        assert!(text.contains(addr), "{kind:?} must name the address: {text}");
        assert!(text.contains(needle), "{kind:?}: {text}");
        assert!(
            lines.last().is_some_and(|l| l.contains("vike-backend study")),
            "{kind:?} must still hand over the command that works: {text}"
        );
    }
}

/// The transport arm carries one extra sentence the other two do not, and it is the one an
/// operator most needs when this fires: nothing is listening on the compute address, so a
/// refused connection is a daemon that was never stood up rather than a broken tunnel — and
/// standing it up would not make the verb WORK either, because `vike-backend backtest --addr`
/// does not advertise `study` yet. ⚠ This doc said the daemon "has not shipped"; ruling 7's
/// daemon half has since landed, so the sentence had to stop blaming its absence.
#[test]
fn a_dead_socket_says_why_nothing_is_listening_there_yet() {
    let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
    let text = connect_failure_lines(&full(), "127.0.0.1:7880", &err).join("\n");
    assert!(text.contains("no compute daemon is listening there"), "{text}");
    assert!(text.contains("backtest --addr"), "it names the daemon: {text}");
}
