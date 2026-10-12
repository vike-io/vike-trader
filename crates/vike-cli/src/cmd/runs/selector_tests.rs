use super::*;
use crate::exit::Exit;

use super::test_run_at as run_at;
use super::test_scan_of as scan_of;

/// The arity-one entry point with no marks store — every case that does not test a mark. Spelled
/// once so a third parameter does not have to be threaded through twenty call sites.
fn resolve_one<'a>(scan: &'a RunScan, selector: &str) -> Result<&'a ScannedRun, CliError> {
    super::resolve_one(scan, None, selector)
}

fn resolve_many<'a>(
    scan: &'a RunScan,
    selector: Option<&str>,
) -> Result<Vec<&'a ScannedRun>, CliError> {
    super::resolve_many(scan, None, selector)
}

fn three() -> RunScan {
    scan_of(vec![
        run_at("1789213143-8821-00", "backtest"),
        run_at("1789213999-8821-00", "search"),
        run_at("1789300000-9001-00", "backtest"),
    ])
}

#[test]
fn a_full_id_resolves_to_that_directory() {
    let s = three();
    assert_eq!(resolve_one(&s, "1789213999-8821-00").unwrap().run_id, "1789213999-8821-00");
}

#[test]
fn a_unique_short_prefix_resolves() {
    let s = three();
    assert_eq!(resolve_one(&s, "17893").unwrap().run_id, "1789300000-9001-00");
}

/// §4: ambiguity is an exit-`2` refusal NAMING the candidates. A "most recent wins" tiebreak is
/// exactly the silent precedence this grammar exists to refuse.
#[test]
fn an_ambiguous_prefix_is_a_usage_refusal_that_names_every_candidate() {
    let s = three();
    let e = resolve_one(&s, "1789213").expect_err("ambiguous");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains("1789213143-8821-00"), "{}", e.msg);
    assert!(e.msg.contains("1789213999-8821-00"), "{}", e.msg);
    assert!(!e.msg.contains("1789300000-9001-00"), "only the candidates: {}", e.msg);
}

/// A selector naming nothing is a fact about the STORE, not about the command line: the line was
/// well-formed and nothing here can be retyped to fix it.
#[test]
fn a_selector_matching_nothing_is_the_failed_rung() {
    let s = three();
    let e = resolve_one(&s, "does-not-exist").expect_err("no match");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("does-not-exist"), "{}", e.msg);
}

/// `@last` is the LAST in directory-name order, which for a minted id is chronological —
/// `vike_model::runs::RunManifest::run_id` documents the `<unix-seconds>-<pid>-<seq>` shape.
#[test]
fn at_last_is_the_most_recent_run() {
    let s = three();
    assert_eq!(resolve_one(&s, "@last").unwrap().run_id, "1789300000-9001-00");
}

#[test]
fn at_last_of_a_kind_is_the_most_recent_of_that_kind() {
    let s = three();
    assert_eq!(resolve_one(&s, "@last:search").unwrap().run_id, "1789213999-8821-00");
    assert_eq!(resolve_one(&s, "@last:backtest").unwrap().run_id, "1789300000-9001-00");
}

#[test]
fn at_last_of_an_unseen_kind_is_the_failed_rung_naming_the_kind() {
    let s = three();
    let e = resolve_one(&s, "@last:a-kind-nobody-minted").expect_err("no such kind");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("a-kind-nobody-minted"), "{}", e.msg);
}

/// ⚠ **Owner ruling 7: walk-forward is a MODIFIER, not a run kind.** A kind says WHAT was
/// computed (`backtest`, `study`, and `search` from the stage that persists one); how it was
/// VALIDATED — one slice or walked forward — is a second axis on top of that, so `walkforward`
/// is a kind nothing writes and nothing ever will.
///
/// It is still the form an operator types, because `crates/vike-cli/src/lib.rs`'s `COMMANDS`
/// carries a top-level `walkforward` verb. So the miss must SAY why: a bare "no run matching
/// @last:walkforward" reads as an empty store and sends them to re-run something, while a
/// message naming the kinds actually present shows in one line that they asked the wrong axis.
#[test]
fn at_last_walkforward_misses_and_the_message_names_the_kinds_that_exist() {
    let s = three();
    let e = resolve_one(&s, "@last:walkforward").expect_err("not a kind, by ruling 7");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("walkforward"), "{}", e.msg);
    // ⚠ Assert the CLAUSE, not just the word `backtest` — that word is also in the
    // `vike-cli backtest ls` hint, so a `contains("backtest")` alone passes with the clause
    // deleted and pins nothing.
    assert!(e.msg.contains("The kinds here are"), "{}", e.msg);
    assert!(e.msg.contains("search"), "every kind present, deduped: {}", e.msg);
}

