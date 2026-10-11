//! The JSON-RPC face: one message in, at most one response out, and the envelope helpers — plus the
//! split [`serve`](super::serve) makes between a request answered INLINE ([`Server::handle`]) and a
//! daemon tool DEFERRED to a worker thread ([`Server::classify`], [`run_deferred`],
//! [`Server::finish_deferred`]).
//!
//! # Why exactly three tools are deferred
//!
//! A daemon RUN (`run_backtest`, `run_sweep`, `run_walk_forward`) blocks in one unbounded reply read
//! for as long as the compute daemon computes — minutes for a large search. On the loop's own thread
//! that read is a wall: a `notifications/cancelled` behind it is not even READ until the run is
//! over, which is the same as never. Those three run on a worker and every other request is
//! answered inline exactly as before, so nothing that was fast moved. The worker gets an OWNED
//! [`DeferredCall`] (the id, the tool, its arguments, the daemon address — cloned out of the
//! [`Server`], which never leaves the loop's thread) and hands back a [`DeferredOutcome`]; the loop
//! does everything that touches the `Server` — the transcript, `last_backtest`, the response — so
//! stdout keeps ONE writer.

use std::panic::{AssertUnwindSafe, catch_unwind};

use serde_json::{Value, json};

use super::backtest_tools::{tool_run_backtest, tool_run_paramscan, tool_run_walk_forward};
use super::cancel::CancelToken;
use super::instructions::instructions;
use super::prompts::{prompt_description, prompts_spec, render_prompt};
use super::resources::{render, resources_spec_for};
use super::{DEFAULT_PROTOCOL, SERVER_NAME, SERVER_VERSION, Server, ToolError};

/// The tools [`serve`](super::serve) runs off the loop's thread — the daemon RUNS, and nothing else
/// (the module doc says why). Each is cancellable by `notifications/cancelled`.
pub(super) const DEFERRED_TOOLS: [&str; 3] = ["run_backtest", "run_sweep", "run_walk_forward"];

/// The transcript detail of a call the client cancelled — it got no response, so the record is the
/// only place the call's end is written down.
pub(super) const CANCELLED_DETAIL: &str =
    "cancelled by the client (notifications/cancelled); no response was sent";

/// A JSON-RPC request id as a map key. JSON-RPC allows a number or a string, and `Value` does not
/// hash, so the key is the id's canonical JSON TEXT: `7` and `"7"` stay two different ids, as the
/// spec says they are, and a `notifications/cancelled` naming `7` finds exactly the request `7`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct RequestId(String);

impl RequestId {
    pub(super) fn of(id: &Value) -> Self {
        Self(id.to_string())
    }
}

/// One deferred `tools/call`, OWNED — nothing in it borrows the [`Server`].
///
/// The compute-daemon tools dial `backtest_addr` with the key-less `DatahubClient::connect`, so the
/// address is the whole of the server state a worker needs.
#[derive(Debug)]
pub(super) struct DeferredCall {
    pub(super) id: Value,
    pub(super) name: String,
    pub(super) args: Value,
    backtest_addr: String,
}

/// A tool's result exactly as [`Server::handle`] sees it: a panic, a failure, or the
/// `structuredContent`.
pub(super) type ToolOutcome = std::thread::Result<Result<Value, ToolError>>;

/// What a worker hands back to the loop.
pub(super) enum DeferredOutcome {
    /// The tool ran to an answer (success, failure or panic) with nobody cancelling it.
    Finished(ToolOutcome),
    /// The token was cancelled; whatever the tool returned is the cancel's echo and is dropped.
    Cancelled,
}

/// Run one deferred call on the worker's thread. The tool bodies are the inline ones; the only
/// difference is `token`, which their connection is registered with.
///
/// ⚠ `is_cancelled` is read AFTER the tool returns, and that ordering is the point: a cancel closes
/// the socket, so the tool returns a transport error that would otherwise read as a tool failure.
pub(super) fn run_deferred(call: &DeferredCall, token: &CancelToken) -> DeferredOutcome {
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<Value, ToolError> {
        let (addr, args) = (call.backtest_addr.as_str(), &call.args);
        match call.name.as_str() {
            "run_backtest" => Ok(tool_run_backtest(addr, args, token)?),
            "run_sweep" => Ok(tool_run_paramscan(addr, args, token)?),
            "run_walk_forward" => Ok(tool_run_walk_forward(addr, args, token)?),
            other => Err(ToolError::from(format!("unknown deferred tool: {other}"))),
        }
    }));
    if token.is_cancelled() {
        DeferredOutcome::Cancelled
    } else {
        DeferredOutcome::Finished(outcome)
    }
}

