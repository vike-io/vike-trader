//! The two global files the planner judges PART BY PART (decision 0142): `ci.yml` by job block, the
//! feature-suite script by `case` arm.
//!
//! # The contract
//!
//! Both files stay global by PATH ([`tables::escalation::NARROWED_GLOBAL_FILES`]): an edit the readers
//! below cannot split is the full escalation it always was. What they can split is a CONTENT exception,
//! in the family of the additive `Cargo.lock` and the `#`-comment edit:
//!
//!   * **`ci.yml`** ([`workflow_jobs_moved`]): an edit inside job X's block fires X's plan outputs
//!     ([`tables::escalation::CI_WORKFLOW_JOBS`]) and nothing else. It ESCALATES when the workflow's text
//!     outside every block moved (the `on:`/`env:`/`concurrency:`/`permissions:`/`defaults:` header, a
//!     top-level key; whole-line comments there do not count), when the `plan:` block moved, when a
//!     job-level `needs:` or `if:` of ANY job moved (the wiring between jobs), when a job was added,
//!     removed, renamed or reordered, when a moved job has no table row, and when either side cannot be
//!     read or split. `.github/actions/**` is a separate global file and escalates on its own.
//!     A block is compared as [`yaml_comments::comment_only`] reads it (decision 0148): a whole-line
//!     comment or a blank line inside it moves nothing, except in a `run: |` script that could make it data
//!     (a heredoc, a quote left open across lines, a backslash-quote). A SIDE job (a row of several outputs:
//!     `side-a`, `side-b`, whose steps were jobs until 2026-11-22) fires only the parts whose [`output_view`]
//!     moved: an edit of one part's steps fires that part, an edit of the rest of its block every part.
//!   * **the suite script** ([`suite_arms_moved`]): an edit inside one `  key)` arm fires that suite; an
//!     edit of the frame (everything above the `case`, the `case` line, the bottom from `esac`) or of
//!     the arm list (an arm added, removed, renamed or reordered) fires EVERY suite, the `features`
//!     legs being the only CI jobs that run the script; a script it cannot read or split escalates.
//!
//! Each narrowed edit plans the gate crates and the file's readers into the lane (the caller does
//! that, `plan::compute_with`), so every gate that reads the file runs as for an exempted file.
//!
//! # What the job that runs the edit proves
//!
//! An edit of a job's block runs THAT job on its own change: the edited steps execute, which is the
//! proof they work. A suite arm runs in `side-b`'s `features` step, for its key. What a block edit cannot show
//! is an effect on ANOTHER job, and every path by which one job's text reaches another (a `needs:`, an
//! `if:` reading another job's result or output, the `plan:` outputs, a workflow-level `env:`, a YAML
//! anchor) is one of the escalations above.

use std::collections::BTreeSet;
use std::path::Path;

use super::git;
use super::selection::job_block;
use super::tables;
use super::tables::escalation::{JobFires, PlanOutput};
use super::yaml_comments;

/// `ci.yml` split into what [`workflow_jobs_moved`] compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowJobs {
    /// The text outside every job block, line by line: the header up to and including `jobs:`, and the
    /// gap lines between blocks (each job's opening comment). Blank lines dropped; whole-line comments
    /// dropped too unless a line of it opens a YAML block scalar (then a `#` line could be data).
    pub rest: Vec<String>,
    /// `(key, block)` in file order, each block exactly [`job_block`]'s text for that key.
    pub jobs: Vec<(String, String)>,
}

/// The job key a `  <key>:` line opens, or `None` for any other line.
fn job_header(line: &str) -> Option<&str> {
    let key = line.trim_end().strip_prefix("  ")?.strip_suffix(':')?;
    let ok = !key.is_empty()
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !key.starts_with('-');
    ok.then_some(key)
}

/// The number of leading spaces.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// True for a line that opens a YAML block scalar (`key: |`, `key: >-`, `- |`), whose body may hold
/// lines that only LOOK like comments.
fn opens_block_scalar(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with('#') {
        return false;
    }
    let item = t.strip_prefix("- ").map_or(t, str::trim_start);
    let value = item.split_once(": ").map_or(item, |(_, v)| v).trim_start();
    value.starts_with('|') || value.starts_with('>')
}

/// True when a line carries a YAML anchor (`&name`) or alias (`*name`) in a value position, or a merge
/// key: one block's text could then stand in another's.
fn has_anchor_or_alias(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with('#') {
        return false;
    }
    if t.starts_with("<<:") {
        return true;
    }
    let b = t.as_bytes();
    (2..b.len()).any(|i| {
        matches!(&b[i - 2..i], b": " | b"- ")
            && (b[i] == b'&' || b[i] == b'*')
            && b.get(i + 1).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
    })
}

