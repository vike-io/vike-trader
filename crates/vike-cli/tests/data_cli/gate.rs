//! `data hist gate`: the verb whose product is the exit code, over a real loopback datahub.

use super::support::{spawn_seeded_datahub, spawn_shared_account_store_datahub};
use super::*;

/// **The verb whose product is the exit code**, over the three outcomes that must never share a
/// number: held, breached, and nothing evaluated.
///
/// ⚠ The load-bearing row is the THIRD. A spec the store matches nowhere is `7`, not `0` — a gate
/// that answered "pass" for a series it never found is the green-means-nothing-ran failure this
/// ladder exists against, and it is the exact shape a typo in a CI step produces.
#[test]
fn the_gate_answers_a_store_with_one_of_three_rungs() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let gate = |args: &[&str]| {
        let mut v = vec!["data", "hist", "gate"];
        v.extend_from_slice(args);
        v.extend_from_slice(&["--addr", addr.as_str()]);
        run(scratch.path(), &v)
    };

    let held = gate(&["binance:BTCUSDT:1h", "--require-days", "2"]);
    assert_eq!(held.status.code(), Some(0), "two days are there: {}", stderr(&held));
    assert!(stdout(&held).contains("PASS"), "{}", stdout(&held));

    let breached = gate(&["binance:BTCUSDT:1h", "--require-days", "365"]);
    assert_eq!(
        breached.status.code(),
        Some(6),
        "a DECLARED THRESHOLD was breached — the command WORKED: {}",
        stderr(&breached)
    );

    let nothing = gate(&["binance:NOSUCHSYMBOL", "--require-days", "1"]);
    assert_eq!(
        nothing.status.code(),
        Some(7),
        "a spec matching nothing is NOT a pass: {}",
        stderr(&nothing)
    );
    let err = stderr(&nothing);
    assert!(err.contains("nothing to gate"), "{err}");
    assert!(err.contains("this is not a pass"), "…and says so outright: {err}");
    assert!(err.contains("data hist ls"), "…and names the verb that shows the spelling: {err}");
    assert_eq!(stdout(&nothing), "", "there were no criteria, so there is no document");
}

/// ⚠ **The document reaches STDOUT on a BREACH**, which is this verb's one departure from the rule
/// every sibling in `data` follows (a failure is a sentence on stderr, and stdout carries nothing).
/// A breach is not a failure: §7.1 of the backtest surface design requires the verdict to name
/// every criterion that passed and failed, and a CI step handed only the rung would have to re-run
/// the gate to learn which criterion moved.
#[test]
fn a_breaching_gate_still_prints_the_whole_verdict() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT:1h", "--require-days", "365", "--addr", &addr],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("gate binance:BTCUSDT:1h"), "the subject is named: {text}");
    assert!(text.contains("CRITERION") && text.contains("VERDICT"), "{text}");
    assert!(text.contains("require-days"), "the criterion that failed is NAMED: {text}");
    assert!(text.contains(">=365d"), "…with what was asked for: {text}");
    assert!(text.contains("2d"), "…and what the store actually holds: {text}");
    assert!(text.contains("require-kind"), "the PASSING criterion is rendered too: {text}");
    assert!(text.contains("BREACH"), "{text}");
    // ⚠ The DISCLOSURE, which is the difference between a gate that checked one half and a gate
    // that reads as though it checked both. No `--max-gap` was given, so the holes were not looked
    // at, and a verdict that did not say so would be a green over a store with a hole in it.
    assert!(text.contains("HOLES"), "a half-checked gate must say so: {text}");
}

/// A REQUIRED KIND the store does not hold is a BREACH — the operator declared it required — and
/// the row names the kinds this spec DOES hold, so the next command is obvious.
///
/// ⚠ It is emphatically NOT the nothing-was-evaluated rung. That one means *you named a series
/// this store has never heard of*; this one means *the instrument is here and the tape you need is
/// not*. Two different actions, so two different numbers, and the case above asserts the other.
#[test]
fn a_required_kind_the_store_lacks_breaches_and_names_what_is_there() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("require-kind"), "{text}");
    assert!(text.contains("trade"), "the kind that is missing: {text}");
    assert!(text.contains("this spec holds: bar"), "…and the kind that IS there: {text}");
}

