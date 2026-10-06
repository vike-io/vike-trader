//! `get`, the ROW verb: its window, its row ceiling and its formats; then the USAGE roster.

use super::*;

// ── §8.2: `get`, the ROW verb ───────────────────────────────────────────────────────────

/// **§8.2 RULE 1 REACHES THE GRAMMAR.** A `get` with no window is refused BY NAME, and the
/// three flag spellings that ARE a window all parse.
///
/// ⚠ The refusal lives in [`get::parse_window`] and is unit-tested there; what this case adds
/// is that the flags REACH it — `--days`/`--from`/`--to` survive the read half's
/// [`Sub::refuses_a_window`] refusal, which every other read verb but `universe` trips.
#[test]
fn get_requires_a_window_and_the_window_flags_reach_it() {
    let err = parse_of(&["hist", "get", "binance:BTCUSDT:1h"]).expect_err("no window");
    assert!(err.contains("get needs a window"), "{err}");
    for window in [vec!["--days", "7"], vec!["--from", "2026-01-01"], vec!["--to", "2026-02-01"]] {
        let mut v = vec!["hist", "get", "binance:BTCUSDT:1h"];
        v.extend_from_slice(&window);
        let a = parse_of(&v).unwrap_or_else(|e| panic!("{window:?}: {e}"));
        assert_eq!(a.sub, Sub::Get);
        assert!(a.get.is_some(), "{window:?} must build a GetArgs");
        assert!(a.spec.is_none(), "the spec is MOVED into GetArgs, never left in both");
    }
}

/// The RESOLVED defaults `GetArgs` promises: the ceiling folded in, the fold recorded, and the
/// rendering decided — so nothing downstream re-decides one.
#[test]
fn get_args_arrive_resolved_with_the_ceiling_folded_in() {
    let a = parse_of(&["hist", "get", "binance:BTCUSDT:1h", "--days", "7"]).expect("a line");
    let g = a.get.as_ref().expect("a GetArgs");
    assert_eq!(g.limit, get::ROW_CEILING);
    assert!(g.limit_defaulted, "nobody typed --limit, and the disclosure has to know");
    assert_eq!(g.render, get::Render::Table, "the plane's default");
    assert_eq!(g.window, get::Window::Days(7));
    assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");

    let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "5", "--json"])
        .expect("a line");
    let g = a.get.as_ref().expect("a GetArgs");
    assert_eq!(g.limit, 5);
    assert!(!g.limit_defaulted, "a named limit says so rather than reading as the default");
    assert_eq!(g.render, get::Render::Json);
    assert!(a.json, "…and `Args::json` is its PROJECTION, not a second decision");
}

/// **`jsonl` is a THIRD state and `Args::json` has two**, so the render is the authority for
/// this verb — the reason [`GetArgs::render`] exists at all. A `jsonl` run must not be
/// indistinguishable from a `table` one downstream.
#[test]
fn a_jsonl_get_is_not_a_json_get_and_not_a_table_one_either() {
    let line = |f: &str| {
        parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--format", f])
            .unwrap_or_else(|e| panic!("{f}: {e}"))
    };
    let jsonl = line("jsonl");
    assert_eq!(jsonl.get.as_ref().expect("a GetArgs").render, get::Render::Jsonl);
    assert!(!jsonl.json, "jsonl is NOT the one-document form");
    // THE CONTROLS: the two states `Args::json` CAN express, so the assertion above is about
    // `jsonl` being a third rather than about the field always being false.
    assert!(!line("table").json);
    assert!(line("json").json);

    // ...and because it is a third state, `--json --format jsonl` is a SECOND way the two
    // spellings can disagree — refused with a sentence of its own rather than resolved, for
    // the same reason `--json --format table` is.
    let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
        .expect_err("a sequence is not one document");
    assert!(err.contains("SEQUENCE"), "{err}");
    assert!(err.contains("Pass one"), "…and what to do: {err}");
    // THE CONTROL: the AGREEING pair is not a contradiction, so the refusal is about the
    // values rather than about naming both spellings at all.
    assert!(
        parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "json"])
            .expect("agreeing spellings")
            .json
    );
}

