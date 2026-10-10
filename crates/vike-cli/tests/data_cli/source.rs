//! `data source`: the source roster, `show`, and the axis the listing must agree with.

use super::*;

/// **THE DERIVED CROSS-CHECK, across two processes.** `ls --json` names the sources; every row it
/// calls `designed` is then driven at the REAL `--source` axis and must be refused by name. So the
/// listing cannot advertise a state the axis disagrees with, and neither side's roster is written
/// down in this file — an integration test cannot see the module's private consts, and a literal
/// here would be the subtract-only failure `help_names_every_subcommand_and_exits_zero` documents.
///
/// ⚠ The `built` half is the anti-vacuity control, and it is driven too: `--source demo` reaches
/// the ENGINE and fails on a missing one (exit 3), which proves the axis did not refuse the value.
/// Without it, "every designed source is refused" would pass on a listing that called all nine
/// designed.
#[test]
fn source_ls_is_a_roster_the_axis_agrees_with() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("stdout under --json is the document, whole");
    let rows = doc["sources"].as_array().expect("a sources array");

    let designed: Vec<&str> = rows
        .iter()
        .filter(|r| r["state"] == "designed")
        .map(|r| r["name"].as_str().expect("a name"))
        .collect();
    let built: Vec<&str> = rows
        .iter()
        .filter(|r| r["state"] == "built")
        .map(|r| r["name"].as_str().expect("a name"))
        .collect();
    assert!(!designed.is_empty(), "the listing must carry the designed half: {doc}");
    assert!(!built.is_empty(), "…and the built half: {doc}");

    for name in &designed {
        let fetch = run(
            scratch.path(),
            &["data", "hist", "fetch", "binance:BTCUSDT:1h", "--days", "1", "--source", name],
        );
        let err = stderr(&fetch);
        assert_eq!(fetch.status.code(), Some(2), "`--source {name}` is a usage refusal: {err}");
        // ⚠ The FLAG SPELLING, not the bare name — and the difference is that the bare name could
        // not fail for the `vike` row. Every refusal this binary writes is prefixed
        // `vike-cli data: `, so `err.contains("vike")` was satisfied by the binary's own name
        // whatever the message said; the refusal could have stopped interpolating the value
        // entirely and this assertion would still have passed off the prefix.
        assert!(
            err.contains(&format!("--source {name}")),
            "…naming the VALUE that was refused, not just the plane: {err}"
        );
        assert!(err.contains("not built yet"), "…and never as a spelling mistake: {err}");
        // ...and the listing's own cells say the same thing the axis just said.
        let row = rows.iter().find(|r| r["name"] == *name).expect("the row we came from");
        assert_eq!(row["reaches"], serde_json::Value::Null, "a designed source reaches nothing");
        assert!(row["cost"].as_str().is_some_and(|c| !c.is_empty()), "{row}");
    }

    // THE CONTROL. `demo` is listed as built, so the axis must ACCEPT it — the run gets as far as
    // looking for the engine and fails on the connect rung, which no refused value ever reaches.
    assert!(built.contains(&"demo"), "the built half must name `demo`: {doc}");
    let absent = scratch.path().join("no-such-engine");
    let ok = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "fetch",
            "--source",
            "demo",
            "--engine",
            absent.to_str().expect("utf-8 temp path"),
        ],
    );
    let err = stderr(&ok);
    assert_eq!(ok.status.code(), Some(3), "a built source is not a usage refusal: {err}");
    assert!(!err.contains("not built yet"), "{err}");
}

