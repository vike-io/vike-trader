use super::*;
use crate::cmd::args::HELP_SENTINEL;
// ⚠ Imported HERE rather than at the top of the file: nothing outside these tests reads the
// roster (the PARSER reaches it through `Period::parse`, which owns the refusal), so a
// top-level import would be an unused one — and this crate's clippy lane runs `-D warnings`.
use crate::cmd::report::schema::PERIODS;

fn parsed(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

fn node_only() -> Args {
    Args {
        source: Source::Node("the CI box:9200".to_string()),
        seed: None,
        periods_per_year: None,
        breakdown: None,
        json: false,
    }
}

// ---- the grammar: the LIVE source, unchanged ----

#[test]
fn both_flag_forms_parse_and_the_optionals_default_to_the_nodes_own() {
    let a = parsed(&["--node", "<host>:9200"]).unwrap();
    assert_eq!(a.source, Source::Node("<host>:9200".to_string()));
    assert_eq!(a.seed, None);
    assert_eq!(a.periods_per_year, None);
    assert!(!a.json);
    let a = parsed(&["--node=<host>:9200", "--seed=25000", "--json"]).unwrap();
    assert_eq!(a.seed, Some(25_000.0));
    assert!(a.json);
}

#[test]
fn json_is_a_bare_boolean_and_an_inline_value_is_a_usage_error() {
    let err = parsed(&["--node", "n:1", "--json=1"]).unwrap_err();
    assert_eq!(err, "--json takes no value");
}

/// A number that does not parse is refused by NAME — and so is one that parses to a non-finite
/// value, because `serde_json` would write it as `null` and the node would then apply its own
/// default while the operator believed they had set one.
#[test]
fn a_bad_number_is_refused_by_name_and_a_non_finite_one_counts_as_bad() {
    let err = parsed(&["--node", "n:1", "--seed", "lots"]).unwrap_err();
    assert!(err.contains("--seed"), "{err}");
    for bad in ["nan", "inf", "-inf"] {
        let err = parsed(&["--node", "n:1", "--periods-per-year", bad]).unwrap_err();
        assert!(err.contains("finite"), "{bad}: {err}");
    }
}

#[test]
fn help_short_circuits_even_without_a_source() {
    for spelling in ["-h", "--help"] {
        assert_eq!(parsed(&[spelling]).unwrap_err(), HELP_SENTINEL);
    }
}

#[test]
fn an_unknown_flag_is_rejected_by_name() {
    let err = parsed(&["--node", "n:1", "--html", "out.html"]).unwrap_err();
    assert!(err.contains("--html"), "{err}");
}

// ---- the grammar: the STORED source ----

/// A bare positional is the run SELECTOR, and it takes the whole selector grammar — including
/// the `@` forms, which a naive parser would mistake for something else.
#[test]
fn a_positional_is_the_stored_run_selector() {
    for selector in ["1756080000-abcd-0", "1756080", "@last", "@last:backtest", "@baseline"] {
        let a = parsed(&[selector]).unwrap();
        assert_eq!(a.source, Source::Run(selector.to_string()), "{selector}");
    }
    let a = parsed(&["@last", "--breakdown", "month", "--json"]).unwrap();
    assert_eq!(a.breakdown, Some(Period::Month));
    assert!(a.json);
}

/// ⚠ A selector carrying an `=` survives. The shared flag iterator splits every argument on the
/// first `=`, so a positional has to be re-joined or a selector is silently TRUNCATED — which
/// would resolve to a different run, or to none, with nothing saying why.
#[test]
fn a_selector_containing_an_equals_is_put_back_together() {
    let a = parsed(&["@mark=v2"]).unwrap();
    assert_eq!(a.source, Source::Run("@mark=v2".to_string()));
}

/// `--breakdown`'s value is checked against the PUBLISHED roster, so the parser and
/// `crate::cmd::report::schema::json_schema`'s `enum` cannot name different sets.
#[test]
fn a_breakdown_period_is_checked_against_the_published_roster() {
    let err = parsed(&["@last", "--breakdown", "fortnight"]).unwrap_err();
    for p in PERIODS {
        assert!(err.contains(p), "the refusal must name `{p}`: {err}");
    }
    assert_eq!(parsed(&["@last", "--breakdown=day"]).unwrap().breakdown, Some(Period::Day));
}

/// Two run selectors is a refusal that names the verb which DOES take two runs — an operator
/// who typed two wants a comparison, and this verb renders one run.
#[test]
fn two_selectors_are_refused_and_point_at_the_verb_that_takes_two() {
    let err = parsed(&["a-1-0", "b-1-0"]).unwrap_err();
    assert!(err.contains("a-1-0") && err.contains("b-1-0"), "{err}");
    assert!(err.contains("backtest diff"), "{err}");
}

// ---- the SOURCE rules ----

/// ⚠ **No source is a refusal that names every source, and no source may become a default.**
/// Defaulting to the node would dial an address nobody gave; defaulting to `@last` would report
/// on whatever happened to run most recently, which is the silent precedence the selector
/// grammar refuses everywhere else.
#[test]
fn no_source_names_all_three_rather_than_defaulting_to_one() {
    let err = parsed(&["--json"]).unwrap_err();
    assert!(err.contains("--node"), "{err}");
    assert!(err.contains("run selector"), "{err}");
    assert!(err.contains("--schema"), "{err}");
}

/// ⚠ **Two sources is a refusal, never a precedence.** An operator who typed both believes both
/// matter; answering about one silently is the defect. All three pairings are asserted, because
/// a guard that only caught the obvious one (`--node` beside a selector) would let `--schema`
/// silently win over a real run.
#[test]
fn naming_two_sources_is_refused_with_both_named() {
    for (line, a, b) in [
        (vec!["@last", "--node", "n:1"], "--node", "<run>"),
        (vec!["@last", "--schema"], "<run>", "--schema"),
        (vec!["--node", "n:1", "--schema"], "--node", "--schema"),
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(err.contains(a) && err.contains(b), "{line:?}: {err}");
        assert!(err.contains("ONE source"), "{line:?}: {err}");
        assert!(err.contains("no precedence"), "{line:?}: {err}");
    }
    // …and all three at once still names all three.
    let err = parsed(&["@last", "--node", "n:1", "--schema"]).unwrap_err();
    for name in ["--node", "<run>", "--schema"] {
        assert!(err.contains(name), "{err}");
    }
}

/// ⚠ **The refusal that matters most.** `--seed` and `--periods-per-year` RESCALE every return
/// ratio the live renderer computes; a stored run recomputes none of them. Silently dropping
/// either would hand somebody ratios they believe are scaled to their account — so both are
/// refused by name, with what to do instead.
#[test]
fn a_live_only_knob_is_refused_on_a_stored_run_rather_than_dropped() {
    let err = parsed(&["@last", "--seed", "25000"]).unwrap_err();
    assert!(err.starts_with("--seed"), "{err}");
    assert!(err.contains("verbatim"), "it must say what a stored run does instead: {err}");
    assert!(err.contains("--node"), "…and name the source that takes it: {err}");

    let err = parsed(&["@last", "--periods-per-year", "365"]).unwrap_err();
    assert!(err.starts_with("--periods-per-year"), "{err}");
    assert!(err.contains("annualized when it RAN"), "{err}");
}

/// …and the mirror image: `--breakdown` buckets a STORED curve, so the live source refuses it
/// rather than sending a flag the node has never heard of.
#[test]
fn a_stored_only_knob_is_refused_on_the_live_source() {
    let err = parsed(&["--node", "n:1", "--breakdown", "day"]).unwrap_err();
    assert!(err.starts_with("--breakdown"), "{err}");
    assert!(err.contains("no stored curve"), "{err}");
}

/// `--schema` answers about the DOCUMENT, so every other flag on the line was written in the
/// belief that it would change the output and none of them can. An inert flag is the defect
/// this workspace refuses; a named refusal is the cure.
#[test]
fn schema_refuses_every_flag_that_cannot_change_it() {
    for line in [
        vec!["--schema", "--json"],
        vec!["--schema", "--seed", "1"],
        vec!["--schema", "--periods-per-year", "365"],
        vec!["--schema", "--breakdown", "day"],
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(err.contains("--schema"), "{line:?}: {err}");
        assert!(err.contains("one form"), "{line:?}: {err}");
    }
    // …and on its own it parses to the schema source and nothing else.
    assert_eq!(parsed(&["--schema"]).unwrap().source, Source::Schema);
}

/// The schema really is printable with no run, no node and no key — the property that makes it
/// the one surface this verb has on a box with none of those.
#[test]
fn the_schema_prints_without_a_run_a_node_or_a_key() {
    let text = schema_text();
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(doc["type"], serde_json::json!("object"));
    assert!(doc["properties"]["schema"]["const"].is_number(), "{doc}");
}

// ---- the LIVE rendering ----

/// `--json` is the node's body VERBATIM (a machine consumer gets the one schema the producer
/// owns), and the default is that same document formatted.
#[test]
fn json_is_verbatim_and_the_default_is_the_same_document_formatted() {
    let body = r#"{"trades":3,"sharpe":1.25}"#;
    assert_eq!(render(body, true).unwrap(), body);

    let pretty = render(body, false).unwrap();
    assert!(pretty.contains('\n'), "formatted: {pretty}");
    let back: serde_json::Value = serde_json::from_str(&pretty).unwrap();
    assert_eq!(back["trades"], 3);
    assert_eq!(back["sharpe"], 1.25);
}

/// A body that is not the contracted document is an ERROR, never something echoed with a
/// success status — a script reading stdout would otherwise get garbage and exit 0.
#[test]
fn a_non_json_body_is_a_failure_rather_than_an_echo() {
    let err = render("<html>502 Bad Gateway</html>", false).unwrap_err();
    assert!(err.contains("not valid JSON"), "{err}");
}

// ---- the LIVE failure mapping ----

/// The refusal every `--node` invocation currently takes: the client's own sentence survives
/// verbatim (it names the capability and that nothing was sent), and the added lines carry the
/// ONE thing that works today — on the box that holds the journal, with this invocation's own
/// flags carried across so nothing is dropped in translation.
///
/// ⚠ These are the WORDS only, and the error is one this test WROTE. That a real node
/// produces it — that the path is reachable at all — is
/// `crates/vike-cli/tests/study_report_refusal_cli.rs`, driving the shipped binary against a
/// real paper `vike-tradehub`.
#[test]
fn an_unsupported_node_is_told_what_works_today_with_the_flags_carried_over() {
    let err = io::Error::new(
        io::ErrorKind::Unsupported,
        "this node does not advertise the \"tearsheet\" capability — Tearsheet refused \
             client-side, nothing was sent",
    );
    let args = Args { seed: Some(25_000.0), json: true, ..node_only() };
    let lines = failure_lines("the CI box:9200", &args, &err);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[0].contains("tearsheet"), "{}", lines[0]);
    assert!(lines[0].contains("nothing was sent"), "{}", lines[0]);
    assert!(lines[1].contains("the CI box:9200"), "{}", lines[1]);
    assert!(lines[2].contains("vike-backend report"), "{}", lines[2]);
    assert!(lines[2].contains("--seed 25000"), "the seed is carried: {}", lines[2]);
    assert!(lines[2].contains("--json"), "the output shape is carried: {}", lines[2]);
}

