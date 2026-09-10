//! The provider-agnostic LLM seam (Studio SP3 Part B). One `LlmClient::run` drives a tool-use loop:
//! it calls the model, dispatches each requested tool via the caller's closure, feeds results back,
//! and returns the final assistant text. Anthropic and Cerebras impl this over blocking `ureq`
//! (no tokio); a `FakeClient` drives the loop in CI without network. See the LLM wire reference doc.

use std::time::Duration;

use serde_json::Value;

/// One tool advertised to the model.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A tool the model asked to call.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// One parsed assistant turn, provider-neutral. Each provider's `parse_*` normalizes its wire shape
/// into this (Anthropic's `content` blocks vs Cerebras's `choices[0].message`).
pub struct Turn {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub stop: String,
}

/// Why an LLM turn failed.
#[derive(Debug, Clone)]
pub enum LlmError {
    /// Network / non-2xx HTTP (message includes status + provider error text).
    Http(String),
    /// The response JSON did not match the expected shape.
    Parse(String),
    /// No API key configured for this provider.
    NoKey,
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Http(m) => write!(f, "llm http error: {m}"),
            LlmError::Parse(m) => write!(f, "llm parse error: {m}"),
            LlmError::NoKey => write!(f, "no API key configured"),
        }
    }
}
impl std::error::Error for LlmError {}

/// The agentic tool-use loop, provider-agnostic. `dispatch` executes one tool call and returns its
/// result text; `run` loops until the model stops requesting tools (or `max_turns`), returning the
/// final assistant text.
pub trait LlmClient {
    fn run(
        &self,
        system: &str,
        user: &str,
        tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError>;
}

/// A blocking `ureq` agent (rustls, no OpenSSL) — mirrors `vike_bridge_core::http::blocking_agent`
/// without depending on the venue-bridge layer. 4xx/5xx are returned as responses, not `Err`.
pub fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent("vike-trader-rust")
        .build()
        .new_agent()
}

/// Turn a provider response's `(status, body)` into parsed JSON or an [`LlmError`] — the shared tail
/// of every provider's `post`: a non-2xx status extracts `error.message` (falling back to the raw
/// body), a 2xx parses the body as JSON. The request (endpoint + auth headers) is provider-specific
/// and stays at the call site; only this identical error/parse handling is shared.
pub(crate) fn parse_json_or_error(status: u16, text: String) -> Result<Value, LlmError> {
    if !(200..300).contains(&status) {
        let msg = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .map(String::from)
            })
            .unwrap_or(text);
        return Err(LlmError::Http(format!("{status}: {msg}")));
    }
    serde_json::from_str(&text).map_err(|e| LlmError::Parse(e.to_string()))
}

#[cfg(test)]
pub(crate) enum FakeTurn {
    Tool(ToolCall),
    Text(String),
}

#[cfg(test)]
pub(crate) struct FakeClient {
    turns: std::cell::RefCell<std::collections::VecDeque<FakeTurn>>,
}

#[cfg(test)]
impl FakeClient {
    pub fn new(turns: Vec<FakeTurn>) -> Self {
        Self { turns: std::cell::RefCell::new(turns.into_iter().collect()) }
    }
}

#[cfg(test)]
impl LlmClient for FakeClient {
    fn run(
        &self,
        _system: &str,
        _user: &str,
        _tools: &[ToolSpec],
        dispatch: &mut dyn FnMut(ToolCall) -> String,
    ) -> Result<String, LlmError> {
        loop {
            let turn = self.turns.borrow_mut().pop_front();
            match turn {
                Some(FakeTurn::Tool(call)) => {
                    let _ = dispatch(call);
                }
                Some(FakeTurn::Text(text)) => return Ok(text),
                None => return Ok(String::new()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fake_client_runs_a_scripted_tool_loop() {
        // Scripts: turn 1 emits a tool call; the loop dispatches it; turn 2 returns final text.
        let fake = FakeClient::new(vec![
            FakeTurn::Tool(ToolCall {
                id: "t1".into(),
                name: "submit_strategy".into(),
                input: json!({"code":"x"}),
            }),
            FakeTurn::Text("done".into()),
        ]);
        let mut seen: Vec<String> = Vec::new();
        let out = fake
            .run("sys", "user", &[], &mut |call| {
                seen.push(call.name.clone());
                "ok".to_string()
            })
            .unwrap();
        assert_eq!(out, "done");
        assert_eq!(seen, vec!["submit_strategy"]);
    }

    #[test]
    fn fake_client_stops_at_final_text_without_dispatch() {
        let fake = FakeClient::new(vec![FakeTurn::Text("immediate".into())]);
        let out = fake.run("s", "u", &[], &mut |_| unreachable!("no tool call")).unwrap();
        assert_eq!(out, "immediate");
    }
}
