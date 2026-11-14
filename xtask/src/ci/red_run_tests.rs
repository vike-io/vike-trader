//! `red_run`'s conditions that need no git, one planted defect each over a red run that IS carried
//! (decision 0278). The git half (the tested tree, the head's ancestry, the diff) and the plan it feeds are
//! `xtask/tests/ci_plan_gate/incremental_after_red.rs`'s.

use std::path::Path;

use serde_json::{Value, json};

use super::red_run::{
    NEXTEST_STEP, ROSTER_STEPS, judge, junit_results, plans_in_full, read_red_run,
};

const TESTED: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const FAILED: (&str, &str) = ("vike-core::journal", "gtd::the_refusal_stands");

/// A red run of pull request #42 with two legs: leg 1/2 ran two tests and one failed, leg 2/2 ran one and
/// passed; every other job is green or skipped.
struct World {
    dir: tempfile::TempDir,
    run: Value,
    jobs: Value,
}

fn step(name: &str, conclusion: &str) -> Value {
    json!({"name": name, "conclusion": conclusion})
}

fn leg(k: &str, conclusion: &str, nextest: &str) -> Value {
    json!({"name": format!("test ({k})"), "conclusion": conclusion, "steps": [
        step("Set up job", "success"),
        step(NEXTEST_STEP, nextest),
        step("Slowest tests (this job)", "success"),
        step(ROSTER_STEPS[0], "skipped"),
        step(ROSTER_STEPS[1], "skipped"),
    ]})
}

const JUNIT_1: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="2" skipped="0" failures="1" errors="0" time="1.5">
    <testsuite name="vike-core::journal" tests="2" skipped="0" errors="0" failures="1">
        <testcase name="gtd::the_refusal_stands" classname="vike-core::journal" time="0.004">
            <failure message="thread &apos;x&apos; panicked" type="test failure with exit code 101">a &lt;failure&gt; quoted</failure>
            <system-out>
</system-out>
        </testcase>
        <testcase name="gtd::the_flaky_one" classname="vike-core::journal" time="0.004">
            <flakyFailure time="0.004" message="first attempt fails" type="test failure">first attempt fails</flakyFailure>
        </testcase>
    </testsuite>
</testsuites>
"#;

const JUNIT_2: &str = r#"<testsuites name="nextest-run" tests="1">
    <testsuite name="vike-model" tests="1">
        <testcase name="a::passes" classname="vike-model" time="0.1"/>
    </testsuite>
</testsuites>
"#;

