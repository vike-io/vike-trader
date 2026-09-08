//! The unattended runner's properties that need no world: the ring cross-check, the record that is
//! written whatever happened, and the credential that cannot reach it.
//!
//! ⚠ A SEPARATE BINARY from `crates/vike-agent-eval/tests/unattended_e2e.rs`, which stands a real
//! paper node up. These assertions cost milliseconds and must stay runnable when the shipped
//! binaries are not built; putting them in the same file as the world would make every one of them
//! wait for a daemon they do not need.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use vike_agent_eval::driver::{ModelDriver, Scripted};
use vike_agent_eval::unattended::{
    DEFAULT_BUDGET_SECS, DEFAULT_MAX_STEPS, FULL_PROFILE, Outcome, READ_ONLY_PROFILE, REDACTED,
    RunSpec, Summary, refuse_write_tools, run, scripted_probe, write_summary, write_tools_in,
};

/// A spec pointed at `record_dir`, with everything else at its default. `vike_cli` is a parameter
/// because two of the tests below deliberately point it at nothing.
fn spec<'a>(record_dir: &'a Path, vike_cli: Option<&'a Path>) -> RunSpec<'a> {
    RunSpec {
        vike_cli,
        task: "node-review",
        prompt: "report the state of the node",
        node: None,
        datahub: None,
        settings_dir: None,
        record_dir,
        allow_writes: false,
        max_steps: DEFAULT_MAX_STEPS,
        budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
        scrub: &[],
    }
}

/// A roster shaped like the one `vike-cli mcp` advertises: a read tool and a write tool, each with
/// the annotation the server puts on it.
fn roster_with_a_write_tool() -> Vec<Value> {
    vec![
        json!({"name": "node_snapshot", "annotations": {"readOnlyHint": true}}),
        json!({"name": "submit_order", "annotations": {"destructiveHint": true}}),
    ]
}

fn read_only_roster() -> Vec<Value> {
    vec![json!({"name": "node_snapshot", "annotations": {"readOnlyHint": true}})]
}

/// The ring cross-check, in both directions and against the vacuous case.
///
/// ⚠ The EMPTY-roster arm is the one that matters most: without it "no write tool was advertised"
/// would be discharged by a server that advertised nothing at all, which is the vacuous green this
/// crate exists to make impossible.
#[test]
fn a_read_only_run_refuses_a_roster_that_carries_a_write_tool() {
    let with_write = roster_with_a_write_tool();
    let err = refuse_write_tools(&with_write, false)
        .expect_err("a destructive tool under the read-only ring must be refused");
    assert!(
        err.contains("submit_order") && err.contains(READ_ONLY_PROFILE),
        "the refusal must name the offending tool and the ring it broke: {err}"
    );

    refuse_write_tools(&with_write, true)
        .expect("--allow-writes is exactly the flag that admits a write roster");
    refuse_write_tools(&read_only_roster(), false)
        .expect("a roster with no destructive tool is what the read-only ring asks for");

    assert!(
        refuse_write_tools(&[], false).is_err() && refuse_write_tools(&[], true).is_err(),
        "an EMPTY roster must be refused under BOTH rings — a run that could do nothing must never \
         be recorded as one that did"
    );

    // …and the derivation itself: the write set is read off the server's own annotation.
    assert_eq!(write_tools_in(&with_write), vec!["submit_order".to_string()]);
    assert!(write_tools_in(&read_only_roster()).is_empty());
}

/// A run that could not even be stood up still leaves a record.
///
/// The failure is planted at the earliest possible point — the `vike-cli` binary does not exist — so
/// nothing downstream of the spawn could have written anything. If the record survives THAT, the
/// later paths (a dead server, a model error, a budget) are all strictly further along.
#[test]
fn a_run_that_fails_before_it_starts_still_records_its_summary() {
    let dir = tempfile::tempdir().expect("a throwaway record directory");
    let missing = dir.path().join("no-such-vike-cli");
    let mut driver = Scripted::new(scripted_probe());
    let summary = run(&spec(dir.path(), Some(&missing)), &mut driver);

    assert_eq!(summary.outcome, Outcome::Failed, "a missing server binary is a failed run");
    assert_ne!(summary.outcome.exit_code(), 0, "a failed run must not exit 0");
    let detail = summary.detail.clone().expect("a failure must say what failed");
    assert!(
        detail.contains("no-such-vike-cli"),
        "the failure must name the missing path: {detail}"
    );
    assert_eq!(summary.steps, 0, "nothing was driven");

    let path = write_summary(dir.path(), &summary, &[]).expect("the record must be written");
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read it back"))
            .expect("the record must be readable JSON");
    assert_eq!(record["outcome"], json!("failed"));
    assert_eq!(record["profile"], json!(READ_ONLY_PROFILE));
    assert_eq!(record["allow_writes"], json!(false));
    assert_eq!(record["task"], json!("node-review"));
    assert!(record["started_utc"].as_str().is_some_and(|s| s.ends_with('Z')));
}

