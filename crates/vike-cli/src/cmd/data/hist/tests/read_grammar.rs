//! The read verbs' grammar: defaults, flags, `universe`'s bounds and the refusals between verbs.

use super::*;

// ---- the read half: the grammar ----

#[test]
fn the_read_subcommands_default_the_addr_and_take_the_filter_flags() {
    let a = parse_of(&["hist", "ls"]).unwrap();
    assert_eq!(a.addr, DEFAULT_ADDR);
    assert_eq!(a.filter, Filter::default());
    assert!(!a.gaps && !a.class && !a.partial_only && !a.json);

    let a = parse_of(&[
        "hist",
        "ls",
        "--addr",
        "1.2.3.4:9",
        "--kind",
        "bar",
        "--venue",
        "binance",
        "--name",
        "BTC",
        "--class",
        "--json",
    ])
    .unwrap();
    assert_eq!(a.addr, "1.2.3.4:9");
    assert_eq!(a.filter.kind.as_deref(), Some("bar"));
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    assert_eq!(a.filter.name.as_deref(), Some("BTC"));
    assert!(a.class && a.json && !a.gaps, "`ls` never probes gaps");

    // ...and the same filters on `gaps`, where the probe is on because the VERB is.
    let a = parse_of(&["hist", "gaps", "--kind", "bar", "--venue", "binance", "--name", "BTC"])
        .unwrap();
    assert_eq!(a.filter.kind.as_deref(), Some("bar"));
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
    assert_eq!(a.filter.name.as_deref(), Some("BTC"));
    assert!(a.gaps && !a.class, "the verb arms the probe, and only the probe");

    let a = parse_of(&["hist", "coverage", "--venue", "binance", "--partial-only"]).unwrap();
    assert!(a.partial_only);
    assert_eq!(a.filter.venue.as_deref(), Some("binance"));
}

/// The bare booleans reject an inline value, on the same rung every valueless flag in this
/// crate uses.
#[test]
fn the_read_booleans_take_no_value() {
    assert!(parse_of(&["hist", "ls", "--class=1"]).unwrap_err().contains("--class"));
    assert!(parse_of(&["hist", "ls", "--json=1"]).unwrap_err().contains("--json"));
    assert!(
        parse_of(&["hist", "coverage", "--partial-only=yes"])
            .unwrap_err()
            .contains("--partial-only")
    );
}

/// `--class` is accepted on `ls` ALONE, and every other subcommand refuses it BY NAME with
/// the reason that subcommand has — never silently ignores it.
///
/// ⚠ The three read siblings are the interesting rows and they do NOT share a reason, which is
/// why each is asserted on its own words rather than on "is an error": `coverage` refuses it
/// because its roster is the TICK kinds and a bar-only instrument is in none of them;
/// `tape-health` because every finding it makes is folded from the one inventory it already
/// fetched; `universe` because its cells are as-of a window the operator wrote down and this
/// probe is as-of now.
#[test]
fn the_class_flag_belongs_to_list_alone_and_every_refusal_says_why() {
    assert!(parse_of(&["hist", "ls", "--class"]).unwrap().class);

    let err = |args: &[&str]| parse_of(args).unwrap_err();

    // ⚠ The SIBLING that is easiest to get wrong: `gaps` takes the same filters as `ls` and
    // deliberately not this annotation — every line it prints is about an ABSENCE.
    let gaps = err(&["hist", "gaps", "--class"]);
    assert!(gaps.contains("--class"), "{gaps}");
    assert!(gaps.contains("does NOT"), "the absence reason: {gaps}");
    assert!(gaps.contains("data hist ls --class"), "…and where to go: {gaps}");

    let coverage = err(&["hist", "coverage", "--class"]);
    assert!(coverage.contains("--class"), "{coverage}");
    assert!(coverage.contains("TICK kinds"), "the MEASURED reason: {coverage}");
    assert!(coverage.contains("data hist ls --class"), "…and where to go: {coverage}");

    let health = err(&["hist", "health", "--class"]);
    assert!(health.contains("inventory"), "{health}");

    let uni = err(&["hist", "universe", "--class"]);
    assert!(uni.contains("as of NOW") || uni.contains("as of the window"), "{uni}");

    for args in [
        &["hist", "fetch", "binance:BTCUSDT:1h", "--days", "2", "--class"][..],
        &["hist", "fetch", "--source", "demo", "--class"][..],
        &["hist", "fetch", "--source", "starter", "--class"][..],
        &["hist", "export", "demo:X:1h", "--out", "o.parquet", "--class"][..],
        &["hist", "rm", "--kind", "bar", "--venue", "demo", "--class"][..],
        &[
            "hist",
            "repair",
            "--kind",
            "bar",
            "--venue",
            "demo",
            "--symbol",
            "X",
            "--interval",
            "1h",
            "--class",
        ][..],
    ] {
        let e = err(args);
        assert!(e.contains("--class"), "{args:?} must refuse it by name: {e}");
    }
}