/// The kinds-present clause is for the `@last:<kind>` form ALONE. A plain selector that matched
/// nothing is not a question about kinds, and listing them there is noise in the one message an
/// operator reads when a run id is mistyped.
#[test]
fn a_plain_miss_does_not_recite_the_kinds() {
    let s = three();
    let e = resolve_one(&s, "1789999").expect_err("no match");
    assert_eq!(e.exit, Exit::Failed);
    assert!(!e.msg.contains("kinds here"), "{}", e.msg);
}

#[test]
fn at_last_over_an_empty_scan_is_the_failed_rung() {
    let s = scan_of(Vec::new());
    assert_eq!(resolve_one(&s, "@last").expect_err("nothing").exit, Exit::Failed);
}

/// ⚠ The CHILD form is RECOGNISED and refused BY NAME, naming what has to persist first.
/// Treating `<id>#12` as a literal directory name would be the silent divergence §15.6's
/// one-implementation rule exists to stop.
#[test]
fn the_deferred_child_form_is_a_named_refusal_not_a_missing_directory() {
    let s = three();
    let e = resolve_one(&s, "1789213143-8821-00#12").expect_err("deferred");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains('#'), "{}", e.msg);
    assert!(e.msg.contains("search") || e.msg.contains("walk-forward"), "{}", e.msg);
}

/// ⚠ **A MARK resolves to the run it points at, by the same rule a typed id takes.** Both
/// spellings work — §4's table writes the `@`, §7.1's own example omits it — and a `/` cannot
/// occur in a minted run id, so the bare form costs no ambiguity.
#[test]
fn a_mark_resolves_to_the_run_it_points_at_with_or_without_the_at_sign() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    vike_model::runs::write_mark(&marks, "baseline/momentum", "1789213999-8821-00", None, 1)
        .unwrap();
    let s = three();

    for spelling in ["@baseline/momentum", "baseline/momentum"] {
        assert_eq!(
            super::resolve_one(&s, Some(&marks), spelling).unwrap().run_id,
            "1789213999-8821-00",
            "{spelling}"
        );
    }
}

/// A SINGLE-segment mark needs its `@`. Without one, `prod` is indistinguishable from a short id
/// prefix, and guessing between the two is how a gate silently judges the wrong run — so the
/// bare spelling stays a prefix lookup and misses.
#[test]
fn a_single_segment_mark_needs_its_at_sign() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    vike_model::runs::write_mark(&marks, "prod", "1789213999-8821-00", None, 1).unwrap();
    let s = three();

    assert_eq!(super::resolve_one(&s, Some(&marks), "@prod").unwrap().run_id, "1789213999-8821-00");
    let e = super::resolve_one(&s, Some(&marks), "prod").expect_err("a bare word is a prefix");
    assert_eq!(e.exit, Exit::Failed, "…which matches no directory: {}", e.msg);
}

/// ⚠ **DANGLING is not MISSING.** A mark whose run was pruned says which run is gone and tells
/// you to re-point the name; a mark that was never set tells you to create one. Reporting the
/// first as the second sends an operator to set a name that is already there.
#[test]
fn a_mark_whose_run_is_gone_is_dangling_rather_than_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    vike_model::runs::write_mark(&marks, "baseline/gone", "1700000000-1-00", None, 1).unwrap();
    let s = three();

    let e = super::resolve_one(&s, Some(&marks), "@baseline/gone").expect_err("dangling");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("1700000000-1-00"), "names the run that is gone: {}", e.msg);
    assert!(e.msg.contains("baseline/gone"), "…and the mark pointing at it: {}", e.msg);
    assert!(e.msg.contains("pruned") || e.msg.contains("removed"), "{}", e.msg);

    let e = super::resolve_one(&s, Some(&marks), "@baseline/never-set").expect_err("missing");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("no mark named"), "a different sentence: {}", e.msg);
    assert!(!e.msg.contains("pruned"), "…and not the dangling one: {}", e.msg);
}

/// With no project above the working directory there is nowhere to KEEP marks, and saying "no
/// such mark" would be a claim about a store that does not exist.
#[test]
fn a_mark_with_no_marks_root_says_there_is_nowhere_to_keep_one() {
    let s = three();
    let e = super::resolve_one(&s, None, "@baseline/m").expect_err("no marks root");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("no project directory"), "{}", e.msg);
    assert!(!e.msg.contains("no mark named"), "{}", e.msg);
}

/// ⚠ An unknown `@form` is a MISSING MARK, not a grammar error — marks are user-named, so this
/// grammar has no closed roster of `@` words a typo could be checked against, and `@lst` is
/// indistinguishable from a mark called `lst`.
///
/// It must still put the operator right in one line, which is why the `@` spelling alone gets
/// the FORMS roster appended: without it the answer to a mistyped `@last` would be a sentence
/// about a mark they never meant to name. **The RUNG moved when marks shipped** — it was
/// `Usage` while every `@` word was a closed set — and that is the honest classification: the
/// command line is well-formed and nothing in it can be retyped to make a mark exist.
#[test]
fn an_unknown_at_form_reads_as_a_missing_mark_and_still_names_the_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    let s = three();
    let e = super::resolve_one(&s, Some(&marks), "@lst").expect_err("typo");
    assert_eq!(e.exit, Exit::Failed);
    assert!(e.msg.contains("@last"), "the forms are named: {}", e.msg);
    assert!(e.msg.contains("lst"), "…and so is what was typed: {}", e.msg);
}

