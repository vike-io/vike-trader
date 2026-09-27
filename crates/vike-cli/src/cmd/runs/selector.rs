//! **The selector grammar — one implementation, shared by every verb that takes a run** (spec §4,
//! and the obligation §15.6 states: "every verb taking a run accepts the same forms in the same
//! position, or the family fragments").
//!
//! | form | means |
//! |---|---|
//! | `1789213143-8821-01` | a full run id |
//! | `1789213` | a unique short prefix; ambiguity is an exit-`2` refusal naming the candidates |
//! | `@last` | the most recent run |
//! | `@last:search` | the most recent run of that kind |
//! | `@baseline/momentum` | a mark set by `tag --as` |
//! | `<id>#12` | a child of a search or walk-forward — **not yet; see below** |
//!
//! # ⚠ `<kind>` is the WHAT axis, and `walkforward` is not on it
//!
//! Owner ruling 7: a kind says what was COMPUTED — `"backtest"`
//! (`crates/vike-backtest/src/backtest_cli.rs`'s `persist_run`), `"study"`
//! (`crates/vike-studio-core/src/study_run.rs`'s `STUDY_RUN_KIND`), and `"search"` from the stage
//! that persists one. How a run was VALIDATED — one slice, or walked forward — is a MODIFIER on top
//! of that, not a third kind, so nothing writes `walkforward` into
//! `vike_model::runs::RunManifest::kind` and `@last:walkforward` can only ever miss.
//!
//! An operator will still type it: `crates/vike-cli/src/lib.rs`'s `COMMANDS` carries a top-level
//! `walkforward` verb. So the miss NAMES the kinds that are present ([`no_match_message`]) rather
//! than reading as an empty store — the one-line difference between "you asked the wrong axis" and
//! "re-run your validation".
//!
//! # ⚠ A MARK is the form that does not move, and it is how `gate` and `diff` are written
//!
//! A run id changes every time you run, so a comparison written against one passes once and then
//! names a run nobody is comparing to. `vike-cli backtest tag <run> --as baseline/momentum` sets a
//! NAME that points at a run, and [`resolve_one`] accepts it wherever an id goes — which is §15.6's
//! obligation applied to the second operand rather than the first.
//!
//! **Both spellings are accepted**, with and without the `@`: §4's table writes
//! `@baseline/momentum` and §7.1's own example writes `--against baseline/momentum`. A minted run id
//! is `<seconds>-<address>-<seq>` and can never contain a `/`, so accepting both costs no ambiguity
//! and spares every user the question of which page was right. A SINGLE-segment mark needs its `@`
//! (`@prod`): without one, `prod` is indistinguishable from a short id prefix, and guessing is how a
//! gate silently judges the wrong run.
//!
//! ⚠ **A mark that names a run which is GONE is a different answer from a mark that was never set.**
//! The first says a prune or an `rm` took the run out from under a name somebody is still gating on;
//! the second says the name was never there. They have different fixes, so [`candidates`] reports
//! them as different sentences rather than as one "not found".
//!
//! # ⚠ One form is RECOGNISED and refused, and that is the point
//!
//! `<id>#N` needs CHILDREN, and spec §13b records that today's sweep branch returns before the clock
//! read that mints a run id — so a search writes nothing at all for a child to hang off. It is still
//! PARSED, because the alternative is worse: letting it fall through to a directory lookup answers
//! "no run named 1789…#12", which sends an operator to look for a folder. A refusal that names the
//! form and what has to persist first keeps this file the one place the grammar is decided.
//!
//! # The DIRECTORY is what every form resolves to
//!
//! Never `vike_model::runs::RunManifest::run_id`, which is duplicated into the file and can disagree
//! with it — `crate::cmd::runs::scan`'s doc carries the rule. A selector that resolved against the
//! declared id would open a run the operator did not name.

use std::path::Path;

use vike_model::runs::{MarkError, read_mark};

use crate::cmd::runs::scan::{RunScan, ScannedRun};
use crate::exit::CliError;