/// ⚠ **THE refusal this module exists to make loud.** `--store` on a read verb is the mistake
/// an operator makes first, because the sibling subcommand takes one — and a silently-ignored
/// `--store` would answer about a completely different store with no sign that it had. The
/// message must name the flag, the verb, and the flag that reaches the other store.
#[test]
fn a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store() {
    // ⚠ EVERY read subcommand, not just the two that shipped first. A verb that inherited
    // `is_read()` without inheriting this refusal would accept `--store` and answer about a
    // completely different store, which is the exact failure this test is named for.
    let mut exercised = 0;
    for sub in SUBCOMMANDS.iter().filter(|s| s.is_read()) {
        for (flag, value) in [("--store", "/srv/hist"), ("--engine", "/opt/backtest")] {
            let argv = ["hist", sub.as_str(), flag, value];
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("--addr"), "{argv:?} must point at --addr: {err}");
        }
        // ⚠ `--store` meets the ONE sentence every history reader prints — the same one
        // `export` prints, never a hand-spelled copy of its replacement — in every spelling,
        // including a trailing one with no directory, which would otherwise be asked for a
        // value that is then refused. The shared sentence LEADS, so it names the verb as typed.
        let shared = vike_datahub_client::flag_vocab::store_flag_removed(&format!(
            "data hist {}",
            sub.as_str()
        ));
        for store in [&["--store", "/srv/hist"][..], &["--store=/srv/hist"][..], &["--store"][..]] {
            let argv = [&["hist", sub.as_str()][..], store].concat();
            let err = parse_of(&argv).unwrap_err();
            assert!(err.starts_with(&shared), "{argv:?} must lead with the shared sentence: {err}");
            assert!(err.contains("VIKE_DATAHUB_STORE=DIR vike-backend datahub"), "{err}");
            assert!(!err.contains("docs/"), "no withheld path: {err}");
        }
        exercised += 1;
    }
    assert!(exercised >= 5, "the read roster lost its rows: {exercised}");
    // …and `export` gets the shared sentence EXACTLY, with no `--addr` clause: its `--addr`
    // selects a different route rather than a different source — see [`store_refusal`].
    assert_eq!(
        parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--store", "/s"]).unwrap_err(),
        vike_datahub_client::flag_vocab::store_flag_removed("data hist export")
    );
}

/// ⚠ **The WINDOW refusal moved out of the test above, and the move is the point.** `--days`
/// and `--from`/`--to` used to ride the store-side refusal and so were asserted to name
/// `--addr` — which was never apt for them: neither flag names a store, and pointing an
/// operator at `--addr` answers a question they did not ask. They bound a FETCH, and the
/// verbs that refuse them fold each series' WHOLE recorded span, so the useful thing to name
/// is the read verb whose question IS a window. See [`Sub::refuses_a_window`].
#[test]
fn a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one() {
    for sub in SUBCOMMANDS.iter().filter(|s| s.refuses_a_window()) {
        for (flag, value) in [("--days", "7"), ("--from", "0"), ("--to", "0")] {
            let argv = ["hist", sub.as_str(), flag, value];
            let err = parse_of(&argv).unwrap_err();
            assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
            assert!(err.contains("universe"), "{argv:?} must name the verb: {err}");
        }
    }
}

