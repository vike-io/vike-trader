//! Provider selection for the in-app copilot. Default Anthropic; Cerebras alternate. Absent key ->
//! None.

use crate::anthropic::AnthropicClient;
use crate::cerebras::CerebrasClient;
use crate::client::LlmClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Provider {
    #[default]
    Anthropic,
    Cerebras,
}

pub fn make_client(provider: Provider, api_key: Option<String>) -> Option<Box<dyn LlmClient>> {
    match provider {
        Provider::Anthropic => {
            AnthropicClient::new(api_key).map(|c| Box::new(c) as Box<dyn LlmClient>)
        }
        Provider::Cerebras => {
            CerebrasClient::new(api_key).map(|c| Box::new(c) as Box<dyn LlmClient>)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_client_present_key_some_absent_none() {
        assert!(make_client(Provider::Anthropic, Some("k".into())).is_some());
        assert!(make_client(Provider::Cerebras, Some("k".into())).is_some());
        assert!(make_client(Provider::Anthropic, None).is_none());
    }

    #[test]
    fn default_provider_is_anthropic() {
        assert_eq!(Provider::default(), Provider::Anthropic);
    }
}
