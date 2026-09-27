//! `vike-cli backtest diff <a> <b>` — what differed in the INPUTS beside what differed in the
//! OUTPUTS (`docs/superpowers/specs/2026-09-12-backtest-cli-surface-design.md` §6.3).
//!
//! # It diffs the DOCUMENT, not a list of fields, and that is the load-bearing decision
//!
//! §6.3 argues the input half is "a real diff rather than a guess" BECAUSE the resolved config, the
//! data fingerprint and the build stamp are in the run record. When this was designed, none of the
//! three was: `crates/vike-backtest/src/backtest_cli.rs`'s `persist_run` wrote `git_sha: None`
//! outright and there was no resolved config anywhere — only the profile path the operator typed.
//!
//! So this walks the two JSON documents themselves. `crate::cmd::runs::jsondoc`'s `leaves` flattens
//! each to sorted dotted leaf paths and the diff walks two sorted lists in lockstep. Three
//! consequences, and the third is why the design did not have to wait:
//!
//! * it diffs exactly what is RECORDED today — the profile path and name, the kind, the producer,
//!   the git sha, the fingerprint, the strategy, the data window, the series, the store root;
//! * every field the run record GAINS joins the diff with no edit here, so widening the record
//!   cannot make this verb obsolete, only richer;
//! * it works on a research run, and on a kind invented next year — the same property
//!   `crates/vike-studio-core/src/listing.rs` has, for the same reason. That stopped being
//!   hypothetical when `research` became a top-level plane: `kind: "study"` runs
//!   (`crates/vike-studio-core/src/study_run.rs`'s `STUDY_RUN_KIND`) are a first-class product this
//!   verb is expected to diff, and a typed comparison of `BacktestReport`'s named fields would have
//!   needed a second arm for them.
//!
//! What it costs: no field gets a bespoke renderer. A moved data window renders as two changed leaf
//! rows (`detail.data.from`, `detail.data.to`) rather than as one "the window moved" sentence.
//!
//! # ⚠ Arrays are INDEX-keyed, never compared as sets
//!
//! A reordered array renders as N changed rows rather than "reordered". Deliberate:
//! `per_symbol_pnl`'s ORDER is what `vike_model::py_sum` bit-parity depends on, so a set comparison
//! would hide a real change in the one place this workspace cares most about ordering.
//!
//! ⚠ The DECLARED RESIDUAL of that choice: an element INSERTED near the front of a long array
//! cascades into a changed row for every element after it. `--trades` is where that bites hardest —
//! a ledger whose first trade differs re-indexes the whole list — so the trades section prints the
//! two lengths first, which is the fact that explains the cascade.
//!
//! # ⚠ A diff NEVER breaches
//!
//! Judging is `crate::cmd::runs::gate`'s job. A `diff` that could exit on `Exit::Breach` would give
//! a CI step two verbs producing one number for different reasons, and the number would stop meaning
//! anything. Two identical runs diff to nothing and exit `0`: "no change" is an answer.
//!
//! # ⚠ It reads `report.json` as a `Value`
//!
//! `crates/vike-analytics/src/report.rs`'s `BacktestReport` derives `Serialize` only, and this crate
//! does not link that one at all. That is not a workaround here — a document diff has no use for a
//! typed struct, and a typed one would have to be edited every time the report grows.

use serde_json::Value;
use vike_model::runs::{MANIFEST_FILE, REPORT_FILE, TRADES_FILE};

use crate::cmd::backtest::ReadArgs;
use crate::cmd::runs::scan::{ScannedRun, scan_runs};
use crate::cmd::runs::{Ctx, jsondoc, selector::resolve_one};
use crate::exit::{CliError, CmdResult};

/// Manifest keys EXCLUDED from the input diff, because they differ between any two runs by
/// construction.
///
/// They are not hidden: they render in the HEADER, which is where identity belongs. Leaving them in
/// the diff would mean three guaranteed rows in a view whose entire job is removing noise, and a
/// reader who has learned to skip the first three rows of every diff is a reader who will skip a
/// fourth.
const VOLATILE: &[&str] = &["run_id", "started_at", "finished_at"];