/// …and `universe` ACCEPTS the pair, parses both bounds HERE, and keeps each one independent.
/// The parse is what makes it different from `export`'s forwarded strings — see
/// [`membership_window`].
#[test]
fn universe_takes_an_independent_pair_of_bounds_and_parses_them_here() {
    let a = parse_of(&["hist", "universe"]).unwrap();
    assert_eq!(
        a.universe_window,
        Some(universe::MembershipWindow::default()),
        "a bare `universe` is unbounded, not an error — the store's own span is the window"
    );

    let a = parse_of(&["hist", "universe", "--from", "2026-01-01"]).unwrap();
    let window = a.universe_window.expect("universe always carries a window");
    assert_eq!(window.from, Some(1_767_225_600_000), "YYYY-MM-DD resolves to UTC midnight");
    assert_eq!(window.to, None, "one bound stands alone, as on `export`");

    let a = parse_of(&["hist", "universe", "--to", "0"]).unwrap();
    let window = a.universe_window.expect("universe always carries a window");
    assert_eq!(window.from, None);
    assert_eq!(window.to, Some(0), "a bare epoch-ms stays an epoch-ms, including zero");
}

/// An unreadable bound is a USAGE error here rather than a silently-discarded flag, because
/// nothing is forwarded: the comparison happens in this process.
#[test]
fn an_unreadable_universe_bound_is_refused_by_name() {
    let err = parse_of(&["hist", "universe", "--from", "last-tuesday"]).unwrap_err();
    assert!(err.contains("--from"), "{err}");
    assert!(err.contains("last-tuesday"), "the message names what was typed: {err}");
    assert!(err.contains("YYYY-MM-DD"), "…and the spellings it wanted: {err}");
}

/// An INVERTED window is refused rather than swapped: it would report every instrument in the
/// store `absent`, which reads exactly like an empty store.
#[test]
fn an_inverted_universe_window_is_refused_rather_than_swapped() {
    let err =
        parse_of(&["hist", "universe", "--from", "2026-06-01", "--to", "2026-01-01"]).unwrap_err();
    assert!(err.contains("--from") && err.contains("--to"), "{err}");
    assert!(err.contains("absent"), "…and what it would have produced: {err}");
}

/// `--days` is refused on `universe` for a reason of its OWN, and the message has to carry it:
/// a window counted back from now answers a different question every day it is run, and a
/// point-in-time universe exists to be re-askable.
#[test]
fn days_is_refused_on_universe_with_the_re_askability_reason() {
    let err = parse_of(&["hist", "universe", "--days", "30"]).unwrap_err();
    assert!(err.contains("--days"), "{err}");
    assert!(err.contains("--from"), "…and the spelling that works: {err}");
    assert!(err.contains("NOW"), "…and why: {err}");
}

/// The two new read verbs refuse the ABSENCE flag, and the message says which verb answers
/// absence — `health`'s whole distinction is present-and-impossible versus missing.
#[test]
fn the_new_read_verbs_refuse_the_absence_flags_and_name_the_verbs_that_answer_absence() {
    for sub in ["health", "universe"] {
        let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
        assert!(err.contains("--partial-only"), "{sub}: {err}");
        assert!(err.contains("hist gaps"), "{sub} must name the verb that answers: {err}");
    }
}

