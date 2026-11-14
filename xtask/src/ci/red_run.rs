//! The INCREMENTAL plan after a RED `ci` run of the same pull request (decision 0278, amending 0106).
//!
//! After a red run, the next push does not run the whole plan again. It plans the diff between the tree the
//! red run TESTED (its merge commit) and the tree this run tests, exactly as a plan from that base, and adds
//! the tests that FAILED in the red run ([`RedRun::filter`]). Everything else carries over from the red run:
//! its inputs did not change. Clippy and the doctests are no exception to carry: the `test` job runs them over
//! the whole roster on every run, so a red one there is simply re-run (cargo's cache replays an unchanged
//! crate's lints).
//!
//! `scripts/ci_red_run.sh` FETCHES (the REST calls, the artifacts each `test` leg uploads); this module
//! DECIDES. [`judge`] answers `Err(reason)`, which is a plan exactly as before, on any doubt:
//!
//!   * the event is not a `pull_request`, or this is not the run's first attempt (`main`'s runs stay full);
//!   * the script found no earlier finished run of this pull request (its reason, `CI_RED_RUN_WHY`);
//!   * that run is not `completed` + `failure` + attempt 1 ([`read_red_run`]);
//!   * a job other than a `test` leg (and `merge-gate`, which only reflects the others) is not success or
//!     skipped; a `test` leg is not success or failure; a step of a leg other than the nextest, doctest and
//!     clippy steps is not success or skipped; a leg's nextest step did not run (`read_jobs`);
//!   * a leg has no record, or a record names another pull request, another tree or head than its siblings,
//!     or a leg the run did not have (`read_records`);
//!   * a leg ran fewer (or more) tests than it planned: the red run did not finish every test;
//!   * a leg's nextest step failed and its report names no failed test, or a failed test's name is not one
//!     nextest's filter can quote;
//!   * the tested tree cannot be resolved here, or this pull request's head does not descend from the red
//!     run's head ([`judge_tree`]);
//!   * the diff touches [`plans_in_full`]: the workflows, nextest's profile, the planner, the toolchain, a
//!     manifest or the lockfile, or this mechanism's own scripts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::git::{self, Env};

/// The `test` job's nextest step, by its `name:` in `ci.yml`: the one step of a leg that may fail and still
/// leave a run that can be carried. `xtask/tests/ci_plan_gate/incremental_after_red.rs` holds it to the file.
pub const NEXTEST_STEP: &str = "Test (nextest)";

/// The `test` job's steps that run over the whole roster on EVERY run (`ci.yml`'s `${ROSTER:?}`), so their
/// red in the red run carries nothing and is re-run: the doctests and clippy.
pub const ROSTER_STEPS: &[&str] =
    &["Doctests (nextest cannot run these)", "Clippy (deny warnings)"];

/// The script that fetches the red run, and the file it sources.
pub const RED_RUN_SCRIPTS: &[&str] = &["scripts/ci_red_run.sh", "scripts/ci_plan_log.sh"];

/// The red run whose results this plan carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedRun {
    /// The run's id.
    pub id: String,
    /// The merge commit it tested: the base of the incremental diff.
    pub tested: String,
    /// The pull request head that merge commit holds.
    pub head: String,
    /// `(binary id, test name)` of every test that failed in it.
    pub failed: BTreeSet<(String, String)>,
}

impl RedRun {
    /// The nextest terms that select exactly the failed tests, joined by ` | `: one
    /// `(binary_id(=B) & test(=T))` per test, an exact match on both (measured on a the latency box lane).
    pub fn filter(&self) -> String {
        let terms: Vec<String> =
            self.failed.iter().map(|(b, t)| format!("(binary_id(={b}) & test(={t}))")).collect();
        terms.join(" | ")
    }
}

/// The red run this pull-request run may plan incrementally from, and the files that differ between the
/// tree it tested and HEAD; `Err(reason)` plans in full. Every condition is in the module doc.
pub fn judge(env: &Env, cwd: &Path) -> Result<(RedRun, Vec<String>), String> {
    let event = git::var(env, "GITHUB_EVENT_NAME");
    if event != "pull_request" {
        return Err(format!("a '{event}' run always plans in full"));
    }
    let attempt = git::var(env, "GITHUB_RUN_ATTEMPT");
    if attempt != "1" {
        return Err(format!("attempt '{attempt}' of this run: a re-run plans in full"));
    }
    let dir = git::var(env, "CI_RED_RUN_DIR");
    if dir.is_empty() {
        let why = git::var(env, "CI_RED_RUN_WHY");
        return Err(if why.is_empty() {
            "no earlier red run was fetched".to_string()
        } else {
            why.to_string()
        });
    }
    let red = read_red_run(Path::new(dir), git::var(env, "CI_PR_NUMBER"))?;
    let files = judge_tree(&red, git::var(env, "CI_PR_HEAD"), cwd)?;
    Ok((red, files))
}

