//! `vike-cli backtest gate <run> --against <mark> --fail-if EXPR` — the verb whose PRODUCT is the
//! exit code.
//!
//! A CI step, a merge gate or a systemd `ExecStartPre=` calls this
//! (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §7.1). Decision 8 of that
//! design is what it exists for: "An engine edit compiles, passes tests, satisfies clippy, and still
//! moves every strategy's result."
//!
//! # The verdict is a DOCUMENT and the rung is its summary
//!
//! §7.1: "The verdict is a document naming every criterion that passed and failed, never a bare
//! number." So the table goes to **stdout** in full — on a pass as well as on a breach, because a CI
//! step that prints its gate's verdict on success is the normal case — and the rung is computed off
//! the same `Vec<Judgement>` the table is rendered from. [`render_human`] and [`rung`] are split for
//! the reason `crates/vike-cli/src/cmd/trade_status.rs` splits `failure_lines` from `failure_exit`:
//! the words and the number must be decided over one pass, or they come to disagree about which
//! criterion is which.
//!
//! # Three outcomes, ordered, and the order is the whole safety property
//!
//! `Breach` beats `Unevaluated` beats `Pass` — the same ordered-level-and-`.max()` shape
//! `crates/vike-cli/src/cmd/config_check.rs`'s `Report::worst` uses, and the ordering itself is
//! derived from `crate::cmd::runs::failif::Outcome`'s declaration order rather than restated here.
//!
//! * A BREACH must never be masked by a typo elsewhere in the same expression.
//! * An UNEVALUATED criterion must never read as a pass. A gate where one of three criteria silently
//!   stopped being checked is the "green means nothing ran" failure this repository is organised
//!   against, and `crate::exit::Exit::Empty` exists so a CI step can tell it from a zero.
//!
//! # ⚠ A MISSING BASELINE IS NOT EVIDENCE THAT THE INPUTS CHANGED
//!
//! **This verb has no "the inputs changed" verdict, by construction rather than by a rule**, and
//! that is the honest answer to a question worth asking outright. It never reads
//! `vike_model::runs::RunManifest::fingerprint` at all: the baseline is whatever SELECTOR the
//! operator named — a mark, an id, `@last` — so "no comparable baseline" is the only shape a
//! baseline failure can take here, and there is nowhere for an input-address comparison to be
//! mistaken for one. Attributing a move to inputs is `crate::cmd::runs::diff`'s job, and that verb
//! says by name what the record cannot support.
//!
//! That matters because the run address is ABSENT far more often than a reader expects, and absent
//! is never DIFFERENT. Distinct shapes produce a null or an unmatchable address, and none of them is
//! evidence about the data:
//!
//! 1. **a store fault** — the producer refuses to address a run whose data slice it could not
//!    inventory, rather than addressing it wrongly, so the field is `null`;
//! 2. **a grouped series** — its coverage is the WHOLE GROUP, so an unrelated symbol's rows move the
//!    address and a run with byte-identical inputs finds no comparable baseline (spec §18 row 10);
//! 3. **a producer that computes none at all** — the field is optional on
//!    `vike_model::runs::RunManifest` precisely so a producer with no notion of an input slice can
//!    still write a manifest.
//!
//! ⚠ **A SEARCH PARENT used to be on that list and is not any more.** It carried no address because
//! `crates/vike-backtest/src/backtest_cli.rs` collected the fingerprint below its sweep branch to
//! keep a search's manifest reads down; `vike_data::DataFusionHist::series_facts` merged the two
//! reads that cost bought, so a search now addresses its inputs exactly as a single run does and
//! this verb can gate one against a baseline. Nothing about the verb changed to allow it — it reads
//! no fingerprint either way; it gained a comparable population, not a new failure mode.
//!
//! So a baseline this verb could not resolve is REFUSED — naming the mark and what is missing — on
//! `Exit::Failed` or `Exit::Empty`, never `Exit::Breach`. A gate that read "I could not find the
//! baseline" as "the numbers moved" would turn a store hiccup, a group-mate's backfill or a sweep
//! into a red build with a confident and false explanation.
//!
//! # What it reads, and what it deliberately does not
//!
//! Two `report.json` documents, as `serde_json::Value`.
//! `crates/vike-analytics/src/report.rs`'s `BacktestReport` derives `Serialize` ONLY —
//! `crate::cmd::backtest`'s `execute` already re-parses to a `Value` at its own call site to avoid
//! that, and this crate does not link `vike-analytics` at all. Adding `Deserialize` there would be a
//! change to a type `vike-report`'s `LiveTearsheet` and the compute wire both consume, for no gain
//! here: a criterion names a JSON KEY, which is what makes this verb work on a research run's report
//! and on every field the run record gains later without an edit.
//!
//! ⚠ **It reads no manifest field for the judgement**, deliberately. §6.3's input half — the
//! resolved config, the data fingerprint, the build stamp — belongs to `crate::cmd::runs::diff`; a
//! gate answers one question, "did the numbers move too far", and answering "why" in the same verb
//! would make its exit code mean two things.