/// **THE OUTPUT DOOR.** `--format` is the axis, `--json` is its shorthand, the two are refused
/// only where they DISAGREE, and every value this plane names but this verb does not serve is
/// refused BY NAME.
///
/// ⚠ **[`UNBUILT_FORMATS`] IS EMPTY NOW, and the loop over it was therefore VACUOUS** — it
/// asserted three things about every row of a roster with no rows, which passes whatever the
/// refusals say. That is the shape this suite exists to catch, so the roster's emptiness is
/// asserted as a CLAIM (with the reason a row would be added back) and each of the three
/// departed values is exercised through the arm that actually refuses it:
///
/// * `jsonl` — BUILT, on [`ROW_VERB`]. Refused here because a catalog is not rows.
/// * `csv`/`parquet` — BUILT, on [`FILE_VERB`]. Refused here because this verb PRINTS.
///
/// None of the three may read as "waiting on a phase", which is what each of them said once.
#[test]
fn the_format_axis_carries_json_and_refuses_the_other_verbs_formats_by_name() {
    assert!(!parse_of(&["hist", "ls"]).unwrap().json, "table is the default");
    assert!(parse_of(&["hist", "ls", "--format", "json"]).unwrap().json);
    assert!(!parse_of(&["hist", "ls", "--format", "table"]).unwrap().json);
    assert!(parse_of(&["hist", "ls", "--json"]).unwrap().json, "the shorthand still works");
    assert!(
        parse_of(&["hist", "ls", "--json", "--format", "json"]).unwrap().json,
        "agreeing spellings are not a contradiction"
    );

    // ...and the ONE way they can disagree is refused rather than resolved.
    let err = parse_of(&["hist", "ls", "--json", "--format", "table"]).unwrap_err();
    assert!(err.contains("--json") && err.contains("--format table"), "{err}");
    assert!(err.contains("pass one"), "…and what to do: {err}");

    // THE ROSTER'S EMPTINESS AS A CLAIM. Every value it once held is now written by some verb
    // of this plane, so there is nothing left that is "designed but not built". A row added
    // back is a value nothing writes, and it gets the arm that names what it is waiting on.
    assert!(
        UNBUILT_FORMATS.is_empty(),
        "every value this roster held is now WRITTEN by a verb; a row here must name what it \
             is waiting on: {UNBUILT_FORMATS:?}"
    );
    // ...but the loop over it is vacuous, so each departed value is exercised through its own
    // arm — and none of the three may read as waiting on a phase.
    for (name, needle) in [("jsonl", ROW_VERB), ("csv", FILE_VERB), ("parquet", FILE_VERB)] {
        let err = parse_of(&["hist", "ls", "--format", name]).unwrap_err();
        assert!(err.contains(name), "{name}: {err}");
        assert!(err.contains(needle), "{name} must name the verb that serves it: {err}");
        assert!(!err.contains("not built"), "{name} SHIPS: {err}");
        // ⚠ …and must not send the operator to a LOCAL store for it: `export` reads through a
        // datahub on both routes since 2026-09-26 (decision 0084's amendment), and this
        // message said `parquet` came "from a store on this machine" for a day after that.
        if needle == FILE_VERB {
            assert!(!err.contains("on this machine"), "{name}: no local-store route: {err}");
            assert!(err.contains("datahub"), "{name}: names where export reads: {err}");
        }
    }
    // ⚠ THE ANTI-VACUITY CONTROL for all three at once: each is genuinely ACCEPTED somewhere,
    // so the refusals above are about THIS verb rather than about a value nothing serves.
    parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", "jsonl"])
        .expect("`get` is the verb that serves jsonl on stdout");
    parse_of(&[
        "hist", "export", "d:S:1h", "--out", "o", "--addr", "h:1", "--format", "csv", "--from",
        "1", "--to", "2",
    ])
    .expect("`export --addr` is the verb that writes csv");
    parse_of(&["hist", "export", "d:S:1h", "--out", "o", "--format", "parquet"])
        .expect("`export` is the verb that writes parquet");

    // An unrecognised one IS a spelling mistake, and says so — unlike `--source`, whose roster
    // lives in a process this crate cannot see.
    let err = parse_of(&["hist", "ls", "--format", "yaml"]).unwrap_err();
    assert!(err.contains("unknown") && err.contains("table | json"), "{err}");
    assert!(parse_of(&["hist", "ls", "--format", ""]).unwrap_err().contains("EMPTY"));

    // The axis is on EVERY verb, not just the read half — a `fetch` report is a document too.
    assert!(parse_of(&["hist", "fetch", "--source", "demo", "--format", "json"]).unwrap().json);
}

