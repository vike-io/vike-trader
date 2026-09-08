//! The unattended runner end to end: a REAL `vike-cli mcp` server, a REAL node, the real ring, and
//! a record on disk a person could read.
//!
//! ⚠ ONE TEST, ONE WORLD, for the reason `crates/vike-agent-eval/tests/scripted_pipeline.rs` gives:
//! three `#[test]` functions would be three processes each deciding whether the shipped binaries
//! need building and racing each other for cargo's package lock while nextest's per-test clock ran.
//! The three scenarios below share one paper node and are reported as labelled assertions instead.
//!
//! ⚠ **The node is a PAPER `vike-tradehub` in a throwaway project, not the operator's.** That is the
//! one thing this test cannot borrow from the product it is testing: the runner exists precisely to
//! point at a real node, and a test that did so would be a test that trades. What is real here is
//! everything else — the spawned server, the profile the runner passed it, the roster it advertised
//! back, the transport, and the record. `crates/vike-agent-eval/src/node.rs`'s `PaperNode` argues
//! why nothing this stands up can reach a venue: every mount is paper, and both node keys are
//! obviously-fake `DUMMY-` strings this crate mints.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use vike_agent_eval::driver::Scripted;
use vike_agent_eval::locate_binary;
use vike_agent_eval::node::{PaperNode, Project, pick_port};
use vike_agent_eval::unattended::{
    DEFAULT_MAX_STEPS, FULL_PROFILE, Outcome, READ_ONLY_PROFILE, RECORD_KIND, RunSpec, run,
    scripted_probe, write_summary,
};

/// The two binaries this test drives. A missing one FAILS IMMEDIATELY naming the build rather than
/// shelling out to cargo — the trap `scripted_pipeline.rs` documents: a cold build outruns any
/// per-test budget, so the convenience would turn "you have not built the binaries" into a killed
/// test with no reason attached.
fn binaries() -> (PathBuf, PathBuf) {
    match (locate_binary("vike-cli", None), locate_binary("vike-tradehub", None)) {
        (Ok(cli), Ok(tradehub)) => (cli, tradehub),
        (cli, tradehub) => panic!(
            "this test drives the SHIPPED binaries and one is not built in this tree.\n  \
             vike-cli:       {}\n  vike-tradehub:  {}\nBuild them first:\n  \
             cargo build -p vike-cli -p vike-tradehub --bin vike-cli --bin vike-tradehub",
            cli.map_or_else(|e| e, |p| p.display().to_string()),
            tradehub.map_or_else(|e| e, |p| p.display().to_string()),
        ),
    }
}

/// One scenario, run against the shared world.
struct Scenario {
    label: &'static str,
    record_dir: PathBuf,
    allow_writes: bool,
    budget: Duration,
}

