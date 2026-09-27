//! Which EIP-712 signing domain an outcome token's order is signed against — the ONE input
//! [`build_order`](crate::order::build_order) needs that cannot be derived from the order itself.
//!
//! A Polymarket market is either standard (the CTF Exchange) or **NegRisk** (the NegRisk Exchange,
//! the multi-outcome/"one of N" shape). The two have DIFFERENT `verifyingContract`s, so signing an
//! order against the wrong one produces a signature the CLOB rejects — and, worse, does so with a
//! generic "invalid signature" that reads like a key problem. The flag is a property of the MARKET,
//! and an order only carries its outcome `token_id`, so the mapping has to come from somewhere else.
//!
//! ## Why this is a source, not a `HashSet`
//! [`PolymarketExecutionClient::spawn`](crate::client::PolymarketExecutionClient::spawn) has always
//! taken a pre-built `HashSet<String>` of NegRisk token ids and asked it `contains(token)`. That is
//! correct **only when the set is exhaustive**, because `contains` answers `false` for a token the
//! set never heard of — silently picking the standard domain for what may well be a NegRisk market.
//! `crate::instruments::fetch_token_neg_risk`'s own doc states the rule this module enforces: *a
//! caller that cannot resolve it must NOT assume `false`*.
//!
//! Building an exhaustive set means walking every `/markets` page (tens of thousands of markets) at
//! mount, which is both slow and stale the moment a new market lists. So the live mount uses
//! [`NegRiskSource::Lookup`] instead: the per-token `GET /neg-risk?token_id=…` point endpoint, memoised
//! per token, and — the load-bearing part — an **unresolvable token rejects the order** rather than
//! guessing a domain. A local reject costs nothing (the venue would have rejected the signature
//! anyway) and names the real reason.
//!
//! [`NegRiskSource::Static`] preserves the pre-existing set semantics byte-for-byte for every caller
//! that already has an exhaustive set (the tests, the smokes, any caller that fetched a catalog).

use std::collections::{HashMap, HashSet};

/// Resolve `token_id` → is-NegRisk over the network. `None` = could not resolve (REST failure,
/// unparseable body, unknown token) — NEVER `Some(false)`, which would be an assumption.
pub type NegRiskFetch = Box<dyn FnMut(&str) -> Option<bool> + Send>;

/// Where the exec thread learns a token's signing domain from. See the module doc.
pub enum NegRiskSource {
    /// A pre-built, assumed-exhaustive set: `contains` IS the answer, and an absent token means
    /// standard. The historical behavior, preserved verbatim.
    Static(HashSet<String>),
    /// Per-token point lookup, memoised. An unresolvable token yields `None` and the caller must
    /// reject the order (the exec thread does).
    Lookup {
        /// Answers already known (seeded at mount and/or filled by earlier lookups).
        cache: HashMap<String, bool>,
        /// The network resolver — production is [`clob_neg_risk_fetch`]; tests inject a closure.
        fetch: NegRiskFetch,
    },
}

impl NegRiskSource {
    /// The historical shape: a pre-built set, absent ⇒ standard domain.
    pub fn from_set(set: HashSet<String>) -> Self {
        Self::Static(set)
    }

    /// The live-mount shape: memoised `/neg-risk` point lookups through the proxy-aware CLOB agent,
    /// pre-seeded with whatever the caller already knows (`seed`, typically the mounted symbol).
    pub fn lookup(seed: HashMap<String, bool>) -> Self {
        Self::Lookup { cache: seed, fetch: clob_neg_risk_fetch() }
    }

    /// [`Self::lookup`] with an injected resolver — the testable core (no network).
    pub fn lookup_with(seed: HashMap<String, bool>, fetch: NegRiskFetch) -> Self {
        Self::Lookup { cache: seed, fetch }
    }

    /// Is `token_id` a NegRisk market's outcome? `None` = unresolvable; the caller MUST NOT read
    /// that as `false` (see the module doc) — the exec thread turns it into an `OrderRejected`.
    pub fn resolve(&mut self, token_id: &str) -> Option<bool> {
        match self {
            Self::Static(set) => Some(set.contains(token_id)),
            Self::Lookup { cache, fetch } => {
                if let Some(v) = cache.get(token_id) {
                    return Some(*v);
                }
                let v = fetch(token_id)?;
                cache.insert(token_id.to_string(), v);
                Some(v)
            }
        }
    }
}