/// Condition 3: a change since the red run to one of these plans in full. A manifest edit is a cross-crate
/// change (the root `CLAUDE.md`), and the rest is the machinery the carried results were produced by.
pub fn plans_in_full(f: &str) -> bool {
    f.starts_with(".github/")
        || f == ".config/nextest.toml"
        || f.starts_with("xtask/src/ci/")
        || f.starts_with("rust-toolchain")
        || f == "Cargo.lock"
        || f == "Cargo.toml"
        || f.ends_with("/Cargo.toml")
        || RED_RUN_SCRIPTS.contains(&f)
}

/// The red run fetched into `dir` (`run.json`, `jobs.json`, `records/<leg>/{meta,junit.xml}`),
/// judged with no git: conditions 1 and the records. `pr` is this run's pull request number.
pub fn read_red_run(dir: &Path, pr: &str) -> Result<RedRun, String> {
    let run = read_json(&dir.join("run.json"))?;
    let id = run["id"].as_u64().ok_or("run.json names no run id")?.to_string();
    let field = |k: &str| run[k].as_str().unwrap_or("").to_string();
    if field("status") != "completed" || field("conclusion") != "failure" {
        return Err(format!(
            "the newest finished run {id} of this pull request is '{}' ({}), not a failure",
            field("conclusion"),
            field("status")
        ));
    }
    if field("event") != "pull_request" || run["run_attempt"].as_u64() != Some(1) {
        return Err(format!("run {id} is not the first attempt of a pull-request run"));
    }
    let legs = read_jobs(&read_json(&dir.join("jobs.json"))?, &id)?;
    read_records(&dir.join("records"), &legs, pr, id)
}

/// The `test` legs of the red run's job listing, `name in "test (K/N)"` -> whether its nextest step failed,
/// after every job and step condition in the module doc.
fn read_jobs(jobs: &Value, id: &str) -> Result<BTreeMap<String, bool>, String> {
    let list =
        jobs["jobs"].as_array().ok_or_else(|| format!("run {id}'s job listing is no list"))?;
    if jobs["total_count"].as_u64() != Some(list.len() as u64) {
        return Err(format!("run {id}'s job listing is incomplete"));
    }
    let mut legs = BTreeMap::new();
    for job in list {
        let name = job["name"].as_str().unwrap_or("");
        let conclusion = job["conclusion"].as_str().unwrap_or("");
        if name == "merge-gate" {
            continue;
        }
        let Some(leg) = name.strip_prefix("test (").and_then(|s| s.strip_suffix(')')) else {
            if !matches!(conclusion, "success" | "skipped") {
                return Err(format!(
                    "run {id}: job '{name}' is '{conclusion}', and only a test leg's failure is re-run"
                ));
            }
            continue;
        };
        if !matches!(conclusion, "success" | "failure") {
            return Err(format!("run {id}: test leg {leg} is '{conclusion}': it did not finish"));
        }
        let mut nextest = None;
        for step in job["steps"].as_array().ok_or_else(|| format!("leg {leg} lists no steps"))? {
            let step_name = step["name"].as_str().unwrap_or("");
            let c = step["conclusion"].as_str().unwrap_or("");
            if step_name == NEXTEST_STEP {
                nextest = Some(c.to_string());
            } else if !ROSTER_STEPS.contains(&step_name) && !matches!(c, "success" | "skipped") {
                return Err(format!("run {id}: step '{step_name}' of test leg {leg} is '{c}'"));
            }
        }
        let failed = match nextest.as_deref() {
            Some("success") => false,
            Some("failure") => true,
            other => {
                return Err(format!(
                    "run {id}: the nextest step of test leg {leg} is {other:?}, it did not run to its end"
                ));
            }
        };
        legs.insert(leg.to_string(), failed);
    }
    if legs.is_empty() {
        return Err(format!("run {id} has no test leg"));
    }
    Ok(legs)
}