impl Server {
    /// Is this message a `tools/call` [`serve`](super::serve) should run off its thread? `Some` only
    /// for a REQUEST (it has an `id`) naming one of [`DEFERRED_TOOLS`] that this session's scope
    /// admits — a withheld tool stays inline, so its refusal is answered (and recorded) exactly as
    /// before.
    pub(super) fn classify(&self, msg: &Value) -> Option<DeferredCall> {
        if msg.get("method").and_then(Value::as_str) != Some("tools/call") {
            return None;
        }
        let id = msg.get("id")?.clone();
        let name = msg.pointer("/params/name").and_then(Value::as_str)?;
        if !DEFERRED_TOOLS.contains(&name) || !self.access.admits(name) {
            return None;
        }
        let args = msg.pointer("/params/arguments").cloned().unwrap_or_else(|| json!({}));
        Some(DeferredCall {
            id,
            name: name.to_string(),
            args,
            backtest_addr: self.backtest_addr.clone(),
        })
    }

    /// Answer a deferred call that FINISHED, on the loop's thread: what `call_tool` and the
    /// `tools/call` arm of [`Server::handle`] would have done with the same outcome — remember a
    /// `run_backtest` report for `vike://backtest/last`, record the transcript, build the response.
    pub(super) fn finish_deferred(&mut self, call: DeferredCall, outcome: ToolOutcome) -> Value {
        if call.name == "run_backtest"
            && let Ok(Ok(report)) = &outcome
        {
            self.last_backtest = Some(render(report));
        }
        self.trace_call(&call.name, &call.args, &outcome);
        tool_response(call.id, &call.name, outcome)
    }

    /// Record a call the client cancelled. It gets no response; the transcript still says it ended.
    pub(super) fn trace_cancelled(&self, call: &DeferredCall) {
        let outcome: ToolOutcome = Ok(Err(ToolError::from(CANCELLED_DETAIL.to_string())));
        self.trace_call(&call.name, &call.args, &outcome);
    }