/// **THE HONEST BOUNDARY, over the shipped binary.** `show` describes a source and says, in its own
/// output, that it verified nothing — because it cannot: this binary links no HTTP client, so the
/// unauthenticated manifest read §11 puts on this verb is not available in this phase.
///
/// ⚠ The `--json` half is the one that matters for a wrapper: `verified_against_the_vendor` is a
/// FIELD rather than a sentence, so a consumer folding this document cannot mistake a local
/// description for a probe of what its key reaches. The URL assertion is the third leg — all three
/// of that source's bases are overridable where their lane is configured and this side resolves
/// none of them, so printing one would name a base this box may not be using.
#[test]
fn source_show_describes_without_ever_claiming_a_probe() {
    let scratch = tempfile::tempdir().expect("tempdir");
    for name in ["vike", "demo", "binance"] {
        let out = run(scratch.path(), &["data", "source", "show", name]);
        assert!(out.status.success(), "{}", stderr(&out));
        let text = stdout(&out);
        assert!(text.contains("nothing above was read"), "`show {name}`: {text}");
        assert!(!text.contains("https://"), "`show {name}` resolved no base: {text}");

        let out = run(scratch.path(), &["data", "source", "show", name, "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        let doc: serde_json::Value =
            serde_json::from_str(&stdout(&out)).expect("stdout under --json is the document");
        assert_eq!(doc["source"], name);
        assert_eq!(
            doc["verified_against_the_vendor"],
            serde_json::Value::Bool(false),
            "`show {name} --json` must state the limit as a field: {doc}"
        );
    }

    // `vike` is the one row §9.1/§9.2 are about: TWO classes, kept apart, and the ruling that it
    // serves no CEX market data stated outright rather than left to be inferred.
    let out = run(scratch.path(), &["data", "source", "show", "vike"]);
    let text = stdout(&out);
    assert!(text.contains("TWO CLASSES"), "{text}");
    assert!(text.contains("NO CEX market data"), "{text}");
    let out = run(scratch.path(), &["data", "source", "show", "vike", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(doc["holds"].as_array().map(Vec::len), Some(2), "two classes, separately: {doc}");
    // The control: a source with nothing to separate carries an EMPTY `holds`, so the assertion
    // above is about that row rather than about the field existing at all.
    let out = run(scratch.path(), &["data", "source", "show", "demo", "--json"]);
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(doc["holds"].as_array().map(Vec::len), Some(0), "{doc}");
}

/// `--addr` is refused BY NAME on a group that reaches no server, on the USAGE rung — and another
/// flag is a DIFFERENT answer, so the refusal is about this flag rather than about every flag.
///
/// ⚠ **The second half used to assert `unknown option`, and the shipped binary no longer says it.**
/// `data source ls --store /srv/vike/data` printed `unknown option '--store'` — a lie by the
/// module's own standard, since `--store` is a real, documented `data hist` flag, and one that sent
/// an operator to check a spelling that was right. Every `--` token this group does not take is now
/// refused as a flag that belongs ELSEWHERE, so this case drives a real sibling flag rather than an
/// invented one.
#[test]
fn source_refuses_addr_by_name_and_a_sibling_groups_flag_differently() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls", "--addr", "127.0.0.1:7878"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "a fixable command line is the usage rung: {err}");
    assert!(err.contains("--addr"), "{err}");
    assert!(err.contains("reaches no server"), "the refusal must say WHY: {err}");
    assert!(err.contains("data hist"), "…and name what does take one: {err}");
    assert_eq!(stdout(&out), "", "nothing was printed as though it had run");

    let out = run(scratch.path(), &["data", "source", "ls", "--store", "/srv/vike/data"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2));
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("--store"), "{sentence}");
    assert!(
        !sentence.contains("unknown"),
        "`--store` is a real `data hist` flag, so calling it unknown is the lie: {sentence}"
    );
    assert!(sentence.contains("data hist"), "…and the refusal must say where it belongs: {err}");
    assert!(!sentence.contains("reaches no server"), "…which is `--addr`'s own answer: {sentence}");

    // ⚠ THE OTHER FACE OF THE SAME LIE, and the control this pair needs. The refusal above used to
    // answer EVERY `--` token, so a TYPO was told it was spelt correctly and belonged to a sibling.
    // `--stroe` is a flag nowhere in this binary; it must be refused WITHOUT that claim, or the
    // operator is sent to `data hist` to type it again.
    let out = run(scratch.path(), &["data", "source", "ls", "--stroe", "/srv/vike/data"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2));
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("--stroe"), "{sentence}");
    assert!(
        sentence.contains("not a `data hist` flag either"),
        "a typo must not be told it belongs to a sibling group: {sentence}"
    );
    assert!(
        !sentence.contains("it belongs there"),
        "…which is the claim that made this a lie: {sentence}"
    );
}

/// The ROW a token heads in a help page, or `None`.
///
/// ⚠ It exists because `text.contains(verb)` over a whole help page cannot fail for the reason the
/// test below names. `ls` is a substring of `jsonl` in the `--format` row, of `` `ls` `` in the
/// `--json` prose and of the word `false`; `show` is a substring of "For `show`". Deleting either
/// verb's ROW left `data source --help` advertising neither verb and every assertion green. A row
/// is found by its HEAD — the token at the start of an indented line — which only that row can
/// satisfy.
fn advertised_row<'a>(text: &'a str, token: &str) -> Option<&'a str> {
    text.lines()
        .map(str::trim_start)
        .find(|line| line.strip_prefix(token).is_some_and(|r| r.starts_with(char::is_whitespace)))
}

