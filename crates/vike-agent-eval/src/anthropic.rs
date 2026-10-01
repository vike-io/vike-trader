//! The real model in the loop: the Anthropic Messages API, driving the MCP server's tools.
//!
//! The loop is the ordinary agent loop and nothing more — model turn, `tool_use` blocks executed
//! against the real MCP server, `tool_result` blocks fed back, repeat until the model stops or the
//! step cap is reached. What matters here is what is NOT in it:
//!
//!   * **No procedural help.** [`SYSTEM`] tells the model what surface it is on and stops. What the
//!     SERVER says about itself — its tool descriptions, and its `initialize` `instructions` —
//!     is the whole of what the model is taught, because that is exactly the claim under test: an
//!     agent reading only the shipped surface reaches the operator's intended outcome through the
//!     mandatory preview gate.
//!     ⚠ [`system_for`] appending `instructions` to the system prompt is that rule being OBEYED,
//!     not bent. The field ships in the `initialize` response and every real client renders it;
//!     this driver has no client between it and the server, so a harness that dropped the field
//!     would grade the model against a surface no operator has. Nothing of the harness's own
//!     invention joins the string.
//!   * **No grading.** This module produces a transcript and a final string. Nothing here decides
//!     whether a case passed; `crates/vike-agent-eval/src/grade.rs` does that, deterministically.
//!
//! ⚠ THE COPY THIS STRUCT HOLDS NEVER LEAVES IT. It is read once, in `main.rs`, from the process
//! environment; it is passed as a header and nowhere else; [`Anthropic`]'s `Debug` redacts it; and
//! no error path formats it. It is never an argv (visible in a process listing), never a log line,
//! never a transcript entry and never in the report.
//!
//! ⚠ That claim is about the COPY, deliberately — the ORIGINAL is a variable in this process's own
//! environment, which every child inherits by default, and saying otherwise here would describe
//! only half of where the key is. The other half is `main.rs`'s `SCRUB_FROM_CHILDREN`, threaded to
//! `crates/vike-agent-eval/src/mcp.rs`'s `apply_case_env`, which removes the variable from both
//! binaries the harness spawns per case.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};

use crate::driver::{DriveContext, ModelDriver, Outcome, ToolChannel};

/// The Messages endpoint.
pub const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
/// The API version header every request carries.
pub const API_VERSION: &str = "2023-06-01";
/// The model used when `--model` names none.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
/// The reply ceiling per turn. Answers here are short — a tool call or a paragraph — and a large
/// ceiling only widens the blast radius of a model that decides to narrate.
const MAX_TOKENS: u64 = 4096;
/// The whole-request ceiling. A model turn that has not answered by then is a harness failure, not
/// a slow one: every wait in this crate is bounded.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// The only instruction the model is given beyond the tool descriptions themselves.
pub const SYSTEM: &str = "You are operating a trading system through the tools you have been given \
                          and nothing else. Use the tools to answer; when a tool refuses or the \
                          system is unreachable, say so plainly rather than guessing. Finish with a \
                          short answer for the operator.";

/// One `tool_use` block from a model turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// One model turn, reduced to what the loop needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Turn {
    pub stop_reason: String,
    /// Every `text` block, joined — the answer as the operator would read it.
    pub text: String,
    pub tool_uses: Vec<ToolUse>,
    /// The assistant `content` array verbatim, to be echoed back as the next turn's history.
    pub content: Value,
}

/// Translate the MCP roster into the Messages API's `tools` array.
///
/// The three fields the API names are `name`, `description`, `input_schema`; MCP spells the last
/// one `inputSchema` and adds `annotations`, which the API has no slot for. A tool with no schema
/// is given the empty object schema rather than being dropped: dropping it would silently shrink
/// the roster the agent is being evaluated against.
pub fn tools_for_api(mcp_tools: &[Value]) -> Vec<Value> {
    mcp_tools
        .iter()
        .filter_map(|t| {
            let name = t["name"].as_str()?;
            let schema = if t["inputSchema"].is_object() {
                t["inputSchema"].clone()
            } else {
                json!({ "type": "object", "properties": {} })
            };
            Some(json!({
                "name": name,
                "description": t["description"].as_str().unwrap_or_default(),
                "input_schema": schema,
            }))
        })
        .collect()
}

/// The request body for one turn.
pub fn request_body(model: &str, system: &str, tools: &[Value], messages: &[Value]) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "system": system,
        "tools": tools,
        "messages": messages,
    })
}

/// [`SYSTEM`] plus the server's own `instructions`, which is where an MCP CLIENT puts them.
///
/// ⚠ This is not procedural help smuggled past the clean-room rule — it is the clean room being
/// assembled correctly. `instructions` is a field of the shipped `initialize` response
/// (`crates/vike-cli/src/cmd/mcp.rs`'s `instructions`), and a real client shows it to the model.
/// This driver has no MCP client between it and the server: the harness IS the client, so if the
/// harness drops the field, the model is handed a SMALLER surface than any operator's client would
/// hand it, and every case then grades what the model remembered about this product instead. That
/// is precisely what the 2026-09-06 run measured, four times.
///
/// The join is one blank line and nothing else — no framing sentence of our own, because a sentence
/// this harness invented would be exactly the procedural help the clean room excludes. An empty
/// `instructions` returns [`SYSTEM`] unchanged.
pub fn system_for(instructions: &str) -> String {
    if instructions.trim().is_empty() {
        return SYSTEM.to_string();
    }
    format!("{SYSTEM}\n\n{instructions}")
}

