//! Anthropic Messages API client (Studio SP3 Part B) over blocking `ureq`. Native tool-use loop
//! with ephemeral prompt-caching on the system prompt + tool defs. See the LLM wire reference doc.

use serde_json::{json, Value};

use crate::client::{
    http_agent, parse_json_or_error, LlmClient, LlmError, ToolCall, ToolSpec, Turn,
};

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-4-8";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TURNS: usize = 8;

/// Pure parse of a `/v1/messages` response body into a `Turn`.
pub fn parse_anthropic(body: &Value) -> Result<Turn, LlmError> {
    let stop = body.get("stop_reason").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let content = body
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| LlmError::Parse("missing content array".into()))?;
    let mut text: Option<String> = None;
    let mut tool_calls = Vec::new();
    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text = Some(t.to_string());
                }
            }
            Some("tool_use") => tool_calls.push(ToolCall {
                id: block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                name: block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                input: block.get("input").cloned().unwrap_or(json!({})),
            }),
            _ => {}
        }
    }
    Ok(Turn { text, tool_calls, stop })
}

pub struct AnthropicClient {
    model: String,
    max_tokens: u32,
    api_key: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicClient")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl AnthropicClient {
    pub fn new(api_key: Option<String>) -> Option<Self> {
        let api_key = api_key?;
        Some(Self { model: MODEL.into(), max_tokens: 4096, api_key, agent: http_agent() })
    }

    fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
        let n = tools.len();
        tools
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mut v = json!({"name": t.name, "description": t.description, "input_schema": t.input_schema});
                if i + 1 == n {
                    v["cache_control"] = json!({"type": "ephemeral"});
                }
                v
            })
            .collect()
    }

    fn post(&self, body: &Value) -> Result<Value, LlmError> {
        let mut resp = self
            .agent
            .post(ENDPOINT)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| LlmError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| LlmError::Http(e.to_string()))?;
        parse_json_or_error(status, text)
    }
}

impl LlmClient for AnthropicClient {
    fn run(
        &self,
        system: &str,
        user: &str,
        tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError> {
        let tools_json = Self::tools_json(tools);
        let mut messages: Vec<Value> = vec![json!({"role":"user","content":user})];
        for _ in 0..MAX_TURNS {
            let body = json!({
                "model": self.model,
                "max_tokens": self.max_tokens,
                "system": [{"type":"text","text":system,"cache_control":{"type":"ephemeral"}}],
                "tools": tools_json,
                "tool_choice": {"type":"auto"},
                "messages": messages,
            });
            let resp = self.post(&body)?;
            let turn = parse_anthropic(&resp)?;
            if turn.stop != "tool_use" || turn.tool_calls.is_empty() {
                return Ok(turn.text.unwrap_or_default());
            }
            // Echo the assistant turn verbatim, then one batched user turn of tool_results.
            messages.push(json!({"role":"assistant","content": resp["content"]}));
            let results: Vec<Value> = turn
                .tool_calls
                .into_iter()
                .map(|call| {
                    let id = call.id.clone();
                    let out = dispatch(call);
                    json!({"type":"tool_result","tool_use_id": id, "content": out})
                })
                .collect();
            messages.push(json!({"role":"user","content": results}));
        }
        Err(LlmError::Http("max turns exceeded".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_anthropic_extracts_tool_use() {
        let body = json!({
            "stop_reason": "tool_use",
            "content": [
                {"type":"text","text":"I'll submit."},
                {"type":"tool_use","id":"toolu_1","name":"submit_strategy","input":{"code":"fn on_bar(){}","explanation":"noop"}}
            ]
        });
        let turn = parse_anthropic(&body).unwrap();
        assert_eq!(turn.stop, "tool_use");
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].name, "submit_strategy");
        assert_eq!(turn.tool_calls[0].input["code"], json!("fn on_bar(){}"));
        assert_eq!(turn.text.as_deref(), Some("I'll submit."));
    }

    #[test]
    fn parse_anthropic_end_turn_text_only() {
        let body =
            json!({"stop_reason":"end_turn","content":[{"type":"text","text":"final answer"}]});
        let turn = parse_anthropic(&body).unwrap();
        assert!(turn.tool_calls.is_empty());
        assert_eq!(turn.text.as_deref(), Some("final answer"));
    }

    #[test]
    fn new_returns_none_without_key() {
        assert!(AnthropicClient::new(None).is_none());
    }

    #[test]
    fn debug_redacts_the_key() {
        let c = AnthropicClient::new(Some("sk-secret-123".into())).unwrap();
        assert!(!format!("{c:?}").contains("sk-secret-123"));
    }
}