/// Split a workflow into its job blocks and the rest, or `None` when the reader is not sure of the
/// split: no `jobs:` line or two, a tab, an anchor or alias anywhere, a top-level key after `jobs:`, a
/// line indented less than a block's body that opens no job and is no comment, a body line before the
/// first job or after a gap comment, a repeated key.
pub fn workflow_jobs(text: &str) -> Option<WorkflowJobs> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.iter().any(|l| l.trim_start_matches(' ').starts_with('\t')) {
        return None;
    }
    if lines.iter().any(|l| has_anchor_or_alias(l)) {
        return None;
    }
    let mut at_jobs = lines.iter().enumerate().filter(|(_, l)| l.trim_end() == "jobs:");
    let (at, _) = at_jobs.next()?;
    if at_jobs.next().is_some() {
        return None;
    }
    let mut rest: Vec<&str> = lines[..=at].to_vec();
    let mut keys: Vec<String> = Vec::new();
    let mut in_job = false;
    for line in &lines[at + 1..] {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if indent(line) <= 2 {
            if let Some(key) = job_header(line) {
                if keys.iter().any(|k| k == key) {
                    return None;
                }
                keys.push(key.to_string());
                in_job = true;
            } else if t.starts_with('#') {
                in_job = false;
                rest.push(*line);
            } else {
                return None;
            }
        } else if !in_job {
            return None;
        }
    }
    // Each block is the latency gate's reader over the text AFTER `jobs:`, so a header key under `on:`
    // that shares a job's name can never be read as that job.
    let region = lines[at + 1..].join("\n");
    let jobs = keys
        .into_iter()
        .map(|k| job_block(&region, &k).map(|b| (k, b)))
        .collect::<Option<Vec<_>>>()?;
    let raw = rest.iter().any(|l| opens_block_scalar(l));
    let rest = rest
        .into_iter()
        .filter(|l| !l.trim().is_empty() && (raw || !l.trim_start().starts_with('#')))
        .map(str::to_string)
        .collect();
    Some(WorkflowJobs { rest, jobs })
}

/// A job's WIRING: its job-level `needs:` and `if:` entries (the lines at the block's body indent that
/// open them, and every deeper line that continues one), in order. A moved wiring can change what
/// ANOTHER job sees, so [`workflow_jobs_moved`] escalates on it.
pub fn job_wiring(block: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut taking = false;
    for line in block.lines().skip(1) {
        if line.trim().is_empty() {
            continue;
        }
        let i = indent(line);
        let t = line.trim_start();
        // A job-level comment line is no part of an entry and does not close one: a comment between two
        // lines of a multi-line `if:` must not cut its continuation out.
        if i <= 4 && t.starts_with('#') {
            continue;
        }
        if i <= 4 {
            taking = i == 4 && (t.starts_with("needs:") || t.starts_with("if:"));
        }
        if taking {
            out.push(line);
        }
    }
    out.join("\n")
}

/// The answer for a global file judged part by part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartEdit {
    /// The readers cannot split the edit, or it moved a part every job depends on: the escalation.
    Whole,
    /// The parts (job keys, or suite keys) the edit moved; empty when only text no part owns moved.
    Parts(BTreeSet<String>),
}

/// What an edit of `ci.yml` moved, from its text at the diff base (`old`) and at HEAD (`new`), both
/// from git. [`PartEdit::Parts`] holds the keys of the jobs whose block text differs; every other answer
/// is [`PartEdit::Whole`] — the rules are the module doc's, and **every doubt is `Whole`**: no text on
/// either side (a new, deleted or unreadable workflow), a side [`workflow_jobs`] cannot split, the rest
/// moved, the job list moved, a job's [`job_wiring`] moved, or a moved job whose
/// [`tables::escalation::CI_WORKFLOW_JOBS`] row is missing or [`JobFires::Everything`].
pub fn workflow_jobs_moved(old: Option<&str>, new: Option<&str>) -> PartEdit {
    let (Some(old), Some(new)) = (old, new) else { return PartEdit::Whole };
    let (Some(a), Some(b)) = (workflow_jobs(old), workflow_jobs(new)) else {
        return PartEdit::Whole;
    };
    if a.rest != b.rest {
        return PartEdit::Whole;
    }
    let keys = |w: &WorkflowJobs| w.jobs.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    if keys(&a) != keys(&b) {
        return PartEdit::Whole;
    }
    let mut moved = BTreeSet::new();
    for ((key, old_block), (_, new_block)) in a.jobs.iter().zip(&b.jobs) {
        if job_wiring(old_block) != job_wiring(new_block) {
            return PartEdit::Whole;
        }
        // Comments and blank lines inside a block move nothing (decision 0148): a `run: |` script that could
        // print them as data is the one place they count, and `comment_only` knows it.
        if !yaml_comments::comment_only(old_block, new_block) {
            match job_fires(key) {
                Some(JobFires::Outputs(_)) => {
                    moved.insert(key.clone());
                }
                Some(JobFires::Everything) | None => return PartEdit::Whole,
            }
        }
    }
    PartEdit::Parts(moved)
}

