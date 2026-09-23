//! The seam a model plugs into, and the canned driver CI runs in its place.
//!
//! A [`ModelDriver`] is handed one user prompt and the tool roster the real MCP server advertised,
//! and drives that server through a [`ToolChannel`] until it stops. It returns the FINAL TEXT it
//! would have shown the operator — which is half the evidence the grader reads; the other half is
//! the transcript the channel recorded on the way through.
//!
//! ⚠ The three implementations exist for different reasons and none replaces another.
//! [`crate::anthropic::Anthropic`] and [`crate::claude_cli::ClaudeCli`] are both the measurement —
//! nondeterministic, never a merge gate — and they differ in WHO IS BILLED: the first spends API
//! credits per model turn, the second drives the locally-installed Claude Code CLI and spends the
//! operator's subscription. [`Scripted`] is the PIPELINE gate: it removes the model and leaves
//! everything else — the real server, the real paper node, the real preview-token flow, the real
//! grader — so a break in any of those reddens CI on every PR without a model in the loop.

use std::path::Path;

use serde_json::{Value, json};

use crate::mcp::ToolResult;

/// What a driver may do to the system under test: call one advertised tool.
///
/// Deliberately the WHOLE surface. A driver cannot read the transcript, cannot see the node except
/// through a tool, and cannot influence grading — which is what stops a driver from being able to
/// make itself pass.
pub trait ToolChannel {
    /// Call one tool by name. `Err` is a TRANSPORT failure (the server is gone); a tool that
    /// answered `isError` is `Ok` carrying that answer, because a refusal is information the agent
    /// is supposed to read and act on.
    fn call(&mut self, name: &str, args: &Value) -> Result<ToolResult, String>;
}

/// What one case's drive produced.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// The final text the agent would have shown the operator.
    pub final_text: String,
    /// How many model turns (or scripted steps) were spent. Reported so the cost of a case is
    /// visible: with a real model this is also the number of API calls it made.
    pub steps: usize,
}

/// Which case is being driven, and where a driver may put its own files.
///
/// ⚠ It exists because a driver that SPAWNS A PROCESS needs both and a driver that does not needs
/// neither. [`crate::claude_cli::ClaudeCli`] writes an MCP client configuration and a child's
/// stderr somewhere, and its timeout message has to name the case it killed — and neither fact is
/// derivable from the prompt. Handing every driver the pair is cheaper than a second trait, and it
/// keeps the one rule that matters: `work_dir` is the case's own throwaway directory, never a path
/// this process chose for itself and never the operating system's temp directory (the rule
/// `crates/vike-ops/tests/system_temp_gate.rs` states).
pub struct DriveContext<'a> {
    /// The case's `--case` name, for an error that has to say which run it ended.
    pub case: &'a str,
    /// The case's throwaway directory. It EXISTS by the time a driver is called
    /// (`crates/vike-agent-eval/src/harness.rs`'s `run_case_inner` creates it) and it is removed
    /// with the run unless `--keep-work` was passed.
    pub work_dir: &'a Path,
    /// The server's own `instructions` string from `initialize` — free text an MCP client shows the
    /// model as guidance about the server (`crates/vike-cli/src/cmd/mcp.rs`'s `instructions`).
    ///
    /// ⚠ It rides here rather than beside `tools` because **each driver's CLIENT is a different
    /// program, so each renders it by a different mechanism**, and a single argument slot would
    /// have implied one. [`crate::anthropic::Anthropic`] has no MCP client at all — this harness is
    /// it — so the string is appended to the system prompt, which is where a client puts it.
    /// [`crate::claude_cli::ClaudeCli`] hands it to a real client through the bridge's own
    /// `initialize`, the SHIPPED path. [`Scripted`] ignores it: a canned script is not reading
    /// anything.
    ///
    /// Empty when the server served none, and a driver renders an empty string as nothing.
    pub instructions: &'a str,
}

/// A driver of the MCP surface under evaluation.
pub trait ModelDriver {
    /// Identifies the driver in the report — the model id, or `scripted`.
    ///
    /// ⚠ It may CHANGE across a run, and one driver's does: [`crate::claude_cli::ClaudeCli`] cannot
    /// know which model the subscription will hand it until the CLI has answered once, so it names
    /// the request before the first case and the concrete id afterwards. `main.rs` therefore reads
    /// it again after the suite and stamps the report with what actually answered.
    fn name(&self) -> String;

