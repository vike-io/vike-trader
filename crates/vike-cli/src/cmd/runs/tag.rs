//! `vike-cli backtest tag <run>` — the ONE write this family performs against a run's metadata.
//!
//! Two different writes, because they are two different things
//! (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §7.2):
//!
//! * `--add TAG` / `--note TEXT` attach a LABEL to one run. They live in the run's own directory
//!   (`vike_model::runs::META_FILE`) and travel with it.
//! * `--as NAME` sets a MARK — a NAME that points at a run. It lives in a file named for the mark, a
//!   SIBLING of the runs root, because a mark must resolve without opening every run directory and
//!   because a `marks/` directory under `runs/` would read as an unfinished run forever
//!   (`vike_model::state_path::MARKS_SUBDIR`).
//!
//! ⚠ **A mark is what makes `gate` writable at all.** A run id moves every time you run, so a gate
//! written against one passes once and then names a run nobody is comparing to.
//!
//! ⚠ **This module is not the only writer of the label half.** `vike_model::runs::add_tags` is the
//! one implementation and `backtest run --tag/--note` (spec §5.8) calls it too — see that function's
//! doc for why the two verbs may not each own a format.
//!
//! # A write of NOTHING is a usage error
//!
//! `backtest tag <run>` with none of `--add`, `--note` or `--as` is refused on the usage rung rather
//! than exiting `0` having done nothing. A script that meant to set a mark and mistyped the flag
//! must not read as green — that is positive confirmation of something false, which is the defect
//! this repository deletes settings keys over.

use vike_model::runs::{MarkError, add_tags, valid_mark_name, write_mark};

use crate::cmd::backtest::ReadArgs;
use crate::cmd::runs::scan::scan_runs;
use crate::cmd::runs::{Ctx, selector::resolve_one};
use crate::exit::{CliError, CmdResult};

/// `tag`'s whole product, so the renderer prints what was written rather than re-deriving it.
struct Tagged {
    run_id: String,
    tags: Vec<String>,
    notes_added: Option<String>,
    /// The mark that was set, as `(name, previous run id)` — `None` when `--as` was not given, and
    /// the inner `None` when the mark is new.
    mark: Option<(String, Option<String>)>,
    marked_at: Option<String>,
}

/// ⚠ **One `--note` is written in TWO places when `--as` is also given, and that is deliberate.**
/// It goes into the run's sidecar (what this RUN was) and into the mark (why the mark MOVED), which
/// are two different questions the same sentence usually answers. §7.2's "re-marking is explicit and
/// recorded" is only true if the reason travels with the pointer, and a note that lived only in the
/// sidecar would be invisible to anyone reading the mark's history — which is the document somebody
/// opens when a gate starts judging against a baseline they did not choose.
///
/// The COST, stated rather than discovered: a note is duplicated on disk. That is cheap, and the
/// alternative — a second `--mark-note` flag — is a second thing to remember for a distinction
/// nobody has asked for.
pub(crate) fn run_tag(ctx: &Ctx<'_>, a: &ReadArgs, now: i64) -> CmdResult<()> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             tag anything in. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    // Problems go to STDERR in both modes, so an unreadable run directory cannot make a
    // machine-readable document unparseable — the rule `crate::cmd::runs`'s doc states.
    for problem in &scan.problems {
        eprintln!("vike-cli backtest tag: {problem}");
    }
    let selector = a.selector.as_deref().unwrap_or_default();
    let run = resolve_one(&scan, ctx.marks_root, selector)?;

    let mut out = Tagged {
        run_id: run.run_id.clone(),
        tags: Vec::new(),
        notes_added: a.note.clone(),
        mark: None,
        marked_at: None,
    };

    if !a.add.is_empty() || a.note.is_some() {
        let meta = add_tags(&run.dir, &a.add, a.note.as_deref(), now)
            .map_err(|e| CliError::failed(e.to_string()))?;
        out.tags = meta.tags;
    }

    if let Some(name) = &a.mark_as {
        let marks_root = ctx.marks_root.ok_or_else(|| {
            CliError::failed(
                "no project directory above the working directory, so there is nowhere to keep a \
                 mark. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
            )
        })?;
        // What it POINTED at before, read before the write so the render can show the move. A
        // missing mark is the ordinary first-set case and is not an error here.
        let previous = match vike_model::runs::read_mark(marks_root, name) {
            Ok(m) => Some(m.run_id),
            Err(MarkError::Missing { .. }) => None,
            Err(e) => return Err(CliError::failed(e.to_string())),
        };
        let mark = write_mark(marks_root, name, &run.run_id, a.note.as_deref(), now)
            .map_err(|e| mark_write_error(&e))?;
        out.marked_at = Some(mark.marked_at);
        out.mark = Some((name.clone(), previous));
    }

    if a.json {
        render_json(&out)
    } else {
        render_human(&out)
    }
    Ok(())
}

/// Classify a mark-write failure at the site that KNOWS which failure it is — the split
/// `crates/vike-cli/src/cmd/trade_status.rs` makes between its `failure_lines` and `failure_exit`.
fn mark_write_error(e: &MarkError) -> CliError {
    match e {
        // The name cannot be a path. Nothing was attempted, and re-running unchanged cannot succeed.
        MarkError::BadName { .. } => CliError::usage(e.to_string()),
        _ => CliError::failed(e.to_string()),
    }
}