/// Run `id` from every leg's record: one record per leg, all naming this pull request and one tree, each leg
/// having run exactly the tests it planned.
fn read_records(
    dir: &Path,
    legs: &BTreeMap<String, bool>,
    pr: &str,
    id: String,
) -> Result<RedRun, String> {
    let entries = std::fs::read_dir(dir).map_err(|_| format!("run {id} left no test records"))?;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let (mut tested, mut head) = (None::<String>, None::<String>);
    let mut failed = BTreeSet::new();
    for entry in entries.flatten() {
        let rec = entry.path();
        let text = |f: &str| {
            std::fs::read_to_string(rec.join(f))
                .map_err(|_| format!("a record of run {id} holds no {f}"))
        };
        let meta: BTreeMap<String, String> = text("meta")?
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_string(), v.trim().to_string()))
            .collect();
        let get = |k: &str| meta.get(k).cloned().unwrap_or_default();
        let leg = get("shard");
        let Some(&leg_failed) = legs.get(&leg) else {
            return Err(format!("run {id} has a record of leg '{leg}', which it did not run"));
        };
        if !seen.insert(leg.clone()) {
            return Err(format!("run {id} has two records of leg {leg}"));
        }
        if get("pr") != pr || pr.is_empty() {
            return Err(format!("run {id} tested pull request '{}', this is '{pr}'", get("pr")));
        }
        for (key, slot) in [("tested", &mut tested), ("head", &mut head)] {
            let sha = get(key);
            if !is_sha(&sha) || slot.as_ref().is_some_and(|s| *s != sha) {
                return Err(format!("run {id}: the legs' records disagree on the {key} commit"));
            }
            *slot = Some(sha);
        }
        let planned: usize = get("planned")
            .parse()
            .map_err(|_| format!("run {id}: leg {leg}'s record names no planned test count"))?;
        let (ran, fails) = junit_results(&text("junit.xml")?)
            .ok_or_else(|| format!("run {id}: leg {leg}'s JUnit report is not nextest's"))?;
        if ran != planned {
            return Err(format!(
                "run {id}: leg {leg} ran {ran} of the {planned} tests it planned, so it did not finish every test"
            ));
        }
        if leg_failed && fails.is_empty() {
            return Err(format!(
                "run {id}: leg {leg}'s nextest failed and its report names no failed test"
            ));
        }
        if let Some((b, t)) = fails.iter().find(|(b, t)| !quotable(b, "/.-") || !quotable(t, "")) {
            return Err(format!(
                "run {id}: the failed test '{b} {t}' has a name a filter cannot quote"
            ));
        }
        failed.extend(fails);
    }
    if seen.len() != legs.len() {
        return Err(format!(
            "run {id} ran {} test legs and left {} records",
            legs.len(),
            seen.len()
        ));
    }
    Ok(RedRun { id, tested: tested.unwrap_or_default(), head: head.unwrap_or_default(), failed })
}

/// `(tests run, failed (binary id, test name))` of a nextest JUnit report, in the shape
/// `scripts/ci_slowest_tests.sh` measured: one `<testcase ` start tag per test run (an `#[ignore]`d test is
/// absent), self-closing when it passed; an open one FAILED when a `<failure` or `<error` child comes before
/// its `</testcase>` (a retried test that passed carries only `<flakyFailure>`). Captured output escapes `<`,
/// so no test's own text can forge a tag. `None` when a tag is cut short.
pub fn junit_results(xml: &str) -> Option<(usize, BTreeSet<(String, String)>)> {
    let attr = |tag: &str, key: &str| -> Option<String> {
        let start = tag.find(&format!(" {key}=\""))? + key.len() + 3;
        Some(tag[start..].split('"').next()?.to_string())
    };
    let (mut ran, mut failed, mut rest) = (0, BTreeSet::new(), xml);
    while let Some(i) = rest.find("<testcase ") {
        rest = &rest[i..];
        let end = rest.find('>')?;
        let tag = &rest[..=end];
        ran += 1;
        if tag.ends_with("/>") {
            rest = &rest[end + 1..];
            continue;
        }
        let close = rest.find("</testcase>")?;
        let body = &rest[end + 1..close];
        if body.contains("<failure") || body.contains("<error") {
            failed.insert((attr(tag, "classname")?, attr(tag, "name")?));
        }
        rest = &rest[close..];
    }
    Some((ran, failed))
}

/// Condition 2 and 3 against git: the tested tree resolves here, this pull request's head descends from the
/// red run's, and nothing in [`plans_in_full`] changed between the two trees. `-> the changed files`.
pub fn judge_tree(red: &RedRun, pr_head: &str, cwd: &Path) -> Result<Vec<String>, String> {
    let id = &red.id;
    let tested = git::rev(&red.tested, cwd)
        .ok_or_else(|| format!("the tree run {id} tested ({}) cannot be read here", red.tested))?;
    if pr_head.is_empty() || !git::is_ancestor(&red.head, pr_head, cwd) {
        return Err(format!(
            "this pull request's head does not descend from the head run {id} tested ({})",
            red.head
        ));
    }
    let files = git::diff_names(&tested, cwd)
        .ok_or_else(|| format!("the diff from the tree run {id} tested cannot be read"))?;
    match files.iter().find(|f| plans_in_full(f)) {
        Some(f) => Err(format!("{f} changed since run {id}")),
        None => Ok(files),
    }
}

fn read_json(path: &Path) -> Result<Value, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text =
        std::fs::read_to_string(path).map_err(|_| format!("the red run's {name} is missing"))?;
    serde_json::from_str(&text).map_err(|_| format!("the red run's {name} is not JSON"))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A name nextest's `=` matcher reads as written: word characters, `:`, and the `extra` ones.
fn quotable(s: &str, extra: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || extra.contains(c))
}