/// `--max-gap` is EVALUATED rather than merely accepted: the probe runs, the criterion renders its
/// own answer, and a tolerance finer than the store's own resolution says so.
///
/// ⚠ The second half is a MEASUREMENT of the store, not a style note. A hole is derived from the
/// `date=` partition set, so the smallest one that can be reported is a whole UTC day — an
/// operator who writes `--max-gap 4h` believing they tolerate a four-hour outage is believing
/// something this store cannot express. The `1d` run is the control: it carries no such note, so
/// the note cannot be passing by always firing.
#[test]
fn a_gap_tolerance_is_evaluated_and_a_sub_day_one_says_the_store_cannot_answer_that_finely() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let gate = |max_gap: &str| {
        run(
            scratch.path(),
            &[
                "data",
                "hist",
                "gate",
                "binance:BTCUSDT:1h",
                "--require-days",
                "2",
                "--max-gap",
                max_gap,
                "--addr",
                &addr,
            ],
        )
    };

    let coarse = gate("1d");
    assert_eq!(coarse.status.code(), Some(0), "{}", stderr(&coarse));
    let text = stdout(&coarse);
    assert!(text.contains("max-gap"), "the criterion is rendered: {text}");
    assert!(text.contains("no gaps"), "…with the probe's own answer: {text}");
    assert!(!text.contains("HOLES"), "the holes WERE checked, so no disclosure: {text}");
    assert!(!text.contains("whole UTC day"), "a day-wide tolerance is answerable: {text}");

    let fine = gate("4h");
    assert_eq!(fine.status.code(), Some(0), "{}", stderr(&fine));
    let text = stdout(&fine);
    assert!(text.contains("whole UTC day"), "{text}");
    assert!(text.contains("no missing day at all"), "…and what it therefore means: {text}");
}

/// The `--json` verdict: the same criteria the table renders, plus the EVIDENCE each one was
/// derived from, so a consumer re-derives a judgement rather than trusting it.
///
/// ⚠ The document carries no note and no prose disclosure — it carries the FACTS both notes are
/// derived from (a null `max_gap_ms`, and each series' own numbers), which is this module's
/// standing rule: a note appended to a document a program parses is noise at best.
#[test]
fn the_gate_document_carries_every_criterion_and_the_numbers_behind_it() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "365",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let doc: serde_json::Value = serde_json::from_str(&stdout(&out))
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {}", stdout(&out)));

    assert_eq!(doc["subcommand"], "gate");
    assert_eq!(doc["verdict"], "breach");
    assert_eq!(doc["spec"]["text"], "binance:BTCUSDT:1h");
    assert_eq!(doc["spec"]["venue"], "binance");
    assert_eq!(doc["spec"]["grouped"], false);
    assert_eq!(doc["require_days"], 365);
    assert!(doc["max_gap_ms"].is_null(), "the holes were not asked about: {doc}");
    assert_eq!(doc["require_kinds"][0], "bar", "defaulted rather than absent");
    assert_eq!(doc["series_reported"], 2, "the fixture's whole store");
    assert_eq!(doc["series_matched"], 1);
    assert_eq!(doc["series_judged"], 1);

    let criteria = doc["criteria"].as_array().expect("an array of criteria");
    assert_eq!(criteria.len(), 2, "one presence criterion and one days criterion: {criteria:?}");
    let by = |name: &str| -> serde_json::Value {
        criteria
            .iter()
            .find(|c| c["criterion"] == name)
            .unwrap_or_else(|| panic!("{name} is a criterion: {doc}"))
            .clone()
    };
    assert_eq!(by("require-days")["verdict"], "breach");
    assert!(by("require-days")["why"].is_null(), "nothing went unevaluated: {doc}");
    assert_eq!(by("require-kind")["verdict"], "pass");

    // The EVIDENCE — the store's own numbers, so the verdict above can be re-derived rather than
    // trusted. `gaps` is null because no probe was made, never `[]`, which would say "no holes".
    let series = doc["series"].as_array().expect("an array of series");
    assert_eq!(series.len(), 1);
    assert_eq!(series[0]["kind"], "bar");
    assert_eq!(series[0]["interval"], "1h");
    assert_eq!(series[0]["rows"], 3);
    assert_eq!(series[0]["span_days"], 2, "365 was asked for and 2 is what is there");
    assert!(series[0]["gaps"].is_null(), "an unmade probe is null: {}", series[0]);
    assert!(series[0]["gaps_error"].is_null(), "{}", series[0]);

    // ...and a PASSING gate emits the SAME shape, so a CI step that parses one on success is not
    // parsing something else on failure.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT:1h",
            "--require-days",
            "2",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let doc: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("the same document on a pass");
    assert_eq!(doc["verdict"], "pass");
    assert_eq!(doc["criteria"].as_array().expect("criteria").len(), 2);
}

