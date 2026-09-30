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
/// is which (`crates/vike-cli/src/cmd/trade/status.rs` is the shape).
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

#[path = "selector_tests.rs"]
#[cfg(test)]
mod selector_tests;
