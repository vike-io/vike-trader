//! `neg_risk_set` — the neg-risk market SET: a group of MUTUALLY EXCLUSIVE Polymarket markets that
//! share one on-chain `negRiskMarketID`, plus the Σ-price invariant a set arbitrage trades against.
//!
//! Before this module `neg_risk` was a per-market **bool** in all three representations
//! (`instruments.rs`, `gamma.rs`, `positions.rs`) — there was no collection of mutually-exclusive
//! outcomes and no cross-market invariant, so the members of a set could not even be ENUMERATED.
//!
//! ## The grouping key (live-verified — see `gamma.rs`'s module doc for the raw JSON)
//! [`GammaMarket::neg_risk_market_id`] (`negRiskMarketID`) is THE key: shared by every member,
//! equal to the parent event's, and equal to the on-chain `NegRiskAdapter` `_marketId`. Each
//! member additionally has a 0-based `group_item_index` (`groupItemThreshold`, misnamed on the
//! wire) that is also the last byte of its `questionID`. `negRiskRequestID` is PER-MARKET and must
//! NOT be used to group.
//!
//! ## The invariant
//! Exactly one member of a neg-risk set resolves YES; the rest resolve NO. So over a COMPLETE set
//! the YES prices must sum to 1:
//! ```text
//!     Σ_i  P(YES_i)  ==  1
//! ```
//! A sum meaningfully BELOW 1 is the buy-side set arb (buy every YES leg for less than the $1 the
//! winner certainly pays); a sum ABOVE 1 is the sell/convert side. Both edges are gross —
//! [`NegRiskSet::arb_edge`] subtracts a caller-supplied per-leg cost so the number compared against
//! zero is net.
//!
//! ## ⚠ COMPLETENESS IS LOAD-BEARING — and is why this type refuses to lie
//! Σ over HALF a set is not "a slightly wrong edge", it is a **guaranteed phantom arb**: dropping
//! members can only lower the sum, so an incomplete set always looks like free money. Two ways a
//! set arrives incomplete:
//!   1. the volume-ordered `/markets` browse ranks members independently and splits groups across
//!      pages (use [`crate::gamma::GammaClient::markets_by_event_slug`] instead);
//!   2. Gamma sends no `outcomePrices` for an inactive member (live-observed: 77 of the 128 members
//!      of `democratic-presidential-nominee-2028` had none).
//!
//! [`NegRiskSet::completeness`] reports both, and [`NegRiskSet::arb_edge`] returns `None` unless
//! the set is [`SetCompleteness::Complete`]. Nothing here guesses a missing leg's price.
//!
//! ### The worked example that motivated this gate (both fetches real, 2026-07-22)
//! The `next-prime-minister-of-ethiopia` set, seen two ways:
//!
//! | source | members returned | Σ YES | reads as |
//! |---|---|---|---|
//! | `/markets` volume browse | **7** (indices 1..7) | **0.039** | a 96c "arb" — PHANTOM |
//! | `/events?slug=` | **33** (indices 0..32, contiguous) | **0.997** | correctly priced, no edge |
//!
//! The browse simply never returned index 0, `"Abiy Ahmed"` at **0.958** — the incumbent, i.e. the
//! entire probability mass. Σ = 0.039 over the 7 stragglers is not a small error, it is a
//! catastrophic one, and it looks exactly like the trade you are hunting. That partial set lands in
//! [`SetCompleteness::IndexGap`] (7 members, max index 7 ⇒ index 0 missing) and is refused.
//!
//! That same real set also shows why unpriced members are the NORM, not an edge case: only **8** of
//! its 33 outcomes carried an `outcomePrices` at all. A strict `Complete` therefore rejects most
//! live sets — deliberately. [`NegRiskSet::yes_price_sum_with_ceiling`] is the explicit, opt-in
//! escape hatch: substitute a caller-chosen UPPER bound for each unpriced member so Σ is
//! over-stated, the edge is under-stated, and the error can only ever be conservative.
//!
//! **Known residual (stated, not silently swallowed):** the gap check is `indices == 0..n-1`, so it
//! catches a HOLE but cannot catch a TRUNCATED TAIL — indices `{0,1}` of a real 3-member set are
//! indistinguishable from a genuine 2-member set, because Gamma's per-market payload carries no
//! member COUNT. The cure is provenance, not arithmetic: build sets from
//! [`crate::gamma::GammaClient::markets_by_event_slug`], which returns an event's complete
//! `markets[]` in one response, rather than from a bounded `/markets` browse. A future
//! event-count field would let this be tightened.
//!
//! READ-ONLY. This module models and screens sets; it places no orders and sends no transaction.