/// …and an invocation with no optional flags gets a fallback with none either — the line
/// describes THIS run; it is not a template with everything filled in.
#[test]
fn the_fallback_carries_only_the_flags_that_were_given() {
    let line = backend_fallback(&node_only());
    assert!(line.contains("vike-backend report --journal"), "{line}");
    assert!(!line.contains("--seed"), "{line}");
    assert!(!line.contains("--periods-per-year"), "{line}");
    assert!(!line.contains("--json"), "{line}");
}

#[test]
fn an_auth_refusal_points_at_the_observe_key_not_the_verb() {
    let err = io::Error::new(io::ErrorKind::PermissionDenied, "auth denied: bad mac");
    let lines = failure_lines("the CI box:9200", &node_only(), &err);
    assert!(lines[0].contains("refused the observe handshake"), "{}", lines[0]);
    assert!(lines[1].contains(OBSERVE_KEY_ENV), "{}", lines[1]);
}

#[test]
fn a_transport_fault_names_the_address() {
    let err = io::Error::new(io::ErrorKind::ConnectionRefused, "connection refused");
    let lines = failure_lines("the CI box:9200", &node_only(), &err);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("the CI box:9200"), "{}", lines[0]);
}

/// The RUNG half of the same match, pinned kind by kind: **the node ANSWERED** ⇒ the
/// pre-existing rung, and only a socket that never got an answer is the retry rung. The
/// `Unsupported` row is the load-bearing one here — it is the outcome of every `--node`
/// invocation until the server half lands, and on the retry rung a wrapper would back off and
/// re-dial a node that can never answer.
#[test]
fn the_rung_follows_whether_the_node_answered() {
    for kind in
        [io::ErrorKind::Unsupported, io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData]
    {
        let err = io::Error::new(kind, "the node said something");
        assert_eq!(failure_exit(&err), Exit::Failed, "{kind:?} is a node that ANSWERED");
    }
    for kind in [
        io::ErrorKind::ConnectionRefused,
        io::ErrorKind::TimedOut,
        io::ErrorKind::ConnectionAborted,
    ] {
        let err = io::Error::new(kind, "no answer");
        assert_eq!(failure_exit(&err), Exit::Connect, "{kind:?} never reached a node");
    }
}

/// ⚠ The usage text must advertise BOTH sources. It is the only discovery surface this verb
/// has, and a usage that named only the node would keep the stored source — the half that works
/// on every box — findable by reading source alone. That is the exact defect
/// `crates/vike-cli/tests/help_cli.rs`'s roster tests exist for, one level up.
#[test]
fn the_usage_advertises_both_sources_and_the_schema() {
    assert!(USAGE.contains("usage:"), "help_cli.rs asserts this token on the shipped binary");
    for token in ["<run>", "--node", "--schema", "--breakdown", "@last"] {
        assert!(USAGE.contains(token), "the usage must advertise `{token}`");
    }
    for period in PERIODS {
        assert!(USAGE.contains(period), "…and every --breakdown period: `{period}`");
    }
    // ⚠ The DECIMATION rule is in the usage, not only in the code: an operator reading `-h` is
    // the person who will otherwise be surprised by a refusal, and the refusal is the feature.
    assert!(USAGE.contains("MAX_EQUITY_SAMPLES"), "the cap that causes the refusal is named");
    assert!(USAGE.contains("DECIMATED"), "…and the word the refusal itself uses");
}