/// A summary built as if a model had echoed its own credential back.
fn summary_with(conclusion: &str, outcome: Outcome) -> Summary {
    Summary {
        started_ms: 1_600_000_000_000,
        finished_ms: 1_600_000_001_000,
        task: "node-review".to_string(),
        prompt: "report the state of the node".to_string(),
        driver: "scripted".to_string(),
        profile: READ_ONLY_PROFILE,
        allow_writes: false,
        node: Some("127.0.0.1:7900".to_string()),
        max_steps: DEFAULT_MAX_STEPS,
        budget: Duration::from_secs(DEFAULT_BUDGET_SECS),
        steps: 2,
        roster_size: 9,
        write_tools_offered: 0,
        tool_calls: 1,
        write_tools_called: 0,
        outcome,
        detail: None,
        conclusion: conclusion.to_string(),
        record_dir: PathBuf::from("record"),
    }
}

/// No byte of a credential this process holds may reach the record.
///
/// ⚠ The planted secret sits in the model's own CONCLUSION, which is the realistic route: nothing
/// here ever puts a credential into a field on purpose, so a leak arrives as free text a model
/// wrote. The guarantee is exactly as wide as it is stated to be — [`write_summary`] is handed the
/// values this process has, and replaces those.
#[test]
fn no_credential_this_process_holds_can_reach_the_record() {
    let dir = tempfile::tempdir().expect("a throwaway record directory");
    let token = "sk-ant-oat01-PRETEND-TOKEN-BYTES";
    let summary = summary_with(
        &format!("I authenticated with {token} and then read the node. All quiet."),
        Outcome::Ok,
    );

    let path = write_summary(dir.path(), &summary, &[token]).expect("the record must be written");
    let text = std::fs::read_to_string(&path).expect("read it back");
    assert!(!text.contains(token), "the credential survived into the record:\n{text}");
    assert!(text.contains(REDACTED), "the suppression must be visible, not silent:\n{text}");

    // …and the rest of the sentence is still there, so redaction is a replacement rather than a
    // deletion of the evidence around it.
    assert!(text.contains("All quiet."), "redaction must not eat the record:\n{text}");
}

/// A timeout is recorded as a timeout — never as a success, and never as a plain failure.
///
/// ⚠ This pins the CLASSIFIER, not the channel: a run that is past its deadline when it stops is a
/// timeout whatever its error message said, which is the property that keeps a driver failing "for
/// a reason of its own" at minute fifteen from being filed as an ordinary failure. The other half —
/// the channel actually refusing a tool call once the deadline has passed — needs a real server and
/// is pinned in `crates/vike-agent-eval/tests/unattended_e2e.rs`.
#[test]
fn an_exhausted_budget_is_recorded_as_a_timeout() {
    let dir = tempfile::tempdir().expect("a throwaway record directory");
    let missing = dir.path().join("no-such-vike-cli");
    let mut driver = Scripted::new(scripted_probe());
    let mut s = spec(dir.path(), Some(&missing));
    s.budget = Duration::from_secs(0);
    let summary = run(&s, &mut driver);

    assert_eq!(
        summary.outcome,
        Outcome::Timeout,
        "a run whose budget is already spent must be a timeout, whatever failed first"
    );
    assert_ne!(summary.outcome.exit_code(), 0, "a timeout must not exit 0");
    assert_ne!(
        summary.outcome.exit_code(),
        Outcome::Failed.exit_code(),
        "a timeout and a failure must be distinguishable from the exit status alone"
    );

    let path = write_summary(dir.path(), &summary, &[]).expect("the record must be written");
    let record: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read it back"))
            .expect("readable JSON");
    assert_eq!(record["outcome"], json!("timeout"));
}

/// A record is never overwritten.
#[test]
fn two_records_written_in_the_same_moment_are_two_files() {
    let dir = tempfile::tempdir().expect("a throwaway record directory");
    let summary = summary_with("first", Outcome::Ok);
    let a = write_summary(dir.path(), &summary, &[]).expect("first record");
    let b = write_summary(dir.path(), &summary, &[]).expect("second record");
    assert_ne!(a, b, "the second record must take a fresh name rather than replace the first");
    assert!(a.is_file() && b.is_file(), "both records must be on disk");
    assert!(
        std::fs::read_to_string(&a).expect("read a").contains("first"),
        "the first record must survive the second"
    );
}

/// The scripted probe is a real plan that answers, not a placeholder.
#[test]
fn the_scripted_probe_reads_the_node_and_then_answers() {
    let plan = scripted_probe();
    assert!(plan.len() >= 2, "a probe that only answers would drive nothing");
    let driver = Scripted::new(plan);
    assert_eq!(driver.name(), "scripted", "the record must name the model-free driver as such");
}

/// The two profile spellings are the server's, not a second vocabulary.
#[test]
fn the_profile_names_are_the_servers_own() {
    let mcp = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vike-cli")
        .join("src")
        .join("cmd")
        .join("mcp.rs");
    let text =
        std::fs::read_to_string(&mcp).unwrap_or_else(|e| panic!("read {}: {e}", mcp.display()));
    for name in [READ_ONLY_PROFILE, FULL_PROFILE] {
        assert!(
            text.contains(&format!("\"{name}\"")),
            "`{name}` is passed to `vike-cli mcp --profile` and that binary does not spell it — the \
             runner would be asking for a ring the server rejects as an unknown name"
        );
    }
}