/// The group's own help is a SUCCESS on stdout — the shared `HELP_SENTINEL` path — and it gives
/// each verb a ROW of its own, which is the only place they are advertised.
#[test]
fn source_help_names_both_verbs_and_exits_zero() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    for verb in ["ls", "show"] {
        assert!(
            advertised_row(&text, verb).is_some(),
            "`data source --help` must give `{verb}` a row of its own: {text}"
        );
    }
    assert!(text.contains("verified"), "…and the limit the group is built around: {text}");
    // THE CONTROL: a verb this group does not have heads no row, so the assertions above are about
    // the rows rather than about the page being long enough to contain any short string.
    assert!(advertised_row(&text, "fetch").is_none(), "a `data hist` verb is not on this page");

    // ...and a group with no verb RENDERS that roster rather than restating it. ⚠ The SENTENCE, not
    // the whole stream: `exit_for_parse_error` prints the usage after the message, and the usage
    // names every verb — so a refusal that named none of them would have passed off the help text
    // printed underneath it.
    let out = run(scratch.path(), &["data", "source"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(2), "{err}");
    let sentence = err.lines().next().unwrap_or_default();
    assert!(sentence.contains("needs a verb"), "{sentence}");
    for verb in ["ls", "show"] {
        assert!(sentence.contains(verb), "the refusal itself must name `{verb}`: {sentence}");
    }
}

/// **THE HONEST BOUNDARY ON THE OTHER VERB.** `ls` says, in BOTH renderings, that it read nothing
/// — and the document says it as the same testable FIELD `show` carries.
///
/// ⚠ It did not. The disclaimer was pushed by `show` alone, so the verb that prints a column headed
/// REACHES — `a datahub`, `the engine, on this box` — carried no statement anywhere that nothing
/// had been asked, and `ls --json` carried `count`/`sources`/`notes` with no honesty field at all.
/// A wrapper folding it read `{"name":"starter","reaches":"the engine, on this box"}` and reported
/// that the starter lane was reachable FROM THIS BOX. It is a fact about the build.
#[test]
fn source_ls_says_it_read_nothing_either() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let out = run(scratch.path(), &["data", "source", "ls"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing above was read"), "`ls` must name the limit: {text}");
    assert!(text.contains("REACHES"), "…on the verb that prints that column: {text}");

    let out = run(scratch.path(), &["data", "source", "ls", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("one document");
    assert_eq!(
        doc["verified_against_the_vendor"],
        serde_json::Value::Bool(false),
        "the roster document must carry the same field its sibling verb carries: {doc}"
    );
    // ...and every note it documents is a note the table prints, which is what makes `notes` mean
    // one thing across this group's two verbs.
    for note in doc["notes"].as_array().expect("an array of footnotes") {
        let note = note.as_str().expect("a footnote is a sentence");
        assert!(text.contains(note), "`ls` must print the note it documents: {note}");
    }
}