use serde_json::Value;
use vike_model::runs::REPORT_FILE;

use crate::cmd::backtest::ReadArgs;
use crate::cmd::runs::failif::{self, Judgement, Outcome, judge, parse_fail_if, verdict_word};
use crate::cmd::runs::scan::{ScannedRun, scan_runs};
use crate::cmd::runs::{Ctx, jsondoc, selector::resolve_one};
use crate::exit::{CliError, CmdResult, Exit};

pub(crate) fn run_gate(ctx: &Ctx<'_>, a: &ReadArgs) -> CmdResult<Exit> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             gate against. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    for problem in &scan.problems {
        eprintln!("vike-cli backtest gate: {problem}");
    }

    // Parsed BEFORE anything is opened: a malformed `--fail-if` is a command line to fix, and
    // resolving two runs first would hand the operator a message about the store instead.
    let criteria =
        parse_fail_if(a.fail_if.as_deref().unwrap_or_default()).map_err(CliError::usage)?;

    let subject = resolve_one(&scan, ctx.marks_root, a.selector.as_deref().unwrap_or_default())?;
    let against_selector = a.against.as_deref().unwrap_or_default();
    let baseline = resolve_one(&scan, ctx.marks_root, against_selector)?;

    let subject_doc = read_report(subject)?;
    let baseline_doc = read_report(baseline)?;

    let judgements: Vec<Judgement> = criteria
        .iter()
        .map(|c| {
            judge(
                c,
                jsondoc::number_at(&baseline_doc, &c.key),
                jsondoc::number_at(&subject_doc, &c.key),
            )
        })
        .collect();

    let exit = rung(&judgements);
    // ⚠ Only computed when something went UNEVALUATED, and only then rendered. "Here are the keys
    // this document carries" is the whole difference between a refusal somebody can act on and one
    // that leaves them guessing at a spelling — and on a clean pass it is noise.
    let available = judgements
        .iter()
        .any(|j| matches!(j.outcome, Outcome::Unevaluated(_)))
        .then(|| jsondoc::number_keys(&subject_doc));
    if a.json {
        render_json(
            a,
            subject,
            baseline,
            against_selector,
            &judgements,
            exit,
            available.as_deref(),
        );
    } else {
        render_human(subject, baseline, against_selector, &judgements, exit, available.as_deref());
    }
    Ok(exit)
}