fn render_human(t: &Tagged) {
    println!("tagged {}", t.run_id);
    if !t.tags.is_empty() {
        println!("  tags   {}", t.tags.join(", "));
    }
    if let Some(note) = &t.notes_added {
        println!("  note   {note}");
    }
    // ⚠ The MOVE is rendered when there was one, because §7.2's "re-marking is explicit and
    // recorded" is only true if somebody is told. A mark that silently re-pointed is how a gate
    // comes to judge against a baseline nobody chose.
    if let Some((name, previous)) = &t.mark {
        match previous {
            Some(prev) if prev != &t.run_id => {
                println!("  mark   {name}  {prev} → {}", t.run_id);
            }
            Some(_) => println!("  mark   {name}  (unchanged)"),
            None => println!("  mark   {name}  → {}", t.run_id),
        }
    }
}

fn render_json(t: &Tagged) {
    let doc = serde_json::json!({
        "schema": 1,
        "run_id": t.run_id,
        "tags": t.tags,
        "note": t.notes_added,
        "mark": t.mark.as_ref().map(|(name, previous)| serde_json::json!({
            "name": name,
            "run_id": t.run_id,
            "previous_run_id": previous,
            "marked_at": t.marked_at,
        })),
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => println!("{text}"),
        // A document that cannot be serialized is a bug here, not the user's — say so rather than
        // printing half of one.
        Err(e) => eprintln!("vike-cli backtest tag: cannot render the JSON document: {e}"),
    }
}

/// The parse-time refusals this verb owns, applied by `crate::cmd::backtest`'s `parse_read` so that
/// every message about a command line is produced before any directory is opened.
///
/// ⚠ It lives HERE rather than in the shared parser because both rules are about what `tag` MEANS: a
/// write of nothing that reads as success, and a mark name that could not be a file. The parser owns
/// the mechanics; a verb owns its own semantics.
pub(crate) fn refuse_a_tag_that_writes_nothing(a: &ReadArgs) -> Result<(), String> {
    if a.add.is_empty() && a.note.is_none() && a.mark_as.is_none() {
        return Err(format!(
            "`backtest tag {}` would write nothing — give it at least one of `--add TAG` (a label \
             on the run), `--note TEXT` (a note beside it) or `--as NAME` (a MARK, the stable name \
             `gate --against` takes). A tag command that changed nothing and exited 0 would be \
             positive confirmation of something that did not happen.",
            a.selector.as_deref().unwrap_or("<run>")
        ));
    }
    if let Some(name) = &a.mark_as {
        valid_mark_name(name).map_err(|why| format!("--as {why}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cmd::backtest::ReadSub;

    fn args(selector: &str) -> ReadArgs {
        let mut a = ReadArgs::empty(ReadSub::Tag);
        a.selector = Some(selector.to_string());
        a
    }

    /// A `tag` that would write nothing is a USAGE refusal naming all three flags — a script that
    /// meant to set a mark and mistyped must not read as green.
    #[test]
    fn a_tag_with_nothing_to_write_names_every_flag_that_would_have_worked() {
        let e = refuse_a_tag_that_writes_nothing(&args("@last")).expect_err("writes nothing");
        for flag in ["--add", "--note", "--as"] {
            assert!(e.contains(flag), "{flag} is not named: {e}");
        }
        assert!(e.contains("@last"), "…and neither is the run: {e}");
    }

    /// Any ONE of the three is enough. This is the negative control for the test above: a rule that
    /// refused everything would pass that one and break the verb.
    #[test]
    fn any_one_of_the_three_writes_is_accepted() {
        let mut add = args("@last");
        add.add = vec!["ci".to_string()];
        assert!(refuse_a_tag_that_writes_nothing(&add).is_ok());

        let mut note = args("@last");
        note.note = Some("looked at it".to_string());
        assert!(refuse_a_tag_that_writes_nothing(&note).is_ok());

        let mut mark = args("@last");
        mark.mark_as = Some("baseline/m".to_string());
        assert!(refuse_a_tag_that_writes_nothing(&mark).is_ok());
    }

    /// ⚠ A mark NAME is refused at PARSE time, before any directory is opened — the name becomes a
    /// file path, so a traversal must never reach the filesystem at all.
    #[test]
    fn a_mark_name_that_could_not_be_a_file_is_refused_before_anything_opens() {
        for bad in ["../escape", "a//b", ".hidden", "with space", "con"] {
            let mut a = args("@last");
            a.mark_as = Some(bad.to_string());
            let e = refuse_a_tag_that_writes_nothing(&a)
                .expect_err("a mark name that could not be a file must be refused");
            assert!(e.contains("--as"), "`{bad}`: the refusal names the flag: {e}");
        }
    }

    /// ⚠ **The marks store is a SIBLING of the runs store**, and this is the assertion in the CLI
    /// that holds it there. `vike_model::state_path::MARKS_SUBDIR` argues why, and
    /// `crates/vike-cli/src/lib.rs`'s dispatcher is what joins both onto `user_data`.
    #[test]
    fn the_marks_root_is_never_under_the_runs_root() {
        let user_data = Path::new("/p/user_data");
        let runs = user_data.join(vike_model::state_path::RUNS_SUBDIR);
        let marks = user_data.join(vike_model::state_path::MARKS_SUBDIR);
        assert!(
            !marks.starts_with(&runs),
            "a marks directory under runs/ reads as a broken run in every listing"
        );
        assert_eq!(marks.parent(), runs.parent());
    }
}