/// **THE PROMOTION.** `gaps` is a verb, `--gaps` is refused on EVERY verb by name, and the
/// refusal names the verb that replaced it.
///
/// ⚠ The `ls` row is the one that matters. A refusal that fired only on the siblings would
/// leave the ONE verb that used to accept the flag answering `unknown option '--gaps'` — which
/// tells an operator the flag never existed and sends them to check their spelling. Every verb
/// is asserted rather than a sample, for [`RETIRED_SPELLINGS`]'s reason one layer up.
#[test]
fn gaps_is_a_verb_and_the_retired_flag_is_refused_everywhere_by_name() {
    let a = parse_of(&["hist", "gaps"]).unwrap();
    assert_eq!(a.sub, Sub::Gaps);
    assert!(a.gaps, "the verb IS the probe");
    assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so it resolves a datahub");

    for sub in SUBCOMMANDS {
        // Each verb needs its own minimum argv before the flag can be judged, so the refusal
        // is reached with the line that verb would otherwise accept.
        let mut argv: Vec<&str> = match sub {
            Sub::Fetch => vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "1"],
            Sub::Export => vec!["hist", "export", "demo:X:1h", "--out", "o.parquet"],
            Sub::Rm => vec!["hist", "rm", "--kind", "bar", "--venue", "demo"],
            Sub::Repair => {
                vec!["hist", "repair", "--kind", "bar", "--venue", "demo", "--symbol", "X"]
            }
            other => vec!["hist", other.as_str()],
        };
        argv.push("--gaps");
        let err = parse_of(&argv).unwrap_err();
        assert!(!err.contains("unknown option"), "{argv:?} must not read as a typo: {err}");
        assert!(err.contains("--gaps"), "{argv:?} must name the flag: {err}");
        assert!(
            err.contains("data hist gaps"),
            "{argv:?} must name the verb that replaced it: {err}"
        );
    }
}

/// `--kind` is ACCEPTED on `tape-health` and on `universe` while `coverage` refuses it, and the
/// asymmetry is deliberate: a coverage row IS the join across kinds, while a universe narrowed
/// to `--kind bar` is exactly the set a bar-driven profile reads.
#[test]
fn kind_narrows_the_new_read_verbs_while_coverage_still_refuses_it() {
    for sub in ["health", "universe"] {
        let a = parse_of(&["hist", sub, "--kind", "bar"]).unwrap();
        assert_eq!(a.filter.kind.as_deref(), Some("bar"), "{sub}");
    }
    assert!(parse_of(&["hist", "coverage", "--kind", "bar"]).is_err());
}