    /// Drive one case. `tools` is the roster exactly as `tools/list` answered it.
    fn drive(
        &mut self,
        ctx: &DriveContext<'_>,
        prompt: &str,
        tools: &[Value],
        channel: &mut dyn ToolChannel,
    ) -> Result<Outcome, String>;
}

/// One step of a canned plan.
///
/// The plan is not a recorded transcript replayed back: [`Step::ConfirmOf`] and
/// [`Step::CallWithFirstOrderCoid`] resolve against what the REAL server answered a moment earlier,
/// so the preview token and the client-order-id a scripted run confirms are the live ones. A plan
/// of fixed JSON could not confirm anything — the token is minted per preview and fires once.
#[derive(Debug, Clone)]
pub enum Step {
    /// Call `tool` with exactly these arguments.
    Call { tool: String, args: Value },
    /// Call `tool` with `args` plus the `client_order_id` of the first order in the snapshot the
    /// step at `from` returned — the shape an agent reaches after reading `node_snapshot`.
    CallWithFirstOrderCoid { tool: String, from: usize, args: Value },
    /// Re-issue the call made at `from`, adding `confirm: true` and the `preview_token` that call's
    /// preview returned. This is the mandatory two-call gate, driven the way an agent must.
    ConfirmOf { from: usize },
    /// Stop, answering the operator with this text.
    Answer(String),
}

/// The canned driver: a fixed plan per case, no model.
pub struct Scripted {
    plan: Vec<Step>,
}

impl Scripted {
    pub fn new(plan: Vec<Step>) -> Self {
        Self { plan }
    }
}

impl ModelDriver for Scripted {
    fn name(&self) -> String {
        "scripted".to_string()
    }

    fn drive(
        &mut self,
        _ctx: &DriveContext<'_>,
        _prompt: &str,
        _tools: &[Value],
        channel: &mut dyn ToolChannel,
    ) -> Result<Outcome, String> {
        // What each step called and what came back, so a later step can resolve against it.
        let mut done: Vec<(String, Value, ToolResult)> = Vec::new();
        for (i, step) in self.plan.iter().enumerate() {
            let steps = i + 1;
            let (tool, args) = match step {
                Step::Answer(text) => {
                    return Ok(Outcome { final_text: text.clone(), steps });
                }
                Step::Call { tool, args } => (tool.clone(), args.clone()),
                Step::CallWithFirstOrderCoid { tool, from, args } => {
                    let (_, _, prior) =
                        done.get(*from).ok_or_else(|| format!("step {from} has not run yet"))?;
                    let coid = prior.structured["orders"][0]["client_order_id"]
                        .as_str()
                        .ok_or_else(|| {
                            format!(
                                "step {from} returned no orders to take a client_order_id from: {}",
                                prior.text
                            )
                        })?
                        .to_string();
                    let mut a = args.clone();
                    a["client_order_id"] = json!(coid);
                    (tool.clone(), a)
                }
                Step::ConfirmOf { from } => {
                    let (tool, args, preview) =
                        done.get(*from).ok_or_else(|| format!("step {from} has not run yet"))?;
                    let token = preview.structured["preview_token"].as_str().ok_or_else(|| {
                        format!(
                            "step {from} returned no preview_token, so nothing can be confirmed: {}",
                            preview.text
                        )
                    })?;
                    let mut a = args.clone();
                    a["confirm"] = json!(true);
                    a["preview_token"] = json!(token);
                    (tool.clone(), a)
                }
            };
            // ⚠ A tool that answers `isError` does NOT stop the plan. An error is information the
            // grader is meant to see — the node-down case exists precisely to assert one — and a
            // driver that bailed on it would turn a graded refusal into a harness failure.
            let result = channel.call(&tool, &args)?;
            done.push((tool, args, result));
        }
        Err("the scripted plan ended without an Answer step — every plan must state what the agent \
             would have told the operator, because the final text is half of what is graded"
            .to_string())
    }
}