/// A bare mark miss does NOT recite the forms — it is unambiguously a mark, and the roster
/// there is noise in the one message somebody reads when a mark name is mistyped.
#[test]
fn a_bare_mark_miss_does_not_recite_the_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    let s = three();
    let e = super::resolve_one(&s, Some(&marks), "baseline/nope").expect_err("missing");
    // ⚠ The POSITIVE half first: an EMPTY message would satisfy the negative below while proving
    // nothing. This is the shape that has produced seven false assertions in this programme.
    assert!(e.msg.contains("no mark named"), "it still says what went wrong: {}", e.msg);
    assert!(e.msg.contains("baseline/nope"), "…and names the mark: {}", e.msg);
    assert!(!e.msg.contains("@last"), "{}", e.msg);
}

/// A mark NAME that would escape the marks root is a USAGE refusal, and nothing is opened.
#[test]
fn a_mark_name_that_would_escape_is_a_usage_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let marks = tmp.path().join("marks");
    let s = three();
    let e = super::resolve_one(&s, Some(&marks), "@../escape").expect_err("traversal");
    assert_eq!(e.exit, Exit::Usage);
    assert!(!marks.exists(), "nothing was created looking it up");
}

/// `@last:` with an empty kind is a refusal rather than a silent `@last` — and it must not fall
/// into the mark arm either, since `last:` is the one reserved word after an `@`.
#[test]
fn at_last_with_an_empty_kind_is_refused() {
    let s = three();
    let e = resolve_one(&s, "@last:").expect_err("no kind");
    assert_eq!(e.exit, Exit::Usage);
    assert!(e.msg.contains("@last:"), "{}", e.msg);
    let e = resolve_one(&s, "@").expect_err("bare at");
    assert_eq!(e.exit, Exit::Usage);
}

/// `ls` takes the same grammar in the same position, as a FILTER: absent = everything, `@last`
/// = exactly one row, a kind = that kind's rows.
#[test]
fn ls_takes_the_same_grammar_as_a_filter() {
    let s = three();
    assert_eq!(resolve_many(&s, None).unwrap().len(), 3);
    assert_eq!(resolve_many(&s, Some("@last")).unwrap().len(), 1);
    let searches = resolve_many(&s, Some("@last:search")).unwrap();
    assert_eq!(searches.len(), 1);
    assert_eq!(searches[0].manifest.kind, "search");
    // A PREFIX as a filter matches every run it prefixes — plural is not ambiguous here, which
    // is the one place the two entry points deliberately differ.
    assert_eq!(resolve_many(&s, Some("1789213")).unwrap().len(), 2);
}

/// **The module doc's table and the refusal's roster are the same roster**, and this test READS
/// the doc to say so — at run time, from this file's own source, the way
/// `vike_model::runs`'s reserved-file harvest reads its own writer.
///
/// ⚠ It used to assert four literals against [`FORMS`] and never open the doc it was named for,
/// so a form deleted from the `//!` table left it green. What it pins now is the shape a
/// grammar actually acquires a second, undocumented spelling through: a row in one and not the
/// other. `include_str!` is deliberately NOT used — the published mirror may withhold a file,
/// and a compile-time embed would break the build there rather than skipping.
#[test]
fn the_doc_table_and_the_refusal_agree() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("cmd")
            .join("runs")
            .join("selector.rs"),
    )
    .expect("this module's own source — read at run time, never `include_str!`");
    let doc: String = src.lines().filter(|l| l.trim_start().starts_with("//!")).collect();

    // The SHIPPING forms: in FORMS, and in the doc's table. `@<mark>` joined them when marks
    // shipped — it is a form the grammar RESOLVES now, not one it recognises and refuses.
    for form in ["<run-id>", "<unique-prefix>", "@last", "@last:<kind>", "@<mark>"] {
        assert!(FORMS.contains(form), "{form} is missing from FORMS");
    }
    // ⚠ The doc's table spells the parameterised rows as EXAMPLES rather than as placeholders
    // (`@last:search`, `@baseline/momentum`, a literal id), so the doc is held to the row it can
    // carry: every `@`-form FORMS names, and the one DEFERRED form (`#12`), which must stay
    // documented precisely because it is refused rather than silently missing.
    for spelled in ["`@last`", "`@last:search`", "`@baseline/", "#12`"] {
        assert!(doc.contains(spelled), "the module doc's table omits {spelled}");
    }
    // The vacuity floor: a filter that stopped matching `//!` would compare two empty strings
    // and pass every assertion above.
    assert!(doc.len() > 500, "the doc harvest found {} bytes — did the filter break?", doc.len());
}
