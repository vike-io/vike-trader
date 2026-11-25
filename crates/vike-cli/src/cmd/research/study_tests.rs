use std::io;

use super::*;

fn parsed(args: &[&str]) -> Result<Args, args::Stop> {
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
//
// The ladder and the blank rung are `crates/vike-client/src/resolved.rs`'s `backtesthub_dial_address`,
// the one implementation this verb calls, and are pinned beside it:
// `crates/vike-client/src/resolved_tests.rs`'s
// `the_dial_address_is_the_flag_then_the_setting_then_the_default_and_a_blank_rung_is_absent`.

/// ⚠ The default is the backtesthub's, and this test is a THREE-WAY separation because two
/// of the three numbers are other people's daemons: `127.0.0.1:7879` is the LIVE ORDER-SIGNING
/// daemon (`ss -ltnp` on the production box, 2026-09-10) and `127.0.0.1:7878` is the data
/// server ruling 7 empties. A default equal to either would aim this verb at a process that
/// does not serve it — one of them the one that signs orders.
#[test]
fn the_default_address_is_the_backtesthubs_and_neither_neighbours() {
    let dialled = backtesthub_dial_address(None, None);
    assert_eq!(dialled, "127.0.0.1:7880");
    assert_ne!(dialled, "127.0.0.1:7879", "that is the LIVE order-signing daemon");
    assert_ne!(dialled, "127.0.0.1:7878", "that is the DATA server ruling 7 empties");
}

/// `--addr`'s help names the last rung and its two neighbours from vike-config, in the bytes it always
/// printed (B5.03).
#[test]
fn the_address_help_names_the_three_hub_defaults_as_it_always_read() {
    let help = address_help();
    assert!(
        help.contains(
            "setting, then 127.0.0.1:7880 — NOT 7879, which is the live order-signing daemon, and \
             not 7878, which is the data server"
        ),
        "{help}"
    );
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
        assert!(parsed(&[spelling]).unwrap_err().is_help(), "{spelling} is a success");
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
/// `crates/vike-cli/tests/shipped_cli/study_report_refusal_cli.rs`, because a unit test over a pure
/// formatter cannot tell the difference between a message an invocation produces and one
/// nothing can reach.
#[test]
fn a_backend_without_the_capability_is_told_what_works_today() {
    let lines = no_capability_lines(&full(), "127.0.0.1:17880", 412);
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

/// The RUNG, pinned variant by variant against the rule `crate::cmd::trade::status`'s
/// `failure_exit` states: **the backend ANSWERED** ⇒ the pre-existing rung, and only a socket that
/// never got an answer is the retry rung. The ANSWERED rows are the load-bearing ones — an auth
/// refusal, a protocol-version mismatch and an older backend are permanent configuration facts about
/// a REACHABLE box, and on `Exit::Connect` a wrapper would back off and re-dial forever. A dial a
/// local firewall refused (`PermissionDenied` from `connect`) never reached it.
#[test]
fn the_rung_follows_whether_the_backend_answered() {
    for answered in answered() {
        assert_eq!(connect_failure_exit(&answered), Exit::Failed, "{answered:?}");
    }
    for kind in
        [io::ErrorKind::ConnectionRefused, io::ErrorKind::TimedOut, io::ErrorKind::PermissionDenied]
    {
        let dial = HubCallError::NotSent(io::Error::new(kind, "no answer"));
        assert_eq!(connect_failure_exit(&dial), Exit::Connect, "{kind:?}");
    }
}

/// The three ways the backend answers a dial: a refused key, a version skew, an older backend.
fn answered() -> [HubCallError; 3] {
    let words = || "something".to_string();
    [
        HubCallError::AccessDenied(words()),
        HubCallError::Protocol(words().into()),
        HubCallError::NotOffered { feature: words(), text: words() },
    ]
}

/// …and the WORDS follow the same split: an auth refusal points at the keys, a protocol
/// mismatch says the backend answered with something else, and everything else is an honest
/// transport report naming the address. EVERY arm ends with the invocation that works — the
/// transport one above all, because a box with nothing listening on the compute address is the
/// ordinary box today.
#[test]
fn a_refused_dial_says_which_half_failed_and_still_names_what_works() {
    let addr = "127.0.0.1:17880";
    let [denied, skew, older] = answered();
    let dead = HubCallError::NotSent(io::Error::new(io::ErrorKind::ConnectionRefused, "something"));
    for (err, needle) in
        [(denied, "access list"), (skew, "answered"), (older, "answered"), (dead, "cannot connect")]
    {
        let lines = connect_failure_lines(&full(), addr, &err);
        let text = lines.join("\n");
        assert!(text.contains(addr), "{err:?} must name the address: {text}");
        assert!(text.contains(needle), "{err:?}: {text}");
        assert!(
            lines.last().is_some_and(|l| l.contains("vike-backend study")),
            "{err:?} must still hand over the command that works: {text}"
        );
    }
}

/// The transport arm carries one extra sentence the other two do not, and it is the one an
/// operator most needs when this fires: nothing is listening on the compute address, so a
/// refused connection is a daemon that was never stood up rather than a broken tunnel — and
/// standing it up would not make the verb WORK either, because `vike-backend backtesthub --addr`
/// does not advertise `study` yet. ⚠ This doc said the daemon "has not shipped"; ruling 7's
/// daemon half has since landed, so the sentence had to stop blaming its absence.
#[test]
fn a_dead_socket_says_why_nothing_is_listening_there_yet() {
    let err = HubCallError::NotSent(io::Error::new(
        io::ErrorKind::ConnectionRefused,
        "connection refused",
    ));
    let text = connect_failure_lines(&full(), "127.0.0.1:17880", &err).join("\n");
    assert!(text.contains("no backtesthub is listening there"), "{text}");
    assert!(text.contains("backtesthub --addr"), "it names the daemon: {text}");
}

// ---- the answer ----

/// A saved run prints its id and directory, then every metric in the backend's printed form.
#[test]
fn a_saved_run_prints_where_it_went_and_its_metrics() {
    let json = r#"{"run_id":"1763424000-1","dir":"/srv/runs/1763424000-1","study":"cohort","metrics":[{"name":"sharpe","value":null,"nonfinite":"NaN","text":"NaN"},{"name":"hit_rate","value":0.5,"nonfinite":null,"text":"0.500000"}],"artifacts":[]}"#;
    let lines = report_lines(json);
    assert_eq!(
        lines.out,
        ["run 1763424000-1 -> /srv/runs/1763424000-1", "sharpe = NaN", "hit_rate = 0.500000"]
    );
    assert_eq!(lines.not_saved, None);
}

/// ⚠ A result the backend could not SAVE still prints its metrics, and the reason rides out for the
/// caller to print on stderr and exit `Failed`: the numbers are never dropped, and never shown as a run.
#[test]
fn an_unsaved_run_prints_its_metrics_and_says_why_it_was_not_saved() {
    let json = r#"{"not_saved":"cannot write /srv/runs/1-1/a.csv: No space left on device","study":"cohort","metrics":[{"name":"sharpe","value":1.25,"nonfinite":null,"text":"1.250000"}],"artifacts":[{"name":"a.csv","bytes":17}]}"#;
    let lines = report_lines(json);
    assert_eq!(lines.out, ["sharpe = 1.250000"]);
    assert_eq!(
        lines.not_saved.as_deref(),
        Some("cannot write /srv/runs/1-1/a.csv: No space left on device")
    );
}
