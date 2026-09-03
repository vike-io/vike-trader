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

use crate::gamma::{neg_risk_question_id, GammaMarket};
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
    /// [`crate::split_merge::convert_positions_calldata`] takes.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A member of the LIVE 7-way "Next Prime Minister of Ethiopia" set (see `gamma.rs`'s doc).
    const ETH_MARKET_ID: &str =
        "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6500";

    fn mk(idx: u32, title: &str, yes: Option<f64>, market_id: &str) -> GammaMarket {
        GammaMarket {
            id: format!("m{idx}"),
            question: format!("Will {title} …?"),
            condition_id: format!("0xcond{idx}"),
            slug: format!("s{idx}"),
            active: true,
            neg_risk: true,
            tick_size: 0.01,
            outcomes: vec!["Yes".into(), "No".into()],
            token_ids: vec![format!("yes{idx}"), format!("no{idx}")],
            outcome_prices: match yes {
                Some(p) => vec![p, 1.0 - p],
                None => vec![],
            },
            question_id: neg_risk_question_id(market_id, idx).unwrap_or_default(),
            neg_risk_market_id: market_id.into(),
            neg_risk_request_id: format!("0xreq{idx}"),
            group_item_title: title.into(),
            group_item_index: Some(idx),
            event_id: "411239".into(),
            event_slug: "next-prime-minister-of-ethiopia".into(),
            event_title: "Next Prime Minister of Ethiopia".into(),
            ..Default::default()
        }
    }

    /// A complete 3-way set, the happy path.
    fn complete_set() -> Vec<GammaMarket> {
        vec![
            mk(1, "B", Some(0.30), ETH_MARKET_ID),
            mk(0, "A", Some(0.50), ETH_MARKET_ID), // out of order on purpose
            mk(2, "C", Some(0.15), ETH_MARKET_ID),
        ]
    }

    #[test]
    fn groups_by_neg_risk_market_id_not_request_id() {
        // THE correctness point: `negRiskRequestID` is per-market (each mk gives a distinct one),
        // so grouping on it would yield 3 singleton sets instead of 1 set of 3.
        let sets = NegRiskSet::group(&complete_set());
        assert_eq!(sets.len(), 1, "one negRiskMarketID => one set");
        let s = &sets[0];
        assert_eq!(s.market_id, ETH_MARKET_ID);
        assert_eq!(s.len(), 3);
        assert_eq!(s.event_slug, "next-prime-minister-of-ethiopia");
        // members come back sorted by index regardless of input order
        assert_eq!(
            s.members.iter().map(|m| m.index).collect::<Vec<_>>(),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!(s.members[0].title, "A");
        // two distinct market ids => two sets
        let mut two = complete_set();
        two.push(mk(
            0,
            "Z",
            Some(0.9),
            "0xaa11223344556677889900aabbccddeeff00112233445566778899aabbccdd00",
        ));
        assert_eq!(NegRiskSet::group(&two).len(), 2);
    }

    #[test]
    fn non_neg_risk_markets_are_not_degenerate_sets() {
        let plain = GammaMarket {
            question: "binary".into(),
            condition_id: "0x1".into(),
            token_ids: vec!["y".into(), "n".into()],
            outcome_prices: vec![0.5, 0.5],
            ..Default::default()
        };
        assert!(!plain.is_neg_risk_member());
        assert!(NegRiskSet::group(&[plain]).is_empty());
    }

    #[test]
    fn token_id_accessors_are_index_ordered() {
        let s = &NegRiskSet::group(&complete_set())[0];
        assert_eq!(s.yes_token_ids(), vec!["yes0", "yes1", "yes2"]);
        assert_eq!(s.no_token_ids(), vec!["no0", "no1", "no2"]);
        assert_eq!(s.all_token_ids(), vec!["yes0", "no0", "yes1", "no1", "yes2", "no2"]);
        assert_eq!(s.member(1).unwrap().title, "B");
        assert!(s.member(9).is_none());
    }

    #[test]
    fn sigma_invariant_over_a_complete_set() {
        let s = &NegRiskSet::group(&complete_set())[0];
        assert_eq!(s.completeness(), SetCompleteness::Complete);
        // 0.50 + 0.30 + 0.15 = 0.95 → a 0.05 gross buy-side edge
        let sum = s.yes_price_sum().expect("complete");
        assert!((sum - 0.95).abs() < 1e-12, "sum={sum}");
        let gross = s.arb_edge(0.0).expect("complete");
        assert!((gross - 0.05).abs() < 1e-12, "gross={gross}");
        // 2c per leg over 3 legs eats 6c > the 5c gross edge → net NEGATIVE, no arb.
        let net = s.arb_edge(0.02).expect("complete");
        assert!(net < 0.0, "net={net}");
    }

    #[test]
    fn a_fairly_priced_set_sums_to_one_and_shows_no_edge() {
        let ms = vec![mk(0, "A", Some(0.60), ETH_MARKET_ID), mk(1, "B", Some(0.40), ETH_MARKET_ID)];
        let s = &NegRiskSet::group(&ms)[0];
        assert!((s.yes_price_sum().unwrap() - 1.0).abs() < 1e-12);
        assert!(s.arb_edge(0.0).unwrap().abs() < 1e-12);
    }

    #[test]
    fn an_incomplete_set_refuses_to_report_an_edge() {
        // THE trap: dropping index 1 lowers Σ to 0.65, which *looks* like a 35c arb. It must not
        // be reported at all.
        let ms: Vec<GammaMarket> =
            complete_set().into_iter().filter(|m| m.group_item_index != Some(1)).collect();
        let s = &NegRiskSet::group(&ms)[0];
        assert_eq!(s.completeness(), SetCompleteness::IndexGap { held: 2, max_index: 2 });
        assert!(s.yes_price_sum().is_none(), "no Σ over a gapped set");
        assert!(s.arb_edge(0.0).is_none(), "no edge over a gapped set");
        // a TRUNCATED tail (0,1 held, 2 dropped) is contiguous-from-zero and therefore
        // indistinguishable from a genuine 2-member set — documented limitation, see below.
        let ms2: Vec<GammaMarket> =
            complete_set().into_iter().filter(|m| m.group_item_index != Some(2)).collect();
        assert_eq!(NegRiskSet::group(&ms2)[0].completeness(), SetCompleteness::Complete);
    }

    /// The REAL divergence that motivates the completeness gate, both halves measured live
    /// (2026-07-22): the `/markets` volume browse returned indices 1..7 of
    /// `next-prime-minister-of-ethiopia` and silently omitted index 0 — `"Abiy Ahmed"` at 0.958,
    /// the whole probability mass. Σ over the stragglers is 0.039, i.e. a 96c phantom arb.
    #[test]
    fn the_live_partial_ethiopia_set_is_refused_not_reported_as_a_96c_arb() {
        // exactly what the volume browse returned: indices 1..7, index 0 absent
        let straggler_prices = [
            (1, 0.008),
            (2, 0.0025),
            (3, 0.0025),
            (4, 0.0025),
            (5, 0.0025),
            (6, 0.0025),
            (7, 0.0185),
        ];
        let partial: Vec<GammaMarket> =
            straggler_prices.iter().map(|&(i, p)| mk(i, "x", Some(p), ETH_MARKET_ID)).collect();
        let s = &NegRiskSet::group(&partial)[0];
        // the naive sum a caller would have computed without this type:
        let naive: f64 = partial.iter().map(|m| m.yes_price().unwrap()).sum();
        assert!(naive < 0.04, "the phantom Σ really is ~0.039 (got {naive})");
        // ...and it is refused, because index 0 is missing.
        assert_eq!(s.completeness(), SetCompleteness::IndexGap { held: 7, max_index: 7 });
        assert!(s.yes_price_sum().is_none());
        assert!(s.arb_edge(0.0).is_none(), "no 96c arb is ever reported");
        // a missing MEMBER cannot be papered over by a price assumption either
        assert!(s.yes_price_sum_with_ceiling(0.01).is_none());

        // the complete /events fetch adds index 0 = 0.958 → Σ ≈ 0.997, no edge.
        let mut full = partial;
        full.push(mk(0, "Abiy Ahmed", Some(0.958), ETH_MARKET_ID));
        let s = &NegRiskSet::group(&full)[0];
        assert_eq!(s.completeness(), SetCompleteness::Complete);
        let sum = s.yes_price_sum().unwrap();
        assert!((sum - 0.997).abs() < 1e-9, "sum={sum}");
        assert!(s.arb_edge(0.0).unwrap() < 0.01, "no meaningful edge once complete");
    }

    #[test]
    fn a_price_ceiling_can_only_understate_the_edge() {
        // 8 priced members summing to 0.997, plus 2 unpriced ones — the live shape.
        let mut ms = vec![mk(0, "Abiy Ahmed", Some(0.958), ETH_MARKET_ID)];
        for (i, p) in [
            (1, 0.008),
            (2, 0.0025),
            (3, 0.0025),
            (4, 0.0025),
            (5, 0.0025),
            (6, 0.0025),
            (7, 0.0185),
        ] {
            ms.push(mk(i, "x", Some(p), ETH_MARKET_ID));
        }
        ms.push(mk(8, "Person C", None, ETH_MARKET_ID));
        ms.push(mk(9, "Person D", None, ETH_MARKET_ID));
        let s = &NegRiskSet::group(&ms)[0];
        // strict path refuses: two members carry no price
        assert_eq!(s.completeness(), SetCompleteness::MissingPrices { missing: 2 });
        assert!(s.yes_price_sum().is_none());
        // with a 1-tick ceiling the two unpriced legs are charged 0.01 each → Σ = 1.017
        let sum = s.yes_price_sum_with_ceiling(0.01).unwrap();
        assert!((sum - 1.017).abs() < 1e-9, "sum={sum}");
        // a HIGHER ceiling can only raise Σ, i.e. lower the edge — never invent one
        let looser = s.yes_price_sum_with_ceiling(0.05).unwrap();
        assert!(looser > sum, "a more conservative ceiling never shrinks Σ");
        // ceiling 0.0 is the optimistic reading, and is exactly the trap: it recovers 0.997
        assert!((s.yes_price_sum_with_ceiling(0.0).unwrap() - 0.997).abs() < 1e-9);
    }

    #[test]
    fn a_member_without_a_price_blocks_sigma() {
        let ms = vec![
            mk(0, "A", Some(0.50), ETH_MARKET_ID),
            mk(1, "B", Some(0.30), ETH_MARKET_ID),
            mk(2, "C", None, ETH_MARKET_ID), // inactive member, Gamma quoted nothing
        ];
        let s = &NegRiskSet::group(&ms)[0];
        assert_eq!(s.completeness(), SetCompleteness::MissingPrices { missing: 1 });
        assert!(s.yes_price_sum().is_none());
        assert!(s.arb_edge(0.0).is_none());
    }

    #[test]
    fn an_unindexed_member_is_never_complete() {
        let mut ms = complete_set();
        ms[0].group_item_index = None;
        let s = &NegRiskSet::group(&ms)[0];
        assert_eq!(s.completeness(), SetCompleteness::UnindexedMembers { without_index: 1 });
        assert!(s.arb_edge(0.0).is_none());
        // unindexed members sort last but are still enumerable
        assert_eq!(s.len(), 3);
        assert_eq!(s.members[2].index, None);
    }

    #[test]
    fn empty_set_is_empty_not_complete() {
        let s = NegRiskSet {
            market_id: ETH_MARKET_ID.into(),
            event_id: String::new(),
            event_slug: String::new(),
            event_title: String::new(),
            members: vec![],
        };
        assert_eq!(s.completeness(), SetCompleteness::Empty);
        assert!(s.is_empty());
        assert!(s.arb_edge(0.0).is_none());
    }

    #[test]
    fn question_id_encoding_holds_across_the_set() {
        let s = &NegRiskSet::group(&complete_set())[0];
        assert!(s.question_id_mismatches().is_empty());
        // the live-observed value: marketId's last byte replaced by the index
        assert_eq!(
            s.member(2).unwrap().question_id,
            "0x55ab76d092f682bf5cbb7e14f13ee12f8410ce7cc1b7906f23b8fb56c11f6502"
        );
        // corrupt one and the cross-check catches it
        let mut ms = complete_set();
        ms[0].question_id = "0xdeadbeef".repeat(8);
        let bad = &NegRiskSet::group(&ms)[0];
        assert_eq!(bad.question_id_mismatches().len(), 1);
    }
}