use crate::gamma::{GammaMarket, neg_risk_question_id};
use std::collections::BTreeMap;

/// Whether a [`NegRiskSet`] is trustworthy enough to evaluate its Σ-price invariant against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetCompleteness {
    /// Indices are contiguous `0..n-1` with no duplicates AND every member quoted a YES price.
    /// Only in this state is Σ meaningful.
    Complete,
    /// The member indices are not a contiguous `0..n-1` run — members are missing (a split page)
    /// or duplicated. Carries the count actually held and the highest index seen, so a caller can
    /// tell "12 of 128" from "a 12-member set".
    IndexGap { held: usize, max_index: u32 },
    /// Indices are contiguous, but `missing` members carry no Gamma price. Σ would under-count.
    MissingPrices { missing: usize },
    /// At least one member has no `group_item_index` at all — the set cannot be ordered or
    /// checked for gaps, so it is never treated as complete.
    UnindexedMembers { without_index: usize },
    /// No members.
    Empty,
}

impl SetCompleteness {
    /// The only state in which the Σ-price invariant may be evaluated.
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// One member (one mutually-exclusive outcome) of a neg-risk set — the projection of a
/// [`GammaMarket`] that a set arb actually needs.
#[derive(Debug, Clone, PartialEq)]
pub struct NegRiskMember {
    /// 0-based outcome index within the set (`groupItemThreshold`; also `questionID`'s last byte).
    pub index: Option<u32>,
    /// The member's short in-group label (`groupItemTitle`), falling back to the full `question`.
    pub title: String,
    /// The member's CTF `conditionId` — the id a per-member redeem/split/merge targets.
    pub condition_id: String,
    /// The member's on-chain `questionID`.
    pub question_id: String,
    /// The YES leg's CLOB token id — the id a set-arb buy leg is submitted against.
    pub yes_token_id: Option<String>,
    /// The NO leg's CLOB token id — the leg `convertPositions` consumes.
    pub no_token_id: Option<String>,
    /// Gamma's cached YES mark. `None` when Gamma quoted none (an inactive member).
    pub yes_price: Option<f64>,
    pub active: bool,
    pub closed: bool,
}

impl NegRiskMember {
    fn from_market(m: &GammaMarket) -> Self {
        let title = if m.group_item_title.is_empty() {
            m.question.clone()
        } else {
            m.group_item_title.clone()
        };
        Self {
            index: m.group_item_index,
            title,
            condition_id: m.condition_id.clone(),
            question_id: m.question_id.clone(),
            yes_token_id: m.yes_token_id().map(str::to_string),
            no_token_id: m.no_token_id().map(str::to_string),
            yes_price: m.yes_price(),
            active: m.active,
            closed: m.closed,
        }
    }
}

/// A group of mutually-exclusive Polymarket markets sharing one `negRiskMarketID`.
///
/// Members are held sorted by `index` (unindexed members last, then by condition id) so the
/// ordering is deterministic regardless of the order Gamma paged them in.
#[derive(Debug, Clone, PartialEq)]
pub struct NegRiskSet {
    /// `negRiskMarketID` — the group key AND the on-chain `NegRiskAdapter` `_marketId` that
    /// [`crate::exec_plane::settlement::split_merge::convert_positions_calldata`] takes.
    pub market_id: String,
    /// The parent Gamma event's id / slug / title (blank when the source markets carried no
    /// `events[]`, e.g. a nested `/events` parse where the event itself was unlabelled).
    pub event_id: String,
    pub event_slug: String,
    pub event_title: String,
    pub members: Vec<NegRiskMember>,
}

impl NegRiskSet {
    /// Group a flat market list into neg-risk sets, keyed by `negRiskMarketID`. Markets with no
    /// group key (`!is_neg_risk_member`) are IGNORED — a non-neg-risk market is not a degenerate
    /// one-member set, and treating it as one would put a binary Yes/No market's `Σ = 1` invariant
    /// in the same bucket as a real N-way set.
    ///
    /// Returns sets ordered by `market_id` (a `BTreeMap` walk) so the output is deterministic.
    pub fn group(markets: &[GammaMarket]) -> Vec<NegRiskSet> {
        let mut by_key: BTreeMap<&str, Vec<&GammaMarket>> = BTreeMap::new();
        for m in markets.iter().filter(|m| m.is_neg_risk_member()) {
            by_key.entry(m.neg_risk_market_id.as_str()).or_default().push(m);
        }
        by_key
            .into_iter()
            .map(|(key, ms)| {
                // event identity: the first member that actually carries one (a `/markets` browse
                // fills it from `events[0]`; a bare page may leave every member blank).
                let ev = ms.iter().find(|m| !m.event_id.is_empty() || !m.event_slug.is_empty());
                let mut members: Vec<NegRiskMember> =
                    ms.iter().map(|m| NegRiskMember::from_market(m)).collect();
                members.sort_by(|a, b| {
                    a.index
                        .is_none()
                        .cmp(&b.index.is_none())
                        .then(a.index.cmp(&b.index))
                        .then_with(|| a.condition_id.cmp(&b.condition_id))
                });
                NegRiskSet {
                    market_id: key.to_string(),
                    event_id: ev.map(|m| m.event_id.clone()).unwrap_or_default(),
                    event_slug: ev.map(|m| m.event_slug.clone()).unwrap_or_default(),
                    event_title: ev.map(|m| m.event_title.clone()).unwrap_or_default(),
                    members,
                }
            })
            .collect()
    }