/// Every form this grammar knows, rendered for a refusal message. Derived from nothing — it IS the
/// roster, and the module doc's table is held to it by `the_doc_table_and_the_refusal_agree`.
const FORMS: &str = "<run-id> | <unique-prefix> | @last | @last:<kind> | @<mark>";

/// Resolve a selector to EXACTLY ONE run — what `show`, `path`, `tag`, `gate` and `diff` take.
///
/// `marks_root` is `<project>/user_data/marks`, a PARAMETER for the reason every path in this
/// family is one: a `src/cmd/` file may not resolve a project for itself. `None` is a process with
/// no project above it, and a mark form then refuses by saying so rather than by reporting the mark
/// missing — "there is nowhere to keep marks" and "that mark was never set" are different facts.
pub(crate) fn resolve_one<'a>(
    scan: &'a RunScan,
    marks_root: Option<&Path>,
    selector: &str,
) -> Result<&'a ScannedRun, CliError> {
    let matched = candidates(scan, marks_root, selector)?;
    match matched.len() {
        1 => Ok(matched[0]),
        0 => Err(CliError::failed(no_match_message(scan, selector))),
        // §4: NAME the candidates. A "most recent wins" tiebreak would be the silent precedence this
        // grammar exists to refuse.
        _ => Err(CliError::usage(format!(
            "'{selector}' matches {} runs — name one of:\n  {}",
            matched.len(),
            matched.iter().map(|r| r.run_id.as_str()).collect::<Vec<_>>().join("\n  ")
        ))),
    }
}

/// Resolve a selector used as a FILTER — what `ls` takes. `None` is every run.
///
/// ⚠ The one place the two entry points differ: a prefix matching several runs is AMBIGUOUS for
/// `show` and is simply a narrower LIST here. An empty result is not an error — `ls` prints an
/// empty-state note and exits `0` (`crate::cmd::runs::ls`).
pub(crate) fn resolve_many<'a>(
    scan: &'a RunScan,
    marks_root: Option<&Path>,
    selector: Option<&str>,
) -> Result<Vec<&'a ScannedRun>, CliError> {
    match selector {
        None => Ok(scan.runs.iter().collect()),
        Some(s) => candidates(scan, marks_root, s),
    }
}

/// The grammar itself. Everything above is arity policy over this.
fn candidates<'a>(
    scan: &'a RunScan,
    marks_root: Option<&Path>,
    selector: &str,
) -> Result<Vec<&'a ScannedRun>, CliError> {
    let sel = selector.trim();
    if sel.is_empty() {
        return Err(CliError::usage(format!("an empty run selector — expected one of: {FORMS}")));
    }

    // ⚠ The `#` form is checked BEFORE the `@` forms and before any directory match, because a run
    // id can legitimately be its prefix and a silent prefix match would open the PARENT while the
    // operator asked for a child.
    if let Some((head, _tail)) = sel.split_once('#') {
        return Err(CliError::usage(format!(
            "'{sel}': the `<id>#N` child form is not available yet — it names trial N of a search \
             or window N of a walk-forward, and neither writes a child run today (a search returns \
             before a run id is minted at all). It ships with the search-persistence stage. For now \
             name the parent outright: '{head}'."
        )));
    }

    if let Some(form) = sel.strip_prefix('@') {
        if form == "last" {
            // `scan.runs` is in directory-name order, which for a minted id is chronological, so the
            // most recent is simply the last one — no clock read, no manifest timestamp parse.
            return Ok(scan.runs.iter().next_back().into_iter().collect());
        }
        if let Some(kind) = form.strip_prefix("last:") {
            if kind.is_empty() {
                return Err(CliError::usage(format!(
                    "'{sel}': @last: needs a kind after the colon, e.g. @last:backtest"
                )));
            }
            let last = scan.runs.iter().rfind(|r| r.manifest.kind.eq_ignore_ascii_case(kind));
            return Ok(last.into_iter().collect());
        }
        if form.is_empty() {
            return Err(CliError::usage(format!(
                "'{sel}': `@` alone names nothing — expected one of: {FORMS}"
            )));
        }
        // ⚠ Everything else after an `@` is a MARK, not an unknown form. That is what makes `@lst`
        // a "no such mark" rather than a grammar error — and it is the right answer: this grammar
        // has no closed roster of `@` words to check a typo against, because marks are user-named.
        // The refusal below names the forms anyway, so the typo is still visible in one line.
        return resolve_mark(scan, marks_root, sel, form);
    }

    // ⚠ A `/` can never occur in a minted run id (`<seconds>-<address>-<seq>`), so a bare token
    // carrying one is a MARK — the `--against baseline/momentum` spelling §7.1's own example uses.
    if sel.contains('/') {
        return resolve_mark(scan, marks_root, sel, sel);
    }

    // A literal: an exact directory name first, and only then a prefix. An id that is also a prefix
    // of a longer one must resolve to ITSELF rather than reporting ambiguity.
    if let Some(exact) = scan.runs.iter().find(|r| r.run_id == sel) {
        return Ok(vec![exact]);
    }
    Ok(scan.runs.iter().filter(|r| r.run_id.starts_with(sel)).collect())
}