/// One leaf, on both sides.
struct Row {
    key: String,
    a: Option<Value>,
    b: Option<Value>,
}

impl Row {
    /// `+` added, `-` removed, `~` changed, ` ` same — one spelling for the table and the JSON.
    fn class(&self) -> &'static str {
        match (&self.a, &self.b) {
            (None, Some(_)) => "added",
            (Some(_), None) => "removed",
            (Some(x), Some(y)) if x != y => "changed",
            _ => "same",
        }
    }

    fn changed(&self) -> bool {
        self.class() != "same"
    }

    /// The percent move, when both sides are finite numbers and the left is not zero.
    ///
    /// `None` at a zero baseline rather than an infinity: "it went from 0 to 4" has no percentage,
    /// and printing one would be a number nobody can act on.
    fn percent(&self) -> Option<f64> {
        let (x, y) = (self.a.as_ref()?.as_f64()?, self.b.as_ref()?.as_f64()?);
        (x.is_finite() && y.is_finite() && x != 0.0).then(|| (y - x) / x.abs() * 100.0)
    }
}

/// Walk two sorted leaf lists in lockstep. Linear, and stable because both sides are sorted.
fn diff_docs(a: &Value, b: &Value, skip: &[&str]) -> Vec<Row> {
    let (la, lb) = (jsondoc::leaves(a), jsondoc::leaves(b));
    let keep = |k: &str| !skip.contains(&k);
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < la.len() || j < lb.len() {
        let row = match (la.get(i), lb.get(j)) {
            (Some((ka, va)), Some((kb, vb))) if ka == kb => {
                i += 1;
                j += 1;
                Row { key: ka.clone(), a: Some(va.clone()), b: Some(vb.clone()) }
            }
            (Some((ka, va)), Some((kb, _))) if ka < kb => {
                i += 1;
                Row { key: ka.clone(), a: Some(va.clone()), b: None }
            }
            (Some(_), Some((kb, vb))) => {
                j += 1;
                Row { key: kb.clone(), a: None, b: Some(vb.clone()) }
            }
            (Some((ka, va)), None) => {
                i += 1;
                Row { key: ka.clone(), a: Some(va.clone()), b: None }
            }
            (None, Some((kb, vb))) => {
                j += 1;
                Row { key: kb.clone(), a: None, b: Some(vb.clone()) }
            }
            (None, None) => break,
        };
        if keep(&row.key) {
            out.push(row);
        }
    }
    out
}

pub(crate) fn run_diff(ctx: &Ctx<'_>, a: &ReadArgs) -> CmdResult<()> {
    let root = ctx.runs_root.ok_or_else(|| {
        CliError::failed(
            "no project directory above the working directory, so there is no runs directory to \
             compare in. `vike-cli init` creates one, or name it with VIKE_USER_DATA_DIR.",
        )
    })?;
    let scan = scan_runs(root);
    for problem in &scan.problems {
        eprintln!("vike-cli backtest diff: {problem}");
    }
    let left = resolve_one(&scan, ctx.marks_root, a.selector.as_deref().unwrap_or_default())?;
    let right = resolve_one(&scan, ctx.marks_root, a.file.as_deref().unwrap_or_default())?;

    let inputs = diff_docs(
        &serde_json::to_value(&left.manifest).unwrap_or(Value::Null),
        &serde_json::to_value(&right.manifest).unwrap_or(Value::Null),
        VOLATILE,
    );
    let outputs = diff_docs(&read_report(left)?, &read_report(right)?, &[]);
    let trades = if a.trades {
        Some(diff_docs(&read_doc(left, TRADES_FILE)?, &read_doc(right, TRADES_FILE)?, &[]))
    } else {
        None
    };

    let notes = unattributable(&left.manifest, &right.manifest);
    let show_all = a.all;
    if a.json {
        render_json(left, right, &inputs, &outputs, trades.as_deref(), &notes, show_all);
    } else if a.md {
        render_md(left, right, &inputs, &outputs, trades.as_deref(), &notes, show_all);
    } else {
        render_human(left, right, &inputs, &outputs, trades.as_deref(), &notes, show_all);
    }
    Ok(())
}

