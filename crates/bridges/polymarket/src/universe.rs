//! `universe` — dynamic market-universe selection for Polymarket (the LEAN "universe selection"
//! idea, reframed for prediction markets). No Python twin — new capability.
//!
//! Polymarket has thousands of markets, most illiquid and untradeable. A tick/market-making
//! strategy wants to stream only the handful that are actually liquid *right now*, and to rotate
//! that set as liquidity shifts. This module is the pure selection + diff core over the existing
//! [`GammaMarket`](crate::gamma::GammaMarket) directory (which already carries `volume`/`liquidity`
//! and is fetched by [`GammaClient::list`](crate::gamma::GammaClient::list)):
//!
//! - [`select_universe`] — filter + rank a fetched market list down to the tradeable universe under
//!   a [`UniversePolicy`] (pure, fixture-tested).
//! - [`UniverseManager`] — tracks the currently-subscribed token_ids and, on each refresh, returns
//!   the [`UniverseDiff`] (which token_ids to subscribe / unsubscribe) so the caller drives the
//!   [`Feeds`](crate::market_feed) subscribe/unsubscribe seam. The manager owns NO network or feed
//!   handles — the caller pairs `GammaClient::list` with its `Feeds`, keeping this unit-testable.
//!
//! Deliberately venue-native (`token_id` strings, over `GammaMarket`) rather than generic over
//! `vike_catalog::Instrument`: Polymarket is the venue where universe selection actually earns its
//! keep (crypto/FX have a handful of symbols). A generic re-home is a later refactor if a second
//! venue needs it, not a reason to over-abstract now.

use std::collections::BTreeSet;

use crate::gamma::GammaMarket;

/// The ranking key used to order candidate markets before taking the top `max_markets`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RankBy {
    /// Order by resting-book depth (`liquidityNum`). The default — a market-maker wants markets it
    /// can actually quote into, and liquidity is the closest proxy for tradeability.
    #[default]
    Liquidity,
    /// Order by traded notional (`volumeNum`). Prefer when the strategy cares about flow/turnover
    /// (taker/momentum) rather than resting depth.
    Volume,
}

/// The universe-selection policy: which markets qualify, and how many to keep.
#[derive(Debug, Clone, PartialEq)]
pub struct UniversePolicy {
    /// Keep at most this many markets (the top-N after filtering + ranking). 0 selects nothing.
    pub max_markets: usize,
    /// Drop markets whose `liquidity` is below this floor (0.0 = no liquidity floor).
    pub min_liquidity: f64,
    /// Drop markets whose `volume` is below this floor (0.0 = no volume floor).
    pub min_volume: f64,
    /// Require `active && !closed` (the tradeable gate). Almost always `true`; `false` only for
    /// analysis over the full directory.
    pub require_open: bool,
    /// The ranking key for the top-N cut.
    pub rank_by: RankBy,
}

impl Default for UniversePolicy {
    /// A conservative market-maker default: the 20 most-liquid open markets with non-trivial depth.
    fn default() -> Self {
        Self {
            max_markets: 20,
            min_liquidity: 1_000.0,
            min_volume: 0.0,
            require_open: true,
            rank_by: RankBy::Liquidity,
        }
    }
}

/// One selected market: the outcome `token_id`s the subscribe seam keys off, plus the ranking
/// fields (so a caller can log/display why it was chosen).
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedMarket {
    /// CTF condition id (stable market identity).
    pub condition_id: String,
    /// Human-readable market question (for logging / display).
    pub question: String,
    /// The tradeable outcome `token_id`s (what `Feeds::subscribe_*` takes).
    pub token_ids: Vec<String>,
    /// `liquidityNum` at selection time.
    pub liquidity: f64,
    /// `volumeNum` at selection time.
    pub volume: f64,
}

/// Filter + rank a fetched market list into the tradeable universe under `policy`.
///
/// Pure and deterministic: filters by open/liquidity/volume, orders by `policy.rank_by` DESCENDING,
/// then takes the top `max_markets`. Ties (equal rank key) break by `condition_id` so the result is
/// stable across calls with the same input (no jitter in the subscribe/unsubscribe diff). Markets
/// with no `token_ids` are dropped — there is nothing to subscribe.
pub fn select_universe(markets: &[GammaMarket], policy: &UniversePolicy) -> Vec<SelectedMarket> {
    let mut candidates: Vec<&GammaMarket> = markets
        .iter()
        .filter(|m| !policy.require_open || (m.active && !m.closed))
        .filter(|m| m.liquidity >= policy.min_liquidity)
        .filter(|m| m.volume >= policy.min_volume)
        .filter(|m| !m.token_ids.is_empty())
        .collect();

    // Rank key DESC, then condition_id ASC for a total, jitter-free order. NaN ranks sort last
    // (partial_cmp None -> treat as Equal, so the condition_id tiebreak still gives a total order).
    candidates.sort_by(|a, b| {
        let (ka, kb) = match policy.rank_by {
            RankBy::Liquidity => (a.liquidity, b.liquidity),
            RankBy::Volume => (a.volume, b.volume),
        };
        kb.partial_cmp(&ka)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.condition_id.cmp(&b.condition_id))
    });

    candidates
        .into_iter()
        .take(policy.max_markets)
        .map(|m| SelectedMarket {
            condition_id: m.condition_id.clone(),
            question: m.question.clone(),
            token_ids: m.token_ids.clone(),
            liquidity: m.liquidity,
            volume: m.volume,
        })
        .collect()
}