/// A mark, resolved to the run it points at.
///
/// ⚠ The mark is turned into an ID and then looked up by the SAME exact-directory-name rule a typed
/// id takes, so a mark and an id cannot reach different runs. `vike_model::runs::read_mark` owns the
/// store; this owns the mapping from its failures onto rungs, at the site that knows which failure
/// is which (`crates/vike-cli/src/cmd/trade_status.rs` is the shape).
fn resolve_mark<'a>(
    scan: &'a RunScan,
    marks_root: Option<&Path>,
    selector: &str,
    name: &str,
) -> Result<Vec<&'a ScannedRun>, CliError> {
    let Some(root) = marks_root else {
        return Err(CliError::failed(format!(
            "'{selector}' names a mark, and there is no project directory above the working \
             directory to keep marks in. `vike-cli init` creates one, or name it with \
             VIKE_USER_DATA_DIR."
        )));
    };
    let mark = read_mark(root, name).map_err(|e| match e {
        // A name that cannot be a path is a command line to FIX — nothing was looked up at all.
        MarkError::BadName { .. } => CliError::usage(format!("'{selector}': {e}")),
        // A mark that was never set is a fact about the STORE, exactly as a missing run id is:
        // the line was well-formed and nothing here can be retyped to fix it.
        //
        // ⚠ The `@` spelling gets the FORMS roster appended and the bare one does not, because
        // that is where a form TYPO lands: `@lst` is indistinguishable from a mark called `lst`
        // (marks are user-named, so this grammar has no closed roster of `@` words to check
        // against), and without the clause the answer to a mistyped `@last` would be a sentence
        // about a mark the operator never meant. A bare `a/b` miss is unambiguously a mark, and
        // reciting the forms there is noise.
        MarkError::Missing { .. } if selector.starts_with('@') => {
            CliError::failed(format!("{e} The selector forms are: {FORMS}."))
        }
        MarkError::Missing { .. } | MarkError::Read { .. } | MarkError::Write { .. } => {
            CliError::failed(e.to_string())
        }
    })?;
    match scan.runs.iter().find(|r| r.run_id == mark.run_id) {
        Some(run) => Ok(vec![run]),
        // ⚠ DANGLING, and it must not read as "no such mark". The name IS set; the run it points at
        // is gone — pruned, or removed by hand — and the fix is to re-point the mark, not to create
        // it. A gate that reported this as a missing mark would send somebody to set a name that is
        // already there.
        None => Err(CliError::failed(format!(
            "the mark '{name}' points at run {}, which is not under {} — the run was pruned or \
             removed. Re-point it with `vike-cli backtest tag <run> --as {name}`.",
            mark.run_id,
            scan.root.display()
        ))),
    }
}