/// The [`tables::escalation::CI_WORKFLOW_JOBS`] row of `key`.
pub fn job_fires(key: &str) -> Option<JobFires> {
    tables::escalation::CI_WORKFLOW_JOBS.iter().find(|(k, _)| *k == key).map(|(_, f)| *f)
}

/// The steps of a job block: `(start, end)` line ranges (end exclusive) of each `      - ` item under the
/// job's `    steps:` key, an item running to the next line at indent six or less (the next item, or a
/// comment between steps, which is no step's). Empty when the block has no `    steps:` line.
fn step_ranges(lines: &[&str]) -> Vec<(usize, usize)> {
    let Some(at) = lines.iter().position(|l| l.trim_end() == "    steps:") else {
        return Vec::new();
    };
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut open: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(at + 1) {
        if line.trim().is_empty() || indent(line) > 6 {
            continue;
        }
        if let Some(s) = open.take() {
            out.push((s, i));
        }
        if indent(line) == 6 && line.trim_start().starts_with("- ") {
            open = Some(i);
        }
    }
    if let Some(s) = open {
        out.push((s, lines.len()));
    }
    out
}

/// The plan outputs of `outputs` a step's `if:` reads: its `if:` line (on the item line or at the step's key
/// indent, eight) and every deeper line continuing it, searched for `needs.plan.outputs.<key>` followed by no
/// identifier character.
fn step_reads(step: &[&str], outputs: &[PlanOutput]) -> Vec<PlanOutput> {
    let mut cond = String::new();
    let mut taking = false;
    for (i, line) in step.iter().enumerate() {
        let t = line.trim_start();
        let key = if i == 0 { t.strip_prefix("- ").unwrap_or(t) } else { t };
        if i == 0 || indent(line) == 8 {
            taking = key.starts_with("if:");
        }
        if taking {
            cond.push_str(key);
            cond.push('\n');
        }
    }
    outputs
        .iter()
        .copied()
        .filter(|o| {
            let read = format!("needs.plan.outputs.{}", o.key());
            cond.match_indices(&read).any(|(at, _)| {
                cond[at + read.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            })
        })
        .collect()
}

/// What a SIDE job (a [`tables::escalation::CI_WORKFLOW_JOBS`] row of two or more `outputs`) runs for its
/// part `output`: the job's block with every step of ANOTHER part (a step whose `if:` reads exactly one of
/// `outputs`, and not `output`) cut to one placeholder line, so the order of the parts still shows. A step
/// that reads none of them, or several (the checkout, the setup, the verdict), is every part's.
pub fn output_view(block: &str, output: PlanOutput, outputs: &[PlanOutput]) -> String {
    let lines: Vec<&str> = block.lines().collect();
    let mut cut = vec![false; lines.len()];
    let mut marks = Vec::new();
    for (s, e) in step_ranges(&lines) {
        let reads = step_reads(&lines[s..e], outputs);
        if reads.len() == 1 && reads[0] != output {
            cut[s..e].iter_mut().for_each(|c| *c = true);
            marks.push(s);
        }
    }
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if marks.contains(&i) {
            out.push("      - (a step of another part)");
        } else if !cut[i] {
            out.push(line);
        }
    }
    out.join("\n")
}