/// Reduce one Messages response to a [`Turn`].
///
/// An `error` body is returned as `Err` with the API's own `type`/`message` — never swallowed into
/// an empty turn, which would look to the loop like a model that had nothing to say.
pub fn parse_turn(resp: &Value) -> Result<Turn, String> {
    if resp["type"] == json!("error") {
        return Err(format!(
            "the Messages API answered an error: {} — {}",
            resp["error"]["type"].as_str().unwrap_or("?"),
            resp["error"]["message"].as_str().unwrap_or("?")
        ));
    }
    let content = resp
        .get("content")
        .filter(|c| c.is_array())
        .ok_or("the Messages API answered no `content` array")?
        .clone();
    let mut text = String::new();
    let mut tool_uses = Vec::new();
    for block in content.as_array().expect("checked above") {
        match block["type"].as_str() {
            Some("text") => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(block["text"].as_str().unwrap_or_default());
            }
            Some("tool_use") => tool_uses.push(ToolUse {
                id: block["id"].as_str().unwrap_or_default().to_string(),
                name: block["name"].as_str().unwrap_or_default().to_string(),
                input: block["input"].clone(),
            }),
            // Every other block type (thinking, redacted content, a type added after this was
            // written) is carried in `content` for the next turn's history and contributes nothing
            // to what is graded.
            _ => {}
        }
    }
    Ok(Turn {
        stop_reason: resp["stop_reason"].as_str().unwrap_or_default().to_string(),
        text,
        tool_uses,
        content,
    })
}

/// The `tool_result` block for one executed tool call.
///
/// The tool's own text is passed through verbatim, `is_error` included: an MCP refusal is a
/// sentence written for an agent to act on (the preview gate's wording, the DOWN error's stale-frame
/// warning), and paraphrasing it here would evaluate this harness's paraphrase instead of the
/// shipped surface.
pub fn tool_result_block(id: &str, text: &str, is_error: bool) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": [{ "type": "text", "text": text }],
        "is_error": is_error,
    })
}

/// The real-model driver.
pub struct Anthropic {
    key: String,
    model: String,
    max_steps: usize,
    endpoint: String,
    agent: ureq::Agent,
}

impl fmt::Debug for Anthropic {
    /// Redacts the key. The manual impl is the point: a derived one would print a live API
    /// credential into any error, panic message or debug log that formats this struct.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Anthropic")
            .field("model", &self.model)
            .field("max_steps", &self.max_steps)
            .field("endpoint", &self.endpoint)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl Anthropic {
    /// `key` comes from `main.rs`'s one environment read and is never stored anywhere else.
    pub fn new(key: String, model: String, max_steps: usize) -> Self {
        Self {
            key,
            model,
            max_steps,
            endpoint: ENDPOINT.to_string(),
            agent: ureq::Agent::config_builder()
                // The API's own error bodies carry the `type`/`message` `parse_turn` reports, so a
                // 4xx must arrive as a RESPONSE to be read rather than as a transport error.
                .http_status_as_error(false)
                .timeout_global(Some(REQUEST_TIMEOUT))
                .user_agent("vike-agent-eval")
                .build()
                .new_agent(),
        }
    }

    /// One round trip.
    fn post(&self, body: &Value) -> Result<Value, String> {
        let payload = serde_json::to_string(body).map_err(|e| format!("encode request: {e}"))?;
        let mut resp = self
            .agent
            .post(&self.endpoint)
            .header("x-api-key", &self.key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .send(payload.as_bytes())
            // ⚠ The error is the transport's, and it is reported without the request body: the
            // body is harmless, but the habit of formatting "what we sent" beside a credentialed
            // request is how a key reaches a log.
            .map_err(|e| format!("POST {}: {e}", self.endpoint))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| format!("read body: {e}"))?;
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| format!("HTTP {status}: the body is not JSON ({e}): {text}"))?;
        Ok(parsed)
    }
}

impl ModelDriver for Anthropic {
    fn name(&self) -> String {
        self.model.clone()
    }

    fn drive(
        &mut self,
        ctx: &DriveContext<'_>,
        prompt: &str,
        tools: &[Value],
        channel: &mut dyn ToolChannel,
    ) -> Result<Outcome, String> {
        let api_tools = tools_for_api(tools);
        // The server's `instructions` reach the model the way a client would deliver them — see
        // [`system_for`]. Computed once: it is the same string for every turn of this case.
        let system = system_for(ctx.instructions);
        let mut messages = vec![json!({ "role": "user", "content": prompt })];
        let mut last_text = String::new();
        for step in 1..=self.max_steps {
            let turn = parse_turn(&self.post(&request_body(
                &self.model,
                &system,
                &api_tools,
                &messages,
            ))?)?;
            if !turn.text.is_empty() {
                last_text = turn.text.clone();
            }
            if turn.tool_uses.is_empty() {
                return Ok(Outcome { final_text: last_text, steps: step });
            }
            messages.push(json!({ "role": "assistant", "content": turn.content }));
            let mut results = Vec::new();
            for use_ in &turn.tool_uses {
                // A tool the server does not advertise still goes THROUGH the server, which answers
                // `unknown tool` — deliberately, because "the agent tried to call a tool that does
                // not exist" is a finding the transcript must carry rather than something this
                // harness hides by refusing locally.
                let result = channel.call(&use_.name, &use_.input)?;
                results.push(tool_result_block(&use_.id, &result.text, result.is_error));
            }
            messages.push(json!({ "role": "user", "content": results }));
        }
        // The cap is a FAILURE of the case, not of the harness: a model still calling tools after
        // its budget has not answered the operator, and reporting the last thing it said as if it
        // had would credit it with an answer it never finished.
        Err(format!(
            "the model was still calling tools after {} steps (--max-steps); the case has no \
             answer. Last text: {last_text}",
            self.max_steps
        ))
    }
}
