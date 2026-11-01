//! Provider selection for the in-app copilot. Default Anthropic; Cerebras alternate. Absent key ->
//! None.

use std::time::Duration;

use crate::anthropic::AnthropicClient;
use crate::cerebras::CerebrasClient;
use crate::client::LlmClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provider {
    #[default]
    Anthropic,
    Cerebras,
}

/// A client for `provider`, or `None` when `api_key` is absent.
///
/// `model` is the id this provider is asked for and `timeout` bounds each request to it. Both are
/// the CALLER's — the Studio chat hands in its `preferences.ai_anthropic_model` /
/// `preferences.ai_cerebras_model` and `preferences.ai_timeout_secs` rows — because this crate is a
/// library: it reads no setting and keeps no model-id or timeout default of its own, so there is
/// nothing here to disagree with the rows. The id is sent as given; a provider that does not know
/// it answers with an HTTP error, which reaches the caller as [`crate::LlmError::Http`].
pub fn make_client(
    provider: Provider,
    api_key: Option<String>,
    model: String,
    timeout: Duration,
) -> Option<Box<dyn LlmClient>> {
    match provider {
        Provider::Anthropic => {
            AnthropicClient::new(api_key, model, timeout).map(|c| Box::new(c) as Box<dyn LlmClient>)
        }
        Provider::Cerebras => {
            CerebrasClient::new(api_key, model, timeout).map(|c| Box::new(c) as Box<dyn LlmClient>)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn make_client_present_key_some_absent_none() {
        assert!(make_client(Provider::Anthropic, Some("k".into()), "m".into(), TIMEOUT).is_some());
        assert!(make_client(Provider::Cerebras, Some("k".into()), "m".into(), TIMEOUT).is_some());
        assert!(make_client(Provider::Anthropic, None, "m".into(), TIMEOUT).is_none());
        assert!(make_client(Provider::Cerebras, None, "m".into(), TIMEOUT).is_none());
    }

    #[test]
    fn default_provider_is_anthropic() {
        assert_eq!(Provider::default(), Provider::Anthropic);
    }
}
