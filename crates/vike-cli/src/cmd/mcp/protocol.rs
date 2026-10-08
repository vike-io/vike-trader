//! The JSON-RPC face: one message in, at most one response out, and the envelope helpers.

use serde_json::{Value, json};

use super::instructions::instructions;
use super::prompts::{prompt_description, prompts_spec, render_prompt};
use super::resources::{render, resources_spec_for};
use super::{DEFAULT_PROTOCOL, SERVER_NAME, SERVER_VERSION, Server};

impl Server {
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
                Some(match outcome {
                    Ok(Ok(structured)) => rpc_result(id, tool_ok(structured)),
                    Ok(Err(e)) => rpc_result(id, tool_err(&e.message)),
                    Err(_) => rpc_result(id, tool_err(&format!("tool panicked: {name}"))),
                })
            }
            _ => id.map(|id| rpc_error(id, -32601, &format!("method not found: {method}"))),
        }
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