/// …and the mirror image: a datahub flag on the write half is refused rather than ignored,
/// naming the half it belongs to. Ignoring one would let `data fetch --addr the CI box:7878` read
/// as "fetch into the remote store", which is not a thing this verb can do.
///
/// ⚠ **`export` used to be in this loop and has been split out, deliberately.** `--addr` and
/// `--kind` are that verb's own now (the route switch and the row shape), so the blanket
/// sentence would be a true no for a false reason. The LISTING aids still are foreign there and
/// still refuse — with a sentence of their own, which is what the second half asserts.
///
/// ⚠ **`--addr` LEFT this loop too, on `fetch` ITSELF — D1 of the 0094 follow-ups, and the same
/// shape as `export`'s departure above.** It used to share this loop's read-half sentence on
/// every source, `--source starter|demo` included — the "`fetch` KEEPS both of the rows `export`
/// took back" comment this replaced was the control for that claim, and D1 is exactly what makes
/// it false: a VENUE fetch's `--addr` is now its own route (it never reaches this refusal, or any
/// refusal, at all — see `a_venue_fetch_keeps_addr_and_an_engine_source_still_refuses_it`), and
/// `--source starter|demo` refuses it with a DIFFERENT sentence of its own (that source has no
/// REMOTE route, not that the flag belongs to the read half) — asserted separately below, the
/// same split the export section below argues for its own two rows.
#[test]
fn a_datahub_flag_on_a_write_verb_is_refused_and_names_the_read_half() {
    for (argv, flag) in [
        (vec!["hist", "fetch", "--source", "demo", "--class"], "--class"),
        (vec!["hist", "fetch", "--source", "demo", "--venue", "binance"], "--venue"),
        (vec!["hist", "fetch", "binance:BTCUSDT:1h", "--days", "7", "--name", "BTC"], "--name"),
        (vec!["hist", "fetch", "--source", "demo", "--kind", "bar"], "--kind"),
    ] {
        let err = parse_of(&argv).unwrap_err();
        assert!(err.contains(flag), "{argv:?} must name {flag}: {err}");
        assert!(err.contains("READ half"), "{argv:?} must name the half it belongs to: {err}");
    }

    // `--addr` on an engine source is still refused, but no longer with the read half's
    // sentence — see the doc comment above for why, and
    // `a_venue_fetch_keeps_addr_and_an_engine_source_still_refuses_it` for the VENUE half of the
    // split this is the other half of.
    let err = parse_of(&["hist", "fetch", "--source", "demo", "--addr", "h:1"]).unwrap_err();
    assert!(err.contains("--addr") && err.contains("REMOTE"), "{err}");
    // ANTI-VACUITY: it is a DIFFERENT sentence, which is the whole point of the split.
    assert!(
        !err.contains("READ half"),
        "an engine source's --addr refusal must not borrow the read half's reason: {err}"
    );

    // `export` refuses the four LISTING aids too, with the sentence that is true of IT.
    for flag in ["--venue", "--name"] {
        let err = parse_of(&["hist", "export", "d:S:1h", "--out", "o", flag, "x"]).unwrap_err();
        assert!(err.contains(flag), "{flag}: {err}");
        assert!(err.contains("names ONE series"), "{flag}: {err}");
        // ANTI-VACUITY: it is a DIFFERENT sentence, which is the whole point of the split.
        assert!(!err.contains("READ half"), "{flag} must not inherit fetch's reason: {err}");
    }
}

/// The intra-half refusals, each about a UNIT rather than about tidiness — see the arms in
/// [`parse`]. `--kind` on `coverage` would filter away the disagreement the report exists to
/// show; `--partial-only` on a per-series listing has no cross-kind verdict to filter.
#[test]
fn each_read_verb_refuses_the_others_flag_with_the_reason() {
    for sub in ["ls", "gaps"] {
        let err = parse_of(&["hist", sub, "--partial-only"]).unwrap_err();
        assert!(err.contains("--partial-only") && err.contains("coverage"), "{sub}: {err}");
    }

    let err = parse_of(&["hist", "coverage", "--kind", "trade"]).unwrap_err();
    assert!(err.contains("--kind") && err.contains("across kinds"), "{err}");
}

/// A colon-string on a read verb is refused with the REASON: a stored series is four
/// dimensions with an alternative inside them, and no `VENUE:SYMBOL:INTERVAL` can spell one.
/// It is the single likeliest thing to type after using `fetch`.
#[test]
fn a_read_verb_refuses_a_fetch_shaped_spec_and_says_why() {
    let err = parse_of(&["hist", "ls", "binance:BTCUSDT:1h"]).unwrap_err();
    assert!(err.contains("binance:BTCUSDT:1h"), "the message names what was typed: {err}");
    assert!(err.contains("--kind"), "…and the flags that do narrow a listing: {err}");
}
