//! **One log line per provider TRANSITION: a provider starts failing, a provider recovers.**
//!
//! A provider's failure used to be logged at `tracing::debug!` while the datahub's log layer is
//! `warn`, so a `403` from a datacenter-hostile provider left no trace in the journal. Logging every
//! failed poll at `warn` would instead repeat the same line every retry for as long as the provider
//! stays down, so [`ProviderHealth`] remembers which `(provider, scope)` pairs are failing and speaks
//! only when that changes: one `warn!` when a pair first fails, one `info!` when it next answers.
//!
//! # ⚠ A line here names the provider and a fixed reason, never an address
//!
//! The fields are the feed (`news`, `economic`, `earnings`, `dividends`, `ipos`), the provider's
//! NAME, its scope (an equity week's Monday, an IPO month, empty for the rest) and the reason string
//! the poll already carries — `http::describe`'s fixed vocabulary (`HTTP 403`, `timed out`) or a
//! parser's one-line complaint, clipped to [`MAX_WHY_CHARS`]. No caller hands this module a URL, a
//! header or a key, and the keyed providers' tests pin that for the line they log
//! (`equity.rs`'s `a_provider_failure_is_logged_once_and_names_no_key_or_query`).

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

/// The longest reason a log line carries, in characters. The transport's reasons are far shorter; a
/// parser's complaint can quote a little of a hostile body, and a journal line should not.
pub const MAX_WHY_CHARS: usize = 120;

/// Which `(provider, scope)` pairs are failing right now. One per poller, so a restart forgets (and the
/// first failure after it is logged again — the intended behaviour).
#[derive(Debug, Default)]
pub struct ProviderHealth {
    failing: Mutex<BTreeSet<(String, String)>>,
}

impl ProviderHealth {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record how `provider`'s latest request for `scope` came out and log only a CHANGE: `Err(why)`
    /// the first time (a `warn!`), `Ok` after failures (an `info!`). Anything else is silent.
    pub fn record(
        &self,
        feed: &'static str,
        provider: &str,
        scope: &str,
        outcome: Result<(), &str>,
    ) {
        let key = (provider.to_string(), scope.to_string());
        let mut failing = self.failing.lock().unwrap_or_else(PoisonError::into_inner);
        match outcome {
            Err(why) => {
                if failing.insert(key) {
                    let why: String = why.chars().take(MAX_WHY_CHARS).collect();
                    tracing::warn!(
                        feed,
                        provider,
                        scope,
                        why = why.as_str(),
                        "vike-datahub tool feed: a provider started failing; its last good rows are \
                         kept, and this is logged once until it answers again"
                    );
                }
            }
            Ok(()) => {
                if failing.remove(&key) {
                    tracing::info!(
                        feed,
                        provider,
                        scope,
                        "vike-datahub tool feed: a provider answers again"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(f: impl FnOnce(&ProviderHealth)) -> Vec<(tracing::Level, String, Option<String>)> {
        let health = ProviderHealth::new();
        let ((), events) = vike_log::capture::captured(|| f(&health));
        events
            .into_iter()
            .map(|e| {
                (
                    e.level,
                    e.field("provider").unwrap_or("").to_string(),
                    e.field("why").map(str::to_string),
                )
            })
            .collect()
    }

    #[test]
    fn a_persistent_failure_is_one_warn_and_its_recovery_is_one_info() {
        let seen = lines(|h| {
            for _ in 0..5 {
                h.record("news", "FXStreet", "", Err("HTTP 403"));
            }
            h.record("news", "FXStreet", "", Ok(()));
            h.record("news", "FXStreet", "", Ok(()));
        });
        assert_eq!(
            seen,
            [
                (tracing::Level::WARN, "FXStreet".to_string(), Some("HTTP 403".to_string())),
                (tracing::Level::INFO, "FXStreet".to_string(), None),
            ]
        );
    }

    #[test]
    fn a_provider_that_never_failed_logs_nothing_and_a_second_failure_after_recovery_logs_again() {
        let seen = lines(|h| {
            h.record("news", "A", "", Ok(()));
            h.record("news", "A", "", Err("timed out"));
            h.record("news", "A", "", Ok(()));
            h.record("news", "A", "", Err("HTTP 500"));
        });
        let levels: Vec<_> = seen.iter().map(|(l, _, _)| *l).collect();
        assert_eq!(levels, [tracing::Level::WARN, tracing::Level::INFO, tracing::Level::WARN]);
    }

    #[test]
    fn two_scopes_of_one_provider_transition_independently() {
        let seen = lines(|h| {
            h.record("earnings", "Finnhub", "2026-10-05", Err("HTTP 429"));
            h.record("earnings", "Finnhub", "2026-10-12", Ok(()));
            h.record("earnings", "Finnhub", "2026-10-12", Err("HTTP 429"));
        });
        assert_eq!(seen.len(), 2, "a success on one week does not clear another week's failure");
    }

    #[test]
    fn a_long_reason_is_clipped() {
        let long = "x".repeat(MAX_WHY_CHARS * 3);
        let seen = lines(|h| h.record("economic", "ForexFactory", "", Err(&long)));
        assert_eq!(seen[0].2.as_ref().map(|w| w.chars().count()), Some(MAX_WHY_CHARS));
    }
}