    /// How many mutually-exclusive outcomes this set holds (as held, not as it exists on-chain —
    /// see [`Self::completeness`]).
    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Every member's YES CLOB token id, in index order — the subscribe/quote set a set arb needs
    /// (hand straight to `Feeds::subscribe_*`). A member with no token id is skipped, so a short
    /// return is itself a signal the set is not tradeable end-to-end.
    pub fn yes_token_ids(&self) -> Vec<&str> {
        self.members.iter().filter_map(|m| m.yes_token_id.as_deref()).collect()
    }

    /// Every member's NO CLOB token id, in index order.
    pub fn no_token_ids(&self) -> Vec<&str> {
        self.members.iter().filter_map(|m| m.no_token_id.as_deref()).collect()
    }

    /// Both legs of every member — the full token universe of the set.
    pub fn all_token_ids(&self) -> Vec<&str> {
        self.members
            .iter()
            .flat_map(|m| [m.yes_token_id.as_deref(), m.no_token_id.as_deref()])
            .flatten()
            .collect()
    }

    /// Look a member up by its 0-based outcome index.
    pub fn member(&self, index: u32) -> Option<&NegRiskMember> {
        self.members.iter().find(|m| m.index == Some(index))
    }

    /// Is the set structurally sound enough to evaluate Σ against? See the module doc — an
    /// incomplete set ALWAYS looks like an arb, so this gate is the point of the type.
    pub fn completeness(&self) -> SetCompleteness {
        if self.members.is_empty() {
            return SetCompleteness::Empty;
        }
        let without_index = self.members.iter().filter(|m| m.index.is_none()).count();
        if without_index > 0 {
            return SetCompleteness::UnindexedMembers { without_index };
        }
        let mut idx: Vec<u32> = self.members.iter().filter_map(|m| m.index).collect();
        idx.sort_unstable();
        let max_index = *idx.last().expect("non-empty");
        let contiguous = idx.len() as u64 == max_index as u64 + 1
            && idx.iter().enumerate().all(|(i, &v)| i as u32 == v);
        if !contiguous {
            return SetCompleteness::IndexGap { held: idx.len(), max_index };
        }
        let missing = self.members.iter().filter(|m| m.yes_price.is_none()).count();
        if missing > 0 {
            return SetCompleteness::MissingPrices { missing };
        }
        SetCompleteness::Complete
    }