    /// Dispatch one JSON-RPC message, returning the response (`None` for a notification — no `id`).
    pub(super) fn handle(&mut self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str)?;
        let id = msg.get("id").cloned();
        match method {
            "initialize" => {
                let proto = msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or(DEFAULT_PROTOCOL)
                    .to_string();
                Some(rpc_result(
                    id?,
                    json!({
                        "protocolVersion": proto,
                        // ⚠ A capability declared here is a PROMISE that the matching methods
                        // answer, and a client that reads `resources` will call `resources/list`
                        // without being asked to. Add a key only alongside its arms.
                        "capabilities": { "tools": {}, "resources": {}, "prompts": {} },
                        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
                        // Free text the client shows the model as guidance about this server —
                        // the ONE channel that can say what this surface is NOT. Scoped by the
                        // same [`ToolAccess`] the roster is; see [`instructions`].
                        "instructions": instructions(&self.access),
                    }),
                ))
            }
            "notifications/initialized" => None,
            "ping" => Some(rpc_result(id?, json!({}))),
            // ⚠ The advertised roster is the SAME derivation the router refuses on — see
            // [`ToolAccess`]. Answering this from `tools_spec()` directly is exactly the drift the
            // type exists to make impossible.
            "tools/list" => Some(rpc_result(id?, json!({ "tools": self.access.advertised() }))),
            "resources/list" => {
                Some(rpc_result(id?, json!({ "resources": resources_spec_for(&self.access) })))
            }
            "resources/read" => {
                let uri = msg.pointer("/params/uri").and_then(Value::as_str).unwrap_or_default();
                // The same guard `tools/call` carries below, and for the same code: a resource is
                // a second WAY IN to the tool's implementation (`vike://node/snapshot` IS
                // `tool_node_snapshot`), so a panic that `tools/call` turns into an `isError`
                // result must not become a dead stdio session when the same code is reached by
                // URI. A resource has no tool-result envelope, so here it is a JSON-RPC error.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.read_resource(uri)
                }));
                match outcome {
                    // MCP wants an ARRAY of contents even for a single-document resource, and the
                    // `uri` echoed back — a client may have several reads in flight.
                    Ok(Ok(text)) => Some(rpc_result(
                        id?,
                        json!({ "contents": [{ "uri": uri, "mimeType": "application/json", "text": text }] }),
                    )),
                    // -32002 is MCP's "resource not found", and an unreachable node lands here too:
                    // a read that could not be served is an ERROR, never an empty document. An empty
                    // document is what an agent would summarise as "the node has no positions".
                    Ok(Err(e)) => Some(rpc_error(id?, -32002, &e)),
                    Err(_) => {
                        Some(rpc_error(id?, -32603, &format!("resource handler panicked: {uri}")))
                    }
                }
            }
            "prompts/list" => Some(rpc_result(id?, json!({ "prompts": prompts_spec() }))),
            "prompts/get" => {
                let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                let empty = json!({});
                let arguments = msg.pointer("/params/arguments").unwrap_or(&empty);
                // Guarded like `resources/read` above. `render_prompt` is pure today, but the guard
                // is a property of the SESSION, not of the handler: every arm that runs code on the
                // agent's behalf carries it, so the next arm added cannot forget.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render_prompt(name, arguments, &self.access)
                }));
                match outcome {
                    Ok(Ok(text)) => Some(rpc_result(
                        id?,
                        // A prompt is a USER turn the client pastes in — `role: "user"`, not
                        // "system": these are instructions the human is asking for, and a client
                        // that shows them to the human is doing the right thing.
                        json!({
                            "description": prompt_description(name),
                            "messages": [
                                { "role": "user", "content": { "type": "text", "text": text } }
                            ]
                        }),
                    )),
                    // -32602 (invalid params) rather than a rendered apology: an unknown prompt
                    // name is the CLIENT asking for something that does not exist, and answering
                    // it with prose would hand an agent an instruction sheet it invented the name of.
                    Ok(Err(e)) => Some(rpc_error(id?, -32602, &e)),
                    Err(_) => {
                        Some(rpc_error(id?, -32603, &format!("prompt handler panicked: {name}")))
                    }
                }
            }
            "tools/call" => {
                let id = id?;
                let name = msg.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
                let empty = json!({});
                let arguments = msg.pointer("/params/arguments").unwrap_or(&empty);
                // A panicking tool handler must not take down the stdio session (the vike-mcp
                // `rpc.rs` catch_unwind idiom): surface it as an `isError` tool result, same as
                // `Err(String)`, not a JSON-RPC protocol error and never a dead loop.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.call_tool(name, arguments)
                }));
                // ⚠ THE TRANSCRIPT, classified from the SAME value the response is built from and
                // BEFORE that value is consumed — so the record cannot describe an outcome
                // different from the one the client was told. A no-op when `--trace` was not asked
                // for, and never a `println!` at any point: this stdout is the protocol.
                self.trace_call(name, arguments, &outcome);
                Some(tool_response(id, name, outcome))
            }
            _ => id.map(|id| rpc_error(id, -32601, &format!("method not found: {method}"))),
        }
    }
}

/// The `tools/call` response for one outcome — ONE spelling for the inline arm and the deferred
/// path, so a daemon tool answers in exactly the shape it did before it moved off the loop's thread.
fn tool_response(id: Value, name: &str, outcome: ToolOutcome) -> Value {
    match outcome {
        Ok(Ok(structured)) => rpc_result(id, tool_ok(structured)),
        Ok(Err(e)) => rpc_result(id, tool_err(&e.message)),
        Err(_) => rpc_result(id, tool_err(&format!("tool panicked: {name}"))),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub(super) fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

pub(super) fn tool_ok(structured: Value) -> Value {
    let text = render(&structured);
    json!({ "content": [ { "type": "text", "text": text } ], "structuredContent": structured, "isError": false })
}

fn tool_err(message: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": message } ], "isError": true })
}