/// The subscribe/unsubscribe delta between the currently-tracked universe and a fresh selection.
/// Both lists are sorted + de-duplicated for a stable, testable result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UniverseDiff {
    /// token_ids to newly subscribe (in the selection, not yet tracked).
    pub to_add: Vec<String>,
    /// token_ids to unsubscribe (tracked, no longer in the selection).
    pub to_remove: Vec<String>,
}

impl UniverseDiff {
    /// True when nothing changed — the caller can skip touching the feeds entirely.
    pub fn is_empty(&self) -> bool {
        self.to_add.is_empty() && self.to_remove.is_empty()
    }
}

/// Tracks the live universe (the set of subscribed token_ids) and computes the subscribe/unsubscribe
/// delta on each refresh. Holds no network or feed handles — the caller fetches markets
/// ([`GammaClient::list`](crate::gamma::GammaClient::list)) and applies the returned diff to its
/// [`Feeds`](crate::market_feed). This keeps the rotation logic pure and unit-testable while the
/// caller owns feed lifecycle + the `SubscriptionId`↔token_id bookkeeping.
#[derive(Debug, Default)]
pub struct UniverseManager {
    policy: UniversePolicy,
    /// The token_ids currently considered "in the universe" (subscribed by the caller).
    subscribed: BTreeSet<String>,
}

impl UniverseManager {
    /// Start empty with `policy`.
    pub fn new(policy: UniversePolicy) -> Self {
        Self { policy, subscribed: BTreeSet::new() }
    }

    /// The active policy.
    pub fn policy(&self) -> &UniversePolicy {
        &self.policy
    }

    /// The token_ids currently tracked as subscribed.
    pub fn subscribed(&self) -> impl Iterator<Item = &str> {
        self.subscribed.iter().map(String::as_str)
    }

    /// Compute the diff between the current universe and a fresh selection over `markets`, WITHOUT
    /// mutating state. Use when the caller wants to inspect the delta before applying it. The
    /// returned diff's `to_add`/`to_remove` are what the caller should subscribe/unsubscribe.
    pub fn plan(&self, markets: &[GammaMarket]) -> (Vec<SelectedMarket>, UniverseDiff) {
        let selected = select_universe(markets, &self.policy);
        let target: BTreeSet<String> =
            selected.iter().flat_map(|m| m.token_ids.iter().cloned()).collect();
        let diff = self.plan_tokens(target);
        (selected, diff)
    }

    /// Diff an ALREADY-CHOSEN target token set against the tracked universe, WITHOUT mutating state
    /// and WITHOUT consulting [`UniversePolicy`]. The policy-free half of [`plan`](Self::plan),
    /// factored out so a caller that computes its desired set some other way — e.g.
    /// [`discovery::RollingWindowPlanner`](crate::discovery::RollingWindowPlanner), whose target is
    /// "the current + next time window", not a liquidity ranking — reuses this one diffing rule
    /// instead of re-deriving it. `plan` is defined in terms of this, so both paths are byte-identical.
    pub fn plan_tokens(&self, target: BTreeSet<String>) -> UniverseDiff {
        let to_add: Vec<String> = target.difference(&self.subscribed).cloned().collect();
        let to_remove: Vec<String> = self.subscribed.difference(&target).cloned().collect();
        UniverseDiff { to_add, to_remove }
    }

    /// Diff the UNION of several independently-computed target sets against the tracked universe.
    ///
    /// The seam for a mount running MORE THAN ONE universe source into a single manager — e.g. the
    /// liquidity ranking plus
    /// [`discovery::RollingWindowPlanner`](crate::discovery::RollingWindowPlanner)'s rolling
    /// windows. Diffing each source's target separately against a shared manager is WRONG and
    /// churns: [`plan_tokens`](Self::plan_tokens) compares against the whole subscribed set, so
    /// each source would unsubscribe everything the other added, every pass, forever. Union first,
    /// commit once — that is what this method is for. Sources that must stay independent should
    /// instead each own their own `UniverseManager`.
    pub fn plan_union<'a>(
        &self,
        targets: impl IntoIterator<Item = &'a BTreeSet<String>>,
    ) -> UniverseDiff {
        let union: BTreeSet<String> = targets.into_iter().flat_map(|t| t.iter().cloned()).collect();
        self.plan_tokens(union)
    }

    /// Commit a diff produced by [`plan`](Self::plan) / [`plan_tokens`](Self::plan_tokens) as the
    /// new tracked universe — call once the caller has (or is about to) apply it to its feeds.
    /// [`refresh`](Self::refresh) is `plan` + `commit`.
    pub fn commit(&mut self, diff: &UniverseDiff) {
        for t in &diff.to_add {
            self.subscribed.insert(t.clone());
        }
        for t in &diff.to_remove {
            self.subscribed.remove(t);
        }
    }

    /// Like [`plan`](Self::plan) but ALSO commits the new universe as the tracked set — call this
    /// once the caller has (or is about to) apply the diff to its feeds. Returns the same
    /// `(selection, diff)` so the caller can drive `Feeds::subscribe_*` / `Feeds::unsubscribe`.
    pub fn refresh(&mut self, markets: &[GammaMarket]) -> (Vec<SelectedMarket>, UniverseDiff) {
        let (selected, diff) = self.plan(markets);
        self.commit(&diff);
        (selected, diff)
    }
}

#[path = "universe_tests.rs"]
#[cfg(test)]
mod universe_tests;