/// The "nothing matched" sentence, which says WHICH tree was looked in — the thing an operator
/// actually needs when `$VIKE_USER_DATA_DIR` is in play.
///
/// ⚠ A `@last:<kind>` miss gets one extra clause, and only that form does: the KINDS actually
/// present. Owner ruling 7 makes walk-forward a validation MODIFIER rather than a run kind, so
/// `@last:walkforward` is a form an operator reasonably types (there is a top-level `walkforward`
/// verb) against an axis that can never carry it. Without the clause the answer is
/// indistinguishable from an empty store and sends them to re-run something; with it, one line
/// shows they asked the wrong axis. A PLAIN miss does not get the clause — a mistyped run id is
/// not a question about kinds, and reciting them there is noise.
fn no_match_message(scan: &RunScan, selector: &str) -> String {
    if scan.runs.is_empty() {
        return format!(
            "no runs under {} — '{selector}' cannot match anything. Run one with \
             `vike-cli backtest run --profile <run.toml>`.",
            scan.root.display()
        );
    }
    let kinds = if selector.trim().starts_with("@last:") {
        let mut present: Vec<&str> = scan.runs.iter().map(|r| r.manifest.kind.as_str()).collect();
        present.sort_unstable();
        present.dedup();
        format!(" The kinds here are: {}.", present.join(", "))
    } else {
        String::new()
    };
    format!(
        "no run matching '{selector}' under {} ({} run(s) there).{kinds} \
         `vike-cli backtest ls` lists them.",
        scan.root.display(),
        scan.runs.len()
    )
}

/// A `ScannedRun` with every manifest field spelled out, so a field added to `RunManifest` reddens
/// this helper rather than silently defaulting under a test.
///
/// ⚠ **It sits OUTSIDE `mod tests` on purpose, and the reason is a gate.** Three sibling test
/// modules share it, which wants `pub(crate)` — and `pub(crate) mod tests {` is invisible to
/// `crates/vike-ops/tests/compile_time_path_gate.rs`'s `is_inline_mod_head`, which admits `mod ` and
/// `pub mod ` and nothing else. A test module spelled that way is judged as PRODUCTION code by that
/// gate and by every sibling that copied the helper, which silently opts the file out of the
/// test-region exclusion they all rely on. A `#[cfg(test)]` FREE FUNCTION is importable across
/// modules of one crate just as well, and leaves `mod tests` in the spelling every gate recognises.
#[cfg(test)]
pub(crate) fn test_run_at(run_id: &str, kind: &str) -> ScannedRun {
    use vike_model::runs::{MANIFEST_SCHEMA, RunConfig, RunManifest};
    ScannedRun {
        run_id: run_id.to_string(),
        dir: std::path::PathBuf::from("/runs").join(run_id),
        manifest: RunManifest {
            schema: MANIFEST_SCHEMA,
            run_id: run_id.to_string(),
            kind: kind.to_string(),
            produced_by: "backtest".to_string(),
            started_at: "2026-08-24T09:15:04Z".to_string(),
            finished_at: "2026-08-24T09:15:16Z".to_string(),
            git_sha: None,
            fingerprint: None,
            config: RunConfig {
                path: Some("profiles/sma.toml".to_string()),
                name: Some("sma cross".to_string()),
            },
            detail: serde_json::json!({ "strategy": "sma_cross" }),
        },
        report: None,
    }
}

/// A `RunScan` over fixture runs, with no problems. Outside `mod tests` for the reason above.
#[cfg(test)]
pub(crate) fn test_scan_of(runs: Vec<ScannedRun>) -> RunScan {
    RunScan { root: std::path::PathBuf::from("/runs"), runs, problems: Vec::new() }
}

#[cfg(test)]
mod tests {
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

        assert_eq!(
            super::resolve_one(&s, Some(&marks), "@prod").unwrap().run_id,
            "1789213999-8821-00"
        );
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
        assert!(
            doc.len() > 500,
            "the doc harvest found {} bytes — did the filter break?",
            doc.len()
        );
    }
}
