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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a GammaMarket with the fields select_universe reads (others defaulted).
    fn mk(cid: &str, liq: f64, vol: f64, toks: &[&str]) -> GammaMarket {
        GammaMarket {
            id: cid.into(),
            question: format!("q-{cid}"),
            condition_id: cid.into(),
            slug: cid.into(),
            end_date: "".into(),
            volume: vol,
            liquidity: liq,
            active: true,
            closed: false,
            neg_risk: false,
            tick_size: 0.01,
            outcomes: vec![],
            token_ids: toks.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn ranks_by_liquidity_desc_and_takes_top_n() {
        let markets = vec![
            mk("a", 100.0, 0.0, &["a1"]),
            mk("b", 300.0, 0.0, &["b1"]),
            mk("c", 200.0, 0.0, &["c1"]),
        ];
        let policy = UniversePolicy { max_markets: 2, min_liquidity: 0.0, ..Default::default() };
        let sel = select_universe(&markets, &policy);
        assert_eq!(sel.len(), 2);
        assert_eq!(sel[0].condition_id, "b"); // 300
        assert_eq!(sel[1].condition_id, "c"); // 200
    }

    #[test]
    fn rank_by_volume_switches_the_order() {
        let markets = vec![mk("a", 10.0, 900.0, &["a1"]), mk("b", 999.0, 5.0, &["b1"])];
        let policy = UniversePolicy {
            max_markets: 1,
            min_liquidity: 0.0,
            rank_by: RankBy::Volume,
            ..Default::default()
        };
        let sel = select_universe(&markets, &policy);
        assert_eq!(sel[0].condition_id, "a"); // higher volume wins under RankBy::Volume
    }

    #[test]
    fn applies_liquidity_and_volume_floors() {
        let markets = vec![mk("keep", 5_000.0, 5_000.0, &["k"]), mk("thin", 10.0, 5_000.0, &["t"])];
        let policy = UniversePolicy {
            max_markets: 10,
            min_liquidity: 1_000.0,
            min_volume: 1_000.0,
            ..Default::default()
        };
        let sel = select_universe(&markets, &policy);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].condition_id, "keep");
    }

    #[test]
    fn require_open_drops_closed_and_inactive() {
        let mut closed = mk("closed", 9_999.0, 0.0, &["c"]);
        closed.closed = true;
        let mut inactive = mk("inactive", 9_999.0, 0.0, &["i"]);
        inactive.active = false;
        let markets = vec![closed, inactive, mk("open", 2_000.0, 0.0, &["o"])];
        let policy = UniversePolicy { max_markets: 10, min_liquidity: 0.0, ..Default::default() };
        let sel = select_universe(&markets, &policy);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].condition_id, "open");
    }

    #[test]
    fn markets_without_token_ids_are_dropped() {
        let markets = vec![mk("has", 5_000.0, 0.0, &["t"]), mk("none", 5_000.0, 0.0, &[])];
        let policy = UniversePolicy { max_markets: 10, min_liquidity: 0.0, ..Default::default() };
        let sel = select_universe(&markets, &policy);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].condition_id, "has");
    }

    #[test]
    fn equal_rank_breaks_by_condition_id_for_stability() {
        // identical liquidity -> deterministic order by condition_id, so the diff never jitters.
        let markets = vec![
            mk("zebra", 100.0, 0.0, &["z"]),
            mk("alpha", 100.0, 0.0, &["a"]),
            mk("mid", 100.0, 0.0, &["m"]),
        ];
        let policy = UniversePolicy { max_markets: 3, min_liquidity: 0.0, ..Default::default() };
        let sel = select_universe(&markets, &policy);
        assert_eq!(
            sel.iter().map(|m| m.condition_id.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "mid", "zebra"]
        );
    }

    #[test]
    fn manager_first_refresh_adds_all_selected_tokens() {
        let mut mgr = UniverseManager::new(UniversePolicy {
            max_markets: 10,
            min_liquidity: 0.0,
            ..Default::default()
        });
        let markets = vec![mk("a", 2.0, 0.0, &["a1", "a2"]), mk("b", 1.0, 0.0, &["b1"])];
        let (sel, diff) = mgr.refresh(&markets);
        assert_eq!(sel.len(), 2);
        assert_eq!(diff.to_add, vec!["a1", "a2", "b1"]); // sorted set order
        assert!(diff.to_remove.is_empty());
        assert_eq!(mgr.subscribed().collect::<Vec<_>>(), vec!["a1", "a2", "b1"]);
    }

    #[test]
    fn manager_rotation_adds_new_and_removes_dropped() {
        let mut mgr = UniverseManager::new(UniversePolicy {
            max_markets: 1,
            min_liquidity: 0.0,
            ..Default::default()
        });
        // Round 1: "a" is most liquid -> subscribe a1.
        let r1 = vec![mk("a", 100.0, 0.0, &["a1"]), mk("b", 10.0, 0.0, &["b1"])];
        let (_, d1) = mgr.refresh(&r1);
        assert_eq!(d1.to_add, vec!["a1"]);
        // Round 2: "b" overtakes -> unsubscribe a1, subscribe b1.
        let r2 = vec![mk("a", 10.0, 0.0, &["a1"]), mk("b", 100.0, 0.0, &["b1"])];
        let (_, d2) = mgr.refresh(&r2);
        assert_eq!(d2.to_add, vec!["b1"]);
        assert_eq!(d2.to_remove, vec!["a1"]);
        assert_eq!(mgr.subscribed().collect::<Vec<_>>(), vec!["b1"]);
    }

    #[test]
    fn plan_union_merges_targets_and_does_not_churn() {
        let mut mgr = UniverseManager::default();
        let a: BTreeSet<String> = ["a1".to_string(), "shared".to_string()].into();
        let b: BTreeSet<String> = ["b1".to_string(), "shared".to_string()].into();

        let d = mgr.plan_union([&a, &b]);
        assert_eq!(d.to_add, vec!["a1", "b1", "shared"], "de-duplicated union, sorted");
        assert!(d.to_remove.is_empty());
        mgr.commit(&d);
        assert!(mgr.plan_union([&a, &b]).is_empty(), "stable union is a no-op");

        // dropping one source removes only that source's exclusive tokens; "shared" survives.
        let d2 = mgr.plan_union([&b]);
        assert_eq!(d2.to_remove, vec!["a1"]);
        assert!(d2.to_add.is_empty());

        // an empty target list unsubscribes everything (union of nothing = nothing).
        let empty: [&BTreeSet<String>; 0] = [];
        assert_eq!(mgr.plan_union(empty).to_remove, vec!["a1", "b1", "shared"]);
    }

    #[test]
    fn manager_stable_universe_is_a_noop_diff() {
        let mut mgr = UniverseManager::new(UniversePolicy {
            max_markets: 5,
            min_liquidity: 0.0,
            ..Default::default()
        });
        let markets = vec![mk("a", 100.0, 0.0, &["a1"]), mk("b", 50.0, 0.0, &["b1"])];
        mgr.refresh(&markets);
        let (_, d2) = mgr.refresh(&markets); // same input again
        assert!(d2.is_empty(), "no churn when the selection is unchanged");
    }
}