impl World {
    fn new() -> Self {
        let w = World {
            dir: tempfile::tempdir().expect("a temp dir"),
            run: json!({"id": 7, "status": "completed", "conclusion": "failure", "event": "pull_request", "run_attempt": 1}),
            jobs: json!({"total_count": 6, "jobs": [
                {"name": "plan", "conclusion": "success", "steps": []},
                leg("1/2", "failure", "failure"),
                leg("2/2", "success", "success"),
                {"name": "features (backfill)", "conclusion": "success"},
                {"name": "latency", "conclusion": "skipped"},
                {"name": "merge-gate", "conclusion": "failure"},
            ]}),
        };
        w.record("ci-test-record-0", "1/2", JUNIT_1, 2);
        w.record("ci-test-record-1", "2/2", JUNIT_2, 1);
        w
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn record(&self, name: &str, shard: &str, junit: &str, planned: usize) {
        let d = self.path().join("records").join(name);
        std::fs::create_dir_all(&d).expect("mkdir");
        let meta =
            format!("tested={TESTED}\nhead={HEAD}\npr=42\nshard={shard}\nplanned={planned}\n");
        std::fs::write(d.join("meta"), meta).expect("meta");
        std::fs::write(d.join("junit.xml"), junit).expect("junit");
    }

    fn rewrite(&self, name: &str, file: &str, edit: impl Fn(String) -> String) {
        let p = self.path().join("records").join(name).join(file);
        std::fs::write(&p, edit(std::fs::read_to_string(&p).expect("read"))).expect("write");
    }

    fn judge(&self) -> Result<super::red_run::RedRun, String> {
        std::fs::write(self.path().join("run.json"), self.run.to_string()).expect("run.json");
        std::fs::write(self.path().join("jobs.json"), self.jobs.to_string()).expect("jobs.json");
        read_red_run(self.path(), "42")
    }

    /// The reason this world plans in full; panics when it is carried.
    fn refused(&self) -> String {
        match self.judge() {
            Ok(red) => panic!("carried a run that must plan in full: {red:?}"),
            Err(why) => why,
        }
    }

    fn job(&mut self, i: usize) -> &mut Value {
        &mut self.jobs["jobs"][i]
    }
}

#[test]
fn a_red_run_that_finished_every_test_is_carried_with_its_failed_test() {
    let red = World::new().judge().expect("carried");
    assert_eq!(red.id, "7");
    assert_eq!((red.tested.as_str(), red.head.as_str()), (TESTED, HEAD));
    let want: Vec<(String, String)> = vec![(FAILED.0.into(), FAILED.1.into())];
    assert_eq!(
        red.failed.iter().cloned().collect::<Vec<_>>(),
        want,
        "the flaky pass is no failure"
    );
    assert_eq!(red.filter(), "(binary_id(=vike-core::journal) & test(=gtd::the_refusal_stands))");
}

#[test]
fn a_run_that_is_not_a_finished_first_attempt_failure_plans_in_full() {
    for (key, value, why) in [
        ("conclusion", json!("success"), "not a failure"),
        ("conclusion", json!("cancelled"), "not a failure"),
        ("status", json!("in_progress"), "not a failure"),
        ("run_attempt", json!(2), "first attempt"),
        ("event", json!("push"), "first attempt"),
    ] {
        let mut w = World::new();
        w.run[key] = value;
        let got = w.refused();
        assert!(got.contains(why), "{key}: {got}");
    }
}

#[test]
fn a_job_other_than_a_test_leg_that_did_not_pass_plans_in_full() {
    for conclusion in ["failure", "cancelled", "timed_out"] {
        let mut w = World::new();
        w.job(3)["conclusion"] = json!(conclusion);
        assert!(w.refused().contains("only a test leg's failure is re-run"), "{conclusion}");
    }
    let mut w = World::new();
    w.job(5)["conclusion"] = json!("cancelled");
    w.judge().expect("merge-gate only reflects the others");
}

#[test]
fn a_test_leg_that_did_not_finish_plans_in_full() {
    let mut w = World::new();
    w.job(1)["conclusion"] = json!("cancelled");
    assert!(w.refused().contains("did not finish"));
    // the build failed, so nextest never ran
    let mut w = World::new();
    w.job(2)["steps"][1]["conclusion"] = json!("skipped");
    assert!(w.refused().contains("did not run to its end"));
    // a step other than nextest, the doctests and clippy failed
    let mut w = World::new();
    w.job(2)["steps"][0]["conclusion"] = json!("failure");
    assert!(w.refused().contains("step 'Set up job'"));
    let mut w = World::new();
    w.jobs["total_count"] = json!(7);
    assert!(w.refused().contains("incomplete"));
}

/// Clippy and the doctests run over the whole roster on every run, so their red carries nothing.
#[test]
fn a_red_clippy_or_doctest_step_is_re_run_not_refused() {
    for i in 3..5 {
        let mut w = World::new();
        w.job(2)["conclusion"] = json!("failure");
        w.job(2)["steps"][i]["conclusion"] = json!("failure");
        w.judge().expect("carried: the step re-runs over the roster");
    }
}

#[test]
fn a_missing_or_foreign_record_plans_in_full() {
    let w = World::new();
    std::fs::remove_dir_all(w.path().join("records/ci-test-record-1")).expect("rm");
    assert!(w.refused().contains("left 1 records"));
    let w = World::new();
    std::fs::remove_file(w.path().join("records/ci-test-record-1/junit.xml")).expect("rm");
    assert!(w.refused().contains("holds no junit.xml"));
    let w = World::new();
    w.rewrite("ci-test-record-1", "meta", |m| m.replace("pr=42", "pr=41"));
    assert!(w.refused().contains("pull request"));
    let w = World::new();
    w.rewrite("ci-test-record-1", "meta", |m| m.replace(TESTED, &"3".repeat(40)));
    assert!(w.refused().contains("disagree on the tested commit"));
    let w = World::new();
    w.rewrite("ci-test-record-1", "meta", |m| m.replace("shard=2/2", "shard=1/2"));
    assert!(w.refused().contains("two records of leg 1/2"));
    let w = World::new();
    w.rewrite("ci-test-record-1", "meta", |m| m.replace("shard=2/2", "shard=3/3"));
    assert!(w.refused().contains("did not run"));
}

#[test]
fn a_leg_that_ran_fewer_tests_than_it_planned_plans_in_full() {
    // the leg stopped early (a killed job, a crash): the report holds fewer tests than the leg listed
    let w = World::new();
    w.rewrite("ci-test-record-0", "meta", |m| m.replace("planned=2", "planned=3"));
    let why = w.refused();
    assert!(why.contains("ran 2 of the 3 tests it planned"), "{why}");
    let w = World::new();
    w.rewrite("ci-test-record-0", "meta", |m| m.replace("planned=2", "planned="));
    assert!(w.refused().contains("names no planned test count"));
}

#[test]
fn a_failed_leg_whose_report_names_no_failure_or_an_unquotable_one_plans_in_full() {
    let w = World::new();
    w.rewrite("ci-test-record-0", "junit.xml", |j| j.replace("<failure ", "<flakyFailure "));
    assert!(w.refused().contains("names no failed test"));
    let w = World::new();
    w.rewrite("ci-test-record-0", "junit.xml", |j| {
        j.replace("gtd::the_refusal_stands", "gtd::a b")
    });
    assert!(w.refused().contains("cannot quote"));
}

/// The two report shapes a naive parse miscounts (an open `<testcase>` that failed, and a retried one that
/// passed), and the `<error>` child: from `crates/vike-ops/tests/ci/ci_slowest_tests_gate.rs`'s sample.
#[test]
fn the_junit_reader_counts_tests_and_names_failures_only() {
    let (ran, failed) = junit_results(JUNIT_1).expect("parsed");
    assert_eq!((ran, failed.len()), (2, 1));
    let errored = JUNIT_2
        .replace(r#"time="0.1"/>"#, r#"time="0.1"><error message="exec">x</error></testcase>"#);
    let (ran, failed) = junit_results(&errored).expect("parsed");
    assert_eq!(
        (ran, failed.into_iter().next()),
        (1, Some(("vike-model".into(), "a::passes".into())))
    );
    assert_eq!(junit_results("<testcase name=\"cut"), None);
}

#[test]
fn the_machinery_the_results_came_from_plans_in_full() {
    for f in [
        ".github/workflows/ci.yml",
        ".github/actions/rust-ci-setup/action.yml",
        ".config/nextest.toml",
        "xtask/src/ci/plan.rs",
        "rust-toolchain.toml",
        "Cargo.lock",
        "Cargo.toml",
        "crates/vike-core/Cargo.toml",
        "scripts/ci_red_run.sh",
        "scripts/ci_plan_log.sh",
    ] {
        assert!(plans_in_full(f), "{f}");
    }
    for f in [
        "crates/vike-core/src/lib.rs",
        "xtask/src/main.rs",
        "docs/ops/ci-and-lanes.md",
        "scripts/x.sh",
    ] {
        assert!(!plans_in_full(f), "{f}");
    }
}

#[test]
fn only_a_first_attempt_pull_request_run_with_a_fetched_red_run_is_judged() {
    let cwd = Path::new(".");
    let env = |pairs: &[(&str, &str)]| -> super::git::Env {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    };
    let why = |pairs: &[(&str, &str)]| judge(&env(pairs), cwd).expect_err("plans in full");
    assert!(why(&[("GITHUB_EVENT_NAME", "push")]).contains("'push' run always plans in full"));
    let pr = [("GITHUB_EVENT_NAME", "pull_request"), ("GITHUB_RUN_ATTEMPT", "2")];
    assert!(why(&pr).contains("a re-run plans in full"));
    let pr = [("GITHUB_EVENT_NAME", "pull_request"), ("GITHUB_RUN_ATTEMPT", "1")];
    assert_eq!(why(&pr), "no earlier red run was fetched");
    let with_why = [pr[0], pr[1], ("CI_RED_RUN_WHY", "the newest finished run 9 is 'success'")];
    assert_eq!(why(&with_why), "the newest finished run 9 is 'success'");
}