fn read_report(run: &ScannedRun) -> Result<Value, CliError> {
    read_doc(run, REPORT_FILE)
}

/// One document out of a run directory, as a `Value`.
///
/// ⚠ An ABSENT document is [`crate::exit::Exit::Empty`], not a failure and not an empty diff: "there
/// was nothing to compare" and "the two are identical" must never look the same to a script. That is
/// this verb's only non-zero rung.
fn read_doc(run: &ScannedRun, file: &str) -> Result<Value, CliError> {
    let path = run.dir.join(file);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CliError::empty(format!(
                "run {} has no {file} — there is nothing to compare. An empty diff and an \
                 impossible one are different answers.",
                run.run_id
            )));
        }
        Err(e) => {
            return Err(CliError::failed(format!("cannot read {}: {e}", path.display())));
        }
    };
    serde_json::from_str(&text)
        .map_err(|e| CliError::failed(format!("cannot parse {}: {e}", path.display())))
}

/// ⚠ **What the record CANNOT attribute, said by name rather than by omission.**
///
/// Without these lines a diff showing only a config change reads as "the config is why", when the
/// truth is that the other two causes were never recorded. Both conditions are properties of the
/// DOCUMENTS rather than version checks, so each line disappears on its own the day a producer fills
/// the field in.
///
/// ⚠ **An ABSENT fingerprint is not a DIFFERENT one, and a DIFFERENT one is not proof the data
/// moved.** Both halves are stated because both are inferences a reader makes unprompted, and both
/// are wrong:
///
/// * **absent** — `vike_model::runs::RunManifest::fingerprint` is `None` for more than one reason
///   and this verb cannot tell them apart from the document: a producer that computes no address at
///   all, and a producer whose STORE could not be inventoried (it refuses to address a run whose
///   data slice it could not read, rather than addressing it wrongly). So a null means "we do not
///   know", and this note may not name one cause as though it were the only one. It did, and the
///   sentence was already a third of the truth when it was written.
///
///   ⚠ **A SEARCH PARENT was the third cause and has stopped being one.** It carried no address
///   because `crates/vike-backtest/src/backtest_cli.rs` collected the fingerprint below its sweep
///   branch to keep a search's manifest reads down, and
///   `vike_data::DataFusionHist::series_facts` removed the read that cost bought. A search run now
///   addresses its inputs exactly as a single run does. The note is written with no COUNT in it
///   this time: the previous two spellings of this paragraph both carried one and both were wrong.
/// * **different** — a GROUPED series contributes its WHOLE GROUP's coverage to the address, so an
///   unrelated symbol's rows landing in the same group move it while this run's inputs are
///   byte-identical (spec §18 row 10). A differing address is a reason to LOOK, never a finding on
///   its own.
fn unattributable(
    a: &vike_model::runs::RunManifest,
    b: &vike_model::runs::RunManifest,
) -> Vec<String> {
    let mut out = Vec::new();
    if a.git_sha.is_none() && b.git_sha.is_none() {
        out.push(
            "no build stamp on either run — a metric move cannot be attributed to a code change"
                .to_string(),
        );
    }
    match (&a.fingerprint, &b.fingerprint) {
        (Some(x), Some(y)) if x != y => out.push(
            "the two runs carry DIFFERENT data fingerprints — worth looking at, but not a finding \
             on its own: a grouped series contributes its whole group's coverage, so an unrelated \
             symbol's rows move the address while these inputs are identical"
                .to_string(),
        ),
        (Some(_), Some(_)) => {}
        _ => out.push(
            "no data fingerprint on at least one run — a metric move cannot be attributed to a \
             data change, and a MISSING address is not a different one. A null has more than one \
             cause this document cannot tell apart: a producer that computes no address, and a \
             store that could not be inventoried"
                .to_string(),
        ),
    }
    out
}