/// **§9.3.2 on the JUDGING verb: an account series is not evidence here either.**
///
/// ⚠ The first cut of `gate` excluded nothing and argued it owed nothing — "this verb selects by
/// an EXACT spec, and `parse` has already refused that spelling of `--require-kind`". That covers
/// the CRITERION side and not the EVIDENCE side. A spec is `VENUE:NAME`, two of a series' four
/// dimensions, so on a shared store `binance:BTCUSDT` reaches this account's `exec_fill` tape as
/// squarely as it reaches the bars: the presence criterion's `this spec holds:` cell named it and
/// the `--json` `series[]` carried its first_ts/last_ts/rows — out of a market-data read verb,
/// while `ls` over the same store showed neither and said so in a note.
///
/// The ANTI-VACUITY controls are the last two blocks: `ls` renders the SAME sentence over the SAME
/// store (so this is one rule and not a second one), and a gate over a store with no account
/// series carries no note at all (so the note is not simply always printed).
#[test]
fn the_gate_withholds_account_series_from_its_evidence_and_says_so() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let addr = spawn_shared_account_store_datahub().to_string();

    // A kind the store does NOT hold, so the breach renders `this spec holds: …` — the one cell
    // that enumerates everything the spec matched, and the cell that used to leak.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(6), "{}", stderr(&out));
    let text = stdout(&out);
    // ⚠ Asserted on the ROW, not on the page. The DISCLOSURE names `exec_fill` deliberately — a
    // count alone cannot be acted on — so a page-wide `!contains` would be asserting the opposite
    // of §9.3.2's own rule, and it would fail on the note that proves the fix works.
    let holds = text
        .lines()
        .find(|l| l.contains("this spec holds:"))
        .unwrap_or_else(|| panic!("the presence criterion's own row: {text}"));
    assert!(holds.contains("bar"), "the MARKET series IS evidence: {holds}");
    assert!(!holds.contains("exec_fill"), "…and the account series is not: {holds}");

    let note = text
        .lines()
        .find(|l| l.starts_with("note: "))
        .unwrap_or_else(|| panic!("the withholding must be DISCLOSED: {text}"));
    assert!(note.contains("not part of this answer"), "{note}");
    assert!(note.contains("exec_fill"), "…naming the kind it withheld: {note}");
    assert!(note.contains("vike-cli account"), "…and the plane that will serve it: {note}");

    // The DOCUMENT carries neither the row nor the note. A consumer computes the difference from
    // `series_reported` and `series_matched`, which is this module's standing rule for `--json`.
    let out = run(
        scratch.path(),
        &[
            "data",
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--json",
            "--addr",
            &addr,
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let raw = stdout(&out);
    assert!(!raw.contains("exec_fill"), "no account row and no note in the document: {raw}");
    let doc: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {raw}"));
    assert_eq!(doc["series_reported"], 2, "the store's own total, account series included");
    assert_eq!(doc["series_matched"], 1, "…and what survived the exclusion");
    let series = doc["series"].as_array().expect("an array of series");
    assert_eq!(series.len(), 1, "{series:?}");
    assert_eq!(series[0]["kind"], "bar");

    // ONE rule, ONE sentence: `ls` over the same store withholds the same series in the same words.
    let listing = run(scratch.path(), &["data", "hist", "ls", "--addr", &addr]);
    let text = stdout(&listing);
    assert!(text.contains("not part of this answer"), "{text}");
    assert!(text.contains("exec_fill"), "the note NAMES the kind it withheld: {text}");

    // ...and a store with no account series at all draws no note, so the assertions above cannot
    // be passing on a note that fires unconditionally.
    let clean = spawn_seeded_datahub().to_string();
    let out = run(
        scratch.path(),
        &["data", "hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1", "--addr", &clean],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(!stdout(&out).contains("not part of this answer"), "{}", stdout(&out));
}

/// **THE GROUP LAYER, now that EVERY group answers.**
///
/// ⚠ **This was `every_unbuilt_group_is_refused_with_one_sentence_on_the_usage_rung`, and its FIRST
/// claim has died with the last unbuilt group.** It once compared three groups' refusals against
/// each other — three code paths, one sentence, because a reader who typed two of them and got two
/// wordings would learn that one was a different KIND of no. `catalog` and `source` shipped, leaving
/// `realtime` as the one group still refused at the `parse` rung; `realtime` ships here, so no
/// group is refused that way any more and asserting that one is would be a test
/// of a path that no longer runs. `crates/vike-cli/src/cmd/data/hist/parse.rs`'s `unbuilt_group_message` went
/// with it, for the reason its own doc had predicted: a function no caller reaches is
/// `-D dead-code`.
///
/// ⚠ **A SECOND claim died with it, and asserting it is what this round removed.** The surviving
/// loop still read `assert!(!err.contains("designed but not built yet"))` over all four groups, and
/// its doc called that the regression guard. It was not one: `unbuilt_group_message` is deleted and
/// the only two producers of that string left in the binary
/// (`crates/vike-cli/src/cmd/data.rs`'s `--source` and `--format` refusals) need a FLAG, which none
/// of these four lines passes — so no code path this loop can reach could produce the needle, and a
/// future PR re-stubbing a group with any other wording would have passed it in silence. What holds
/// the claim instead is stated positively and CAN fail: a group that answers renders ITS OWN verb
/// roster under ITS OWN command label.
///
/// Two claims, each able to go red alone:
///
/// 1. a group that ANSWERS answers as ITSELF — its own diagnostic label, and its own verb roster
///    RENDERED into the refusal. A stub names no verbs and carries the plane's label, and a group
///    routed into a SIBLING's parser carries the sibling's;
/// 2. a group nobody has heard of is a DIFFERENT answer, so claim 1 cannot be passing because
///    everything errors alike.
#[test]
fn a_group_that_answers_never_reads_as_designed_but_unbuilt() {
    let scratch = tempfile::tempdir().expect("tempdir");

    // 1. THE REGRESSION GUARD. `data <group>` with no verb is the narrowest line that reaches each
    // group's own entry point, which is the exact call a stub used to serve.
    //
    // ⚠ The LABEL column is `data` for `hist` alone, and that asymmetry is real rather than an
    // oversight: `crate::cmd::data`'s own `parse` IS the `hist` parser (`run` routes the other
    // three to their modules above it), so a `hist` diagnostic is the plane's by construction.
    // The SENTINEL verb is a literal for the reason the roster above this test is one — these
    // rosters are private consts in three modules a test process cannot name. ⚠ It is not by
    // itself a proof of WHICH group answered: `source`'s two verbs are both `catalog`'s as well,
    // so there is no unique word to pick there. The LABEL is what carries that half, on all four.
    for (built, label, sentinel) in [
        ("hist", "data", "fetch"),
        ("catalog", "data catalog", "refresh"),
        ("realtime", "data realtime", "watch"),
        ("source", "data source", "ls"),
    ] {
        let out = run(scratch.path(), &["data", built]);
        let err = stderr(&out);
        // The DIAGNOSTIC line rather than the first line of stderr: `exit_for_parse_error` prints
        // `vike-cli <command>: <msg>` and then the usage page under it, and a startup warning ahead
        // of either is a thing this binary is allowed to emit. ⚠ The SPACE in the needle is what
        // tells the two apart — a startup line is `vike-cli:` (`crates/vike-cli/src/boot.rs`'s
        // `settings_warning_lines`), a command diagnostic is `vike-cli <command>:`.
        let first = err
            .lines()
            .find(|l| l.starts_with("vike-cli "))
            .unwrap_or_else(|| panic!("`data {built}` must print a diagnostic: {err}"));
        assert!(
            first.starts_with(&format!("vike-cli {label}:")),
            "`data {built}` must answer under its own label `{label}`: {err}"
        );
        // The roster the refusal RENDERS, read back out of the sentence — the same derivation the
        // bare-`data` case above uses, and the half a stub could not produce at all.
        let roster = first
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map(|(inner, _)| inner.split('|').map(str::trim).collect::<Vec<_>>())
            .unwrap_or_else(|| panic!("`data {built}` must RENDER its verb roster: {first}"));
        assert!(
            roster.contains(&sentinel),
            "`data {built}`'s roster must name its own verb `{sentinel}`: {first}"
        );
        assert!(
            !roster.contains(&built),
            "that is the GROUP roster rather than `{built}`'s verbs — this line was answered one \
             rung too high: {first}"
        );
        assert_eq!(out.status.code(), Some(2), "a group with no verb is the USAGE rung: {err}");
    }

    // 2. An unknown group is a DIFFERENT answer, so the loop above cannot be passing by everything
    // erroring identically.
    let out = run(scratch.path(), &["data", "nosuchgroup"]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("unknown"), "an unknown group is not a designed one: {err}");
    assert!(!err.contains("designed but not built"), "{err}");

    // ⚠ This block asserted that `data realtime record ls` was refused as "designed and not built
    // (§11.1)". The verb group SHIPPED on 2026-09-22, so the assertion is REPLACED rather than
    // deleted: what has to hold now is that the word reaches its own group and gets that group's
    // own answer — here, the store refusal, because this scratch directory holds no settings
    // database. A `record` that had fallen back to the parent parser would answer `unknown verb`
    // on the USAGE rung instead, which is the regression this keeps watching for.
    let out = run(scratch.path(), &["data", "realtime", "record", "ls"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "it reaches the STORE, not the parser: {}",
        stderr(&out)
    );
    let err = stderr(&out);
    assert!(err.contains("vike-cli data realtime record:"), "under its own label: {err}");
    assert!(!err.contains("designed and not built"), "the group is BUILT: {err}");

    // ...and the DESIGNED-and-unbuilt half that remains is its REMOTE route, refused by name.
    let out = run(scratch.path(), &["data", "realtime", "record", "ls", "--addr", "1.2.3.4:9"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("designed and not built"), "it must not read as a typo: {err}");
    assert!(err.contains("0081"), "…and must point at the argument: {err}");
}