/// The steps of a side job's part `output` alone, in order (the steps whose `if:` reads exactly that one of
/// `outputs`): what that part runs, which a reader of the part (a gate, the release's twin) reads as the old job.
pub fn output_steps(block: &str, output: PlanOutput, outputs: &[PlanOutput]) -> String {
    let lines: Vec<&str> = block.lines().collect();
    step_ranges(&lines)
        .into_iter()
        .filter(|(s, e)| step_reads(&lines[*s..*e], outputs) == [output])
        .flat_map(|(s, e)| lines[s..e].iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
}

/// True iff an edit of `ci.yml` (`old` at the diff base, `new` at HEAD, both from git) moved what job `key`
/// runs for its part `output` ([`output_view`] of each side's [`job_block`], compared as
/// [`yaml_comments::comment_only`] reads them). **FAIL SAFE:** no text on either side, or no block in
/// either, is `true`. A job of one output (or none) is its whole block.
pub fn output_moved(old: Option<&str>, new: Option<&str>, key: &str, output: PlanOutput) -> bool {
    let outputs = match job_fires(key) {
        Some(JobFires::Outputs(o)) => o,
        _ => &[][..],
    };
    let (Some(old), Some(new)) = (old, new) else { return true };
    match (job_block(old, key), job_block(new, key)) {
        (Some(a), Some(b)) => !yaml_comments::comment_only(
            &output_view(&a, output, outputs),
            &output_view(&b, output, outputs),
        ),
        _ => true,
    }
}

/// The suite script split into what [`suite_arms_moved`] compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteArms {
    /// Every line that is in no arm: the top (shebang, `set`, helpers, the loop) through the `case`
    /// line, anything between it and the first arm, and the bottom from `esac` on. Raw text.
    pub frame: String,
    /// `(key, text)` in file order: the `  key)` line through the line before the next arm or `esac`,
    /// trailing blank lines dropped. `*` is the default arm's key.
    pub arms: Vec<(String, String)>,
}

/// The key a `  key)` arm header opens: indent exactly two, a lowercase/digit/dash word or `*`, nothing
/// after the `)`.
fn arm_header(line: &str) -> Option<&str> {
    let rest = line.trim_end().strip_prefix("  ")?;
    let key = rest.strip_suffix(')')?;
    let ok = key == "*"
        || (!key.is_empty()
            && !key.starts_with('-')
            && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
    ok.then_some(key)
}

/// The number of whole words `word` in `line` (a word is a run of `[A-Za-z0-9_]`).
fn word_count(line: &str, word: &str) -> usize {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| *w == word).count()
}

/// True when a line holds a heredoc operator (`<<`, `<<-`, `<<'X'`), which a here-string (`<<<`) is not.
fn has_heredoc(line: &str) -> bool {
    let b = line.as_bytes();
    (0..b.len().saturating_sub(1)).any(|i| {
        b[i] == b'<'
            && b[i + 1] == b'<'
            && (i == 0 || b[i - 1] != b'<')
            && b.get(i + 2).is_none_or(|c| *c != b'<')
    })
}

/// Split the suite script into its frame and its arms, or `None` when this conservative lexer is not
/// sure: no dispatch `case` (a line at indent zero opening `case … in`) or two, a heredoc anywhere in
/// the `case`, an arm header that is not a bare `  key)`, a repeated key, a line indented less than an
/// arm body (four) that is neither an arm header, the closing `esac` nor a comment, a body line before
/// the first arm, a nested `case`/`esac` that does not balance inside its arm, and no closing `esac`.
pub fn suite_arms(text: &str) -> Option<SuiteArms> {
    let lines: Vec<&str> = text.lines().collect();
    let opens = |l: &str| l.starts_with("case ") && l.trim_end().ends_with(" in");
    let mut at_case = lines.iter().enumerate().filter(|(_, l)| opens(l));
    let (at, _) = at_case.next()?;
    if at_case.next().is_some() {
        return None;
    }
    let mut frame: Vec<&str> = lines[..=at].to_vec();
    let mut arms: Vec<(String, Vec<&str>)> = Vec::new();
    let mut depth: i64 = 0;
    let mut end = None;
    for (i, line) in lines.iter().enumerate().skip(at + 1) {
        let t = line.trim();
        if has_heredoc(line) {
            return None;
        }
        if t.is_empty() || t.starts_with('#') {
            match arms.last_mut() {
                Some((_, body)) => body.push(line),
                None => frame.push(line),
            }
            continue;
        }
        if depth == 0 && *line == "esac" {
            end = Some(i);
            break;
        }
        if depth == 0
            && let Some(key) = arm_header(line)
        {
            if arms.iter().any(|(k, _)| k == key) {
                return None;
            }
            arms.push((key.to_string(), vec![line]));
            continue;
        }
        if indent(line) < 4 {
            return None;
        }
        let (_, body) = arms.last_mut()?;
        depth += word_count(t, "case") as i64 - word_count(t, "esac") as i64;
        if depth < 0 {
            return None;
        }
        body.push(line);
    }
    let end = end?;
    if arms.is_empty() || depth != 0 {
        return None;
    }
    frame.extend(&lines[end..]);
    let arms =
        arms.into_iter().map(|(k, body)| (k, body.join("\n").trim_end().to_string())).collect();
    Some(SuiteArms { frame: frame.join("\n"), arms })
}

