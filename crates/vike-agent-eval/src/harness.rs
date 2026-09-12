//! Running one case end to end: stand the world up, let the driver drive, read the world back, grade.
//!
//! The order below is the whole contract, and two of its steps are the ones a careless harness gets
//! wrong:
//!
//!   * **Setup goes through the SAME server, tagged as the harness.** The order a cancel case has to
//!     cancel is rested by this module, over the same connection the agent will use, recorded in the
//!     same transcript — and marked [`Actor::Harness`], so the grader never credits the agent with
//!     it.
//!   * **The node is read back AFTER the agent stops, and re-read while a node check is failing.**
//!     The node folds a command on its own thread and republishes, so a single read straight after
//!     the confirm would race the fold. What is waited on is the OBSERVABLE, bounded, never a sleep
//!     of a guessed length.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::cases::{Case, NodeSetup};
use crate::driver::{DriveContext, ModelDriver, ToolChannel};
use crate::grade::{Check, Evidence, Roster, grade};
use crate::mcp::{Actor, CaseEnv, McpServer, ToolResult};
use crate::node::{
    CONTROL_KEY, NODE_SYMBOL, NODE_VENUE, OBSERVE_KEY, PaperNode, Project, pick_port,
};
use crate::report::CaseVerdict;

/// How long the node's published state may take to reflect a command before a node check is called
/// failed. The node publishes on its own thread when the fold marks it dirty.
const OBSERVE_SETTLE: Duration = Duration::from_secs(20);
/// How long the harness waits for the node's FIRST real frame after subscribing.
const FIRST_FRAME: Duration = Duration::from_secs(20);

/// The two binaries this harness spawns.
pub struct Binaries {
    pub vike_cli: PathBuf,
    pub vike_tradehub: PathBuf,
}

/// The `McpServer` seen through the narrow hole a driver is allowed.
struct AgentChannel<'a> {
    server: &'a mut McpServer,
}

impl ToolChannel for AgentChannel<'_> {
    fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, String> {
        self.server.call_tool(Actor::Agent, name, args)
    }
}

/// Run one case. A harness-level failure is captured into the verdict rather than propagated: one
/// case that could not be stood up must not take the rest of the suite with it.
///
/// `scrub` names the variables removed from every child's environment on top of
/// [`crate::mcp::apply_case_env`]'s own list — the harness binary's secrets. It is a parameter
/// because only the binary knows what it read.
pub fn run_case(
    case: &'static Case,
    bins: &Binaries,
    driver: &mut dyn ModelDriver,
    work: &Path,
    scrub: &[&str],
) -> CaseVerdict {
    let dir = work.join(case.name);
    match run_case_inner(case, bins, driver, &dir, scrub) {
        Ok(v) => v,
        Err(e) => CaseVerdict {
            name: case.name,
            skill: case.skill,
            pass: false,
            steps: 0,
            checks: Vec::new(),
            error: Some(e),
            transcript: Value::Array(Vec::new()),
            final_text: String::new(),
        },
    }
}

