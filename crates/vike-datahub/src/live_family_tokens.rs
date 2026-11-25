//! **The live tokens of the rolling families this datahub records**: the server half of
//! `Request::LiveFamilyTokens`, and the one place the recorder leaves them for it.
//!
//! A Polymarket up/down family (`btc-updown-5m`) is a new market every few minutes. The recorder resolves each family
//! its profile names to the family's live windows on every tick and reads each new token's facts row through the
//! gatewayhub (`crate::recording::token_facts`); [`LiveFamilyTokensHeld`] is what it holds of them, replaced WHOLESALE
//! per family on every tick (the recorder's membership rule: a token that rotated out is not "removed", it simply is
//! not the family any more). The verb answers from that memory: no gatewayhub or venue call is made on a request, and
//! the request names nothing, so the server bounds its whole cost (decision 0052's rule).
//!
//! The type is UNGATED, like the verb: a build without the `record` plane still answers, in words. The recorder is its
//! one writer.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use vike_datahub_client::family::LiveFamilyToken;
use vike_datahub_client::proto::Response;

/// What a datahub that runs no recorder answers.
pub(crate) const NO_RECORDER: &str = "this datahub runs no recorder, so it follows no rolling family: name one in a \
     recorder profile (`vike-cli config profile create recorder <name> --store <root> --venue polymarket --family \
     <family>`) and restart the datahub";

/// What the recorder holds of each family's live windows, shared with the server. Cloning shares the same map.
#[derive(Clone, Default)]
pub struct LiveFamilyTokensHeld {
    families: Arc<Mutex<BTreeMap<String, Vec<LiveFamilyToken>>>>,
}

impl LiveFamilyTokensHeld {
    fn families(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<LiveFamilyToken>>> {
        self.families.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replace `family`'s live tokens, wholesale.
    pub fn set_family(&self, family: &str, tokens: Vec<LiveFamilyToken>) {
        self.families().insert(family.to_string(), tokens);
    }

    /// Every family's live tokens, families in name order.
    #[must_use]
    pub fn tokens(&self) -> Vec<LiveFamilyToken> {
        self.families().values().flatten().cloned().collect()
    }

    /// Whether any family holds `token` now.
    #[must_use]
    pub fn holds(&self, token: &str) -> bool {
        self.families().values().flatten().any(|t| t.token == token)
    }
}

/// `Request::LiveFamilyTokens`: what the recorder holds, or that this datahub runs none.
pub(crate) fn live_family_tokens_verb(held: Option<&LiveFamilyTokensHeld>) -> Response {
    match held {
        Some(held) => Response::LiveFamilyTokens(held.tokens()),
        None => Response::Error(NO_RECORDER.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(family: &str, token: &str, outcome: &str, end_ms: i64) -> LiveFamilyToken {
        LiveFamilyToken {
            family: family.to_string(),
            token: token.to_string(),
            outcome: outcome.to_string(),
            end_ms: Some(end_ms),
        }
    }

    /// A family's tokens are replaced wholesale, every family is answered in name order, and a datahub with no
    /// recorder answers in words.
    #[test]
    fn the_answer_is_every_familys_latest_tokens() {
        let held = LiveFamilyTokensHeld::default();
        held.set_family("eth-updown-15m", vec![token("eth-updown-15m", "9", "Up", 900)]);
        held.set_family("btc-updown-5m", vec![token("btc-updown-5m", "1", "Up", 300)]);
        held.set_family(
            "btc-updown-5m",
            vec![token("btc-updown-5m", "2", "Up", 600), token("btc-updown-5m", "3", "Down", 600)],
        );
        let Response::LiveFamilyTokens(got) = live_family_tokens_verb(Some(&held)) else {
            panic!("answered")
        };
        let names: Vec<&str> = got.iter().map(|t| t.token.as_str()).collect();
        assert_eq!(names, ["2", "3", "9"]);
        assert!(held.holds("2") && !held.holds("1"));
        let Response::Error(why) = live_family_tokens_verb(None) else {
            panic!("no recorder is an error")
        };
        assert_eq!(why, NO_RECORDER);
    }
}