/// Read one run's report, or refuse on the rung that says WHY there was nothing to judge.
///
/// ⚠ A run with no `report.json` is [`Exit::Empty`], not [`Exit::Failed`]: the gate did not fail, it
/// had nothing to evaluate. From a `1` a CI step would retry; from this it knows the run it named is
/// unfinished or was written by a producer that keeps no report.
fn read_report(run: &ScannedRun) -> Result<Value, CliError> {
    let Some(path) = &run.report else {
        return Err(CliError::empty(format!(
            "{} has no {REPORT_FILE} — there is nothing to judge. The run is unfinished, or it was \
             written by a producer that keeps no report.",
            run.dir.display()
        )));
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::failed(format!("cannot read {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map_err(|e| CliError::failed(format!("cannot parse {}: {e}", path.display())))
}

/// The RUNG the same judgement set exits on — the twin of [`render_human`], so the words and the
/// number are decided over the same `Vec<Judgement>` and cannot come to disagree about which
/// criterion is which.
///
/// ⚠ **The MAPPING is not here.** `crate::cmd::runs::failif::rung` owns it, beside the `Outcome`
/// whose declaration order IS the ranking; this function is the projection from a [`Judgement`] to
/// its outcome and nothing else. It used to carry a copy of the match, and so did
/// `crates/vike-cli/src/cmd/data/gate.rs` — so retuning this plane's vocabulary would have left
/// the data plane printing the old word, both compiling, both suites green.
fn rung(judgements: &[Judgement]) -> Exit {
    failif::rung(judgements.iter().map(|j| &j.outcome))
}

fn num(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_string(), |n| format!("{n:.4}"))
}

fn render_human(
    subject: &ScannedRun,
    baseline: &ScannedRun,
    against_selector: &str,
    judgements: &[Judgement],
    exit: Exit,
    available_keys: Option<&[String]>,
) {
    println!("gate {} against {against_selector}", subject.run_id);
    println!("  baseline run {}  ({})", baseline.run_id, baseline.manifest.finished_at);
    println!();
    println!(
        "  {:<24}{:>12}{:>12}{:>12}{:>12}   VERDICT",
        "CRITERION", "BASELINE", "RUN", "DELTA", "ALLOWED"
    );
    for j in judgements {
        println!(
            "  {:<24}{:>12}{:>12}{:>12}{:>12}   {}",
            j.criterion.to_string(),
            num(j.baseline),
            num(j.value),
            num(j.delta()),
            num(j.allowed),
            j.outcome.word()
        );
        // ⚠ The REASON goes on its own line under the row rather than being left to be discovered.
        // An unevaluated criterion whose row said only "unevaluated" is the shape that makes a
        // person believe the gate checked something.
        if let Outcome::Unevaluated(why) = &j.outcome {
            println!("  {:<24}{why}", "");
        }
    }
    if let Some(keys) = available_keys {
        println!();
        println!("  the run's report carries these numbers: {}", keys.join(", "));
    }
    println!();
    let n = judgements.iter().filter(|j| j.outcome != Outcome::Pass).count();
    println!("  {} — {n} of {} criteria", verdict_word(exit).to_uppercase(), judgements.len());
}

fn render_json(
    a: &ReadArgs,
    subject: &ScannedRun,
    baseline: &ScannedRun,
    against_selector: &str,
    judgements: &[Judgement],
    exit: Exit,
    available_keys: Option<&[String]>,
) {
    let criteria: Vec<Value> = judgements
        .iter()
        .map(|j| {
            serde_json::json!({
                "metric": j.criterion.metric,
                "key": j.criterion.key,
                "direction": if j.criterion.rise_is_bad { "rise_is_bad" } else { "fall_is_bad" },
                "tolerance": j.criterion.tolerance,
                "percent": j.criterion.percent,
                "baseline": j.baseline,
                "value": j.value,
                "delta": j.delta(),
                "allowed": j.allowed,
                "verdict": j.outcome.word(),
                "why": match &j.outcome {
                    Outcome::Unevaluated(why) => Value::String(why.clone()),
                    _ => Value::Null,
                },
            })
        })
        .collect();
    let doc = serde_json::json!({
        "schema": 1,
        "run": subject.run_id,
        "against": {
            "selector": against_selector,
            "run": baseline.run_id,
            "finished_at": baseline.manifest.finished_at,
        },
        "fail_if": a.fail_if,
        "verdict": verdict_word(exit),
        "criteria": criteria,
        // Present only when something went unevaluated — see the human renderer's own note. `null`
        // rather than an omitted key, so a reader's key set does not change shape between runs.
        "available_keys": available_keys,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("vike-cli backtest gate: cannot render the JSON document: {e}"),
    }
}

/// The parse-time refusals this verb owns, applied by `crate::cmd::backtest`'s `parse_read`.
///
/// ⚠ **`--against` is REQUIRED, and the reason is that the criterion grammar is RELATIVE ONLY.** A
/// gate with no baseline would need an absolute form (`sharpe:>1.0`), which
/// `crate::cmd::runs::failif` argues against; so omitting it is a usage error that names `tag --as`
/// as how to make a stable baseline, and names `--json | jq` as the escape for the absolute question
/// this grammar cannot spell.
pub(crate) fn refuse_an_ungateable_line(a: &ReadArgs) -> Result<(), String> {
    if a.against.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(
            "`backtest gate` needs `--against <run|mark>` — every criterion is RELATIVE to a \
             baseline, so there is nothing to judge without one. Make a stable baseline with \
             `vike-cli backtest tag <run> --as baseline/<name>`, then gate `--against \
             @baseline/<name>`. (For an ABSOLUTE threshold this grammar cannot spell, pipe \
             `vike-cli backtest show <run> --json` through `jq`.)"
                .to_string(),
        );
    }
    if a.fail_if.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err(
            "`backtest gate` needs `--fail-if EXPR` — a gate with no criteria would exit 0 having \
             checked nothing, which is the one answer a CI step must never get. For example: \
             `--fail-if 'sharpe:-5%,max_dd:+10%'`."
                .to_string(),
        );
    }
    // Parsed HERE as well as in `run_gate`, so a malformed expression is refused on the usage rung
    // before any directory is opened — a command line the operator must fix should never produce a
    // message about the store.
    parse_fail_if(a.fail_if.as_deref().unwrap_or_default())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::backtest::ReadSub;
    use crate::cmd::runs::failif::Criterion;

    fn args() -> ReadArgs {
        let mut a = ReadArgs::empty(ReadSub::Gate);
        a.selector = Some("@last".to_string());
        a
    }

    fn judged(outcomes: &[Outcome]) -> Vec<Judgement> {
        outcomes
            .iter()
            .map(|o| Judgement {
                criterion: Criterion {
                    metric: "sharpe".into(),
                    key: "sharpe".into(),
                    rise_is_bad: false,
                    tolerance: 5.0,
                    percent: true,
                },
                baseline: Some(1.0),
                value: Some(1.0),
                allowed: Some(0.95),
                outcome: o.clone(),
            })
            .collect()
    }

    /// ⚠ **THE RUNG THIS VERB EXISTS FOR, and its twin.** A breach is its own number because the
    /// command WORKED; "nothing was evaluated" is its own number because a `0` there is the
    /// green-means-nothing-ran failure every gate in this repository is built against.
    #[test]
    fn each_outcome_set_collapses_onto_its_own_rung() {
        assert_eq!(rung(&judged(&[Outcome::Pass, Outcome::Pass])), Exit::Ok);
        assert_eq!(rung(&judged(&[Outcome::Breach, Outcome::Pass])), Exit::Breach);
        assert_eq!(rung(&judged(&[Outcome::Unevaluated("x".into()), Outcome::Pass])), Exit::Empty);
    }

    /// ⚠ A BREACH outranks an unevaluated criterion: a real failure must never be masked by a typo
    /// elsewhere in the same expression. Asserted in BOTH orders, because a `.max()` that had been
    /// replaced by "the first non-pass wins" would pass one of them.
    #[test]
    fn a_breach_outranks_an_unevaluated_criterion_in_either_order() {
        assert_eq!(
            rung(&judged(&[Outcome::Unevaluated("typo".into()), Outcome::Breach])),
            Exit::Breach
        );
        assert_eq!(
            rung(&judged(&[Outcome::Breach, Outcome::Unevaluated("typo".into())])),
            Exit::Breach
        );
    }

    /// An EMPTY judgement set is not a pass. Unreachable through the verb — `--fail-if` is required
    /// and an empty expression is refused — and pinned anyway, because the one thing this rung may
    /// never do is answer `0` for "nothing happened".
    #[test]
    fn no_criteria_at_all_is_not_a_pass() {
        assert_eq!(rung(&[]), Exit::Empty);
    }

    /// The word and the number are decided over ONE pass. A renderer that said "pass" while the
    /// process exited on a breach is the drift `trade_status`'s split exists to prevent.
    #[test]
    fn the_verdict_word_and_the_rung_are_the_same_decision() {
        for (outcomes, word) in [
            (vec![Outcome::Pass], "pass"),
            (vec![Outcome::Breach], "breach"),
            (vec![Outcome::Unevaluated("x".into())], "unevaluated"),
        ] {
            assert_eq!(verdict_word(rung(&judged(&outcomes))), word);
        }
    }

    /// A gate with no `--against` is a USAGE refusal naming how to MAKE a baseline — and the escape
    /// for the absolute question this grammar deliberately cannot spell.
    #[test]
    fn a_gate_with_no_baseline_names_how_to_make_one() {
        let mut a = args();
        a.fail_if = Some("sharpe:-5%".to_string());
        let e = refuse_an_ungateable_line(&a).expect_err("no baseline");
        assert!(e.contains("--against"), "{e}");
        assert!(e.contains("tag"), "it names the verb that makes a stable baseline: {e}");
        assert!(e.contains("jq"), "…and the escape for an absolute threshold: {e}");
    }

    /// A gate with no `--fail-if` would exit 0 having checked nothing — the one answer a CI step
    /// must never get, so it is refused at the door rather than evaluated to a pass.
    #[test]
    fn a_gate_with_no_criteria_is_refused_rather_than_passing() {
        let mut a = args();
        a.against = Some("@baseline/m".to_string());
        let e = refuse_an_ungateable_line(&a).expect_err("no criteria");
        assert!(e.contains("--fail-if"), "{e}");
        assert!(e.contains("checked nothing"), "{e}");
    }

    /// A malformed expression is refused at PARSE time, so a command line the operator must fix
    /// never produces a message about the store.
    #[test]
    fn a_malformed_expression_is_refused_before_any_directory_is_opened() {
        let mut a = args();
        a.against = Some("@baseline/m".to_string());
        a.fail_if = Some("sharpe:5%".to_string());
        let e = refuse_an_ungateable_line(&a).expect_err("no sign");
        assert!(e.contains("SIGN"), "{e}");
    }

    /// The whole line, well-formed, is accepted — the negative control for the three refusals above.
    #[test]
    fn a_complete_gate_line_is_accepted() {
        let mut a = args();
        a.against = Some("@baseline/m".to_string());
        a.fail_if = Some("sharpe:-5%,max_dd:+10%".to_string());
        assert!(refuse_an_ungateable_line(&a).is_ok());
    }
}