    /// Σ of every member's YES price — the invariant's left-hand side. `None` unless the set is
    /// [`SetCompleteness::Complete`]: summing a partial set is the phantom-arb trap the module doc
    /// describes, so it is refused rather than under-reported.
    ///
    /// Naive left-to-right fold in index order (deterministic given the sorted members).
    pub fn yes_price_sum(&self) -> Option<f64> {
        if !self.completeness().is_complete() {
            return None;
        }
        let mut sum = 0.0;
        for m in &self.members {
            sum += m.yes_price?;
        }
        Some(sum)
    }

    /// Σ of every member's YES price, substituting `ceiling` for each member Gamma quoted no price
    /// for — the opt-in escape hatch for the common real-world set where most outcomes are
    /// inactive and unpriced (live: only 8 of `next-prime-minister-of-ethiopia`'s 33 members
    /// carried a price).
    ///
    /// `ceiling` must be an UPPER bound on what an unpriced outcome could be worth (the venue's
    /// minimum tick, say). Then Σ is over-stated, so any edge derived from it is UNDER-stated: the
    /// error can only ever be conservative, never a phantom arb. Passing a `ceiling` that is too
    /// LOW re-opens exactly the trap this module exists to close — hence the naming, and hence
    /// this not being the default.
    ///
    /// Still `None` on a structurally broken set ([`SetCompleteness::IndexGap`],
    /// [`SetCompleteness::UnindexedMembers`], [`SetCompleteness::Empty`]) — a missing MEMBER cannot
    /// be papered over by a price assumption, only a missing PRICE can.
    pub fn yes_price_sum_with_ceiling(&self, ceiling: f64) -> Option<f64> {
        match self.completeness() {
            SetCompleteness::Complete | SetCompleteness::MissingPrices { .. } => {}
            _ => return None,
        }
        let mut sum = 0.0;
        for m in &self.members {
            sum += m.yes_price.unwrap_or(ceiling);
        }
        Some(sum)
    }

    /// The NET buy-side set-arb edge: `1 - Σ P(YES_i) - cost_per_leg * N`.
    ///
    /// Positive = buying one YES of every outcome costs less than the $1 the certain winner pays,
    /// after `cost_per_leg` (fee + expected slippage, in price units, per leg — pass `0.0` for the
    /// gross edge). `None` when the set is not [`SetCompleteness::Complete`].
    ///
    /// ⚠ These are Gamma's CACHED marks, not live top-of-book, and they ignore depth entirely — a
    /// positive number here is a SCREEN, never a trade signal. Executing requires the live book of
    /// every leg.
    pub fn arb_edge(&self, cost_per_leg: f64) -> Option<f64> {
        let sum = self.yes_price_sum()?;
        Some(1.0 - sum - cost_per_leg * self.members.len() as f64)
    }

    /// Cross-check the set's own index encoding: every member's wire `questionID` must equal
    /// `market_id` with its last byte replaced by the member's index (see
    /// [`neg_risk_question_id`]). Returns the members that DISAGREE — empty means the encoding
    /// this crate relies on held for the whole set. Members that carry no `questionID` or no index
    /// are skipped (nothing to check), not reported as mismatches.
    pub fn question_id_mismatches(&self) -> Vec<&NegRiskMember> {
        self.members
            .iter()
            .filter(|m| {
                let (Some(i), false) = (m.index, m.question_id.is_empty()) else { return false };
                match neg_risk_question_id(&self.market_id, i) {
                    Some(derived) => !derived.eq_ignore_ascii_case(&m.question_id),
                    None => false,
                }
            })
            .collect()
    }
}

#[path = "neg_risk_set_tests.rs"]
#[cfg(test)]
mod neg_risk_set_tests;