#[test]
fn an_unattended_run_against_a_real_node_leaves_a_readable_record() {
    let (vike_cli, vike_tradehub) = binaries();
    let work = tempfile::tempdir().expect("a throwaway work directory");
    let project = Project::create(&work.path().join("project")).expect("a throwaway project");
    let mut node = PaperNode::start(&vike_tradehub, &project, &[]).expect("a paper node");

    // ⚠ A datahub port PROVEN FREE, never the server's default. The default is a well-known
    // loopback port a real `vike-datahub` may be listening on — the CI box runs one — and pointing this
    // test at it would silently make the run about whatever that server holds.
    let dead = pick_port().expect("a free loopback port");
    let dead_addr = format!("127.0.0.1:{dead}");

    let scenarios = [
        Scenario {
            label: "default",
            record_dir: work.path().join("rec-default"),
            allow_writes: false,
            budget: Duration::from_secs(120),
        },
        Scenario {
            label: "allow-writes",
            record_dir: work.path().join("rec-writes"),
            allow_writes: true,
            budget: Duration::from_secs(120),
        },
        Scenario {
            label: "spent-budget",
            record_dir: work.path().join("rec-budget"),
            allow_writes: false,
            budget: Duration::from_secs(0),
        },
    ];

    let mut results = Vec::new();
    for scenario in &scenarios {
        let mut driver = Scripted::new(scripted_probe());
        let spec = RunSpec {
            vike_cli: Some(&vike_cli),
            task: "node-review",
            prompt: "report the state of the node",
            node: Some(&node.addr),
            datahub: Some(&dead_addr),
            settings_dir: Some(&project.settings),
            record_dir: &scenario.record_dir,
            allow_writes: scenario.allow_writes,
            max_steps: DEFAULT_MAX_STEPS,
            budget: scenario.budget,
            scrub: &[],
        };
        let summary = run(&spec, &mut driver);
        let path = write_summary(&scenario.record_dir, &summary, &[])
            .unwrap_or_else(|e| panic!("{}: the record must be written: {e}", scenario.label));
        results.push((summary, path));
    }

    // Teardown BEFORE the assertions: a panic below must not leave a daemon holding a port for the
    // next test in this binary. Its own verdict is asserted at the end.
    let stopped = node.stop();

    assert_eq!(results.len(), scenarios.len(), "every scenario must produce a result");
    let (default, default_path) = &results[0];
    let (wide, _) = &results[1];
    let (spent, _) = &results[2];

    // ── 1. THE DEFAULT IS READ-ONLY, and the proof is the server's OWN advertised roster ─────────
    assert_eq!(
        default.outcome,
        Outcome::Ok,
        "the default unattended run must succeed against a live node.\n{}",
        default.render()
    );
    assert_eq!(default.profile, READ_ONLY_PROFILE, "the default ring is read-only");
    assert!(default.roster_size > 0, "the server advertised nothing, so nothing was proven");
    assert_eq!(
        default.write_tools_offered, 0,
        "the server this run launched advertised {} destructive tool(s) — the `--profile \
         read-only` argument did not reach it",
        default.write_tools_offered
    );
    assert_eq!(default.write_tools_called, 0, "a read-only run called a write tool");
    assert!(default.tool_calls >= 1, "the probe must have reached the server at least once");
    assert!(default.steps >= 1, "the driver must have spent at least one step");

    // ── 2. THE WIDER RING REQUIRES ITS FLAG — and this is what makes (1)'s zero meaningful ───────
    // Without this arm, `write_tools_offered == 0` above would be discharged by a server that has
    // no write tools at all rather than by one that withheld them.
    assert_eq!(wide.profile, FULL_PROFILE, "--allow-writes is the only widening");
    assert!(
        wide.write_tools_offered > 0,
        "the FULL ring advertised no destructive tool, so the read-only run above proved nothing \
         about withholding.\n{}",
        wide.render()
    );
    assert!(
        wide.roster_size > default.roster_size,
        "the full ring must be strictly larger than the read-only one: {} vs {}",
        wide.roster_size,
        default.roster_size
    );

    // ── 3. A SPENT BUDGET IS A RECORDED TIMEOUT, not a success and not an absence ────────────────
    // This one reaches the server and initializes, then is refused at the CHANNEL — the half
    // `crates/vike-agent-eval/tests/unattended_record.rs` cannot reach without a world.
    assert_eq!(
        spent.outcome,
        Outcome::Timeout,
        "a run whose budget is spent must end as a timeout.\n{}",
        spent.render()
    );
    assert!(spent.roster_size > 0, "the timeout must have happened AFTER the server answered");
    assert_eq!(spent.tool_calls, 0, "no tool call may go out once the budget is spent");
    assert_ne!(spent.outcome.exit_code(), 0, "a timeout must not exit 0");

    // ── 4. THE RECORD IS READABLE, and both halves land in ONE directory ─────────────────────────
    let text = std::fs::read_to_string(default_path).expect("read the record back");
    let record: Value = serde_json::from_str(&text).expect("the record must be readable JSON");
    assert_eq!(record["kind"], json!(RECORD_KIND));
    assert_eq!(record["outcome"], json!("ok"));
    assert_eq!(record["profile"], json!(READ_ONLY_PROFILE));
    assert_eq!(record["node"].as_str(), default.node.as_deref(), "the record must name the node");
    assert!(
        record["conclusion"].as_str().is_some_and(|c| !c.trim().is_empty()),
        "the record must carry what the agent concluded: {text}"
    );
    assert!(
        record["started_utc"].as_str().is_some_and(|s| s.ends_with('Z')),
        "the record must carry a human-readable UTC stamp: {text}"
    );
    assert!(
        !text.contains("DUMMY-"),
        "the record carried a node key — nothing about the credential store belongs in it:\n{text}"
    );

    // …and the per-call half really did land beside it, written by the SERVER rather than by this
    // crate — the `--trace-dir` the runner always passes.
    let dir = &scenarios[0].record_dir;
    let traces = trace_files(dir);
    assert!(
        !traces.is_empty(),
        "the per-call transcript is missing from {}; the runner must pass --trace-dir so the \
         server's own record lands beside the run record. The directory held: {:?}",
        dir.display(),
        listing(dir)
    );
    let trace = std::fs::read_to_string(&traces[0]).expect("read the transcript");
    assert!(
        trace.lines().any(|l| l.contains("node_snapshot")),
        "the transcript must carry the tool the agent called:\n{trace}"
    );

    stopped.expect("the paper node must stop cleanly");
}

/// Every `mcp-*.jsonl` the server's own transcript writer left in the record directory.
fn trace_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("mcp-") && n.ends_with(".jsonl"))
        })
        .collect();
    out.sort();
    out
}

/// What a directory holds, for a failure message that says more than "it was not there".
fn listing(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()
        })
        .unwrap_or_default()
}