fn run_case_inner(
    case: &'static Case,
    bins: &Binaries,
    driver: &mut dyn ModelDriver,
    dir: &Path,
    scrub: &[&str],
) -> Result<CaseVerdict, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let project = Project::create(&dir.join("project"))?;

    let mut node = match case.node {
        NodeSetup::None => None,
        _ => Some(PaperNode::start(&bins.vike_tradehub, &project, scrub)?),
    };
    let node_addr = node.as_ref().map(|n| n.addr.clone());

    // ⚠ The datahub address is a port PROVEN FREE, never the default. The default is
    // `127.0.0.1:7878`, and a box that happens to be running a real datahub there — the CI box does —
    // would turn the three "report the unreachable server honestly" cases into cases about whatever
    // that server holds. Pointing at a dead port is what makes them deterministic.
    let dead = pick_port()?;
    let dead_addr = format!("127.0.0.1:{dead}");

    let mut server = McpServer::start(
        &bins.vike_cli,
        node_addr.as_deref(),
        &CaseEnv { settings_dir: &project.settings, scrub },
        OBSERVE_KEY,
        CONTROL_KEY,
        &dir.join("mcp.err"),
        &dead_addr,
    )?;
    let (tools, instructions) = server.initialize()?;
    let roster = Roster { tools };

    let mut setup_coid = None;
    let mut stale_order_ids = Vec::new();
    if node.is_some() {
        wait_for_real_frame(&mut server)?;
        if matches!(case.node, NodeSetup::RunningWithRestingOrder | NodeSetup::StoppedAfterConnect)
        {
            let coid = rest_an_order(&mut server)?;
            if case.node == NodeSetup::StoppedAfterConnect {
                // The frame that is about to go STALE — read once more so the ids in it are the ones
                // the agent could leak, then stop the node under the still-open connection.
                stale_order_ids = live_order_ids(&mut server)?;
            }
            setup_coid = Some(coid);
        }
        if case.node == NodeSetup::StoppedAfterConnect {
            if let Some(n) = node.as_mut() {
                n.stop()?;
            }
            node = None;
            wait_until_the_node_reads_down(&mut server)?;
        }
    }

    let outcome = {
        let mut channel = AgentChannel { server: &mut server };
        // The driver is handed the case it is driving and the case's own throwaway directory —
        // everything a driver that SPAWNS A PROCESS needs, and nothing that would let one see the
        // transcript it is about to be graded on.
        let ctx = DriveContext { case: case.name, work_dir: dir, instructions: &instructions };
        driver.drive(&ctx, case.prompt, &roster.tools, &mut channel)
    };
    let (final_text, steps) = match outcome {
        Ok(o) => (o.final_text, o.steps),
        // A driver that could not finish is a HARNESS-level failure, and it is reported as one:
        // grading a partial run against the case's expectations would report a model's step cap as
        // a surface defect.
        Err(e) => {
            let transcript = server.transcript.to_json();
            let _ = server.shutdown();
            if let Some(n) = node.as_mut() {
                let _ = n.stop();
            }
            return Ok(CaseVerdict {
                name: case.name,
                skill: case.skill,
                pass: false,
                steps: 0,
                checks: Vec::new(),
                error: Some(e),
                transcript,
                final_text: String::new(),
            });
        }
    };

    // Read the node back and grade; re-read while a NODE check is failing and time remains.
    let needs_node = case.checks.iter().any(|c| c.reads_node());
    let deadline = Instant::now() + OBSERVE_SETTLE;
    let mut checks;
    loop {
        let post = if node.is_some() { snapshot(&mut server).ok() } else { None };
        checks = grade(
            case.checks,
            &Evidence {
                transcript: &server.transcript,
                final_text: &final_text,
                roster: &roster,
                post_snapshot: post.as_ref(),
                setup_coid: setup_coid.as_deref(),
                stale_order_ids: &stale_order_ids,
            },
        );
        let node_failing = case
            .checks
            .iter()
            .zip(checks.iter())
            .any(|(c, outcome)| c.reads_node() && !outcome.pass);
        if !needs_node || !node_failing || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    let transcript = server.transcript.to_json();
    // ⚠ Both teardowns are FOLDED INTO THE VERDICT rather than propagated. A `?` here would discard
    // a set of checks that had already been computed and report the case as a bare harness error —
    // throwing away the finding to report the tidying-up. Both still fail the case: a server that
    // outlives its own EOF, or a node whose port is still accepting after it was killed, is a
    // finding about this run and the next case is about to want that port.
    let teardown: Vec<String> =
        [server.shutdown().err(), node.as_mut().and_then(|n| n.stop().err())]
            .into_iter()
            .flatten()
            .collect();
    let teardown = (!teardown.is_empty()).then(|| teardown.join("; "));
    let pass = checks.iter().all(|c| c.pass) && teardown.is_none();
    Ok(CaseVerdict {
        name: case.name,
        skill: case.skill,
        pass,
        steps,
        checks,
        error: teardown,
        transcript,
        final_text,
    })
}

/// One harness-issued `node_snapshot`.
fn snapshot(server: &mut McpServer) -> Result<Value, String> {
    let r = server.call_tool(Actor::Harness, "node_snapshot", &json!({}))?;
    if r.is_error { Err(r.text) } else { Ok(r.structured) }
}

/// Wait for a frame the PUBLISHER stamped.
///
/// ⚠ The predicate is `identity`, not `seq`. A freshly-subscribed connection holds
/// `WireSnapshot::empty()` — `seq: 0`, an empty order list — and `node_snapshot` returns it as a
/// SUCCESS as soon as the connection is up, so "the call succeeded" is not evidence a frame
/// arrived. `seq > 0` is no better: an idle daemon publishes its placeholder once and only the
/// first fold bumps the sequence. Only the publisher stamps `identity`.
fn wait_for_real_frame(server: &mut McpServer) -> Result<(), String> {
    let deadline = Instant::now() + FIRST_FRAME;
    let mut last = String::new();
    while Instant::now() < deadline {
        match snapshot(server) {
            Ok(s) if !s["identity"].is_null() => return Ok(()),
            Ok(s) => last = s.to_string(),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "the node published no real frame within {}s (last answer: {last})",
        FIRST_FRAME.as_secs()
    ))
}