/// **The `jsonl` contradiction is [`Sub::Get`]'s ALONE, and on every other verb the answer is
/// the one the sibling group already gives.**
///
/// ⚠ It shipped UNCONDITIONAL, above the verb dispatch, so `data hist ls --json --format
/// jsonl` answered with a sentence describing GET's document and ending "Pass one" — advice
/// that is false there, since dropping `--json` leaves a `--format jsonl` `ls` also refuses.
/// Meanwhile `data catalog ls --json --format jsonl` answered with [`ROW_VERB`], because that
/// parser reads `--format` eagerly and its contradiction check never sees a `jsonl`. Two
/// groups, one plane, one question, two answers — which is exactly what
/// `catalog`'s `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words` holds
/// for the `table` spelling and nothing held for this one.
#[test]
fn a_jsonl_contradiction_on_a_catalog_verb_names_the_row_verb_in_both_groups() {
    let hist = parse_of(&["hist", "ls", "--json", "--format", "jsonl"])
        .expect_err("a catalog verb emits no rows");
    assert!(hist.contains(ROW_VERB), "it must name where `jsonl` works: {hist}");
    assert!(
        !hist.contains("SEQUENCE"),
        "…and must NOT hand over `get`'s document sentence, whose advice is false here: {hist}"
    );
    // ⚠ The CROSS-GROUP half of this pairing lives in `catalog`'s tests
    // (`the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words`) and not here,
    // for a visibility reason rather than a taste one: `catalog::parse` is private to its own
    // module, so only a descendant can reach BOTH parsers.
    //
    // ...and it must not merely be refused: dropping `--json` has to leave the SAME answer,
    // because the arm above is gone on this verb and the format parser is what refuses it.
    let without = parse_of(&["hist", "ls", "--format", "jsonl"]).expect_err("still refused");
    assert_eq!(hist, without, "`--json` must not change what `--format jsonl` means on `ls`");
    // THE CONTROL: the same pair on the ROW verb DOES get the document sentence, so the
    // assertions above are about the VERB rather than about the arm being dead.
    let get = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"])
        .expect_err("a sequence is not one document");
    assert!(get.contains("SEQUENCE"), "{get}");
    assert_ne!(get, hist, "the two verbs answer with different sentences");
}

/// **THE ACCOUNT-KIND REFUSAL, and the ORDER it is applied in.** `--kind` does not apply to
/// `get` at all — but for an ACCOUNT kind the §9.3.2 sentence comes FIRST, because "this plane
/// does not serve your fills" outranks "that flag belongs elsewhere".
#[test]
fn an_account_kind_on_get_is_the_plane_s_refusal_and_not_a_flag_note() {
    for kind in vike_model::ACCOUNT_KINDS {
        let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", kind])
            .expect_err("account data is not this plane's");
        assert!(err.contains("ACCOUNT data"), "{kind}: {err}");
        assert!(err.contains("vike-cli account"), "…and where it will live: {kind}: {err}");
    }
    // THE CONTROL: a MARKET kind gets the flag-placement answer instead, so the refusal above
    // is about the kind rather than about `--kind` being refused with one sentence for all.
    let err = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--kind", "trade"])
        .expect_err("`get` reads bars");
    assert!(!err.contains("ACCOUNT data"), "{err}");
    assert!(err.contains("reads BARS"), "{err}");
    assert!(err.contains("data hist ls"), "…and names where a kind IS a filter: {err}");
}