fn render_value(v: Option<&Value>) -> String {
    match v {
        None => "—".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn section(title: &str, rows: &[Row], show_all: bool) {
    let shown: Vec<&Row> = rows.iter().filter(|r| show_all || r.changed()).collect();
    println!();
    println!("{title}");
    if shown.is_empty() {
        println!("  no change");
        return;
    }
    for r in shown {
        let sign = match r.class() {
            "added" => '+',
            "removed" => '-',
            "changed" => '~',
            _ => ' ',
        };
        let pct = r.percent().map_or_else(String::new, |p| format!("   ({p:+.2}%)"));
        if r.class() == "same" {
            println!("  {sign} {:<28}{}", r.key, render_value(r.a.as_ref()));
        } else {
            println!(
                "  {sign} {:<28}{} → {}{pct}",
                r.key,
                render_value(r.a.as_ref()),
                render_value(r.b.as_ref())
            );
        }
    }
}

fn header(left: &ScannedRun, right: &ScannedRun) {
    println!("diff {} → {}", left.run_id, right.run_id);
    // ⚠ The VOLATILE keys live here, which is why excluding them from the rows hides nothing.
    println!(
        "  a  {}  {}  {}",
        left.manifest.finished_at,
        left.manifest.kind,
        left.manifest.config.path.as_deref().unwrap_or("—")
    );
    println!(
        "  b  {}  {}  {}",
        right.manifest.finished_at,
        right.manifest.kind,
        right.manifest.config.path.as_deref().unwrap_or("—")
    );
}

fn render_human(
    left: &ScannedRun,
    right: &ScannedRun,
    inputs: &[Row],
    outputs: &[Row],
    trades: Option<&[Row]>,
    notes: &[String],
    show_all: bool,
) {
    header(left, right);
    section(&format!("INPUTS ({MANIFEST_FILE})"), inputs, show_all);
    for note in notes {
        println!("  ⓘ {note}");
    }
    section(&format!("OUTPUTS ({REPORT_FILE})"), outputs, show_all);
    if let Some(rows) = trades {
        section(&format!("TRADES ({TRADES_FILE})"), rows, show_all);
    }
    if inputs.iter().chain(outputs).all(|r| !r.changed())
        && trades.is_none_or(|t| t.iter().all(|r| !r.changed()))
    {
        println!();
        println!("the two runs are identical in every recorded field — no change");
    }
}

fn render_md(
    left: &ScannedRun,
    right: &ScannedRun,
    inputs: &[Row],
    outputs: &[Row],
    trades: Option<&[Row]>,
    notes: &[String],
    show_all: bool,
) {
    println!("# diff {} → {}", left.run_id, right.run_id);
    let md_section = |title: &str, rows: &[Row]| {
        println!();
        println!("## {title}");
        println!();
        println!("| | key | a | b |");
        println!("|---|---|---|---|");
        let mut any = false;
        for r in rows.iter().filter(|r| show_all || r.changed()) {
            any = true;
            println!(
                "| {} | `{}` | {} | {} |",
                r.class(),
                r.key,
                render_value(r.a.as_ref()),
                render_value(r.b.as_ref())
            );
        }
        if !any {
            println!("| | _no change_ | | |");
        }
    };
    md_section(&format!("INPUTS ({MANIFEST_FILE})"), inputs);
    for note in notes {
        println!();
        println!("> {note}");
    }
    md_section(&format!("OUTPUTS ({REPORT_FILE})"), outputs);
    if let Some(rows) = trades {
        md_section(&format!("TRADES ({TRADES_FILE})"), rows);
    }
}

fn rows_json(rows: &[Row], show_all: bool) -> Vec<Value> {
    rows.iter()
        .filter(|r| show_all || r.changed())
        .map(|r| {
            serde_json::json!({
                "key": r.key,
                "change": r.class(),
                "a": r.a,
                "b": r.b,
                "percent": r.percent(),
            })
        })
        .collect()
}

fn render_json(
    left: &ScannedRun,
    right: &ScannedRun,
    inputs: &[Row],
    outputs: &[Row],
    trades: Option<&[Row]>,
    notes: &[String],
    show_all: bool,
) {
    let doc = serde_json::json!({
        "schema": 1,
        "a": { "run": left.run_id, "finished_at": left.manifest.finished_at },
        "b": { "run": right.run_id, "finished_at": right.manifest.finished_at },
        "inputs": rows_json(inputs, show_all),
        "outputs": rows_json(outputs, show_all),
        "trades": trades.map(|rows| rows_json(rows, show_all)),
        "notes": notes,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("vike-cli backtest diff: cannot render the JSON document: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::runs::{MANIFEST_SCHEMA, RunConfig, RunManifest};

    fn doc(sharpe: f64, profile: &str) -> Value {
        serde_json::json!({
            "sharpe": sharpe,
            "win_rate": 0.51,
            "config": { "path": profile },
            "run_id": "1756000000-1-0",
            "started_at": "2025-08-24T01:46:40Z",
        })
    }

    /// The lockstep walk classifies every leaf, on both sides, once.
    #[test]
    fn a_leaf_on_one_side_only_is_added_or_removed_and_never_changed() {
        let a = serde_json::json!({ "only_a": 1, "both": 2 });
        let b = serde_json::json!({ "both": 3, "only_b": 4 });
        let rows = diff_docs(&a, &b, &[]);
        let classes: Vec<(&str, &str)> = rows.iter().map(|r| (r.key.as_str(), r.class())).collect();
        assert_eq!(
            classes,
            vec![("both", "changed"), ("only_a", "removed"), ("only_b", "added")],
            "sorted, and each classified once"
        );
    }

    /// ⚠ `run_id`, `started_at` and `finished_at` differ between ANY two runs, so they are excluded
    /// from the INPUT diff and rendered in the header instead. Otherwise three guaranteed rows drown
    /// the real signal in a view whose whole job is removing noise.
    #[test]
    fn the_always_different_keys_are_skipped_in_the_input_diff() {
        let rows = diff_docs(&doc(1.8, "m.toml"), &doc(1.4, "m-v2.toml"), VOLATILE);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(!keys.contains(&"run_id"), "{keys:?}");
        assert!(!keys.contains(&"started_at"), "{keys:?}");
        assert!(keys.contains(&"config.path"), "…and the real signal survives: {keys:?}");
        // The negative control: WITHOUT the skip list they are rows, so the filter is what removes
        // them rather than the fixture happening not to have them.
        let unskipped = diff_docs(&doc(1.8, "m.toml"), &doc(1.4, "m-v2.toml"), &[]);
        assert!(unskipped.iter().any(|r| r.key == "run_id"));
    }

    /// A percent move is computed against `|a|`, and is absent at a zero baseline rather than
    /// infinite — "it went from 0 to 4" has no percentage, and printing one is a number nobody can
    /// act on.
    #[test]
    fn a_percent_move_is_absent_at_a_zero_baseline() {
        let rows = diff_docs(
            &serde_json::json!({ "x": 0.0, "y": 2.0 }),
            &serde_json::json!({ "x": 4.0, "y": 1.0 }),
            &[],
        );
        let x = rows.iter().find(|r| r.key == "x").unwrap();
        let y = rows.iter().find(|r| r.key == "y").unwrap();
        assert_eq!(x.percent(), None);
        assert_eq!(y.percent(), Some(-50.0));
    }

    /// A NEGATIVE left-hand value still gives a percentage with the right SIGN: `|a|` in the
    /// denominator, the same rule `failif`'s tolerance uses and for the same reason.
    #[test]
    fn a_percent_move_from_a_negative_value_keeps_its_direction() {
        let rows =
            diff_docs(&serde_json::json!({ "s": -0.5 }), &serde_json::json!({ "s": -0.6 }), &[]);
        let p = rows[0].percent().unwrap();
        assert!(p < 0.0, "a fall from -0.5 to -0.6 is a FALL, not a rise: {p}");
        assert!((p + 20.0).abs() < 1e-9, "{p}");
    }

    fn manifest(git: Option<&str>, fp: Option<&str>) -> RunManifest {
        RunManifest {
            schema: MANIFEST_SCHEMA,
            run_id: "1756000000-1-0".into(),
            kind: "backtest".into(),
            produced_by: "backtest".into(),
            started_at: "2025-08-24T01:46:40Z".into(),
            finished_at: "2025-08-24T01:46:41Z".into(),
            git_sha: git.map(str::to_string),
            fingerprint: fp.map(str::to_string),
            config: RunConfig { path: Some("m.toml".into()), name: None },
            detail: Value::Null,
        }
    }

    /// ⚠ **A diff that cannot attribute a move SAYS SO.** Without this, a diff showing only a config
    /// change reads as "the config is why", when the truth is that the other two causes were never
    /// recorded.
    #[test]
    fn a_diff_names_the_attributions_the_record_cannot_support() {
        let notes = unattributable(&manifest(None, None), &manifest(None, None));
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("build stamp")), "{notes:?}");
        assert!(notes.iter().any(|n| n.contains("fingerprint")), "{notes:?}");
    }

    /// ⚠ **A MISSING fingerprint on ONE side is enough to warn, and the warning may not name ONE
    /// cause.** A null has more than one this document cannot tell apart — a producer that computes
    /// no address, and a store that could not be inventoried. The sentence named the second one
    /// alone and was a fraction of the truth the day it was written.
    ///
    /// ⚠ A SEARCH PARENT was a third cause until `vike_data::DataFusionHist::series_facts` let
    /// `crates/vike-backtest/src/backtest_cli.rs` address a search for the price of the data
    /// witness it was already paying. It is dropped from this list rather than kept "for old
    /// documents": the note is advice to a reader about what a null might mean NOW, and listing a
    /// cause the producer can no longer exhibit sends them looking for a sweep that is not there.
    #[test]
    fn one_missing_fingerprint_is_enough_and_the_note_names_every_cause() {
        let notes =
            unattributable(&manifest(Some("abc"), Some("dead")), &manifest(Some("abc"), None));
        assert_eq!(notes.len(), 1, "the build stamp is on both sides: {notes:?}");
        assert!(notes[0].contains("not a different one"), "it says what absent MEANS: {notes:?}");
        for cause in ["computes no address", "could not be inventoried"] {
            assert!(notes[0].contains(cause), "the note omits `{cause}`: {notes:?}");
        }
        assert!(
            !notes[0].contains("search parent"),
            "a search run addresses its inputs now — naming it as a cause of a null sends the \
             reader to a sweep that is not there: {notes:?}"
        );
    }

    /// ⚠ **A DIFFERING fingerprint is not proof the data moved either** — the second half of the
    /// same honesty requirement. A GROUPED series contributes its WHOLE GROUP's coverage to the
    /// address, so an unrelated symbol's rows move it while this run's inputs are byte-identical
    /// (spec §18 row 10).
    #[test]
    fn two_different_fingerprints_are_a_reason_to_look_rather_than_a_finding() {
        let notes = unattributable(
            &manifest(Some("abc"), Some("dead")),
            &manifest(Some("abc"), Some("beef")),
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("DIFFERENT"), "{notes:?}");
        assert!(notes[0].contains("grouped series"), "it says WHY it is not a finding: {notes:?}");
        assert!(
            !notes[0].contains("search parent"),
            "…and does not recite the ABSENT-address causes, which do not apply: {notes:?}"
        );
    }

    /// Both facts recorded and AGREEING on both sides: no notes at all. The negative control — every
    /// test above would pass against a function that always warned.
    #[test]
    fn a_fully_recorded_agreeing_pair_carries_no_attribution_notes() {
        let notes = unattributable(
            &manifest(Some("abc"), Some("dead")),
            &manifest(Some("def"), Some("dead")),
        );
        assert!(notes.is_empty(), "{notes:?}");
    }
}