/// Wait until the MCP server ITSELF reports the node as unreadable, and not merely until its port
/// has come free.
///
/// ⚠ **This is the difference between a case and a coin flip.** `PaperNode::stop` waits on the
/// LISTENING socket, which says nothing about the observe handle the MCP server is still holding in
/// another process: `crates/vike-cli/src/cmd/mcp.rs`'s `tool_node_snapshot` gates on
/// `RemoteCoreHandle::is_connected`, an `AtomicBool` a receive thread flips when its read errors,
/// and until it does the tool returns the last frame as a SUCCESS. Letting the agent run before
/// that flip made `node-is-down` fail once in four identical lane runs — the loaded one — and this
/// crate is in the derived CI roster, so that is a nondeterministic red on other people's PRs.
/// What is waited on is therefore the OBSERVABLE the case is about, through the same server, as
/// the harness.
///
/// ⚠ **Accepted consequence: this probe CONSUMES the stale-frame discovery.** The pass that finds
/// the handle dead is the one that names the stale `seq` (`observe_down`), and it drops the handle;
/// the agent's own first call therefore meets `ensure_observe`'s connect failure instead. Still an
/// `isError`, still the error the agent must report honestly and must not answer around — which is
/// what [`crate::cases::NODE_IS_DOWN`] grades — but the richer "seq N is STALE" wording is spent
/// here. It is taken deliberately: no read-only probe of that handle exists (only `node_snapshot`
/// touches `Server::observe`), and a case that is right three runs in four is worth less than a
/// slightly plainer error the agent meets every time.
fn wait_until_the_node_reads_down(server: &mut McpServer) -> Result<(), String> {
    let deadline = Instant::now() + OBSERVE_SETTLE;
    let mut last = String::new();
    while Instant::now() < deadline {
        match server.call_tool(Actor::Harness, "node_snapshot", &json!({})) {
            Ok(r) if r.is_error => return Ok(()),
            Ok(r) => last = r.structured.to_string(),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!(
        "the node was stopped but node_snapshot still answered successfully for {}s, so the agent \
         would have been handed a stale frame as a live one (last answer: {last})",
        OBSERVE_SETTLE.as_secs()
    ))
}

/// The `client_order_id`s of every non-terminal order the node currently holds.
fn live_order_ids(server: &mut McpServer) -> Result<Vec<String>, String> {
    let snap = snapshot(server)?;
    Ok(snap["orders"]
        .as_array()
        .map(|os| {
            os.iter()
                .filter(|o| {
                    !o["status"]
                        .as_str()
                        .is_some_and(|s| crate::grade::TERMINAL_STATUSES.contains(&s))
                })
                .filter_map(|o| o["client_order_id"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// Rest one far-from-market limit order, as the HARNESS, and wait until the node's own published
/// frame carries it.
///
/// It goes through the mandatory two-call gate like any other write — there is no back door, and
/// wanting one would be the first sign this harness had started testing something other than the
/// shipped surface. The paper mount has no feed, so the order stays working and the node's snapshot
/// deterministically carries exactly what was placed.
fn rest_an_order(server: &mut McpServer) -> Result<String, String> {
    let args = json!({
        "venue": NODE_VENUE,
        "symbol": NODE_SYMBOL,
        "side": 1,
        "qty": 20.0,
        "order_type": "limit",
        "price": 0.40,
        "reason": "agent-eval harness: the order this case's prompt is about"
    });
    let preview = server.call_tool(Actor::Harness, "submit_order", &args)?;
    if preview.is_error {
        return Err(format!("the setup preview failed: {}", preview.text));
    }
    let token = preview.structured["preview_token"]
        .as_str()
        .ok_or("the setup preview issued no preview_token")?
        .to_string();
    let coid = preview.structured["wire_command"]["Submit"]["client_order_id"]
        .as_str()
        .ok_or("the setup preview minted no client_order_id")?
        .to_string();
    let mut confirming = args.clone();
    confirming["confirm"] = json!(true);
    confirming["preview_token"] = json!(token);
    let sent = server.call_tool(Actor::Harness, "submit_order", &confirming)?;
    if sent.is_error || sent.structured["outcome"] != json!("accepted") {
        return Err(format!("the setup order was not accepted: {} {}", sent.text, sent.structured));
    }
    let deadline = Instant::now() + OBSERVE_SETTLE;
    while Instant::now() < deadline {
        if live_order_ids(server)?.contains(&coid) {
            return Ok(coid);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!("the setup order {coid} never appeared in the node's published frame"))
}

/// Every check a case declares, for the `list` verb's per-case detail.
pub fn checks_of(case: &Case) -> Vec<String> {
    case.checks.iter().map(|c: &Check| format!("{c:?}")).collect()
}