/// The LISTING flags are refused on `get` by name, and `--limit` is refused everywhere else by
/// name — both directions of the one rule that this verb's noun is a ROW and its siblings' is a
/// series.
///
/// ⚠ **The `--limit` half DERIVES its roster**, as its two neighbours upfile already do
/// ([`a_store_side_flag_on_a_read_verb_is_refused_and_names_the_other_store`],
/// [`a_window_on_a_whole_span_read_verb_is_refused_and_names_the_verb_that_takes_one`]). It
/// shipped hand-typed and three verbs short — `gate`, `rm` and `repair` were never exercised.
/// The production guard is `sub != Sub::Get`, so it was right for them anyway; what was wrong
/// was the PROOF, which would have stayed green through a new [`SUBCOMMANDS`] row or a guard
/// refined into an enumeration.
///
/// ⚠ …and it asserts the RENDERED [`ROW_VERB`], not the words inside it. `contains("data hist
/// get")` is satisfied by a stale hand copy, which is the drift the const exists to stop.
#[test]
fn the_row_flag_and_the_listing_flags_are_refused_on_each_other() {
    for (flag, value) in [("--venue", Some("binance")), ("--name", Some("BTC")), ("--class", None)]
    {
        let mut v = vec!["hist", "get", "d:S:1h", "--days", "1", flag];
        v.extend(value);
        let err = parse_of(&v).expect_err(flag);
        assert!(err.contains(flag), "{flag}: {err}");
        assert!(err.contains("data hist ls"), "{flag} must name the listing verb: {err}");
    }
    let mut refused = 0usize;
    for sub in SUBCOMMANDS.iter().filter(|s| **s != Sub::Get) {
        let err = parse_of(&["hist", sub.as_str(), "--limit", "10"]).expect_err(sub.as_str());
        assert!(err.contains("--limit"), "{}: {err}", sub.as_str());
        assert!(err.contains(ROW_VERB), "{} must RENDER the row verb: {err}", sub.as_str());
        refused += 1;
    }
    // Anti-vacuity: a `SUBCOMMANDS` that had lost its rows, or a filter that kept none, would
    // leave the loop above asserting nothing at all.
    assert_eq!(refused, SUBCOMMANDS.len() - 1, "every verb but `get` was exercised");
    assert!(refused > 5, "and there are enough of them for that to mean something");
    // THE CONTROL: `get` itself TAKES the flag, so the refusals above are about the verb
    // rather than about `--limit` being rejected wherever it appears.
    let a = parse_of(&["hist", "get", "d:S:1h", "--days", "1", "--limit", "10"])
        .expect("`get` is the verb that bounds rows");
    assert_eq!(a.get.as_ref().expect("a GetArgs").limit, 10);
}

/// The usage roster names every subcommand and every flag the parser accepts — `crate::cmd::
/// mcp`'s `the_instructions_name_only_real_commands` reads this text to check the MCP
/// surface's `vike-cli data …` mentions, so a subcommand missing from it makes that check
/// unable to confirm a command that does exist.
#[test]
fn the_usage_names_every_subcommand_and_flag_this_parser_accepts() {
    for sub in SUBCOMMANDS {
        assert!(USAGE.contains(sub.as_str()), "USAGE must name {}", sub.as_str());
    }
    for token in [
        "--out",
        "--produced-by",
        "--dry-run",
        "--yes",
        "--symbol",
        "--group",
        "--interval",
        "--days",
        "--from",
        "--to",
        "--store",
        "--engine",
        "--addr",
        "--kind",
        "--venue",
        "--name",
        "--class",
        "--partial-only",
        "--require-days",
        "--max-gap",
        "--require-kind",
        "--limit",
        "--format",
        "--json",
        "--bars",
        "--verify",
    ] {
        assert!(USAGE.contains(token), "USAGE must name {token}");
    }
    assert!(USAGE.contains(DEFAULT_ADDR), "…and the datahub default it resolves to");
    // ⚠ …and `get`'s ROW CEILING, which this page states as a bare NUMBER twice while
    // [`get::ROW_CEILING`] declares it. A `const &'static str` cannot interpolate, so the
    // duplication is unavoidable and this assertion is what stops it rotting — the same shape
    // the rung above gives `DEFAULT_ADDR`, and the same defect this file has watched a
    // hand-copied count produce more than once.
    assert!(
        USAGE.contains(&get::ROW_CEILING.to_string()),
        "USAGE states the row ceiling as a number and it no longer matches get::ROW_CEILING"
    );
}
