//! Cerebras `LlmClient` impl over blocking `ureq` (Studio SP3 Part B, Task 2). OpenAI-compatible
//! chat-completions wire shape (`choices[0].message.tool_calls`, `arguments` as a JSON string). See
//! the LLM wire reference doc (§B).

use serde_json::{Value, json};

use crate::client::{
    LlmClient, LlmError, ToolCall, ToolSpec, Turn, http_agent, parse_json_or_error,
};

const ENDPOINT: &str = "https://api.cerebras.ai/v1/chat/completions";
const MODEL: &str = "llama-3.3-70b";
const MAX_TURNS: usize = 8;

/// Pure parse of a `/v1/chat/completions` response body into a provider-neutral `Turn`. `arguments`
/// on each tool call is a JSON **string**, not already-parsed JSON (unlike Anthropic's `input`).
pub fn parse_cerebras(body: &Value) -> Result<Turn, LlmError> {
    let choice = body
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| LlmError::Parse("missing choices[0]".into()))?;
    let stop = choice.get("finish_reason").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let msg = choice.get("message").ok_or_else(|| LlmError::Parse("missing message".into()))?;
    let text = msg.get("content").and_then(|c| c.as_str()).map(String::from);
    let mut tool_calls = Vec::new();
    if let Some(calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for c in calls {
            let f = c
                .get("function")
                .ok_or_else(|| LlmError::Parse("tool_call missing function".into()))?;
            let args_str = f.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
            let input =
                serde_json::from_str(args_str).map_err(|e| LlmError::Parse(e.to_string()))?;
            tool_calls.push(ToolCall {
                id: c.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                name: f.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                input,
            });
        }
    }
    Ok(Turn { text, tool_calls, stop })
}

pub struct CerebrasClient {
    model: String,
    max_tokens: u32,
    api_key: String,
    agent: ureq::Agent,
}

impl std::fmt::Debug for CerebrasClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CerebrasClient")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl CerebrasClient {
    pub fn new(api_key: Option<String>) -> Option<Self> {
        let api_key = api_key?;
        Some(Self { model: MODEL.into(), max_tokens: 4096, api_key, agent: http_agent() })
    }

    fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect()
    }

    fn post(&self, body: &Value) -> Result<Value, LlmError> {
        let mut resp = self
            .agent
            .post(ENDPOINT)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .send(body.to_string().as_bytes())
            .map_err(|e| LlmError::Http(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().map_err(|e| LlmError::Http(e.to_string()))?;
        parse_json_or_error(status, text)
    }
}

impl LlmClient for CerebrasClient {
    fn run(
        &self,
        system: &str,
        user: &str,
        tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError> {
        let tools_json = Self::tools_json(tools);
        let mut messages: Vec<Value> =
            vec![json!({"role":"system","content":system}), json!({"role":"user","content":user})];
        for _ in 0..MAX_TURNS {
            let body = json!({
                "model": self.model,
                "max_tokens": self.max_tokens,
                "messages": messages,
                "tools": tools_json,
                "tool_choice": "auto",
            });
            let resp = self.post(&body)?;
            let turn = parse_cerebras(&resp)?;
            if turn.stop != "tool_calls" || turn.tool_calls.is_empty() {
                return Ok(turn.text.unwrap_or_default());
            }
            // Echo the assistant message (with its tool_calls) verbatim, then one `tool` message
            // per call (unlike Anthropic's single batched user turn of tool_result blocks).
            let assistant_msg = resp["choices"][0]["message"].clone();
            messages.push(assistant_msg);
            for call in turn.tool_calls {
                let id = call.id.clone();
                let out = dispatch(call);
                messages.push(json!({"role":"tool","tool_call_id": id, "content": out}));
            }
        }
        Err(LlmError::Http("max turns exceeded".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_cerebras_decodes_string_arguments() {
        let body = json!({"choices":[{"finish_reason":"tool_calls","message":{
            "tool_calls":[{"id":"call_1","type":"function","function":{"name":"submit_strategy","arguments":"{\"code\":\"fn on_bar(){}\"}"}}]
        }}]});
        let turn = parse_cerebras(&body).unwrap();
        assert_eq!(turn.stop, "tool_calls");
        assert_eq!(turn.tool_calls[0].name, "submit_strategy");
        assert_eq!(turn.tool_calls[0].input["code"], json!("fn on_bar(){}"));
    }

    #[test]
    fn new_none_without_key() {
        assert!(CerebrasClient::new(None).is_none());
    }

    #[test]
    fn debug_redacts_the_key() {
        let c = CerebrasClient::new(Some("csk-secret-456".into())).unwrap();
        assert!(!format!("{c:?}").contains("csk-secret-456"));
    }
}