impl std::fmt::Debug for NegRiskSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(s) => write!(f, "NegRiskSource::Static({} tokens)", s.len()),
            Self::Lookup { cache, .. } => {
                write!(f, "NegRiskSource::Lookup({} cached)", cache.len())
            }
        }
    }
}

/// The production resolver: ONE proxy-aware `GET /neg-risk?token_id=…` per uncached token, parsed by
/// the existing pure [`parse_neg_risk`](crate::instruments::parse_neg_risk). Goes through
/// [`crate::egress::get_json`] rather than the `RestTransport` seam because Polymarket is geo-blocked
/// from the dev box and only that path carries the SOCKS tunnel.
pub fn clob_neg_risk_fetch() -> NegRiskFetch {
    Box::new(|token_id: &str| {
        match crate::egress::get_json(
            crate::config::CLOB_BASE,
            "/neg-risk",
            &format!("token_id={token_id}"),
        ) {
            Ok(v) => {
                let parsed = crate::instruments::parse_neg_risk(&v);
                if parsed.is_none() {
                    tracing::warn!(target: "vike_polymarket::negrisk", token_id, "/neg-risk answered without a usable flag — order will be REJECTED rather than signed against a guessed domain");
                }
                parsed
            }
            Err(e) => {
                tracing::warn!(target: "vike_polymarket::negrisk", token_id, error = %e, "/neg-risk lookup failed — order will be REJECTED rather than signed against a guessed domain");
                None
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn static_set_is_the_historical_contains() {
        let mut s = NegRiskSource::from_set(HashSet::from(["111".to_string()]));
        assert_eq!(s.resolve("111"), Some(true));
        // an absent token is STANDARD under the static source — the pre-existing semantics,
        // preserved exactly for callers that hold an exhaustive set.
        assert_eq!(s.resolve("222"), Some(false));
    }

    #[test]
    fn lookup_memoises_and_only_fetches_once_per_token() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut s = NegRiskSource::lookup_with(
            HashMap::new(),
            Box::new(move |t: &str| {
                c.fetch_add(1, Ordering::Relaxed);
                Some(t == "neg")
            }),
        );
        assert_eq!(s.resolve("neg"), Some(true));
        assert_eq!(s.resolve("neg"), Some(true));
        assert_eq!(s.resolve("std"), Some(false));
        assert_eq!(s.resolve("std"), Some(false));
        assert_eq!(calls.load(Ordering::Relaxed), 2, "one fetch per DISTINCT token");
    }

    #[test]
    fn seeded_tokens_never_hit_the_network() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut s = NegRiskSource::lookup_with(
            HashMap::from([("seeded".to_string(), true)]),
            Box::new(move |_| {
                c.fetch_add(1, Ordering::Relaxed);
                Some(false)
            }),
        );
        assert_eq!(s.resolve("seeded"), Some(true));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    /// THE contract: an unresolvable token is `None`, never `Some(false)`. A `false` here would sign
    /// a NegRisk order against the CTF Exchange domain and get a bare "invalid signature" back.
    #[test]
    fn unresolvable_is_none_not_false_and_is_not_cached() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&calls);
        let mut s = NegRiskSource::lookup_with(
            HashMap::new(),
            Box::new(move |_| {
                // fail the first two calls, then succeed — proves a failure is not memoised.
                // `then_some` rather than `Some(true).filter(..)` per 1.97's `some_filter` lint: the
                // lint warns it reorders condition and value, which is inert here — the value is a
                // literal and the counter still ticks exactly once per call, which is the point.
                (c.fetch_add(1, Ordering::Relaxed) >= 2).then_some(true)
            }),
        );
        assert_eq!(s.resolve("t"), None);
        assert_eq!(s.resolve("t"), None);
        assert_eq!(s.resolve("t"), Some(true));
    }

    #[test]
    fn debug_never_dumps_the_whole_set() {
        let s = NegRiskSource::from_set(HashSet::from(["a".to_string(), "b".to_string()]));
        assert_eq!(format!("{s:?}"), "NegRiskSource::Static(2 tokens)");
        let l = NegRiskSource::lookup_with(HashMap::new(), Box::new(|_| None));
        assert_eq!(format!("{l:?}"), "NegRiskSource::Lookup(0 cached)");
    }
}
