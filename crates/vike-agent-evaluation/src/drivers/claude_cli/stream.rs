//! Reading the CLI's `--output-format stream-json` and deciding whether it measured anything.

use serde_json::{Value, json};

use super::MCP_SERVER_NAME;
use crate::drivers::Outcome;

/// What the CLI's `--output-format stream-json` said, reduced to what this driver reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamSummary {
    /// The model id off the `system`/`init` line — what actually answered.
    pub model: Option<String>,
    /// `(name, status)` per MCP server the CLI connected.
    pub mcp_servers: Vec<(String, String)>,
    /// The tool names the CLI exposed to the model, `mcp__<server>__<tool>`-prefixed.
    pub exposed_tools: Vec<String>,
    /// The `result` line's own text — the answer as the operator would read it.
    pub final_text: String,
    /// The `result` line's `is_error`. ⚠ ABSENT reads as an error: a stream shape this driver does
    /// not understand must never be graded as a run that happened.
    pub is_error: bool,
    pub turns: usize,
    pub result_seen: bool,
}

/// Reduce the CLI's stream to a [`StreamSummary`].
///
/// Lines that are not JSON are IGNORED for parsing but reported when nothing usable was found — the
/// CLI writes its own startup refusals as plain text, and swallowing them would turn "the flags
/// were wrong" into "the model said nothing".
pub fn parse_stream(lines: &[String]) -> Result<StreamSummary, String> {
    let mut out = StreamSummary { is_error: true, ..StreamSummary::default() };
    let mut noise: Vec<&str> = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            noise.push(trimmed);
            continue;
        };
        match v["type"].as_str() {
            Some("system") if v["subtype"] == json!("init") => {
                out.model = v["model"].as_str().map(str::to_string);
                out.mcp_servers = v["mcp_servers"]
                    .as_array()
                    .map(|servers| {
                        servers
                            .iter()
                            .map(|s| {
                                (
                                    s["name"].as_str().unwrap_or_default().to_string(),
                                    s["status"].as_str().unwrap_or_default().to_string(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.exposed_tools = v["tools"]
                    .as_array()
                    .map(|ts| ts.iter().filter_map(|t| t.as_str()).map(str::to_string).collect())
                    .unwrap_or_default();
            }
            Some("result") => {
                out.result_seen = true;
                out.final_text = v["result"].as_str().unwrap_or_default().to_string();
                out.is_error = v["is_error"].as_bool().unwrap_or(true);
                out.turns = usize::try_from(v["num_turns"].as_u64().unwrap_or(0)).unwrap_or(0);
            }
            _ => {}
        }
    }
    if !out.result_seen {
        let tail: Vec<&str> = noise.iter().rev().take(5).copied().collect();
        return Err(format!(
            "the Claude Code CLI produced no `result` line, so nothing was measured. {} JSON \
             line(s) and {} non-JSON line(s) were read{}",
            lines.len().saturating_sub(noise.len()),
            noise.len(),
            if tail.is_empty() {
                String::new()
            } else {
                format!("; its plain output was: {}", tail.join(" | "))
            }
        ));
    }
    Ok(out)
}

/// Turn a parsed stream into the case's outcome, or into the reason there is none.
///
/// ⚠ Three ways a run reads GREEN while having measured nothing, all refused here: a stream with no
/// `result` (refused in [`parse_stream`]), a run whose MCP server never connected (the agent had no
/// tools at all), and a `result` carrying `is_error` — which is what an expired subscription
/// session looks like, and whose `result` TEXT is an apology the grader would otherwise have read
/// as the agent's answer.
pub fn outcome_of(summary: &StreamSummary) -> Result<Outcome, String> {
    let connected = summary
        .mcp_servers
        .iter()
        .any(|(name, status)| name == MCP_SERVER_NAME && status == "connected");
    if !connected {
        return Err(format!(
            "the Claude Code CLI did not connect the harness's MCP server {MCP_SERVER_NAME:?}, so \
             the agent had no tools and NOTHING was measured; it reported {:?}",
            summary.mcp_servers
        ));
    }
    if summary.exposed_tools.is_empty() {
        return Err(
            "the Claude Code CLI exposed an EMPTY tool roster to the model, so nothing could be \
             evaluated"
                .to_string(),
        );
    }
    if summary.is_error {
        return Err(format!(
            "the Claude Code CLI reported a FAILED run rather than an answer: {}",
            summary.final_text
        ));
    }
    Ok(Outcome { final_text: summary.final_text.clone(), steps: summary.turns })
}