/// The answer for an edit of the suite script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmEdit {
    /// A side has no text, or does not split ([`suite_arms`]): the escalation.
    Doubt,
    /// The frame or the arm list moved: every suite runs the edited text.
    Every,
    /// Only these arms' text moved (empty: only blank lines between arms).
    Arms(BTreeSet<String>),
}

/// What an edit of the suite script moved, from its text at the diff base (`old`) and at HEAD (`new`),
/// both from git (the rules are the module doc's).
pub fn suite_arms_moved(old: Option<&str>, new: Option<&str>) -> ArmEdit {
    let (Some(old), Some(new)) = (old, new) else { return ArmEdit::Doubt };
    let (Some(a), Some(b)) = (suite_arms(old), suite_arms(new)) else { return ArmEdit::Doubt };
    let keys = |s: &SuiteArms| s.arms.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
    if a.frame != b.frame || keys(&a) != keys(&b) {
        return ArmEdit::Every;
    }
    let moved = a
        .arms
        .iter()
        .zip(&b.arms)
        .filter(|((_, x), (_, y))| x != y)
        .map(|((k, _), _)| k.clone())
        .collect();
    ArmEdit::Arms(moved)
}

/// What the part-judged files of one change fire, for `plan::compute_with`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartPlan {
    /// The [`tables::escalation::NARROWED_GLOBAL_FILES`] of the change that this run narrowed: no longer
    /// global edits.
    pub files: BTreeSet<String>,
    /// The plan outputs the moved job blocks fire ([`tables::escalation::CI_WORKFLOW_JOBS`]).
    pub outputs: BTreeSet<PlanOutput>,
    /// The suite keys to fire: every key for a moved `features:` block or a moved script frame or arm
    /// list, the moved arms' keys otherwise.
    pub suites: BTreeSet<String>,
    /// The suite keys whose OWN arm moved: those run on a pull request too, post-merge suites included:
    /// the arm is the suite's own machinery, and its leg is the only job that runs it.
    pub own_arms: BTreeSet<String>,
}

/// Judge every [`tables::escalation::NARROWED_GLOBAL_FILES`] entry of `files`, except those in `skip`
/// (the `#`-comment-only ones, already not global), at `base` against HEAD. No base: nothing narrowed.
pub fn part_plan(
    files: &[String],
    skip: &BTreeSet<&String>,
    base: Option<&str>,
    cwd: &Path,
) -> PartPlan {
    let mut out = PartPlan::default();
    let Some(base) = base else { return out };
    let all_suites =
        || tables::feature_suites::FEATURE_SUITES.iter().map(|(k, _)| (*k).to_string());
    for f in files.iter().filter(|f| !skip.contains(f)) {
        let sides = || (git::show_at(base, f, cwd), git::show_at("HEAD", f, cwd));
        if f == tables::escalation::CI_WORKFLOW {
            let (old, new) = sides();
            let PartEdit::Parts(jobs) = workflow_jobs_moved(old.as_deref(), new.as_deref()) else {
                continue;
            };
            out.files.insert(f.clone());
            for job in jobs {
                if let Some(JobFires::Outputs(outputs)) = job_fires(&job) {
                    // A side job fires the parts whose view moved; the block moved, so a view moved too, and
                    // should none, every part fires.
                    let moved: Vec<PlanOutput> = outputs
                        .iter()
                        .copied()
                        .filter(|o| output_moved(old.as_deref(), new.as_deref(), &job, *o))
                        .collect();
                    let fired =
                        if outputs.len() < 2 || moved.is_empty() { outputs } else { &moved[..] };
                    out.outputs.extend(fired.iter().copied());
                }
            }
            if out.outputs.contains(&PlanOutput::Suites) {
                out.suites.extend(all_suites());
            }
        } else if f == tables::escalation::SUITE_SCRIPT {
            let (old, new) = sides();
            match suite_arms_moved(old.as_deref(), new.as_deref()) {
                ArmEdit::Doubt => continue,
                ArmEdit::Every => out.suites.extend(all_suites()),
                ArmEdit::Arms(keys) => {
                    let known = |k: &String| {
                        tables::feature_suites::FEATURE_SUITES.iter().any(|(s, _)| *s == k.as_str())
                    };
                    if keys.iter().all(known) {
                        out.suites.extend(keys.iter().cloned());
                        out.own_arms.extend(keys);
                    } else {
                        // The default arm, or an arm no plan names: every leg runs the script.
                        out.suites.extend(all_suites());
                    }
                }
            }
            out.files.insert(f.clone());
        }
    }
    out
}
